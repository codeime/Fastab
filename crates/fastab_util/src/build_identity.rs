//! Identity of this mapped process, independent of files replaced by an update.
//! Kept std-only so the IME can include this leaf without the desktop stack.

use std::fmt::Write as _;
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

static START: OnceLock<(SystemTime, Instant)> = OnceLock::new();

pub fn initialize() {
    START.get_or_init(|| (SystemTime::now(), Instant::now()));
}

pub fn json(component: &str) -> String {
    let (started, elapsed) = START.get_or_init(|| (SystemTime::now(), Instant::now()));
    let fields = [
        ("component", Some(component)),
        ("version", Some(env!("CARGO_PKG_VERSION"))),
        ("commit", option_env!("AMAZON_Q_BUILD_HASH")),
        ("built_at", option_env!("AMAZON_Q_BUILD_DATETIME")),
        ("target", option_env!("AMAZON_Q_BUILD_TARGET_TRIPLE")),
        ("run_id", option_env!("FASTAB_BUILD_RUN_ID")),
    ];
    let mut output = format!(
        "{{\"pid\":{},\"started_at_unix_ms\":{},\"uptime_ms\":{}",
        std::process::id(),
        started.duration_since(UNIX_EPOCH).unwrap_or_default().as_millis(),
        elapsed.elapsed().as_millis(),
    );
    for (key, value) in fields {
        write!(&mut output, ",\"{key}\":").unwrap();
        match value {
            Some(value) => quote(value, &mut output),
            None => output.push_str("null"),
        }
    }
    output.push_str(",\"mapped_image_uuid\":");
    match mapped_image_uuid() {
        Some(uuid) => quote(&uuid, &mut output),
        None => output.push_str("null"),
    }
    output.push('}');
    output
}

fn quote(value: &str, output: &mut String) {
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            character if character < ' ' => write!(output, "\\u{:04x}", character as u32).unwrap(),
            character => output.push(character),
        }
    }
    output.push('"');
}

#[cfg(target_os = "macos")]
fn mapped_image_uuid() -> Option<String> {
    unsafe extern "C" {
        fn _dyld_get_image_header(index: u32) -> *const u8;
    }
    // SAFETY: dyld owns the immutable main image header and its load commands
    // for the process lifetime. Read the 64-bit header used by supported Macs;
    // bound traversal by sizeofcmds and each command's declared size.
    unsafe {
        let header = _dyld_get_image_header(0);
        if header.is_null() || header.cast::<u32>().read_unaligned() != 0xfeedfacf {
            return None;
        }
        let count = header.add(16).cast::<u32>().read_unaligned() as usize;
        let size = header.add(20).cast::<u32>().read_unaligned() as usize;
        let commands = std::slice::from_raw_parts(header.add(32), size);
        let mut offset = 0;
        for _ in 0..count {
            let prefix = commands.get(offset..offset.checked_add(8)?)?;
            let kind = u32::from_ne_bytes(prefix[..4].try_into().ok()?);
            let len = u32::from_ne_bytes(prefix[4..8].try_into().ok()?) as usize;
            if len < 8 {
                return None;
            }
            let command = commands.get(offset..offset.checked_add(len)?)?;
            if kind == 0x1b {
                let bytes = command.get(8..24)?;
                let mut uuid = String::with_capacity(32);
                for byte in bytes {
                    write!(&mut uuid, "{byte:02x}").unwrap();
                }
                return Some(uuid);
            }
            offset += len;
        }
        None
    }
}

#[cfg(not(target_os = "macos"))]
fn mapped_image_uuid() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn identity_is_valid_json_and_describes_this_process() {
        let value: serde_json::Value = serde_json::from_str(&super::json("fixture\"\\\n")).unwrap();
        assert_eq!(value["pid"], std::process::id());
        assert_eq!(value["component"], "fixture\"\\\n");
        assert!(value["started_at_unix_ms"].as_u64().unwrap() > 0);
        #[cfg(target_os = "macos")]
        assert_eq!(value["mapped_image_uuid"].as_str().unwrap().len(), 32);
    }
}
