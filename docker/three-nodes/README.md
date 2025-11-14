# Kafka 三节点集群 Docker Compose

这个配置启动一个三节点 Kafka 集群，使用 KRaft 模式（无需 Zookeeper），包含 Kafka UI。

## 服务说明

- **kafka-1**: Kafka Broker 1
  - 内部通信端口: 19092
  - 外部访问端口: 19093 (localhost:19093)
  - Controller 端口: 19094
- **kafka-2**: Kafka Broker 2
  - 内部通信端口: 29092
  - 外部访问端口: 29093 (localhost:29093)
  - Controller 端口: 29094
- **kafka-3**: Kafka Broker 3
  - 内部通信端口: 39092
  - 外部访问端口: 39093 (localhost:39093)
  - Controller 端口: 39094
- **Kafka UI**: Web 管理界面，端口 8080

## 集群配置

- **副本因子**: 3
- **最小 ISR**: 2
- **事务日志副本因子**: 3

## 启动服务

```bash
docker-compose up -d
```

## 停止服务

```bash
docker-compose down
```

## 停止服务并清除数据

```bash
docker-compose down -v
```

## 访问 Kafka UI

启动后，在浏览器访问：http://localhost:8080

## Kafka 连接信息

- **从容器内连接**: kafka-1:19092,kafka-2:29092,kafka-3:39092
- **从宿主机连接**: localhost:19093,localhost:29093,localhost:39093

## 示例：使用 Kafka 命令行工具

### 创建 Topic (副本因子 3)

```bash
docker exec -it kafka-1 kafka-topics --create --topic test-topic --bootstrap-server kafka-1:19092 --partitions 3 --replication-factor 3
```

### 查看 Topic 详情

```bash
docker exec -it kafka-1 kafka-topics --describe --topic test-topic --bootstrap-server kafka-1:19092
```

### 列出所有 Topics

```bash
docker exec -it kafka-1 kafka-topics --list --bootstrap-server kafka-1:19092
```

### 生产消息

```bash
docker exec -it kafka-1 kafka-console-producer --topic test-topic --bootstrap-server kafka-1:19092
```

### 消费消息

```bash
docker exec -it kafka-1 kafka-console-consumer --topic test-topic --from-beginning --bootstrap-server kafka-1:19092
```

### 查看集群元数据

```bash
docker exec -it kafka-1 kafka-broker-api-versions --bootstrap-server kafka-1:19092
```

### 测试集群高可用性

1. 创建一个有副本的 topic
2. 停止一个节点：`docker stop kafka-1`
3. 验证服务仍然可用
4. 重启节点：`docker start kafka-1`
5. 验证数据同步

## 集群特性

- ✅ 高可用：任意一个节点故障不影响服务
- ✅ 数据冗余：3 个副本保证数据安全
- ✅ 负载均衡：分区分布在多个节点
- ✅ KRaft 模式：无需 Zookeeper
