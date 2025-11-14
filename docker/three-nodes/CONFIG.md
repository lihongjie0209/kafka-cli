# Kafka KRaft 模式配置说明

本文档详细解释三节点 Kafka 集群的配置项。

## 核心配置项

### KAFKA_NODE_ID
```yaml
KAFKA_NODE_ID: 1
```
- **说明**: Kafka 节点的唯一标识符
- **类型**: 整数
- **范围**: 通常从 1 开始
- **作用**: 在 KRaft 模式下，每个节点必须有唯一的 ID，用于在集群中识别该节点
- **示例**: 节点 1 使用 `1`，节点 2 使用 `2`，节点 3 使用 `3`

### KAFKA_PROCESS_ROLES
```yaml
KAFKA_PROCESS_ROLES: broker,controller
```
- **说明**: 定义节点在集群中扮演的角色
- **可选值**: 
  - `broker`: 负责存储和处理消息
  - `controller`: 负责集群元数据管理和选举
  - `broker,controller`: 同时担任两个角色（组合模式）
- **本配置**: 所有节点都是 broker 和 controller，实现完全分布式架构
- **优点**: 简化部署，无需单独的 controller 节点
- **生产环境建议**: 小规模集群可使用组合模式，大规模集群建议分离角色

### KAFKA_LISTENERS
```yaml
KAFKA_LISTENERS: PLAINTEXT://0.0.0.0:19092,CONTROLLER://0.0.0.0:19094,PLAINTEXT_HOST://0.0.0.0:19093
```
- **说明**: Kafka 监听的网络接口和端口
- **格式**: `<协议名>://<IP>:<端口>`
- **0.0.0.0**: 监听所有网络接口
- **本配置包含三个监听器**:
  1. `PLAINTEXT://0.0.0.0:19092` - 容器内部通信端口
  2. `CONTROLLER://0.0.0.0:19094` - Controller 之间的通信端口
  3. `PLAINTEXT_HOST://0.0.0.0:19093` - 宿主机访问端口
- **作用**: 定义 Kafka 在哪些端口上接受连接

### KAFKA_ADVERTISED_LISTENERS
```yaml
KAFKA_ADVERTISED_LISTENERS: PLAINTEXT://kafka-1:19092,PLAINTEXT_HOST://localhost:19093
```
- **说明**: 向客户端公布的连接地址
- **重要性**: ⭐⭐⭐⭐⭐ 最容易出错的配置项
- **作用**: 客户端连接后，Kafka 会返回这个地址让客户端继续通信
- **本配置**:
  - `PLAINTEXT://kafka-1:19092` - 容器内部客户端使用此地址
  - `PLAINTEXT_HOST://localhost:19093` - 宿主机客户端使用此地址
- **注意**: 必须与 `KAFKA_LISTENERS` 的协议名对应
- **常见问题**: 地址不正确会导致客户端连接后无法发送/接收消息

### KAFKA_LISTENER_SECURITY_PROTOCOL_MAP
```yaml
KAFKA_LISTENER_SECURITY_PROTOCOL_MAP: PLAINTEXT:PLAINTEXT,CONTROLLER:PLAINTEXT,PLAINTEXT_HOST:PLAINTEXT
```
- **说明**: 监听器名称与安全协议的映射关系
- **格式**: `<监听器名>:<安全协议>`
- **安全协议选项**:
  - `PLAINTEXT`: 无加密、无认证（开发环境）
  - `SSL`: 加密传输
  - `SASL_PLAINTEXT`: 认证但不加密
  - `SASL_SSL`: 认证且加密（生产环境推荐）
- **本配置**: 所有监听器都使用 PLAINTEXT（无安全措施）
- **生产环境**: 应该使用 SSL 或 SASL_SSL

### KAFKA_CONTROLLER_QUORUM_VOTERS
```yaml
KAFKA_CONTROLLER_QUORUM_VOTERS: 1@kafka-1:19094,2@kafka-2:29094,3@kafka-3:39094
```
- **说明**: KRaft 模式下的 controller 仲裁投票者列表
- **格式**: `<节点ID>@<主机名>:<controller端口>`
- **作用**: 定义哪些节点参与 controller 选举和元数据管理
- **本配置**: 三个节点都参与投票，形成 3 节点仲裁
- **仲裁机制**: 需要至少 (N/2 + 1) 个节点同意才能做出决策
  - 3 节点集群：需要 2 个节点同意（可容忍 1 个节点故障）
  - 5 节点集群：需要 3 个节点同意（可容忍 2 个节点故障）
- **高可用**: 奇数个节点可以最大化容错能力

### KAFKA_CONTROLLER_LISTENER_NAMES
```yaml
KAFKA_CONTROLLER_LISTENER_NAMES: CONTROLLER
```
- **说明**: 指定用于 controller 通信的监听器名称
- **作用**: 告诉 Kafka 使用哪个监听器进行 controller 之间的通信
- **本配置**: 使用名为 `CONTROLLER` 的监听器
- **必须**: 该名称必须在 `KAFKA_LISTENERS` 中定义

### KAFKA_INTER_BROKER_LISTENER_NAME
```yaml
KAFKA_INTER_BROKER_LISTENER_NAME: PLAINTEXT
```
- **说明**: broker 之间通信使用的监听器名称
- **作用**: 定义 broker 之间数据复制和协调时使用的网络通道
- **本配置**: 使用 `PLAINTEXT` 监听器（端口 19092/29092/39092）
- **性能考虑**: broker 间通信量大，应使用内网高速网络

### KAFKA_OFFSETS_TOPIC_REPLICATION_FACTOR
```yaml
KAFKA_OFFSETS_TOPIC_REPLICATION_FACTOR: 3
```
- **说明**: 消费者偏移量主题（__consumer_offsets）的副本因子
- **类型**: 整数
- **本配置**: 3 个副本
- **作用**: 保存消费者的消费位置信息的可靠性
- **建议**: 
  - 单节点: 1
  - 三节点: 3
  - 生产环境: 至少 3
- **重要性**: 偏移量丢失会导致消息重复消费或丢失

### KAFKA_TRANSACTION_STATE_LOG_MIN_ISR
```yaml
KAFKA_TRANSACTION_STATE_LOG_MIN_ISR: 2
```
- **说明**: 事务状态日志的最小同步副本数（In-Sync Replicas）
- **类型**: 整数
- **本配置**: 至少 2 个副本必须同步确认
- **作用**: 保证事务的持久性和一致性
- **ISR**: 与 leader 保持同步的副本集合
- **写入策略**: 只有当至少 2 个副本确认后，事务才算提交成功
- **容错**: 可以容忍 1 个副本失败（3 副本 - 2 ISR = 1 容错）
- **权衡**: 
  - 数值越大越安全，但性能越低
  - 数值越小性能越高，但风险越大

### KAFKA_TRANSACTION_STATE_LOG_REPLICATION_FACTOR
```yaml
KAFKA_TRANSACTION_STATE_LOG_REPLICATION_FACTOR: 3
```
- **说明**: 事务状态日志的副本因子
- **类型**: 整数
- **本配置**: 3 个副本
- **作用**: 定义事务日志在多少个 broker 上保存副本
- **内部主题**: __transaction_state
- **建议**: 应该等于集群节点数（最多）
- **与 MIN_ISR 关系**: 
  - 副本因子 = 3，MIN_ISR = 2
  - 允许 1 个副本离线，仍能继续事务操作

### KAFKA_LOG_DIRS
```yaml
KAFKA_LOG_DIRS: /tmp/kraft-combined-logs
```
- **说明**: Kafka 数据存储目录
- **作用**: 存储消息数据、索引文件、元数据等
- **本配置**: 使用容器内的 `/tmp/kraft-combined-logs`
- **生产环境建议**: 
  - 使用独立的数据卷挂载
  - 使用高性能 SSD 磁盘
  - 多个磁盘可配置多个目录，用逗号分隔：`/data1,/data2,/data3`
- **数据持久化**: 应该使用 Docker volume 或主机目录挂载

### CLUSTER_ID
```yaml
CLUSTER_ID: MkU3OEVBNTcwNTJENDM2Qk
```
- **说明**: Kafka 集群的唯一标识符
- **格式**: Base64 编码的 UUID
- **作用**: 在 KRaft 模式下标识集群身份
- **要求**: 
  - 所有节点必须使用相同的 CLUSTER_ID
  - 不同集群必须使用不同的 CLUSTER_ID
- **生成方法**:
  ```bash
  docker exec -it kafka-1 kafka-storage random-uuid
  ```
- **注意**: 一旦设置，不能更改；否则会导致集群无法识别

## 配置最佳实践

### 开发环境
- 副本因子: 1
- MIN_ISR: 1
- 安全协议: PLAINTEXT
- 数据存储: 临时目录

### 生产环境
- 副本因子: 3（至少）
- MIN_ISR: 2
- 安全协议: SASL_SSL
- 数据存储: 持久化卷 + SSD
- 监控和告警: 必须配置

### 高可用配置
- 至少 3 个节点
- 副本因子 ≥ 3
- MIN_ISR = 副本因子 - 1
- 跨可用区部署

## 端口使用总结

| 节点 | 内部端口 | 外部端口 | Controller 端口 |
|------|----------|----------|-----------------|
| kafka-1 | 19092 | 19093 | 19094 |
| kafka-2 | 29092 | 29093 | 29094 |
| kafka-3 | 39092 | 39093 | 39094 |

## 网络通信流程

1. **客户端连接**: localhost:19093/29093/39093
2. **返回地址**: Broker 返回 ADVERTISED_LISTENERS 中的地址
3. **数据传输**: 客户端使用返回的地址进行后续通信
4. **Broker 间通信**: 使用内部端口（19092/29092/39092）
5. **Controller 通信**: 使用 controller 端口（19094/29094/39094）

## 故障容错

| 场景 | 配置 | 结果 |
|------|------|------|
| 1 个节点故障 | 副本因子=3, MIN_ISR=2 | ✅ 正常服务 |
| 2 个节点故障 | 副本因子=3, MIN_ISR=2 | ❌ 无法写入（ISR < MIN_ISR） |
| 2 个节点故障 | 副本因子=3, MIN_ISR=1 | ✅ 可以写入（但仅1副本，高风险） |

## 参考资料

- [Kafka KRaft 官方文档](https://kafka.apache.org/documentation/#kraft)
- [Kafka Configuration 参考](https://kafka.apache.org/documentation/#configuration)
- [Kafka 副本机制](https://kafka.apache.org/documentation/#replication)
