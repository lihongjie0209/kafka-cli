//! Compatibility boundary test against an Apache Kafka 3.6.2 distribution.

use std::{
    fs,
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand, Stdio},
    thread,
    time::{Duration, Instant},
};

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

struct Broker {
    child: Child,
}

impl Drop for Broker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .expect("bind temporary port")
        .local_addr()
        .expect("temporary address")
        .port()
}

fn kafka_script(home: &Path, name: &str) -> PathBuf {
    home.join("bin").join(name)
}

fn start_kafka_3_6(home: &Path, fixture: &TempDir) -> (Broker, String) {
    let broker_port = free_port();
    let controller_port = free_port();
    let bootstrap = format!("127.0.0.1:{broker_port}");
    let config = fixture.path().join("server.properties");
    let logs = fixture.path().join("logs");
    fs::write(
        &config,
        format!(
            "process.roles=broker,controller\n\
             node.id=1\n\
             controller.quorum.voters=1@127.0.0.1:{controller_port}\n\
             listeners=PLAINTEXT://127.0.0.1:{broker_port},CONTROLLER://127.0.0.1:{controller_port}\n\
             advertised.listeners=PLAINTEXT://127.0.0.1:{broker_port}\n\
             listener.security.protocol.map=CONTROLLER:PLAINTEXT,PLAINTEXT:PLAINTEXT\n\
             controller.listener.names=CONTROLLER\n\
             inter.broker.listener.name=PLAINTEXT\n\
             log.dirs={}\n\
             offsets.topic.replication.factor=1\n\
             transaction.state.log.replication.factor=1\n\
             transaction.state.log.min.isr=1\n\
             group.initial.rebalance.delay.ms=0\n",
            logs.display()
        ),
    )
    .expect("write Kafka configuration");
    let cluster_id = ProcessCommand::new(kafka_script(home, "kafka-storage.sh"))
        .arg("random-uuid")
        .output()
        .expect("generate Kafka cluster ID");
    assert!(
        cluster_id.status.success(),
        "kafka-storage random-uuid failed"
    );
    let cluster_id = String::from_utf8(cluster_id.stdout)
        .expect("cluster ID output")
        .trim()
        .to_owned();
    let format = ProcessCommand::new(kafka_script(home, "kafka-storage.sh"))
        .args(["format", "--ignore-formatted", "-t", &cluster_id, "-c"])
        .arg(&config)
        .output()
        .expect("format Kafka storage");
    assert!(
        format.status.success(),
        "kafka-storage format failed: {}",
        String::from_utf8_lossy(&format.stderr)
    );
    let child = ProcessCommand::new(kafka_script(home, "kafka-server-start.sh"))
        .arg(&config)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start Kafka 3.6.2");
    let broker = Broker { child };
    let deadline = Instant::now() + Duration::from_secs(45);
    while Instant::now() < deadline {
        if TcpStream::connect(&bootstrap).is_ok() {
            return (broker, bootstrap);
        }
        thread::sleep(Duration::from_millis(200));
    }
    panic!("Kafka 3.6.2 did not open {bootstrap}");
}

#[test]
#[ignore = "requires KAFKA_36_HOME pointing to Apache Kafka 3.6.2"]
#[expect(
    clippy::too_many_lines,
    reason = "one broker fixture verifies the Kafka 3.6 compatibility boundary"
)]
fn protocol_and_admin_commands_work_against_kafka_3_6_2() {
    let home = std::env::var_os("KAFKA_36_HOME")
        .map(PathBuf::from)
        .expect("KAFKA_36_HOME must point to Kafka 3.6.2");
    let fixture = TempDir::new().expect("fixture directory");
    let (_broker, bootstrap) = start_kafka_3_6(&home, &fixture);

    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "topics",
            "create",
            "--topic",
            "kafka-36-events",
        ])
        .assert()
        .success();
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args(["--bootstrap-server", &bootstrap, "topics", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("kafka-36-events"));
    let described_topic = Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "--output",
            "json",
            "topics",
            "describe",
            "--topic",
            "kafka-36-events",
        ])
        .output()
        .expect("describe Kafka 3.6 topic");
    assert!(described_topic.status.success());
    let described_topic: serde_json::Value =
        serde_json::from_slice(&described_topic.stdout).expect("topic JSON");
    let topic_id = described_topic["data"][0]["topic_id"]
        .as_str()
        .expect("Kafka 3.6 topic ID");
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "topics",
            "describe",
            "--topic-id",
            topic_id,
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("kafka-36-events"));
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args(["--bootstrap-server", &bootstrap, "api-versions"])
        .assert()
        .success()
        .stdout(predicate::str::contains("ApiVersions"));
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args(["--bootstrap-server", &bootstrap, "log-dirs"])
        .assert()
        .success()
        .stdout(predicate::str::contains("kafka-36-events"));
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "offsets",
            "--topic",
            "kafka-36-events",
            "--time",
            "max-timestamp",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("OFFSET"))
        .stdout(predicate::str::contains("kafka-36-events").not());
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args(["--bootstrap-server", &bootstrap, "cluster", "id"])
        .assert()
        .success()
        .stdout(predicate::str::contains("CLUSTER_ID"));
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "groups",
            "list",
            "--state",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("STATE"));
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args(["--bootstrap-server", &bootstrap, "all-groups", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("PROTOCOL"));
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args(["--bootstrap-server", &bootstrap, "share-groups", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("GROUP"));
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "configs",
            "alter",
            "--entity-type",
            "users",
            "--entity-name",
            "kafka-36-user",
            "--add-config",
            "SCRAM-SHA-512=[iterations=4096,password=integration-secret]",
            "--execute",
        ])
        .assert()
        .success();
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "configs",
            "describe",
            "--entity-type",
            "users",
            "--entity-name",
            "kafka-36-user",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("SCRAM-SHA-512"));
    Command::cargo_bin("kafka")
        .expect("kafka binary")
        .args([
            "--bootstrap-server",
            &bootstrap,
            "configs",
            "alter",
            "--entity-type",
            "users",
            "--entity-name",
            "kafka-36-user",
            "--delete-config",
            "SCRAM-SHA-512",
            "--execute",
        ])
        .assert()
        .success();
}
