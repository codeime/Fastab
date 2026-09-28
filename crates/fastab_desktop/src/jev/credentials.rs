//! Jev credentials live in the local SQLite auth table. A missing record may
//! be imported once from the former Keychain item; an empty record is a
//! tombstone so an explicitly deleted credential never reappears.
//!
//! A cancelled UI waiter does not release the gate while its database or
//! operating-system operation is still running.
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
// read. Keep them bounded independently when the OS Keychain is unresponsive.
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

/// Query local state first. An unmigrated item gets only a no-UI Keychain
/// presence probe; the settings field displays a fixed placeholder.
pub(crate) fn contains(service: &str, cx: &mut App) -> Task<Result<bool, CredentialError>> {
    let lease = match acquire_presence(service, cx) {
        Ok(lease) => lease,
        Err(error) => return Task::ready(Err(error)),
    };
    let service = service.to_owned();
    let operation = cx
        .background_executor()
        .spawn(async move { contains_with_legacy(database()?, &service, legacy_contains) });
    finish(lease, operation, cx)
}

// auth_kv stores the UTF-8 API key itself. An empty value means the old item
// was definitely absent or the user explicitly deleted this credential.
fn stored_secret(value: String) -> Option<Vec<u8>> {
    (!value.is_empty()).then(|| value.into_bytes())
}

fn secret_value(secret: &[u8]) -> anyhow::Result<&str> {
    let value = std::str::from_utf8(secret)?;
    anyhow::ensure!(!value.is_empty(), "Credential is empty");
    Ok(value)
}

fn read_with_legacy(
    db: &Db,
    service: &str,
    legacy_read: impl FnOnce(&str) -> anyhow::Result<Option<Vec<u8>>>,
) -> anyhow::Result<Option<Vec<u8>>> {
    if let Some(value) = db.get_auth_value(service)? {
        return Ok(stored_secret(value));
    }

    // The old Keychain is consulted only after a successful SQLite lookup
    // proves no local record exists. Neither a failed Keychain read nor a
    // failed cache write may be reported as a completed migration.
    let imported = match legacy_read(service) {
        Ok(value) => value,
        Err(error) => {
            // Another process may have populated SQLite while the old read
            // failed. That local record is authoritative even in this case.
            return match db.get_auth_value(service)? {
                Some(value) => Ok(stored_secret(value)),
                None => Err(error),
            };
        },
    };
    let value = imported
        .as_deref()
        .filter(|secret| !secret.is_empty())
        .map(secret_value)
        .transpose()?;
    // Another app instance may save or delete this service while Keychain is
    // waiting for authorization. Its local value wins over this import.
    db.set_auth_value_if_absent(service, value.unwrap_or_default())?;
    let final_value = db
        .get_auth_value(service)?
        .ok_or_else(|| anyhow::anyhow!("Imported credential record disappeared"))?;
    Ok(stored_secret(final_value))
}

fn contains_with_legacy(
    db: &Db,
    service: &str,
    legacy_contains: impl FnOnce(&str) -> anyhow::Result<bool>,
) -> anyhow::Result<bool> {
    if let Some(value) = db.get_auth_value(service)? {
        return Ok(!value.is_empty());
    }
    let old_presence = legacy_contains(service);
    // A no-UI Keychain probe can also race a local save or tombstone.
    if let Some(value) = db.get_auth_value(service)? {
        return Ok(!value.is_empty());
    }
    old_presence
}

fn write_local(db: &Db, service: &str, secret: &[u8]) -> anyhow::Result<()> {
    db.set_auth_value(service, secret_value(secret)?)?;
    Ok(())
}

fn delete_local(db: &Db, service: &str) -> anyhow::Result<()> {
    db.set_auth_value(service, "")?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn legacy_contains(service: &str) -> anyhow::Result<bool> {
    legacy::contains(service)
}

#[cfg(not(target_os = "macos"))]
fn legacy_contains(_service: &str) -> anyhow::Result<bool> {
    anyhow::bail!("Legacy credential presence is unavailable")
}

#[cfg(target_os = "macos")]
fn legacy_read(service: &str) -> anyhow::Result<Option<Vec<u8>>> {
    legacy::read(service)
}

#[cfg(not(target_os = "macos"))]
fn legacy_read(_service: &str) -> anyhow::Result<Option<Vec<u8>>> {
    anyhow::bail!("Legacy credential read is unavailable")
}

#[cfg(target_os = "macos")]
mod legacy {
    use anyhow::Context as _;
    use core_foundation::base::{CFType, CFTypeRef, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::data::CFData;
    use core_foundation::dictionary::{CFDictionary, CFDictionaryRef, CFMutableDictionary};
    use core_foundation::string::{CFString, CFStringRef};

    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        static kSecClass: CFStringRef;
        static kSecClassInternetPassword: CFStringRef;
        static kSecAttrServer: CFStringRef;
        static kSecReturnAttributes: CFStringRef;
        static kSecReturnData: CFStringRef;
        static kSecValueData: CFStringRef;
        static kSecUseAuthenticationUI: CFStringRef;
        static kSecUseAuthenticationUIFail: CFStringRef;
        fn SecItemCopyMatching(query: CFDictionaryRef, result: *mut CFTypeRef) -> i32;
    }

    pub(super) fn read(service: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let server = CFString::new(service);
        let cf_true = CFBoolean::true_value().as_CFTypeRef();
        // Match GPUI 0.2.2's former internet-password/server lookup. Unlike
        // GPUI, cancellation is not "missing": it must leave migration open.
        unsafe {
            let mut query = CFMutableDictionary::with_capacity(4);
            query.set(kSecClass.cast(), kSecClassInternetPassword.cast());
            query.set(kSecAttrServer.cast(), server.as_CFTypeRef());
            query.set(kSecReturnAttributes.cast(), cf_true);
            query.set(kSecReturnData.cast(), cf_true);

            let mut result = std::ptr::null();
            match SecItemCopyMatching(query.as_concrete_TypeRef(), &mut result) {
                0 => {},
                -25300 => return Ok(None), // errSecItemNotFound
                status => anyhow::bail!("Legacy credential read unavailable: {status}"),
            }
            anyhow::ensure!(!result.is_null(), "Legacy credential result was empty");

            let item = CFType::wrap_under_create_rule(result)
                .downcast::<CFDictionary>()
                .context("Legacy credential item was not a dictionary")?;
            let password = item
                .find(kSecValueData.cast())
                .context("Legacy credential data was missing")?;
            let password = CFType::wrap_under_get_rule(*password)
                .downcast::<CFData>()
                .context("Legacy credential data was not bytes")?;
            Ok(Some(password.bytes().to_vec()))
        }
    }

    pub(super) fn contains(service: &str) -> anyhow::Result<bool> {
        let server = CFString::new(service);
        // Match GPUI's internet-password/server identity. With no return-type
        // flags and a null result, Security never returns credential bytes.
        unsafe {
            let mut query = CFMutableDictionary::with_capacity(3);
            query.set(kSecClass.cast(), kSecClassInternetPassword.cast());
            query.set(kSecAttrServer.cast(), server.as_CFTypeRef());
            query.set(kSecUseAuthenticationUI.cast(), kSecUseAuthenticationUIFail.cast());
            match SecItemCopyMatching(query.as_concrete_TypeRef(), std::ptr::null_mut()) {
                0 => Ok(true),
                -25300 => Ok(false), // errSecItemNotFound
                status => anyhow::bail!("Credential presence unavailable: {status}"),
            }
        }
    }
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
        .spawn(async move { read_with_legacy(database()?, &service, legacy_read) });
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
/// until the operating-system operation actually finishes.
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
    fn legacy_secret_is_imported_once_as_plaintext_and_kept_per_service() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.sqlite3");
        let db = Db::open_test(&path).unwrap();
        let first = "app.fastab.ai.jev.v1.first";
        let second = "app.fastab.ai.jev.v1.second";
        let mut legacy_reads = 0;

        assert_eq!(
            read_with_legacy(&db, first, |_| {
                legacy_reads += 1;
                Ok(Some(b"test-api-key".to_vec()))
            })
            .unwrap(),
            Some(b"test-api-key".to_vec())
        );
        assert_eq!(db.get_auth_value(first).unwrap().as_deref(), Some("test-api-key"));
        assert_eq!(legacy_reads, 1);

        // Reopen the actual file to verify the record, rather than relying on
        // an in-memory mock or the same pooled connection.
        drop(db);
        let db = Db::open_test(&path).unwrap();
        assert_eq!(
            read_with_legacy(&db, first, |_| panic!("a migrated item must not read Keychain")).unwrap(),
            Some(b"test-api-key".to_vec())
        );
        assert!(contains_with_legacy(&db, first, |_| panic!("local item is authoritative")).unwrap());
        assert!(db.get_auth_value(second).unwrap().is_none());
        assert_eq!(read_with_legacy(&db, second, |_| Ok(None)).unwrap(), None);
        assert_eq!(db.get_auth_value(second).unwrap().as_deref(), Some(""));
        assert_eq!(db.get_auth_value(first).unwrap().as_deref(), Some("test-api-key"));
    }

    #[test]
    fn deleting_a_migrated_item_prevents_old_keychain_resurrection() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_test(&dir.path().join("credentials.sqlite3")).unwrap();
        let service = "app.fastab.ai.jev.v1.deleted";
        read_with_legacy(&db, service, |_| Ok(Some(b"old-key".to_vec()))).unwrap();

        delete_local(&db, service).unwrap();
        assert_eq!(db.get_auth_value(service).unwrap().as_deref(), Some(""));
        assert_eq!(
            read_with_legacy(&db, service, |_| panic!("delete must not read Keychain")).unwrap(),
            None
        );
        assert!(!contains_with_legacy(&db, service, |_| panic!("delete must not probe Keychain")).unwrap());

        write_local(&db, service, b"new-key").unwrap();
        assert_eq!(db.get_auth_value(service).unwrap().as_deref(), Some("new-key"));
        assert_eq!(
            read_with_legacy(&db, service, |_| panic!("save must not read Keychain")).unwrap(),
            Some(b"new-key".to_vec())
        );
    }

    #[test]
    fn a_concurrent_local_save_or_delete_wins_over_legacy_migration() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.sqlite3");
        let migrating = Db::open_test(&path).unwrap();
        let other_instance = Db::open_test(&path).unwrap();
        let saved = "app.fastab.ai.jev.v1.concurrent-save";
        let deleted = "app.fastab.ai.jev.v1.concurrent-delete";

        assert_eq!(
            read_with_legacy(&migrating, saved, |_| {
                write_local(&other_instance, saved, b"new-key")?;
                Ok(Some(b"old-key".to_vec()))
            })
            .unwrap(),
            Some(b"new-key".to_vec())
        );
        assert_eq!(migrating.get_auth_value(saved).unwrap().as_deref(), Some("new-key"));

        assert_eq!(
            read_with_legacy(&migrating, deleted, |_| {
                delete_local(&other_instance, deleted)?;
                Ok(Some(b"old-key".to_vec()))
            })
            .unwrap(),
            None
        );
        assert_eq!(migrating.get_auth_value(deleted).unwrap().as_deref(), Some(""));

        let failed = "app.fastab.ai.jev.v1.concurrent-read-error";
        assert_eq!(
            read_with_legacy(&migrating, failed, |_| {
                write_local(&other_instance, failed, b"newer-key")?;
                anyhow::bail!("old keychain failed")
            })
            .unwrap(),
            Some(b"newer-key".to_vec())
        );
    }

    #[test]
    fn failed_legacy_read_does_not_cache_a_migration() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_test(&dir.path().join("credentials.sqlite3")).unwrap();
        let service = "app.fastab.ai.jev.v1.retry";

        assert!(read_with_legacy(&db, service, |_| anyhow::bail!("cancelled")).is_err());
        assert!(db.get_auth_value(service).unwrap().is_none());
        assert!(read_with_legacy(&db, service, |_| Ok(Some(vec![0xff]))).is_err());
        assert!(db.get_auth_value(service).unwrap().is_none());
        assert_eq!(read_with_legacy(&db, service, |_| Ok(None)).unwrap(), None);
        assert_eq!(db.get_auth_value(service).unwrap().as_deref(), Some(""));
        assert_eq!(
            read_with_legacy(&db, service, |_| panic!("missing was cached")).unwrap(),
            None
        );
    }

    #[test]
    fn presence_probe_does_not_cache_unmigrated_keychain_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.sqlite3");
        let db = Db::open_test(&path).unwrap();
        let other_instance = Db::open_test(&path).unwrap();
        let service = "app.fastab.ai.jev.v1.presence";
        let mut probes = 0;

        assert!(
            contains_with_legacy(&db, service, |_| {
                probes += 1;
                Ok(true)
            })
            .unwrap()
        );
        assert_eq!(probes, 1);
        assert!(db.get_auth_value(service).unwrap().is_none());
        assert_eq!(
            read_with_legacy(&db, service, |_| Ok(Some(b"key".to_vec()))).unwrap(),
            Some(b"key".to_vec())
        );
        assert!(contains_with_legacy(&db, service, |_| panic!("local record is authoritative")).unwrap());

        let deleted = "app.fastab.ai.jev.v1.presence-race";
        assert!(
            !contains_with_legacy(&db, deleted, |_| {
                delete_local(&other_instance, deleted)?;
                Ok(true)
            })
            .unwrap()
        );
        assert_eq!(db.get_auth_value(deleted).unwrap().as_deref(), Some(""));
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
