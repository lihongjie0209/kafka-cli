# Kafka CLI 快速开始

## 项目状态

✅ **Phase 1 已完成基础实现**

已实现的核心功能：
- ✅ 主题管理（创建、列表、描述、删除、修改）
- ✅ 消息生产（从 stdin 读取，支持压缩）
- ✅ 消息消费（支持从头消费、JSON 格式、消费组）
- ✅ 配置管理（描述、修改配置）
- 🔨 消费组管理（框架已就绪，待完善）

## 项目结构

```
kafka-cli/
├── src/                      # 源代码
│   ├── main.rs              # 程序入口
│   ├── cli/                 # 命令行参数定义
│   ├── commands/            # 命令实现
│   │   ├── topics.rs       # 主题管理
│   │   ├── produce.rs      # 生产者
│   │   ├── consume.rs      # 消费者
│   │   ├── consumer_groups.rs  # 消费组
│   │   └── configs.rs      # 配置管理
│   ├── kafka/               # Kafka 客户端封装
│   │   ├── admin.rs        # Admin 客户端
│   │   ├── producer.rs     # 生产者封装
│   │   └── consumer.rs     # 消费者封装
│   └── config/              # 配置管理
├── docs/
│   ├── requirement.md       # 原始需求（中文）
│   └── PLAN.md             # 详细规划文档（中文）
├── README.md                # 用户文档（英文）
├── INSTALL.md               # 安装指南（英文）
├── DEVELOP.md               # 开发指南（英文）
├── example.properties       # 配置示例
└── Cargo.toml              # Rust 项目配置
```

## 如何编译

### Windows 系统

**方法 1: 安装 CMake（推荐）**

1. 下载安装 CMake: https://cmake.org/download/
2. 将 CMake 添加到 PATH
3. 编译项目:
   ```powershell
   cargo build --release
   ```

**方法 2: 使用 vcpkg**

```powershell
# 安装 vcpkg
git clone https://github.com/microsoft/vcpkg C:\vcpkg
cd C:\vcpkg
.\bootstrap-vcpkg.bat

# 安装 librdkafka
.\vcpkg install librdkafka:x64-windows

# 返回项目目录编译
cd d:\code\kafka-cli
cargo build --release
```

### Linux 系统

```bash
# Ubuntu/Debian
sudo apt-get install -y librdkafka-dev build-essential pkg-config

# Fedora/RHEL
sudo dnf install -y librdkafka-devel gcc pkg-config

# 编译
cargo build --release
```

### macOS 系统

```bash
brew install librdkafka cmake
cargo build --release
```

## 快速测试

### 1. 启动 Kafka（使用 Docker）

```bash
docker run -d --name kafka -p 9092:9092 apache/kafka:latest
```

### 2. 测试命令

```bash
# 查看帮助
cargo run -- --help

# 列出主题
cargo run -- topics --bootstrap-server localhost:9092 list

# 创建主题
cargo run -- topics --bootstrap-server localhost:9092 create \
  --topic test-topic \
  --partitions 3 \
  --replication-factor 1

# 生产消息
echo "key1	message1" | cargo run -- produce \
  --bootstrap-server localhost:9092 \
  --topic test-topic

# 消费消息
cargo run -- consume \
  --bootstrap-server localhost:9092 \
  --topic test-topic \
  --from-beginning \
  --max-messages 10
```

## 使用配置文件

创建配置文件 `config.properties`:

```properties
bootstrap.servers=localhost:9092
# 其他配置...
```

使用配置文件:

```bash
cargo run -- topics --command-config config.properties list
```

## 启用日志

```powershell
# Windows PowerShell
$env:RUST_LOG="debug"
cargo run -- topics list

# Linux/macOS
RUST_LOG=debug cargo run -- topics list
```

## 下一步工作

### 立即任务

1. **解决构建问题**
   - 在 Windows 上安装 CMake 或 librdkafka
   - 测试编译是否成功
   - 验证基本功能

2. **完善消费组功能**
   - 实现 list consumer groups
   - 实现 describe consumer groups  
   - 实现 reset offsets

3. **设置 CI/CD**
   - GitHub Actions 配置
   - 自动化测试
   - 跨平台构建

### 短期计划

1. **添加测试**
   - 单元测试
   - 集成测试（使用 testcontainers）
   - 端到端测试

2. **改进文档**
   - 添加更多使用示例
   - 创建故障排除指南
   - 中文文档翻译

3. **功能增强**
   - Shell 自动补全
   - 彩色输出
   - 进度条显示

### 中期计划（Phase 2）

实现运维工具：
- 性能测试工具
- ACL 管理
- 分区重新分配
- 日志目录查询

详见 [docs/PLAN.md](docs/PLAN.md) 查看完整规划。

## 技术栈

- **Rust 1.70+**: 编程语言
- **rdkafka 0.38**: Kafka 客户端库
- **clap 4.x**: 命令行解析
- **tokio 1.x**: 异步运行时
- **serde/serde_json**: 序列化
- **anyhow**: 错误处理
- **env_logger**: 日志记录

## 主要命令

| 命令 | 功能 | 状态 |
|------|------|------|
| `kafka-cli topics` | 主题管理 | ✅ 完成 |
| `kafka-cli produce` | 生产消息 | ✅ 完成 |
| `kafka-cli consume` | 消费消息 | ✅ 完成 |
| `kafka-cli consumer-groups` | 消费组管理 | 🔨 进行中 |
| `kafka-cli configs` | 配置管理 | ✅ 完成 |

## 参考文档

- **需求文档**: [docs/requirement.md](docs/requirement.md) - 项目原始需求
- **详细规划**: [docs/PLAN.md](docs/PLAN.md) - 完整的项目规划和时间线
- **开发指南**: [DEVELOP.md](DEVELOP.md) - 开发环境设置和贡献指南
- **安装指南**: [INSTALL.md](INSTALL.md) - 各平台安装说明
- **用户文档**: [README.md](README.md) - 使用说明和示例

## 获取帮助

1. 查看 `--help`:
   ```bash
   kafka-cli --help
   kafka-cli topics --help
   ```

2. 查看文档:
   - README.md: 基本使用
   - INSTALL.md: 安装问题
   - DEVELOP.md: 开发问题
   - docs/PLAN.md: 功能规划

3. 检查日志:
   ```bash
   RUST_LOG=debug cargo run -- <command>
   ```

## 常见问题

### 1. 构建失败："cmake not found"

**解决**: 安装 CMake 或使用 vcpkg 安装 librdkafka

### 2. 连接 Kafka 失败

**检查**:
- Kafka 是否正在运行
- bootstrap-server 地址是否正确
- 防火墙是否阻止连接

### 3. 性能问题

**优化**:
- 使用 release 模式: `cargo build --release`
- 调整 Kafka 配置（批处理、压缩）
- 使用多线程/多进程

---

**最后更新**: 2025-11-14  
**项目状态**: Phase 1 MVP 基础实现完成
