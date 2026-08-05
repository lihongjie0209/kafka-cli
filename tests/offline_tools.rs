//! Offline tool coverage that does not require a live Kafka broker.
//!
//! Exercises the shipped `kafka` binary for `storage` and `dump-log` end-to-end.

use std::{fs, path::PathBuf, process::Command as ProcessCommand};

use assert_cmd::Command;
use krafka::protocol::{Record, RecordBatch};
use predicates::prelude::*;
use tempfile::TempDir;

fn kafka_bin() -> assert_cmd::cmd::Command {
    Command::cargo_bin("kafka").expect("kafka binary")
}

#[test]
fn storage_random_uuid_should_print_kafka_style_uuid() {
    kafka_bin()
        .args(["storage", "random-uuid"])
        .assert()
        .success()
        .stdout(predicate::function(|s: &str| {
            let trimmed = s.trim();
            // Kafka Uuid.randomUuid URL-safe base64 without padding, 22 chars typical.
            !trimmed.is_empty() && !trimmed.contains(' ') && trimmed.len() >= 20
        }));
}

#[test]
fn storage_format_and_info_should_round_trip_via_binary() {
    let dir = TempDir::new().expect("log dir");
    let log_dir = dir.path().join("kafka-logs");
    fs::create_dir_all(&log_dir).expect("mkdir");
    let cluster_id = "test-cluster-offline";
    let props = write_minimal_server_props(dir.path());

    kafka_bin()
        .args([
            "storage",
            "format",
            "--cluster-id",
            cluster_id,
            "--config",
            props.to_str().expect("path"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Formatting"));

    kafka_bin()
        .args(["storage", "info", "--config", props.to_str().expect("path")])
        .assert()
        .success()
        .stdout(predicate::str::contains(cluster_id))
        .stdout(predicate::str::contains("acceptably formatted"));
}

fn write_minimal_server_props(parent: &std::path::Path) -> PathBuf {
    let log_dir = parent.join("kafka-logs");
    fs::create_dir_all(&log_dir).expect("mkdir log.dirs");
    let props = parent.join("server.properties");
    // Prefer process.roles controller+broker style; storage format reads log.dirs / process.roles.
    let content = format!(
        "process.roles=broker,controller\n\
         node.id=1\n\
         controller.quorum.voters=1@127.0.0.1:9093\n\
         log.dirs={}\n\
         metadata.log.dir={}\n",
        log_dir.display(),
        log_dir.display()
    );
    fs::write(&props, content).expect("write server.properties");
    props
}

#[test]
fn storage_format_should_not_create_binary_bootstrap_checkpoint_name() {
    let dir = TempDir::new().expect("dir");
    let props = write_minimal_server_props(dir.path());
    kafka_bin()
        .args([
            "storage",
            "format",
            "--cluster-id",
            "abcdefghijklmnopqrstuv",
            "--config",
            props.to_str().expect("path"),
        ])
        .assert()
        .success();

    let log_dir = dir.path().join("kafka-logs");
    let reserved = find_named(&log_dir, "bootstrap.checkpoint");
    assert!(
        reserved.is_empty(),
        "must not write reserved bootstrap.checkpoint: {reserved:?}"
    );
    // Residual marker (if present) must use non-reserved name.
    let residual = find_named(&log_dir, "kafka-cli-bootstrap.residual.json");
    assert!(
        !residual.is_empty() || log_dir.join("__cluster_metadata-0").exists(),
        "expected controller metadata dir or residual marker under {}",
        log_dir.display()
    );
}

fn find_named(root: &std::path::Path, name: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.file_name().and_then(|n| n.to_str()) == Some(name) {
                found.push(path.clone());
            }
            if path.is_dir() {
                stack.push(path);
            }
        }
    }
    found
}

#[test]
fn dump_log_binary_should_print_records_from_synthetic_segment() {
    let dir = TempDir::new().expect("dir");
    let log_path = dir.path().join("00000000000000000010.log");
    fs::write(&log_path, sample_log_bytes()).expect("write log");

    kafka_bin()
        .args([
            "dump-log",
            "--files",
            log_path.to_str().expect("path"),
            "--print-data-log",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Log starting offset: 10"))
        .stdout(predicate::str::contains("baseOffset: 10"))
        .stdout(predicate::str::contains("key: k0 payload: v0"));
}

#[test]
fn dump_log_should_decode_end_txn_control_records_via_binary() {
    let dir = TempDir::new().expect("dir");
    let log_path = dir.path().join("00000000000000000100.log");
    let mut value = Vec::new();
    value.extend_from_slice(&0_i16.to_be_bytes());
    value.extend_from_slice(&9_i32.to_be_bytes());
    fs::write(&log_path, control_batch_bytes(1, &value)).expect("write");

    kafka_bin()
        .args([
            "dump-log",
            "--files",
            log_path.to_str().expect("path"),
            "--deep-iteration",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("isControl: true"))
        .stdout(predicate::str::contains("endTxnMarker: COMMIT"))
        .stdout(predicate::str::contains("coordinatorEpoch: 9"));
}

#[test]
fn dump_log_should_dump_txnindex_and_producer_snapshot() {
    let dir = TempDir::new().expect("dir");
    let txn = dir.path().join("00000000000000000000.txnindex");
    let mut txn_bytes = Vec::new();
    txn_bytes.extend_from_slice(&0_i16.to_be_bytes());
    txn_bytes.extend_from_slice(&7_i64.to_be_bytes());
    txn_bytes.extend_from_slice(&10_i64.to_be_bytes());
    txn_bytes.extend_from_slice(&20_i64.to_be_bytes());
    txn_bytes.extend_from_slice(&25_i64.to_be_bytes());
    fs::write(&txn, &txn_bytes).expect("txnindex");

    let snap = dir.path().join("00000000000000000042.snapshot");
    let mut snap_bytes = Vec::new();
    snap_bytes.extend_from_slice(&1_i16.to_be_bytes());
    snap_bytes.extend_from_slice(&0u32.to_be_bytes());
    snap_bytes.extend_from_slice(&1_i32.to_be_bytes());
    snap_bytes.extend_from_slice(&99_i64.to_be_bytes());
    snap_bytes.extend_from_slice(&3_i16.to_be_bytes());
    snap_bytes.extend_from_slice(&15_i32.to_be_bytes());
    snap_bytes.extend_from_slice(&100_i64.to_be_bytes());
    snap_bytes.extend_from_slice(&5_i32.to_be_bytes());
    snap_bytes.extend_from_slice(&1_700_000_000_000_i64.to_be_bytes());
    snap_bytes.extend_from_slice(&1_i32.to_be_bytes());
    snap_bytes.extend_from_slice(&(-1_i64).to_be_bytes());
    fs::write(&snap, &snap_bytes).expect("snapshot");

    kafka_bin()
        .args(["dump-log", "--files", txn.to_str().expect("p")])
        .assert()
        .success()
        .stdout(predicate::str::contains("producerId: 7"));

    kafka_bin()
        .args(["dump-log", "--files", snap.to_str().expect("p")])
        .assert()
        .success()
        .stdout(predicate::str::contains("Producer snapshot version: 1"))
        .stdout(predicate::str::contains("producerId: 99"));
}

#[test]
fn dump_log_should_diagnose_text_bootstrap_checkpoint_via_binary() {
    let dir = TempDir::new().expect("dir");
    let path = dir.path().join("bootstrap.checkpoint");
    fs::write(
        &path,
        "{\n  \"format\": \"partial-native-bootstrap-v1\"\n}\n",
    )
    .expect("write");

    let output = ProcessCommand::new(Command::cargo_bin("kafka").expect("bin").get_program())
        .args(["dump-log", "--files", path.to_str().expect("p")])
        .output()
        .expect("run dump-log");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Non-binary")
            || stdout.contains("residual")
            || stdout.contains("bootstrap"),
        "unexpected dump-log output: {stdout}"
    );
    assert!(!stdout.contains("Found invalid bytes at the end"));
}

fn sample_log_bytes() -> Vec<u8> {
    let mut batch = RecordBatch::new();
    batch.base_offset = 10;
    batch.last_offset_delta = 1;
    batch.base_timestamp = 1_700_000_000_000;
    batch.max_timestamp = 1_700_000_000_001;
    batch.add_record(
        Record::new(
            Some(bytes::Bytes::from_static(b"k0")),
            Some(bytes::Bytes::from_static(b"v0")),
        )
        .with_offset_delta(0),
    );
    batch.add_record(
        Record::new(
            Some(bytes::Bytes::from_static(b"k1")),
            Some(bytes::Bytes::from_static(b"v1")),
        )
        .with_offset_delta(1)
        .with_timestamp_delta(1),
    );
    batch.encode().expect("encode").to_vec()
}

fn control_batch_bytes(type_id: i16, value: &[u8]) -> Vec<u8> {
    let mut key = Vec::new();
    key.extend_from_slice(&0_i16.to_be_bytes());
    key.extend_from_slice(&type_id.to_be_bytes());
    let mut batch = RecordBatch::new();
    batch.base_offset = 100;
    batch.last_offset_delta = 0;
    batch.base_timestamp = 1_700_000_000_000;
    batch.max_timestamp = 1_700_000_000_000;
    batch.attributes.is_control_batch = true;
    batch.attributes.is_transactional = type_id == 0 || type_id == 1;
    batch.producer_id = 42;
    batch.producer_epoch = 1;
    batch.base_sequence = 0;
    batch.add_record(
        Record::new(
            Some(bytes::Bytes::from(key)),
            Some(bytes::Bytes::copy_from_slice(value)),
        )
        .with_offset_delta(0),
    );
    batch.encode().expect("encode control").to_vec()
}
