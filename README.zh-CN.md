# kafka-cli

`kafka-cli` 是使用 Rust 编写的原生 Kafka 命令行工具，将常用数据读写和集群管理功能整合为一个可执行文件。

```shell
cargo build --release --locked

kafka --bootstrap-server localhost:9092 topics create \
  --topic events --partitions 3 --replication-factor 1

kafka --bootstrap-server localhost:9092 topics describe --topic events
```

支持 `--command-config` 读取 Kafka properties，以及 PLAINTEXT、SSL、
SASL/PLAIN、SCRAM-SHA-256/512。统一入口中的危险操作默认预览，添加
`--execute` 后才实际执行。

Topic 创建和扩分区支持 Kafka 兼容的 `--replica-assignment`。消费组 offset
重置支持 earliest/latest、绝对值、位移，以及 `--to-current`、
`--to-datetime` 和 ISO-8601 `--by-duration`。

执行 `kafka <命令> --help` 查看参数。兼容入口可由
`scripts/install-aliases.sh` 创建，同时识别带 `.sh` 后缀的 Kafka 工具名。
