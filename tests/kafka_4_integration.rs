//! End-to-end command-family coverage against a real Kafka 4.x broker.

use std::{fs, process::Output, thread, time::Duration};

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;
use testcontainers::{core::ImageExt, runners::SyncRunner};
use testcontainers_modules::kafka::apache;

fn kafka(bootstrap: &str, arguments: &[&str]) -> Output {
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .arg("--bootstrap-server")
        .arg(bootstrap)
        .arg("--timeout-ms")
        .arg("15000")
        .args(arguments)
        .output()
        .expect("execute kafka command")
}

fn success(bootstrap: &str, arguments: &[&str]) -> String {
    let output = kafka(bootstrap, arguments);
    assert!(
        output.status.success(),
        "kafka {arguments:?} failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("UTF-8 stdout")
}

fn eventually_contains(bootstrap: &str, arguments: &[&str], expected: &str) -> String {
    let mut last_output = String::new();
    for _ in 0..20 {
        last_output = success(bootstrap, arguments);
        if last_output.contains(expected) {
            return last_output;
        }
        thread::sleep(Duration::from_millis(250));
    }
    panic!(
        "kafka {arguments:?} did not contain {expected:?} after bounded retries\nlast stdout: {last_output}"
    );
}

#[test]
#[ignore = "requires Docker and downloads apache/kafka:4.3.1"]
#[expect(
    clippy::too_many_lines,
    reason = "one broker fixture exercises the complete command-family matrix"
)]
fn all_command_families_work_against_kafka_4_3_1() {
    let broker = apache::Kafka::default()
        .with_jvm_image()
        .with_tag("4.3.1")
        .with_env_var(
            "KAFKA_AUTHORIZER_CLASS_NAME",
            "org.apache.kafka.metadata.authorizer.StandardAuthorizer",
        )
        .with_env_var("KAFKA_ALLOW_EVERYONE_IF_NO_ACL_FOUND", "true")
        .with_env_var("KAFKA_SUPER_USERS", "User:ANONYMOUS")
        .with_env_var("KAFKA_NUM_PARTITIONS", "3")
        .start()
        .expect("start Kafka 4.3.1");
    let port = broker
        .get_host_port_ipv4(apache::KAFKA_PORT)
        .expect("mapped Kafka port");
    let bootstrap = format!("127.0.0.1:{port}");
    let fixture = TempDir::new().expect("fixture directory");
    let delete_file = fixture.path().join("delete.json");
    let topics_file = fixture.path().join("topics.json");
    let reassignment_file = fixture.path().join("reassignment.json");
    let invalid_reassignment_file = fixture.path().join("invalid-reassignment.json");
    let election_file = fixture.path().join("election.json");
    let reset_file = fixture.path().join("reset.csv");
    let topic_config_file = fixture.path().join("topic.properties");
    let reader_config_file = fixture.path().join("reader.properties");
    let formatter_config_file = fixture.path().join("formatter.properties");
    fs::write(
        &delete_file,
        r#"{"partitions":[{"topic":"integration-events","partition":0,"offset":1}]}"#,
    )
    .expect("delete-records fixture");
    fs::write(
        &topics_file,
        r#"{"topics":[{"topic":"integration-events"}],"version":1}"#,
    )
    .expect("topics fixture");
    fs::write(
        &reader_config_file,
        "parse.headers=true\nheaders.delimiter=|\nparse.key=false\nkey.separator=:\nnull.marker=NULL\n",
    )
    .expect("reader properties fixture");
    fs::write(
        &formatter_config_file,
        "print.partition=true\nprint.offset=true\nprint.headers=true\nprint.key=true\nkey.separator=:\nheaders.separator=;\nnull.literal=NULL\n",
    )
    .expect("formatter properties fixture");
    fs::write(
        &reassignment_file,
        r#"{"version":1,"partitions":[{"topic":"integration-events","partition":0,"replicas":[1],"log_dirs":["any"]}]}"#,
    )
    .expect("reassignment fixture");
    fs::write(
        &election_file,
        r#"{"partitions":[{"topic":"integration-events","partition":0},{"topic":"manual-assignment","partition":1}]}"#,
    )
    .expect("leader-election fixture");
    fs::write(
        &topic_config_file,
        "retention.ms=60000\nsegment.bytes=1048576\n",
    )
    .expect("topic config fixture");

    // topics
    let topic_created: serde_json::Value = serde_json::from_str(&success(
        &bootstrap,
        &[
            "--output",
            "json",
            "topics",
            "create",
            "--topic",
            "integration-events",
            "--partitions",
            "1",
        ],
    ))
    .expect("topic create JSON");
    assert_eq!(topic_created["command"], "topics.create");
    success(
        &bootstrap,
        &["topics", "create", "--topic", "broker-default-partitions"],
    );
    let broker_default: serde_json::Value = serde_json::from_str(&success(
        &bootstrap,
        &[
            "--output",
            "json",
            "topics",
            "describe",
            "--topic",
            "broker-default-partitions",
        ],
    ))
    .expect("broker-default topic JSON");
    assert_eq!(
        broker_default["data"]
            .as_array()
            .expect("broker-default partitions")
            .len(),
        3
    );
    success(
        &bootstrap,
        &[
            "topics",
            "create",
            "--topic",
            "manual-assignment",
            "--replica-assignment",
            "1",
        ],
    );
    success(
        &bootstrap,
        &[
            "topics",
            "alter",
            "--topic",
            "manual-assignment",
            "--partitions",
            "2",
            "--replica-assignment",
            "1,1",
        ],
    );
    assert!(
        success(
            &bootstrap,
            &[
                "--output",
                "json",
                "topics",
                "describe",
                "--topic",
                "manual-assignment",
            ]
        )
        .contains("\"partition\": 1")
    );
    let election_preview = success(
        &bootstrap,
        &[
            "leader-election",
            "--election-type",
            "preferred",
            "--path-to-json-file",
            election_file.to_str().expect("fixture path"),
        ],
    );
    assert!(election_preview.contains("integration-events"));
    assert!(election_preview.contains("manual-assignment"));
    success(
        &bootstrap,
        &[
            "leader-election",
            "--election-type",
            "preferred",
            "--path-to-json-file",
            election_file.to_str().expect("fixture path"),
            "--execute",
        ],
    );
    let described_topic: serde_json::Value = serde_json::from_str(&success(
        &bootstrap,
        &[
            "--output",
            "json",
            "topics",
            "describe",
            "--topic",
            "integration-events",
        ],
    ))
    .expect("topic description JSON");
    let topic_id = described_topic["data"][0]["topic_id"]
        .as_str()
        .expect("topic ID");
    assert!(
        success(&bootstrap, &["topics", "describe", "--topic-id", topic_id])
            .contains("integration-events")
    );
    assert!(
        success(
            &bootstrap,
            &[
                "topics",
                "describe",
                "--topic",
                "does-not-match",
                "--topic-id",
                topic_id,
                "--partition-size-limit-per-response",
                "1",
            ],
        )
        .contains("integration-events")
    );
    success(
        &bootstrap,
        &[
            "topics",
            "describe",
            "--topic",
            "does-not-exist",
            "--if-exists",
        ],
    );
    success(
        &bootstrap,
        &[
            "topics",
            "create",
            "--topic",
            "under-min-isr",
            "--partitions",
            "1",
            "--config",
            "min.insync.replicas=2",
        ],
    );
    let described_with_config = success(
        &bootstrap,
        &["topics", "describe", "--topic", "under-min-isr"],
    );
    assert!(described_with_config.contains("REPLICATION_FACTOR"));
    assert!(described_with_config.contains("min.insync.replicas=2"));
    assert!(
        success(
            &bootstrap,
            &[
                "topics",
                "describe",
                "--topic",
                "under-min-isr",
                "--under-min-isr-partitions",
            ]
        )
        .contains("under-min-isr")
    );
    assert!(
        success(
            &bootstrap,
            &[
                "topics",
                "describe",
                "--topic",
                "under-min-isr",
                "--topics-with-overrides",
            ]
        )
        .contains("min.insync.replicas=2")
    );
    assert!(
        success(
            &bootstrap,
            &[
                "topics",
                "describe",
                "--topic",
                "manual-assignment",
                "--at-min-isr-partitions",
            ]
        )
        .contains("manual-assignment")
    );
    assert!(success(&bootstrap, &["topics", "list"]).contains("integration-events"));
    assert!(
        success(&bootstrap, &["topics", "list", "--topic", "integration-.*"])
            .contains("integration-events")
    );
    assert!(
        success(
            &bootstrap,
            &["topics", "describe", "--topic", "integration-events"]
        )
        .contains("integration-events")
    );
    success(
        &bootstrap,
        &[
            "topics",
            "describe",
            "--topic",
            "integration-events",
            "--under-replicated-partitions",
        ],
    );
    success(
        &bootstrap,
        &[
            "topics",
            "alter",
            "--topic",
            "integration-events",
            "--partitions",
            "2",
        ],
    );
    for topic in ["regex-alter-a", "regex-alter-b"] {
        success(
            &bootstrap,
            &["topics", "create", "--topic", topic, "--partitions", "1"],
        );
    }
    let regex_alter = success(
        &bootstrap,
        &[
            "topics",
            "alter",
            "--topic",
            "regex-alter-.*",
            "--partitions",
            "2",
        ],
    );
    assert!(regex_alter.contains("regex-alter-a"));
    assert!(regex_alter.contains("regex-alter-b"));
    assert!(
        success(
            &bootstrap,
            &["topics", "describe", "--topic", "regex-alter-.*"],
        )
        .contains("regex-alter-b")
    );

    // produce and consume
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "topics",
            "create",
            "--topic",
            "integration-json",
            "--partitions",
            "1",
        ])
        .assert()
        .success();
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "produce",
            "--topic",
            "integration-json",
            "--parse-key",
            "--sync",
            "--compression-codec",
            "--batch-size",
            "8192",
            "--max-partition-memory-bytes",
            "16384",
            "--message-send-max-retries",
            "4",
            "--retry-backoff-ms",
            "50",
            "--timeout",
            "5",
            "--request-timeout-ms",
            "5000",
            "--metadata-expiry-ms",
            "60000",
            "--max-block-ms",
            "5000",
            "--max-memory-bytes",
            "33554432",
            "--socket-buffer-size",
            "102400",
            "--command-property",
            "client.id=integration-producer",
            "--command-property",
            "buffer.memory=1048576",
            "--command-property",
            "send.buffer.bytes=4096",
            "--command-property",
            "max.block.ms=1000",
        ])
        .write_stdin("alpha\tfirst\nbeta\tsecond\n")
        .assert()
        .success();
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "consume",
            "--topic",
            "integration-json",
            "--max-messages",
            "0",
        ])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "consume",
            "--topic",
            "integration-json",
            "--from-beginning",
            "--max-messages",
            "2",
            "--group",
            "integration-suite",
            "--command-property",
            "client.id=integration-consumer",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("first"));
    for _ in 0..2 {
        Command::cargo_bin("kafka")
            .expect("kafka binary")
            .args([
                "--bootstrap-server",
                &bootstrap,
                "consume",
                "--topic",
                "integration-json",
                "--from-beginning",
                "--max-messages",
                "1",
                "--timeout-ms",
                "10000",
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains("first"));
    }
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "consume",
            "--include",
            "integration-json",
            "--from-beginning",
            "--max-messages",
            "2",
            "--timeout-ms",
            "10000",
            "--isolation-level",
            "read_committed",
            "--skip-message-on-error",
            "--group",
            "integration-regex",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("second"));
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "produce",
            "--topic",
            "integration-json",
            "--line-reader",
            "org.apache.kafka.tools.LineMessageReader",
            "--sync",
            "--reader-config",
            reader_config_file.to_str().expect("reader fixture path"),
            "--reader-property",
            "parse.key=true",
            "--reader-property",
            "key.separator=|",
            "--reader-property",
            "null.marker=NULL",
        ])
        .write_stdin("trace:abc,empty:NULL|prop-key|prop-value\n")
        .assert()
        .success();
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "consume",
            "--topic",
            "integration-json",
            "--formatter",
            "org.apache.kafka.tools.consumer.DefaultMessageFormatter",
            "--key-deserializer",
            "org.apache.kafka.common.serialization.StringDeserializer",
            "--value-deserializer",
            "org.apache.kafka.common.serialization.StringDeserializer",
            "--from-beginning",
            "--max-messages",
            "3",
            "--group",
            "integration-formatter",
            "--formatter-config",
            formatter_config_file
                .to_str()
                .expect("formatter fixture path"),
            "--formatter-property",
            "key.separator=|",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Partition:0|Offset:2|trace:abc;empty:NULL|prop-key|prop-value",
        ));
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "produce",
            "--topic",
            "integration-events",
            "--json",
        ])
        .write_stdin(
            "{\"key\":\"json-key\",\"value\":\"json-value\",\"partition\":0,\"headers\":{\"trace\":\"abc\"}}\n",
        )
        .assert()
        .success();
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "consume",
            "--topic",
            "integration-events",
            "--partition",
            "0",
            "--max-messages",
            "1",
            "--offset",
            "earliest",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("json-value"))
        .stdout(predicate::str::contains("trace"));

    // groups, configs, offsets
    success(&bootstrap, &["groups", "list"]);
    assert!(success(&bootstrap, &["groups", "list", "--state"]).contains("integration-suite"));
    assert!(
        success(&bootstrap, &["groups", "list", "--type", "classic"]).contains("integration-suite")
    );
    assert!(
        success(
            &bootstrap,
            &[
                "groups",
                "describe",
                "--group",
                "integration-suite",
                "--state",
            ]
        )
        .contains("integration-suite")
    );
    let verbose_members = success(
        &bootstrap,
        &[
            "groups",
            "describe",
            "--group",
            "integration-suite",
            "--members",
            "--verbose",
        ],
    );
    assert!(verbose_members.contains("ASSIGNMENT"));
    assert!(
        success(
            &bootstrap,
            &["groups", "describe", "--all-groups", "--state"]
        )
        .contains("integration-suite")
    );
    assert!(
        success(&bootstrap, &["groups", "describe", "--all-groups"]).contains("integration-json")
    );
    assert!(
        success(
            &bootstrap,
            &[
                "groups",
                "describe",
                "--group",
                "integration-suite",
                "--verbose"
            ]
        )
        .contains("LEADER_EPOCH")
    );
    success(
        &bootstrap,
        &[
            "groups",
            "reset-offsets",
            "--group",
            "integration-suite",
            "--topic",
            "integration-events",
            "--to-earliest",
            "--execute",
        ],
    );
    let described_offsets = success(
        &bootstrap,
        &["groups", "describe", "--group", "integration-suite"],
    );
    assert!(
        described_offsets.contains("integration-events"),
        "unexpected group offsets output: {described_offsets}"
    );
    let selected_reset: serde_json::Value = serde_json::from_str(&success(
        &bootstrap,
        &[
            "--output",
            "json",
            "groups",
            "reset-offsets",
            "--group",
            "integration-suite",
            "--topic",
            "integration-events:0",
            "--to-earliest",
        ],
    ))
    .expect("partition-selected reset JSON");
    let selected_rows = selected_reset["data"].as_array().expect("reset rows");
    assert_eq!(selected_rows.len(), 1);
    assert_eq!(selected_rows[0]["partition"], 0);
    let merged_reset: serde_json::Value = serde_json::from_str(&success(
        &bootstrap,
        &[
            "--output",
            "json",
            "groups",
            "reset-offsets",
            "--group",
            "integration-suite",
            "--topic",
            "integration-events:0",
            "--topic",
            "integration-events:1",
            "--to-earliest",
        ],
    ))
    .expect("merged reset JSON");
    assert_eq!(
        merged_reset["data"].as_array().expect("reset rows").len(),
        2
    );
    let exported_reset = success(
        &bootstrap,
        &[
            "groups",
            "reset-offsets",
            "--group",
            "integration-suite",
            "--topic",
            "integration-events",
            "--to-earliest",
            "--export",
        ],
    );
    assert_eq!(
        exported_reset
            .lines()
            .next()
            .expect("exported reset row")
            .split(',')
            .count(),
        3
    );
    fs::write(&reset_file, &exported_reset).expect("reset CSV fixture");
    let imported_reset = success(
        &bootstrap,
        &[
            "groups",
            "reset-offsets",
            "--group",
            "integration-suite",
            "--from-file",
            reset_file.to_str().expect("fixture path"),
            "--execute",
        ],
    );
    assert!(imported_reset.contains("integration-events"));
    let reset_all = success(
        &bootstrap,
        &[
            "groups",
            "reset-offsets",
            "--all-groups",
            "--all-topics",
            "--to-earliest",
            "--dry-run",
        ],
    );
    assert!(reset_all.contains("GROUP"));
    assert!(reset_all.contains("integration-suite"));
    assert!(reset_all.contains("integration-events"));
    success(
        &bootstrap,
        &[
            "groups",
            "reset-offsets",
            "--group",
            "integration-suite",
            "--topic",
            "integration-events",
            "--shift-by",
            "1",
        ],
    );
    success(
        &bootstrap,
        &[
            "groups",
            "reset-offsets",
            "--group",
            "integration-suite",
            "--topic",
            "integration-events",
            "--to-current",
        ],
    );
    success(
        &bootstrap,
        &[
            "groups",
            "reset-offsets",
            "--group",
            "integration-suite",
            "--topic",
            "integration-events",
            "--to-datetime",
            "2099-01-01T00:00:00.000Z",
        ],
    );
    success(
        &bootstrap,
        &[
            "groups",
            "reset-offsets",
            "--group",
            "integration-suite",
            "--topic",
            "integration-events",
            "--by-duration",
            "P1D",
        ],
    );
    success(
        &bootstrap,
        &[
            "groups",
            "delete-offsets",
            "--group",
            "integration-suite",
            "--topic",
            "integration-events:0",
            "--topic",
            "integration-json",
            "--execute",
        ],
    );
    let mut active_consumer = std::process::Command::new(assert_cmd::cargo::cargo_bin!("kafka"))
        .args([
            "--bootstrap-server",
            &bootstrap,
            "consume",
            "--topic",
            "integration-events",
            "--group",
            "active-reset-group",
            "--timeout-ms",
            "20000",
        ])
        .spawn()
        .expect("start active consumer");
    let mut active = false;
    for _ in 0..20 {
        let state = success(
            &bootstrap,
            &[
                "groups",
                "describe",
                "--group",
                "active-reset-group",
                "--state",
            ],
        );
        if state.contains("Stable") {
            active = true;
            break;
        }
        thread::sleep(Duration::from_millis(250));
    }
    assert!(active, "consumer group did not become active");
    let active_reset: serde_json::Value = serde_json::from_str(&success(
        &bootstrap,
        &[
            "--output",
            "json",
            "groups",
            "reset-offsets",
            "--group",
            "active-reset-group",
            "--topic",
            "integration-events",
            "--to-earliest",
            "--execute",
        ],
    ))
    .expect("active group reset JSON");
    assert!(
        active_reset["data"]
            .as_array()
            .expect("reset rows")
            .is_empty()
    );
    assert!(
        active_reset["errors"][0]
            .as_str()
            .expect("reset error")
            .contains("current state is Stable")
    );
    active_consumer.kill().expect("stop active consumer");
    active_consumer.wait().expect("wait for active consumer");
    let delete_preview: serde_json::Value = serde_json::from_str(&success(
        &bootstrap,
        &["--output", "json", "groups", "delete", "--all-groups"],
    ))
    .expect("group delete preview JSON");
    assert_eq!(delete_preview["command"], "groups.delete");
    assert!(
        delete_preview["data"]
            .as_array()
            .expect("delete rows")
            .iter()
            .any(|row| row["group"] == "integration-suite" && row["status"] == "PREVIEW")
    );
    success(
        &bootstrap,
        &[
            "groups",
            "delete",
            "--group",
            "integration-suite",
            "--execute",
        ],
    );
    success(
        &bootstrap,
        &[
            "configs",
            "alter",
            "--entity-type",
            "brokers",
            "--entity-default",
            "--add-config",
            "message.max.bytes=1048588",
            "--execute",
        ],
    );
    eventually_contains(
        &bootstrap,
        &[
            "configs",
            "describe",
            "--entity-type",
            "brokers",
            "--entity-default",
        ],
        "message.max.bytes",
    );
    success(
        &bootstrap,
        &[
            "configs",
            "alter",
            "--entity-type",
            "brokers",
            "--entity-default",
            "--delete-config",
            "message.max.bytes",
            "--execute",
        ],
    );
    success(
        &bootstrap,
        &[
            "configs",
            "describe",
            "--entity-type",
            "topic",
            "--entity-name",
            "integration-events",
        ],
    );
    let all_topic_configs = success(
        &bootstrap,
        &["configs", "describe", "--entity-type", "topics", "--all"],
    );
    assert!(all_topic_configs.contains("integration-events"));
    assert!(all_topic_configs.contains("cleanup.policy"));
    success(
        &bootstrap,
        &[
            "configs",
            "alter",
            "--entity-type",
            "topic",
            "--entity-name",
            "integration-events",
            "--add-config-file",
            topic_config_file.to_str().expect("fixture path"),
            "--execute",
        ],
    );
    success(
        &bootstrap,
        &[
            "configs",
            "alter",
            "--entity-type",
            "topic",
            "--entity-name",
            "integration-events",
            "--delete-config",
            "retention.ms",
            "--delete-config",
            "segment.bytes",
            "--execute",
        ],
    );
    success(
        &bootstrap,
        &[
            "configs",
            "alter",
            "--entity-type",
            "clients",
            "--entity-name",
            "integration-client",
            "--add-config",
            "producer_byte_rate=1048576",
            "--execute",
        ],
    );
    eventually_contains(
        &bootstrap,
        &[
            "configs",
            "describe",
            "--entity-type",
            "clients",
            "--entity-name",
            "integration-client",
        ],
        "producer_byte_rate",
    );
    success(
        &bootstrap,
        &[
            "configs",
            "alter",
            "--entity-type",
            "users",
            "--entity-type",
            "clients",
            "--entity-name",
            "integration-user",
            "--entity-name",
            "integration-client",
            "--add-config",
            "request_percentage=25",
            "--execute",
        ],
    );
    eventually_contains(
        &bootstrap,
        &[
            "configs",
            "describe",
            "--entity-type",
            "users",
            "--entity-type",
            "clients",
            "--entity-name",
            "integration-user",
            "--entity-name",
            "integration-client",
        ],
        "request_percentage",
    );
    success(
        &bootstrap,
        &[
            "configs",
            "alter",
            "--entity-type",
            "ips",
            "--entity-default",
            "--add-config",
            "connection_creation_rate=1000",
            "--execute",
        ],
    );
    eventually_contains(
        &bootstrap,
        &[
            "configs",
            "describe",
            "--entity-type",
            "ips",
            "--entity-default",
        ],
        "connection_creation_rate",
    );
    success(
        &bootstrap,
        &[
            "configs",
            "alter",
            "--entity-type",
            "broker-loggers",
            "--entity-name",
            "1",
            "--add-config",
            "kafka.server.KafkaApis=INFO",
            "--execute",
        ],
    );
    eventually_contains(
        &bootstrap,
        &[
            "configs",
            "describe",
            "--entity-type",
            "broker-loggers",
            "--entity-name",
            "1",
        ],
        "kafka.server.KafkaApis",
    );
    success(
        &bootstrap,
        &[
            "configs",
            "alter",
            "--entity-type",
            "client-metrics",
            "--entity-name",
            "integration-metrics",
            "--add-config",
            "metrics=org.apache",
            "--execute",
        ],
    );
    eventually_contains(
        &bootstrap,
        &["configs", "describe", "--entity-type", "client-metrics"],
        "integration-metrics",
    );
    let scram_alter: serde_json::Value = serde_json::from_str(&success(
        &bootstrap,
        &[
            "--output",
            "json",
            "configs",
            "alter",
            "--entity-type",
            "users",
            "--entity-name",
            "integration-user",
            "--add-config",
            "SCRAM-SHA-256=[iterations=4096,password=integration-secret]",
            "--execute",
        ],
    ))
    .expect("SCRAM alter JSON");
    assert_eq!(scram_alter["command"], "configs.alter.scram");
    assert!(
        success(
            &bootstrap,
            &[
                "configs",
                "describe",
                "--entity-type",
                "users",
                "--entity-name",
                "integration-user",
            ],
        )
        .contains("SCRAM-SHA-256")
    );
    success(
        &bootstrap,
        &[
            "configs",
            "alter",
            "--entity-type",
            "users",
            "--entity-name",
            "integration-user",
            "--delete-config",
            "SCRAM-SHA-256",
            "--execute",
        ],
    );
    success(
        &bootstrap,
        &[
            "configs",
            "alter",
            "--entity-type",
            "broker-loggers",
            "--entity-name",
            "1",
            "--delete-config",
            "kafka.server.KafkaApis",
            "--execute",
        ],
    );
    success(
        &bootstrap,
        &[
            "configs",
            "alter",
            "--entity-type",
            "client-metrics",
            "--entity-name",
            "integration-metrics",
            "--delete-config",
            "metrics",
            "--execute",
        ],
    );
    success(
        &bootstrap,
        &[
            "configs",
            "alter",
            "--entity-type",
            "clients",
            "--entity-name",
            "integration-client",
            "--delete-config",
            "producer_byte_rate",
            "--execute",
        ],
    );
    success(
        &bootstrap,
        &[
            "configs",
            "alter",
            "--entity-type",
            "users",
            "--entity-type",
            "clients",
            "--entity-name",
            "integration-user",
            "--entity-name",
            "integration-client",
            "--delete-config",
            "request_percentage",
            "--execute",
        ],
    );
    success(
        &bootstrap,
        &[
            "configs",
            "alter",
            "--entity-type",
            "ips",
            "--entity-default",
            "--delete-config",
            "connection_creation_rate",
            "--execute",
        ],
    );
    assert!(
        success(&bootstrap, &["offsets", "--topic", "integration-events"])
            .contains("integration-events")
    );
    assert!(
        success(
            &bootstrap,
            &[
                "offsets",
                "--topic",
                "integration-events",
                "--timestamp",
                "0",
            ]
        )
        .contains("integration-events")
    );
    assert!(
        success(
            &bootstrap,
            &["offsets", "--topic-partitions", "integration-events:0-2",]
        )
        .contains("integration-events")
    );
    let trailing_rule = success(
        &bootstrap,
        &["offsets", "--topic-partitions", "integration-events:0,"],
    );
    assert!(trailing_rule.contains("integration-events"));
    assert!(!trailing_rule.contains("manual-assignment"));
    assert!(
        success(
            &bootstrap,
            &[
                "offsets",
                "--topic",
                "integration-events",
                "--time",
                "max-timestamp",
            ]
        )
        .contains("integration-events")
    );
    assert!(
        success(
            &bootstrap,
            &[
                "offsets",
                "--topic",
                "integration-events",
                "--time",
                "earliest-local",
            ],
        )
        .contains("integration-events")
    );
    for time in ["latest-tiered", "earliest-pending-upload"] {
        success(
            &bootstrap,
            &["offsets", "--topic", "integration-events", "--time", time],
        );
    }

    // ACL and destructive commands use their mandatory safe preview mode.
    success(&bootstrap, &["acls", "list"]);
    let acl_created: serde_json::Value = serde_json::from_str(&success(
        &bootstrap,
        &[
            "--output",
            "json",
            "acls",
            "add",
            "--topic",
            "integration-events",
            "--principal",
            "User:integration",
            "--operation",
            "read",
            "--execute",
        ],
    ))
    .expect("ACL create JSON");
    assert_eq!(acl_created["command"], "acls.add");
    assert!(
        success(
            &bootstrap,
            &[
                "acls",
                "list",
                "--topic",
                "integration-events",
                "--principal",
                "User:integration",
            ]
        )
        .contains("User:integration")
    );
    success(
        &bootstrap,
        &[
            "acls",
            "add",
            "--topic",
            "integration-",
            "--resource-pattern-type",
            "prefixed",
            "--allow-principal",
            "User:prefix-reader",
            "--operation",
            "read",
            "--execute",
        ],
    );
    assert!(
        success(
            &bootstrap,
            &[
                "acls",
                "list",
                "--topic",
                "integration-",
                "--resource-pattern-type",
                "prefixed",
                "--principal",
                "User:prefix-reader",
            ]
        )
        .contains("Prefixed")
    );
    success(
        &bootstrap,
        &[
            "acls",
            "add",
            "--cluster",
            "--allow-principal",
            "User:operator",
            "--deny-principal",
            "User:blocked",
            "--operation",
            "cluster-action",
            "--execute",
        ],
    );
    assert!(success(&bootstrap, &["acls", "list", "--cluster"]).contains("User:operator"));
    success(
        &bootstrap,
        &[
            "acls",
            "remove",
            "--topic",
            "integration-",
            "--resource-pattern-type",
            "prefixed",
            "--principal",
            "User:prefix-reader",
            "--operation",
            "read",
            "--execute",
        ],
    );
    success(
        &bootstrap,
        &[
            "acls",
            "remove",
            "--topic",
            "integration-events",
            "--principal",
            "User:integration",
            "--operation",
            "read",
            "--execute",
        ],
    );
    let deleted_records: serde_json::Value = serde_json::from_str(&success(
        &bootstrap,
        &[
            "--output",
            "json",
            "delete-records",
            "--offset-json-file",
            delete_file.to_str().expect("fixture path"),
            "--execute",
        ],
    ))
    .expect("delete-records JSON");
    assert_eq!(deleted_records["command"], "delete-records");
    success(
        &bootstrap,
        &[
            "leader-election",
            "--election-type",
            "preferred",
            "--topic",
            "integration-events",
            "--partition",
            "0",
            "--execute",
        ],
    );

    // reassignment, log directories, API versions, and cluster metadata
    let log_dirs: serde_json::Value =
        serde_json::from_str(&success(&bootstrap, &["--output", "json", "log-dirs"]))
            .expect("log-dirs JSON");
    let log_dir = log_dirs["data"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["topic"].as_str() == Some("integration-events"))
        })
        .and_then(|row| row["log_dir"].as_str())
        .expect("integration topic log directory");
    let filtered_log_dirs = success(
        &bootstrap,
        &["log-dirs", "--topic-list", "integration-events"],
    );
    assert!(filtered_log_dirs.contains("integration-events"));
    fs::write(
        &reassignment_file,
        serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "partitions": [{
                "topic": "integration-events",
                "partition": 0,
                "replicas": [1],
                "log_dirs": [log_dir]
            }]
        }))
        .expect("reassignment JSON"),
    )
    .expect("write log-dir reassignment fixture");
    let proposal = success(
        &bootstrap,
        &[
            "--output",
            "json",
            "reassign",
            "generate",
            "--topics-to-move-json-file",
            topics_file.to_str().expect("fixture path"),
            "--broker-list",
            "1",
        ],
    );
    assert!(proposal.contains("integration-events"));
    let reassignment_preview: serde_json::Value = serde_json::from_str(&success(
        &bootstrap,
        &[
            "--output",
            "json",
            "reassign",
            "execute",
            "--reassignment-json-file",
            reassignment_file.to_str().expect("fixture path"),
        ],
    ))
    .expect("reassignment preview JSON");
    assert_eq!(reassignment_preview["command"], "reassign.execute");
    success(
        &bootstrap,
        &[
            "reassign",
            "execute",
            "--reassignment-json-file",
            reassignment_file.to_str().expect("fixture path"),
            "--disallow-replication-factor-change",
            "--throttle",
            "1048576",
            "--replica-alter-log-dirs-throttle",
            "524288",
            "--execute",
        ],
    );
    let topic_throttles = success(
        &bootstrap,
        &[
            "configs",
            "describe",
            "--entity-type",
            "topics",
            "--entity-name",
            "integration-events",
        ],
    );
    assert!(topic_throttles.contains("leader.replication.throttled.replicas"));
    let broker_throttles = success(
        &bootstrap,
        &[
            "configs",
            "describe",
            "--entity-type",
            "brokers",
            "--entity-name",
            "1",
        ],
    );
    assert!(broker_throttles.contains("leader.replication.throttled.rate"));
    assert!(broker_throttles.contains("replica.alter.log.dirs.io.max.bytes.per.second"));
    fs::write(
        &invalid_reassignment_file,
        r#"{"version":1,"partitions":[{"topic":"missing-reassignment-topic","partition":0,"replicas":[1]}]}"#,
    )
    .expect("invalid reassignment fixture");
    let failed_reassignment = kafka(
        &bootstrap,
        &[
            "--output",
            "json",
            "reassign",
            "execute",
            "--reassignment-json-file",
            invalid_reassignment_file.to_str().expect("fixture path"),
            "--execute",
        ],
    );
    assert!(!failed_reassignment.status.success());
    let failed_reassignment_json: serde_json::Value =
        serde_json::from_slice(&failed_reassignment.stdout).expect("failed reassignment JSON");
    assert!(
        failed_reassignment_json["data"]
            .as_array()
            .expect("failed reassignment rows")
            .iter()
            .any(|row| row["error"].is_string())
    );
    success(&bootstrap, &["reassign", "list"]);
    assert!(
        success(
            &bootstrap,
            &[
                "reassign",
                "verify",
                "--reassignment-json-file",
                reassignment_file.to_str().expect("fixture path"),
            ]
        )
        .contains("COMPLETED")
    );
    assert!(
        !success(
            &bootstrap,
            &[
                "configs",
                "describe",
                "--entity-type",
                "topics",
                "--entity-name",
                "integration-events",
            ]
        )
        .contains("leader.replication.throttled.replicas")
    );
    assert!(
        !success(
            &bootstrap,
            &[
                "configs",
                "describe",
                "--entity-type",
                "brokers",
                "--entity-name",
                "1",
            ]
        )
        .contains("leader.replication.throttled.rate")
    );
    assert!(success(&bootstrap, &["log-dirs", "--describe"]).contains("integration-events"));
    assert!(success(&bootstrap, &["api-versions"]).contains("ApiVersions"));
    let api_versions: serde_json::Value =
        serde_json::from_str(&success(&bootstrap, &["--output", "json", "api-versions"]))
            .expect("api-versions JSON");
    let api_versions = api_versions["data"]
        .as_array()
        .expect("api-versions data array");
    assert!(
        api_versions.len() > 50,
        "broker returned an incomplete API map"
    );
    assert!(
        api_versions
            .iter()
            .any(|entry| entry["api_key"].as_i64() == Some(18))
    );
    assert!(success(&bootstrap, &["cluster", "id"]).contains("CLUSTER_ID"));
    assert!(success(&bootstrap, &["cluster", "list-endpoints"]).contains("127.0.0.1"));
    let fenced_endpoints = success(
        &bootstrap,
        &["cluster", "list-endpoints", "--include-fenced-brokers"],
    );
    assert!(fenced_endpoints.contains("STATE"));
    assert!(fenced_endpoints.contains("unfenced"));
    let unregister_preview: serde_json::Value = serde_json::from_str(&success(
        &bootstrap,
        &["--output", "json", "cluster", "unregister", "--id", "999"],
    ))
    .expect("unregister preview JSON");
    assert_eq!(unregister_preview["command"], "cluster.unregister");

    success(
        &bootstrap,
        &[
            "acls",
            "remove",
            "--cluster",
            "--allow-principal",
            "User:operator",
            "--deny-principal",
            "User:blocked",
            "--operation",
            "cluster-action",
            "--execute",
        ],
    );

    success(
        &bootstrap,
        &["topics", "delete", "--topic", "integration-events"],
    );
    success(
        &bootstrap,
        &["topics", "delete", "--topic", "integration-json"],
    );
    success(
        &bootstrap,
        &[
            "topics",
            "delete",
            "--topic",
            "integration-events",
            "--if-exists",
        ],
    );
}
