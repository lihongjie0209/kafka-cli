我想使用rust开发一个跨平台的kafka client cli, 目标是实现 kafka-*.sh 中的功能



## 技术选型
1. 使用 Rust 语言进行开发，利用其高性能和内存安全的特性。
2. 使用 `rdkafka` 库作为 Kafka 客户端的基础库，提供对 Kafka 的生产者和消费者功能。
3. 使用 `clap` 库来解析命令行参数，提供用户友好的 CLI 接口。
4. 使用 `serde` 和 `serde_json` 库来处理 JSON 数据格式，便于与 Kafka 消息进行交互。
5. 使用 `tokio` 异步运行时来处理异步 I/O 操作，提高性能和响应速度。
6. 使用 `log` 和 `env_logger` 库来处理日志记录，便于调试和运行时信息输出。


## 功能需求
参考 https://github.com/apache/kafka/tree/trunk/bin 中的 kafka-*.sh 脚本，首先梳理需要实现功能的优先级，然后逐步实现。



## 命令行规范
1. 使用 `kafka-cli` 作为主命令。
2. 按照  kafka-*.sh 划分子命令，例如 `kafka-cli produce`、`kafka-cli consume`、`kafka-cli topic` 等。
3. 每个子命令支持相应的参数和选项，使用 `clap` 库提供的功能来定义和解析。
4. 提供帮助信息和使用示例，用户可以通过 `kafka-cli --help` 或 `kafka-cli <subcommand> --help` 获取详细的使用说明。


## 日志规范

1. 使用 `log` 库进行日志记录，支持不同的日志级别（trace、debug、info、warn、error）。
2. 使用 `env_logger` 库来配置日志输出，支持通过环境变量设置日志级别。
3. 日志格式应包含时间戳、日志级别、模块名和消息内容，便于调试和分析。
4. 要区分日志和正常的命令行输出，日志信息应输出到标准错误（stderr），而正常的命令行输出应输出到标准输出（stdout）。


