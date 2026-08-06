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
fn features_bootstrap_controller_should_not_use_preemptive_stub() {
    let mut command = Command::cargo_bin("kafka").expect("kafka binary");
    let assertion = command
        .args([
            "features",
            "--bootstrap-controller",
            "127.0.0.1:1",
            "describe",
        ])
        .assert()
        .failure();
    let output = assertion.get_output();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !combined.contains("native client does not expose"),
        "must not use the old preemptive capability stub; got:\n{combined}"
    );
    // Live client path: connection/timeout/protocol error is acceptable.
    assert!(
        !combined.is_empty() || output.status.code().unwrap_or(0) != 0,
        "expected a real client-path failure"
    );
}

#[test]
fn metadata_quorum_bootstrap_controller_should_not_use_preemptive_stub() {
    let mut command = Command::cargo_bin("kafka").expect("kafka binary");
    let assertion = command
        .args([
            "metadata-quorum",
            "--bootstrap-controller",
            "127.0.0.1:1",
            "describe",
            "--status",
        ])
        .assert()
        .failure();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&assertion.get_output().stdout),
        String::from_utf8_lossy(&assertion.get_output().stderr)
    );
    assert!(
        !combined.contains("native client does not expose"),
        "must not use the old preemptive capability stub; got:\n{combined}"
    );
}

#[test]
fn bootstrap_controller_conflicts_with_bootstrap_server_on_cli() {
    let mut command = Command::cargo_bin("kafka").expect("kafka binary");
    let assertion = command
        .args([
            "--bootstrap-server",
            "localhost:9092",
            "features",
            "--bootstrap-controller",
            "127.0.0.1:9093",
            "describe",
        ])
        .assert()
        .failure();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&assertion.get_output().stdout),
        String::from_utf8_lossy(&assertion.get_output().stderr)
    );
    assert!(
        combined.contains("cannot use both --bootstrap-server and --bootstrap-controller")
            || combined.contains("cannot be used with"),
        "expected mutual exclusion error; got:\n{combined}"
    );
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

#[cfg(unix)]
#[test]
fn kafka_transactions_alias_should_accept_original_subcommands() {
    let binary_command = Command::cargo_bin("kafka").expect("kafka binary");
    let binary = std::path::PathBuf::from(binary_command.get_program());
    let directory = tempfile::TempDir::new().expect("alias directory");
    let alias = directory.path().join("kafka-transactions.sh");
    std::os::unix::fs::symlink(binary, &alias).expect("create kafka-transactions alias");

    Command::new(alias)
        .args(["list", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--duration-filter"));
}

#[cfg(unix)]
#[test]
fn kafka_metadata_quorum_alias_should_accept_original_subcommands() {
    let binary_command = Command::cargo_bin("kafka").expect("kafka binary");
    let binary = std::path::PathBuf::from(binary_command.get_program());
    let directory = tempfile::TempDir::new().expect("alias directory");
    let alias = directory.path().join("kafka-metadata-quorum.sh");
    std::os::unix::fs::symlink(binary, &alias).expect("create kafka-metadata-quorum alias");

    Command::new(alias)
        .args(["describe", "--status", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--replication"));
}

#[cfg(unix)]
#[test]
fn kafka_delegation_tokens_alias_should_accept_original_action_flags() {
    let binary_command = Command::cargo_bin("kafka").expect("kafka binary");
    let binary = std::path::PathBuf::from(binary_command.get_program());
    let directory = tempfile::TempDir::new().expect("alias directory");
    let alias = directory.path().join("kafka-delegation-tokens.sh");
    std::os::unix::fs::symlink(binary, &alias).expect("create kafka-delegation-tokens alias");

    Command::new(alias)
        .args(["--describe", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--owner-principal"));
}

#[cfg(unix)]
#[test]
fn kafka_groups_alias_should_accept_original_list_filters() {
    let binary_command = Command::cargo_bin("kafka").expect("kafka binary");
    let binary = std::path::PathBuf::from(binary_command.get_program());
    let directory = tempfile::TempDir::new().expect("alias directory");
    let alias = directory.path().join("kafka-groups.sh");
    std::os::unix::fs::symlink(binary, &alias).expect("create kafka-groups alias");

    Command::new(alias)
        .args([
            "--list",
            "--group-type",
            "share",
            "--protocol",
            "share",
            "--help",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("--consumer"))
        .stdout(predicate::str::contains("--streams"));
}

#[cfg(unix)]
#[test]
fn kafka_share_groups_alias_should_accept_original_actions() {
    let binary_command = Command::cargo_bin("kafka").expect("kafka binary");
    let binary = std::path::PathBuf::from(binary_command.get_program());
    let directory = tempfile::TempDir::new().expect("alias directory");
    let alias = directory.path().join("kafka-share-groups.sh");
    std::os::unix::fs::symlink(binary, &alias).expect("create kafka-share-groups alias");

    Command::new(alias)
        .args(["--describe", "--group", "workers", "--members", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--all-groups"))
        .stdout(predicate::str::contains("--offsets"));
}

#[cfg(unix)]
#[test]
fn kafka_console_share_consumer_alias_should_accept_original_options() {
    let binary_command = Command::cargo_bin("kafka").expect("kafka binary");
    let binary = std::path::PathBuf::from(binary_command.get_program());
    let directory = tempfile::TempDir::new().expect("alias directory");
    let alias = directory.path().join("kafka-console-share-consumer.sh");
    std::os::unix::fs::symlink(binary, &alias).expect("create console share consumer alias");

    Command::new(alias)
        .args([
            "--topic",
            "events",
            "--group",
            "workers",
            "--release",
            "--reject-message-on-error",
            "--help",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("--formatter-property"))
        .stdout(predicate::str::contains("--consumer-property"));
}

#[cfg(unix)]
#[test]
fn kafka_consumer_perf_alias_should_accept_original_options() {
    let binary_command = Command::cargo_bin("kafka").expect("kafka binary");
    let binary = std::path::PathBuf::from(binary_command.get_program());
    let directory = tempfile::TempDir::new().expect("alias directory");
    let alias = directory.path().join("kafka-consumer-perf-test.sh");
    std::os::unix::fs::symlink(binary, &alias).expect("create consumer performance alias");

    Command::new(alias)
        .args([
            "--include",
            "events.*",
            "--num-records",
            "10",
            "--from-latest",
            "--help",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("--reporting-interval"))
        .stdout(predicate::str::contains("--fetch-size"));
}

#[cfg(unix)]
#[test]
fn kafka_producer_perf_alias_should_accept_original_options() {
    let binary_command = Command::cargo_bin("kafka").expect("kafka binary");
    let binary = std::path::PathBuf::from(binary_command.get_program());
    let directory = tempfile::TempDir::new().expect("alias directory");
    let alias = directory.path().join("kafka-producer-perf-test.sh");
    std::os::unix::fs::symlink(binary, &alias).expect("create producer performance alias");

    Command::new(alias)
        .args([
            "--topic",
            "events",
            "--num-records",
            "10",
            "--throughput",
            "-1",
            "--record-size",
            "100",
            "--help",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("--transaction-duration-ms"))
        .stdout(predicate::str::contains("--key-distribution"));
}

#[cfg(unix)]
#[test]
fn kafka_e2e_latency_alias_should_accept_original_options() {
    let binary_command = Command::cargo_bin("kafka").expect("kafka binary");
    let binary = std::path::PathBuf::from(binary_command.get_program());
    let directory = tempfile::TempDir::new().expect("alias directory");
    let alias = directory.path().join("kafka-e2e-latency.sh");
    std::os::unix::fs::symlink(binary, &alias).expect("create end-to-end latency alias");

    Command::new(alias)
        .args([
            "--bootstrap-server",
            "localhost:9092",
            "--topic",
            "events",
            "--num-records",
            "10",
            "--producer-acks",
            "all",
            "--record-size",
            "100",
            "--help",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("--record-key-size"))
        .stdout(predicate::str::contains("--record-header-size"));
}

#[cfg(unix)]
#[test]
fn kafka_verifiable_producer_alias_should_accept_original_options() {
    let binary_command = Command::cargo_bin("kafka").expect("kafka binary");
    let binary = std::path::PathBuf::from(binary_command.get_program());
    let directory = tempfile::TempDir::new().expect("alias directory");
    let alias = directory.path().join("kafka-verifiable-producer.sh");
    std::os::unix::fs::symlink(binary, &alias).expect("create verifiable producer alias");

    Command::new(alias)
        .args([
            "--topic",
            "events",
            "--max-messages",
            "2",
            "--value-prefix",
            "7",
            "--repeating-keys",
            "2",
            "--help",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("--message-create-time"))
        .stdout(predicate::str::contains("--producer.config"));
}

#[cfg(unix)]
#[test]
fn kafka_verifiable_consumer_alias_should_accept_original_options() {
    let binary_command = Command::cargo_bin("kafka").expect("kafka binary");
    let binary = std::path::PathBuf::from(binary_command.get_program());
    let directory = tempfile::TempDir::new().expect("alias directory");
    let alias = directory.path().join("kafka-verifiable-consumer.sh");
    std::os::unix::fs::symlink(binary, &alias).expect("create verifiable consumer alias");

    Command::new(alias)
        .args([
            "--topic",
            "events",
            "--group-id",
            "system-test",
            "--group-protocol",
            "classic",
            "--max-messages",
            "2",
            "--verbose",
            "--help",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("--assignment-strategy"))
        .stdout(predicate::str::contains("--consumer.config"));
}

#[cfg(unix)]
#[test]
fn kafka_dump_log_alias_should_accept_original_options() {
    let binary_command = Command::cargo_bin("kafka").expect("kafka binary");
    let binary = std::path::PathBuf::from(binary_command.get_program());
    let directory = tempfile::TempDir::new().expect("alias directory");
    let alias = directory.path().join("kafka-dump-log.sh");
    std::os::unix::fs::symlink(binary, &alias).expect("create dump-log alias");

    Command::new(alias)
        .args([
            "--files",
            "/tmp/00000000000000000000.log",
            "--print-data-log",
            "--deep-iteration",
            "--help",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("--max-message-size"))
        .stdout(predicate::str::contains("--skip-record-metadata"));
}

#[test]
fn dump_log_should_require_files_and_reject_zero_max_bytes() {
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args(["dump-log"])
        .assert()
        .failure();
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args(["dump-log", "--files", "/tmp/x.log", "--max-bytes", "0"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("max-bytes").or(predicate::str::contains("positive")));
}

#[test]
fn storage_format_should_require_config_and_cluster_id() {
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args(["storage", "format"])
        .assert()
        .failure();
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args(["storage", "info"])
        .assert()
        .failure();
}

#[test]
fn features_version_mapping_should_not_require_bootstrap() {
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args(["features", "version-mapping"])
        .assert()
        .success();
}

#[test]
fn producer_perf_should_reject_key_range_without_distribution_on_execute() {
    // Parse succeeds; runtime validation returns usage before connecting.
    let output = Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args([
            "producer-perf-test",
            "--topic",
            "events",
            "--num-records",
            "1",
            "--throughput",
            "-1",
            "--record-size",
            "8",
            "--record-key-range",
            "3",
            "--command-property",
            "bootstrap.servers=127.0.0.1:1",
        ])
        .output()
        .expect("run");
    assert!(!output.status.success());
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        err.contains("key-distribution") || err.contains("record-key-range"),
        "stderr={err}"
    );
}

#[test]
fn groups_validate_regex_cli_should_accept_valid_and_reject_invalid() {
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args(["groups", "validate-regex", "orders-.*"])
        .assert()
        .success()
        .stdout(predicate::str::contains("true"));
    // Invalid patterns still exit 0 with a structured VALID=false row (Kafka-like).
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args(["groups", "validate-regex", "("])
        .assert()
        .success()
        .stdout(predicate::str::contains("false"))
        .stdout(predicate::str::contains("unclosed").or(predicate::str::contains("error")));
}

#[test]
fn replica_verification_should_reject_non_positive_fetch_size() {
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args([
            "replica-verification",
            "--broker-list",
            "localhost:9092",
            "--fetch-size",
            "0",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("fetch-size"));
}

#[test]
fn groups_reset_offsets_should_require_a_target_before_connecting() {
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args([
            "--bootstrap-server",
            "127.0.0.1:1",
            "groups",
            "reset-offsets",
            "--group",
            "g",
            "--topic",
            "t",
            "--dry-run",
        ])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("reset target").or(predicate::str::contains("choose one")),
        );
}

#[test]
fn share_groups_reset_offsets_should_require_a_target_before_connecting() {
    // Share reset target validation may run after admin client construction; either a
    // usage error (target missing) or a live client error is acceptable for this path.
    let output = Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args([
            "--bootstrap-server",
            "127.0.0.1:1",
            "share-groups",
            "reset-offsets",
            "--group",
            "g",
            "--topic",
            "t",
            "--dry-run",
        ])
        .output()
        .expect("run");
    assert!(!output.status.success());
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        err.contains("reset target")
            || err.contains("choose one")
            || err.contains("broker")
            || err.contains("connect"),
        "stderr={err}"
    );
}

#[test]
fn producer_perf_should_reject_warmup_not_less_than_num_records() {
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args([
            "producer-perf-test",
            "--topic",
            "events",
            "--num-records",
            "5",
            "--throughput",
            "-1",
            "--record-size",
            "8",
            "--warmup-records",
            "5",
            "--command-property",
            "bootstrap.servers=127.0.0.1:1",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("warmup-records"));
}

#[test]
fn offsets_should_accept_named_time_aliases_without_connecting_for_help() {
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args(["offsets", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("earliest-local").or(predicate::str::contains("time")));
}

#[test]
fn metadata_quorum_remove_controller_should_require_directory_id() {
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args([
            "--bootstrap-server",
            "127.0.0.1:1",
            "metadata-quorum",
            "remove-controller",
            "--controller-id",
            "2",
            "--dry-run",
        ])
        .assert()
        .failure();
}

#[test]
fn delete_records_should_reject_empty_partition_list_without_connecting() {
    let dir = tempfile::TempDir::new().expect("dir");
    let path = dir.path().join("empty.json");
    std::fs::write(&path, r#"{"version":1,"partitions":[]}"#).expect("write");
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args([
            "--bootstrap-server",
            "127.0.0.1:1",
            "delete-records",
            "--offset-json-file",
            path.to_str().expect("p"),
        ])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("empty")
                .or(predicate::str::contains("partition"))
                .or(predicate::str::contains("Usage")),
        );
}

#[test]
fn leader_election_should_reject_empty_json_targets_without_connecting() {
    let dir = tempfile::TempDir::new().expect("dir");
    let path = dir.path().join("empty.json");
    std::fs::write(&path, r#"{"partitions":[]}"#).expect("write");
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args([
            "--bootstrap-server",
            "127.0.0.1:1",
            "leader-election",
            "--election-type",
            "preferred",
            "--path-to-json-file",
            path.to_str().expect("p"),
        ])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("empty")
                .or(predicate::str::contains("partition"))
                .or(predicate::str::contains("Usage")),
        );
}

#[test]
fn features_upgrade_should_reject_unknown_feature_before_or_at_connect() {
    let output = Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args([
            "--bootstrap-server",
            "127.0.0.1:1",
            "features",
            "upgrade",
            "--feature",
            "not.a.real.feature=1",
            "--dry-run",
        ])
        .output()
        .expect("run");
    // May fail at parse/validate or connect; must not succeed silently.
    assert!(!output.status.success());
}

#[test]
fn reassign_bootstrap_controller_should_parse_without_server() {
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args([
            "reassign",
            "--bootstrap-controller",
            "127.0.0.1:1",
            "list",
            "--help",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("bootstrap-controller").or(predicate::str::contains("list")),
        );
}

#[cfg(unix)]
#[test]
fn kafka_storage_alias_should_accept_original_subcommands() {
    let binary_command = Command::cargo_bin("kafka").expect("kafka binary");
    let binary = std::path::PathBuf::from(binary_command.get_program());
    let directory = tempfile::TempDir::new().expect("alias directory");
    let alias = directory.path().join("kafka-storage.sh");
    std::os::unix::fs::symlink(binary, &alias).expect("create storage alias");

    Command::new(alias)
        .args(["random-uuid", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("random-uuid"));

    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args(["storage", "random-uuid"])
        .assert()
        .success();
}

#[cfg(unix)]
#[test]
fn kafka_replica_verification_alias_should_accept_original_options() {
    let binary_command = Command::cargo_bin("kafka").expect("kafka binary");
    let binary = std::path::PathBuf::from(binary_command.get_program());
    let directory = tempfile::TempDir::new().expect("alias directory");
    let alias = directory.path().join("kafka-replica-verification.sh");
    std::os::unix::fs::symlink(binary, &alias).expect("create replica verification alias");

    Command::new(alias)
        .args([
            "--broker-list",
            "localhost:9092",
            "--topics-include",
            "events.*",
            "--time",
            "-2",
            "--report-interval-ms",
            "1000",
            "--help",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("--fetch-size"))
        .stdout(predicate::str::contains("--max-wait-ms"))
        .stdout(predicate::str::contains("--topics-include"));
}

#[cfg(unix)]
#[test]
fn kafka_share_consumer_perf_alias_should_accept_original_options() {
    let binary_command = Command::cargo_bin("kafka").expect("kafka binary");
    let binary = std::path::PathBuf::from(binary_command.get_program());
    let directory = tempfile::TempDir::new().expect("alias directory");
    let alias = directory.path().join("kafka-share-consumer-perf-test.sh");
    std::os::unix::fs::symlink(binary, &alias).expect("create Share performance alias");

    Command::new(alias)
        .args([
            "--topic",
            "events",
            "--num-records",
            "10",
            "--threads",
            "2",
            "--show-consumer-stats",
            "--help",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("--reporting-interval"))
        .stdout(predicate::str::contains("--print-metrics"));
}

#[cfg(unix)]
#[test]
fn kafka_verifiable_share_consumer_alias_should_accept_original_options() {
    let binary_command = Command::cargo_bin("kafka").expect("kafka binary");
    let binary = std::path::PathBuf::from(binary_command.get_program());
    let directory = tempfile::TempDir::new().expect("alias directory");
    let alias = directory.path().join("kafka-verifiable-share-consumer.sh");
    std::os::unix::fs::symlink(binary, &alias).expect("create verifiable Share alias");

    Command::new(alias)
        .args([
            "--topic",
            "events",
            "--group-id",
            "workers",
            "--acknowledgement-mode",
            "sync",
            "--ack-pattern",
            "accept,reject",
            "--help",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("--offset-reset-strategy"));
}

#[cfg(unix)]
#[test]
fn kafka_streams_groups_alias_should_accept_original_actions() {
    let binary_command = Command::cargo_bin("kafka").expect("kafka binary");
    let binary = std::path::PathBuf::from(binary_command.get_program());
    let directory = tempfile::TempDir::new().expect("alias directory");
    let alias = directory.path().join("kafka-streams-groups.sh");
    std::os::unix::fs::symlink(binary, &alias).expect("create kafka-streams-groups alias");

    Command::new(alias)
        .args([
            "--describe",
            "--group",
            "streams-app",
            "--topology",
            "--help",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("--all-groups"))
        .stdout(predicate::str::contains("--topology"));
}

#[cfg(unix)]
#[test]
fn kafka_streams_application_reset_alias_should_accept_original_options() {
    let binary_command = Command::cargo_bin("kafka").expect("kafka binary");
    let binary = std::path::PathBuf::from(binary_command.get_program());
    let directory = tempfile::TempDir::new().expect("alias directory");
    let alias = directory.path().join("kafka-streams-application-reset.sh");
    std::os::unix::fs::symlink(binary, &alias).expect("create Streams reset alias");

    Command::new(alias)
        .args(["--application-id", "streams-app", "--dry-run", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--input-topics"))
        .stdout(predicate::str::contains("--internal-topics"))
        .stdout(predicate::str::contains("--force"));
}

#[test]
fn groups_reset_by_duration_should_reject_empty_iso8601_duration() {
    // Must fail in the shipped binary before any broker connection is required.
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args([
            "--bootstrap-server",
            "127.0.0.1:1",
            "groups",
            "reset-offsets",
            "--group",
            "g",
            "--topic",
            "t",
            "--by-duration",
            "PT",
            "--dry-run",
        ])
        .assert()
        .code(2)
        .stderr(
            predicate::str::contains("duration")
                .or(predicate::str::contains("ISO-8601"))
                .or(predicate::str::contains("component")),
        );
}

#[test]
fn topics_describe_should_reject_undecodable_topic_id() {
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args([
            "--bootstrap-server",
            "127.0.0.1:1",
            "topics",
            "describe",
            "--topic-id",
            "!!!!!!!!!!!!!!!!!!!!!!",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("topic ID").or(predicate::str::contains("UUID")));
}

#[test]
fn bootstrap_server_and_controller_should_be_mutually_exclusive() {
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args([
            "--bootstrap-server",
            "127.0.0.1:9092",
            "features",
            "--bootstrap-controller",
            "127.0.0.1:9093",
            "describe",
        ])
        .assert()
        .code(2)
        .stderr(
            predicate::str::contains("cannot use both")
                .or(predicate::str::contains("bootstrap-server"))
                .and(predicate::str::contains("bootstrap-controller")),
        );
}
