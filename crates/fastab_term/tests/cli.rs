use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;

#[test]
fn version_flag_has_status_code_zero() {
    let mut cmd = Command::cargo_bin("fterm").unwrap();
    cmd.arg("--version");

    cmd.assert().success().stdout(predicate::str::starts_with(format!(
        "fterm {}",
        env!("CARGO_PKG_VERSION")
    )));
}
