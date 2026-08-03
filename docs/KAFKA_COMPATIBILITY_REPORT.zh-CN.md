# kafka-cli 与 Apache Kafka 原版工具功能对比报告

报告日期：2026-08-03

## 1. 结论摘要

本项目当前是一个可用的 Rust Kafka 管理与数据 CLI，但还不能称为 Apache Kafka 全部 Bash 工具的完整复刻。

- Apache Kafka 对比基准：`trunk`，版本 `4.4.0-SNAPSHOT`，提交 `4959a8de25422a64e8313d1fc666617120c746f8`。
- 本项目版本：`master`（报告随每个对齐提交持续更新）。
- Kafka 原版 `bin/` 目录有 44 个 `.sh` 入口；本项目识别其中 13 个兼容名称，入口覆盖率为 13/44（约 30%）。这个数字只表示入口名称，不表示选项或行为已完全兼容。
- 已覆盖的核心领域包括 Topic、普通 Consumer Group、动态配置、offset 查询、ACL、分区迁移、删除记录、leader election、log dirs、API versions、cluster、console producer 和 console consumer。
- Topic、offset 查询、删除记录、API versions 和 log dirs 的常用路径覆盖较完整；Consumer Group、配置、ACL、分区迁移和 console 工具是部分覆盖。
- Connect、Share Group、Streams Group、事务、delegation token、metadata quorum、storage、性能测试、验证工具等原版工具尚未实现。
- 当前网络栈以 `rdkafka`（底层为 librdkafka）为主；`rdkafka-sys` 用于 Rust 高层库未暴露的 Admin API。ACL create/describe/delete 已迁移到 librdkafka；少数其他管理路径仍使用 `krafka`，尚未完全统一。

## 2. 状态定义

| 状态 | 含义 |
|---|---|
| 已支持 | 核心动作和主要输入语义已实现，并有自动化测试覆盖 |
| 部分支持 | 命令可用，但缺少原版的一部分资源类型、选项、输出细节或高级模式 |
| 未支持 | 没有对应命令入口或实质实现 |
| 有意差异 | 本项目为了安全或机器可读输出而采用不同默认行为 |

## 3. 顶层脚本覆盖

### 3.1 已提供兼容入口的原版脚本

| Apache Kafka 脚本 | Rust 入口 | 状态 | 说明 |
|---|---|---|---|
| `kafka-topics.sh` | `kafka topics` | 已支持 | list、describe、create、alter、delete |
| `kafka-console-producer.sh` | `kafka produce` | 部分支持 | 行输入、sync/async、key、JSON、headers、partition 及主要 producer 调优参数；未复刻可插拔 reader |
| `kafka-console-consumer.sh` | `kafka consume` | 部分支持 | topic/include/group/partition/offset/from-beginning/max-messages/timeout/isolation；未复刻 Java formatter/deserializer |
| `kafka-consumer-groups.sh` | `kafka groups` | 部分支持 | list、批量 describe/delete/reset-offsets、delete-offsets，以及 reset CSV 导入导出 |
| `kafka-configs.sh` | `kafka configs` | 部分支持 | topic、broker、group，以及 librdkafka SCRAM user credential；缺少其他 entity type |
| `kafka-get-offsets.sh` | `kafka offsets` | 已支持 | librdkafka ListOffsets；topic 正则、partition 模式、earliest/latest/max-timestamp/timestamp、排除内部主题 |
| `kafka-acls.sh` | `kafka acls` | 部分支持 | librdkafka Admin API；list/add/remove、常见资源和 producer/consumer 快捷角色 |
| `kafka-reassign-partitions.sh` | `kafka reassign` | 部分支持 | generate/execute/verify/cancel/list；缺少 throttle 高级参数 |
| `kafka-delete-records.sh` | `kafka delete-records` | 已支持 | JSON 文件、预览、执行 |
| `kafka-leader-election.sh` | `kafka leader-election` | 已支持 | preferred/unclean、单分区、JSON 文件批量选择或全部分区 |
| `kafka-log-dirs.sh` | `kafka log-dirs` | 已支持 | broker/topic 过滤与目录、大小、lag 展示 |
| `kafka-broker-api-versions.sh` | `kafka api-versions` | 已支持 | 全 broker 或指定 broker 的 API version 范围 |
| `kafka-cluster.sh` | `kafka cluster` | 部分支持 | cluster ID、endpoints、API versions、unregister；缺少 fenced broker 细节选项 |

兼容入口可以通过软链接名称调用，支持带或不带 `.sh` 后缀。对原版使用 `--create`、`--describe` 等动作 flag 的部分脚本，会自动改写为 Rust 子命令。

### 3.2 尚未实现的原版脚本

| 类别 | 未支持脚本 |
|---|---|
| Kafka Connect | `connect-distributed.sh`、`connect-internal-topics.sh`、`connect-mirror-maker.sh`、`connect-plugin-path.sh`、`connect-standalone.sh` |
| 新消费组模型 | `kafka-console-share-consumer.sh`、`kafka-share-groups.sh`、`kafka-share-consumer-perf-test.sh`、`kafka-verifiable-share-consumer.sh` |
| Streams | `kafka-streams-application-reset.sh`、`kafka-streams-groups.sh` |
| 集群与元数据高级工具 | `kafka-client-metrics.sh`、`kafka-features.sh`、`kafka-metadata-quorum.sh`、`kafka-metadata-shell.sh`、`kafka-storage.sh` |
| 安全与事务 | `kafka-delegation-tokens.sh`、`kafka-transactions.sh` |
| 性能、校验与诊断 | `kafka-consumer-perf-test.sh`、`kafka-producer-perf-test.sh`、`kafka-e2e-latency.sh`、`kafka-replica-verification.sh`、`kafka-verifiable-consumer.sh`、`kafka-verifiable-producer.sh`、`kafka-dump-log.sh`、`trogdor.sh` |
| 服务进程与基础启动器 | `kafka-run-class.sh`、`kafka-server-start.sh`、`kafka-server-stop.sh`、`kafka-jmx.sh` |
| 其他 Group 工具 | `kafka-groups.sh` |

服务启动、JVM class runner、JMX 和 Trogdor 一类脚本不适合由客户端 CLI 等价替代；如果项目目标仅是 Kafka 客户端管理工具，可以明确将它们排除在范围之外。

## 4. 已覆盖命令的详细对比

### 4.1 Topics

已支持：

- list、describe、create、alter、delete。
- topic 名称和 Java 风格整串正则匹配。
- create 的 partitions、replication factor、手工 replica assignment、重复 `--config`、`--if-not-exists`。
- alter 增加 partition 数量、手工 assignment、`--if-exists`。
- delete 的正则选择和 `--if-exists`。
- describe 过滤：under-replicated、unavailable、under-min-ISR、at-min-ISR、topics-with-overrides、exclude-internal。
- describe 通过 librdkafka `DescribeTopics` 返回 topic UUID，支持按 `--topic-id` 选择。
- unavailable 判定会核对 leader 是否仍在 live broker 集合中。

缺少或有差异：

- 未支持 `--partition-size-limit-per-response`。
- 未指定 replication factor 时通过 librdkafka 的 `-1` sentinel 使用 broker 默认值。
- 原版废弃的 `--delete-config` 未提供。
- 表格列名和排版不是原版逐字符复制。

### 4.2 Console Producer

已支持：topic、acks（含原版 `--request-required-acks` 名称）、compression（含 `--compression-codec`）、parse-key、key separator、stdin 行输入、JSON 输入、指定 partition、headers、`--sync` 逐条等待和默认异步排队、`--command-property` 及旧名 `--producer-property`。原版 batch-size、message-send-max-retries、retry-backoff-ms、timeout/linger、request-timeout-ms、metadata-expiry-ms、max-memory-bytes、socket-buffer-size 均映射到对应 librdkafka 配置；command-property 保持最高优先级。

缺少：Java 自定义 line reader、reader config/property、已废弃的 max-partition-memory-bytes，以及 Java producer 特有而 librdkafka 没有直接等价项的 max-block-ms 专用语义。其他底层 producer 配置仍可通过 `--command-property key=value` 传入。

### 4.3 Console Consumer

已支持：topic 或整串 `--include` 正则、group、partition、offset、from-beginning、max-messages、`--timeout-ms` 空闲退出、read_committed/read_uncommitted isolation level、skip-message-on-error、JSON、print-key、key separator、`--command-property` 及旧名 `--consumer-property`。include 使用 librdkafka 的动态正则订阅（语法以 librdkafka/POSIX 能力为准）；isolation level 映射到 librdkafka，command-property 保持最高优先级。手工 partition 未指定 offset 时现在与原版一致默认为 latest，而不是 beginning。

缺少：formatter/formatter-config/formatter-property、key/value deserializer 和 systest events。这些选项依赖 Java 类加载或 Kafka 测试工具协议，不能由 librdkafka直接复用；其他底层 consumer 配置可通过 `--command-property` 传入。

### 4.4 Consumer Groups

已支持：

- list 使用 librdkafka `ListConsumerGroups` Admin API，支持裸 `--state`/`--type` 展示列，以及逗号分隔的 broker-side state/type 过滤。
- describe 默认 offsets 视图以及 `--state`、`--members`、`--offsets`；支持重复 `--group` 和 `--all-groups`，state/members 使用 librdkafka `DescribeConsumerGroups` Admin API。
- committed offset、log end offset、lag 和错误列。
- members 输出已解码的当前和 target topic-partition assignment；state 输出 group type、assignor、member count 与 coordinator。
- describe 支持原版 `--verbose`：offset 视图增加 librdkafka 返回的 committed leader epoch，members 视图增加 current/target assignment；非 verbose members 只显示 partition 数量。librdkafka 2.12 未暴露 consumer group/member epoch，因此这些 Kafka 4 新列不会伪造。
- delete group 支持重复 `--group` 和 `--all-groups`；delete-offsets 支持重复裸 topic 与 `topic:partition,partition`，单个 librdkafka 请求可携带跨 topic partition list；两者默认预览并通过 `--execute` 执行。
- reset offsets：支持重复 `--group`/`--topic`、`--all-groups`、`--all-topics`、原版 `topic:partition,partition` selector、`--dry-run`/`--execute`，以及 earliest、latest、absolute offset、shift-by、current、datetime、ISO-8601 duration；支持原版 `--export` 与 `--from-file` 无表头 CSV（单 group 三列、多 group 四列），导入校验选择范围、重复目标并按 log start/end 边界调整 offset；执行阶段使用 librdkafka `AlterConsumerGroupOffsets` Admin API。
- `validate-regex` 可在不提供 bootstrap-server 时独立校验正则；兼容脚本入口的原版 `--validate-regex` 会自动改写到该子命令，结果使用表格/JSON 输出。语法由 Rust regex/librdkafka 可用集合约束，不宣称覆盖所有 Java Pattern 扩展。

缺少或有差异：

- librdkafka 2.12 未暴露 Kafka consumer protocol 的 group epoch、member epoch 与 target assignment epoch；verbose 无法输出这些原版新列。

### 4.5 Configs

已支持：

- topic、broker、group 的 describe 和增量 add/delete config。
- describe 可省略 entity-name 枚举该类型的全部实体；默认只显示动态 override，原版 `--all` 显示继承、静态和默认配置。多实体输出包含 entity type/name 与 config source。
- 原版复数 entity type（`topics`、`brokers`、`groups`、`users`）以及兼容的单数别名。
- user SCRAM credential describe、upsert 和 delete，支持 SCRAM-SHA-256、SCRAM-SHA-512、iterations 与 password。
- SCRAM 预览只显示 mechanism 和 iterations，不回显 password。
- alter 支持原版 `--add-config-file` Java properties 文件，并与 `--add-config` 互斥；普通 config 预览统一使用表格/JSON/YAML，而非手工文本。
- 预览与 `--execute`。

缺少：普通 user quota、client、user/client 组合、IP、broker-logger、client-metrics、entity-default、bootstrap-controller。除 SCRAM credential 外，资源类型覆盖仍少于原版。broker default entity 必须使用空 ConfigResource name，但 librdkafka 2.12 的 `rd_kafka_ConfigResource_new` 在 name 长度为 0 时直接返回 NULL，因此不能通过该库表达。SCRAM upsert 依赖启用 OpenSSL 的 librdkafka；bundled 与 musl 构建均启用 vendored OpenSSL。

### 4.6 Get Offsets

已支持：topic 正则、`topic:partition`、闭区间与开放区间 partition 范围、多个 pattern、exclude-internal、earliest/`-2`、latest/`-1`、max-timestamp/`-3`、Unix 毫秒 timestamp。所有查询统一通过 librdkafka `ListOffsets` Admin API，结果包含 offset 与 broker 返回的 timestamp。

缺少：librdkafka 2.12 尚未提供 Kafka 新版本的 earliest-local/`-4`、latest-tiered/`-5`、earliest-pending-upload/`-6` OffsetSpec。输出采用统一表格/JSON envelope，而非原版 `topic:partition:offset` 文本格式。

### 4.7 ACLs

已支持：list/add/remove；Topic、Group、Cluster、Transactional ID；Literal/Prefixed/Any；allow/deny principal 与 host；常见 operations；producer、consumer、idempotent 快捷角色；预览与 `--execute`。Create、Describe 和 Delete 均通过 librdkafka Admin API 执行。

缺少或有差异：

- 未支持 Kafka 4.4 的 `--user-principal` 资源语义。
- librdkafka 2.12 的 ACL ResourceType 不包含 Delegation Token；使用 `--delegation-token` 时会明确返回不支持，而不会切换到另一套协议实现。
- 未支持 bootstrap-controller。
- 原版 remove 会交互确认或使用 `--force`；本项目采用更安全的显式 `--execute`，没有交互确认。
- 快捷角色当前只允许 allow principal/host，不允许 deny 组合。
- ACL FFI 使用窄范围 RAII 封装管理 Admin queue、options、event 和 native binding 生命周期。

### 4.8 Partition Reassignment

已支持：generate、execute、verify、cancel、list；topics-to-move 与 reassignment JSON；broker list；rack-aware/disable-rack-aware；log directory relocation；预览与 `--execute`。

缺少：`--additional`、`--throttle`、`--replica-alter-log-dirs-throttle`、`--preserve-throttles`、`--disallow-replication-factor-change`、bootstrap-controller。生成算法目标与原版一致，但不承诺在相同输入下产生逐字节相同 assignment。

### 4.9 Delete Records

offset JSON file、请求校验、执行和结果输出均已实现。本项目额外要求 `--execute` 才会修改数据；原版命令本身即执行，这是有意的安全差异。

### 4.10 Leader Election

已支持 preferred、unclean；topic+partition、原版 `--path-to-json-file` 批量 partition 输入或 all-topic-partitions；预览与 `--execute`。批量输入会校验空列表、负 partition 和重复 topic-partition，执行统一使用 librdkafka `ElectLeaders` Admin API。

差异：未提供旧 `--admin.config` 别名和 bootstrap-controller。原版执行命令直接产生变更；本项目要求 `--execute`，预览使用统一表格/JSON/YAML 输出。

### 4.11 Log Dirs

已支持 broker list、topic list、log directory、partition size、offset lag 和错误展示。原版的 `--describe` 动作在本项目中由 `kafka log-dirs` 直接表示。

### 4.12 Broker API Versions

支持读取 broker API key 的 min/max version，可查询所有 broker 或指定 broker。原版主要只有 bootstrap-server 与 command-config；本项目额外提供结构化 JSON 输出。

### 4.13 Cluster

已支持 cluster ID、broker endpoints、API versions、unregister broker。Cluster ID 和 broker endpoints 均通过 librdkafka `DescribeCluster` Admin API 获取；endpoint 输出包含 rack 与 controller 标记。

缺少或有差异：bootstrap-controller、list-endpoints 的 `--include-fenced-brokers`、原版短参数和废弃 config 别名。unregister 要求 `--execute`，避免误操作。

## 5. 全局行为差异

| 项目 | Apache Kafka 原版 | kafka-cli |
|---|---|---|
| 运行时 | JVM/Scala/Java | Rust 原生二进制 |
| Kafka 客户端 | Apache Java client | `rdkafka`/librdkafka 为主，少数路径为 `krafka` |
| 输出 | 各脚本分别定义纯文本 | 统一 `comfy-table` 表格或稳定 JSON envelope |
| 破坏性操作 | 不同工具行为不一致，部分交互确认 | 多数操作先预览，要求 `--execute` |
| 动作语法 | 常见 `--list`、`--describe` flag | 原生子命令；兼容入口会改写部分旧 flag |
| 配置文件 | Java properties | 支持 Java-compatible properties，并映射常见 librdkafka key |
| Java 插件 | formatter、reader、deserializer 可加载类 | 不支持加载 Java 类 |
| 超时 | 各工具分别定义 | 全局 `--timeout-ms`，部分命令有原版语义差异 |

JSON 输出是本项目扩展，不属于原版 Bash 输出兼容。表格输出已全部使用 `comfy-table`，没有继续手工拼接 table 文本。

## 6. 客户端实现架构

推荐并正在采用的依赖边界：

1. `rdkafka`：主要 API。它是 librdkafka 的安全 Rust 封装，并非另一套 Kafka 实现。
2. `rdkafka-sys`：只包装 `rdkafka` 尚未暴露的 librdkafka Admin API，例如 leader election、部分 group offset/config API。
3. `krafka`：当前仍用于 API versions、unregister、部分 reassignment/log-dir 协议路径。ACL 已完成迁移。为确保认证、重试、协议协商和错误行为一致，应继续迁移到 librdkafka/`rdkafka-sys`，最后评估删除该依赖。

不建议全量直接使用 `rdkafka-sys`：它与 `rdkafka` 使用同一个 librdkafka，但会把 C 指针、回调、队列和资源生命周期全部暴露为 `unsafe`，不会获得额外协议权威性。

## 7. 测试与验证现状

本地验证：

- `cargo fmt --check`：通过。
- `cargo clippy --all-targets --locked -- -D warnings`：通过。
- Rust 单元测试与普通 CLI 测试：53 个通过。
- Kafka 4.3.1 Docker 集成测试：通过，覆盖所有 13 个命令族的代表性路径。
- Kafka 3.6.2 真实进程集成测试：通过，覆盖协议和 Admin 兼容边界。
- GitHub Actions workflow 经 `actionlint` 校验通过。

测试仍有不足：

- 集成测试是单 broker，不能充分验证多 broker reassignment、rack awareness、ISR 变化和 failover。
- 未覆盖 TLS、mTLS、SASL/PLAIN、SCRAM、OAuth/OIDC 和 Kerberos 组合。
- 未覆盖真实 ARM64 机器运行；ARM64 当前只由 CI 交叉编译。
- 没有逐个原版选项的 golden test，也没有与 Java CLI 输出做逐命令差分测试。
- ACL 快捷角色和复杂 deny/host/prefix 组合仍需更完整矩阵。

## 8. CI 与发布目标

CI workflow 包含：

- fmt、Clippy、单元测试。
- bundled glibc release build。
- Kafka 4.3.1 集成测试。
- Kafka 3.6.2 集成测试。
- `x86_64-unknown-linux-musl` 静态构建及 artifact。
- `aarch64-unknown-linux-musl` 静态交叉构建及 artifact。

musl 构建只在 CI 内进行，使用 Rust 1.88、Zig 和 `cargo-zigbuild`。x86_64 musl 二进制面向 CentOS 7 等旧 glibc 环境时不依赖目标机器 glibc；ARM64 musl artifact 用于 ARM64 Linux。最终兼容性仍应在对应架构机器或容器中执行 smoke test，而不能只以 `file` 输出判断。

## 9. 建议的后续优先级

### P0：统一客户端与保证现有功能可靠

1. 将 API versions、unregister、reassignment/log-dir 等剩余 `krafka` 路径迁移至 `rdkafka` 或窄范围 `rdkafka-sys` RAII 封装。
2. 增加 TLS/SASL 集成测试。
3. 增加多 broker Kafka 4 集成环境，验证 reassignment、leader election、ISR 和 rack-aware 分配。
4. 在 CI 对两个 musl artifact 执行 `--help`/`--version` smoke test；ARM64 使用 QEMU 或原生 ARM runner。

### P1：补齐已支持脚本的主要差距

1. Configs 增加 user、client、IP、broker-logger、client-metrics 和 default entity。
2. Reassignment 增加 throttle、preserve-throttles、additional 等参数。
3. 在 librdkafka 增加相应 OffsetSpec 后，为 Get Offsets 增加 earliest-local、latest-tiered、earliest-pending-upload。
4. Topics 增加 `partition-size-limit-per-response`（需要 librdkafka 暴露对应请求选项）。

### P2：扩大原版工具覆盖面

优先考虑 `kafka-features.sh`、`kafka-metadata-quorum.sh`、`kafka-transactions.sh`、`kafka-delegation-tokens.sh` 和 `kafka-client-metrics.sh`。Connect、server start/stop、run-class、JMX 等 JVM 运行工具建议明确声明不在项目范围内，而不是做表面兼容。

## 10. 最终评估

当前项目适合作为轻量、可静态分发的 Kafka 日常管理 CLI，尤其适用于 Topic、offset、基础 Consumer Group、ACL、记录删除和集群信息查询。它已经具备跨 Kafka 3.6/4.3 的实测基础，但对于“替换 Kafka 发行包全部 Bash 脚本”这一目标仍不完整。

在对外发布时，建议使用“兼容 13 个常用 Kafka CLI 入口的 Rust 工具”表述，不应使用“100% 兼容 Apache Kafka CLI”。完成 P0 和 P1 后，才适合将已覆盖的 13 个脚本声明为主要功能兼容。

## 11. librdkafka 对齐变更记录

| 日期 | 功能 | 结果 | 验证 |
|---|---|---|---|
| 2026-08-03 | ACL Create/Describe/Delete | 从手写 Kafka 协议迁移到 librdkafka Admin FFI；不支持的 Delegation Token 资源改为明确报错 | Clippy、48 个普通测试、Kafka 3.6.2 与 Kafka 4.3.1 集成测试通过 |
| 2026-08-03 | User SCRAM credential | 新增 `configs` users entity 的 SCRAM-SHA-256/512 describe、upsert、delete；CI 集成测试改用 bundled OpenSSL librdkafka | Clippy、50 个普通测试、bundled Kafka 3.6.2 与 Kafka 4.3.1 集成测试通过 |
| 2026-08-03 | Cluster endpoints | `cluster list-endpoints` 从独立协议客户端迁移到 librdkafka metadata API | 既有 Kafka 3.6.2 与 Kafka 4.3.1 cluster 集成覆盖 |
| 2026-08-03 | Topic ID describe | 使用 librdkafka `DescribeTopics` 获取 topic UUID；describe 新增 `--topic-id`，表格和 JSON 输出携带 topic ID | Clippy、50 个普通测试、bundled Kafka 3.6.2 与 Kafka 4.3.1 名称/ID 闭环集成测试通过 |
| 2026-08-03 | Get Offsets ListOffsets | earliest、latest、timestamp 全部迁移至 librdkafka `ListOffsets`，新增 max-timestamp 及 `-1/-2/-3` 原版别名，输出 timestamp | Clippy、52 个普通测试、bundled Kafka 3.6.2 与 Kafka 4.3.1 集成测试通过 |
| 2026-08-03 | Consumer Group list | 从旧同步 group-list API 迁移到 librdkafka `ListConsumerGroups` Admin API，新增 Kafka 原版 `--state` 与 `--type` 可选过滤 | Clippy、53 个普通测试、bundled Kafka 3.6.2 与 Kafka 4.3.1 集成测试通过 |
| 2026-08-03 | Consumer Group describe | state/members 从旧 group-list API 迁移到 librdkafka `DescribeConsumerGroups`；解码 current/target member assignment，并输出 type、assignor、coordinator | Clippy、53 个普通测试、bundled Kafka 3.6.2 与 Kafka 4.3.1 state/members 集成测试通过 |
| 2026-08-03 | Consumer Group reset write | reset-offsets 从 consumer synchronous commit 迁移到 librdkafka `AlterConsumerGroupOffsets` Admin API，保留安全预览和所有 reset target 算法 | Clippy、53 个普通测试、bundled Kafka 3.6.2 与 Kafka 4.3.1 reset/describe 闭环集成测试通过 |
| 2026-08-03 | Describe Cluster | cluster ID 与 endpoint 查询统一迁移到 librdkafka `DescribeCluster` Admin API，新增 rack 与 controller 输出 | Clippy、53 个普通测试、bundled Kafka 3.6.2 与 Kafka 4.3.1 cluster 集成测试通过 |
| 2026-08-03 | Topic default replication factor | create 未指定 `--replication-factor` 时使用 librdkafka `-1` sentinel，交由 broker 的 default.replication.factor 决定 | Topic create 单元路径及 Kafka 3.6.2/4.3.1 集成测试 |
| 2026-08-03 | Consumer Group 批量目标 | describe 的 offsets/state/members 视图及 delete 支持重复 `--group` 与 `--all-groups`；all-groups 通过 librdkafka `ListConsumerGroups` 解析目标 | Clippy、53 个普通测试、bundled Kafka 4.3.1 批量 describe/delete 集成测试通过 |
| 2026-08-03 | Consumer Group 批量 reset | reset-offsets 支持重复 group/topic、all-groups、all-topics 和显式 dry-run；预览改为统一表格/JSON/YAML 输出，执行仍使用 librdkafka `AlterConsumerGroupOffsets` | Clippy、53 个普通测试、bundled Kafka 4.3.1 all-groups/all-topics 集成测试通过 |
| 2026-08-03 | Leader Election JSON 批量目标 | 新增原版 `--path-to-json-file` 格式，校验空列表、非法/重复 partition；librdkafka `ElectLeaders` FFI 从单目标扩展为 partition list，预览改为统一结构化输出 | Clippy、54 个普通测试、bundled Kafka 4.3.1 单目标/批量目标集成测试通过 |
| 2026-08-03 | Consumer Group reset CSV | 新增原版 `--export`/`--from-file` 无表头 CSV；支持单 group 三列与多 group 四列格式、CSV 转义、重复/选择范围校验和 offset 边界调整，执行复用 librdkafka `AlterConsumerGroupOffsets` | Clippy、55 个普通测试、bundled Kafka 4.3.1 export→import→execute 集成闭环通过 |
| 2026-08-03 | Console Producer 主要参数 | 新增 sync/默认 async 发送模式，并将 batch、retries/backoff、linger、request timeout、metadata expiry、buffer memory、socket buffer 等原版参数映射到 librdkafka；补充原版 acks/compression 参数名 | Clippy、56 个普通测试、bundled Kafka 4.3.1 sync 调优参数与默认 async 发送/消费闭环通过 |
| 2026-08-03 | Console Consumer librdkafka 选项 | 新增 include 正则订阅、空闲 timeout、isolation level 和 skip-message-on-error；修正手工 partition 无 offset 时默认为 latest，command-property 保持最高优先级 | Clippy、57 个普通测试、bundled Kafka 4.3.1 include/read_committed/timeout 参数消费闭环通过 |
| 2026-08-03 | Configs add-config-file | 新增原版 Java properties `--add-config-file`，与 add-config 互斥并复用 librdkafka IncrementalAlterConfigs；普通配置变更预览改为统一表格/JSON/YAML | Clippy、58 个普通测试、bundled Kafka 4.3.1 properties 文件 add/delete 闭环通过 |
| 2026-08-03 | Configs 全实体与 `--all` | describe 的 entity-name 改为可选，topic/broker/group 分别通过 librdkafka metadata/ListConsumerGroups 枚举；默认过滤为动态配置，`--all` 返回继承/静态/默认配置并输出 entity/source | Clippy、59 个普通测试、bundled Kafka 4.3.1 全 topic 枚举及 all config 集成测试通过 |
| 2026-08-03 | Consumer Group validate-regex | 新增无需 broker 的结构化正则校验子命令，并为 kafka-consumer-groups 兼容入口改写原版 `--validate-regex` | Clippy、61 个普通测试（含无 bootstrap CLI 测试）通过 |
| 2026-08-03 | Consumer Group verbose | offset verbose 输出 librdkafka committed leader epoch；members verbose 输出 current/target assignment，普通视图只显示 partition 数量；明确 group/member epoch 未由 librdkafka 2.12 暴露 | Clippy、61 个普通测试、bundled Kafka 4.3.1 verbose offsets/members 集成测试通过 |
| 2026-08-03 | Consumer Group reset partition selector | reset-offsets 新增原版 `topic:partition,partition` 选择语法，校验负数、重复和不存在 partition；规划及执行继续使用 librdkafka metadata/ListOffsets/AlterConsumerGroupOffsets | Clippy、62 个普通测试、bundled Kafka 4.3.1 单 partition JSON 规划集成测试通过 |
| 2026-08-03 | Consumer Group delete-offsets 批量 topic | delete-offsets 支持重复 topic 与 `topic:partition,partition`，校验不存在 partition；librdkafka DeleteConsumerGroupOffsets FFI 扩展为跨 topic partition list，预览改用统一表格/JSON/YAML | Clippy、62 个普通测试、bundled Kafka 4.3.1 跨 topic 删除集成测试通过 |
