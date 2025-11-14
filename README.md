# kafka-cli

A cross-platform Kafka CLI tool written in Rust, providing a modern alternative to Kafka's shell scripts.

## Features

- **Cross-platform**: Works on Windows, Linux, and macOS
- **High performance**: Built with Rust for speed and memory safety
- **User-friendly**: Clear command structure and helpful error messages
- **Full-featured**: Implements core Kafka operations
- **Well-tested**: Comprehensive integration and functional tests ✅
  - Rust Integration Tests: 12/12 passed
  - Python CLI Functional Tests: 11/11 passed

## Installation

### Build from source

✅ **Windows**: Successfully builds with vcpkg! See [BUILD_SUCCESS.md](BUILD_SUCCESS.md) for detailed instructions.

```bash
git clone <repository-url>
cd kafka-cli

# Linux/macOS
cargo build --release

# Windows (requires vcpkg with librdkafka installed)
# See BUILD_SUCCESS.md for detailed instructions
```

The binary will be available at `target/release/kafka-cli` (or `kafka-cli.exe` on Windows).

## Usage

### Topics Management

List all topics:
```bash
kafka-cli topics --bootstrap-server localhost:9092 list
```

Create a topic:
```bash
kafka-cli topics --bootstrap-server localhost:9092 create \
  --topic my-topic \
  --partitions 3 \
  --replication-factor 1
```

Describe topics:
```bash
kafka-cli topics --bootstrap-server localhost:9092 describe --topic my-topic
```

Delete a topic:
```bash
kafka-cli topics --bootstrap-server localhost:9092 delete --topic my-topic
```

Alter topic configuration:
```bash
kafka-cli topics --bootstrap-server localhost:9092 alter \
  --topic my-topic \
  --add-config retention.ms=86400000
```

### Producer

Produce messages from stdin (key and value separated by tab):
```bash
kafka-cli produce --bootstrap-server localhost:9092 --topic my-topic
```

Produce with custom key separator:
```bash
kafka-cli produce --bootstrap-server localhost:9092 \
  --topic my-topic \
  --key-separator ":"
```

Enable compression:
```bash
kafka-cli produce --bootstrap-server localhost:9092 \
  --topic my-topic \
  --compression-type gzip
```

### Consumer

Consume messages from a topic:
```bash
kafka-cli consume --bootstrap-server localhost:9092 --topic my-topic
```

Consume from beginning:
```bash
kafka-cli consume --bootstrap-server localhost:9092 \
  --topic my-topic \
  --from-beginning
```

Consume with a consumer group:
```bash
kafka-cli consume --bootstrap-server localhost:9092 \
  --topic my-topic \
  --group my-consumer-group
```

Output as JSON:
```bash
kafka-cli consume --bootstrap-server localhost:9092 \
  --topic my-topic \
  --formatter json
```

Limit number of messages:
```bash
kafka-cli consume --bootstrap-server localhost:9092 \
  --topic my-topic \
  --max-messages 10
```

### Configuration Management

Describe topic configuration:
```bash
kafka-cli configs --bootstrap-server localhost:9092 describe \
  --entity-type topics \
  --entity-name my-topic
```

Alter configuration:
```bash
kafka-cli configs --bootstrap-server localhost:9092 alter \
  --entity-type topics \
  --entity-name my-topic \
  --add-config retention.ms=86400000 \
  --add-config compression.type=gzip
```

### Consumer Groups

List all consumer groups:
```bash
kafka-cli consumer-groups --bootstrap-server localhost:9092 list
```

Describe a consumer group:
```bash
kafka-cli consumer-groups --bootstrap-server localhost:9092 describe \
  --group my-consumer-group
```

Reset offsets (dry-run):
```bash
kafka-cli consumer-groups --bootstrap-server localhost:9092 reset-offsets \
  --group my-consumer-group \
  --topic my-topic \
  --to-earliest
```

Reset offsets (execute):
```bash
kafka-cli consumer-groups --bootstrap-server localhost:9092 reset-offsets \
  --group my-consumer-group \
  --topic my-topic \
  --to-earliest \
  --execute
```

Delete a consumer group:
```bash
kafka-cli consumer-groups --bootstrap-server localhost:9092 delete \
  --group my-consumer-group
```

## Configuration Files

You can use configuration files in properties format:

```properties
# config.properties
bootstrap.servers=localhost:9092
security.protocol=SASL_SSL
sasl.mechanism=PLAIN
sasl.username=myuser
sasl.password=mypassword
```

Use with any command:
```bash
kafka-cli topics --command-config config.properties list
```

## Environment Variables

Enable debug logging:
```bash
# Windows PowerShell
$env:RUST_LOG="debug"

# Linux/macOS
export RUST_LOG=debug
```

## Testing

### Run Integration Tests

Start the local Kafka cluster using Docker Compose:

```bash
cd docker/single-node
docker-compose up -d
cd ../..
```

Run the integration tests:

```bash
# Windows (with vcpkg environment)
$env:RUST_LOG = "info"
cargo test --test integration_test -- --nocapture --test-threads=1

# Linux/macOS
RUST_LOG=info cargo test --test integration_test -- --nocapture --test-threads=1
```

**Test Coverage**: 
- ✅ Rust Integration Tests: 12/12 passed - See [TEST_REPORT.md](TEST_REPORT.md)
- ✅ Python CLI Functional Tests: 11/11 passed - See [CLI_TEST_REPORT.md](CLI_TEST_REPORT.md)

Test categories:
- Admin API: Topics management, configurations  
- Producer API: Single/batch message production
- Consumer API: Message consumption with key-value support
- Consumer Groups: List, describe, reset offsets
- End-to-end workflow: Full lifecycle testing
- CLI Commands: Help, version, error handling

## Development

### Prerequisites

- Rust 1.70 or later
- A running Kafka cluster for testing

### Building

```bash
cargo build
```

### Running with logging

```bash
RUST_LOG=debug cargo run -- topics --bootstrap-server localhost:9092 list
```

## Architecture

The project is organized into several modules:

- `cli/`: Command-line interface definitions using clap
- `commands/`: Command handlers for each subcommand
- `kafka/`: Kafka client wrappers (admin, producer, consumer)
- `config/`: Configuration management and parsing
- `utils/`: Common utility functions

## Roadmap

### Phase 1: Core Commands (✅ Completed)
- [x] Topics management
- [x] Console producer
- [x] Console consumer
- [x] Configuration management
- [ ] Consumer groups management (in progress)

### Phase 2: Operational Tools
- [ ] Producer performance test
- [ ] Consumer performance test
- [ ] ACL management
- [ ] Partition reassignment
- [ ] Log directory information
- [ ] Get offsets
- [ ] Delete records

### Phase 3: Advanced Features
- [ ] Verifiable producer/consumer
- [ ] Replica verification
- [ ] Leader election
- [ ] Broker API versions
- [ ] JMX tools

### Phase 4: Specialized Tools
- [ ] KRaft storage tools
- [ ] Metadata quorum management
- [ ] Delegation tokens
- [ ] Transactions
- [ ] Share consumer features

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## License

[Add your license here]

## Acknowledgments

- Built with [rdkafka](https://github.com/fede1024/rust-rdkafka)
- CLI powered by [clap](https://github.com/clap-rs/clap)
- Async runtime by [tokio](https://github.com/tokio-rs/tokio)
