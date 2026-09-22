//! The only GPUI Keychain access point for Jev. A cancelled UI waiter does not
//! release the gate while its operating-system operation is still running.
use std::cell::Cell;
use std::rc::Rc;

use gpui::{App, Global, Task};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CredentialError {
    Busy,
    Unavailable,
    InvalidService,
}

#[derive(Default)]
struct CredentialGate(Rc<Cell<bool>>);
impl Global for CredentialGate {}

struct Lease(Rc<Cell<bool>>);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

fn acquire(service: &str, cx: &mut App) -> Result<Lease, CredentialError> {
    if !service.starts_with("app.fastab.ai.jev.v1.") {
        return Err(CredentialError::InvalidService);
    }
    let gate = cx.default_global::<CredentialGate>().0.clone();
    if gate.replace(true) {
        return Err(CredentialError::Busy);
    }
    Ok(Lease(gate))
}

fn finish<T: 'static>(
    lease: Lease,
    operation: Task<anyhow::Result<T>>,
    cx: &mut App,
) -> Task<Result<T, CredentialError>> {
    let (tx, rx) = futures::channel::oneshot::channel();
    cx.spawn(async move |_| {
        let result = operation.await.map_err(|_platform_error| CredentialError::Unavailable);
        drop(lease);
        let _ = tx.send(result);
    })
    .detach();
    cx.spawn(async move |_| rx.await.unwrap_or(Err(CredentialError::Unavailable)))
}

pub(crate) fn read(service: &str, cx: &mut App) -> Task<Result<Option<Vec<u8>>, CredentialError>> {
    let lease = match acquire(service, cx) {
        Ok(lease) => lease,
        Err(error) => return Task::ready(Err(error)),
    };
    let operation = cx.read_credentials(service);
    let operation = cx.spawn(async move |_| {
        operation.await.map(|value| value.map(|(_, secret)| secret))
    });
    finish(lease, operation, cx)
}

pub(crate) fn write(
    service: &str,
    secret: Vec<u8>,
    cx: &mut App,
) -> Task<Result<(), CredentialError>> {
    let lease = match acquire(service, cx) {
        Ok(lease) => lease,
        Err(error) => return Task::ready(Err(error)),
    };
    let operation = cx.write_credentials(service, "Fastab Jev", &secret);
    finish(lease, operation, cx)
}

pub(crate) fn delete(service: &str, cx: &mut App) -> Task<Result<(), CredentialError>> {
    let lease = match acquire(service, cx) {
        Ok(lease) => lease,
        Err(error) => return Task::ready(Err(error)),
    };
    let operation = cx.delete_credentials(service);
    let operation = cx.spawn(async move |_| {
        let result = operation.await;
        // GPUI 0.2.2's macOS backend exposes only this OSStatus string.
        // A missing item is safe to delete idempotently; cancellation and all
        // other failures still retain the profile for another explicit attempt.
        #[cfg(target_os = "macos")]
        if result.as_ref().is_err_and(|error| error.to_string() == "delete password failed: -25300") {
            return Ok(());
        }
        result
    });
    finish(lease, operation, cx)
}
