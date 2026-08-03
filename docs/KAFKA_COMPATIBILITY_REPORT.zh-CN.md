# kafka-cli 与 Apache Kafka 原版工具功能对比报告

报告日期：2026-08-04

## 1. 结论摘要

本项目当前是一个可用的 Rust Kafka 管理与数据 CLI，但还不能称为 Apache Kafka 全部 Bash 工具的完整复刻。

- Apache Kafka 对比基准：`trunk`，版本 `4.4.0-SNAPSHOT`，提交 `4959a8de25422a64e8313d1fc666617120c746f8`。
- 本项目基准：`master`，提交 `1eef9ae1dfe7e528d26f5553b78f026111071b6b`。
- Kafka 原版 `bin/` 目录有 44 个 `.sh` 入口；本项目识别其中 13 个兼容名称，入口覆盖率为 13/44（29.5%）。这个数字只表示入口名称，不表示选项或行为已完全兼容。
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

### 2.1 审计方法与统计口径

本报告不是根据 README 或命令名称推断能力，而是同时核对以下证据：

1. Apache Kafka 基准提交的 `bin/*.sh`，以及脚本实际转发到的 Java `*CommandOptions`/`OptionSpec` 定义。
2. 本项目 `src/cli.rs` 的 clap 命令树、`src/commands.rs` 的执行路径、`src/ffi.rs` 的 librdkafka 调用。
3. `tests/cli.rs`、Kafka 3.6.2/4.3.1 集成测试与 GitHub Actions 结果。

“入口覆盖”只统计可识别的脚本名称；“动作覆盖”统计 list/create/alter 等一级动作；“选项覆盖”必须同时具备解析和有效执行语义。仅能解析、没有后端效果的参数不算支持。本报告不以输出逐字符一致为目标，表格/JSON 属于本项目自己的输出契约。

### 2.2 覆盖度仪表盘

| 维度 | 结果 | 解读 |
|---|---:|---|
| Kafka `.sh` 入口 | 13 / 44（29.5%） | 31 个入口未实现；其中部分是 JVM 服务/测试工具，不宜由本 CLI 替代 |
| 已覆盖入口的一级动作 | 31 / 31 | 仅表示这 13 个入口的 list/create/alter 等一级动作存在真实执行路径；不代表动作内参数、Java 插件或输出逐字符兼容；本项目另扩展 cluster api-versions |
| librdkafka 2.12 Admin operation | 21 / 21 个应调用操作 | 22 个实际枚举中，旧 `AlterConfigs` 被 `IncrementalAlterConfigs` 替代 |
| 普通自动化测试 | 98 个通过 | 91 个 library unit tests + 7 个 CLI tests；两个真实 Kafka 测试默认 ignored，由 CI 运行 |
| 已验证 broker | Kafka 3.6.2、Kafka 4.3.1 | 当前基准在两者全绿；均为单 broker 代表性路径，不等于完整兼容矩阵 |
| 静态发布目标 | glibc、x86_64 musl、aarch64 musl | musl 只在 CI 构建；ARM64 当前是交叉编译验证 |

原版动作数 31 的构成：Topics 5、Consumer Groups 6、Configs 2、ACLs 3、Reassignment 5、Cluster 3，其余 7 个入口各 1。Console producer/consumer 属于单动作数据命令；本项目额外把 API versions 也放入 cluster 子命令。

## 3. 顶层脚本覆盖

### 3.1 已提供兼容入口的原版脚本

| Apache Kafka 脚本 | Rust 入口 | 状态 | 说明 |
|---|---|---|---|
| `kafka-topics.sh` | `kafka topics` | 已支持 | list、describe、create、alter、delete |
| `kafka-console-producer.sh` | `kafka produce` | 部分支持 | 行输入、sync/async、key、JSON、headers、partition、reader config 及主要 producer 调优参数；未复刻可插拔 reader class |
| `kafka-console-consumer.sh` | `kafka consume` | 部分支持 | topic/include/group/partition/offset/from-beginning/max-messages/timeout/isolation、formatter config 和 StringDeserializer；未复刻任意 Java formatter/deserializer class 加载 |
| `kafka-consumer-groups.sh` | `kafka groups` | 部分支持 | list、批量 describe/delete/reset-offsets、delete-offsets，以及 reset CSV 导入导出 |
| `kafka-configs.sh` | `kafka configs` | 部分支持 | topic、broker（含 default）、group、SCRAM、client quota、broker logger、client metrics；缺 bootstrap-controller |
| `kafka-get-offsets.sh` | `kafka offsets` | 已支持 | librdkafka ListOffsets；topic 正则、partition 模式、earliest/latest/max-timestamp/timestamp、排除内部主题 |
| `kafka-acls.sh` | `kafka acls` | 部分支持 | librdkafka Admin API；list/add/remove、常见资源和 producer/consumer 快捷角色 |
| `kafka-reassign-partitions.sh` | `kafka reassign` | 部分支持 | generate/execute/verify/cancel/list；已支持 execute 限流/安全参数及 verify/cancel throttle 生命周期；缺 controller 模式 |
| `kafka-delete-records.sh` | `kafka delete-records` | 已支持 | JSON 文件；原生入口支持预览，兼容脚本按原版立即执行 |
| `kafka-leader-election.sh` | `kafka leader-election` | 已支持 | preferred/unclean、单分区、JSON 文件批量选择或全部分区；兼容脚本按原版立即执行 |
| `kafka-log-dirs.sh` | `kafka log-dirs` | 已支持 | broker/topic 过滤与目录、大小、lag 展示 |
| `kafka-broker-api-versions.sh` | `kafka api-versions` | 已支持 | 全 broker 或指定 broker 的 API version 范围 |
| `kafka-cluster.sh` | `kafka cluster` | 部分支持 | cluster ID、endpoints（含 fenced broker）、API versions、unregister；缺 bootstrap-controller |

兼容入口可以通过软链接名称调用，支持带或不带 `.sh` 后缀。对原版使用 `--create`、`--describe` 等动作 flag 的部分脚本，会自动改写为 Rust 子命令。原版 mutation 动作会同时保留“立即执行”语义，不会被静默降级为预览；`kafka-cluster.sh` 接受原版 `cluster-id` 名称及 `-b/-c/-i` 短参数，同时保留 `id` 作为原生别名。

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

已支持：topic、acks（含原版 `--request-required-acks` 名称）、compression（含可省略值并默认 gzip 的 `--compression-codec`）、stdin 行输入、JSON 输入、指定 partition、headers、`--sync` 逐条等待和默认异步排队、`--command-property` 及旧名 `--producer-property`，兼容脚本也接受废弃的 `--producer.config`。默认 `LineMessageReader` 的 `--reader-config` Java properties 文件、规范 `--reader-property` 及旧名 `--property` 已支持 parse.key、key.separator、parse.headers、headers.delimiter、headers.separator、headers.key.separator、ignore.error、null.marker，并保留 header 顺序和重复 key；命令行 property 覆盖文件值。原版 batch-size、覆盖它的废弃 max-partition-memory-bytes、message-send-max-retries、retry-backoff-ms、timeout/linger、request-timeout-ms、metadata-expiry-ms、max-block-ms、max-memory-bytes、socket-buffer-size 均已实现；metadata-expiry-ms 精确映射 `metadata.max.age.ms`，max-block-ms 控制本地队列满时的等待上限。

Producer 配置遵循原版三层优先级：显式 CLI 选项覆盖 `--command-property`/配置文件，property 覆盖脚本默认值。未配置时对齐原版的 acks `-1`、batch size `16384`、retries `3`、retry backoff `100ms`、linger `1000ms`、request timeout `1500ms`、metadata max age `300000ms`、max block `60000ms`、buffer memory `32MiB`、socket send buffer `102400` 和 client id `console-producer`。Java producer property `buffer.memory`、`send.buffer.bytes`、`max.block.ms` 会分别转换为 librdkafka 队列容量、socket buffer 和本地排队等待语义，不会作为无效 librdkafka 配置透传。

原版 `--line-reader org.apache.kafka.tools.LineMessageReader` 可显式传入；其他 Java 自定义 line reader class 会明确返回 unsupported，因为原生二进制不能加载 JVM 插件。其他底层 producer 配置仍可通过 `--command-property key=value` 传入。

### 4.3 Console Consumer

已支持：topic 或整串 `--include` 正则、group、partition、offset、from-beginning、max-messages、`--timeout-ms` 空闲退出、read_committed/read_uncommitted isolation level、skip-message-on-error、JSON、print-key、key separator、`--command-property` 及旧名 `--consumer-property`，兼容脚本也接受废弃的 `--consumer.config`。默认 `DefaultMessageFormatter` 的 `--formatter-config` Java properties 文件、规范 `--formatter-property` 及旧名 `--property` 支持 print.timestamp、print.partition、print.offset、print.delivery、print.epoch、print.headers、print.key、print.value、key.separator、line.separator、headers.separator、null.literal；命令行 property 覆盖文件值，并直接写出消息原始 bytes。include 使用 librdkafka 的动态正则订阅（语法以 librdkafka/POSIX 能力为准）。未显式指定 group 时会生成 `console-consumer-*` 临时 group，并在用户没有配置 `enable.auto.commit` 时默认关闭自动提交；显式 group、配置文件 `group.id` 与命令行 property 的值必须一致。手工 partition 与 group 互斥；`--offset` 必须配合 partition，接受 `earliest`、`latest` 或非负整数，未指定时与原版一致使用 latest。`--from-beginning` 与显式 offset 互斥，并拒绝冲突的 `auto.offset.reset`；CLI isolation level 覆盖 properties，未提供 CLI 值时保留 property 或使用 read_uncommitted 默认值。

原版 `--formatter org.apache.kafka.tools.consumer.DefaultMessageFormatter` 可显式传入。`--key-deserializer`、`--value-deserializer` 以及 formatter property 中的 key/value/headers deserializer 已支持 Kafka `StringDeserializer`，包括 formatter property 覆盖 CLI class、UTF-8/UTF8 encoding 校验和非法 UTF-8 替换字符语义。其他自定义 Java formatter/deserializer class 和 systest events 不支持，并会明确返回 unsupported，而不是静默忽略；delivery/epoch 在 librdkafka 未暴露相应记录字段时输出原版的 `NOT_PRESENT`。其他底层 consumer 配置可通过 `--command-property` 传入。

### 4.4 Consumer Groups

已支持：

- list 使用 librdkafka `ListConsumerGroups` Admin API，支持裸 `--state`/`--type` 展示列，以及逗号分隔的 broker-side state/type 过滤。
- describe 默认 offsets 视图以及 `--state`、`--members`、`--offsets`；支持重复 `--group` 和 `--all-groups`，state/members 使用 librdkafka `DescribeConsumerGroups` Admin API。
- committed offset、log end offset、lag 和错误列。
- members 输出已解码的当前和 target topic-partition assignment；state 输出 group type、assignor、member count 与 coordinator。
- describe 支持原版 `--verbose`：offset 视图增加 librdkafka 返回的 committed leader epoch，members 视图增加 current/target assignment；非 verbose members 只显示 partition 数量。librdkafka 2.12 未暴露 consumer group/member epoch，因此这些 Kafka 4 新列不会伪造。
- delete group 支持重复 `--group` 和 `--all-groups`；delete-offsets 支持重复裸 topic 与 `topic:partition,partition`，单个 librdkafka 请求可携带跨 topic partition list；原生子命令默认预览并通过 `--execute` 执行，兼容脚本的原版 `--delete`/`--delete-offsets` 动作立即执行。
- reset offsets：支持重复 `--group`/`--topic`、`--all-groups`、`--all-topics`、原版 `topic:partition,partition` selector、`--dry-run`/`--execute`，以及 earliest、latest、absolute offset、shift-by、current、datetime、ISO-8601 duration；支持原版 `--export` 与 `--from-file` 无表头 CSV（单 group 三列、多 group 四列），导入校验选择范围、重复目标并按 log start/end 边界调整 offset；执行阶段使用 librdkafka `AlterConsumerGroupOffsets` Admin API。
- `validate-regex` 可在不提供 bootstrap-server 时独立校验正则；兼容脚本入口的原版 `--validate-regex` 会自动改写到该子命令，结果使用表格/JSON 输出。语法由 Rust regex/librdkafka 可用集合约束，不宣称覆盖所有 Java Pattern 扩展。

缺少或有差异：

- librdkafka 2.12 未暴露 Kafka consumer protocol 的 group epoch、member epoch 与 target assignment epoch；verbose 无法输出这些原版新列。

### 4.5 Configs

已支持：

- topic、broker、group 的 describe 和增量 add/delete config；broker 支持命名实体和 `--entity-default` 默认实体。
- describe 可省略 entity-name 枚举该类型的全部实体；默认只显示动态 override，原版 `--all` 显示继承、静态和默认配置。多实体输出包含 entity type/name 与 config source。
- 原版复数 entity type（`topics`、`brokers`、`groups`、`users`）以及兼容的单数别名。
- user SCRAM credential describe、upsert 和 delete，支持 SCRAM-SHA-256、SCRAM-SHA-512、iterations 与 password。
- user、client、IP quota 的 describe/alter/delete；支持重复 entity-type/entity-name 的 user+client 复合实体，以及 users/clients/ips 的 default entity。quota 值按 Kafka Double 语义解析，删除前会与现有配置核对。
- broker-logger 与 client-metrics 的 describe/alter/delete；broker logger 请求路由到指定 broker，client metrics 支持省略名称枚举全部 subscription，并把写请求路由到 controller。
- SCRAM 预览只显示 mechanism 和 iterations，不回显 password。
- alter 支持原版 `--add-config-file` Java properties 文件，并与 `--add-config` 互斥；普通 config 预览统一使用表格/JSON，而非手工文本。
- 原生子命令支持预览与 `--execute`；兼容脚本的原版 `--alter` 动作立即执行。

缺少：bootstrap-controller。Client quotas 使用 Kafka DescribeClientQuotas/AlterClientQuotas API 48/49；broker-logger/client-metrics 与 broker default entity 使用 DescribeConfigs/IncrementalAlterConfigs 32/44，client-metrics 枚举还使用 ListConfigResources 74。librdkafka 2.12 没有 quota 和高级配置资源的公开 C API；broker default entity 还必须使用空 ConfigResource name，而 `rd_kafka_ConfigResource_new` 在 name 长度为 0 时返回 NULL。因此这些路径由项目协议客户端完成版本协商、目标 broker/controller 路由和逐资源错误处理。SCRAM upsert 依赖启用 OpenSSL 的 librdkafka；bundled 与 musl 构建均启用 vendored OpenSSL。

### 4.6 Get Offsets

已支持：topic 正则、`topic:partition`、闭区间与开放区间 partition 范围、多个 pattern、exclude-internal、earliest/`-2`、latest/`-1`、max-timestamp/`-3`、Unix 毫秒 timestamp。所有查询统一通过 librdkafka `ListOffsets` Admin API，结果包含 offset 与 broker 返回的 timestamp。

缺少：librdkafka 2.12 尚未提供 Kafka 新版本的 earliest-local/`-4`、latest-tiered/`-5`、earliest-pending-upload/`-6` OffsetSpec。输出采用统一表格/JSON envelope，而非原版 `topic:partition:offset` 文本格式。

### 4.7 ACLs

已支持：list/add/remove；Topic、Group、Cluster、Transactional ID；Literal/Prefixed/Any；allow/deny principal 与 host；常见 operations；producer、consumer、idempotent 快捷角色。原生子命令使用预览与 `--execute`；兼容脚本 `--add` 立即执行，`--remove --force` 接受原版 force 语义。Create、Describe 和 Delete 均通过 librdkafka Admin API 执行。

缺少或有差异：

- 未支持 Kafka 4.4 的 `--user-principal` 资源语义。
- librdkafka 2.12 的 ACL ResourceType 不包含 Delegation Token；使用 `--delegation-token` 时会明确返回不支持，而不会切换到另一套协议实现。
- 未支持 bootstrap-controller。
- 原版 remove 会交互确认或使用 `--force`；本项目不做交互确认：原生形式使用 `--execute`，兼容入口接受 `--force`，未提供 force 时保持预览。
- 快捷角色当前只允许 allow principal/host，不允许 deny 组合。
- ACL FFI 使用窄范围 RAII 封装管理 Admin queue、options、event 和 native binding 生命周期。

### 4.8 Partition Reassignment

已支持：generate、execute、verify、cancel、list；topics-to-move 与 reassignment JSON；broker list；rack-aware/disable-rack-aware；log directory relocation。原生子命令提供预览与 `--execute`，兼容脚本的原版 `--execute`/`--cancel` 动作立即执行。execute 支持原版 `--additional` 的全局活动迁移保护、`--disallow-replication-factor-change` 的逐分区复制因子校验、`--throttle` 的 topic/broker leader/follower replication throttle，以及 `--replica-alter-log-dirs-throttle` 的 broker log-dir throttle。throttle 通过 librdkafka `IncrementalAlterConfigs` 写入，并把已有迁移和新增计划合并计算 source/destination replicas。verify 在全局 partition reassignment 与目标 log-dir move 均结束后自动清理 throttle；cancel 成功后也会清理；两者均支持 `--preserve-throttles` 跳过清理。

缺少：bootstrap-controller。生成算法目标与原版一致，但不承诺在相同输入下产生逐字节相同 assignment。

### 4.9 Delete Records

offset JSON file、请求校验、执行和结果输出均已实现。原生 `kafka delete-records` 先预览并要求 `--execute`；`kafka-delete-records.sh` 兼容入口会自动进入执行路径，与原版命令本身即执行的语义一致。

### 4.10 Leader Election

已支持 preferred、unclean；topic+partition、原版 `--path-to-json-file` 批量 partition 输入或 all-topic-partitions；预览与 `--execute`。批量输入会校验空列表、负 partition 和重复 topic-partition，执行统一使用 librdkafka `ElectLeaders` Admin API。

差异：未提供 bootstrap-controller。兼容入口接受已废弃的 `--admin.config` 并映射到 `--command-config`；原生 `kafka leader-election` 要求 `--execute`，预览使用统一表格/JSON 输出；`kafka-leader-election.sh` 兼容入口直接产生变更，与原版一致。

### 4.11 Log Dirs

已支持 broker list、topic list、log directory、partition size、offset lag 和错误展示。原生形式可直接运行 `kafka log-dirs`；兼容入口也接受原版必需的 `kafka-log-dirs.sh --describe` 动作 flag。

### 4.12 Broker API Versions

支持读取 broker API key 的 min/max version，可查询所有 broker 或指定 broker。原版主要只有 bootstrap-server 与 command-config；本项目额外提供结构化 JSON 输出。

### 4.13 Cluster

已支持 cluster ID、broker endpoints、API versions、unregister broker。Cluster ID 和默认 broker endpoints 通过 librdkafka `DescribeCluster` Admin API 获取；`list-endpoints --include-fenced-brokers` 协商 Kafka DescribeCluster v2，输出原版兼容的 STATE 与 ENDPOINT_TYPE 列，并在旧 broker 不支持 v2 时明确报错。

缺少或有差异：bootstrap-controller。`cluster-id`、`-b/--bootstrap-server`、`-c/--command-config`、废弃的 `--config` 别名、unregister 的 `-i/--id` 已与原版对齐。原生 unregister 仍要求 `--execute`；`kafka-cluster.sh unregister` 兼容入口按原版立即执行。

## 5. 全局行为差异

| 项目 | Apache Kafka 原版 | kafka-cli |
|---|---|---|
| 运行时 | JVM/Scala/Java | Rust 原生二进制 |
| Kafka 客户端 | Apache Java client | `rdkafka`/librdkafka 为主，少数路径为 `krafka` |
| 输出 | 各脚本分别定义纯文本 | 统一 `comfy-table` 表格或稳定 JSON envelope |
| 破坏性操作 | 不同工具行为不一致，部分交互确认 | 原生子命令多数先预览并要求 `--execute`；兼容入口保留原版立即执行动作，ACL remove 用 `--force` |
| 动作语法 | 常见 `--list`、`--describe` flag | 原生子命令；兼容入口会改写部分旧 flag |
| 配置文件 | Java properties | 支持 Java-compatible properties，并映射常见 librdkafka key |
| Java 插件 | formatter、reader、deserializer 可加载类 | 不加载 JVM 类；原生实现默认 reader/formatter 与 StringDeserializer |
| 超时 | 各工具分别定义 | 全局 `--timeout-ms`，部分命令有原版语义差异 |

JSON 输出是本项目扩展，不属于原版 Bash 输出兼容。所有管理结果集和 mutation 结果均通过统一输出层生成，表格由 `comfy-table` 渲染，JSON 使用稳定 envelope。producer/consumer 的记录流、reset-offsets 的 CSV 导出，以及 consumer `--skip-message-on-error` 诊断仍有意使用 stdout/stderr 流，不属于表格结果集。项目当前不支持 YAML 输出。

## 6. 客户端实现架构

推荐并正在采用的依赖边界：

1. `rdkafka`：主要 API。它是 librdkafka 的安全 Rust 封装，并非另一套 Kafka 实现。
2. `rdkafka-sys`：只包装 `rdkafka` 尚未暴露的 librdkafka Admin API，例如 leader election、部分 group offset/config API。
3. `krafka`：当前仍用于 API versions、unregister、部分 reassignment/log-dir 协议路径。ACL 已完成迁移。为确保认证、重试、协议协商和错误行为一致，应继续迁移到 librdkafka/`rdkafka-sys`，最后评估删除该依赖。

认证审计发现，`krafka 0.14.0` 的内部 transport 具备 SCRAM-over-TLS，但公开 `AuthConfig` API 不能从 Kafka properties 构造“SCRAM + 自定义 TLS 配置”；本项目因此不会通过访问私有字段伪装支持。当前独立协议路径明确支持 PLAINTEXT、SSL、SASL_PLAINTEXT 的 PLAIN/SCRAM 和 SASL_SSL 的 PLAIN；SASL_SSL + SCRAM 仍是已知缺口。librdkafka 路径不受此限制。

不建议全量直接使用 `rdkafka-sys`：它与 `rdkafka` 使用同一个 librdkafka，但会把 C 指针、回调、队列和资源生命周期全部暴露为 `unsafe`，不会获得额外协议权威性。

## 7. 测试与验证现状

本地验证：

- `cargo fmt --check`：通过。
- `cargo clippy --all-targets --locked -- -D warnings`：通过。
- Rust 单元测试与普通 CLI 测试：98 个通过（91 个 library unit tests + 7 个 CLI tests）。
- Kafka 4.3.1 Docker 集成测试：通过，覆盖所有 13 个命令族及 broker default、quota、broker logger、client metrics 的设置→查询→删除闭环。
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

当前实现基准 `1eef9ae` 已由 GitHub Actions 运行
[`30836869612`](https://github.com/lihongjie0209/kafka-cli/actions/runs/30836869612) 完整验证通过：fmt/Clippy/98 个普通测试、bundled glibc、Kafka 3.6.2、包含 producer 配置三层优先级、原版默认值和 Java property 转换的 Kafka 4.3.1 实际生产消费闭环、x86_64 musl 和 aarch64 musl。此前覆盖的显式默认 `--line-reader`/`--formatter` class、StringDeserializer、console component properties、consumer 临时 group、offset 与冲突校验均持续回归通过。CI Actions 使用 Node.js 24 主版本；Zig 0.15.2 从官方 release index 获取并校验 SHA-256。该运行的六个 job 全部成功。配置写入后的集成断言采用最多 5 秒的有界重试处理 Kafka 配置传播，超时仍会保留最后一次实际输出并使测试失败。

musl 构建只在 CI 内进行，使用 Rust 1.88、固定 Zig 0.15.2 和 `cargo-zigbuild`。x86_64 musl 二进制面向 CentOS 7 等旧 glibc 环境时不依赖目标机器 glibc；ARM64 musl artifact 用于 ARM64 Linux。最终兼容性仍应在对应架构机器或容器中执行 smoke test，而不能只以 `file` 输出判断。

## 9. 建议的后续优先级

### P0：统一客户端与保证现有功能可靠

1. API versions、unregister、reassignment/log-dir 等操作在 librdkafka 2.12 中没有公开 Admin API；继续把 `krafka` 限制在这些必要路径，并统一配置、鉴权、错误与生命周期边界。只有上游公开稳定 C API 后才迁移到 `rdkafka-sys` RAII 封装，避免调用 librdkafka 私有符号。
2. 增加 TLS/SASL 集成测试。
3. 增加多 broker Kafka 4 集成环境，验证 reassignment、leader election、ISR 和 rack-aware 分配。
4. 在 CI 对两个 musl artifact 执行 `--help`/`--version` smoke test；ARM64 使用 QEMU 或原生 ARM runner。

### P1：补齐已支持脚本的主要差距

1. Configs 增加 bootstrap-controller；broker default entity 已实现并完成 Kafka 4 回归验证。
2. Reassignment 增加 bootstrap-controller，并补多 broker 长时间迁移的限流差分测试。
3. 在 librdkafka 增加相应 OffsetSpec 后，为 Get Offsets 增加 earliest-local、latest-tiered、earliest-pending-upload。
4. Topics 增加 `partition-size-limit-per-response`（需要 librdkafka 暴露对应请求选项）。

### P2：扩大原版工具覆盖面

优先考虑 `kafka-features.sh`、`kafka-metadata-quorum.sh`、`kafka-transactions.sh`、`kafka-delegation-tokens.sh` 和 `kafka-client-metrics.sh`。Connect、server start/stop、run-class、JMX 等 JVM 运行工具建议明确声明不在项目范围内，而不是做表面兼容。

## 10. 最终评估

当前项目适合作为轻量、可静态分发的 Kafka 日常管理 CLI，尤其适用于 Topic、offset、基础 Consumer Group、ACL、记录删除和集群信息查询。它已经具备跨 Kafka 3.6/4.3 的实测基础，但对于“替换 Kafka 发行包全部 Bash 脚本”这一目标仍不完整。

在对外发布时，建议使用“兼容 13 个常用 Kafka CLI 入口的 Rust 工具”表述，不应使用“100% 兼容 Apache Kafka CLI”。完成 P0 和 P1 后，才适合将已覆盖的 13 个脚本声明为主要功能兼容。

### 10.1 二次代码审计发现的待修正项

以下不是远期功能愿望，而是已实现命令与原版语义之间仍需处理的具体问题：

| 优先级 | 位置 | 当前行为 | 原版/期望行为 |
|---|---|---|---|
| P2 | regex | Rust regex 预校验与 librdkafka POSIX topic regex 不是同一语法实现 | 应明确分离 Java Pattern 兼容校验与实际订阅语法，或避免过度承诺 |

本轮已经修复 reset-offsets 的 active group 安全检查、空 group 批处理中断、重复 topic selector 合并、groups delete 结构化输出，以及 reassignment 的逐 partition/log-dir 错误结果。当前代码审计剩余问题是 Java Pattern、Rust regex 与 librdkafka POSIX regex 的语法边界。

### 10.2 逐入口验收结论

| 入口 | 动作完整性 | 主要参数完整性 | 行为/输出兼容性 | 综合结论 |
|---|---|---|---|---|
| topics | 完整 | 高 | 高/中 | 常用功能可替代，缺单次响应 partition 限制 |
| console-producer | 单动作 | 中 | 中 | 可做常规生产，不替代 Java reader 插件体系 |
| console-consumer | 单动作 | 中 | 中/高 | group、commit、offset 与配置优先级已对齐；不替代 formatter/deserializer 插件体系 |
| consumer-groups | 完整 | 高 | 高/中 | reset 已对齐 inactive group、空 group、重复 selector 和结构化批量输出；epoch 列仍受 librdkafka 限制 |
| configs | 完整 | 高 | 中/高 | topic/broker（含 default entity）/group/SCRAM、quota、broker-logger/client-metrics 可用，缺 bootstrap-controller |
| get-offsets | 单动作 | 高 | 中 | 查询能力较完整，缺分层存储相关 OffsetSpec |
| acls | 完整 | 中/高 | 中 | 常见 ACL 可用，缺新资源、controller 和原版确认模式 |
| reassign-partitions | 完整 | 高 | 中/高 | 支持追加迁移、复制因子保护、两类限流及自动清理/preserve；仍缺 controller |
| delete-records | 单动作 | 高 | 高 | 核心功能完整；兼容脚本立即执行，原生入口保留预览保护 |
| leader-election | 单动作 | 高 | 高 | 核心功能完整；兼容脚本立即执行，原生入口保留预览保护 |
| log-dirs | 单动作 | 高 | 高/中 | 常用 describe 能力完整 |
| broker-api-versions | 单动作 | 高 | 中 | 核心查询完整，输出格式不同 |
| cluster | 完整 | 中/高 | 中/高 | 常用查询、fenced broker endpoint 和 unregister 可用；兼容 unregister 立即执行，仍缺 bootstrap-controller |

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
| 2026-08-03 | Consumer Group 批量 reset | reset-offsets 支持重复 group/topic、all-groups、all-topics 和显式 dry-run；预览改为统一表格/JSON 输出，执行仍使用 librdkafka `AlterConsumerGroupOffsets` | Clippy、53 个普通测试、bundled Kafka 4.3.1 all-groups/all-topics 集成测试通过 |
| 2026-08-03 | Leader Election JSON 批量目标 | 新增原版 `--path-to-json-file` 格式，校验空列表、非法/重复 partition；librdkafka `ElectLeaders` FFI 从单目标扩展为 partition list，预览改为统一结构化输出 | Clippy、54 个普通测试、bundled Kafka 4.3.1 单目标/批量目标集成测试通过 |
| 2026-08-03 | Consumer Group reset CSV | 新增原版 `--export`/`--from-file` 无表头 CSV；支持单 group 三列与多 group 四列格式、CSV 转义、重复/选择范围校验和 offset 边界调整，执行复用 librdkafka `AlterConsumerGroupOffsets` | Clippy、55 个普通测试、bundled Kafka 4.3.1 export→import→execute 集成闭环通过 |
| 2026-08-03 | Console Producer 主要参数 | 新增 sync/默认 async 发送模式，并将 batch、retries/backoff、linger、request timeout、metadata expiry、buffer memory、socket buffer 等原版参数映射到 librdkafka；补充原版 acks/compression 参数名 | Clippy、56 个普通测试、bundled Kafka 4.3.1 sync 调优参数与默认 async 发送/消费闭环通过 |
| 2026-08-03 | Console Consumer librdkafka 选项 | 新增 include 正则订阅、空闲 timeout、isolation level 和 skip-message-on-error；修正手工 partition 无 offset 时默认为 latest，command-property 保持最高优先级 | Clippy、57 个普通测试、bundled Kafka 4.3.1 include/read_committed/timeout 参数消费闭环通过 |
| 2026-08-03 | Configs add-config-file | 新增原版 Java properties `--add-config-file`，与 add-config 互斥并复用 librdkafka IncrementalAlterConfigs；普通配置变更预览改为统一表格/JSON | Clippy、58 个普通测试、bundled Kafka 4.3.1 properties 文件 add/delete 闭环通过 |
| 2026-08-03 | Configs 全实体与 `--all` | describe 的 entity-name 改为可选，topic/broker/group 分别通过 librdkafka metadata/ListConsumerGroups 枚举；默认过滤为动态配置，`--all` 返回继承/静态/默认配置并输出 entity/source | Clippy、59 个普通测试、bundled Kafka 4.3.1 全 topic 枚举及 all config 集成测试通过 |
| 2026-08-03 | Consumer Group validate-regex | 新增无需 broker 的结构化正则校验子命令，并为 kafka-consumer-groups 兼容入口改写原版 `--validate-regex` | Clippy、61 个普通测试（含无 bootstrap CLI 测试）通过 |
| 2026-08-03 | Consumer Group verbose | offset verbose 输出 librdkafka committed leader epoch；members verbose 输出 current/target assignment，普通视图只显示 partition 数量；明确 group/member epoch 未由 librdkafka 2.12 暴露 | Clippy、61 个普通测试、bundled Kafka 4.3.1 verbose offsets/members 集成测试通过 |
| 2026-08-03 | Consumer Group reset partition selector | reset-offsets 新增原版 `topic:partition,partition` 选择语法，校验负数、重复和不存在 partition；规划及执行继续使用 librdkafka metadata/ListOffsets/AlterConsumerGroupOffsets | Clippy、62 个普通测试、bundled Kafka 4.3.1 单 partition JSON 规划集成测试通过 |
| 2026-08-03 | Consumer Group delete-offsets 批量 topic | delete-offsets 支持重复 topic 与 `topic:partition,partition`，校验不存在 partition；librdkafka DeleteConsumerGroupOffsets FFI 扩展为跨 topic partition list，预览改用统一表格/JSON | Clippy、62 个普通测试、bundled Kafka 4.3.1 跨 topic 删除集成测试通过 |
| 2026-08-03 | Consumer Group 二次语义审计 | reset-offsets 仅处理 Empty/Dead/不存在的 group，活动组写入结构化 errors；无 committed topic 的 group 不再中断批次；重复 topic selector 合并；group delete 统一 table/JSON 结果 | Clippy、66 个普通测试、Kafka 4.3.1 active-group/重复 selector/delete JSON 集成测试通过 |
| 2026-08-03 | 管理结果结构化输出 | Topics create/alter/delete、SCRAM alter、ACL add/remove、delete-records 统一接入 `comfy-table`/JSON envelope；ACL FFI 不再直接写 stderr，逐请求错误进入 envelope | Clippy、66 个普通测试、Kafka 4.3.1 Topics/SCRAM/ACL/delete-records JSON 集成测试通过 |
| 2026-08-03 | 非流式输出闭环 | Reassignment execute/cancel、cluster unregister 及 Topics no-op 结果接入统一 mutation table/JSON；生产/消费流和 CSV 保持专用流格式 | Clippy、66 个普通测试、Kafka 4.3.1 reassignment/unregister JSON 集成测试通过 |
| 2026-08-03 | 参数与 panic 路径复审 | reset-offsets 在任何 broker 请求前校验 reset scenario；移除 consume、group dispatch、user config 枚举路径的生产代码 `expect!/unreachable!` | all-features Clippy、67 个普通测试通过 |
| 2026-08-03 | Reassignment partial failure | execute/cancel 将 top-level 和 partition error 映射到具体 mutation 行；AlterReplicaLogDirs 错误包含 broker/topic/partition，不再只返回汇总数量 | all-features Clippy、67 个普通测试、Kafka 4.3.1 失败请求 JSON 与非零退出码集成测试通过 |
| 2026-08-03 | Console 默认 reader/formatter property | 完整实现原版默认 LineMessageReader 的 8 个解析属性，以及 DefaultMessageFormatter 的 12 个输出属性；保留 header 顺序/重复 key 和原始消息 bytes，Java class/deserializer 明确 unsupported | all-features Clippy、69 个普通测试、Kafka 4.3.1 header/key/null/separator 数据闭环通过 |
| 2026-08-03 | Reassignment 安全与限流 | execute 新增 additional 全局活动迁移保护、复制因子变化保护，以及 inter-broker/log-dir throttle；topic/broker throttle 由 librdkafka IncrementalAlterConfigs 写入，并合并已有迁移计算限流副本集合 | all-features Clippy、73 个普通测试通过；Kafka 4.3.1 与 musl 由当前 CI 验证 |
| 2026-08-03 | Reassignment throttle 生命周期 | verify 同时核对全局活动 partition reassignment 与目标 future log-dir replica，结束后清理目标 topic、集群及计划 broker 的全部 throttle；cancel 成功后同样清理，preserve-throttles 可显式保留 | all-features Clippy、73 个普通测试通过；Kafka 4.3.1 集成增加设置→describe→verify→清理闭环 |
| 2026-08-03 | Client quota entities | configs 新增 users/clients/ips quota、user+client 复合实体和 default entity；Describe/AlterClientQuotas API 48/49 负责查询、写入、删除校验和 controller 路由；user describe 同时合并 quota 与 SCRAM | all-features Clippy、76 个普通测试、Kafka 3.6.2/4.3.1 client/user+client/default IP 设置→查询→删除闭环及双 musl CI 通过 |
| 2026-08-03 | Broker logger 与 client metrics configs | 新增 ConfigResource type 8/16；broker logger 直连指定 broker，client metrics 使用 API 74 枚举并由 controller 处理增量修改，所有结果进入统一 table/JSON envelope | all-features Clippy、76 个普通测试、Kafka 3.6.2/4.3.1 设置→查询/枚举→删除闭环及双 musl CI 通过 |
| 2026-08-03 | Broker default config entity | `configs` 使用空名称 ConfigResource 支持 broker `--entity-default` 的 describe/add/delete，并按动态默认 broker source 过滤结果 | 76 个普通测试、Kafka 3.6.2/4.3.1 设置→查询→删除闭环、glibc release 及 x86_64/aarch64 musl CI 全部通过 |
| 2026-08-03 | Fenced broker endpoints | `cluster list-endpoints --include-fenced-brokers` 使用 DescribeCluster v2 返回 fenced 状态；默认路径仍使用 librdkafka，表格对齐原版 STATE/ENDPOINT_TYPE 列 | 77 个普通测试、Kafka 4.3.1 实际协议请求、Kafka 3.6.2 回归及双 musl CI 全部通过 |
| 2026-08-04 | kafka-cluster 入口语法 | cluster ID 的规范子命令改为原版 `cluster-id` 并保留 `id` 别名；补齐全局 `-b/-c` 和 unregister `-i` | 74 个单元测试、5 个 CLI 测试（含真实 `kafka-cluster.sh` symlink 启动）、Kafka 3.6.2/4.3.1 及双 musl CI 全部通过 |
| 2026-08-04 | kafka-log-dirs 动作语法 | 接受原版必需的 `--describe` 动作 flag，同时保留原生无动作 flag 的简洁调用 | 74 个单元测试、6 个 CLI 测试（含真实 `kafka-log-dirs.sh` symlink 启动）、Kafka 4.3.1 原版语法请求、Kafka 3.6.2 及双 musl CI 全部通过 |
| 2026-08-04 | 兼容入口 mutation 语义 | legacy rewrite 对 groups delete/delete-offsets、configs alter、ACL add、reassignment execute/cancel 自动保留立即执行；ACL remove 接受 `--force` | 77 个单元测试逐动作核对、7 个 CLI 测试（真实 configs alias 验证非 PREVIEW）、Kafka 3.6.2/4.3.1 及双 musl CI 全部通过 |
| 2026-08-04 | 单动作 mutation 兼容语义 | `kafka-delete-records.sh`、`kafka-leader-election.sh` 和 `kafka-cluster.sh unregister` 自动进入执行路径；原生子命令继续要求 `--execute` | 78 个单元测试、7 个 CLI 测试、Kafka 3.6.2/4.3.1、bundled glibc 及 x86_64/aarch64 musl CI 全部通过 |
| 2026-08-04 | Console component config | producer `--reader-config` 与 consumer `--formatter-config` 加载 Java properties，重复命令行 property 覆盖文件值；兼容入口补齐 leader election `--admin.config` 和 cluster `--config` 废弃别名 | 80 个单元测试、7 个 CLI 测试、Kafka 4.3.1 实际文件生产消费闭环、Kafka 3.6.2、bundled glibc 及双 musl CI 全部通过 |
| 2026-08-04 | Console producer 参数语义 | 修正 metadata-expiry 映射，新增 max-block 队列等待与 max-partition-memory 覆盖，compression-codec 无值默认 gzip；规范 reader/formatter property 名称并兼容 producer.config/consumer.config | 81 个单元测试、7 个 CLI 测试、Kafka 4.3.1 实际生产消费、Kafka 3.6.2、bundled glibc 及双 musl CI 全部通过 |
| 2026-08-04 | Console consumer 默认语义 | 无显式 group 时生成临时 group 并默认关闭自动提交；校验 group 来源一致性及 group/partition 冲突；offset 支持 earliest/latest/非负整数并要求 partition；对齐 from-beginning、auto.offset.reset 与 isolation 配置优先级 | 85 个单元测试、7 个 CLI 测试、Kafka 4.3.1 重复无 group 消费和命名 offset 闭环、Kafka 3.6.2、bundled glibc 及双 musl CI 全部通过 |
| 2026-08-04 | Console component class 参数 | producer 接受原版默认 `--line-reader` 类名，consumer 接受原版默认 `--formatter` 类名；自定义 JVM class 改为可解析后明确 unsupported，不再作为未知参数失败 | 87 个单元测试、7 个 CLI 测试、Kafka 4.3.1 显式默认 class 生产消费闭环、Kafka 3.6.2、bundled glibc 及双 musl CI 全部通过 |
| 2026-08-04 | Console StringDeserializer | 新增原版 key/value deserializer 参数；默认 formatter 原生支持 Kafka StringDeserializer 及 headers property，formatter property 覆盖 CLI，UTF-8 非法字节按 Java 语义替换；其他 JVM class 明确 unsupported | 89 个单元测试、7 个 CLI 测试、Kafka 4.3.1 StringDeserializer 生产消费闭环、Kafka 3.6.2、bundled glibc 及双 musl CI 全部通过 |
| 2026-08-04 | CI Node.js 24 与 Zig 供应链 | checkout/setup-java/upload-artifact/rust-cache 升级到 Node.js 24 版本；移除仍使用 Node.js 20 的 setup-zig，改为从官方索引解析固定 Zig 0.15.2、校验 SHA-256 后安装；quota/broker logger 配置传播断言统一使用有界重试 | actionlint 1.7.12 通过；96 个普通测试、Kafka 3.6.2/4.3.1、bundled glibc、x86_64/aarch64 musl 全绿，六个 job 零 annotation |
| 2026-08-04 | Console producer 配置优先级与默认值 | 对齐“显式 CLI > command property/config > 原版脚本默认值”；补齐 acks、batch、retries、backoff、linger、timeout、buffer、client id 等默认值，并将 Java `buffer.memory`、`send.buffer.bytes`、`max.block.ms` 转换为 librdkafka/本地语义 | 91 个单元测试、7 个 CLI 测试、Kafka 4.3.1 property 与显式参数优先级生产闭环、Kafka 3.6.2、bundled glibc 及双 musl CI 全部通过 |

## 12. librdkafka 2.12 能力闭环审计

对 `rd_kafka_admin_op_t` 的 22 个实际 operation（排除 `ANY` 和计数 sentinel）逐项核对如下。表中的“已覆盖”表示项目存在真实调用路径，而不是只存在绑定或占位命令。

| librdkafka Admin operation | 项目功能 | 状态 |
|---|---|---|
| CreateTopics / DeleteTopics / CreatePartitions | topics create/delete/alter | 已覆盖 |
| DescribeConfigs | configs describe | 已覆盖 |
| IncrementalAlterConfigs | configs alter | 已覆盖 |
| AlterConfigs | 被 IncrementalAlterConfigs 完整替代，避免全量覆盖配置 | 有意不单独调用 |
| DeleteRecords | delete-records | 已覆盖 |
| DeleteGroups | groups delete | 已覆盖 |
| DeleteConsumerGroupOffsets | groups delete-offsets | 已覆盖，含跨 topic partition list |
| CreateAcls / DescribeAcls / DeleteAcls | acls add/list/remove | 已覆盖 |
| ListConsumerGroups / DescribeConsumerGroups | groups list/describe | 已覆盖 |
| ListConsumerGroupOffsets / AlterConsumerGroupOffsets | groups describe/reset-offsets | 已覆盖 |
| DescribeUserScramCredentials / AlterUserScramCredentials | configs users describe/alter | 已覆盖 |
| DescribeTopics | topics describe/topic-id | 已覆盖 |
| DescribeCluster | cluster id/list-endpoints | 已覆盖 |
| ListOffsets | offsets 与 group reset 规划 | 已覆盖 |
| ElectLeaders | leader-election | 已覆盖，含 JSON 批量目标 |

非 Admin 客户端能力也已接入 console producer/consumer：异步与同步 delivery、主要 producer tuning 配置、正则订阅、空闲 timeout、isolation level、手工 partition/offset、headers 和结构化消息。其他 librdkafka 配置可通过 `--command-property` 传入。

以下 Kafka 原版能力在 librdkafka 2.12 中没有对应 C API，因此不属于本轮可实现集合：partition reassignment、describe/alter log dirs、API version 明细、unregister broker、client quota、broker logger/client metrics config resource、metadata quorum、feature update、transaction listing/abort、delegation token、share/streams group Admin API。项目现有少数同名功能仍由 `krafka` 实现，报告不会把它们误记为 librdkafka 路径。

此外，已通过实际 Kafka 4.3.1 测试确认 broker default config 无法用 librdkafka 表达：Kafka 要求空 ConfigResource name，而 `rd_kafka_ConfigResource_new` 在空 name 时返回 NULL。Get Offsets 的 `-4/-5/-6`、consumer group epoch、fenced broker inclusion、Kafka 4.4 user-principal ACL resource和 Delegation Token ACL resource也未由当前版本暴露；其中 fenced broker inclusion 已通过独立 DescribeCluster v2 协议路径补齐。

结论：以 librdkafka 2.12 的公开 Admin operation 枚举为边界，除已被增量 API 取代的旧 AlterConfigs 外，当前所有 operation 均已有实际 CLI 调用与测试路径。后续剩余差距需要升级 librdkafka、继续使用独立协议客户端，或属于 Java 插件/服务进程而非 librdkafka 客户端能力。
