mod common;

use common::*;

#[test]
fn engine_complete_help_lists_compare() -> Result<()> {
    cli()
        .args(["engine", "complete", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--compare"))
        .stdout(predicate::str::contains("--session"));
    Ok(())
}

#[test]
fn engine_complete_requires_buffer_or_session() -> Result<()> {
    cli()
        .args(["engine", "complete"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--buffer"));
    Ok(())
}
