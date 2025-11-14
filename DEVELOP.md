# Development Setup

## Windows Development Prerequisites

To build kafka-cli on Windows, you need one of the following setups:

### Option 1: Install CMake (Recommended for Windows)

1. Download and install CMake from https://cmake.org/download/
2. Add CMake to your PATH
3. Build with: `cargo build`

The project will automatically build librdkafka from source using CMake.

### Option 2: Install librdkafka via vcpkg

```powershell
# Install vcpkg
git clone https://github.com/microsoft/vcpkg C:\vcpkg
cd C:\vcpkg
.\bootstrap-vcpkg.bat

# Install librdkafka
.\vcpkg install librdkafka:x64-windows

# Set environment variable
$env:PKG_CONFIG_PATH = "C:\vcpkg\installed\x64-windows\lib\pkgconfig"

# Update Cargo.toml to use dynamic-linking feature
```

Then change in `Cargo.toml`:
```toml
rdkafka = { version = "0.38.0", default-features = false, features = ["dynamic-linking", "tokio"] }
```

### Option 3: Use WSL or Docker

Develop inside WSL2 or a Docker container with Linux, where librdkafka is easier to install.

## Linux Development Prerequisites

### Ubuntu/Debian
```bash
sudo apt-get update
sudo apt-get install -y librdkafka-dev build-essential pkg-config
```

### Fedora/RHEL/CentOS
```bash
sudo dnf install -y librdkafka-devel gcc pkg-config
```

### Arch Linux
```bash
sudo pacman -S librdkafka base-devel
```

## macOS Development Prerequisites

```bash
brew install librdkafka cmake
```

## Building

```bash
# Debug build
cargo build

# Release build (optimized)
cargo build --release

# Run tests
cargo test

# Run with logging
RUST_LOG=debug cargo run -- --help
```

## Project Structure

```
kafka-cli/
├── src/
│   ├── main.rs              # Application entry point
│   ├── cli/                 # CLI definitions
│   │   └── mod.rs          # Command-line argument parsing
│   ├── commands/            # Command implementations
│   │   ├── mod.rs
│   │   ├── topics.rs       # Topic management
│   │   ├── produce.rs      # Producer command
│   │   ├── consume.rs      # Consumer command
│   │   ├── consumer_groups.rs  # Consumer group management
│   │   └── configs.rs      # Configuration management
│   ├── kafka/               # Kafka client wrappers
│   │   ├── mod.rs
│   │   ├── admin.rs        # Admin client operations
│   │   ├── producer.rs     # Producer wrapper
│   │   └── consumer.rs     # Consumer wrapper
│   ├── config/              # Configuration management
│   │   └── mod.rs          # Config file parsing
│   └── utils/               # Utility functions
│       └── mod.rs
├── docs/                    # Documentation
│   └── requirement.md      # Original requirements (Chinese)
├── Cargo.toml              # Rust dependencies
├── README.md               # User documentation
├── INSTALL.md              # Installation guide
├── DEVELOP.md              # This file
└── example.properties      # Example configuration file
```

## Development Workflow

### Adding a New Command

1. Define the CLI arguments in `src/cli/mod.rs`
2. Create a new command module in `src/commands/`
3. Implement the command handler function
4. Add the command to the match statement in `src/main.rs`
5. Update documentation in README.md

Example:
```rust
// 1. In src/cli/mod.rs
#[derive(Subcommand)]
pub enum Commands {
    // ... existing commands
    NewCommand(NewCommandArgs),
}

#[derive(Parser)]
pub struct NewCommandArgs {
    #[arg(long)]
    pub example_arg: String,
}

// 2. Create src/commands/new_command.rs
use anyhow::Result;
use crate::cli::NewCommandArgs;

pub async fn handle_new_command(args: NewCommandArgs) -> Result<()> {
    println!("Executing new command with arg: {}", args.example_arg);
    Ok(())
}

// 3. Add to src/commands/mod.rs
pub mod new_command;
pub use new_command::*;

// 4. Update src/main.rs
Commands::NewCommand(args) => commands::handle_new_command(args).await,
```

### Testing

Run unit tests:
```bash
cargo test
```

Run integration tests (requires a running Kafka cluster):
```bash
# Start Kafka with Docker
docker run -d --name kafka -p 9092:9092 apache/kafka:latest

# Run tests
cargo test -- --ignored
```

### Code Quality

Format code:
```bash
cargo fmt
```

Run linter:
```bash
cargo clippy
```

Check for issues:
```bash
cargo check
```

### Debugging

Enable debug logging:
```bash
# Windows PowerShell
$env:RUST_LOG="debug"; cargo run -- topics list

# Linux/macOS
RUST_LOG=debug cargo run -- topics list
```

Use Rust debugger:
```bash
# Install rust-gdb (Linux) or rust-lldb (macOS)
rust-gdb target/debug/kafka-cli
```

## Common Issues

### rdkafka build failures

**Problem**: CMake or pkg-config not found
**Solution**: Install CMake or use vcpkg to install librdkafka

**Problem**: OpenSSL errors
**Solution**: Install OpenSSL development libraries or use `cmake-build` feature

### Connection issues during testing

**Problem**: Can't connect to Kafka
**Solution**: Ensure Kafka is running and accessible at the specified bootstrap server

### Performance testing

To test with a real Kafka cluster:
```bash
# Create a test topic
cargo run -- topics --bootstrap-server localhost:9092 create --topic test --partitions 3

# Produce test messages
for i in {1..1000}; do echo "key$i	value$i"; done | cargo run -- produce --bootstrap-server localhost:9092 --topic test

# Consume messages
cargo run -- consume --bootstrap-server localhost:9092 --topic test --from-beginning --max-messages 10
```

## Contributing

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/my-feature`)
3. Make your changes
4. Run tests and linting
5. Commit your changes (`git commit -am 'Add my feature'`)
6. Push to the branch (`git push origin feature/my-feature`)
7. Create a Pull Request

## Release Process

1. Update version in `Cargo.toml`
2. Update CHANGELOG.md
3. Build release binaries:
   ```bash
   cargo build --release
   ```
4. Test the release binary
5. Create a git tag:
   ```bash
   git tag -a v0.1.0 -m "Release v0.1.0"
   git push origin v0.1.0
   ```
6. Publish to crates.io (if applicable):
   ```bash
   cargo publish
   ```

## Resources

- [rdkafka documentation](https://docs.rs/rdkafka/)
- [Apache Kafka documentation](https://kafka.apache.org/documentation/)
- [Rust async book](https://rust-lang.github.io/async-book/)
- [clap documentation](https://docs.rs/clap/)
