# 构建状态和已知问题

## 当前状态

项目已完成 Phase 1 (MVP) 的代码实现，包括：

✅ **已完成代码**：
- 主题管理 (`topics`)
- 消息生产 (`produce`)  
- 消息消费 (`consume`)
- 配置管理 (`configs`)
- 消费组管理框架 (`consumer-groups`)

## Windows 构建问题

### 问题描述

在 Windows 上使用 MSVC 工具链编译时，会遇到 zlib 链接错误：

```
error LNK2001: unresolved external symbol __imp_crc32
error LNK2019: unresolved external symbol __imp_deflate
error LNK2019: unresolved external symbol __imp_inflate
```

这是 `rdkafka-sys` 在 Windows 上的已知问题，与 zlib 动态/静态链接配置相关。

### 解决方案

#### 方案 1: 使用 vcpkg 安装 librdkafka (推荐)

```powershell
# 安装 vcpkg
git clone https://github.com/microsoft/vcpkg C:\vcpkg
cd C:\vcpkg
.\bootstrap-vcpkg.bat

# 安装 librdkafka 和 zlib
.\vcpkg install librdkafka:x64-windows

# 设置环境变量
$env:PKG_CONFIG_PATH = "C:\vcpkg\installed\x64-windows\lib\pkgconfig"

# 修改 Cargo.toml
# rdkafka = { version = "0.38.0", default-features = false, features = ["dynamic-linking", "tokio"] }
```

然后在 Cargo.toml 中使用：
```toml
rdkafka = { version = "0.38.0", default-features = false, features = ["dynamic-linking", "tokio"] }
```

#### 方案 2: 使用 WSL2 (Linux 环境)

```bash
# 在 WSL2 中
sudo apt-get install -y librdkafka-dev build-essential pkg-config
cargo build --release
```

#### 方案 3: 使用 Docker

```dockerfile
FROM rust:latest
WORKDIR /app
COPY . .
RUN apt-get update && apt-get install -y librdkafka-dev
RUN cargo build --release
```

#### 方案 4: 使用预编译的二进制文件

从 GitHub Releases 下载预编译的 Windows 二进制文件（待发布）。

## Linux 构建

✅ Linux 上构建应该没有问题：

```bash
# Ubuntu/Debian
sudo apt-get install -y librdkafka-dev
cargo build --release

# Fedora/RHEL
sudo dnf install -y librdkafka-devel
cargo build --release
```

## macOS 构建

✅ macOS 上构建应该没有问题：

```bash
brew install librdkafka
cargo build --release
```

## 下一步

1. **在 Linux 或 macOS 上完成编译测试**
2. **使用 vcpkg 方案解决 Windows 构建问题**
3. **设置 CI/CD 进行跨平台构建**
4. **生成预编译的二进制文件**

## 相关问题

- [rdkafka-rust#158](https://github.com/fede1024/rust-rdkafka/issues/158) - Windows build issues
- [rdkafka-sys Windows build](https://github.com/fede1024/rust-rdkafka/tree/master/rdkafka-sys#windows)

## 临时测试方案

如果需要立即测试功能，建议：

1. 使用 Docker 运行 Kafka
2. 在 WSL2 中编译和运行 kafka-cli
3. 或者等待 vcpkg 方案配置完成

---

**更新日期**: 2025-11-14  
**状态**: 代码完成，Windows 构建问题待解决
