//! Write the named-adapter catalog from the Rust registry.
//!
//! ```text
//! cargo run -p ec_engine --example dump-adapters
//! cargo run -p ec_engine --example dump-adapters -- --check
//! ```

fn main() {
    let catalog = ec_engine::dump_native_adapter_catalog();
    let dest = std::env::args()
        .position(|arg| arg == "--out")
        .and_then(|index| std::env::args().nth(index + 1))
        .unwrap_or_else(|| ec_engine::native_adapter_catalog_path().to_string_lossy().into_owned());
    if std::env::args().any(|arg| arg == "--check") {
        let committed = std::fs::read_to_string(&dest).unwrap_or_default();
        if committed != catalog {
            eprintln!("adapters.json is stale at {dest}; run dump-adapters");
            std::process::exit(1);
        }
        println!("adapters.json matches the Rust registry");
        return;
    }
    std::fs::write(&dest, catalog).expect("write adapters.json");
    println!("wrote {dest}");
}
