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
        .stdout(predicate::str::contains("delete-records"))
        .stdout(predicate::str::contains("client-metrics"))
        .stdout(predicate::str::contains("features"));
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

#[test]
fn reassignment_preview_should_accept_safety_flags_without_connecting() {
    let file = tempfile::NamedTempFile::new().expect("temporary reassignment file");
    std::fs::write(
        file.path(),
        r#"{"version":1,"partitions":[{"topic":"events","partition":0,"replicas":[1]}]}"#,
    )
    .expect("write reassignment fixture");
    let mut command = Command::cargo_bin("kafka").expect("kafka binary");
    command
        .args([
            "--bootstrap-server",
            "127.0.0.1:1",
            "reassign",
            "execute",
            "--reassignment-json-file",
            file.path().to_str().expect("fixture path"),
            "--additional",
            "--disallow-replication-factor-change",
            "--throttle",
            "1048576",
            "--replica-alter-log-dirs-throttle",
            "524288",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("PREVIEW EXECUTE"));
}

#[cfg(unix)]
#[test]
fn kafka_cluster_alias_should_accept_original_cluster_id_command() {
    let binary_command = Command::cargo_bin("kafka").expect("kafka binary");
    let binary = std::path::PathBuf::from(binary_command.get_program());
    let directory = tempfile::TempDir::new().expect("alias directory");
    let alias = directory.path().join("kafka-cluster.sh");
    std::os::unix::fs::symlink(binary, &alias).expect("create kafka-cluster alias");

    Command::new(alias)
        .args(["cluster-id", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

#[cfg(unix)]
#[test]
fn kafka_log_dirs_alias_should_accept_original_describe_flag() {
    let binary_command = Command::cargo_bin("kafka").expect("kafka binary");
    let binary = std::path::PathBuf::from(binary_command.get_program());
    let directory = tempfile::TempDir::new().expect("alias directory");
    let alias = directory.path().join("kafka-log-dirs.sh");
    std::os::unix::fs::symlink(binary, &alias).expect("create kafka-log-dirs alias");

    Command::new(alias)
        .args(["--describe", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--describe"));
}

#[cfg(unix)]
#[test]
fn kafka_configs_alias_alter_should_execute_instead_of_previewing() {
    let binary_command = Command::cargo_bin("kafka").expect("kafka binary");
    let binary = std::path::PathBuf::from(binary_command.get_program());
    let directory = tempfile::TempDir::new().expect("alias directory");
    let alias = directory.path().join("kafka-configs.sh");
    std::os::unix::fs::symlink(binary, &alias).expect("create kafka-configs alias");

    Command::new(alias)
        .args([
            "--bootstrap-server",
            "127.0.0.1:1",
            "--timeout-ms",
            "100",
            "--alter",
            "--entity-type",
            "topics",
            "--entity-name",
            "events",
            "--add-config",
            "retention.ms=1",
        ])
        .assert()
        .failure()
        .stdout(predicate::str::is_empty());
}

#[cfg(unix)]
#[test]
fn kafka_client_metrics_alias_should_accept_original_action_flags() {
    let binary_command = Command::cargo_bin("kafka").expect("kafka binary");
    let binary = std::path::PathBuf::from(binary_command.get_program());
    let directory = tempfile::TempDir::new().expect("alias directory");
    let alias = directory.path().join("kafka-client-metrics.sh");
    std::os::unix::fs::symlink(binary, &alias).expect("create kafka-client-metrics alias");

    Command::new(alias)
        .args(["--list", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

#[cfg(unix)]
#[test]
fn kafka_features_alias_should_accept_original_subcommands() {
    let binary_command = Command::cargo_bin("kafka").expect("kafka binary");
    let binary = std::path::PathBuf::from(binary_command.get_program());
    let directory = tempfile::TempDir::new().expect("alias directory");
    let alias = directory.path().join("kafka-features.sh");
    std::os::unix::fs::symlink(binary, &alias).expect("create kafka-features alias");

    Command::new(alias)
        .args(["version-mapping", "--release-version", "4.3-IV0"])
        .assert()
        .success()
        .stdout(predicate::str::contains("metadata.version"))
        .stdout(predicate::str::contains("30"));
}
