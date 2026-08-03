# kafka-cli

`kafka-cli` is a native Kafka command-line client written in Rust. It combines
the common Apache Kafka client and administration scripts behind one binary.

## Build

```shell
cargo build --release --locked
```

Use `--features bundled` to compile the packaged librdkafka and vendored
OpenSSL instead of linking the system librdkafka. `Dockerfile.release` contains
the reproducible release build environment.

The resulting executable is `target/release/kafka`. It supports Kafka client
properties through `--command-config`; command-line values take precedence.

## Examples

```shell
kafka --bootstrap-server localhost:9092 topics create \
  --topic events --partitions 3 --replication-factor 1

printf 'key\tvalue\n' | kafka --bootstrap-server localhost:9092 \
  produce --topic events --parse-key

kafka --bootstrap-server localhost:9092 consume \
  --topic events --from-beginning --max-messages 1 --json

kafka --bootstrap-server localhost:9092 share-consume \
  --topic events --group shared-workers --max-messages 1

kafka --bootstrap-server localhost:9092 groups list --output json
```

Destructive operations in the unified CLI preview their work unless
`--execute` is specified. Topic deletion follows Kafka's direct behavior for
compatibility.

## Command coverage

The binary exposes `topics`, `produce`, `consume`, `share-consume`, `groups`, `all-groups`,
`share-groups`, `streams-groups`, `streams-application-reset`, `configs`, `offsets`, `acls`, `reassign`, `delete-records`,
`leader-election`, `log-dirs`, `api-versions`, `cluster`, `client-metrics`,
`features`, `transactions`, `metadata-quorum`, and `delegation-tokens` command families. Run
`kafka <command> --help` for details.

Topic administration, production, consumption, group discovery/reset, topic,
broker and group configuration, offsets, record deletion, cluster ID and
endpoint discovery are implemented with librdkafka. Reassignment, log-directory
inspection, broker API-version discovery, leader election, ACL administration,
and broker unregistration use Kafka's native wire protocols; no Kafka JVM or
shell scripts are required at runtime.

Topic creation and partition expansion accept Kafka-compatible manual
`--replica-assignment` values. Consumer-group resets support earliest/latest,
absolute and shifted offsets as well as `--to-current`, `--to-datetime`, and
ISO-8601 `--by-duration` targets.

Share and Kafka Streams group management provide their original list,
describe, delete, reset-offsets, and delete-offsets actions. Streams describe
includes state, member task assignments, offset lag, and Kafka 4.4 topology
descriptions; tabular output is rendered through the shared table library.
The legacy Streams application reset workflow is also available: it guards or
force-removes active classic group members, resets input offsets, seeks
intermediate topics to their end offsets, and deletes only inferred Kafka
Streams internal topics. Use `--dry-run` before executing this irreversible
workflow.

The console Share consumer uses KIP-932 ShareFetch/ShareAcknowledge with
explicit acknowledgements. Successful records can be accepted (default),
released, or rejected, and formatter failures can be rejected without stopping
the process. Its formatter and JSON modes share the regular console consumer's
native output implementation.

## Authentication

PLAINTEXT, SSL, SASL/PLAIN and SASL/SCRAM are configured with standard Kafka
properties, for example:

```properties
security.protocol=SASL_SSL
sasl.mechanism=SCRAM-SHA-512
sasl.username=alice
sasl.password=secret
ssl.ca.location=/etc/ssl/certs/cluster-ca.pem
```

Secrets are passed directly to librdkafka and are never included in command
output.

## Compatibility aliases

Run `scripts/install-aliases.sh /path/to/kafka` to create `kafka-topics`,
`kafka-console-producer`, and the other supported Kafka-style names beside the
binary. The dispatcher also recognizes names ending in `.sh`.
