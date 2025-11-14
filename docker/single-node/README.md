# Kafka 单节点 Docker Compose

这个配置启动一个单节点 Kafka 集群，包含 Zookeeper、Kafka 和 Kafka UI。

## 服务说明

- **Zookeeper**: Kafka 的协调服务，端口 2181
- **Kafka**: 消息代理服务
  - 内部通信端口: 9092
  - 外部访问端口: 9093 (localhost:9093)
- **Kafka UI**: Web 管理界面，端口 8080

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

- **从容器内连接**: kafka:9092
- **从宿主机连接**: localhost:9093

## 示例：使用 Kafka 命令行工具

### 创建 Topic

```bash
docker exec -it kafka kafka-topics --create --topic test-topic --bootstrap-server localhost:9092 --partitions 1 --replication-factor 1
```

### 列出 Topics

```bash
docker exec -it kafka kafka-topics --list --bootstrap-server localhost:9092
```

### 生产消息

```bash
docker exec -it kafka kafka-console-producer --topic test-topic --bootstrap-server localhost:9092
```

### 消费消息

```bash
docker exec -it kafka kafka-console-consumer --topic test-topic --from-beginning --bootstrap-server localhost:9092
```
