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
        .stdout(predicate::str::contains("watch"))
        .stdout(predicate::str::contains("watchlist"));
}

#[test]
fn trader_agent_help_lists_background_and_stop() {
    Command::cargo_bin("schwab-trader")
        .unwrap()
        .args(["agent", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("run"))
        .stdout(predicate::str::contains("stop"))
        .stdout(predicate::str::contains("reload"));

    Command::cargo_bin("schwab-trader")
        .unwrap()
        .args(["agent", "run", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--background"));
}

#[test]
fn trader_background_cannot_combine_with_once() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../rules/trader-rules.example.yaml"
    );
    Command::cargo_bin("schwab-trader")
        .unwrap()
        .args(["agent", "run", path, "--background", "--once", "--simulate"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--background cannot be combined with --once"));
}

#[test]
fn trader_rules_validate_example() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../rules/trader-rules.example.yaml"
    );
    Command::cargo_bin("schwab-trader")
        .unwrap()
        .args(["rules", "validate", path, "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("example-swing-v1"))
        .stdout(predicate::str::contains("hints"));
}
