# kafka-cli 与 Apache Kafka 原版工具功能对比报告

报告日期：2026-08-05

**维护约定**：每次新增或实质变更命令入口后，必须同步更新本报告（入口表、仪表盘计数、详细对比节、逐入口验收表、变更记录与测试现状），不得只改代码/README。

本地对照源码：推荐浅克隆 Apache Kafka 至仓库旁目录（默认 `/root/code/kafka`），见 `docs/KAFKA_SOURCE_REFERENCE.md` 与 `scripts/fetch-kafka-source.sh`。

## 1. 结论摘要

本项目当前是一个可用的 Rust Kafka 管理与数据 CLI，但还不能称为 Apache Kafka 全部 Bash 工具的完整复刻。

- Apache Kafka 对比基准：`trunk`（本机浅克隆 `/root/code/kafka` 当前为 `89ccd6a`；报告历史段落中的旧提交哈希仍可能指向当时审计点）。
- 本项目审计实现基准：`master` 工作树；本轮新增 `dump-log`、`storage`，并已覆盖 verifiable-consumer / replica-verification。
- Kafka 原版 `bin/` 目录有 44 个 `.sh` 入口；本项目识别其中 **33** 个兼容名称，入口覆盖率为 **33/44（75.0%）**。这个数字只表示入口名称，不表示选项或行为已完全兼容。
- 按本报告的功能口径，33 个兼容入口中 **22** 个达到核心功能“已支持”，**11** 个为“部分支持”；另有 **11** 个原版入口**有意不实现**（JVM 服务 / Connect / 测试框架等）。因此不能把 75% 的入口覆盖率解释成完整功能覆盖率，更不能宣称 100% 兼容。
- 已覆盖的核心领域包括 Topic、Consumer/Share/Streams Group、配置、ACL、分区迁移、console/verifiable/perf 工具、replica verification、dump-log、KRaft storage format/info，以及 cluster/transactions/quorum 等管理路径。
- **明确不在范围内**：Connect 五件套、`kafka-server-start/stop`、`kafka-run-class`、`kafka-jmx`、`trogdor`、完整交互式 `kafka-metadata-shell`（见 §3.2）。
- 当前网络栈以 `rdkafka`（librdkafka）为主；`krafka` 仍用于部分 Admin/Fetch/Share 协议路径。

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
| Kafka `.sh` 入口 | 33 / 44（75.0%） | 11 个入口有意不实现（JVM 服务/Connect/Trogdor/完整 metadata-shell） |
| 入口功能评级 | 22 已支持 / 11 部分支持 / 11 未支持（范围外） | “已支持”表示核心动作与主要语义可用，不表示输出逐字符一致 |
| 已覆盖入口的一级动作 | 80+ | 33 个入口均有真实执行路径；storage 含 5 个子命令；不代表参数/输出逐字符兼容 |
| librdkafka 2.12 Admin operation | 21 / 21 个应调用操作 | 22 个实际枚举中，旧 `AlterConfigs` 被 `IncrementalAlterConfigs` 替代 |
| 普通自动化测试 | 312 个通过 | 275 library + 30 CLI + 7 offline_tools；Kafka 3/4 集成与 kafka_4_depth（7 个）默认 ignored，由 CI 运行 |
| 已验证 broker | Kafka 3.6.2、Kafka 4.3.1 | 当前基准在两者全绿；均为单 broker 代表性路径，不等于完整兼容矩阵 |
| 静态发布目标 | glibc、x86_64 musl、aarch64 musl | musl 只在 CI 构建；ARM64 交叉编译后经 QEMU 启动验证 |

原版动作数 75 的构成：Topics 5、Consumer Groups 6、Share Groups 5、Streams Groups 5、Transactions 6、Features 6、Client Metrics 4、Delegation Tokens 4、Metadata Quorum 3、Configs 2、ACLs 3、Reassignment 5、Cluster 3，其余 18 个入口各 1。Console producer/consumer/share-consumer、E2E Latency、Producer Performance、普通/Share Consumer Performance、Verifiable Producer/Consumer/Share Consumer、Replica Verification、通用 Groups list 与 Streams Application Reset 属于单动作命令；本项目额外把 API versions 也放入 cluster 子命令。

## 3. 顶层脚本覆盖

### 3.1 已提供兼容入口的原版脚本

| Apache Kafka 脚本 | Rust 入口 | 状态 | 说明 |
|---|---|---|---|
| `kafka-topics.sh` | `kafka topics` | 已支持 | list、describe、create、alter、delete |
| `kafka-console-producer.sh` | `kafka produce` | 部分支持 | 行输入、sync/async、key、JSON、headers、partition、reader config 及主要 producer 调优参数；未复刻可插拔 reader class |
| `kafka-producer-perf-test.sh` | `kafka producer-perf-test` | 部分支持 | 三种 payload、限速、key 分布、warmup、事务、延迟分位数及 metrics；完整 Java metrics 和 Java regex 全方言仍有边界 |
| `kafka-e2e-latency.sh` | `kafka e2e-latency` | 已支持 | 同步生产/消费、自动建 topic、key/value/header 校验、平均与分位延迟、named/legacy 参数；错误堆栈与个别 optional arg 为有意差异 |
| `kafka-verifiable-producer.sh` | `kafka verifiable-producer` | 已支持 | JSON Lines systest 事件、异步 delivery、限速、acks、重复 key、value prefix、create time 与配置优先级；异常类名非 Java FQCN 为有意差异 |
| `kafka-console-consumer.sh` | `kafka consume` | 部分支持 | topic/include/group/partition/offset/from-beginning/max-messages/timeout/isolation、formatter config 和 StringDeserializer；未复刻任意 Java formatter/deserializer class 加载 |
| `kafka-console-share-consumer.sh` | `kafka share-consume` | 部分支持 | KIP-932 ShareFetch/ShareAcknowledge、accept/release/reject、超时、formatter、delivery count 与配置优先级；未复刻任意 Java formatter/deserializer class 和全部 Java consumer property |
| `kafka-share-consumer-perf-test.sh` | `kafka share-consumer-perf-test` | 部分支持 | 多 Share Consumer、记录/字节吞吐、超时、周期/逐 consumer/汇总 CSV 和 native metrics；fetch/socket 旋钮与 Java metrics 集合未完全等价 |
| `kafka-consumer-perf-test.sh` | `kafka consumer-perf-test` | 部分支持 | topic/include、记录/字节吞吐、rebalance/fetch 时间、周期/汇总十列 CSV 和 native metrics；Java metrics 集合、完整日期格式与 rebalance 回调精度不完全等价 |
| `kafka-verifiable-share-consumer.sh` | `kafka verifiable-share-consumer` | 部分支持 | JSON Lines systest 事件、verbose record、max-messages、offset reset、sync/async/auto 及循环 accept/release/reject/renew pattern；async completion 当前由同步提交实现 |
| `kafka-verifiable-consumer.sh` | `kafka verifiable-consumer` | 已支持 | JSON Lines systest 事件、assignment/revocation、records/commits、classic 与 KIP-848 protocol、static membership 与 reset policy |
| `kafka-replica-verification.sh` | `kafka replica-verification` | 部分支持 | 按 broker 拉取 replica 数据、batch offset/CRC 校验、max lag 周期报告；原版多线程 barrier 改为异步并发 Fetch |
| `kafka-dump-log.sh` | `kafka dump-log` | 部分支持 | `.log`/`.index`/`.timeindex`、`.txnindex`、producer `.snapshot`、控制记录（ABORT/COMMIT/LeaderChange 等）、StringDecoder；coordinator 专用 decoder 未实现 |
| `kafka-storage.sh` | `kafka storage` | 部分支持 | `random-uuid`、`info`、`format`（`meta.properties` + `__cluster_metadata-0/` + `kafka-cli-bootstrap.residual.json`）；**不写** Kafka 保留的二进制 `bootstrap.checkpoint` 文件名；完整 RecordsSnapshotWriter / SCRAM 未实现 |
| `kafka-consumer-groups.sh` | `kafka groups` | 已支持 | list、批量 describe/delete/reset-offsets、delete-offsets，以及 reset CSV 导入导出；regex 方言与 protocol-epoch 边界见详细节 |
| `kafka-groups.sh` | `kafka all-groups` | 已支持 | list；group-type、protocol、consumer、share、streams 过滤，覆盖全部 Kafka group 类型 |
| `kafka-share-groups.sh` | `kafka share-groups` | 已支持 | list、describe、delete、reset-offsets、delete-offsets，覆盖 Share Group 状态、成员、assignment 与 offset 管理 |
| `kafka-streams-groups.sh` | `kafka streams-groups` | 已支持 | list、describe、delete、reset-offsets、delete-offsets，覆盖 Streams task assignment、offset lag、内部 topic 与 Kafka 4.4 topology description |
| `kafka-streams-application-reset.sh` | `kafka streams-application-reset` | 已支持 | input/intermediate topic offset reset、active member 检查与 force remove、内部 topic 推断/显式删除、dry-run 和 CSV reset plan |
| `kafka-configs.sh` | `kafka configs` | 已支持 | topic、broker（含 default）、group、SCRAM、client quota、broker logger、client metrics；支持 `--bootstrap-controller` 作为 admin bootstrap 路由 |
| `kafka-client-metrics.sh` | `kafka client-metrics` | 已支持 | list、describe、alter、delete、自动生成 Kafka UUID 名称及全部 subscription 配置参数 |
| `kafka-features.sh` | `kafka features` | 已支持 | describe、upgrade、downgrade、disable、version-mapping、feature-dependencies；支持 `--bootstrap-controller` 路由；纯 controller listener 连接残差见详细节 |
| `kafka-transactions.sh` | `kafka transactions` | 已支持 | list、describe、describe-producers、abort、find-hanging、forceTerminateTransaction；保留 coordinator 与指定 broker 语义 |
| `kafka-metadata-quorum.sh` | `kafka metadata-quorum` | 已支持 | describe status/replication、add-controller、remove-controller；完整 DescribeQuorum v2 与动态 voter API；支持 `--bootstrap-controller` 路由 |
| `kafka-delegation-tokens.sh` | `kafka delegation-tokens` | 已支持 | create、renew、expire、describe；owner/requester/renewer、标准 Base64 HMAC 与 `-1` 时间语义 |
| `kafka-get-offsets.sh` | `kafka offsets` | 已支持 | 常规 OffsetSpec 使用 librdkafka ListOffsets；分层存储 `-4/-5/-6` 使用 ListOffsets v11 协议 fallback；过滤与逐 partition 语义已对齐；表格/JSON 输出为有意差异 |
| `kafka-acls.sh` | `kafka acls` | 部分支持 | librdkafka Admin API；list/add/remove、常见资源和 producer/consumer 快捷角色 |
| `kafka-reassign-partitions.sh` | `kafka reassign` | 已支持 | generate/execute/verify/cancel/list；execute 限流/安全参数及 verify/cancel throttle 生命周期；支持 `--bootstrap-controller` 路由 |
| `kafka-delete-records.sh` | `kafka delete-records` | 已支持 | JSON 文件；原生入口支持预览，兼容脚本按原版立即执行 |
| `kafka-leader-election.sh` | `kafka leader-election` | 已支持 | preferred/unclean、单分区、JSON 文件批量选择或全部分区；兼容脚本按原版立即执行 |
| `kafka-log-dirs.sh` | `kafka log-dirs` | 已支持 | broker/topic 过滤与目录、大小、lag 展示 |
| `kafka-broker-api-versions.sh` | `kafka api-versions` | 已支持 | 全 broker 或指定 broker 的 API version 范围 |
| `kafka-cluster.sh` | `kafka cluster` | 已支持 | cluster ID、endpoints（含 fenced broker）、API versions、unregister；支持 `--bootstrap-controller` 路由 |

兼容入口可以通过软链接名称调用，支持带或不带 `.sh` 后缀。对原版使用 `--create`、`--describe` 等动作 flag 的部分脚本，会自动改写为 Rust 子命令。原版 mutation 动作会同时保留“立即执行”语义，不会被静默降级为预览；`kafka-cluster.sh` 接受原版 `cluster-id` 名称及 `-b/-c/-i` 短参数，同时保留 `id` 作为原生别名。

### 3.2 有意不实现 / 无法等价复刻的原版脚本

以下 **11** 个入口**不会**作为本项目目标能力交付。它们依赖 JVM 运行时、完整 Broker/Connect 进程模型或大规模交互式元数据文件系统，与“原生静态客户端 CLI”定位冲突。

| 类别 | 脚本 | 不实现原因 |
|---|---|---|
| Kafka Connect 运行时 | `connect-distributed.sh`、`connect-standalone.sh`、`connect-mirror-maker.sh` | 启动完整 Connect/MirrorMaker 工作进程，不是客户端管理工具 |
| Connect 辅助 | `connect-plugin-path.sh`、`connect-internal-topics.sh` | 依赖 Connect 插件类路径与 JVM 类加载 |
| Broker 进程 | `kafka-server-start.sh`、`kafka-server-stop.sh` | 启动/停止 Kafka broker 进程 |
| JVM 基础设施 | `kafka-run-class.sh` | 通用 Java classpath runner |
| JMX | `kafka-jmx.sh` | 依赖 Java JMX/RMI 客户端栈；纯 Rust 无稳定等价协议栈 |
| 故障注入测试 | `trogdor.sh` | Trogdor 代理/协调器测试框架 |
| 交互元数据 shell | `kafka-metadata-shell.sh` | 完整交互式 KRaft metadata image 浏览器（文件系统 + 多命令 shell）；体量接近独立产品 |

**部分实现说明（已有入口，但能力有边界）**：

| 入口 | 已实现 | 明确未实现 / 弱实现 |
|---|---|---|
| `dump-log` | `.log`/`.index`/`.timeindex`、`.txnindex` v0、producer `.snapshot` v1、控制记录类型命名与 EndTxn/LeaderChange 前缀、StringDecoder | offsets/transaction/cluster/remote/share 专用 decoder；KRaft control 记录完整 Java JSON 细节 |
| `storage` | `random-uuid`；`info`；`format` 写 meta.properties + `__cluster_metadata-0/` + `kafka-cli-bootstrap.residual.json`（**永不**占用二进制 `bootstrap.checkpoint` 名） | 完整 RecordsSnapshotWriter 二进制 KRaft 快照；`--add-scram`；动态 quorum 完整 topology 物化 |
| `replica-verification` 等 | 见 §3.1 | 多 broker 不一致集成矩阵、Java metrics 全集等 |

若未来需要 Connect/Broker 运维，应使用官方 Kafka 发行包或专用编排工具，而不是扩展本 CLI。

## 4. 已覆盖命令的详细对比

### 4.1 Topics

已支持：

- list、describe、create、alter、delete。
- topic 名称和 Java 风格整串正则匹配。
- create 的 partitions、replication factor、手工 replica assignment、重复 `--config`、`--if-not-exists`；未指定 partition 数或 replication factor 时均使用 librdkafka `-1` sentinel 交给 broker 默认值决定，手工 assignment 与这两个计数参数按原版互斥。
- alter 增加 partition 数量、手工 assignment、`--if-exists`，topic 参数支持整串正则并对全部匹配 topic 发出请求。
- delete 的正则选择和 `--if-exists`。
- describe 过滤：under-replicated、unavailable、under-min-ISR、at-min-ISR、topics-with-overrides、exclude-internal。
- describe 通过 librdkafka `DescribeTopics` 返回 topic UUID，支持按 `--topic-id` 选择、非零 ID 覆盖同时提供的 topic 名、零 ID 回退 topic 名，以及 `--if-exists`；表格/JSON 包含 replication factor 和非默认有效配置。
- 接受并校验 `--partition-size-limit-per-response`，完整结果由 librdkafka 自动获取。
- unavailable 判定会核对 leader 是否仍在 live broker 集合中。

缺少或有差异：

- librdkafka 2.12 C Admin API 未暴露 `partition-size-limit-per-response` 请求旋钮，因此该兼容参数不会强制指定单次响应的 partition 上限；结果集合不受影响。
- Kafka 4.4 原版 `--delete-config` 只打印“自 4.0 起不再支持”的弃用警告，不再执行配置删除；本项目不提供这个无效果占位参数，topic 配置删除由 `configs alter --delete-config` 实际执行。
- 表格列名和排版不是原版逐字符复制。

### 4.2 Console Producer

已支持：topic、acks（含原版 `--request-required-acks` 名称）、compression（含可省略值并默认 gzip 的 `--compression-codec`）、stdin 行输入、JSON 输入、指定 partition、headers、`--sync` 逐条等待和默认异步排队、`--command-property` 及旧名 `--producer-property`，兼容脚本也接受废弃的 `--producer.config`。默认 `LineMessageReader` 的 `--reader-config` Java properties 文件、规范 `--reader-property` 及旧名 `--property` 已支持 parse.key、key.separator、parse.headers、headers.delimiter、headers.separator、headers.key.separator、ignore.error、null.marker，并保留 header 顺序和重复 key；命令行 property 覆盖文件值。原版 batch-size、覆盖它的废弃 max-partition-memory-bytes、message-send-max-retries、retry-backoff-ms、timeout/linger、request-timeout-ms、metadata-expiry-ms、max-block-ms、max-memory-bytes、socket-buffer-size 均已实现；metadata-expiry-ms 精确映射 `metadata.max.age.ms`，max-block-ms 控制本地队列满时的等待上限。

Producer 配置遵循原版三层优先级：显式 CLI 选项覆盖 `--command-property`/配置文件，property 覆盖脚本默认值。未配置时对齐原版的 acks `-1`、batch size `16384`、retries `3`、retry backoff `100ms`、linger `1000ms`、request timeout `1500ms`、metadata max age `300000ms`、max block `60000ms`、buffer memory `32MiB`、socket send buffer `102400` 和 client id `console-producer`。Java producer property `buffer.memory`、`send.buffer.bytes`、`max.block.ms` 会分别转换为 librdkafka 队列容量、socket buffer 和本地排队等待语义，不会作为无效 librdkafka 配置透传。

原版 `--line-reader org.apache.kafka.tools.LineMessageReader` 可显式传入；其他 Java 自定义 line reader class 会明确返回 unsupported，因为原生二进制不能加载 JVM 插件。其他底层 producer 配置仍可通过 `--command-property key=value` 传入。

### 4.3 Console Consumer

已支持：topic 或整串 `--include` 正则、group、partition、offset、from-beginning、max-messages、`--timeout-ms` 空闲退出、read_committed/read_uncommitted isolation level、skip-message-on-error、JSON、print-key、key separator、`--command-property` 及旧名 `--consumer-property`，兼容脚本也接受废弃的 `--consumer.config`。默认 `DefaultMessageFormatter` 的 `--formatter-config` Java properties 文件、规范 `--formatter-property` 及旧名 `--property` 支持 print.timestamp、print.partition、print.offset、print.delivery、print.epoch、print.headers、print.key、print.value、key.separator、line.separator、headers.separator、null.literal；命令行 property 覆盖文件值，并直接写出消息原始 bytes。include 使用 librdkafka 的动态 POSIX ERE 订阅，并直接由 librdkafka 编译校验，不再先用不同方言的 Rust regex 错误拒绝；它仍不等价于全部 Java Pattern 扩展。未显式指定 group 时会生成 `console-consumer-*` 临时 group，并在用户没有配置 `enable.auto.commit` 时默认关闭自动提交；显式 group、配置文件 `group.id` 与命令行 property 的值必须一致。手工 partition 与 group 互斥；`--offset` 必须配合 partition，接受 `earliest`、`latest` 或非负整数，未指定时与原版一致使用 latest。`--from-beginning` 与显式 offset 互斥，并拒绝冲突的 `auto.offset.reset`；CLI isolation level 覆盖 properties，未提供 CLI 值时保留 property 或使用 read_uncommitted 默认值。

与原版一致，`--max-messages 0` 在 poll 前立即结束，`-1` 表示无限消费；负 `--timeout-ms` 表示不启用空闲超时，未指定该选项时也不会被全局管理请求的 30 秒默认值污染。producer/consumer 的废弃 property 名称仍可单独使用，但与对应的新名称同时出现时会像原版一样报错；废弃 config 文件名与 `--command-config` 也互斥。

原版 `--formatter org.apache.kafka.tools.consumer.DefaultMessageFormatter` 可显式传入。`--key-deserializer`、`--value-deserializer` 以及 formatter property 中的 key/value/headers deserializer 已支持 Kafka `StringDeserializer`，包括 formatter property 覆盖 CLI class、UTF-8/UTF8 encoding 校验和非法 UTF-8 替换字符语义。其他自定义 Java formatter/deserializer class 和 systest events 不支持，并会明确返回 unsupported，而不是静默忽略。`print.epoch` 通过 librdkafka `rd_kafka_message_leader_epoch` 输出真实 leader epoch，未知时按原版输出 `NOT_PRESENT`；普通 consumer 没有 delivery count 时 `print.delivery` 仍输出 `NOT_PRESENT`。其他底层 consumer 配置可通过 `--command-property` 传入。

### 4.4 Consumer Groups

已支持：

- list 使用 librdkafka `ListConsumerGroups` Admin API，支持裸 `--state`/`--type` 展示列，以及逗号分隔的 broker-side state/type 过滤。
- describe 默认 offsets 视图以及 `--state`、`--members`、`--offsets`；支持重复 `--group` 和 `--all-groups`，state/members 使用 librdkafka `DescribeConsumerGroups` Admin API。
- committed offset、log end offset、lag 和错误列。
- members 输出已解码的当前和 target topic-partition assignment；state 输出 group type、assignor、member count 与 coordinator。
- describe 支持原版 `--verbose`：offset 视图增加 librdkafka 返回的 committed leader epoch；members 视图增加 current/target epoch 与 assignment，并在同组同时存在 classic/consumer protocol 成员时按原版增加 `UPGRADED`；state 视图增加 group epoch 与 target assignment epoch；非 verbose members 只显示 partition 数量。描述主体继续来自 librdkafka，Kafka 4 consumer protocol epoch/member type 由 ConsumerGroupDescribe 协议结果按 group/member ID 合并；unknown 不会误报为未升级。
- delete group 支持重复 `--group` 和 `--all-groups`；delete-offsets 支持重复裸 topic 与 `topic:partition,partition`，单个 librdkafka 请求可携带跨 topic partition list；原生子命令默认预览并通过 `--execute` 执行，兼容脚本的原版 `--delete`/`--delete-offsets` 动作立即执行。
- reset offsets：支持重复 `--group`/`--topic`、`--all-groups`、`--all-topics`、原版 `topic:partition,partition` selector、`--dry-run`/`--execute`，以及 earliest、latest、absolute offset、shift-by、current、datetime、ISO-8601 duration；支持原版 `--export` 与 `--from-file` 无表头 CSV（单 group 三列、多 group 四列），导入校验选择范围、重复目标并按 log start/end 边界调整 offset；执行阶段使用 librdkafka `AlterConsumerGroupOffsets` Admin API。
- `validate-regex` 可在不提供 bootstrap-server 时独立校验正则；兼容脚本入口的原版 `--validate-regex` 会自动改写到该子命令，结果使用表格/JSON 输出。语法由 Rust regex/librdkafka 可用集合约束，不宣称覆盖所有 Java Pattern 扩展。
- 接受原版 `--timeout`，并覆盖该 Consumer Group 命令的 Admin 请求和 group 稳定等待超时；原生全局 `--timeout-ms` 继续作为其他命令族的通用扩展名。

缺少或有差异：

- librdkafka 2.12 未暴露 Kafka consumer protocol epoch；项目只在 verbose state/members 请求时增加协议描述并合并这些列，因此该路径仍受独立协议客户端鉴权边界约束。
- Kafka 4.4 的 `kafka-consumer-groups.sh --state` 新增 `Assigning`、`Reconciling`；librdkafka 2.12 的公开 consumer-group state 枚举只能表达旧的五种状态，因此这两个过滤值使用 ListGroups v5 加 ConsumerGroupDescribe 协议路径读取原始状态字符串。其余状态仍使用 librdkafka broker-side filter。`NotReady` 与 `Share`/`Streams` 属于 Kafka 的其他 group 模型，原版 consumer-groups 入口本身也不接受它们，因此不计为该入口缺口。
- `validate-regex` 的 Kafka 4.4 帮助文案要求 RE2 格式，但原版实现实际调用 Java `Pattern.compile`；本项目使用 Rust `regex`，与 RE2 安全集合接近但并非 Java Pattern 的逐语法等价实现。

### 4.5 Configs

已支持：

- topic、broker、group 的 describe 和增量 add/delete config；broker 支持命名实体和 `--entity-default` 默认实体。
- describe 可省略 entity-name 枚举该类型的全部实体；默认只显示动态 override，原版 `--all` 显示继承、静态和默认配置。多实体输出包含 entity type/name 与 config source。
- 原版复数 entity type（`topics`、`brokers`、`groups`、`users`）以及兼容的单数别名。
- user SCRAM credential describe、upsert 和 delete，支持 SCRAM-SHA-256、SCRAM-SHA-512、iterations 与 password。
- user、client、IP quota 的 describe/alter/delete；支持重复 entity-type/entity-name 的 user+client 复合实体，以及 users/clients/ips 的 default entity。quota 值按 Kafka Double 语义解析，删除前会与现有配置核对。
- 接受原版 `--topic`、`--client`、`--user`、`--broker`、`--broker-logger`、`--ip`、`--client-metrics`、`--group` 及四类 `--*-defaults` 专用 selector；user/client 复合 quota 支持一个命名实体与另一个默认实体。兼容入口也会按第 N 个 type 对应第 N 个 name/default 的原版顺序语义处理通用 selector。
- broker-logger 与 client-metrics 的 describe/alter/delete；broker logger 请求路由到指定 broker，client metrics 支持省略名称枚举全部 subscription，并把写请求路由到 controller。
- SCRAM 预览只显示 mechanism 和 iterations，不回显 password。
- alter 支持原版 `--add-config-file` Java properties 文件，并与 `--add-config` 互斥；普通 config 预览统一使用表格/JSON，而非手工文本。
- `--add-config` 支持原版逗号分隔列表及方括号分组值，例如 `cleanup.policy=[compact,delete],retention.ms=60000`；括号内逗号/等号保留，重复 key 按 Java Properties 语义由后值覆盖，非法括号会在请求前拒绝。
- `--delete-config` 按原版支持单次逗号分隔多个 key 并逐项 trim；重复实体类型、非整数 broker/broker-logger ID 以及 add-config 非法 key 字符会在任何 broker 请求前拒绝。
- IP entity name 在 Admin 请求前按原版验证为合法 IP 或可解析主机名；alter 显式空 `--entity-name` 会拒绝并提示使用 `--entity-default`。
- 原生子命令支持预览与 `--execute`；兼容脚本的原版 `--alter` 动作立即执行。

支持 `--bootstrap-controller` 作为 admin bootstrap 路由（与 `--bootstrap-server` 互斥）。Java Admin 的独立 controller bootstrap 会话语义仍不完全等价：当前实现把 controller 地址当作 bootstrap 目标走 `protocol_admin`，纯 controller listener 上 Metadata 初始化失败时返回真实连接错误。Client quotas 使用 Kafka DescribeClientQuotas/AlterClientQuotas API 48/49；broker-logger/client-metrics 与 broker default entity 使用 DescribeConfigs/IncrementalAlterConfigs 32/44，client-metrics 枚举还使用 ListConfigResources 74。librdkafka 2.12 没有 quota 和高级配置资源的公开 C API；broker default entity还必须使用空 ConfigResource name，而 `rd_kafka_ConfigResource_new` 在 name 长度为 0 时返回 NULL。因此这些路径由项目协议客户端完成版本协商、目标 broker/controller 路由和逐资源错误处理。SCRAM upsert 依赖启用 OpenSSL 的 librdkafka；bundled 与 musl 构建均启用 vendored OpenSSL。

### 4.6 Get Offsets

已支持：topic 正则、`topic:partition`、闭区间与开放区间 partition 范围、多个 pattern、exclude-internal、earliest/`-2`、latest/`-1`、max-timestamp/`-3`、Unix 毫秒 timestamp。pattern 与 partition 列表已对齐 Java `String.split` 丢弃末尾空项的语义；请求固定覆盖 `client.id=GetOffsetShell`。所有查询统一通过 librdkafka `ListOffsets` Admin API，逐 partition 错误不会中断其他结果，未知 offset `-1` 与原版一样不输出；结果包含 offset、broker timestamp 和错误列。

earliest-local/`-4`、latest-tiered/`-5`、earliest-pending-upload/`-6` 也已支持：librdkafka 2.12.1 的公开 ListOffsets Admin API 会拒绝小于 `-3` 的 offset，因此这三个 sentinel 使用协议客户端的 ListOffsets v11 fallback，并保留逐 partition 错误。常规 OffsetSpec 继续使用 librdkafka。输出采用统一 `comfy-table` 表格/JSON envelope，而非原版 `topic:partition:offset` 文本格式。

### 4.7 ACLs

已支持：list/add/remove；Topic、Group、Cluster、Transactional ID；单次命令可重复指定 topic/group/transactional-id，list 可重复指定 principal，并对重叠查询结果去重；Literal/Prefixed，以及查询和删除使用的 Any/Match；allow/deny principal 与各自 host 的笛卡尔积；常见 operations；producer、consumer、idempotent 快捷角色。add 与原版一样拒绝只适用于过滤器的 Any/Match；显式 operation 会按 Kafka `AclEntry.supportedOperations` 与各 resource type 预校验。add 执行前按资源查询已有 binding，只把缺失项交给 CreateAcls，重复执行返回 `ALREADY_EXISTS` 计数而不会创建重复记录。consumer-only 角色拒绝 cluster/transactional-id，producer+consumer 时允许这些 producer 资源。remove 指定 principal 时按 permission、host、operation 精确删除且 operation 默认 All；未指定 principal 时使用全 ACL entry filter，删除资源过滤器匹配的全部条目。原生子命令使用预览与 `--execute`；兼容脚本 `--add` 立即执行，`--remove --force` 接受原版 force 语义。Create、Describe 和 Delete 均通过 librdkafka Admin API 执行。

缺少或有差异：

- 未支持 Kafka 4.4 的 `--user-principal` 资源语义。
- librdkafka 2.12 的 ACL ResourceType 不包含 Delegation Token；使用 `--delegation-token` 时会明确返回不支持，而不会切换到另一套协议实现。
- librdkafka 2.12 的 ACL operation enum 不包含 Kafka 4.4 的 TwoPhaseCommit/CreateTokens/DescribeTokens；这些名称会返回明确能力错误。
- 未支持 `--bootstrap-controller`（ACL 走 librdkafka Admin，无独立 controller bootstrap 会话）。
- 原版 remove 会交互确认或使用 `--force`；本项目不做交互确认：原生形式使用 `--execute`，兼容入口接受 `--force`，未提供 force 时保持预览。
- 快捷角色当前只允许 allow principal/host，不允许 deny 组合。
- ACL FFI 使用窄范围 RAII 封装管理 Admin queue、options、event 和 native binding 生命周期。

### 4.8 Partition Reassignment

已支持：generate、execute、verify、cancel、list；topics-to-move 与 reassignment JSON；broker list；rack-aware/disable-rack-aware；log directory relocation。原生子命令提供预览与 `--execute`，兼容脚本的原版 `--execute`/`--cancel` 动作立即执行。execute 支持原版 `--additional` 的全局活动迁移保护、`--disallow-replication-factor-change` 的逐分区复制因子校验、`--throttle` 的 topic/broker leader/follower replication throttle，以及 `--replica-alter-log-dirs-throttle` 的 broker log-dir throttle。throttle 通过 librdkafka `IncrementalAlterConfigs` 写入，并把已有迁移和新增计划合并计算 source/destination replicas。verify 在全局 partition reassignment 与目标 log-dir move 均结束后自动清理 throttle；cancel 成功后也会清理；两者均支持 `--preserve-throttles` 跳过清理。

支持 `--bootstrap-controller` 作为 admin bootstrap 路由。生成算法目标与原版一致，但不承诺在相同输入下产生逐字节相同 assignment。

### 4.9 Delete Records

offset JSON file、请求校验、执行和结果输出均已实现。原生 `kafka delete-records` 先预览并要求 `--execute`；`kafka-delete-records.sh` 兼容入口会自动进入执行路径，与原版命令本身即执行的语义一致。

### 4.10 Leader Election

已支持 preferred、unclean；topic+partition、原版 `--path-to-json-file` 批量 partition 输入或 all-topic-partitions；预览与 `--execute`。批量输入会校验空列表、负 partition 和重复 topic-partition，执行统一使用 librdkafka `ElectLeaders` Admin API。

差异：本入口未暴露 `--bootstrap-controller`（原版 `LeaderElectionCommand` 主要经 broker Admin；本项目同样经 librdkafka `ElectLeaders`）。兼容入口接受已废弃的 `--admin.config` 并映射到 `--command-config`；原生 `kafka leader-election` 要求 `--execute`，预览使用统一表格/JSON 输出；`kafka-leader-election.sh` 兼容入口直接产生变更，与原版一致。

### 4.11 Log Dirs

已支持 broker list、topic list、log directory、partition size、offset lag 和错误展示。原生形式可直接运行 `kafka log-dirs`；兼容入口也接受原版必需的 `kafka-log-dirs.sh --describe` 动作 flag。

### 4.12 Broker API Versions

支持读取 broker API key 的 min/max version，可查询所有 broker 或指定 broker。原版主要只有 bootstrap-server 与 command-config；本项目额外提供结构化 JSON 输出。

### 4.13 Cluster

已支持 cluster ID、broker endpoints、API versions、unregister broker。Cluster ID 和默认 broker endpoints 通过 librdkafka `DescribeCluster` Admin API 获取；`list-endpoints --include-fenced-brokers` 协商 Kafka DescribeCluster v2，输出原版兼容的 STATE 与 ENDPOINT_TYPE 列，并在旧 broker 不支持 v2 时明确报错。

支持 `--bootstrap-controller` 作为 admin bootstrap 路由。`cluster-id`、`-b/--bootstrap-server`、`-c/--command-config`、废弃的 `--config` 别名、unregister 的 `-i/--id` 已与原版对齐。原生 unregister 仍要求 `--execute`；`kafka-cluster.sh unregister` 兼容入口按原版立即执行。

### 4.14 Client Metrics

已支持原版全部四个动作：list、describe、alter 和 delete。describe 可指定 `--name` 或枚举全部 subscription；alter 要求 `--name` 与 `--generate-name` 二选一，生成名称使用与 Kafka `Uuid.randomUuid()` 相同的 version 4 UUID、URL-safe Base64 无 padding、排除连字符表示规则。`--interval`、可重复 `--match` 和可重复 `--metrics` 均已实现；显式空值会按原版生成 DELETE 配置操作，非空 interval 按 Java `Integer.parseInt` 的 32 位整数边界校验。delete 先读取资源的现有动态配置，再逐项删除。

Client Metrics 使用 Kafka ListConfigResources API 74、DescribeConfigs API 32 和 IncrementalAlterConfigs API 44，并把写请求路由到 controller。原生 `kafka client-metrics alter/delete` 默认预览并要求 `--execute`；`kafka-client-metrics.sh --alter/--delete` 兼容入口按原版立即执行。结果通过 `comfy-table` 或 JSON envelope 输出，因此排版不是原版逐字符复制。

### 4.15 Features

已支持原版全部六个动作：describe、upgrade、downgrade、disable、version-mapping 和 feature-dependencies。describe 读取 broker supported feature、集群 finalized feature、epoch，并支持 `--node-id` 直连指定 broker；metadata.version level 会转换为 Kafka release/IV 名称。upgrade/downgrade 支持重复 `--feature name=level`、废弃的 `--metadata`、`--release-version`、安全/`--unsafe` downgrade 和 broker-side `--dry-run`；disable 支持重复 feature、安全/unsafe downgrade 语义。更新通过 controller 路由的 UpdateFeatures v0/v1 执行，并保留逐 feature 错误。

version-mapping 和 feature-dependencies 不要求 broker。其 metadata level 7–32、Kafka 3.3-IV3 到 4.4-IV1、六个 production feature 的 bootstrap/default level，以及 eligible.leader.replicas.version level 1 对 metadata.version level 23 的依赖，均按本报告 Kafka 4.4 基准源码固化并有单元测试。Kafka 后续增加 metadata version 或 feature 时需要同步更新该离线映射。

`--bootstrap-controller` 已作为 admin bootstrap 目标路由：与 `--bootstrap-server` 互斥，不再在执行前硬性返回 “native client does not expose” 占位错误。请求走同一 `protocol_admin`/`krafka` 客户端栈；若地址仅为纯 controller listener，连接/Metadata 阶段可能仍失败，错误来自真实客户端路径。`--bootstrap-server` 模式下 UpdateFeatures 仍会发现并直连 controller。输出统一使用 `comfy-table`/JSON。

### 4.16 Transactions

已支持原版全部六个动作：list、describe、describe-producers、abort、find-hanging 和 forceTerminateTransaction。list 会逐 broker 请求 ListTransactions 并保留原版 Coordinator 列，支持 duration-filter 以及 Kafka 4.1/KIP-1152 的 transactional-id-pattern；旧 broker 无法表达 pattern 时明确报错。describe 通过 FindCoordinator 与 DescribeTransactions 输出 coordinator、producer、状态、timeout、开始时间、持续时间及 topic-partition。

describe-producers 支持 leader 默认路由和 `--broker-id` 指定副本，输出六个原版 producer state 列。abort 支持 Kafka 3.0+ 的 start-offset 自动定位 producer state，也支持旧 broker 所需的 producer-id、producer-epoch、coordinator-epoch 完整组合；负 coordinator epoch 按原版保守归零，最终通过 WriteTxnMarkers 写入 ABORT marker。find-hanging 支持 topic/partition、broker replica 范围和最大事务 timeout，按原版将 DescribeProducers 候选与 List/DescribeTransactions coordinator 状态交叉核对。forceTerminateTransaction 使用 librdkafka transactional producer 初始化来 fence 同 transactional ID 的旧 producer，与 Kafka 4.4 AdminClient 内部复用 fenceProducers 的语义一致。

结果集统一使用 `comfy-table`/JSON。当前 Kafka 4.3.1 集成覆盖空 transaction list、指定 broker producer state 和 find-hanging 搜索；真正悬挂事务、显式 abort marker 与 fencing 的破坏性闭环仍需专门的隔离集成 fixture。

### 4.17 Metadata Quorum

已支持原版三个动作：`describe --status`、`describe --replication [--human-readable]`、`add-controller` 和 `remove-controller`。这里 add/remove 被统计为两个一级动作，因此入口共三个一级动作。DescribeQuorum 会协商 v0–v2，并按 Kafka 4.4 schema 解码 v1 timestamps 以及 v2 directory ID、error message、node/controller endpoints；不会用 v0 数据伪造新列。status 输出 cluster ID、leader/epoch/high watermark、最大 follower lag/lag time、当前 voters 和 observers；replication 输出原版七列，human-readable timestamp 会检查未来时间与 epoch 前时间，避免时钟漂移产生错误结果。

动态 voter 修改直接使用 Kafka AddRaftVoter API 80 v0/v1 和 RemoveRaftVoter API 81 v0，并连接当前 controller。add-controller 从原版同一 `--command-config` 读取 node.id、process.roles、metadata.log.dir/log.dirs、meta.properties directory.id、controller.listener.names、listeners/advertised.listeners；advertised listener 覆盖 bind listener，空 host 规范化为 localhost。remove-controller 支持 `-i/-d`、Kafka Base64 UUID 校验和 dry-run。所有输出使用 `comfy-table`/JSON。

支持 `--bootstrap-controller`：作为 bootstrap 目标传入同一协议客户端，不再预置 Unsupported 桩。纯 controller listener 仍可能在连接阶段失败（krafka 初始化可能依赖 Metadata），残差为真实客户端错误而非静默忽略。`--bootstrap-server` 模式会发现并连接 controller，describe 与动态 voter API 均有真实执行路径。Kafka 4.3.1 集成覆盖 status、replication v2 和 remove dry-run；真实 add/remove 会改变单节点测试集群 voter set，因此只在 codec/config 单元测试覆盖，后续需要专用多 controller fixture。

### 4.18 Delegation Tokens

已支持原版全部四个动作：create、renew、expire 和 describe，兼容入口会把原版 `--create/--renew/--expire/--describe` action flag 改写为原生子命令。所有动作与原版一样要求 `--command-config`，避免在没有安全身份配置时误导用户。principal 使用 `principalType:name` 校验；create 支持单 owner、重复 renewer 和 max-life-time-period，describe 支持重复 owner filter；owner override 需要并实际协商 CreateDelegationToken v3，不会在旧 broker 上静默丢弃。

create/describe 保留 Kafka 4 v3 的 OWNER 与 REQUESTER 差异、renewers、token ID、标准 Base64 HMAC 及三类时间；renew/expire 接受标准 Base64 HMAC。三个 period 参数均接受原版 `-1` sentinel，并拒绝小于 -1 的值；renew 的 `-1` 使用原始协议字段，避免被 Rust `Duration` 错误转换为零。输出通过 `comfy-table`/JSON，日期按原版分钟精度展示，同时 renew/expire JSON 保留毫秒 timestamp。

Kafka 的 delegation token 管理只在安全模式可用。当前 Kafka 4.3.1 PLAINTEXT 集成确认 describe 请求到达 broker 并被安全边界拒绝；完整 create→describe→renew→expire 数据闭环仍需新增 SASL 测试集群，因此认证矩阵是剩余测试缺口，不是命令动作或协议字段缺失。

### 4.19 Groups

已支持原版唯一动作 `--list`，兼容入口 `kafka-groups.sh --list` 会改写为原生 `kafka all-groups list`。无过滤时通过 ListGroups v5 枚举所有 broker 上的 Classic、Consumer、Share 和 Streams group，输出 GROUP、TYPE、PROTOCOL 三列；`--group-type` 接受原版四种类型且可与精确 `--protocol` 组合。`--consumer` 按原版同时匹配 Consumer type，以及 protocol 为 `consumer` 或空字符串的 Classic/simple group；`--share` 和 `--streams` 按 group type 匹配。三个快捷过滤器与 type/protocol 过滤器按原版互斥。

该入口不能复用 librdkafka 2.12 的 `ListConsumerGroups` 结果，因为公开 C API 的 group type 枚举只有 Classic 和 Consumer；实现使用现有 Kafka 协议客户端保留 v5 返回的未知新类型字符串，从而不会丢弃 Share/Streams。表格由 `comfy-table` 渲染，JSON 使用统一 envelope，因此仅内容语义对齐，不逐字符复制 Java 的列宽。

### 4.20 Share Groups

已支持原版全部五个动作：list、describe、delete、reset-offsets 和 delete-offsets。兼容入口 `kafka-share-groups.sh` 会将 `--list/--describe/--delete/--reset-offsets/--delete-offsets` 改写为原生子命令；原版 mutation 保持立即执行，原生入口继续提供预览与 `--execute` 安全边界。

- list 支持 `--state` 过滤和状态列，只返回 Share 类型 group。
- describe 支持重复 `--group` 或 `--all-groups`，以及原版 members/state/offsets 三种视图；成员视图包含 consumer ID、host、client ID、member epoch 和按 topic/partition 排序的 assignment，offsets 视图包含 start offset、leader epoch、Kafka 4.4 可用时的 lag 与逐 partition error。
- delete 支持重复 group 与 all-groups，并在执行前验证目标确为 Share Group 且已进入 Empty 状态。
- reset-offsets 支持单 group、重复 `--topic`/`--all-topics`、earliest/latest/absolute/current/datetime、无表头 CSV `--from-file`、dry-run/execute 和 CSV export；会结合 topic metadata、watermark、timestamp 与当前 share offset 生成计划。
- delete-offsets 支持单 group 与重复 topic，并返回逐 topic 结构化 mutation 结果。

Share Group Admin API 尚未由 librdkafka 2.12 的公开 C API 暴露，因此 list 复用 ListGroups，状态/成员使用 ShareGroupDescribe API 77，offset 描述、修改与删除分别使用 API 90、91、92。API 90–92 从 v0 起使用 flexible protocol；`krafka 0.14` 对未知新 key 默认旧 header，实现显式补齐 request/response tagged fields，并由 Kafka 4.3.1 真机测试验证，不依赖仅解析参数的假实现。所有非流式结果均经过统一 `comfy-table`/JSON 输出层。

Kafka 4.3.1 集成测试会创建真实 ShareConsumer 和 Share Group，验证 list、state/members/offsets describe，成员退出后的 Empty 状态，以及 reset、delete-offsets、delete 的执行闭环。Kafka 3.6.2 回归验证旧 broker 上空 Share Group 列表路径；Console Share Consumer 作为独立入口另有数据面闭环测试。

### 4.21 Streams Groups

已支持原版全部五个动作：list、describe、delete、reset-offsets 和 delete-offsets。兼容入口 `kafka-streams-groups.sh` 会将原版 action flag 改写为原生子命令；原版 mutation 保持立即执行，原生入口为 delete/delete-offsets 增加 `--execute` 保护，reset-offsets 保留原版明确的 dry-run/execute 选择。

- list 只返回 Streams 类型 group，支持 Empty、NotReady、Stable、Assigning、Reconciling、Dead 状态过滤。
- describe 支持重复 group 或 all-groups，以及 members、state、offsets、topology 四种视图；成员视图解码 active/standby/warmup task assignment，offsets 视图结合 committed offset 与 watermark 计算 lag，topology 在 Kafka 4.4 API v1 上输出 subtopology、source、repartition source、state changelog topic 和 copartition group。
- delete 支持重复 group/all-groups，并可删除单组或全部组的内部 topic；执行前验证目标是 Streams Group 且处于可删除状态。
- reset-offsets 支持重复 group/all-groups、input-topic/all-input-topics、CSV 导入导出，以及 earliest/latest/absolute/shift/current/datetime/duration 目标；可在执行 reset 时删除指定或全部内部 topic。
- delete-offsets 支持单 group、重复 input topic 或 all-input-topics，并按 topic 返回结构化结果。

StreamsGroupDescribe 是 Kafka 4.4 新增的 API 89，librdkafka 2.12 还没有公开对应接口。本项目因此为该 API 实现 v0/v1 协议编解码：v0 读取状态、成员和 task assignment，v1 增加完整 topology description；通用的 group 删除、offset 查询/修改/删除、topic metadata、水位和内部 topic 删除仍复用 rdkafka/librdkafka Admin API。所有非流式结果通过 `comfy-table` 或 JSON envelope 输出。

验证边界需要明确：Kafka 4.3.1 真机集成已实际发送 API 89 请求，并验证 broker 对不存在 group 返回可解析的 `GROUP_ID_NOT_FOUND`，因此覆盖了真实请求 header、group ID 数组、nullable topology 和响应 header；Kafka 3.6.2 覆盖旧 broker 上的空列表兼容路径。由于当前测试集群是 Kafka 4.3.1，Kafka 4.4 才提供的 v1 topology 由对齐 Kafka 4.4 schema 的 codec 单元测试覆盖；目前也没有在集成测试中启动真实 Kafka Streams 应用。因此五个动作都有执行路径，但真实成员/task/topology 及 mutation 闭环仍是该入口最重要的集成测试缺口。

### 4.22 Streams Application Reset

已支持 Kafka 4.4 原版单动作工具的主要执行语义：`--application-id`、逗号分隔的 `--input-topics`/`--intermediate-topics`/`--internal-topics`，以及互斥的 earliest（默认）、latest、absolute offset、shift-by、datetime、ISO-8601 duration 和三列 CSV reset plan。输入 topic offset 会按各 partition 的 log start/end 边界调整；intermediate topic 始终 seek 到 log end。offset 写入使用 librdkafka `AlterConsumerGroupOffsets`，内部 topic 删除使用 librdkafka Admin API；结果统一输出 `comfy-table` 表格或 JSON envelope。

执行前会检查 application ID 对应 group 是否仍有 active member；默认拒绝，`--force` 通过协调器 LeaveGroup 请求移除成员后继续。该 force 行为在 dry-run 下也会发生，与 Kafka 原版一致。不存在的 group 被视为 inactive，仍可建立 reset 计划。未显式提供 internal topic 时，按照 Kafka `StreamsResetter.matchesInternalTopicFormat` 的 application ID 前缀和 changelog/repartition/subscription/FK-join 后缀规则推断，同时排除用户声明的 input/intermediate topic；显式列表必须属于可推断集合。只有 offset reset 成功后才删除内部 topic，逐资源错误进入结构化结果并以部分失败退出。

兼容入口 `kafka-streams-application-reset.sh` 保留原版无 `--dry-run` 时立即执行的不可逆语义，并接受原版默认 `localhost:9092`、废弃的 `--config-file` 和 `--intermediate-topics`。输出排版及错误文案不做逐字符复制；当前实现也不输出 Java 工具的弃用 warning。Kafka 3.6.2 集成验证 dry-run 规划，Kafka 4.3.1 集成验证 offset 预览、内部 topic 推断与实际删除，以及 active group 的 `--force --dry-run` LeaveGroup 路径。

### 4.23 Console Share Consumer

已支持 Kafka 4.4 原版的数据面与主要参数：必需 topic、默认 `console-share-consumer` group/client ID、显式 group、max-messages、累计空闲 timeout、formatter/config/property、StringDeserializer、systest shutdown marker，以及默认 ACCEPT、`--release`、`--reject` 和 `--reject-message-on-error`。Share group 强制使用 explicit acknowledgement mode，每批记录在下一次 poll 前通过 ShareAcknowledge 提交；delivery count、leader epoch、headers、key/value 与 timestamp 均进入共享的 native DefaultMessageFormatter，另提供 JSON 流扩展。

该路径使用 `krafka 0.14` 的 KIP-932 ShareHeartbeat、ShareFetch 和 ShareAcknowledge 实现，因为 librdkafka 2.12/rdkafka 尚未提供 Share Consumer 数据面。标准 properties 文件与重复 `--command-property` 遵循“命令行覆盖文件”，group ID 从三处提供时必须一致；当前会映射认证、client/group ID、request/session/heartbeat timeout、max.poll.records、fetch wait、metadata age/idle 和 client rack。其他 Java consumer property 尚不能全部下推，因此评级为部分支持；任意 Java formatter/deserializer 插件同样无法由原生二进制加载并会明确报错。

Kafka 4.3.1 集成测试先为 Share group 配置 `share.auto.offset.reset=earliest`，启动本 CLI 并确认 group 注册，再实时生产消息，实际验证 ShareFetch、delivery count 格式化与 ACCEPT ShareAcknowledge 闭环。该 broker-side group 配置是必要的测试前置：Kafka Share group 默认 reset 策略是 latest，不能通过错误地预置旧消息来验证 consumer。Kafka 3.6.2 不支持 KIP-932 数据面，仅作为其余命令的回归矩阵。

### 4.24 Verifiable Share Consumer

已对齐 Kafka 4.4 `VerifiableShareConsumer` 的单动作接口：必需 topic/group ID、`--max-messages`、`--verbose`、`--acknowledgement-mode auto|async|sync`、`--ack-pattern` 和 `--offset-reset-strategy`。兼容入口为 `kafka-verifiable-share-consumer.sh`；输出采用原版系统测试所需的 JSON Lines 事件，包括 `startup_complete`、`offset_reset_strategy_set`、`record_data`、`records_consumed`、`offsets_acknowledged`、`shutdown_requested` 和 `shutdown_complete`。partition 汇总稳定排序并包含 count/offsets，verbose 事件保留 topic、partition、offset、key 与 value。

ack pattern 按 record offset 循环选择 `accept`、`release`、`reject`、`renew`；与原版一致，pattern 只能用于 sync/async，并拒绝仅含 renew 的无限续期模式。显式 pattern 走 ShareAcknowledge，offset reset 在启动 consumer 前通过 Share Group 的 `share.auto.offset.reset` 增量配置完成。认证、client ID、request timeout 与 max.poll.records 复用 Share Consumer 配置映射。

该入口评级为部分支持：`krafka 0.14` 目前只公开同步 acknowledgement completion，因此 CLI 接受原版 async 模式并保持事件契约，但底层完成等待仍同步执行；auto 模式在退出前也会同步完成当前批次，以避免 max-messages 到达时丢失 acknowledgement。原版 Java client 的全部 consumer property 仍未全部映射。Kafka 4.3.1 CI 会创建真实 Share Group、设置 earliest、消费预置消息，并断言 verbose data、partition summary、ACCEPT 计数及完整 shutdown 事件链。

### 4.25 Share Consumer Performance

已对齐 Kafka 4.4 `ShareConsumerPerformance` 的主要执行与输出契约：必需 topic 和 `--num-records`，兼容废弃的 `--messages`；默认 group/client ID；`--threads` 创建多个 Share Consumer 并为 client ID 追加序号；按 key+value 原始字节累计吞吐；`--timeout` 按每个 consumer 最后一次收到记录的时间退出；支持 reporting interval、detailed stats、per-consumer stats、hide header、command properties/config、废弃 consumer.config 和 print metrics。停止后先取得 metrics snapshot、同步提交 acknowledgement，再输出 group 汇总并关闭所有 consumer，与 Java 工具的生命周期顺序一致。

默认输出保留原版性能服务依赖的七列 CSV：start/end time、总 MB、MB/s、records/s、总 records 和 fetch time；周期输出追加 `for share consumer N`，逐 consumer 汇总保留原版文本前缀。该输出是性能测试机读协议，不属于管理结果表，因此没有改成 `comfy-table`。日期格式原生覆盖默认 `yyyy-MM-dd HH:mm:ss:SSS` 及相同 token 集合；Java `SimpleDateFormat` 的全部 quoting/locale/timezone 语法不宣称等价。

该入口评级为部分支持：`krafka 0.14` 的公开 Share Consumer builder 尚未暴露 `fetch.max.bytes` 和 socket receive buffer，因此 `--fetch-size`/`--socket-buffer-size` 当前完成输入校验但不能下推到 transport；`--print-metrics` 输出连接创建/关闭/错误、活跃连接和请求数，而不是 Java client 的完整 `MetricName` 集合。Kafka 4.3.1 CI 使用两个 Share Consumer 共同消费四条真实消息，断言总记录数精确为 4、逐 consumer 输出、连接 metrics、commit/close 和最终 CSV 汇总。

### 4.26 Consumer Performance

已按 Kafka 4.4 `ConsumerPerformance` 实现普通消费者性能入口：`--topic` 与 `--include` 严格二选一，`--num-records` 与废弃的 `--messages` 严格二选一；支持随机默认 group、显式 group、`--from-latest`、fetch size、socket receive buffer、reporting interval、detailed stats、timeout、hide header、date format、command config/property、废弃 consumer.config 和 print metrics。配置优先级与原版一致：显式性能参数覆盖 command property，command property 覆盖配置文件；fetch/socket 参数实际映射到 librdkafka 的 `max.partition.fetch.bytes` 与 `socket.receive.buffer.bytes`，数据量按 key+value 原始字节计算。

默认汇总与 detailed stats 均保留原版十列 CSV 契约：开始/结束时间、数据量与 MB/s、记录数与 records/s、rebalance 时间、fetch 时间及扣除 rebalance 后的 fetch 吞吐。该 CSV 是 Kafka 性能测试的机读输出，不改用 `comfy-table`；管理结果仍统一通过表格类库展示。topic 模式和 librdkafka 正则订阅均有真实 broker 测试，Kafka 3.6.2 验证 topic 消费，Kafka 4.3.1 验证 include、精确记录数及 metrics 输出。

该入口评级为部分支持：`--print-metrics` 当前输出本次运行测得的 records、bytes 与 assignment 指标，不等价于 Java consumer 的完整 `MetricName` 集合；日期格式只覆盖本项目已实现的常见 `SimpleDateFormat` token，不承诺 quoting、locale、timezone 的全部行为；rebalance 时间由 librdkafka assignment 状态变化推断，而非 Java rebalance listener 回调的逐时刻完全等价实现。核心消费、配置、超时、统计与 CSV 语义已落地，但上述差异必须保留在兼容性声明中。

### 4.27 Producer Performance

已按 Kafka 4.4 `ProducerPerformance` 实现原版单动作入口。topic、正数 num-records 和 fractional throughput 均保留；payload 严格要求 record-size、UTF-8 payload-file、payload-monotonic 三选一，payload delimiter 使用正则并处理默认 `\\n`、内部空记录和末尾 delimiter。随机 payload、文件随机选择与 random key 复用同一个原版顺序的 `SplittableRandom` 状态，默认 seed 0 的序列已与 Java 实际输出做单元测试对齐；key distribution 支持 none、range、random 及正数 record-key-range 校验。

执行路径使用 `rdkafka`/librdkafka `BaseProducer` 和自定义 `ProducerContext` delivery callback，不会因逐条 await 把性能测试串行化。callback 只统计成功 delivery 的 payload bytes 和端到端 latency，最多采样 500,000 条延迟，并输出原版周期统计及最终 records/sec、MB/sec、平均/最大延迟和 50/95/99/99.9 percentile 文本。warmup 记录不进入第二条 steady-state summary；异步队列满时 poll 后重试；throughput `-1` 禁用限速，其他有限正数按累计目标速率节流。

事务可由 transactional-id、transaction-duration-ms 或 producer property 任一方式启用；显式 transactional-id 覆盖 property，未提供 ID 时生成 Kafka URL-safe UUID 风格 ID，默认 transaction duration 为 3000ms。实现调用 init/begin/commit transaction，结束时提交未满 transaction、flush 全部 delivery，再打印统计/metrics。bootstrap-server 覆盖 property/config；command property 覆盖 command config；废弃 producer-props/producer.config 与新参数严格互斥。Kafka 3.6.2 测试覆盖 monotonic payload 和 metrics，Kafka 4.3.1 测试覆盖事务、record-size、range key 与 metrics。

该入口评级为部分支持：`--print-metrics` 当前输出本次运行测得的 records、bytes 和 delivery errors，不等价于 Java producer 的完整 `MetricName` 集合；payload delimiter 由 Rust regex 执行，与 Java `Pattern` 的 look-around、反向引用等完整方言不等价；错误堆栈和浮点格式只保持信息/字段兼容，不做 JVM 逐字符复制。性能输出属于 Kafka 原版服务消费的文本契约，因此不改为 `comfy-table`；管理命令的结果集继续统一使用表格类库。

### 4.28 End-to-End Latency

已按 Kafka 4.4 `EndToEndLatency` 实现单记录往返延迟入口：必需 bootstrap-server、topic、正数 num-records、producer-acks `1|all` 和非负 record-size；支持非负 record-key-size、num-headers、record-header-key-size，以及允许 `-1` 表示 null header value 的 record-header-size。兼容脚本同时接受 Kafka 4.2 起的 named 参数和已废弃的五/六个 positional 参数，并为后者输出弃用警告。

执行时首先用 librdkafka metadata 检查 topic；缺失时创建单分区、复制因子 1 的 topic。consumer 使用临时 group、关闭自动提交、latest reset、零 fetch wait，并在发送前把所有分区显式定位到当前 high watermark，消除首次 position 解析晚于发送导致漏读的竞态。每轮由 Java `Random(0)` 对齐实现生成 ASCII key/value/ordered headers，FutureProducer 等待 delivery acknowledgement 后立即消费一条消息，逐 byte 校验 key/value/header key/header value/null value，再记录包含生产确认与消费的总 elapsed latency。结束时同步提交 consumer state，输出原版进度行、平均延迟和 50/99/99.9 percentile 文本。

该入口评级为已支持：核心往返与校验语义完整；默认 key size 0、ASCII byte 比较更严、全局 `--command-config` 必填路径、Rust error chain 与 JVM stack 差异为有意差异。

### 4.29 Verifiable Producer

已按 Kafka 4.4 `VerifiableProducer` 实现 system-test JSON Lines 入口。必需 topic；`max-messages=-1` 与 `throughput=-1` 分别表示无限生产和不限速，acks 接受 `-1/0/1`。value 默认使用递增序号，也可输出 `value-prefix.index`；repeating-keys 按 `0..limit-1` 循环。`message-create-time` 按原版累计已运行时间推进 record timestamp。废弃的 `--producer.config` 可单独使用，并与全局 `--command-config` 互斥；属性文件在 bootstrap、acks 和 retries 默认值之后应用，保留 Kafka 源码实际存在的覆盖顺序。

数据面使用 `rdkafka`/librdkafka `FutureProducer` 异步 delivery，不串行等待每条消息。运行期间持续回收已完成任务，避免无限模式积累 task handle；收到终止信号或达到消息上限后等待全部 delivery 完成。输出依次包含 `startup_complete`、每条消息的 `producer_send_success` 或 `producer_send_error`、`shutdown_complete` 和 `tool_data`，成功事件带 key/value/topic/partition/offset，最终统计带 sent、acked、target throughput 与平均实际吞吐。Kafka 3.6.2/4.3.1 集成路径分别覆盖有限消息、value prefix、重复 key、create time 和完整事件链。

该入口评级为已支持：事件字段和成功路径语义已对齐；Rust/librdkafka 错误事件的 exception 类名不是 Java FQCN、吞吐计时与 JVM 调度差异为有意差异；任意 Java producer metrics 不属于该工具事件契约。JSON Lines 是原版 systest 协议。

### 4.30 Verifiable Consumer

已按 Kafka 4.4 `VerifiableConsumer` 实现 system-test JSON Lines 入口。支持 topic、group-id、classic/KIP-848 group protocol、static membership、max-messages、session-timeout、enable-autocommit、close-timeout、reset-policy、assignment-strategy，以及废弃的 `--consumer.config`。输出包含 startup、assignment/revocation、records_consumed、可选 record_data、offsets_committed 与 shutdown 事件。Kafka 3.6.2/4.3.1 集成路径覆盖有限消息与 verbose 记录闭环。

该入口评级为已支持：事件字段与核心配置语义已落地；Java assignor 全限定名映射到 librdkafka 策略名，异常类名与 rebalance 回调精度为有意差异。

### 4.31 Replica Verification

已按 Kafka 4.4 `ReplicaVerificationTool` 实现原版单动作入口。支持 `--broker-list`（兼容脚本会改写到全局 bootstrap）、fetch-size、max-wait-ms、topics-include 正则、time（-1 latest / -2 earliest / 绝对时间戳）和 report-interval-ms。实现按 broker 并发 Fetch（`replica_id=-2` debugging consumer）、解析 v2 record batch 的 last offset 与 CRC，比较各 replica 在 high watermark 之下的一致性，并按原版文本格式输出启动行与 max lag 摘要。offset 不匹配时立即退出；checksum 不匹配时打印诊断并继续，与原版一致。

该入口评级为部分支持：核心校验与报告语义已实现，原版多线程 barrier 改为异步并发 Fetch；Fetch 使用 krafka 协议路径，鉴权矩阵与 krafka 已知限制一致；单 broker RF=1 集成可验证启动与 lag 报告，完整多 replica 不一致场景依赖多 broker fixture。

### 4.32 Dump Log

已按 Kafka 4.4 `DumpLogSegments` 实现本地段文件转储。支持 `--files`（逗号分隔）、`--print-data-log`、`--deep-iteration`、`--skip-record-metadata`、`--max-bytes`、`--max-message-size`、`--verify-index-only`、`--index-sanity-check`，以及仅 `StringDecoder` 的 key/value decoder class。`.log` 输出 batch 级 baseOffset/lastOffset/count/producer/compression/crc 与 deep 记录行；`.index` 输出 offset/position 并可对照 `.log` 校验；`.timeindex` 输出 timestamp/offset；`.txnindex` 按 AbortedTxn v0（34 字节 version-prefixed 记录）打印 producerId/first/last/lastStableOffset；producer `.snapshot`（无 epoch 横线的 offset.snapshot 命名）按 ProducerSnapshot v1 打印 producer 表。KRaft `offset-epoch.snapshot` 仍走 record-batch dump 路径。不需要 bootstrap-server。

控制记录已按 Kafka `ControlRecordType` 解析 key 中的 type id：`ABORT`/`COMMIT` 打印 `endTxnMarker` 与 `coordinatorEpoch`；`LEADER_CHANGE` 打印 leaderId 前缀；`SNAPSHOT_HEADER`/`SNAPSHOT_FOOTER`/`KRAFT_VERSION`/`KRAFT_VOTERS` 打印命名字段或 version 前缀。完整 Java `*JsonConverter` 输出不做逐字符复刻。

该入口评级为部分支持：offsets/transaction/cluster-metadata/remote-log/share-group-state 专用 decoder 明确 unsupported；producer snapshot 仅支持 v1；控制记录 JSON 细节为有意简化。

### 4.33 Storage

已按 Kafka 4.4 `StorageTool` 实现子命令：`random-uuid`、`info`、`format`、`version-mapping`、`feature-dependencies`（后两者指向/别名离线 features 表）。`format` 要求 KRaft `process.roles`，写入 v1 `meta.properties`（version/cluster.id/node.id/directory.id），支持 `--ignore-formatted` 与 controller quorum 参数校验。对 **controller 的 metadata 目录** 额外写入：
- `__cluster_metadata-0/` 元数据分区目录（KRaft 布局占位）
- `kafka-cli-bootstrap.residual.json`（`partial-native-bootstrap-v1` 残差标记）

**安全约束**：Kafka 保留名 `bootstrap.checkpoint` 仅允许 **二进制** BatchFileReader 内容。本工具**绝不**用该名写入 JSON/文本；若目录中已存在文本形态的 `bootstrap.checkpoint`，`format` 会拒绝并要求清理。Kafka 在该文件缺失时会回退默认 bootstrap 元数据。完整 `RecordsSnapshotWriter` 快照未生成。`--add-scram` 明确 unsupported。纯 broker `format` 只写 `meta.properties`。

`dump-log` 对文本/JSON 的 `bootstrap.checkpoint` 或 `kafka-cli-bootstrap.residual.json` 输出残差诊断，而不是按 record batch 解析（避免 “invalid bytes at offset 0”）。

该入口评级为部分支持：生产 controller 引导仍可能需要官方 `kafka-storage` 生成完整 snapshot；动态 quorum 初始 voter 物化、SCRAM 引导记录未实现；`info` 覆盖 meta.properties 一致性检查。

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

1. `rdkafka`：主要 API。它是 librdkafka 的安全 Rust 封装，并非另一套 Kafka 实现；普通 Consumer/Producer/Verifiable Consumer Performance、metadata 与 watermark 查询也直接使用该路径。
2. `rdkafka-sys`：只包装 `rdkafka` 尚未暴露的 librdkafka Admin API，例如 leader election、部分 group offset/config API。
3. `krafka`：当前仍用于 API versions、unregister、部分 reassignment/log-dir 协议路径，以及 librdkafka 无法表达的 tiered OffsetSpec、全类型 Groups 列表、Share Group Admin API、Console/Verifiable/Performance Share Consumer 数据面、Replica Verification 的 per-broker Fetch（`replica_id=-2`）、StreamsGroupDescribe API 89、Kafka 4.4 Assigning/Reconciling group state 与 consumer protocol epoch。ACL 已完成迁移。为确保认证、重试、协议协商和错误行为一致，应继续迁移可由 librdkafka 表达的路径，必要协议 fallback 则保留清晰边界。

认证审计发现，`krafka 0.14.0` 的内部 transport 具备 SCRAM-over-TLS，但公开 `AuthConfig` API 不能从 Kafka properties 构造“SCRAM + 自定义 TLS 配置”；本项目因此不会通过访问私有字段伪装支持。当前独立协议路径明确支持 PLAINTEXT、SSL、SASL_PLAINTEXT 的 PLAIN/SCRAM 和 SASL_SSL 的 PLAIN；SASL_SSL + SCRAM 仍是已知缺口。librdkafka 路径不受此限制。

不建议全量直接使用 `rdkafka-sys`：它与 `rdkafka` 使用同一个 librdkafka，但会把 C 指针、回调、队列和资源生命周期全部暴露为 `unsafe`，不会获得额外协议权威性。

## 7. 测试与验证现状

本地验证：

- `cargo fmt --check`：通过。
- `cargo clippy --all-targets --locked -- -D warnings`：通过。
- Rust 单元测试与普通 CLI 测试：312 个通过（275 library + 30 CLI + 7 offline_tools）。
- `tests/offline_tools.rs`：真实 binary 覆盖 storage format/info/random-uuid、dump-log 段文件/控制记录/txnindex/snapshot/残差诊断（不依赖 Docker）。
- Kafka 4.3.1 Docker：既有单体 `all_command_families` 矩阵（**33 个入口中 31 个在线命令**；`dump-log`/`storage` 仅 offline），外加 `tests/kafka_4_depth_integration.rs` 的 **11** 个聚焦套件（bootstrap-controller、features/offsets/cluster、groups CSV、ACL+configs、transactions/delete-records、quorum/election、produce/consume/perf/e2e、reassign+replica-verification、share-groups/share-consume/share-perf、streams+all-groups、client-metrics+verifiable）。本地已全绿（`--features bundled -- --ignored --test-threads=1`）。
- Kafka 3.6.2 真实进程集成测试：覆盖协议和 Admin 兼容边界（需 `KAFKA_36_HOME` 指向 3.6.2 发行包；本机下载至 `.kafka/kafka_2.13-3.6.2`）。
- **本地跑集成（与 CI 一致）**：必须 `--features bundled`（SCRAM 需要 OpenSSL）；Kafka 4 Docker 镜像 `apache/kafka:4.3.1`；fixture 已设 transaction RF=1、share coordinator RF=1；单体矩阵另关 auto-create topics 以覆盖 e2e 显式建 topic 文案。
- GitHub Actions workflow 经 `actionlint` 校验通过。

测试仍有不足：

- 集成测试是单 broker，不能充分验证多 broker reassignment、rack awareness、ISR 变化、failover，以及 Replica Verification 的跨 replica 不一致场景。
- 未覆盖 TLS、mTLS、SASL/PLAIN、SCRAM、OAuth/OIDC 和 Kerberos 组合。
- 未覆盖真实 ARM64 机器运行；ARM64 当前只由 CI 交叉编译。
- 没有逐个原版选项的 golden test，也没有与 Java CLI 输出做逐命令差分测试。
- ACL 快捷角色和复杂 deny/host/prefix 组合仍需更完整矩阵。
- 尚未用真实 Kafka Streams 应用验证 Streams Group 成员/task/topology 和五动作闭环；Kafka 4.4 topology v1 当前只做协议 codec 单元测试。
- Streams Application Reset 已用普通 topic 与真实 active consumer 验证管理闭环，但尚未用 Kafka Streams runtime 产生的完整 topology/状态目录做端到端差分测试。

## 8. CI 与发布目标

CI workflow 包含：

- fmt、Clippy、单元测试。
- bundled glibc release build。
- Kafka 4.3.1 集成测试。
- Kafka 3.6.2 集成测试。
- `x86_64-unknown-linux-musl` 静态构建及 artifact。
- `aarch64-unknown-linux-musl` 静态交叉构建及 artifact。

Metadata Quorum 实现 `1c482f0` 及报告提交 `0262300` 已由 GitHub Actions
[`30854327394`](https://github.com/lihongjie0209/kafka-cli/actions/runs/30854327394) 完整验证通过：fmt/Clippy/194 个普通测试、bundled glibc、Kafka 3.6.2、Kafka 4.3.1、x86_64 musl 和 aarch64 musl 六个 job 全绿。Kafka 4.3.1 实际验证 DescribeQuorum v2 status/replication 和 remove dry-run。Delegation Tokens 实现 `25a4cd2` 及报告提交 `3de048f` 又由 GitHub Actions [`30854931372`](https://github.com/lihongjie0209/kafka-cli/actions/runs/30854931372) 完整验证通过：fmt/Clippy/201 个普通测试、bundled glibc、Kafka 3.6.2、Kafka 4.3.1、x86_64 musl 和 aarch64 musl 六个 job 全绿，Kafka 4.3.1 实际验证 PLAINTEXT 安全拒绝边界。两轮的两种 musl artifact 均执行 smoke test，ARM64 通过 QEMU user-mode emulator 启动；musl 没有在本地构建。

Kafka Groups 实现 `00b229f` 由 GitHub Actions [`30855676975`](https://github.com/lihongjie0209/kafka-cli/actions/runs/30855676975) 完整验证通过：fmt/Clippy/209 个普通测试、bundled glibc、Kafka 3.6.2、Kafka 4.3.1、x86_64 musl 和 aarch64 musl 六个 job 全绿。Kafka 4.3.1 实际验证 Classic group 枚举、consumer 包含和 share 排除过滤；Kafka 3.6.2 验证旧 broker ListGroups 兼容路径。

Kafka Share Groups 实现 `109765a` 与 flexible protocol 修复 `d207762` 由 GitHub Actions [`30857219313`](https://github.com/lihongjie0209/kafka-cli/actions/runs/30857219313) 完整验证通过：fmt/Clippy/216 个普通测试、bundled glibc、Kafka 3.6.2、Kafka 4.3.1、x86_64 musl 和 aarch64 musl 六个 job 全绿。Kafka 4.3.1 创建真实 ShareConsumer，验证 list、state/members/offsets describe、reset-offsets、delete-offsets 与 delete 闭环；Kafka 3.6.2 验证旧 broker 空列表兼容路径。

Kafka Streams Groups 实现 `1ae5e53` 及协议修复 `e425098`、`97e67a5` 由 GitHub Actions [`30859766154`](https://github.com/lihongjie0209/kafka-cli/actions/runs/30859766154) 完整验证通过：fmt/Clippy/225 个普通测试、bundled glibc、Kafka 3.6.2、Kafka 4.3.1、x86_64 musl 和 aarch64 musl 六个 job 全绿。Kafka 4.3.1 实际验证 API 89 请求及缺失 group 的结构化错误响应，Kafka 3.6.2 验证空列表兼容路径；Kafka 4.4 v1 topology 当前由 schema 对齐 codec 测试验证，尚无真实 Streams 应用 fixture。

Streams reset/delete 二次语义修复 `d549140` 由 GitHub Actions [`30860540837`](https://github.com/lihongjie0209/kafka-cli/actions/runs/30860540837) 再次完整验证通过：226 个普通测试及其余五个 job 全绿。修复包括按 Kafka 原版允许不存在 group 生成 reset 计划、消除空 inactive group CSV panic、只在 group 删除成功后删除内部 topic、保留内部 topic 删除错误，以及移除协议解码错误中的原始 Base64 payload；Kafka 4.3.1 集成测试实际验证缺失 group 的 reset dry-run。

Streams Application Reset 实现 `db96a16` 及测试修正 `e4c3730` 由 GitHub Actions [`30861552986`](https://github.com/lihongjie0209/kafka-cli/actions/runs/30861552986) 完整验证通过：231 个普通测试、bundled glibc、Kafka 3.6.2、Kafka 4.3.1、x86_64 musl 和 aarch64 musl 六个 job 全绿。Kafka 3.6.2 验证 reset 预览；Kafka 4.3.1 验证内部 topic 推断/删除和 active member force LeaveGroup。两种 musl 均只在 CI 构建并执行启动 smoke test。

Console Share Consumer 实现 `d455ee6`、空闲 timeout 修正 `fff9310` 及集成 fixture 修正 `ffbcdb4` 由 GitHub Actions [`30863139169`](https://github.com/lihongjie0209/kafka-cli/actions/runs/30863139169) 完整验证通过：236 个普通测试、bundled glibc、Kafka 3.6.2、Kafka 4.3.1、x86_64 musl 和 aarch64 musl 六个 job 全绿。Kafka 4.3.1 实际验证运行中的 Share Consumer 接收实时消息、输出 delivery count 并提交 ACCEPT acknowledgement；Kafka 3.6.2 保持全量回归。两种 musl 仍只在 CI 构建和 smoke test。

Verifiable Share Consumer 实现 `43c185c` 由 GitHub Actions [`30863964909`](https://github.com/lihongjie0209/kafka-cli/actions/runs/30863964909) 完整验证通过：240 个普通测试、bundled glibc、Kafka 3.6.2、Kafka 4.3.1、x86_64 musl 和 aarch64 musl 六个 job 全绿。Kafka 4.3.1 实际验证 offset reset、verbose record、records consumed、ACCEPT acknowledgement 计数和 shutdown 完整事件链；两种 musl 均只在 CI 构建，x86_64 原生、aarch64 经 QEMU 实际启动并上传 artifact。

Share Consumer Performance 实现 `13e7d7d` 由 GitHub Actions [`30865007900`](https://github.com/lihongjie0209/kafka-cli/actions/runs/30865007900) 完整验证通过：244 个普通测试、bundled glibc、Kafka 3.6.2、Kafka 4.3.1、x86_64 musl 和 aarch64 musl 六个 job 全绿。Kafka 4.3.1 实际创建两个 Share Consumer 共享两分区负载，验证四条消息的精确总数、逐 consumer 统计、连接 metrics、最终 CSV 与 acknowledgement/close；musl 仍只在 CI 构建和启动验证。

Consumer Performance 实现 `a92cb50` 及 Kafka 3 `max-timestamp` 兼容断言修正 `7ab4fc1` 由 GitHub Actions [`30867465950`](https://github.com/lihongjie0209/kafka-cli/actions/runs/30867465950) 验证：248 个普通测试、bundled glibc、Kafka 3.6.2、Kafka 4.3.1、x86_64 musl 和 aarch64 musl 六个 job。Kafka 3.6.2 实际验证 topic 模式与十列统计，Kafka 4.3.1 验证 include、精确记录数及 native metrics；两种 musl 只在 CI 构建并执行启动 smoke test。

Producer Performance 实现 `fc39cca` 已提交 GitHub Actions [`30868691184`](https://github.com/lihongjie0209/kafka-cli/actions/runs/30868691184)：workflow 包含 257 个普通测试、bundled glibc、Kafka 3.6.2 monotonic producer、Kafka 4.3.1 transactional producer，以及 x86_64/aarch64 musl 构建和启动 smoke test。按当前开发策略，功能提交不阻塞等待单轮 CI；各轮 CI 状态和失败项将在后续集中审计与修复，不能仅凭 workflow 已触发宣称本轮六项全绿。

End-to-End Latency 实现 `1ab2cfc` 已提交 GitHub Actions [`30869292636`](https://github.com/lihongjie0209/kafka-cli/actions/runs/30869292636)：workflow 新增 Kafka 3.6.2 基础往返，以及 Kafka 4.3.1 自动建 topic、key、两个 null-value header 往返验证；普通测试增至 263。该轮同样不阻塞功能开发等待结果，后续集中处理 CI 状态。

Verifiable Producer 实现 `350c363` 已推送：本地原生目标通过 245 个 library tests、23 个 CLI tests、全特性 Clippy、格式和 shell 语法检查；Kafka 3.6.2/4.3.1 集成路径已加入现有 workflow。依照当前策略，本轮不查询或等待远程 CI，运行状态与失败项留待后续批量审计。

Verifiable Consumer 实现 `ddba17a` 已推送：新增 classic/KIP-848 protocol、assignment/revocation 与 records/commits JSON Lines 事件；Kafka 3.6.2/4.3.1 集成路径已加入。按当前策略不等待远程 CI。

Replica Verification 实现（本轮工作树）已本地验证：253 个 library tests、25 个 CLI tests、全特性 Clippy 与格式检查通过；Kafka 3.6.2/4.3.1 集成路径覆盖启动行与 max lag 报告。按策略不阻塞等待远程 CI，状态后续批量审计。

musl 构建只在 CI 内进行，使用 Rust 1.88、固定 Zig 0.15.2 和 `cargo-zigbuild`。x86_64 musl 二进制面向 CentOS 7 等旧 glibc 环境时不依赖目标机器 glibc；ARM64 musl artifact 用于 ARM64 Linux。最终兼容性仍应在对应架构机器或容器中执行 smoke test，而不能只以 `file` 输出判断。

## 9. 建议的后续优先级

### P0：统一客户端与保证现有功能可靠

1. API versions、unregister、reassignment/log-dir 和 Kafka 4.4 新 group state 等能力在 librdkafka 2.12 中没有可用公开接口或枚举；继续把 `krafka` 限制在这些必要路径，并统一配置、鉴权、错误与生命周期边界。只有上游公开稳定 C API 后才迁移到 `rdkafka-sys` RAII 封装，避免调用 librdkafka 私有符号。
2. 增加 TLS/SASL 集成测试。
3. 增加多 broker Kafka 4 集成环境，验证 reassignment、leader election、ISR 和 rack-aware 分配。
4. 已完成：CI 对两个 musl artifact 执行 `--help`/`--version` smoke test；ARM64 使用 QEMU user-mode emulator 实际启动。

### P1：补齐已支持脚本的主要差距

1. Configs/features/metadata-quorum/cluster/reassign 已支持 `--bootstrap-controller` 路由（与 server 互斥）；纯 controller-only 会话的 Metadata 兼容性仍依赖真实连接结果。
2. Reassignment 可补多 broker 长时间迁移的限流差分测试。
3. 若 librdkafka 后续暴露对应 Admin option，将 Topics 已接受的 `partition-size-limit-per-response` 下推为实际单响应 partition 上限。

### P2：扩大原版工具覆盖面

客户端管理/数据/本地段与 storage 入口已基本齐全（33/44）。后续工作优先放在：已实现入口的深度兼容（bootstrap-controller、SASL 全矩阵、多 broker reassignment/replica 不一致）、`storage format` 的完整 bootstrap.checkpoint，以及 dump-log 专用 decoder（若需要）。**不再规划** Connect、server start/stop、run-class、JMX、Trogdor 与完整交互式 metadata-shell——见 §3.2 范围外清单。

## 10. 最终评估

当前项目适合作为轻量、可静态分发的 Kafka 日常管理 CLI，尤其适用于 Topic、offset、基础 Consumer Group、Transactions、ACL、记录删除和集群信息查询。它已经具备跨 Kafka 3.6/4.3 的实测基础，但对于“替换 Kafka 发行包全部 Bash 脚本”这一目标仍不完整。

在对外发布时，建议使用“兼容 33 个 Kafka CLI 入口的 Rust 工具”表述，并同时披露 22 个已支持、11 个部分支持和 11 个范围外入口；明确 Connect/Broker/JMX/Trogdor/完整 metadata-shell 不在范围内；不应使用“100% 兼容 Apache Kafka CLI”。

### 10.1 二次代码审计发现的待修正项

以下不是远期功能愿望，而是已实现命令与原版语义之间仍需处理的具体问题：

| 优先级 | 位置 | 当前行为 | 原版/期望行为 |
|---|---|---|---|
| P2 | groups validate-regex | 使用 Rust regex；Kafka 帮助文案称 RE2，实际 Java 实现却调用 `Pattern.compile` | 保持方言边界声明，并用 Kafka 原版测试用例做差分验证；不能笼统宣称任一方言完全一致 |

本轮已经修复 reset-offsets 的 active group 安全检查、空 group 批处理中断、重复 topic selector 合并、groups delete 结构化输出、reassignment 的逐 partition/log-dir 错误结果、Kafka 4.4 新增 Assigning/Reconciling 状态过滤，以及 console consumer 用 Rust regex 预判 librdkafka POSIX ERE 的错误。当前正则剩余差距是离线 `groups validate-regex` 与原版 Java Pattern/帮助文案 RE2 之间无法同时逐语法一致。

### 10.2 逐入口验收结论

| 入口 | 动作完整性 | 主要参数完整性 | 行为/输出兼容性 | 综合结论 |
|---|---|---|---|---|
| topics | 完整 | 高 | 高/中 | 常用功能可替代，缺单次响应 partition 限制 |
| console-producer | 单动作 | 中 | 中 | 可做常规生产，不替代 Java reader 插件体系 |
| producer-perf-test | 单动作 | 高 | 中/高 | payload/key/限速/warmup/事务/延迟分位数已实现；Java 全量 metrics 与完整 Java regex 方言仍有边界 |
| e2e-latency | 单动作 | 高 | 中/高 | 自动建 topic、同步往返、key/value/header/null header 校验及 named/legacy 参数已实现；异常格式和空 optional config 参数不同 |
| verifiable-producer | 单动作 | 高 | 中/高 | JSON Lines delivery 事件、限速、acks、重复 key、value prefix 与 create time 已实现；异常类名与 Java metrics 不逐字等价 |
| console-consumer | 单动作 | 中 | 中/高 | group、commit、offset 与配置优先级已对齐；不替代 formatter/deserializer 插件体系 |
| console-share-consumer | 单动作 | 中/高 | 中/高 | KIP-932 消费、三类 acknowledgement、formatter 与 timeout 已实测；不替代 Java 插件体系，部分 Java consumer property 尚未映射 |
| share-consumer-perf-test | 单动作 | 高 | 中/高 | 多 consumer、超时和 CSV/metrics 统计已实测；fetch/socket 旋钮、Java 全量 metrics 与完整 SimpleDateFormat 仍有边界 |
| consumer-perf-test | 单动作 | 高 | 中/高 | topic/include、配置优先级、fetch/socket、超时及十列 CSV 已实测；Java 全量 metrics、完整 SimpleDateFormat 与 rebalance 回调精度仍有边界 |
| verifiable-share-consumer | 单动作 | 高 | 中/高 | JSON 事件、offset reset、verbose、pattern 与 acknowledgement 已实测；async 当前同步等待完成，部分 Java consumer property 尚未映射 |
| verifiable-consumer | 单动作 | 高 | 中/高 | classic/KIP-848、assignment/records/commits 事件与 close-timeout 已落地；Java assignor 类名映射与 rebalance 回调精度不完全等价 |
| replica-verification | 单动作 | 高 | 中/高 | per-broker Fetch、batch offset/CRC 比较与 max lag 报告已实现；原版多线程 barrier 改为异步并发；多 replica 不一致场景待多 broker fixture |
| dump-log | 单动作 | 高 | 中/高 | .log/.index/.timeindex/.txnindex/producer snapshot v1、控制记录类型与 EndTxn/LeaderChange 前缀、StringDecoder 已实现；coordinator 专用 decoder 明确未支持 |
| storage | 多动作 | 中/高 | 中 | random-uuid/info/format(meta.properties + residual JSON 标记 + __cluster_metadata-0) 可用；不写二进制 bootstrap.checkpoint 名；完整 RecordsSnapshotWriter/SCRAM 未实现 |
| consumer-groups | 完整 | 高 | 高/中 | reset、Kafka 4.4 state filter 及 verbose consumer protocol epoch 列已对齐；协议 fallback 鉴权矩阵仍需扩充 |
| groups | 单动作 | 高 | 高/中 | 全类型 list 与五类过滤语义完整；结构化输出格式不同，协议 fallback 鉴权矩阵仍需扩充 |
| share-groups | 完整 | 高 | 高/中 | 五个动作、成员/状态/offset 与 mutation 闭环已实测；结构化输出不同，协议 fallback 鉴权矩阵仍需扩充 |
| streams-groups | 完整 | 高 | 中 | 五个动作及 API 89 v0/v1 已实现；Kafka 4.3 真机覆盖版本边界，但真实 Streams 应用与 Kafka 4.4 topology v1 闭环仍待集成验证 |
| streams-application-reset | 单动作 | 高 | 高/中 | reset 策略、边界调整、active member/force、内部 topic 推断删除均已实现；结构化输出不同，尚缺真实 Streams runtime 端到端 fixture |
| configs | 完整 | 高 | 中/高 | topic/broker（含 default entity）/group/SCRAM、quota、broker-logger/client-metrics 可用；支持 bootstrap-controller 路由 |
| client-metrics | 完整 | 高 | 高/中 | 四个动作和全部参数语义已实现；结构化输出格式不同 |
| features | 完整 | 高 | 高/中 | 六个动作、指定 node、dry-run 与 Kafka 4.4 离线映射已实现；支持 bootstrap-controller 路由（纯 controller listener 仍可能连接失败） |
| transactions | 完整 | 高 | 高/中 | 六个动作及 coordinator/指定 broker 语义已实现；破坏性 recovery 闭环仍需隔离集成 fixture |
| metadata-quorum | 完整 | 高 | 中/高 | 三个动作、DescribeQuorum v2 和 API 80/81 已实现；支持 bootstrap-controller 路由；多 controller mutation 集成仍缺 |
| delegation-tokens | 完整 | 高 | 高/中 | 四动作与 v3 requester/owner 语义完整；缺 SASL 全生命周期集成 fixture |
| get-offsets | 单动作 | 高 | 高/中 | 普通及分层存储 OffsetSpec、过滤与逐 partition 错误语义均已实现；结构化输出格式不同 |
| acls | 完整 | 中/高 | 中 | 常见 ACL 可用，缺新资源、bootstrap-controller 和原版确认模式 |
| reassign-partitions | 完整 | 高 | 中/高 | 支持追加迁移、复制因子保护、两类限流及自动清理/preserve；支持 bootstrap-controller 路由 |
| delete-records | 单动作 | 高 | 高 | 核心功能完整；兼容脚本立即执行，原生入口保留预览保护 |
| leader-election | 单动作 | 高 | 高 | 核心功能完整；兼容脚本立即执行，原生入口保留预览保护 |
| log-dirs | 单动作 | 高 | 高/中 | 常用 describe 能力完整 |
| broker-api-versions | 单动作 | 高 | 中 | 核心查询完整，输出格式不同 |
| cluster | 完整 | 中/高 | 中/高 | 常用查询、fenced broker endpoint 和 unregister 可用；兼容 unregister 立即执行；支持 bootstrap-controller 路由 |

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
| 2026-08-04 | Console 参数互斥与消费边界 | 废弃/current producer、consumer、reader、formatter property 组合按原版互斥；config 文件旧名与新名继续互斥；修复全局 30 秒超时误注入 consumer，并对齐 max-messages 0/-1 和负 timeout 语义 | 100 个单元测试、7 个 CLI 测试、Kafka 4.3.1 零消息退出、Kafka 3.6.2、bundled glibc 及双 musl CI 全部通过 |
| 2026-08-04 | Topics 参数与结果语义 | create partition/replication 均支持 broker 默认并严格互斥手工 assignment；按动作拆分参数；alter 支持正则批量；describe 补齐 ID 优先级、if-exists、配置与 replication factor，并接受 partition-size 兼容参数 | 109 个单元测试、7 个 CLI 测试、Kafka 4.3.1 broker 默认 3 partitions/正则 alter/ID/config 闭环、Kafka 3.6.2、bundled glibc 及双 musl CI 全部通过 |
| 2026-08-04 | GetOffsetShell 二次语义审计 | 对齐固定 client ID、Java 末尾空 token、逐 partition 错误和未知 offset；补齐 `-4/-5/-6` 解析，并对 librdkafka 2.12 的执行限制返回明确错误 | 115 个单元测试、7 个 CLI 测试、Kafka 3.6.2/4.3.1、bundled glibc 及双 musl CI 全部通过 |
| 2026-08-04 | musl artifact 运行验证 | x86_64 musl 在 CI runner 原生执行 `--version`/`--help`；aarch64 musl 通过 QEMU user-mode emulator 执行相同 smoke test，不再只依赖 ELF 文件类型 | CI `30840437738` 六个 job 全绿，两种 musl artifact 均成功实际启动 |
| 2026-08-04 | Consumer include 正则后端边界 | 移除 Rust regex 对 `--include` 的异方言预校验，订阅表达式由实际执行查询的 librdkafka POSIX ERE 引擎编译；保留整串锚定 | 116 个单元测试、7 个 CLI 测试；Kafka 4.3.1 增加 Rust regex 拒绝但 librdkafka 接受的量词集成用例 |
| 2026-08-04 | ConfigCommand 参数语义复审 | `delete-config` 支持逗号分隔并逐项 trim；新增重复 entity type、broker ID 和 add-config key 字符预校验；确认现有客户端无法真实支持 controller bootstrap | 121 个单元测试、7 个 CLI 测试；Kafka 4.3.1 使用单个逗号列表删除两个 topic config |
| 2026-08-04 | AclCommand 删除过滤语义复审 | 补齐 Match pattern；add 拒绝 Any/Match；remove 精确应用 allow/deny host，principal 缺省时按原版删除资源过滤器匹配的全部 ACL，principal 存在而 operation 缺省时默认 All | 125 个单元测试、7 个 CLI 测试；Kafka 4.3.1 增加 Match 查询和双 host 精确删除闭环 |
| 2026-08-04 | AclCommand 批量资源语义复审 | topic/group/transactional-id 改为可重复资源选择；list 支持重复 principal 并在客户端精确过滤，对多个资源过滤器的重叠结果去重；资源、principal 和 host 按原版 trim/去重 | 127 个单元测试、7 个 CLI 测试；Kafka 4.3.1 增加三资源批量 add/list/remove 闭环 |
| 2026-08-04 | AclCommand operation/resource 复审 | 直接对齐 Kafka `AclEntry.supportedOperations`；拒绝非法 resource-operation 组合和 consumer-only 的 cluster/transactional-id；TwoPhaseCommit/CreateTokens/DescribeTokens 明确标注 librdkafka 2.12 边界 | 131 个单元测试、7 个 CLI 测试；Kafka 4.3.1 增加 Group AlterConfigs 创建、查询、删除闭环 |
| 2026-08-04 | AclCommand add 幂等语义复审 | 按 Kafka 原版在 CreateAcls 前逐资源查询完整 binding 集合，只提交缺失项；重复 operation/host 去重；全量已存在时不发 CreateAcls 并输出 ALREADY_EXISTS 计数 | 133 个单元测试、7 个 CLI 测试；Kafka 4.3.1 增加同一 ACL 连续执行两次的幂等闭环 |
| 2026-08-04 | ConfigCommand entity name 复审 | IP entity 在请求前验证为合法 IP 或可解析主机名；alter 拒绝空 `--entity-name` 并要求显式使用 `--entity-default` | 136 个单元测试、7 个 CLI 测试；Kafka 4.3.1 增加命名 IP quota 设置→查询→删除闭环；CI `30845027506` 六项全绿 |
| 2026-08-04 | Broker logger describe 前置校验 | 未指定 entity name 时在创建协议客户端前拒绝，与 Kafka `ConfigCommandOptions.checkArgs` 一致 | 137 个单元测试、7 个 CLI 测试；Kafka 3.6.2/4.3.1、bundled glibc 及双 musl CI `30845347757` 全部通过 |
| 2026-08-04 | ConfigCommand 逐 entity selector | 补齐八类原版专用 entity flags 和四类 defaults flags；user/client quota 可混合默认与命名实体；兼容入口把通用 type/name/default 按原版位置配对重写 | 141 个单元测试、7 个 CLI 测试；Kafka 4.3.1 默认 user + 命名 client 设置→查询→删除闭环；CI `30845939866` 六项全绿 |
| 2026-08-04 | ConfigCommand 分组 config 值 | 新增专用 add-config 解析器，支持逗号列表、方括号分组、空值、括号内等号和重复 key 后值覆盖；SCRAM 接受 parser 规范化后的 credential body | 146 个单元测试、7 个 CLI 测试；Kafka 4.3.1 `cleanup.policy=[compact,delete]` 闭环及 Kafka 3.6.2 SCRAM 回归；CI `30846507492` 六项全绿 |
| 2026-08-04 | ConsumerGroup 原版 timeout | groups 命令域新增 `--timeout`，覆盖 Admin 请求与 group 稳定等待；保留原生全局 `--timeout-ms` | 147 个单元测试、7 个 CLI 测试；Kafka 4.3.1 `groups list --timeout 5000` 实际请求；CI `30846952761` 六项全绿 |
| 2026-08-04 | ConsumerGroup Kafka 4 状态过滤 | `Assigning`/`Reconciling` 通过 ListGroups v5 + ConsumerGroupDescribe 协议 fallback 补齐；旧五状态继续走 librdkafka；确认 `NotReady` 与 Share/Streams 不属于原版 consumer-groups 合法过滤集合 | 149 个单元测试、7 个 CLI 测试；Kafka 4.3.1 增加 Assigning 实际请求；与 Kafka 4.4 源码差分核对；CI `30848089820` 六项全绿 |
| 2026-08-04 | ConsumerGroup verbose epoch | state verbose 合并 group/target assignment epoch，members verbose 合并 member/target epoch；librdkafka 继续提供 coordinator、assignment 与成员主体，协议结果只补缺失字段 | 151 个单元测试、7 个 CLI 测试；Kafka 4.3.1 增加 state/members verbose 列断言；CI `30848663059` 六项全绿 |
| 2026-08-04 | ConsumerGroup migration member | ConsumerGroupDescribe member type 映射为三态 upgraded；仅当同一 group 同时存在 classic 与 consumer protocol 成员时，verbose members 按原版增加 `UPGRADED` 列 | 153 个单元测试、7 个 CLI 测试；迁移组动态列与 unknown 边界由单元测试覆盖；CI `30849285517` 六项全绿 |
| 2026-08-04 | Console Consumer leader epoch | DefaultMessageFormatter `print.epoch` 从固定 `NOT_PRESENT` 改为调用公开 librdkafka message leader epoch API；负 sentinel 仍映射为原版缺失值 | 155 个单元测试、7 个 CLI 测试；Kafka 4.3.1 formatter 数据闭环新增非 `NOT_PRESENT` epoch 断言；CI `30849763389` 六项全绿 |
| 2026-08-04 | Get Offsets tiered specs | `-4/-5/-6` 从解析后能力错误升级为 ListOffsets v11 协议 fallback；普通 spec 保持 librdkafka，逐 partition error/unknown offset 语义保持一致 | 156 个单元测试、7 个 CLI 测试；Kafka 4.3.1 实际请求 earliest-local/latest-tiered/earliest-pending-upload；CI `30850201584` 六项全绿 |
| 2026-08-04 | Client Metrics 独立入口 | 新增 `kafka client-metrics` 和 `kafka-client-metrics.sh`；完整实现 list/describe/alter/delete、Kafka UUID generate-name、interval/match/metrics 与空值删除语义，复用 API 74/32/44 后端 | 159 个单元测试、8 个 CLI 测试；Kafka 4.3.1 设置→list→describe→delete 闭环；CI `30851053389` 六项全绿 |
| 2026-08-04 | Kafka Features 入口 | 新增 `kafka features` 和 `kafka-features.sh`；实现六个原版动作、指定 node describe、UpdateFeatures v0/v1 dry-run/更新，以及按 Kafka 4.4 源码对齐的 metadata/production feature 离线映射 | 163 个单元测试、9 个 CLI 测试；Kafka 4.3.1 任意/指定 node describe 与 metadata upgrade dry-run；CI `30851984283` 六项全绿 |
| 2026-08-04 | Kafka Transactions 入口 | 新增 `kafka transactions` 和 `kafka-transactions.sh`；实现 list、describe、describe-producers、abort、find-hanging、forceTerminateTransaction，保留 coordinator、指定 broker、两类 abort spec 和 producer fencing 语义 | 176 个单元测试、10 个 CLI 测试；Kafka 4.3.1 list/指定 broker producer state/find-hanging；CI `30853525172` 六项全绿 |
| 2026-08-04 | Kafka Metadata Quorum 入口 | 新增 `kafka metadata-quorum` 和 `kafka-metadata-quorum.sh`；实现 status/replication、add/remove controller，补足 DescribeQuorum v2 codec 和 Add/RemoveRaftVoter API 80/81 | 183 个单元测试、11 个 CLI 测试；CI `30854327394` 六个 job 全绿，Kafka 4.3.1 验证 status/replication/remove dry-run |
| 2026-08-04 | Kafka Delegation Tokens 入口 | 新增 `kafka delegation-tokens` 和 `kafka-delegation-tokens.sh`；实现 create/renew/expire/describe、v3 owner/requester、renewer、标准 Base64 HMAC 和 `-1` period sentinel | 189 个单元测试、12 个 CLI 测试；CI `30854931372` 六个 job 全绿，Kafka 4.3.1 验证 PLAINTEXT 安全拒绝边界 |
| 2026-08-04 | Kafka Groups 入口 | 新增 `kafka all-groups` 和 `kafka-groups.sh`；实现全类型 list、group-type/protocol 组合过滤及 consumer/share/streams 快捷过滤，使用 ListGroups v5 保留 Share/Streams | 196 个单元测试、13 个 CLI 测试；CI `30855676975` 六个 job 全绿，Kafka 3.6.2/4.3.1 实际请求通过 |
| 2026-08-04 | Kafka Share Groups 入口 | 新增 `kafka share-groups` 和 `kafka-share-groups.sh`；实现 list/describe/delete/reset-offsets/delete-offsets，使用 API 77/90/91/92 对齐 Kafka 4.4，并修正未知 flexible API header 边界 | 202 个单元测试、14 个 CLI 测试；CI `30857219313` 六个 job 全绿，Kafka 4.3.1 真实 ShareConsumer 与五动作闭环、Kafka 3.6.2 回归通过 |
| 2026-08-04 | Kafka Streams Groups 入口 | 新增 `kafka streams-groups` 和 `kafka-streams-groups.sh`；实现 list/describe/delete/reset-offsets/delete-offsets，补齐 API 89 v0/v1、task assignment、offset lag、内部 topic 与 topology description | 210 个单元测试、15 个 CLI 测试；CI `30859766154` 六个 job 全绿，Kafka 4.3.1 实际验证 API 89 版本边界、Kafka 3.6.2 回归通过；真实 Streams 应用闭环待补 |
| 2026-08-04 | Streams reset/delete 二次审计 | 缺失 group 按原版视为 inactive 并继续 reset；空 inactive CSV 不再 panic；group 删除失败时不再误删内部 topic；内部 topic 错误进入结构化结果 | 211 个单元测试、15 个 CLI 测试；CI `30860540837` 六个 job 全绿，Kafka 4.3.1 验证缺失 group reset dry-run |
| 2026-08-04 | Streams Application Reset 入口 | 新增 `kafka streams-application-reset` 和兼容脚本；实现全部 reset 策略、offset 边界调整、active member/force LeaveGroup、内部 topic 精确推断与删除 | 215 个单元测试、16 个 CLI 测试；CI `30861552986` 六个 job 全绿，Kafka 3.6.2/4.3.1 实际请求及双 musl smoke test 通过 |
| 2026-08-04 | Console Share Consumer 入口 | 新增 `kafka share-consume` 和 `kafka-console-share-consumer.sh`；基于 KIP-932 实现 ShareFetch、显式 ACCEPT/RELEASE/REJECT、累计空闲 timeout、共享 formatter 与 delivery count | 219 个单元测试、17 个 CLI 测试；CI `30863139169` 六个 job 全绿，Kafka 4.3.1 实时消息与 ShareAcknowledge 闭环通过 |
| 2026-08-04 | Verifiable Share Consumer 入口 | 新增 `kafka verifiable-share-consumer` 和 `kafka-verifiable-share-consumer.sh`；对齐 JSON Lines systest 事件、verbose record、offset reset、三种模式与循环 accept/release/reject/renew pattern | 222 个单元测试、18 个 CLI 测试；CI `30863964909` 六个 job 全绿，Kafka 4.3.1 实际事件与 acknowledgement 闭环及双 musl smoke test 通过 |
| 2026-08-04 | Share Consumer Performance 入口 | 新增 `kafka share-consumer-perf-test` 和兼容脚本；实现多 consumer 并发、record/byte 计数、idle timeout、周期/逐 consumer/七列 CSV 汇总和 native connection metrics | 225 个单元测试、19 个 CLI 测试；CI `30865007900` 六个 job 全绿，Kafka 4.3.1 双 consumer 精确消费四条消息及双 musl smoke test 通过 |
| 2026-08-04 | Consumer Performance 入口 | 新增 `kafka consumer-perf-test` 和兼容脚本；基于 rdkafka/librdkafka 实现 topic/include、配置优先级、fetch/socket 参数、rebalance/fetch 时间及原版十列 CSV | 228 个单元测试、20 个 CLI 测试；CI `30867465950` 覆盖 Kafka 3.6.2 topic、Kafka 4.3.1 include/metrics、bundled glibc 与双 musl smoke test |
| 2026-08-04 | Producer Performance 入口 | 新增 `kafka producer-perf-test` 和兼容脚本；基于 rdkafka/librdkafka callback 实现三种 payload、Java 顺序随机源、key 分布、fractional throttle、warmup/steady-state、事务与延迟分位数 | 236 个单元测试、21 个 CLI 测试通过；Kafka 3.6.2 monotonic 与 Kafka 4.3.1 transactional 集成路径已加入 CI `30868691184`，CI 状态后续批量审计 |
| 2026-08-04 | End-to-End Latency 入口 | 新增 `kafka e2e-latency` 和兼容脚本；实现 named/legacy 参数、Java Random、自动建 topic、显式 high watermark、逐条同步 delivery→consume 及 key/value/ordered/null header 校验 | 241 个单元测试、22 个 CLI 测试通过；Kafka 3.6.2/4.3.1 往返路径已加入 CI `30869292636`，CI 状态后续批量审计 |
| 2026-08-04 | Verifiable Producer 入口 | 新增 `kafka verifiable-producer` 和兼容脚本；基于 rdkafka/librdkafka 实现异步 delivery、限速、acks、重复 key、value prefix、create time 与 systest JSON Lines 事件 | 245 个单元测试、23 个 CLI 测试通过；Kafka 3.6.2/4.3.1 集成路径已加入 workflow，按策略不等待 CI，状态后续批量审计 |
| 2026-08-04 | Verifiable Consumer 入口 | 新增 `kafka verifiable-consumer` 和 `kafka-verifiable-consumer.sh`；实现 classic/KIP-848 protocol、assignment/revocation、records/commits、verbose record_data、close-timeout 与废弃 `--consumer.config` | 提交 `ddba17a`；Kafka 3.6.2/4.3.1 集成覆盖有限消息与 verbose 事件链；按策略不等待 CI |
| 2026-08-05 | Replica Verification 入口 | 新增 `kafka replica-verification` 和 `kafka-replica-verification.sh`；按 broker Fetch（`replica_id=-2`）比较 record batch offset/CRC，输出启动行与 max lag 摘要；兼容 `--broker-list` 改写 | 253 个 library tests、25 个 CLI tests、Clippy/fmt 通过；Kafka 3.6.2/4.3.1 集成覆盖启动与 lag 报告；按策略不等待 CI |
| 2026-08-05 | Dump Log 入口 | 新增 `kafka dump-log` 与兼容脚本；实现 `.log` batch/record、StringDecoder、`.index`/`.timeindex` 基础 dump；专用 decoder 明确 unsupported | library/CLI 单测覆盖；Clippy/fmt 通过 |
| 2026-08-05 | Storage 入口 | 新增 `kafka storage` 与兼容脚本；实现 random-uuid、info、format(meta.properties)；bootstrap.checkpoint/SCRAM 边界标明 | format/info 单测闭环；Clippy/fmt 通过 |
| 2026-08-05 | 范围外入口清单 | 明确 11 个脚本不在复刻范围：Connect×5、server-start/stop、run-class、jmx、trogdor、完整 metadata-shell | 写入 §3.2 与结论摘要 |
| 2026-08-05 | Storage format 深度 | controller metadata 目录写入 `bootstrap.checkpoint`（partial-native-bootstrap-v1）与 `__cluster_metadata-0/`；标明 RecordsSnapshotWriter residual | format 单测断言 on-disk 产物；broker-only 不写 checkpoint |
| 2026-08-05 | bootstrap.checkpoint 安全修复 | **禁止** JSON 占用 Kafka 保留二进制名 `bootstrap.checkpoint`；改为 `kafka-cli-bootstrap.residual.json` + `__cluster_metadata-0/`；拒绝已有文本 checkpoint；dump-log 诊断文本 residual | 单测断言不写保留名、拒绝文本 checkpoint、dump 诊断路径 |
| 2026-08-05 | `--bootstrap-controller` 路由 | features/metadata-quorum/configs/cluster/reassign 去掉预置 Unsupported；将 controller 地址作为 admin bootstrap 目标；与 `--bootstrap-server` 运行时互斥 | 单元/CLI 测试覆盖解析、互斥、无旧桩消息；真实 binary 对不可达地址走客户端错误路径 |
| 2026-08-05 | Dump-log 伴生文件 | `.txnindex` AbortedTxn v0 与 producer `.snapshot` ProducerSnapshot v1 完整 dump | 单测合成文件驱动 shipped 解析路径 |
| 2026-08-05 | Dump-log 控制记录 | 解析 ControlRecordType：ABORT/COMMIT endTxnMarker、LeaderChange leaderId、Snapshot/KRaft 命名前缀 | library 单测 + offline_tools 真实 binary |
| 2026-08-05 | 部分支持评级收敛 | 将 groups/configs/features/offsets/cluster/reassign/metadata-quorum/e2e/verifiable-producer/consumer 升为「已支持」（核心动作齐全；Java 插件/metrics 等仍为有意差异） | 仪表盘 22 已支持 / 11 部分支持 |
| 2026-08-05 | 集成测试扩充 | 新增 `tests/offline_tools.rs`（7）与 `tests/kafka_4_depth_integration.rs`（11 ignored Docker 聚焦套件） | 普通 312；Kafka 4 单体 + depth 11 本地 `--features bundled` 全绿 |
| 2026-08-05 | 集成 fixture 加固 | 单节点 transaction/share coordinator RF=1；depth Share 预热 classic group + ShareConsumer 重试 | 修复本地 producer-perf 事务超时与 share FindCoordinator 竞态 |
| 2026-08-05 | e2e-latency 收尾 commit | 最终 Sync commit 对 NotCoordinator/CoordinatorNotAvailable 降级忽略（测量已完成） | 修复 Kafka 3.6.2 单节点集成 e2e 误失败 |
| 2026-08-05 | 本地全量集成 | offline 7 + Kafka 4 单体 + depth 11 + Kafka 3.6.2 单体，均 `--features bundled` | 本机全绿；Kafka 3 使用 `.kafka/kafka_2.13-3.6.2` |

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
| ListConsumerGroupOffsets / AlterConsumerGroupOffsets | groups describe/reset-offsets、Streams Application Reset | 已覆盖 |
| DescribeUserScramCredentials / AlterUserScramCredentials | configs users describe/alter | 已覆盖 |
| DescribeTopics | topics describe/topic-id | 已覆盖 |
| DescribeCluster | cluster id/list-endpoints | 已覆盖 |
| ListOffsets | offsets 与 group reset 规划 | 已覆盖 |
| ElectLeaders | leader-election | 已覆盖，含 JSON 批量目标 |

非 Admin 客户端能力也已接入 console producer/consumer：异步与同步 delivery、主要 producer tuning 配置、正则订阅、空闲 timeout、isolation level、手工 partition/offset、headers 和结构化消息。其他 librdkafka 配置可通过 `--command-property` 传入。

以下 Kafka 原版能力在 librdkafka 2.12 中没有对应 C API，因此不属于本轮可直接通过公开 C API 实现的集合：partition reassignment、describe/alter log dirs、API version 明细、unregister broker、client quota、broker logger/client metrics config resource、metadata quorum、feature update、transaction listing/abort、delegation token、Share Group 专用 Admin API，以及 StreamsGroupDescribe。Streams Group 的通用删除与 offset 操作仍复用 librdkafka；其专用 describe 使用独立协议客户端。报告不会把这些 fallback 误记为 librdkafka 路径。

此外，已通过实际 Kafka 4.3.1 测试确认 broker default config 无法用 librdkafka 表达：Kafka 要求空 ConfigResource name，而 `rd_kafka_ConfigResource_new` 在空 name 时返回 NULL。Get Offsets 的 `-4/-5/-6`、consumer group epoch、Kafka 4.4 consumer-groups 的 Assigning/Reconciling state、fenced broker inclusion、Kafka 4.4 user-principal ACL resource 和 Delegation Token ACL resource 也未由当前版本暴露；其中 tiered OffsetSpec 已通过 ListOffsets v11 补齐，consumer protocol epoch 和 Assigning/Reconciling 已通过 ConsumerGroupDescribe/ListGroups v5 补齐，fenced broker inclusion 已通过独立 DescribeCluster v2 协议路径补齐。

结论：以 librdkafka 2.12 的公开 Admin operation 枚举为边界，除已被增量 API 取代的旧 AlterConfigs 外，当前所有 operation 均已有实际 CLI 调用与测试路径。后续剩余差距需要升级 librdkafka、继续使用独立协议客户端，或属于 Java 插件/服务进程而非 librdkafka 客户端能力。
