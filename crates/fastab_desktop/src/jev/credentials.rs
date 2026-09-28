//! The only GPUI Keychain access point for Jev. A cancelled UI waiter does not
//! release the gate while its operating-system operation is still running.
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

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

/// Query only whether an item exists, without reading its secret or asking for
/// Keychain authentication. The settings field displays a fixed placeholder.
pub(crate) fn contains(service: &str, cx: &mut App) -> Task<Result<bool, CredentialError>> {
    let lease = match acquire_presence(service, cx) {
        Ok(lease) => lease,
        Err(error) => return Task::ready(Err(error)),
    };
    let service = service.to_owned();
    let operation = cx.background_executor().spawn(async move {
        #[cfg(target_os = "macos")]
        {
            presence::contains(&service)
        }
        #[cfg(not(target_os = "macos"))]
        {
            anyhow::bail!("Credential presence is unavailable for {service}")
        }
    });
    finish(lease, operation, cx)
}

#[cfg(target_os = "macos")]
mod presence {
    use core_foundation::base::{CFTypeRef, TCFType};
    use core_foundation::dictionary::{CFDictionaryRef, CFMutableDictionary};
    use core_foundation::string::{CFString, CFStringRef};

    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        static kSecClass: CFStringRef;
        static kSecClassInternetPassword: CFStringRef;
        static kSecAttrServer: CFStringRef;
        static kSecUseAuthenticationUI: CFStringRef;
        static kSecUseAuthenticationUIFail: CFStringRef;
        fn SecItemCopyMatching(query: CFDictionaryRef, result: *mut CFTypeRef) -> i32;
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
    let operation = cx.read_credentials(service);
    let operation = cx.spawn(async move |_| operation.await.map(|value| value.map(|(_, secret)| secret)));
    Ok((lease, operation))
}

pub(crate) fn read(service: &str, cx: &mut App) -> Task<Result<Option<Vec<u8>>, CredentialError>> {
    let (lease, operation) = match begin_read(service, cx) {
        Ok(value) => value,
        Err(error) => return Task::ready(Err(error)),
    };
    finish(lease, operation, cx)
}

/// The overlay must receive a late Keychain result even after the settings
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
        if result
            .as_ref()
            .is_err_and(|error| error.to_string() == "delete password failed: -25300")
        {
            return Ok(());
        }
        result
    });
    finish(lease, operation, cx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use std::cell::RefCell;

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
