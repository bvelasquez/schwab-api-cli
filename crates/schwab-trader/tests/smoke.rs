use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn trader_help_lists_subcommands() {
    Command::cargo_bin("schwab-trader")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("agent"))
        .stdout(predicate::str::contains("capital"))
        .stdout(predicate::str::contains("watch"));
}

#[test]
fn trader_rules_validate_example() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../rules/trader-swing-9947.yaml"
    );
    Command::cargo_bin("schwab-trader")
        .unwrap()
        .args(["rules", "validate", path, "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("swing-beneficiary-9947"));
}
