# Installation Guide

## Prerequisites

### Windows

You need to install librdkafka. You can use one of the following methods:

#### Option 1: Using vcpkg (Recommended)

```powershell
# Install vcpkg if you haven't already
git clone https://github.com/microsoft/vcpkg
cd vcpkg
.\bootstrap-vcpkg.bat
.\vcpkg integrate install

# Install librdkafka
.\vcpkg install librdkafka
```

#### Option 2: Using pre-built binaries

Download pre-built librdkafka binaries from the [Confluent Platform](https://www.confluent.io/download/) or build from source.

### Linux

#### Ubuntu/Debian

```bash
sudo apt-get install -y librdkafka-dev
```

#### Fedora/CentOS/RHEL

```bash
sudo dnf install -y librdkafka-devel
```

#### Arch Linux

```bash
sudo pacman -S librdkafka
```

### macOS

```bash
brew install librdkafka
```

## Building

Once librdkafka is installed:

```bash
cargo build --release
```

The compiled binary will be at `target/release/kafka-cli` (or `kafka-cli.exe` on Windows).

## Installation

### Copy to PATH

#### Windows (PowerShell)

```powershell
copy target\release\kafka-cli.exe C:\Windows\System32\
# Or add to your user bin directory
```

#### Linux/macOS

```bash
sudo cp target/release/kafka-cli /usr/local/bin/
```

### Or run from the build directory

```bash
# From the project root
./target/release/kafka-cli --help
```

## Verifying Installation

```bash
kafka-cli --help
```

You should see the help message with available commands.

## Quick Start

1. Start a local Kafka cluster (using Docker):

```bash
docker run -d --name kafka -p 9092:9092 apache/kafka:latest
```

2. Create a topic:

```bash
kafka-cli topics --bootstrap-server localhost:9092 create --topic test --partitions 1 --replication-factor 1
```

3. Produce messages:

```bash
echo "key1	value1" | kafka-cli produce --bootstrap-server localhost:9092 --topic test
```

4. Consume messages:

```bash
kafka-cli consume --bootstrap-server localhost:9092 --topic test --from-beginning
```

## Troubleshooting

### "librdkafka not found"

Make sure librdkafka is installed and in your system's library path.

On Linux, you may need to run:
```bash
sudo ldconfig
```

On Windows, ensure the DLL is in your PATH or in the same directory as the executable.

### Build errors

If you encounter build errors, try cleaning the build directory:

```bash
cargo clean
cargo build --release
```

### Connection errors

Check that:
- Kafka is running on the specified bootstrap server
- The port is not blocked by a firewall
- Network connectivity is available

For more help, see the [README.md](README.md) or open an issue on GitHub.
