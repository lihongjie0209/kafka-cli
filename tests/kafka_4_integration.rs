//! End-to-end command-family coverage against a real Kafka 4.x broker.

use std::{fs, process::Output};

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
    let election_file = fixture.path().join("election.json");
    let reset_file = fixture.path().join("reset.csv");
    let topic_config_file = fixture.path().join("topic.properties");
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
    success(
        &bootstrap,
        &[
            "topics",
            "create",
            "--topic",
            "integration-events",
            "--partitions",
            "1",
        ],
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
    success(
        &bootstrap,
        &[
            "topics",
            "create",
            "--topic",
            "under-min-isr",
            "--config",
            "min.insync.replicas=2",
        ],
    );
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
            "--batch-size",
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
            "--max-memory-bytes",
            "33554432",
            "--socket-buffer-size",
            "102400",
            "--command-property",
            "client.id=integration-producer",
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
            "--from-beginning",
            "--max-messages",
            "1",
            "--offset",
            "0",
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
    success(
        &bootstrap,
        &[
            "groups",
            "describe",
            "--group",
            "integration-suite",
            "--members",
        ],
    );
    assert!(
        success(
            &bootstrap,
            &["groups", "describe", "--all-groups", "--state"]
        )
        .contains("integration-suite")
    );
    assert!(
        success(&bootstrap, &["groups", "describe", "--all-groups"]).contains("integration-events")
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
            "integration-events",
            "--execute",
        ],
    );
    assert!(
        success(&bootstrap, &["groups", "delete", "--all-groups"]).contains("integration-suite")
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
            "describe",
            "--entity-type",
            "topic",
            "--entity-name",
            "integration-events",
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
            "users",
            "--entity-name",
            "integration-user",
            "--add-config",
            "SCRAM-SHA-256=[iterations=4096,password=integration-secret]",
            "--execute",
        ],
    );
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

    // ACL and destructive commands use their mandatory safe preview mode.
    success(&bootstrap, &["acls", "list"]);
    success(
        &bootstrap,
        &[
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
    );
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
    success(
        &bootstrap,
        &[
            "delete-records",
            "--offset-json-file",
            delete_file.to_str().expect("fixture path"),
            "--execute",
        ],
    );
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
    success(
        &bootstrap,
        &[
            "reassign",
            "execute",
            "--reassignment-json-file",
            reassignment_file.to_str().expect("fixture path"),
            "--execute",
        ],
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
    assert!(success(&bootstrap, &["log-dirs"]).contains("integration-events"));
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
    success(&bootstrap, &["cluster", "unregister", "--id", "999"]);

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
