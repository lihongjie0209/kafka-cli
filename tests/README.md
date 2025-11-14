# 运行 CLI 功能测试

## 快速开始

```bash
# 确保 Kafka 正在运行
cd docker/single-node
docker-compose up -d
cd ../..

# 运行测试
python tests/test_cli_functional.py
```

## Windows 用户

如果遇到编码问题，使用以下命令:

```powershell
$OutputEncoding = [System.Text.Encoding]::UTF8
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
python tests/test_cli_functional.py
```

## 测试内容

### Python CLI 功能测试 (14 个测试)
- ✅ 基础命令 (help, version)
- ✅ Topics 管理 (list, create, describe, delete)
- ✅ 配置管理 (describe, alter)
- ✅ 消息生产消费
- ✅ Consumer Groups 管理
  - 列出 consumer groups
  - 描述 consumer group
  - 重置 offsets (earliest, latest, offset, timestamp)
  - 使用 --partitions 参数重置指定分区
  - 使用 --to-datetime 按时间戳重置
  - 删除 consumer group（包含活跃成员验证）
- ✅ 错误处理

### Rust 集成测试 (14 个测试)
- ✅ Admin 操作 (list, create, describe, delete, alter config)
- ✅ Producer 操作 (produce messages with/without keys)
- ✅ Consumer 操作 (consume messages, key-value messages)
- ✅ Consumer Groups 操作
  - List consumer groups
  - Describe consumer group
  - Reset offsets (earliest)
  - Reset offsets with partitions parameter
  - Reset offsets by timestamp
- ✅ 端到端工作流测试

## 预期输出

```
======================================================================
Kafka CLI 功能测试套件
======================================================================

...

======================================================================
测试摘要
======================================================================
总计: 14 个测试
通过: 14
失败: 0
```

## 注意事项

- 测试需要已构建的 CLI 可执行文件 (`target/release/kafka-cli.exe`)
- 测试需要运行中的 Kafka 集群 (localhost:9093)
- 测试会创建和删除临时 topics (前缀: `pytest_`)
- 测试执行时间约 10-15 秒
