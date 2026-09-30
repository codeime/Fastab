//! Cooperative cancellation for one completion attempt.
//!
//! Checks are cheap and thread-local. State publication is serialized against
//! cancellation so a check followed by a cache write cannot race cancellation.

use std::cell::RefCell;
use std::fmt;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

const ACTIVE: u8 = 0;
const CANCELLED: u8 = 1;
const COMPLETED: u8 = 2;

/// A superseded or explicitly cancelled completion, distinct from a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompletionCancelled;

impl fmt::Display for CompletionCancelled {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("completion cancelled")
    }
}

impl std::error::Error for CompletionCancelled {}

#[derive(Debug, Clone)]
pub(crate) struct CancellationToken(Arc<TokenState>);

#[derive(Debug)]
struct TokenState {
    state: AtomicU8,
    publication: Mutex<()>,
}

impl CancellationToken {
    pub(crate) fn new() -> Self {
        Self(Arc::new(TokenState {
            state: AtomicU8::new(ACTIVE),
            publication: Mutex::new(()),
        }))
    }

    /// Returns true only for the transition from active to cancelled.
    /// Completed attempts cannot be cancelled by a subsequently dropped reply.
    pub(crate) fn cancel(&self) -> bool {
        let _publication = self.0.publication.lock().unwrap_or_else(|error| error.into_inner());
        self.0
            .state
            .compare_exchange(ACTIVE, CANCELLED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.state.load(Ordering::Acquire) == CANCELLED
    }

    fn commit_if_active<T>(&self, commit: impl FnOnce() -> T) -> Result<T, CompletionCancelled> {
        let _publication = self.0.publication.lock().unwrap_or_else(|error| error.into_inner());
        if self.0.state.load(Ordering::Acquire) != ACTIVE {
            return Err(CompletionCancelled);
        }
        Ok(commit())
    }

    /// The final engine-state publication and completion win or lose together.
    /// Only the attempt owner calls this, once, after all computation finishes.
    fn finish_if_active<T>(&self, commit: impl FnOnce() -> T) -> Result<T, CompletionCancelled> {
        let _publication = self.0.publication.lock().unwrap_or_else(|error| error.into_inner());
        if self.0.state.load(Ordering::Acquire) != ACTIVE {
            return Err(CompletionCancelled);
        }
        let result = commit();
        self.0.state.store(COMPLETED, Ordering::Release);
        Ok(result)
    }
}

thread_local! {
    static CURRENT: RefCell<Option<CancellationToken>> = const { RefCell::new(None) };
}

/// Restores the enclosing attempt on normal return and unwind. The marker
/// prevents moving a scope to a different thread from its thread-local slot.
pub(crate) struct Scope {
    previous: Option<CancellationToken>,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl Drop for Scope {
    fn drop(&mut self) {
        CURRENT.with(|current| {
            *current.borrow_mut() = self.previous.take();
        });
    }
}

pub(crate) fn enter(token: CancellationToken) -> Scope {
    Scope {
        previous: CURRENT.with(|current| current.replace(Some(token))),
        _thread: std::marker::PhantomData,
    }
}

pub(crate) fn is_cancelled() -> bool {
    CURRENT.with(|current| current.borrow().as_ref().is_some_and(CancellationToken::is_cancelled))
}

pub(crate) fn check() -> Result<(), CompletionCancelled> {
    if is_cancelled() {
        Err(CompletionCancelled)
    } else {
        Ok(())
    }
}

/// Publish a fully computed value only while cancellation has not won.
///
/// The closure must be a short state write: do not run hooks, spawn processes,
/// cancel this token, or recursively call a publication function inside it.
/// Acquire cache locks inside this closure, never before entering it.
pub(crate) fn commit_if_active<T>(commit: impl FnOnce() -> T) -> Result<T, CompletionCancelled> {
    // Do not retain a RefCell borrow across user code or nested TLS queries.
    let token = CURRENT.with(|current| current.borrow().clone());
    match token {
        Some(token) => token.commit_if_active(commit),
        None => Ok(commit()),
    }
}

/// Finish the current attempt with one final, cancellation-serialized commit.
/// Has the same short-closure and lock-order requirements as commit_if_active.
pub(crate) fn finish_if_active<T>(commit: impl FnOnce() -> T) -> Result<T, CompletionCancelled> {
    let token = CURRENT.with(|current| current.borrow().clone());
    match token {
        Some(token) => token.finish_if_active(commit),
        None => Ok(commit()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_scopes_restore_sticky_cancellation_even_after_unwind() {
        let outer = CancellationToken::new();
        let scope = enter(outer.clone());
        assert!(outer.cancel());
        assert_eq!(check(), Err(CompletionCancelled));
        let result = std::panic::catch_unwind(|| {
            let _inner = enter(CancellationToken::new());
            assert_eq!(check(), Ok(()));
            panic!("exercise scope unwind");
        });
        assert!(result.is_err());
        assert_eq!(check(), Err(CompletionCancelled));
        assert!(!outer.cancel());
        drop(scope);
        assert_eq!(check(), Ok(()));
    }

    #[test]
    fn completion_and_cancellation_have_one_winner() {
        let completed = CancellationToken::new();
        {
            let _scope = enter(completed.clone());
            assert_eq!(finish_if_active(|| 42), Ok(42));
            let late: Result<(), _> = commit_if_active(|| panic!("completed attempt cannot publish"));
            assert_eq!(late, Err(CompletionCancelled));
        }
        assert!(!completed.cancel());
        assert!(!completed.is_cancelled());

        let cancelled = CancellationToken::new();
        let _scope = enter(cancelled.clone());
        assert!(cancelled.cancel());
        assert_eq!(
            commit_if_active(|| -> () { panic!("must not publish") }),
            Err(CompletionCancelled)
        );
        assert_eq!(
            finish_if_active(|| -> () { panic!("must not finish") }),
            Err(CompletionCancelled)
        );
    }

    #[test]
    fn cancellation_waits_for_an_existing_commit_and_rejects_later_writes() {
        let token = CancellationToken::new();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (events_tx, events_rx) = std::sync::mpsc::channel();
        let writer_token = token.clone();
        let writer_events = events_tx.clone();
        let writer = std::thread::spawn(move || {
            let _scope = enter(writer_token);
            commit_if_active(|| {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                writer_events.send("committed").unwrap();
            })
            .unwrap();
        });
        started_rx.recv().unwrap();
        let cancelling_token = token.clone();
        let canceller = std::thread::spawn(move || {
            assert!(cancelling_token.cancel());
            events_tx.send("cancelled").unwrap();
        });
        release_tx.send(()).unwrap();
        writer.join().unwrap();
        canceller.join().unwrap();
        assert_eq!(events_rx.iter().collect::<Vec<_>>(), ["committed", "cancelled"]);
        let _scope = enter(token);
        assert_eq!(
            commit_if_active(|| -> () { panic!("late write") }),
            Err(CompletionCancelled)
        );
    }
}
