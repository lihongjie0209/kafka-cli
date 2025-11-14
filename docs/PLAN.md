# Kafka CLI 项目规划文档

## 项目概述

基于 Rust 开发的跨平台 Kafka 客户端命令行工具，目标是实现 Apache Kafka 官方 kafka-*.sh 脚本的所有核心功能。

**项目名称**: kafka-cli  
**开发语言**: Rust  
**初始版本**: 0.1.0  
**许可证**: [待定]

## 技术架构

### 核心技术栈

| 组件 | 库/框架 | 版本 | 用途 |
|-----|---------|------|-----|
| Kafka 客户端 | rdkafka | 0.38.0 | 提供 Kafka 生产者、消费者和管理功能 |
| 命令行解析 | clap | 4.x | 解析命令行参数，提供用户友好的 CLI 接口 |
| 异步运行时 | tokio | 1.x | 处理异步 I/O 操作，提高性能和响应速度 |
| 数据序列化 | serde + serde_json | 1.x | 处理 JSON 数据格式 |
| 日志记录 | log + env_logger | 最新 | 日志记录和调试 |
| 错误处理 | anyhow | 1.x | 统一错误处理 |
| 时间处理 | chrono | 0.4.x | 时间戳和日期处理 |

### 项目结构

```
kafka-cli/
├── src/
│   ├── main.rs              # 应用程序入口点
│   ├── cli/                 # CLI 定义模块
│   │   └── mod.rs          # 命令行参数定义
│   ├── commands/            # 命令实现模块
│   │   ├── mod.rs
│   │   ├── topics.rs       # 主题管理命令
│   │   ├── produce.rs      # 生产者命令
│   │   ├── consume.rs      # 消费者命令
│   │   ├── consumer_groups.rs  # 消费组管理
│   │   └── configs.rs      # 配置管理
│   ├── kafka/               # Kafka 客户端封装
│   │   ├── mod.rs
│   │   ├── admin.rs        # 管理客户端操作
│   │   ├── producer.rs     # 生产者封装
│   │   └── consumer.rs     # 消费者封装
│   ├── config/              # 配置管理
│   │   └── mod.rs          # 配置文件解析
│   └── utils/               # 工具函数
│       └── mod.rs
├── docs/
│   └── requirement.md      # 需求文档
├── Cargo.toml              # Rust 依赖配置
├── README.md               # 用户文档（英文）
├── INSTALL.md              # 安装指南
├── DEVELOP.md              # 开发指南
└── example.properties      # 配置文件示例
```

## 功能规划

### Apache Kafka 脚本分析

根据 Apache Kafka 官方仓库 (https://github.com/apache/kafka/tree/trunk/bin)，共有 40+ 个 kafka-*.sh 脚本。按照使用频率和重要性，划分为以下优先级：

### Phase 1: 核心功能 (MVP) - ✅ 已实现

**目标**: 实现最常用的基础功能，提供立即可用的工具  
**预计工期**: 4-6 周  
**状态**: 已完成基础实现

#### 1.1 主题管理 (kafka-topics.sh) - ✅ 已实现

**命令**: `kafka-cli topics`

**功能**:
- ✅ 列出所有主题
- ✅ 创建新主题（指定分区数、副本因子、配置）
- ✅ 描述主题（显示分区、副本、ISR 信息）
- ✅ 删除主题
- ✅ 修改主题配置

**示例**:
```bash
# 列出所有主题
kafka-cli topics --bootstrap-server localhost:9092 list

# 创建主题
kafka-cli topics --bootstrap-server localhost:9092 create \
  --topic my-topic \
  --partitions 3 \
  --replication-factor 2 \
  --config retention.ms=86400000

# 描述主题
kafka-cli topics --bootstrap-server localhost:9092 describe --topic my-topic

# 删除主题
kafka-cli topics --bootstrap-server localhost:9092 delete --topic my-topic
```

#### 1.2 控制台生产者 (kafka-console-producer.sh) - ✅ 已实现

**命令**: `kafka-cli produce`

**功能**:
- ✅ 从标准输入读取消息并发送到 Kafka
- ✅ 支持 key-value 分隔符（默认为 tab）
- ✅ 支持压缩类型（gzip, snappy, lz4, zstd）
- ✅ 支持自定义配置

**示例**:
```bash
# 基本使用
echo "key1	value1" | kafka-cli produce --bootstrap-server localhost:9092 --topic my-topic

# 使用压缩
kafka-cli produce --bootstrap-server localhost:9092 \
  --topic my-topic \
  --compression-type gzip
```

#### 1.3 控制台消费者 (kafka-console-consumer.sh) - ✅ 已实现

**命令**: `kafka-cli consume`

**功能**:
- ✅ 从 Kafka 主题消费消息并显示到控制台
- ✅ 支持从头开始消费 (--from-beginning)
- ✅ 支持消费组
- ✅ 支持限制消息数量
- ✅ 支持多种输出格式（默认、JSON）

**示例**:
```bash
# 基本消费
kafka-cli consume --bootstrap-server localhost:9092 --topic my-topic

# 从头开始消费
kafka-cli consume --bootstrap-server localhost:9092 \
  --topic my-topic \
  --from-beginning \
  --max-messages 100

# JSON 格式输出
kafka-cli consume --bootstrap-server localhost:9092 \
  --topic my-topic \
  --formatter json
```

#### 1.4 消费组管理 (kafka-consumer-groups.sh) - 🔨 部分实现

**命令**: `kafka-cli consumer-groups`

**功能**:
- ⏳ 列出所有消费组
- ⏳ 描述消费组（成员、分区分配、lag）
- ⏳ 删除消费组
- ⏳ 重置消费组偏移量

**说明**: rdkafka 库对消费组管理的支持有限，需要使用 Kafka Admin API 或其他方法实现。

#### 1.5 配置管理 (kafka-configs.sh) - ✅ 已实现

**命令**: `kafka-cli configs`

**功能**:
- ✅ 描述实体配置（主题、broker、客户端）
- ✅ 修改配置（添加、更新、删除）

**示例**:
```bash
# 查看主题配置
kafka-cli configs --bootstrap-server localhost:9092 describe \
  --entity-type topics \
  --entity-name my-topic

# 修改配置
kafka-cli configs --bootstrap-server localhost:9092 alter \
  --entity-type topics \
  --entity-name my-topic \
  --add-config retention.ms=86400000 \
  --delete-config max.message.bytes
```

### Phase 2: 运维工具 (4-5 周)

**目标**: 添加运维和管理功能  
**状态**: 未开始

#### 2.1 性能测试工具

| 命令 | 对应脚本 | 功能 | 优先级 |
|------|---------|------|--------|
| `kafka-cli perf producer` | kafka-producer-perf-test.sh | 生产者性能测试 | 高 |
| `kafka-cli perf consumer` | kafka-consumer-perf-test.sh | 消费者性能测试 | 高 |

#### 2.2 管理工具

| 命令 | 对应脚本 | 功能 | 优先级 |
|------|---------|------|--------|
| `kafka-cli acls` | kafka-acls.sh | ACL 管理（安全访问控制） | 高 |
| `kafka-cli reassign-partitions` | kafka-reassign-partitions.sh | 分区重新分配 | 高 |
| `kafka-cli log-dirs` | kafka-log-dirs.sh | 查询日志目录使用情况 | 中 |
| `kafka-cli get-offsets` | kafka-get-offsets.sh | 获取分区偏移量信息 | 中 |
| `kafka-cli delete-records` | kafka-delete-records.sh | 删除分区记录 | 中 |
| `kafka-cli leader-election` | kafka-leader-election.sh | 触发 leader 选举 | 中 |
| `kafka-cli broker-api-versions` | kafka-broker-api-versions.sh | 列出 broker 支持的 API 版本 | 低 |

### Phase 3: 高级功能 (3-4 周)

**目标**: 验证和高级管理功能  
**状态**: 未开始

#### 3.1 验证工具

| 命令 | 对应脚本 | 功能 |
|------|---------|------|
| `kafka-cli verify producer` | kafka-verifiable-producer.sh | 可验证的生产者 |
| `kafka-cli verify consumer` | kafka-verifiable-consumer.sh | 可验证的消费者 |
| `kafka-cli replica-verification` | kafka-replica-verification.sh | 副本一致性验证 |

#### 3.2 高级管理

| 命令 | 对应脚本 | 功能 |
|------|---------|------|
| `kafka-cli jmx` | kafka-jmx.sh | 查询 JMX 指标 |
| `kafka-cli dump-log` | kafka-dump-log.sh | 转储日志段内容 |
| `kafka-cli cluster` | kafka-cluster.sh | 集群操作 |

### Phase 4: 专用工具 (按需实现，每批 2-3 周)

**目标**: 实现特定场景的高级功能  
**状态**: 未开始

#### 4.1 KRaft 工具（Kafka 新架构）

| 命令 | 对应脚本 | 功能 |
|------|---------|------|
| `kafka-cli storage` | kafka-storage.sh | KRaft 存储工具 |
| `kafka-cli metadata quorum` | kafka-metadata-quorum.sh | 元数据仲裁管理 |
| `kafka-cli metadata shell` | kafka-metadata-shell.sh | 交互式元数据 shell |

#### 4.2 安全和特性管理

| 命令 | 对应脚本 | 功能 |
|------|---------|------|
| `kafka-cli delegation-tokens` | kafka-delegation-tokens.sh | 委托令牌管理 |
| `kafka-cli transactions` | kafka-transactions.sh | 事务管理 |
| `kafka-cli features` | kafka-features.sh | 特性管理 |
| `kafka-cli client-metrics` | kafka-client-metrics.sh | 客户端指标管理 |

#### 4.3 Share Consumer（新特性）

| 命令 | 对应脚本 | 功能 |
|------|---------|------|
| `kafka-cli share consume` | kafka-console-share-consumer.sh | Share consumer |
| `kafka-cli share groups` | kafka-share-groups.sh | Share group 管理 |
| `kafka-cli share perf` | kafka-share-consumer-perf-test.sh | Share consumer 性能测试 |

## 命令行规范

### 基本结构

```
kafka-cli [OPTIONS] <COMMAND> [SUBCOMMAND] [ARGS]
```

### 通用选项

所有命令都支持以下通用选项：

```bash
--bootstrap-server <SERVER>    # Kafka bootstrap 服务器，默认: localhost:9092
--command-config <FILE>         # 配置文件路径（properties 格式）
--property <KEY=VALUE>          # 额外的 Kafka 配置属性
```

### 帮助系统

```bash
kafka-cli --help                      # 显示主帮助
kafka-cli <command> --help            # 显示命令帮助
kafka-cli <command> <subcommand> --help  # 显示子命令帮助
```

### 配置文件格式

支持标准的 Java properties 格式：

```properties
# 连接配置
bootstrap.servers=localhost:9092

# 安全配置
security.protocol=SASL_SSL
sasl.mechanism=PLAIN
sasl.username=myuser
sasl.password=mypassword

# SSL 配置
ssl.ca.location=/path/to/ca-cert
ssl.certificate.location=/path/to/client-cert
ssl.key.location=/path/to/client-key

# 生产者配置
compression.type=gzip
acks=all

# 消费者配置
auto.offset.reset=earliest
enable.auto.commit=true
```

## 技术实现要点

### 1. 连接管理

**挑战**: 所有工具都需要连接 Kafka  
**解决方案**: 
- 实现 `config::build_client_config()` 函数
- 支持多种配置源（命令行、配置文件、环境变量）
- 配置优先级：命令行 > 配置文件 > 默认值

### 2. Admin 操作

**实现**: 使用 rdkafka 的 `AdminClient`  
**支持的操作**:
- 创建/删除主题
- 修改/查询配置
- 创建/删除 ACL（需要 Kafka 支持）
- 分区重新分配

### 3. 生产者实现

**实现**: 使用 rdkafka 的 `FutureProducer`  
**关键特性**:
- 消息 key/value 支持
- Header 支持
- 压缩（gzip, snappy, lz4, zstd）
- 分区策略
- 同步/异步模式
- 事务支持（Phase 4）

### 4. 消费者实现

**实现**: 使用 rdkafka 的 `StreamConsumer`  
**关键特性**:
- 订阅主题/分区
- 消费组管理
- 偏移量管理（commit, seek, reset）
- 消息格式化（JSON、plain text、自定义）
- 按时间戳过滤

### 5. 消息格式化

**默认格式**: `key:value`  
**JSON 格式**:
```json
{
  "topic": "my-topic",
  "partition": 0,
  "offset": 123,
  "timestamp": 1699999999000,
  "key": "key1",
  "value": "value1"
}
```

### 6. 错误处理

**策略**:
- 使用 `anyhow::Result` 统一错误类型
- 将 rdkafka 错误映射为用户友好的消息
- 提供重试逻辑（生产者）
- 退出码与原始脚本保持一致

### 7. 跨平台兼容性

**考虑因素**:
- 使用 `std::path::Path` 处理文件路径
- 正确处理信号（SIGINT, SIGTERM）
- 在 Windows、Linux、macOS 上测试
- 处理配置文件路径分隔符差异

## 测试策略

### 单元测试

```bash
cargo test
```

**覆盖范围**:
- 配置解析
- 消息格式化
- 命令行参数解析
- 工具函数

### 集成测试

**要求**: 运行中的 Kafka 集群  
**工具**: testcontainers-rs

```bash
cargo test -- --ignored
```

**覆盖范围**:
- 主题管理操作
- 生产和消费消息
- 消费组操作
- 配置管理

### 端到端测试

**方法**: 与原始 kafka-*.sh 脚本输出对比

```bash
# 启动 Kafka
docker run -d --name kafka -p 9092:9092 apache/kafka:latest

# 测试主题列表
./target/release/kafka-cli topics --bootstrap-server localhost:9092 list
kafka-topics.sh --bootstrap-server localhost:9092 --list

# 比较输出
```

### 性能测试

**基准测试**:
- 与 Java 工具对比吞吐量
- 内存使用情况
- CPU 使用情况
- 启动时间

## 工程化配置

### CI/CD

**平台**: GitHub Actions

**流程**:
1. 代码检查（`cargo fmt --check`, `cargo clippy`）
2. 单元测试（`cargo test`）
3. 集成测试（使用 Kafka 容器）
4. 跨平台构建（Windows, Linux, macOS）
5. 发布二进制文件

### 发布策略

**分发渠道**:
1. GitHub Releases（二进制文件）
2. crates.io（Rust 包）
3. Homebrew（macOS）
4. Chocolatey（Windows）
5. Docker Hub（容器镜像）

**版本规范**: 遵循 Semantic Versioning 2.0.0

## 项目时间线

### Phase 1: MVP（已完成）
- ✅ 项目初始化和依赖配置
- ✅ CLI 框架搭建
- ✅ 主题管理命令
- ✅ 生产者命令
- ✅ 消费者命令
- ✅ 配置管理命令
- 🔨 消费组管理（部分完成）

**已用时间**: 约 2 周（初始实现）

### Phase 2: 运维工具（待开始）
**预计时间**: 4-5 周
- 性能测试工具
- ACL 管理
- 分区重新分配
- 日志目录查询
- 其他管理工具

### Phase 3: 高级功能（待开始）
**预计时间**: 3-4 周
- 可验证的生产者/消费者
- 副本验证
- JMX 工具
- 高级管理功能

### Phase 4: 专用工具（按需）
**预计时间**: 6-9 周（分批实现）
- KRaft 工具
- 安全特性
- Share Consumer

**总预计时间**: 17-24 周（约 4-6 个月）

## 下一步工作

### 立即任务

1. **完善消费组管理**
   - 实现 list consumer groups
   - 实现 describe consumer groups
   - 实现 reset offsets

2. **改进错误处理**
   - 统一错误消息格式
   - 添加更友好的错误提示
   - 提供问题解决建议

3. **完善文档**
   - 添加更多使用示例
   - 创建故障排除指南
   - 添加性能调优建议

### 短期计划（1-2 周）

1. **构建系统优化**
   - 解决 Windows 构建问题
   - 提供预编译二进制文件
   - 设置 CI/CD 流程

2. **功能增强**
   - 添加 Shell 自动补全
   - 实现配置 profile 功能
   - 添加彩色输出选项

3. **测试完善**
   - 设置集成测试环境
   - 添加更多单元测试
   - 实现端到端测试

### 中期计划（1-2 个月）

1. **Phase 2 功能开发**
   - 性能测试工具
   - ACL 管理
   - 分区管理工具

2. **用户体验优化**
   - 交互式模式
   - 进度条显示
   - 更好的输出格式

3. **文档和社区**
   - 创建 Wiki 文档
   - 发布使用教程
   - 收集用户反馈

## 技术债务和待解决问题

### 已知限制

1. **消费组管理**
   - rdkafka 库对消费组操作支持有限
   - 需要实现自定义 Admin API 调用
   - 偏移量重置功能需要特殊处理

2. **Windows 构建**
   - 需要 CMake 或预安装的 librdkafka
   - 构建时间较长
   - 考虑提供预编译的静态链接版本

3. **JMX 支持**
   - Kafka JMX 指标需要 Java 环境
   - 可能需要调用外部工具或实现 JMX 协议

### 优化方向

1. **性能优化**
   - 批处理优化
   - 内存使用优化
   - 连接池管理

2. **功能增强**
   - Avro/Protobuf 序列化支持
   - Schema Registry 集成
   - 更多输出格式（CSV、XML）

3. **可用性提升**
   - 交互式配置向导
   - 自动发现 Kafka 集群
   - 健康检查工具

## 参考资源

- [Apache Kafka 官方文档](https://kafka.apache.org/documentation/)
- [rdkafka Rust 文档](https://docs.rs/rdkafka/)
- [Kafka Shell 脚本源码](https://github.com/apache/kafka/tree/trunk/bin)
- [Rust 异步编程书](https://rust-lang.github.io/async-book/)
- [clap 命令行解析](https://docs.rs/clap/)

## 贡献指南

欢迎贡献！请参考 [DEVELOP.md](DEVELOP.md) 了解开发设置和贡献流程。

### 贡献重点领域

1. **功能实现**: Phase 2-4 的命令实现
2. **测试**: 增加测试覆盖率
3. **文档**: 改进文档和示例
4. **Bug 修复**: 修复已知问题
5. **性能优化**: 提高工具性能

## 许可证

[待定 - 建议使用 Apache 2.0 或 MIT]

---

**文档版本**: 1.0  
**最后更新**: 2025-11-14  
**状态**: Phase 1 MVP 基本完成，待完善和测试
