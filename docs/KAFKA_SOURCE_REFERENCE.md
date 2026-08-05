# Apache Kafka 本地源码参考

为方便对照原版 `bin/*.sh` 与 Java 工具实现，建议在本机维护一份 Kafka 浅克隆。

## 推荐位置

```text
/root/code/kafka          # 与 kafka-cli 同级，不纳入本仓库
/root/code/kafka-cli      # 本项目
```

路径不强制；也可用环境变量 `KAFKA_SRC` 指向其他目录。

## 一键浅克隆 / 刷新

在 `kafka-cli` 仓库根目录执行：

```shell
scripts/fetch-kafka-source.sh
# 或指定路径
scripts/fetch-kafka-source.sh /path/to/kafka
```

脚本行为：

- `git clone --depth 1 --branch trunk`（浅克隆）
- `--filter=blob:none --sparse`，只检出与 CLI 复刻相关的目录（`bin/`、`tools/`、`clients/`、`core/`、`shell/` 等）
- 已存在时执行 `git fetch --depth 1` + `reset --hard origin/trunk`

当前本机实例（示例）：

| 项 | 值 |
|---|---|
| 路径 | `/root/code/kafka` |
| 分支 | `trunk` |
| 体积 | 约 70 MiB（sparse） |
| `bin/*.sh` | 44 |

## 常用检索

```shell
export KAFKA_SRC=${KAFKA_SRC:-/root/code/kafka}

# 脚本入口
ls "$KAFKA_SRC/bin"/*.sh

# 工具实现
rg -n "class ReplicaVerificationTool|class DumpLogSegments|class VerifiableConsumer" \
  "$KAFKA_SRC/tools"

# 某脚本转发到哪个 Java 类
rg -n "kafka-run-class" "$KAFKA_SRC/bin/kafka-replica-verification.sh"
```

## 与兼容报告的关系

`docs/KAFKA_COMPATIBILITY_REPORT.zh-CN.md` 中的 Kafka 基准提交应尽量与本地克隆一致或可追溯。更新克隆后，若对比基准变了，请同步改报告中的提交哈希与日期。

本目录**不要**提交进 `kafka-cli` 仓库。
