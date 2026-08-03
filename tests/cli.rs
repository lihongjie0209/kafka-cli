use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn help_should_list_the_management_suite() {
    let mut command = Command::cargo_bin("kafka").expect("kafka binary");
    command
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("leader-election"))
        .stdout(predicate::str::contains("delete-records"));
}

#[test]
fn missing_bootstrap_server_should_return_usage_exit_code() {
    let mut command = Command::cargo_bin("kafka").expect("kafka binary");
    command
        .args(["topics", "list"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--bootstrap-server is required"));
}

#[test]
fn group_regex_validation_should_not_require_a_broker() {
    let mut command = Command::cargo_bin("kafka").expect("kafka binary");
    command
        .args(["groups", "validate-regex", "orders-.*"])
        .assert()
        .success()
        .stdout(predicate::str::contains("orders-.*"))
        .stdout(predicate::str::contains("true"));
}
