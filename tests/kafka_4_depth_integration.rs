//! Focused Kafka 4.x integration suites beyond the monolithic command-family matrix.
//!
//! Each test starts its own broker fixture so failures isolate to one command family.
//! All tests are `#[ignore]` and require Docker (apache/kafka:4.3.1).

use std::{fs, process::Output, thread, time::Duration};

use assert_cmd::Command;
use krafka::share_consumer::ShareConsumer;
use predicates::prelude::*;
use tempfile::TempDir;
use testcontainers::{core::ImageExt, runners::SyncRunner};
use testcontainers_modules::kafka::apache;

fn start_broker() -> (testcontainers::Container<apache::Kafka>, String) {
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
    // Wait until the broker answers Admin Metadata (readiness probe can fire early).
    for _ in 0..40 {
        let out = kafka(&bootstrap, &["topics", "list"]);
        if out.status.success() {
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

fn eventually(bootstrap: &str, arguments: &[&str], expected: &str) -> String {
    let mut last = String::new();
    for _ in 0..24 {
        let output = kafka(bootstrap, arguments);
        last = String::from_utf8_lossy(&output.stdout).into_owned();
        if output.status.success() && last.contains(expected) {
            return last;
        }
        thread::sleep(Duration::from_millis(250));
    }
    panic!("kafka {arguments:?} never contained {expected:?}\nlast: {last}");
}

#[test]
#[ignore = "requires Docker and downloads apache/kafka:4.3.1"]
fn bootstrap_controller_routes_admin_commands_on_single_node() {
    let (_broker, bootstrap) = start_broker();
    // On single-node KRaft, the PLAINTEXT listener commonly accepts admin RPCs that
    // the Java tools also route via --bootstrap-controller. Success or an honest
    // live client error is acceptable; the old preemptive stub must not appear.
    let families: &[&[&str]] = &[
        &["features", "describe"],
        &["metadata-quorum", "describe", "--status"],
        &["cluster", "cluster-id"],
        &[
            "configs",
            "describe",
            "--entity-type",
            "brokers",
            "--entity-name",
            "1",
            "--all",
        ],
    ];
    for args in families {
        let mut cmd = Command::cargo_bin("kafka").expect("bin");
        let mut full = vec![
            "--bootstrap-controller",
            bootstrap.as_str(),
            "--timeout-ms",
            "15000",
        ];
        full.extend(args.iter().copied());
        let output = cmd.args(&full).output().expect("run");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = format!("{stdout}{stderr}");
        assert!(
            !combined.contains("native client does not expose"),
            "preemptive stub for {args:?}: {combined}"
        );
        // Prefer success; if the path cannot open pure controller semantics, the
        // error must still come from the live client (connection/metadata).
        if !output.status.success() {
            assert!(
                combined.contains("broker")
                    || combined.contains("connect")
                    || combined.contains("timed")
                    || combined.contains("Metadata")
                    || combined.contains("metadata")
                    || combined.contains("unavailable")
                    || combined.contains("error"),
                "unexpected failure for {args:?}: {combined}"
            );
        }
    }
}

#[test]
#[ignore = "requires Docker and downloads apache/kafka:4.3.1"]
fn features_offsets_and_cluster_depth_paths() {
    let (_broker, bootstrap) = start_broker();

    // Offline feature tables do not need a broker, but still exercise the binary.
    Command::cargo_bin("kafka")
        .expect("bin")
        .args(["features", "version-mapping"])
        .assert()
        .success()
        .stdout(predicate::str::contains("metadata").or(predicate::str::contains("feature")));

    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "features",
            "feature-dependencies",
            "--feature",
            "metadata.version=20",
        ])
        .assert()
        .success();

    assert!(
        success(&bootstrap, &["features", "describe"]).contains("Feature")
            || success(&bootstrap, &["--output", "json", "features", "describe"])
                .contains("feature")
    );

    success(
        &bootstrap,
        &[
            "topics",
            "create",
            "--topic",
            "depth-offsets",
            "--partitions",
            "2",
        ],
    );
    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "produce",
            "--topic",
            "depth-offsets",
            "--sync",
        ])
        .write_stdin("a\nb\nc\n")
        .assert()
        .success();

    let earliest = success(
        &bootstrap,
        &["offsets", "--topic", "depth-offsets", "--time", "-2"],
    );
    assert!(
        earliest.contains("depth-offsets") || earliest.contains("offset"),
        "earliest offsets: {earliest}"
    );
    let latest = success(
        &bootstrap,
        &["offsets", "--topic", "depth-offsets", "--time", "-1"],
    );
    assert!(
        latest.contains("depth-offsets") || latest.contains("offset"),
        "latest offsets: {latest}"
    );

    assert!(
        success(&bootstrap, &["cluster", "cluster-id"]).contains("Cluster")
            || success(&bootstrap, &["--output", "json", "cluster", "cluster-id"])
                .contains("cluster")
    );
    success(&bootstrap, &["cluster", "list-endpoints"]);
    success(&bootstrap, &["api-versions"]);
    success(&bootstrap, &["log-dirs"]);
}

#[test]
#[ignore = "requires Docker and downloads apache/kafka:4.3.1"]
fn consumer_groups_reset_export_import_and_delete_offsets() {
    let (_broker, bootstrap) = start_broker();
    let fixture = TempDir::new().expect("fixture");
    let csv = fixture.path().join("reset.csv");

    success(
        &bootstrap,
        &[
            "topics",
            "create",
            "--topic",
            "depth-groups",
            "--partitions",
            "1",
        ],
    );
    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "produce",
            "--topic",
            "depth-groups",
            "--sync",
        ])
        .write_stdin("m0\nm1\nm2\n")
        .assert()
        .success();

    // Seed a consumer group by consuming two messages.
    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "consume",
            "--topic",
            "depth-groups",
            "--group",
            "depth-group-a",
            "--from-beginning",
            "--max-messages",
            "2",
            "--timeout-ms",
            "15000",
        ])
        .assert()
        .success();

    eventually(&bootstrap, &["groups", "list"], "depth-group-a");
    success(
        &bootstrap,
        &["groups", "describe", "--group", "depth-group-a"],
    );

    let export = success(
        &bootstrap,
        &[
            "groups",
            "reset-offsets",
            "--group",
            "depth-group-a",
            "--topic",
            "depth-groups",
            "--to-earliest",
            "--export",
            "--dry-run",
        ],
    );
    fs::write(&csv, &export).expect("write csv");
    assert!(!export.trim().is_empty(), "export CSV empty");

    success(
        &bootstrap,
        &[
            "groups",
            "reset-offsets",
            "--group",
            "depth-group-a",
            "--from-file",
            csv.to_str().expect("csv"),
            "--execute",
        ],
    );

    success(
        &bootstrap,
        &[
            "groups",
            "delete-offsets",
            "--group",
            "depth-group-a",
            "--topic",
            "depth-groups",
            "--execute",
        ],
    );
    success(
        &bootstrap,
        &["groups", "delete", "--group", "depth-group-a", "--execute"],
    );
}

#[test]
#[ignore = "requires Docker and downloads apache/kafka:4.3.1"]
fn acls_and_configs_mutation_depth() {
    let (_broker, bootstrap) = start_broker();

    success(
        &bootstrap,
        &[
            "topics",
            "create",
            "--topic",
            "depth-acl-cfg",
            "--partitions",
            "1",
        ],
    );

    success(
        &bootstrap,
        &[
            "acls",
            "add",
            "--topic",
            "depth-acl-cfg",
            "--allow-principal",
            "User:depth-op",
            "--operation",
            "Read",
            "--operation",
            "Write",
            "--execute",
        ],
    );
    let listed = success(&bootstrap, &["acls", "list", "--topic", "depth-acl-cfg"]);
    assert!(
        listed.contains("depth-op") || listed.contains("User:depth-op"),
        "acl list: {listed}"
    );

    success(
        &bootstrap,
        &[
            "configs",
            "alter",
            "--entity-type",
            "topics",
            "--entity-name",
            "depth-acl-cfg",
            "--add-config",
            "retention.ms=123456",
            "--execute",
        ],
    );
    let described = success(
        &bootstrap,
        &[
            "configs",
            "describe",
            "--entity-type",
            "topics",
            "--entity-name",
            "depth-acl-cfg",
        ],
    );
    assert!(
        described.contains("retention.ms") && described.contains("123456"),
        "config describe: {described}"
    );

    success(
        &bootstrap,
        &[
            "configs",
            "alter",
            "--entity-type",
            "topics",
            "--entity-name",
            "depth-acl-cfg",
            "--delete-config",
            "retention.ms",
            "--execute",
        ],
    );

    success(
        &bootstrap,
        &[
            "acls",
            "remove",
            "--topic",
            "depth-acl-cfg",
            "--allow-principal",
            "User:depth-op",
            "--operation",
            "Read",
            "--operation",
            "Write",
            "--execute",
        ],
    );
}

#[test]
#[ignore = "requires Docker and downloads apache/kafka:4.3.1"]
fn transactions_and_delete_records_depth() {
    let (_broker, bootstrap) = start_broker();
    let fixture = TempDir::new().expect("fixture");
    let delete_json = fixture.path().join("delete.json");

    success(
        &bootstrap,
        &[
            "topics",
            "create",
            "--topic",
            "depth-txn",
            "--partitions",
            "1",
        ],
    );
    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "produce",
            "--topic",
            "depth-txn",
            "--sync",
        ])
        .write_stdin("t0\nt1\nt2\n")
        .assert()
        .success();

    success(&bootstrap, &["transactions", "list"]);
    success(
        &bootstrap,
        &[
            "transactions",
            "describe-producers",
            "--topic",
            "depth-txn",
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
            "depth-txn",
            "--partition",
            "0",
            "--max-transaction-timeout",
            "0",
        ],
    );

    fs::write(
        &delete_json,
        r#"{"partitions":[{"topic":"depth-txn","partition":0,"offset":1}]}"#,
    )
    .expect("delete json");
    success(
        &bootstrap,
        &[
            "delete-records",
            "--offset-json-file",
            delete_json.to_str().expect("path"),
            "--execute",
        ],
    );
}

#[test]
#[ignore = "requires Docker and downloads apache/kafka:4.3.1"]
fn metadata_quorum_and_leader_election_depth() {
    let (_broker, bootstrap) = start_broker();
    success(
        &bootstrap,
        &[
            "topics",
            "create",
            "--topic",
            "depth-elect",
            "--partitions",
            "1",
        ],
    );

    let status = success(&bootstrap, &["metadata-quorum", "describe", "--status"]);
    assert!(
        status.contains("HighWatermark")
            || status.contains("LeaderId")
            || status.contains("ClusterId"),
        "quorum status: {status}"
    );
    let replication = success(
        &bootstrap,
        &["metadata-quorum", "describe", "--replication"],
    );
    assert!(
        replication.contains("NodeId")
            || replication.contains("DirectoryId")
            || replication.contains("LogEndOffset"),
        "quorum replication: {replication}"
    );

    let preview = success(
        &bootstrap,
        &[
            "leader-election",
            "--election-type",
            "preferred",
            "--topic",
            "depth-elect",
            "--partition",
            "0",
        ],
    );
    assert!(
        preview.contains("depth-elect")
            || preview.contains("PREVIEW")
            || preview.contains("partition"),
        "election preview: {preview}"
    );
    success(
        &bootstrap,
        &[
            "leader-election",
            "--election-type",
            "preferred",
            "--topic",
            "depth-elect",
            "--partition",
            "0",
            "--execute",
        ],
    );
}

#[test]
#[ignore = "requires Docker and downloads apache/kafka:4.3.1"]
#[expect(
    clippy::too_many_lines,
    reason = "focused data-plane smoke covers produce/consume/perf/e2e against one fixture"
)]
fn produce_consume_and_perf_smoke_depth() {
    let (_broker, bootstrap) = start_broker();
    success(
        &bootstrap,
        &[
            "topics",
            "create",
            "--topic",
            "depth-data",
            "--partitions",
            "1",
        ],
    );

    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "produce",
            "--topic",
            "depth-data",
            "--sync",
            "--property",
            "parse.key=true",
            "--property",
            "key.separator=:",
        ])
        .write_stdin("k1:v1\nk2:v2\n")
        .assert()
        .success();

    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "consume",
            "--topic",
            "depth-data",
            "--from-beginning",
            "--max-messages",
            "2",
            "--timeout-ms",
            "15000",
            "--property",
            "print.key=true",
            "--property",
            "key.separator=:",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("v1").or(predicate::str::contains("k1")));

    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "producer-perf-test",
            "--topic",
            "depth-data",
            "--num-records",
            "5",
            "--record-size",
            "32",
            "--throughput",
            "-1",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("records sent"));

    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "consumer-perf-test",
            "--topic",
            "depth-data",
            "--messages",
            "3",
            "--timeout",
            "20000",
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
            "depth-data",
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
}

#[test]
#[ignore = "requires Docker and downloads apache/kafka:4.3.1"]
#[expect(
    clippy::too_many_lines,
    reason = "reassign + replica-verification share one broker fixture"
)]
fn reassign_generate_execute_list_and_replica_verification() {
    let (_broker, bootstrap) = start_broker();
    let fixture = TempDir::new().expect("fixture");
    let topics_file = fixture.path().join("topics.json");
    let reassignment_file = fixture.path().join("reassignment.json");

    success(
        &bootstrap,
        &[
            "topics",
            "create",
            "--topic",
            "depth-reassign",
            "--partitions",
            "1",
        ],
    );
    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "produce",
            "--topic",
            "depth-reassign",
            "--sync",
        ])
        .write_stdin("r0\nr1\n")
        .assert()
        .success();

    fs::write(
        &topics_file,
        r#"{"topics":[{"topic":"depth-reassign"}],"version":1}"#,
    )
    .expect("topics json");
    let generated = success(
        &bootstrap,
        &[
            "reassign",
            "generate",
            "--topics-to-move-json-file",
            topics_file.to_str().expect("path"),
            "--broker-list",
            "1",
        ],
    );
    assert!(
        generated.contains("depth-reassign") || generated.contains("partitions"),
        "generate: {generated}"
    );

    let log_dirs: serde_json::Value =
        serde_json::from_str(&success(&bootstrap, &["--output", "json", "log-dirs"]))
            .expect("log-dirs json");
    let log_dir = log_dirs["data"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["topic"].as_str() == Some("depth-reassign"))
        })
        .and_then(|row| row["log_dir"].as_str())
        .unwrap_or("any");
    fs::write(
        &reassignment_file,
        serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "partitions": [{
                "topic": "depth-reassign",
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
            reassignment_file.to_str().expect("path"),
            "--execute",
        ],
    );
    success(&bootstrap, &["reassign", "list"]);
    success(
        &bootstrap,
        &[
            "reassign",
            "verify",
            "--reassignment-json-file",
            reassignment_file.to_str().expect("path"),
        ],
    );

    let kafka_binary = Command::cargo_bin("kafka")
        .expect("bin")
        .get_program()
        .to_owned();
    let replica = std::process::Command::new("timeout")
        .args([
            "20s",
            kafka_binary.to_str().expect("path"),
            "--bootstrap-server",
            &bootstrap,
            "replica-verification",
            "--topics-include",
            "depth-reassign",
            "--time",
            "-2",
            "--report-interval-ms",
            "0",
            "--max-wait-ms",
            "500",
        ])
        .output()
        .expect("replica-verification");
    let stdout = String::from_utf8_lossy(&replica.stdout);
    assert!(
        stdout.contains("verification process is started") || stdout.contains("max lag"),
        "replica-verification stdout={stdout}\nstderr={}",
        String::from_utf8_lossy(&replica.stderr)
    );
}

#[test]
#[ignore = "requires Docker and downloads apache/kafka:4.3.1"]
#[expect(
    clippy::too_many_lines,
    reason = "share warmup + consume + perf against one fixture"
)]
fn share_groups_and_share_consume_depth() {
    let (_broker, bootstrap) = start_broker();
    success(
        &bootstrap,
        &[
            "topics",
            "create",
            "--topic",
            "depth-share",
            "--partitions",
            "1",
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
            "depth-share-group",
            "--add-config",
            "share.auto.offset.reset=earliest",
            "--execute",
        ],
    );

    // Empty list path should succeed before members exist.
    success(&bootstrap, &["share-groups", "list"]);

    // Prime classic consumer coordinator + offsets topic on a separate topic so
    // ShareFetch is not polluted by priming records.
    success(
        &bootstrap,
        &[
            "topics",
            "create",
            "--topic",
            "depth-share-prime",
            "--partitions",
            "1",
        ],
    );
    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "produce",
            "--topic",
            "depth-share-prime",
            "--sync",
        ])
        .write_stdin("prime\n")
        .assert()
        .success();
    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "consume",
            "--topic",
            "depth-share-prime",
            "--group",
            "depth-share-classic-prime",
            "--from-beginning",
            "--max-messages",
            "1",
            "--timeout-ms",
            "20000",
        ])
        .assert()
        .success();

    // Warm the Share coordinator path with a live ShareConsumer (same as monolith).
    let runtime = tokio::runtime::Runtime::new().expect("share runtime");
    let mut last_err = String::new();
    let mut warm_ok = false;
    for attempt in 0..12 {
        match runtime.block_on(async {
            let consumer = ShareConsumer::builder()
                .bootstrap_servers(&bootstrap)
                .group_id("depth-share-warmup")
                .build()
                .await?;
            consumer.subscribe(&["depth-share"]).await?;
            Ok::<_, krafka::error::KrafkaError>(consumer)
        }) {
            Ok(_consumer) => {
                warm_ok = true;
                break;
            }
            Err(error) => {
                last_err = error.to_string();
                thread::sleep(Duration::from_millis(500 + attempt * 250));
            }
        }
    }
    assert!(
        warm_ok,
        "share coordinator warmup failed after retries: {last_err}"
    );
    assert!(
        eventually(&bootstrap, &["share-groups", "list"], "depth-share-warmup")
            .contains("depth-share-warmup")
    );

    // Produce first so ShareFetch can deliver immediately once the consumer joins.
    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "produce",
            "--topic",
            "depth-share",
            "--sync",
        ])
        .write_stdin("share-msg-1\n")
        .assert()
        .success();

    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "share-consume",
            "--topic",
            "depth-share",
            "--group",
            "depth-share-group",
            "--max-messages",
            "1",
            "--timeout-ms",
            "30000",
            "--formatter-property",
            "print.delivery=true",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("share-msg-1"));

    assert!(
        eventually(&bootstrap, &["share-groups", "list"], "depth-share-group")
            .contains("depth-share-group")
            || eventually(
                &bootstrap,
                &["share-groups", "list", "--state", "Empty"],
                "depth-share-group",
            )
            .contains("depth-share-group")
    );

    // Prefill for perf: produce one more record, then measure.
    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "produce",
            "--topic",
            "depth-share",
            "--sync",
        ])
        .write_stdin("share-msg-2\n")
        .assert()
        .success();
    success(
        &bootstrap,
        &[
            "configs",
            "alter",
            "--entity-type",
            "groups",
            "--entity-name",
            "depth-share-perf",
            "--add-config",
            "share.auto.offset.reset=earliest",
            "--execute",
        ],
    );
    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "share-consumer-perf-test",
            "--topic",
            "depth-share",
            "--group",
            "depth-share-perf",
            "--num-records",
            "1",
            "--timeout",
            "30000",
        ])
        .assert()
        .success();
}

#[test]
#[ignore = "requires Docker and downloads apache/kafka:4.3.1"]
fn streams_groups_reset_and_all_groups_depth() {
    let (_broker, bootstrap) = start_broker();
    success(
        &bootstrap,
        &[
            "topics",
            "create",
            "--topic",
            "depth-streams-in",
            "--partitions",
            "1",
        ],
    );
    // Empty Streams list is a valid path on 4.3 without a live Streams app.
    success(&bootstrap, &["streams-groups", "list"]);
    success(&bootstrap, &["all-groups", "list"]);

    let reset = success(
        &bootstrap,
        &[
            "streams-application-reset",
            "--application-id",
            "depth-streams-app",
            "--input-topics",
            "depth-streams-in",
            "--dry-run",
        ],
    );
    assert!(
        reset.contains("depth-streams") || reset.contains("Topic") || reset.contains("Partition"),
        "streams reset dry-run: {reset}"
    );

    // Missing group describe should fail structured, not panic.
    let missing = kafka(
        &bootstrap,
        &[
            "streams-groups",
            "describe",
            "--group",
            "does-not-exist-streams",
        ],
    );
    assert!(
        !missing.status.success()
            || String::from_utf8_lossy(&missing.stdout).contains("does-not-exist")
            || String::from_utf8_lossy(&missing.stderr).contains("NOT_FOUND")
            || String::from_utf8_lossy(&missing.stderr).contains("not found")
            || String::from_utf8_lossy(&missing.stderr).contains("GROUP"),
        "expected structured missing-group response: stdout={} stderr={}",
        String::from_utf8_lossy(&missing.stdout),
        String::from_utf8_lossy(&missing.stderr)
    );
}

#[test]
#[ignore = "requires Docker and downloads apache/kafka:4.3.1"]
fn client_metrics_and_verifiable_tools_depth() {
    let (_broker, bootstrap) = start_broker();
    success(
        &bootstrap,
        &[
            "topics",
            "create",
            "--topic",
            "depth-verifiable",
            "--partitions",
            "1",
        ],
    );

    success(
        &bootstrap,
        &[
            "client-metrics",
            "alter",
            "--name",
            "depth-metrics-sub",
            "--metrics",
            "org.apache.kafka.producer.",
            "--interval",
            "30000",
            "--execute",
        ],
    );
    assert!(
        eventually(&bootstrap, &["client-metrics", "list"], "depth-metrics-sub")
            .contains("depth-metrics-sub")
    );
    success(
        &bootstrap,
        &["client-metrics", "describe", "--name", "depth-metrics-sub"],
    );
    success(
        &bootstrap,
        &[
            "client-metrics",
            "delete",
            "--name",
            "depth-metrics-sub",
            "--execute",
        ],
    );

    Command::cargo_bin("kafka")
        .expect("bin")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "verifiable-producer",
            "--topic",
            "depth-verifiable",
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
            "depth-verifiable",
            "--group-id",
            "depth-verifiable-consumer",
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
