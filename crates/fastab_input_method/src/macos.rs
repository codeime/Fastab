use std::os::raw::c_void;

use core_foundation::array::CFArrayRef;
use core_foundation::base::{CFRelease, OSStatus, TCFType};
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::string::{CFString, CFStringRef};
use core_foundation::url::CFURL;
use objc2::rc::autoreleasepool;
use objc2::runtime::Bool;
use objc2::{ClassType, msg_send};
use objc2_app_kit::NSApp;
use objc2_foundation::{MainThreadMarker, NSBundle, NSObject, ns_string};

use crate::imk;

const CONNECTION_NAME: &str = env!("InputMethodConnectionName");

type TISInputSourceRef = *const c_void;

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn TISRegisterInputSource(location: *const core_foundation::url::__CFURL) -> OSStatus;

    static kTISPropertyInputSourceID: CFStringRef;
    fn TISCreateInputSourceList(properties: CFDictionaryRef, include_all_installed: bool) -> CFArrayRef;
    fn TISSelectInputSource(input_source: TISInputSourceRef) -> OSStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFArrayGetCount(array: CFArrayRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CFArrayRef, idx: isize) -> *const c_void;
}

fn with_self_input_source(input_source_id: &str, work: impl FnOnce(TISInputSourceRef)) -> bool {
    let id_value = CFString::new(input_source_id);
    let key = unsafe { CFString::wrap_under_get_rule(kTISPropertyInputSourceID) };
    let dict = CFDictionary::from_CFType_pairs(&[(key.as_CFType(), id_value.as_CFType())]);

    // `include_all_installed = true` so we find the source even while it is still disabled.
    let list = unsafe { TISCreateInputSourceList(dict.as_concrete_TypeRef(), true) };
    if list.is_null() {
        log_info!("with_self_input_source: TISCreateInputSourceList returned null (no NSApplication context?)");
        return false;
    }

    let count = unsafe { CFArrayGetCount(list) };
    if count <= 0 {
        log_info!("with_self_input_source: no input source found for {input_source_id}");
        unsafe { CFRelease(list.cast()) };
        return false;
    }

    // Borrowed pointer into `list`; valid until we release the list below.
    let src: TISInputSourceRef = unsafe { CFArrayGetValueAtIndex(list, 0) };
    work(src);
    unsafe { CFRelease(list.cast()) };
    true
}

/// Keep the existing palette selection attempt after persisting its enabled
/// state. Selecting a palette may return paramErr (-50), which is harmless.
fn select_self_in_tis(input_source_id: &str) {
    with_self_input_source(input_source_id, |src| {
        let select_status = unsafe { TISSelectInputSource(src) };
        log_info!("select_self_in_tis: TISSelectInputSource status = {select_status}");
    });
}

/// Persist both palette lists without calling `TISEnableInputSource`, which
/// opens macOS Keyboard settings for third-party input sources. A selected-only
/// source gives new IME-only terminal windows no IMK connection.
///
/// In-process, not through `python3`: TIS launches this bundle with a bare
/// `PATH`, where `python3` is the Command Line Tools stub and fails outright on
/// a machine that never installed them. That failure costs the caret.
fn persist_palette_in_hitoolbox(bundle_id: &str) {
    if fastab_hitoolbox::ensure_palette_enabled(bundle_id) {
        log_info!("persist_palette_in_hitoolbox: enabled+selected lists confirmed for {bundle_id}");
    } else {
        log_error!("persist_palette_in_hitoolbox: could not write the HIToolbox palette lists for {bundle_id}");
    }
}

fn register_self_with_tis() {
    // Get the bundle path and register with TIS so macOS routes IMK connections to us
    let bundle = objc2_foundation::NSBundle::mainBundle();
    let bundle_path = unsafe { bundle.bundlePath() };
    let path_str = bundle_path.to_string();
    if let Some(url) = CFURL::from_path(&path_str, true) {
        let result = unsafe { TISRegisterInputSource(url.as_concrete_TypeRef()) };
        log_info!("TISRegisterInputSource result: {result}");
    }
}

pub fn main() {
    // Default is ERROR, same as the desktop app. The previous `trace` filter
    // plus an INFO `respondsToSelector` probe wrote on every IMK query and
    // kept a multi-thread tokio runtime alive for a process that only needs
    // AppKit. `Q_LOG_LEVEL=debug` still raises this when diagnosing caret
    // delivery.
    crate::logging::init();

    log_info!("Registering imk controller");
    imk::register_controller();
    log_info!("Registered imk controller");

    let mtm = MainThreadMarker::new().expect("must be on the main thread");

    autoreleasepool(|_pool| {
        let app = NSApp(mtm);

        let k_connection_name = ns_string!(CONNECTION_NAME);
        let nib_name = ns_string!("MainMenu");

        let bundle = NSBundle::mainBundle();
        let identifier = unsafe { bundle.bundleIdentifier() };

        register_self_with_tis();

        log_info!("Attempting connection...");
        imk::connect_imkserver(k_connection_name, identifier.as_deref());
        log_info!("Connected!");

        // Enable the palette before trying TIS selection; its enabled property
        // may be stale in this process. The persisted lists determine whether new
        // Otty / Ghostty / Kitty windows receive an IMK connection.
        if let Some(id) = identifier.as_deref() {
            let id = id.to_string();
            persist_palette_in_hitoolbox(&id);
            select_self_in_tis(&id);
        } else {
            log_error!("Could not determine bundle identifier; skipping palette enablement");
        }

        let app_id: &NSObject = app.as_ref();
        let loaded_nib: Bool = unsafe { msg_send![NSBundle::class(), loadNibNamed:nib_name owner:app_id] };
        log_info!("RUNNING {loaded_nib:?}!");

        unsafe { app.run() };
    });
}

#[cfg(test)]
mod tests {
    /// The production half of this file with comments stripped: these tests are
    /// about what the code does, and must not trip over prose naming the very
    /// thing being avoided.
    fn production_code() -> String {
        include_str!("macos.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("production source")
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn startup_never_disables_the_input_source() {
        let prod = production_code();
        assert!(
            !prod.contains("TISDisableInputSource"),
            "disable after enable is what took Otty down on install"
        );
        assert!(!prod.contains("schedule_reconnect"));
        assert!(
            prod.contains("fastab_hitoolbox::ensure_palette_enabled"),
            "TIS enable alone does not persist the palette"
        );
    }

    /// TIS launches this bundle with a bare `PATH`. A palette write that shells
    /// out is a write that fails on a machine without Command Line Tools, and a
    /// failed palette write means no caret in Otty / Ghostty / Kitty.
    #[test]
    fn the_palette_write_spawns_nothing() {
        assert!(!production_code().contains("Command::new"));
    }
}
