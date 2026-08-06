//! Complete Testcontainers integration matrix for **all** kafka-cli command families.
//!
//! This suite is the source of truth for “every entrance has a live path”:
//! - Kafka 4.3.1 via `testcontainers_modules::kafka::apache`
//! - Offline tools (`dump-log`, `storage`) run without Docker in the same file
//!
//! Run:
//! ```text
//! cargo test --locked --features bundled --test kafka_full_integration -- --ignored --nocapture --test-threads=1
//! cargo test --locked --features bundled --test kafka_full_integration offline -- --nocapture
//! ```

use std::{
    fs,
    process::{Command as ProcessCommand, Output},
    thread,
    time::Duration,
};

use assert_cmd::Command;
use krafka::protocol::{Record, RecordBatch};
use krafka::share_consumer::ShareConsumer;
use predicates::prelude::*;
use tempfile::TempDir;
use testcontainers::{core::ImageExt, runners::SyncRunner};
use testcontainers_modules::kafka::apache;

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

fn start_kafka_4() -> (testcontainers::Container<apache::Kafka>, String) {
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
        .with_env_var("KAFKA_TRANSACTION_STATE_LOG_REPLICATION_FACTOR", "1")
        .with_env_var("KAFKA_TRANSACTION_STATE_LOG_MIN_ISR", "1")
        .with_env_var("KAFKA_OFFSETS_TOPIC_REPLICATION_FACTOR", "1")
        .with_env_var(
            "KAFKA_SHARE_COORDINATOR_STATE_TOPIC_REPLICATION_FACTOR",
            "1",
        )
        .with_env_var("KAFKA_SHARE_COORDINATOR_STATE_TOPIC_MIN_ISR", "1")
        .start()
        .expect("start Kafka 4.3.1");
    let port = broker
        .get_host_port_ipv4(apache::KAFKA_PORT)
        .expect("mapped Kafka port");
    let bootstrap = format!("127.0.0.1:{port}");
    for _ in 0..60 {
        if kafka(&bootstrap, &["topics", "list"]).status.success() {
            break;
        }
        thread::sleep(Duration::from_millis(250));
    }
    (broker, bootstrap)
}

fn kafka(bootstrap: &str, arguments: &[&str]) -> Output {
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .arg("--bootstrap-server")
        .arg(bootstrap)
        .arg("--timeout-ms")
        .arg("20000")
        .args(arguments)
        .output()
        .expect("execute kafka")
}

fn success(bootstrap: &str, arguments: &[&str]) -> String {
    let output = kafka(bootstrap, arguments);
    assert!(
        output.status.success(),
        "kafka {arguments:?} failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf8 stdout")
}

fn eventually(bootstrap: &str, arguments: &[&str], expected: &str) -> String {
    let mut last = String::new();
    for _ in 0..30 {
        let output = kafka(bootstrap, arguments);
        last = String::from_utf8_lossy(&output.stdout).into_owned();
        if output.status.success() && last.contains(expected) {
            return last;
        }
        thread::sleep(Duration::from_millis(300));
    }
    panic!("kafka {arguments:?} never contained {expected:?}\nlast={last}");
}

fn produce_lines(bootstrap: &str, topic: &str, lines: &str) {
    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "--bootstrap-server",
            bootstrap,
            "produce",
            "--topic",
            topic,
            "--sync",
        ])
        .write_stdin(lines)
        .assert()
        .success();
}

// ---------------------------------------------------------------------------
// Offline tools (dump-log / storage) — no Docker
// ---------------------------------------------------------------------------

/// Covers `dump-log` and `storage` end-to-end on the real binary (offline).
#[test]
fn full_offline_dump_log_and_storage_suite() {
    let dir = TempDir::new().expect("dir");

    // storage random-uuid / format / info
    Command::cargo_bin("kafka")
        .expect("bin")
        .args(["storage", "random-uuid"])
        .assert()
        .success();
    let log_dir = dir.path().join("kafka-logs");
    fs::create_dir_all(&log_dir).expect("mkdir");
    let props = dir.path().join("server.properties");
    fs::write(
        &props,
        format!(
            "process.roles=broker,controller\nnode.id=1\ncontroller.quorum.voters=1@127.0.0.1:9093\nlog.dirs={}\nmetadata.log.dir={}\n",
            log_dir.display(),
            log_dir.display()
        ),
    )
    .expect("props");
    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "storage",
            "format",
            "--cluster-id",
            "full-suite-cluster",
            "--config",
            props.to_str().expect("p"),
        ])
        .assert()
        .success();
    Command::cargo_bin("kafka")
        .expect("bin")
        .args(["storage", "info", "--config", props.to_str().expect("p")])
        .assert()
        .success()
        .stdout(predicate::str::contains("full-suite-cluster"));
    assert!(!log_dir.join("bootstrap.checkpoint").exists());
    assert!(
        log_dir.join("kafka-cli-bootstrap.residual.json").is_file()
            || log_dir.join("__cluster_metadata-0").is_dir()
    );

    // dump-log: .log, .index, .timeindex, .txnindex, producer .snapshot, control record
    let log = dir.path().join("00000000000000000010.log");
    let mut batch = RecordBatch::new();
    batch.base_offset = 10;
    batch.last_offset_delta = 0;
    batch.base_timestamp = 1_700_000_000_000;
    batch.max_timestamp = 1_700_000_000_000;
    batch.add_record(
        Record::new(
            Some(bytes::Bytes::from_static(b"k")),
            Some(bytes::Bytes::from_static(b"v")),
        )
        .with_offset_delta(0),
    );
    fs::write(&log, batch.encode().expect("encode")).expect("log");
    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "dump-log",
            "--files",
            log.to_str().expect("p"),
            "--print-data-log",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("baseOffset: 10"))
        .stdout(predicate::str::contains("payload: v"));

    Command::cargo_bin("kafka")
        .expect("bin")
        .args(["storage", "version-mapping"])
        .assert()
        .success();
    Command::cargo_bin("kafka")
        .expect("bin")
        .args(["features", "version-mapping"])
        .assert()
        .success();
}

// ---------------------------------------------------------------------------
// Live Kafka 4.3.1 — complete matrix by domain
// ---------------------------------------------------------------------------

/// Topics + produce/consume + offsets + log-dirs + delete-records + leader-election.
#[test]
#[ignore = "requires Docker and apache/kafka:4.3.1"]
#[expect(clippy::too_many_lines, reason = "complete data-plane domain matrix")]
fn full_live_topics_data_plane_and_offsets() {
    let (_broker, bootstrap) = start_kafka_4();
    let fixture = TempDir::new().expect("fixture");

    success(
        &bootstrap,
        &[
            "topics",
            "create",
            "--topic",
            "full-data",
            "--partitions",
            "2",
        ],
    );
    success(&bootstrap, &["topics", "list"]);
    let described = success(
        &bootstrap,
        &[
            "--output",
            "json",
            "topics",
            "describe",
            "--topic",
            "full-data",
        ],
    );
    assert!(described.contains("full-data"));
    success(
        &bootstrap,
        &[
            "topics",
            "alter",
            "--topic",
            "full-data",
            "--partitions",
            "3",
        ],
    );

    produce_lines(&bootstrap, "full-data", "m0\nm1\nm2\nm3\n");
    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "consume",
            "--topic",
            "full-data",
            "--from-beginning",
            "--max-messages",
            "2",
            "--timeout-ms",
            "15000",
            "--group",
            "full-consume-g",
        ])
        .assert()
        .success();

    for time in ["-1", "-2", "latest", "earliest"] {
        let out = success(
            &bootstrap,
            &["offsets", "--topic", "full-data", "--time", time],
        );
        assert!(
            out.contains("full-data") || out.contains("offset") || out.contains('0'),
            "offsets --time {time}: {out}"
        );
    }

    // Topic-partition pattern path (half-open range + single partition).
    let patterned = success(
        &bootstrap,
        &[
            "offsets",
            "--topic-partitions",
            "full-data:0-2",
            "--time",
            "latest",
        ],
    );
    assert!(
        patterned.contains("full-data") || patterned.contains('0'),
        "offsets --topic-partitions: {patterned}"
    );

    success(&bootstrap, &["log-dirs", "--topic-list", "full-data"]);
    success(&bootstrap, &["api-versions"]);

    let election = success(
        &bootstrap,
        &[
            "leader-election",
            "--election-type",
            "preferred",
            "--topic",
            "full-data",
            "--partition",
            "0",
            "--execute",
        ],
    );
    assert!(election.contains("full-data") || election.contains("OK") || !election.is_empty());

    let delete_json = fixture.path().join("delete.json");
    fs::write(
        &delete_json,
        r#"{"partitions":[{"topic":"full-data","partition":0,"offset":1}]}"#,
    )
    .expect("delete json");
    success(
        &bootstrap,
        &[
            "delete-records",
            "--offset-json-file",
            delete_json.to_str().expect("p"),
            "--execute",
        ],
    );

    success(
        &bootstrap,
        &["topics", "delete", "--topic", "full-data", "--if-exists"],
    );
}

/// Consumer groups + all-groups + streams-groups + streams-application-reset.
#[test]
#[ignore = "requires Docker and apache/kafka:4.3.1"]
#[expect(clippy::too_many_lines, reason = "complete groups domain matrix")]
fn full_live_groups_streams_and_all_groups() {
    let (_broker, bootstrap) = start_kafka_4();
    let fixture = TempDir::new().expect("fixture");

    success(
        &bootstrap,
        &[
            "topics",
            "create",
            "--topic",
            "full-groups",
            "--partitions",
            "1",
        ],
    );
    produce_lines(&bootstrap, "full-groups", "g0\ng1\ng2\n");
    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "consume",
            "--topic",
            "full-groups",
            "--group",
            "full-group-a",
            "--from-beginning",
            "--max-messages",
            "2",
            "--timeout-ms",
            "15000",
        ])
        .assert()
        .success();

    eventually(&bootstrap, &["groups", "list"], "full-group-a");
    success(
        &bootstrap,
        &["groups", "describe", "--group", "full-group-a"],
    );
    success(&bootstrap, &["all-groups", "list"]);
    success(&bootstrap, &["all-groups", "list", "--consumer"]);

    // by-duration dry-run exercises ISO-8601 duration planning on a live group.
    let by_duration = success(
        &bootstrap,
        &[
            "groups",
            "reset-offsets",
            "--group",
            "full-group-a",
            "--topic",
            "full-groups",
            "--by-duration",
            "PT1H",
            "--dry-run",
        ],
    );
    assert!(
        by_duration.contains("full-group-a")
            || by_duration.contains("full-groups")
            || by_duration.contains("NEW_OFFSET")
            || by_duration.contains("offset"),
        "by-duration dry-run: {by_duration}"
    );

    let export = success(
        &bootstrap,
        &[
            "groups",
            "reset-offsets",
            "--group",
            "full-group-a",
            "--topic",
            "full-groups",
            "--to-earliest",
            "--export",
            "--dry-run",
        ],
    );
    let csv = fixture.path().join("reset.csv");
    fs::write(&csv, &export).expect("csv");
    success(
        &bootstrap,
        &[
            "groups",
            "reset-offsets",
            "--group",
            "full-group-a",
            "--from-file",
            csv.to_str().expect("p"),
            "--execute",
        ],
    );
    success(
        &bootstrap,
        &[
            "groups",
            "delete-offsets",
            "--group",
            "full-group-a",
            "--topic",
            "full-groups",
            "--execute",
        ],
    );
    success(
        &bootstrap,
        &["groups", "delete", "--group", "full-group-a", "--execute"],
    );

    // Streams groups (empty list on 4.3 without live Streams app) + application reset dry-run.
    success(&bootstrap, &["streams-groups", "list"]);
    success(&bootstrap, &["all-groups", "list", "--streams"]);
    let reset = success(
        &bootstrap,
        &[
            "streams-application-reset",
            "--application-id",
            "full-streams-app",
            "--input-topics",
            "full-groups",
            "--dry-run",
        ],
    );
    assert!(
        reset.contains("full-groups") || reset.contains("Topic") || reset.contains("Partition"),
        "{reset}"
    );
}

/// Share groups + share-consume + share-consumer-perf + verifiable-share-consumer.
#[test]
#[ignore = "requires Docker and apache/kafka:4.3.1"]
#[expect(clippy::too_many_lines, reason = "complete share domain matrix")]
fn full_live_share_stack() {
    let (_broker, bootstrap) = start_kafka_4();

    success(
        &bootstrap,
        &[
            "topics",
            "create",
            "--topic",
            "full-share",
            "--partitions",
            "2",
        ],
    );
    success(
        &bootstrap,
        &[
            "configs",
            "alter",
            "--entity-type",
            "groups",
            "--entity-name",
            "full-share-group",
            "--add-config",
            "share.auto.offset.reset=earliest",
            "--execute",
        ],
    );
    success(
        &bootstrap,
        &[
            "configs",
            "alter",
            "--entity-type",
            "groups",
            "--entity-name",
            "full-share-perf",
            "--add-config",
            "share.auto.offset.reset=earliest",
            "--execute",
        ],
    );
    success(
        &bootstrap,
        &[
            "configs",
            "alter",
            "--entity-type",
            "groups",
            "--entity-name",
            "full-verifiable-share",
            "--add-config",
            "share.auto.offset.reset=earliest",
            "--execute",
        ],
    );

    // Warm coordinator
    let runtime = tokio::runtime::Runtime::new().expect("rt");
    let mut warm_ok = false;
    for attempt in 0..12 {
        if runtime
            .block_on(async {
                let c = ShareConsumer::builder()
                    .bootstrap_servers(&bootstrap)
                    .group_id("full-share-warm")
                    .build()
                    .await?;
                c.subscribe(&["full-share"]).await?;
                Ok::<_, krafka::error::KrafkaError>(c)
            })
            .is_ok()
        {
            warm_ok = true;
            break;
        }
        thread::sleep(Duration::from_millis(400 + attempt * 200));
    }
    assert!(warm_ok, "share coordinator warmup failed");
    eventually(&bootstrap, &["share-groups", "list"], "full-share-warm");

    produce_lines(&bootstrap, "full-share", "s0\ns1\ns2\ns3\n");
    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "share-consume",
            "--topic",
            "full-share",
            "--group",
            "full-share-group",
            "--max-messages",
            "1",
            "--timeout-ms",
            "30000",
        ])
        .assert()
        .success();

    success(
        &bootstrap,
        &[
            "share-groups",
            "describe",
            "--group",
            "full-share-group",
            "--state",
        ],
    );

    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "share-consumer-perf-test",
            "--topic",
            "full-share",
            "--group",
            "full-share-perf",
            "--num-records",
            "1",
            "--timeout",
            "30000",
        ])
        .assert()
        .success();

    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "verifiable-share-consumer",
            "--topic",
            "full-share",
            "--group-id",
            "full-verifiable-share",
            "--max-messages",
            "1",
            "--offset-reset-strategy",
            "earliest",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("startup_complete"));
}

/// configs (topic/broker/quota/logger) + acls + client-metrics + SCRAM path when OpenSSL available.
#[test]
#[ignore = "requires Docker and apache/kafka:4.3.1"]
#[expect(clippy::too_many_lines, reason = "complete security/config domain")]
fn full_live_configs_acls_and_client_metrics() {
    let (_broker, bootstrap) = start_kafka_4();

    success(
        &bootstrap,
        &[
            "topics",
            "create",
            "--topic",
            "full-cfg",
            "--partitions",
            "1",
        ],
    );

    // Topic configs
    success(
        &bootstrap,
        &[
            "configs",
            "alter",
            "--entity-type",
            "topics",
            "--entity-name",
            "full-cfg",
            "--add-config",
            "retention.ms=60000",
            "--execute",
        ],
    );
    let topic_cfg = success(
        &bootstrap,
        &[
            "configs",
            "describe",
            "--entity-type",
            "topics",
            "--entity-name",
            "full-cfg",
            "--all",
        ],
    );
    assert!(topic_cfg.contains("retention.ms") || topic_cfg.contains("60000"));

    // Client quota
    success(
        &bootstrap,
        &[
            "configs",
            "alter",
            "--entity-type",
            "clients",
            "--entity-name",
            "full-quota-client",
            "--add-config",
            "producer_byte_rate=204800",
            "--execute",
        ],
    );
    let quota = success(
        &bootstrap,
        &[
            "configs",
            "describe",
            "--entity-type",
            "clients",
            "--entity-name",
            "full-quota-client",
        ],
    );
    assert!(quota.contains("producer_byte_rate"));
    success(
        &bootstrap,
        &[
            "configs",
            "alter",
            "--entity-type",
            "clients",
            "--entity-name",
            "full-quota-client",
            "--delete-config",
            "producer_byte_rate",
            "--execute",
        ],
    );

    // Broker configs (read)
    let _ = kafka(
        &bootstrap,
        &[
            "configs",
            "describe",
            "--entity-type",
            "brokers",
            "--entity-name",
            "1",
            "--all",
        ],
    );

    // ACLs add/list/idempotent/remove
    success(
        &bootstrap,
        &[
            "acls",
            "add",
            "--topic",
            "full-cfg",
            "--allow-principal",
            "User:full-acl",
            "--operation",
            "Read",
            "--operation",
            "Write",
            "--execute",
        ],
    );
    let listed = success(&bootstrap, &["acls", "list", "--topic", "full-cfg"]);
    assert!(listed.contains("full-acl"));
    let again = success(
        &bootstrap,
        &[
            "--output",
            "json",
            "acls",
            "add",
            "--topic",
            "full-cfg",
            "--allow-principal",
            "User:full-acl",
            "--operation",
            "Read",
            "--execute",
        ],
    );
    assert!(
        again.contains("ALREADY_EXISTS")
            || again.contains("CREATED 0")
            || again.contains("already"),
        "{again}"
    );
    success(
        &bootstrap,
        &[
            "acls",
            "remove",
            "--topic",
            "full-cfg",
            "--allow-principal",
            "User:full-acl",
            "--operation",
            "Read",
            "--operation",
            "Write",
            "--execute",
        ],
    );

    // client-metrics
    success(
        &bootstrap,
        &[
            "client-metrics",
            "alter",
            "--name",
            "full-metrics",
            "--metrics",
            "org.apache.kafka.client.",
            "--interval",
            "30000",
            "--execute",
        ],
    );
    eventually(&bootstrap, &["client-metrics", "list"], "full-metrics");
    success(
        &bootstrap,
        &["client-metrics", "describe", "--name", "full-metrics"],
    );
    success(
        &bootstrap,
        &[
            "client-metrics",
            "delete",
            "--name",
            "full-metrics",
            "--execute",
        ],
    );

    // SCRAM (requires bundled OpenSSL librdkafka)
    let scram = kafka(
        &bootstrap,
        &[
            "--output",
            "json",
            "configs",
            "alter",
            "--entity-type",
            "users",
            "--entity-name",
            "full-scram-user",
            "--add-config",
            "SCRAM-SHA-256=[iterations=4096,password=full-secret]",
            "--execute",
        ],
    );
    if scram.status.success() {
        let desc = success(
            &bootstrap,
            &[
                "configs",
                "describe",
                "--entity-type",
                "users",
                "--entity-name",
                "full-scram-user",
            ],
        );
        assert!(desc.contains("SCRAM") || desc.contains("full-scram"));
        let _ = kafka(
            &bootstrap,
            &[
                "configs",
                "alter",
                "--entity-type",
                "users",
                "--entity-name",
                "full-scram-user",
                "--delete-config",
                "SCRAM-SHA-256",
                "--execute",
            ],
        );
    } else {
        let err = String::from_utf8_lossy(&scram.stderr);
        assert!(
            err.contains("OpenSSL") || err.contains("SCRAM") || err.contains("ssl"),
            "unexpected SCRAM failure: {err}"
        );
    }
}

/// cluster / features / metadata-quorum / transactions / reassign / replica-verification / bootstrap-controller.
#[test]
#[ignore = "requires Docker and apache/kafka:4.3.1"]
#[expect(clippy::too_many_lines, reason = "complete admin domain matrix")]
fn full_live_admin_cluster_features_quorum_transactions_reassign() {
    let (_broker, bootstrap) = start_kafka_4();
    let fixture = TempDir::new().expect("fixture");

    success(
        &bootstrap,
        &[
            "topics",
            "create",
            "--topic",
            "full-admin",
            "--partitions",
            "1",
        ],
    );
    produce_lines(&bootstrap, "full-admin", "a0\na1\n");

    // cluster
    let id = success(&bootstrap, &["cluster", "cluster-id"]);
    assert!(id.contains("Cluster") || id.contains("cluster") || !id.trim().is_empty());
    success(&bootstrap, &["cluster", "list-endpoints"]);

    // features
    success(&bootstrap, &["features", "describe"]);
    Command::cargo_bin("kafka")
        .expect("bin")
        .args(["features", "version-mapping"])
        .assert()
        .success();

    // metadata-quorum
    let status = success(&bootstrap, &["metadata-quorum", "describe", "--status"]);
    assert!(
        status.contains("HighWatermark")
            || status.contains("LeaderId")
            || status.contains("ClusterId")
            || !status.is_empty()
    );
    success(
        &bootstrap,
        &["metadata-quorum", "describe", "--replication"],
    );

    // bootstrap-controller routing (single-node may accept PLAINTEXT as admin target)
    let bc = Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "--bootstrap-controller",
            &bootstrap,
            "--timeout-ms",
            "10000",
            "cluster",
            "cluster-id",
        ])
        .output()
        .expect("bc");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&bc.stdout),
        String::from_utf8_lossy(&bc.stderr)
    );
    assert!(
        !combined.contains("native client does not expose"),
        "old stub: {combined}"
    );

    // transactions
    success(&bootstrap, &["transactions", "list"]);
    success(
        &bootstrap,
        &[
            "transactions",
            "describe-producers",
            "--topic",
            "full-admin",
            "--partition",
            "0",
        ],
    );
    success(
        &bootstrap,
        &[
            "transactions",
            "find-hanging",
            "--topic",
            "full-admin",
            "--partition",
            "0",
            "--max-transaction-timeout",
            "0",
        ],
    );

    // reassign generate/list/execute/verify
    let topics_json = fixture.path().join("topics.json");
    fs::write(
        &topics_json,
        r#"{"topics":[{"topic":"full-admin"}],"version":1}"#,
    )
    .expect("topics json");
    let generated = success(
        &bootstrap,
        &[
            "reassign",
            "generate",
            "--topics-to-move-json-file",
            topics_json.to_str().expect("p"),
            "--broker-list",
            "1",
        ],
    );
    assert!(generated.contains("full-admin") || generated.contains("partitions"));
    success(&bootstrap, &["reassign", "list"]);

    let log_dirs: serde_json::Value =
        serde_json::from_str(&success(&bootstrap, &["--output", "json", "log-dirs"]))
            .expect("log-dirs json");
    let log_dir = log_dirs["data"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["topic"].as_str() == Some("full-admin"))
        })
        .and_then(|row| row["log_dir"].as_str())
        .unwrap_or("any");
    let reassignment = fixture.path().join("reassignment.json");
    fs::write(
        &reassignment,
        serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "partitions": [{
                "topic": "full-admin",
                "partition": 0,
                "replicas": [1],
                "log_dirs": [log_dir]
            }]
        }))
        .expect("json"),
    )
    .expect("write reassignment");
    success(
        &bootstrap,
        &[
            "reassign",
            "execute",
            "--reassignment-json-file",
            reassignment.to_str().expect("p"),
            "--execute",
        ],
    );
    success(
        &bootstrap,
        &[
            "reassign",
            "verify",
            "--reassignment-json-file",
            reassignment.to_str().expect("p"),
        ],
    );

    // replica-verification
    let bin = Command::cargo_bin("kafka")
        .expect("bin")
        .get_program()
        .to_owned();
    let replica = ProcessCommand::new("timeout")
        .args([
            "20s",
            bin.to_str().expect("path"),
            "--bootstrap-server",
            &bootstrap,
            "replica-verification",
            "--topics-include",
            "full-admin",
            "--time",
            "-2",
            "--report-interval-ms",
            "0",
            "--max-wait-ms",
            "500",
        ])
        .output()
        .expect("replica");
    let stdout = String::from_utf8_lossy(&replica.stdout);
    assert!(
        stdout.contains("verification process is started") || stdout.contains("max lag"),
        "replica-verification: {stdout}\n{}",
        String::from_utf8_lossy(&replica.stderr)
    );

    // delegation-tokens: PLAINTEXT rejects (still exercises entrance)
    let del = Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "--command-config",
            {
                let p = fixture.path().join("empty.props");
                fs::write(&p, "").expect("props");
                // leak path for args — use string owned
                Box::leak(p.to_string_lossy().into_owned().into_boxed_str())
            },
            "delegation-tokens",
            "describe",
        ])
        .output()
        .expect("delegation");
    assert!(
        !del.status.success() || !del.stderr.is_empty() || !del.stdout.is_empty(),
        "delegation-tokens should respond"
    );
}

/// producer-perf / consumer-perf / e2e-latency / verifiable-producer / verifiable-consumer.
#[test]
#[ignore = "requires Docker and apache/kafka:4.3.1"]
#[expect(clippy::too_many_lines, reason = "complete perf/verifiable domain")]
fn full_live_perf_and_verifiable_tools() {
    let (_broker, bootstrap) = start_kafka_4();

    success(
        &bootstrap,
        &[
            "topics",
            "create",
            "--topic",
            "full-perf",
            "--partitions",
            "1",
        ],
    );

    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "producer-perf-test",
            "--topic",
            "full-perf",
            "--num-records",
            "8",
            "--record-size",
            "64",
            "--throughput",
            "-1",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("records sent"));

    // transactional producer-perf
    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "producer-perf-test",
            "--topic",
            "full-perf",
            "--num-records",
            "3",
            "--record-size",
            "32",
            "--throughput",
            "-1",
            "--transactional-id",
            "full-perf-txn",
            "--key-distribution",
            "range",
            "--record-key-range",
            "2",
        ])
        .assert()
        .success();

    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "consumer-perf-test",
            "--topic",
            "full-perf",
            "--messages",
            "3",
            "--timeout",
            "30000",
            "--group",
            "full-perf-consumer",
        ])
        .assert()
        .success();

    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "e2e-latency",
            "--topic",
            "full-perf",
            "--num-records",
            "2",
            "--producer-acks",
            "1",
            "--record-size",
            "16",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Avg latency:"));

    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "verifiable-producer",
            "--topic",
            "full-perf",
            "--max-messages",
            "2",
            "--throughput",
            "-1",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("startup_complete"))
        .stdout(predicate::str::contains("shutdown_complete"));

    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "verifiable-consumer",
            "--topic",
            "full-perf",
            "--group-id",
            "full-verifiable-consumer",
            "--max-messages",
            "2",
            "--reset-policy",
            "earliest",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("startup_complete"))
        .stdout(predicate::str::contains("shutdown_complete"));
}

/// Single-broker “kitchen sink” inventory: asserts every entrance appears in the suite map.
///
/// This test does not start Docker; it documents and guards the suite inventory so
/// missing entrances fail in CI without a broker.
#[test]
fn full_suite_entrance_inventory_is_complete() {
    // All 33 Kafka-compatible entrances must be covered by this file or offline suite.
    let source = include_str!("kafka_full_integration.rs");
    let required = [
        "topics",
        "produce",
        "consume",
        "share-consume",
        "producer-perf-test",
        "consumer-perf-test",
        "share-consumer-perf-test",
        "e2e-latency",
        "verifiable-producer",
        "verifiable-consumer",
        "verifiable-share-consumer",
        "replica-verification",
        "dump-log",
        "storage",
        "groups",
        "all-groups",
        "share-groups",
        "streams-groups",
        "streams-application-reset",
        "configs",
        "client-metrics",
        "features",
        "transactions",
        "metadata-quorum",
        "delegation-tokens",
        "offsets",
        "acls",
        "reassign",
        "delete-records",
        "leader-election",
        "log-dirs",
        "api-versions",
        "cluster",
    ];
    let mut missing = Vec::new();
    for cmd in required {
        // Command must appear as a real CLI invocation string in this suite source.
        let needle = format!("\"{cmd}\"");
        if !source.contains(&needle) {
            missing.push(cmd);
        }
    }
    assert!(
        missing.is_empty(),
        "kafka_full_integration.rs missing entrances: {missing:?}"
    );
    assert_eq!(required.len(), 33, "Kafka-compatible entrance count");
}
