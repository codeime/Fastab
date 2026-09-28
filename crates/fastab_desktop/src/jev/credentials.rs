//! Jev credentials are stored only in the local SQLite auth table.
//!
//! A cancelled UI waiter keeps the gate until its database operation finishes.
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use fastab_settings::sqlite::{Db, database};
use gpui::{App, Global, Task};

const CREDENTIAL_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CredentialError {
    Busy,
    TimedOut,
    Unavailable,
    InvalidService,
}

#[derive(Default)]
struct CredentialGate(Rc<Cell<bool>>);
impl Global for CredentialGate {}

// Presence queries return no secret and must not delay a runtime credential
// read. Keep them bounded independently when the database is busy.
#[derive(Default)]
struct PresenceGate(Rc<Cell<bool>>);
impl Global for PresenceGate {}

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

fn acquire_presence(service: &str, cx: &mut App) -> Result<Lease, CredentialError> {
    if !service.starts_with("app.fastab.ai.jev.v1.") {
        return Err(CredentialError::InvalidService);
    }
    // Do not cache a pre-mutation answer while a timed-out write/delete is
    // still running. Reading this gate never delays runtime credential work.
    if cx.default_global::<CredentialGate>().0.get() {
        return Err(CredentialError::Busy);
    }
    let gate = cx.default_global::<PresenceGate>().0.clone();
    if gate.replace(true) {
        return Err(CredentialError::Busy);
    }
    Ok(Lease(gate))
}

/// Query SQLite only; the settings field displays a fixed placeholder.
pub(crate) fn contains(service: &str, cx: &mut App) -> Task<Result<bool, CredentialError>> {
    let lease = match acquire_presence(service, cx) {
        Ok(lease) => lease,
        Err(error) => return Task::ready(Err(error)),
    };
    let service = service.to_owned();
    let operation = cx
        .background_executor()
        .spawn(async move { contains_local(database()?, &service) });
    finish(lease, operation, cx)
}

// auth_kv stores the UTF-8 API key itself. Existing empty records remain
// equivalent to an absent credential.
fn stored_secret(value: String) -> Option<Vec<u8>> {
    (!value.is_empty()).then(|| value.into_bytes())
}

fn secret_value(secret: &[u8]) -> anyhow::Result<&str> {
    let value = std::str::from_utf8(secret)?;
    anyhow::ensure!(!value.is_empty(), "Credential is empty");
    Ok(value)
}

fn read_local(db: &Db, service: &str) -> anyhow::Result<Option<Vec<u8>>> {
    Ok(db.get_auth_value(service)?.and_then(stored_secret))
}

fn contains_local(db: &Db, service: &str) -> anyhow::Result<bool> {
    Ok(db.get_auth_value(service)?.is_some_and(|value| !value.is_empty()))
}

fn write_local(db: &Db, service: &str, secret: &[u8]) -> anyhow::Result<()> {
    db.set_auth_value(service, secret_value(secret)?)?;
    Ok(())
}

fn delete_local(db: &Db, service: &str) -> anyhow::Result<()> {
    db.unset_auth_value(service)?;
    Ok(())
}

fn finish_operation<T: 'static>(
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

fn finish<T: 'static>(
    lease: Lease,
    operation: Task<anyhow::Result<T>>,
    cx: &mut App,
) -> Task<Result<T, CredentialError>> {
    let result = finish_operation(lease, operation, cx);
    let executor = cx.background_executor().clone();
    cx.spawn(async move |_| {
        let timeout = executor.timer(CREDENTIAL_OPERATION_TIMEOUT);
        futures::pin_mut!(result);
        futures::pin_mut!(timeout);
        match futures::future::select(result, timeout).await {
            futures::future::Either::Left((result, _)) => result,
            futures::future::Either::Right((_, _)) => Err(CredentialError::TimedOut),
        }
    })
}

type CredentialReadOperation = Task<anyhow::Result<Option<Vec<u8>>>>;

fn begin_read(service: &str, cx: &mut App) -> Result<(Lease, CredentialReadOperation), CredentialError> {
    let lease = acquire(service, cx)?;
    let service = service.to_owned();
    let operation = cx
        .background_executor()
        .spawn(async move { read_local(database()?, &service) });
    Ok((lease, operation))
}

pub(crate) fn read(service: &str, cx: &mut App) -> Task<Result<Option<Vec<u8>>, CredentialError>> {
    let (lease, operation) = match begin_read(service, cx) {
        Ok(value) => value,
        Err(error) => return Task::ready(Err(error)),
    };
    finish(lease, operation, cx)
}

/// The overlay must receive a late credential result even after the settings
/// window's 30-second wait limit. Dropping this task still keeps the gate held
/// until the database operation actually finishes.
pub(crate) fn read_runtime(service: &str, cx: &mut App) -> Task<Result<Option<Vec<u8>>, CredentialError>> {
    let (lease, operation) = match begin_read(service, cx) {
        Ok(value) => value,
        Err(error) => return Task::ready(Err(error)),
    };
    finish_operation(lease, operation, cx)
}

pub(crate) fn write(service: &str, secret: Vec<u8>, cx: &mut App) -> Task<Result<(), CredentialError>> {
    let lease = match acquire(service, cx) {
        Ok(lease) => lease,
        Err(error) => return Task::ready(Err(error)),
    };
    let service = service.to_owned();
    let operation = cx
        .background_executor()
        .spawn(async move { write_local(database()?, &service, &secret) });
    finish(lease, operation, cx)
}

pub(crate) fn delete(service: &str, cx: &mut App) -> Task<Result<(), CredentialError>> {
    let lease = match acquire(service, cx) {
        Ok(lease) => lease,
        Err(error) => return Task::ready(Err(error)),
    };
    let service = service.to_owned();
    let operation = cx
        .background_executor()
        .spawn(async move { delete_local(database()?, &service) });
    finish(lease, operation, cx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use std::cell::RefCell;

    #[test]
    fn local_credentials_survive_reopen_and_stay_per_service() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.sqlite3");
        let db = Db::open_test(&path).unwrap();
        let first = "app.fastab.ai.jev.v1.first";
        let second = "app.fastab.ai.jev.v1.second";
        write_local(&db, first, b"test-api-key").unwrap();
        assert_eq!(db.get_auth_value(first).unwrap().as_deref(), Some("test-api-key"));

        drop(db);
        let db = Db::open_test(&path).unwrap();
        assert_eq!(read_local(&db, first).unwrap(), Some(b"test-api-key".to_vec()));
        assert!(contains_local(&db, first).unwrap());
        assert_eq!(read_local(&db, second).unwrap(), None);
        assert!(!contains_local(&db, second).unwrap());
        // Reading or checking a missing key must not create any record.
        assert!(db.get_auth_value(second).unwrap().is_none());
    }

    #[test]
    fn deleting_a_local_credential_removes_only_its_record() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_test(&dir.path().join("credentials.sqlite3")).unwrap();
        let service = "app.fastab.ai.jev.v1.deleted";
        let other = "app.fastab.ai.jev.v1.kept";
        write_local(&db, service, b"old-key").unwrap();
        write_local(&db, other, b"other-key").unwrap();

        delete_local(&db, service).unwrap();
        delete_local(&db, service).unwrap();
        assert!(db.get_auth_value(service).unwrap().is_none());
        assert_eq!(read_local(&db, service).unwrap(), None);
        assert!(!contains_local(&db, service).unwrap());
        assert_eq!(read_local(&db, other).unwrap(), Some(b"other-key".to_vec()));

        write_local(&db, service, b"new-key").unwrap();
        assert_eq!(read_local(&db, service).unwrap(), Some(b"new-key".to_vec()));
    }

    #[test]
    fn empty_records_are_absent_and_invalid_writes_preserve_a_saved_key() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_test(&dir.path().join("credentials.sqlite3")).unwrap();
        let service = "app.fastab.ai.jev.v1.empty";
        db.set_auth_value(service, "").unwrap();
        assert_eq!(read_local(&db, service).unwrap(), None);
        assert!(!contains_local(&db, service).unwrap());

        write_local(&db, service, b"saved-key").unwrap();
        assert!(write_local(&db, service, b"").is_err());
        assert!(write_local(&db, service, &[0xff]).is_err());
        assert_eq!(read_local(&db, service).unwrap(), Some(b"saved-key".to_vec()));
    }

    #[gpui::test]
    fn presence_timeout_never_blocks_runtime_credentials(cx: &mut TestAppContext) {
        let service = "app.fastab.ai.jev.v1.test";
        let (operation_tx, operation_rx) = futures::channel::oneshot::channel();
        let result = Rc::new(Cell::new(None));
        cx.update(|app| {
            let runtime_lease = acquire(service, app).unwrap();
            assert!(matches!(acquire_presence(service, app), Err(CredentialError::Busy)));
            drop(runtime_lease);
            let lease = acquire_presence(service, app).unwrap();
            assert!(matches!(acquire_presence(service, app), Err(CredentialError::Busy)));
            assert!(
                acquire(service, app).is_ok(),
                "metadata must not acquire the secret gate"
            );
            let operation = app.spawn(async move |_| operation_rx.await.unwrap());
            let pending = finish(lease, operation, app);
            let result = result.clone();
            app.spawn(async move |_| result.set(Some(pending.await))).detach();
        });
        cx.run_until_parked();
        cx.executor()
            .advance_clock(CREDENTIAL_OPERATION_TIMEOUT + Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(result.get(), Some(Err(CredentialError::TimedOut)));
        cx.update(|app| {
            assert!(matches!(acquire_presence(service, app), Err(CredentialError::Busy)));
            assert!(acquire(service, app).is_ok());
        });
        operation_tx.send(Ok(true)).unwrap();
        cx.run_until_parked();
        cx.update(|app| {
            assert!(acquire_presence(service, app).is_ok());
            assert!(acquire(service, app).is_ok());
        });
    }

    #[gpui::test]
    fn timeout_keeps_gate_until_the_underlying_operation_finishes(cx: &mut TestAppContext) {
        let service = "app.fastab.ai.jev.v1.test";
        let (operation_tx, operation_rx) = futures::channel::oneshot::channel();
        let result = Rc::new(Cell::new(None));

        cx.update(|app| {
            let lease = acquire(service, app).expect("first operation acquires the gate");
            let operation = app.spawn(async move |_| operation_rx.await.expect("test operation is completed"));
            let result_task = finish(lease, operation, app);
            let result = result.clone();
            app.spawn(async move |_| {
                result.set(Some(result_task.await));
            })
            .detach();
        });

        cx.run_until_parked();
        cx.executor()
            .advance_clock(CREDENTIAL_OPERATION_TIMEOUT + Duration::from_secs(1));
        cx.run_until_parked();

        assert_eq!(result.get(), Some(Err(CredentialError::TimedOut)));
        cx.update(|app| {
            assert!(matches!(acquire(service, app), Err(CredentialError::Busy)));
        });

        operation_tx
            .send(Ok(()))
            .expect("test operation receiver remains alive");
        cx.run_until_parked();
        cx.update(|app| {
            assert!(acquire(service, app).is_ok());
        });
    }

    #[gpui::test]
    fn runtime_receives_a_credential_after_the_settings_timeout(cx: &mut TestAppContext) {
        let service = "app.fastab.ai.jev.v1.test";
        let (operation_tx, operation_rx) = futures::channel::oneshot::channel();
        let result = Rc::new(RefCell::new(None));

        cx.update(|app| {
            let lease = acquire(service, app).expect("runtime read acquires the gate");
            let operation = app.spawn(async move |_| operation_rx.await.expect("test operation is completed"));
            let result_task = finish_operation(lease, operation, app);
            let result = result.clone();
            app.spawn(async move |_| {
                *result.borrow_mut() = Some(result_task.await);
            })
            .detach();
        });

        cx.run_until_parked();
        cx.executor()
            .advance_clock(CREDENTIAL_OPERATION_TIMEOUT + Duration::from_secs(1));
        cx.run_until_parked();

        assert!(result.borrow().is_none());
        cx.update(|app| {
            assert!(matches!(acquire(service, app), Err(CredentialError::Busy)));
        });

        operation_tx
            .send(Ok(Some(b"secret".to_vec())))
            .expect("test operation receiver remains alive");
        cx.run_until_parked();

        assert_eq!(result.borrow().as_ref(), Some(&Ok(Some(b"secret".to_vec()))));
        cx.update(|app| {
            assert!(acquire(service, app).is_ok());
        });
    }
}
