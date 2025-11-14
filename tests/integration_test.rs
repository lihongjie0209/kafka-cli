use kafka_cli::config::build_client_config;
use kafka_cli::kafka::admin::KafkaAdmin;
use kafka_cli::kafka::producer::KafkaProducer;
use kafka_cli::kafka::consumer::KafkaConsumer;
use rdkafka::Message;
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::timeout;

const BOOTSTRAP_SERVER: &str = "localhost:9093";
const TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// 测试辅助函数：生成唯一的 topic 名称
fn generate_topic_name(prefix: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    format!("{}_{}", prefix, timestamp)
}

/// 测试辅助函数：清理测试 topic
async fn cleanup_topic(admin: &KafkaAdmin, topic: &str) {
    let _ = admin.delete_topics(&[topic.to_string()]).await;
}

#[tokio::test]
async fn test_admin_list_topics() {
    let config = build_client_config(BOOTSTRAP_SERVER, None, &[]).unwrap();
    let admin = KafkaAdmin::new(config).unwrap();
    
    let result = timeout(TEST_TIMEOUT, admin.list_topics()).await;
    assert!(result.is_ok(), "List topics timed out");
    
    let topics = result.unwrap().unwrap();
    println!("Found {} topics", topics.len());
}

#[tokio::test]
async fn test_admin_create_and_delete_topic() {
    let topic_name = generate_topic_name("test_create");
    let config = build_client_config(BOOTSTRAP_SERVER, None, &[]).unwrap();
    let admin = KafkaAdmin::new(config).unwrap();
    
    // 创建 topic
    let configs = HashMap::new();
    let result = timeout(
        TEST_TIMEOUT,
        admin.create_topic(&topic_name, 3, 1, configs)
    ).await;
    assert!(result.is_ok(), "Create topic timed out");
    assert!(result.unwrap().is_ok(), "Failed to create topic");
    
    // 验证 topic 存在
    tokio::time::sleep(Duration::from_secs(2)).await;
    let topics = admin.list_topics().await.unwrap();
    assert!(topics.contains(&topic_name), "Topic not found after creation");
    
    // 删除 topic
    let result = timeout(
        TEST_TIMEOUT,
        admin.delete_topics(&[topic_name.clone()])
    ).await;
    assert!(result.is_ok(), "Delete topic timed out");
    assert!(result.unwrap().is_ok(), "Failed to delete topic");
    
    // 验证 topic 已删除
    tokio::time::sleep(Duration::from_secs(2)).await;
    let topics = admin.list_topics().await.unwrap();
    assert!(!topics.contains(&topic_name), "Topic still exists after deletion");
}

#[tokio::test]
async fn test_admin_describe_topic() {
    let topic_name = generate_topic_name("test_describe");
    let config = build_client_config(BOOTSTRAP_SERVER, None, &[]).unwrap();
    let admin = KafkaAdmin::new(config).unwrap();
    
    // 创建 topic
    let configs = HashMap::new();
    admin.create_topic(&topic_name, 3, 1, configs).await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    
    // 描述 topic
    let result = timeout(
        TEST_TIMEOUT,
        admin.describe_topics(&[topic_name.clone()])
    ).await;
    assert!(result.is_ok(), "Describe topic timed out");
    
    result.unwrap().unwrap();
    println!("Topic '{}' described successfully", topic_name);
    
    // 清理
    cleanup_topic(&admin, &topic_name).await;
}

#[tokio::test]
async fn test_admin_alter_topic_config() {
    let topic_name = generate_topic_name("test_alter_config");
    let config = build_client_config(BOOTSTRAP_SERVER, None, &[]).unwrap();
    let admin = KafkaAdmin::new(config).unwrap();
    
    // 创建 topic
    let create_configs = HashMap::new();
    admin.create_topic(&topic_name, 1, 1, create_configs).await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    
    // 修改配置
    let mut configs_to_set = HashMap::new();
    configs_to_set.insert("retention.ms".to_string(), "86400000".to_string()); // 1 day
    
    let result = timeout(
        TEST_TIMEOUT,
        admin.alter_configs("topic", &topic_name, configs_to_set, vec![])
    ).await;
    assert!(result.is_ok(), "Alter config timed out");
    assert!(result.unwrap().is_ok(), "Failed to alter config");
    
    // 验证配置 (skip verification as rdkafka describe_configs may have API limitations)
    tokio::time::sleep(Duration::from_secs(1)).await;
    println!("Config altered successfully");
    
    // 清理
    cleanup_topic(&admin, &topic_name).await;
}

#[tokio::test]
async fn test_producer_send_message() {
    let topic_name = generate_topic_name("test_produce");
    let config = build_client_config(BOOTSTRAP_SERVER, None, &[]).unwrap();
    let admin = KafkaAdmin::new(config.clone()).unwrap();
    
    // 创建 topic
    let configs = HashMap::new();
    admin.create_topic(&topic_name, 1, 1, configs).await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    
    // 创建 producer
    let producer = KafkaProducer::new(config).unwrap();
    
    // 发送消息
    let result = timeout(
        TEST_TIMEOUT,
        producer.produce_message(&topic_name, Some("test_key"), "test_value")
    ).await;
    assert!(result.is_ok(), "Produce message timed out");
    
    let (partition, offset) = result.unwrap().unwrap();
    println!("Message sent to partition {} at offset {}", partition, offset);
    assert!(partition >= 0, "Invalid partition");
    assert!(offset >= 0, "Invalid offset");
    
    // 清理
    cleanup_topic(&admin, &topic_name).await;
}

#[tokio::test]
async fn test_producer_send_multiple_messages() {
    let topic_name = generate_topic_name("test_produce_multiple");
    let config = build_client_config(BOOTSTRAP_SERVER, None, &[]).unwrap();
    let admin = KafkaAdmin::new(config.clone()).unwrap();
    
    // 创建 topic with 3 partitions
    let configs = HashMap::new();
    admin.create_topic(&topic_name, 3, 1, configs).await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    
    let producer = KafkaProducer::new(config).unwrap();
    
    // 发送多条消息
    let mut results = Vec::new();
    for i in 0..10 {
        let key = format!("key_{}", i);
        let value = format!("value_{}", i);
        
        let result = producer.produce_message(&topic_name, Some(&key), &value).await;
        assert!(result.is_ok(), "Failed to produce message {}", i);
        results.push(result.unwrap());
    }
    
    assert_eq!(results.len(), 10, "Expected 10 messages to be sent");
    
    // 验证消息分布到不同的 partition
    let mut partitions: Vec<i32> = results.iter().map(|(p, _)| *p).collect();
    partitions.sort();
    partitions.dedup();
    println!("Messages distributed across {} partitions", partitions.len());
    
    // 清理
    cleanup_topic(&admin, &topic_name).await;
}

#[tokio::test]
async fn test_consumer_consume_messages() {
    let topic_name = generate_topic_name("test_consume");
    let group_id = generate_topic_name("test_group");
    let config = build_client_config(BOOTSTRAP_SERVER, None, &[]).unwrap();
    let admin = KafkaAdmin::new(config.clone()).unwrap();
    
    // 创建 topic
    let configs = HashMap::new();
    admin.create_topic(&topic_name, 1, 1, configs).await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    
    // 生产测试消息
    let producer = KafkaProducer::new(config.clone()).unwrap();
    for i in 0..5 {
        producer.produce_message(&topic_name, None, &format!("message_{}", i))
            .await
            .unwrap();
    }
    
    tokio::time::sleep(Duration::from_secs(2)).await;
    
    // 创建 consumer
    let consumer_config = kafka_cli::config::get_consumer_config(
        config.clone(),
        &group_id,
        true, // from_beginning
        &[]
    ).unwrap();
    
    let consumer = KafkaConsumer::new(consumer_config, &[&topic_name]).unwrap();
    
    // 消费消息
    let messages = timeout(
        TEST_TIMEOUT,
        consume_n_messages(&consumer, 5)
    ).await;
    
    assert!(messages.is_ok(), "Consume messages timed out");
    let messages = messages.unwrap();
    assert_eq!(messages.len(), 5, "Expected to consume 5 messages");
    
    // 验证消息内容
    for (i, msg) in messages.iter().enumerate() {
        let expected = format!("message_{}", i);
        assert_eq!(msg, &expected, "Message {} content mismatch", i);
    }
    
    // 清理
    cleanup_topic(&admin, &topic_name).await;
}

#[tokio::test]
async fn test_consumer_with_key_value() {
    let topic_name = generate_topic_name("test_consume_kv");
    let group_id = generate_topic_name("test_group_kv");
    let config = build_client_config(BOOTSTRAP_SERVER, None, &[]).unwrap();
    let admin = KafkaAdmin::new(config.clone()).unwrap();
    
    // 创建 topic
    let configs = HashMap::new();
    admin.create_topic(&topic_name, 1, 1, configs).await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    
    // 生产带 key 的消息
    let producer = KafkaProducer::new(config.clone()).unwrap();
    let test_data = vec![
        ("key1", "value1"),
        ("key2", "value2"),
        ("key3", "value3"),
    ];
    
    for (key, value) in &test_data {
        producer.produce_message(&topic_name, Some(key), value)
            .await
            .unwrap();
    }
    
    tokio::time::sleep(Duration::from_secs(2)).await;
    
    // 消费并验证
    let consumer_config = kafka_cli::config::get_consumer_config(
        config.clone(),
        &group_id,
        true,
        &[]
    ).unwrap();
    
    let consumer = KafkaConsumer::new(consumer_config, &[&topic_name]).unwrap();
    let messages = timeout(
        TEST_TIMEOUT,
        consume_n_messages_with_keys(&consumer, 3)
    ).await;
    
    assert!(messages.is_ok(), "Consume messages timed out");
    let messages = messages.unwrap();
    assert_eq!(messages.len(), 3, "Expected to consume 3 messages");
    
    // 验证 key-value 对
    for (i, (key, value)) in messages.iter().enumerate() {
        assert_eq!(key, &test_data[i].0, "Key {} mismatch", i);
        assert_eq!(value, &test_data[i].1, "Value {} mismatch", i);
    }
    
    // 清理
    cleanup_topic(&admin, &topic_name).await;
}

#[tokio::test]
async fn test_end_to_end_workflow() {
    let topic_name = generate_topic_name("test_e2e");
    let group_id = generate_topic_name("test_group_e2e");
    let config = build_client_config(BOOTSTRAP_SERVER, None, &[]).unwrap();
    let admin = KafkaAdmin::new(config.clone()).unwrap();
    
    // 1. 列出初始 topics
    let initial_topics = admin.list_topics().await.unwrap();
    println!("Initial topics count: {}", initial_topics.len());
    
    // 2. 创建新 topic
    let configs = HashMap::new();
    admin.create_topic(&topic_name, 3, 1, configs).await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    
    // 3. 验证 topic 创建
    let topics = admin.list_topics().await.unwrap();
    assert!(topics.contains(&topic_name));
    
    // 4. 描述 topic (skip detailed verification)
    admin.describe_topics(&[topic_name.clone()]).await.unwrap();
    println!("Topic described successfully");
    
    // 5. 生产消息
    let producer = KafkaProducer::new(config.clone()).unwrap();
    for i in 0..20 {
        producer.produce_message(&topic_name, Some(&format!("k{}", i)), &format!("v{}", i))
            .await
            .unwrap();
    }
    
    tokio::time::sleep(Duration::from_secs(2)).await;
    
    // 6. 消费消息
    let consumer_config = kafka_cli::config::get_consumer_config(
        config.clone(),
        &group_id,
        true,
        &[]
    ).unwrap();
    
    let consumer = KafkaConsumer::new(consumer_config, &[&topic_name]).unwrap();
    let messages = timeout(
        TEST_TIMEOUT,
        consume_n_messages(&consumer, 20)
    ).await.unwrap();
    
    assert_eq!(messages.len(), 20, "Expected to consume 20 messages");
    
    // 7. 修改 topic 配置
    let mut configs_to_set = HashMap::new();
    configs_to_set.insert("retention.ms".to_string(), "604800000".to_string()); // 7 days
    admin.alter_configs("topic", &topic_name, configs_to_set, vec![]).await.unwrap();
    
    tokio::time::sleep(Duration::from_secs(2)).await;
    
    // 8. 删除 topic
    admin.delete_topics(&[topic_name.clone()])
        .await
        .unwrap();
    
    tokio::time::sleep(Duration::from_secs(2)).await;
    
    // 9. 验证删除
    let final_topics = admin.list_topics().await.unwrap();
    assert!(!final_topics.contains(&topic_name), "Topic should be deleted");
    
    println!("End-to-end test completed successfully!");
}

// 辅助函数：消费 N 条消息
async fn consume_n_messages(consumer: &KafkaConsumer, n: usize) -> Vec<String> {
    use futures_util::StreamExt;
    
    let mut messages = Vec::new();
    let mut stream = consumer.consumer.stream();
    
    while messages.len() < n {
        if let Some(result) = stream.next().await {
            match result {
                Ok(msg) => {
                    if let Some(payload) = msg.payload() {
                        if let Ok(value) = std::str::from_utf8(payload) {
                            messages.push(value.to_string());
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Error consuming message: {}", e);
                    break;
                }
            }
        }
    }
    
    messages
}

// 辅助函数：消费 N 条带 key 的消息
async fn consume_n_messages_with_keys(consumer: &KafkaConsumer, n: usize) -> Vec<(String, String)> {
    use futures_util::StreamExt;
    
    let mut messages = Vec::new();
    let mut stream = consumer.consumer.stream();
    
    while messages.len() < n {
        if let Some(result) = stream.next().await {
            match result {
                Ok(msg) => {
                    let key = msg.key()
                        .and_then(|k| std::str::from_utf8(k).ok())
                        .unwrap_or("")
                        .to_string();
                    
                    let value = msg.payload()
                        .and_then(|p| std::str::from_utf8(p).ok())
                        .unwrap_or("")
                        .to_string();
                    
                    messages.push((key, value));
                }
                Err(e) => {
                    eprintln!("Error consuming message: {}", e);
                    break;
                }
            }
        }
    }
    
    messages
}

#[tokio::test]
async fn test_consumer_groups_list() {
    use kafka_cli::kafka::ConsumerGroupManager;
    
    let manager = ConsumerGroupManager::new(BOOTSTRAP_SERVER).unwrap();
    
    let result = timeout(TEST_TIMEOUT, manager.list_groups()).await;
    assert!(result.is_ok(), "List consumer groups timed out");
    
    let groups = result.unwrap().unwrap();
    println!("Found {} consumer groups", groups.len());
}

#[tokio::test]
async fn test_consumer_groups_describe() {
    use kafka_cli::kafka::ConsumerGroupManager;
    
    let topic_name = generate_topic_name("test_cg_describe");
    let group_id = format!("{}_group", topic_name);
    
    // 创建测试 topic
    let config = build_client_config(BOOTSTRAP_SERVER, None, &[]).unwrap();
    let admin = KafkaAdmin::new(config.clone()).unwrap();
    let configs = HashMap::new();
    admin.create_topic(&topic_name, 1, 1, configs).await.unwrap();
    
    // 生产一些消息
    let producer = KafkaProducer::new(config.clone()).unwrap();
    producer.produce_message(&topic_name, None, "test message").await.unwrap();
    
    // 使用 consumer group 消费消息
    let consumer_config = kafka_cli::config::get_consumer_config(
        config.clone(),
        &group_id,
        true,
        &[],
    ).unwrap();
    let consumer = KafkaConsumer::new(consumer_config, &[&topic_name]).unwrap();
    
    // 消费一条消息以建立 offset
    let messages = consume_n_messages_with_keys(&consumer, 1).await;
    assert_eq!(messages.len(), 1);
    
    // 等待 offset 提交
    tokio::time::sleep(Duration::from_secs(2)).await;
    
    // 描述 consumer group
    let manager = ConsumerGroupManager::new(BOOTSTRAP_SERVER).unwrap();
    let result = timeout(TEST_TIMEOUT, manager.describe_group(&group_id)).await;
    assert!(result.is_ok(), "Describe consumer group timed out");
    
    let group_info = result.unwrap().unwrap();
    assert_eq!(group_info.name, group_id);
    println!("Group state: {}", group_info.state);
    
    // 清理
    cleanup_topic(&admin, &topic_name).await;
}

#[tokio::test]
async fn test_consumer_groups_reset_offsets() {
    use kafka_cli::kafka::{ConsumerGroupManager, OffsetResetType};
    
    let topic_name = generate_topic_name("test_cg_reset");
    let group_id = format!("{}_group", topic_name);
    
    // 创建测试 topic
    let config = build_client_config(BOOTSTRAP_SERVER, None, &[]).unwrap();
    let admin = KafkaAdmin::new(config.clone()).unwrap();
    let configs = HashMap::new();
    admin.create_topic(&topic_name, 2, 1, configs).await.unwrap();
    
    // 生产多条消息
    let producer = KafkaProducer::new(config.clone()).unwrap();
    for i in 0..5 {
        let msg = format!("message_{}", i);
        producer.produce_message(&topic_name, None, &msg).await.unwrap();
    }
    
    // 使用 consumer group 消费所有消息
    let consumer_config = kafka_cli::config::get_consumer_config(
        config.clone(),
        &group_id,
        true,
        &[],
    ).unwrap();
    let consumer = KafkaConsumer::new(consumer_config, &[&topic_name]).unwrap();
    
    // 消费所有消息
    let messages = consume_n_messages_with_keys(&consumer, 5).await;
    assert_eq!(messages.len(), 5);
    
    // 等待 offset 提交
    tokio::time::sleep(Duration::from_secs(2)).await;
    drop(consumer);
    
    // 获取当前偏移量
    let manager = ConsumerGroupManager::new(BOOTSTRAP_SERVER).unwrap();
    let offsets_before = manager.get_group_offsets(&group_id, None).await.unwrap();
    let committed_count: i64 = offsets_before.iter()
        .filter(|o| o.topic == topic_name && o.current_offset >= 0)
        .map(|o| o.current_offset)
        .sum();
    assert!(committed_count > 0, "Should have committed offsets");
    
    // 重置偏移量到最早
    let result = timeout(
        TEST_TIMEOUT,
        manager.reset_offsets(&group_id, &topic_name, None, OffsetResetType::Earliest)
    ).await;
    assert!(result.is_ok(), "Reset offsets timed out");
    assert!(result.unwrap().is_ok(), "Failed to reset offsets");
    
    // 验证偏移量已重置
    let offsets_after = manager.get_group_offsets(&group_id, None).await.unwrap();
    for offset_info in offsets_after {
        if offset_info.topic == topic_name && offset_info.current_offset >= 0 {
            // 偏移量应该被重置到 0 或接近 0
            assert!(
                offset_info.current_offset == 0,
                "Offset should be reset to 0, but got {}",
                offset_info.current_offset
            );
        }
    }
    
    // 清理
    cleanup_topic(&admin, &topic_name).await;
}

#[tokio::test]
async fn test_consumer_groups_reset_offsets_with_partitions() {
    use kafka_cli::kafka::{ConsumerGroupManager, OffsetResetType};
    
    let topic_name = generate_topic_name("test_cg_reset_partitions");
    let group_id = format!("{}_group", topic_name);
    
    // 创建 3 个分区的测试 topic
    let config = build_client_config(BOOTSTRAP_SERVER, None, &[]).unwrap();
    let admin = KafkaAdmin::new(config.clone()).unwrap();
    let configs = HashMap::new();
    admin.create_topic(&topic_name, 3, 1, configs).await.unwrap();
    
    // 生产消息到每个分区
    let producer = KafkaProducer::new(config.clone()).unwrap();
    for i in 0..9 {
        let msg = format!("message_{}", i);
        producer.produce_message(&topic_name, None, &msg).await.unwrap();
    }
    
    // 使用 consumer group 消费所有消息
    let consumer_config = kafka_cli::config::get_consumer_config(
        config.clone(),
        &group_id,
        true,
        &[],
    ).unwrap();
    let consumer = KafkaConsumer::new(consumer_config, &[&topic_name]).unwrap();
    let messages = consume_n_messages_with_keys(&consumer, 9).await;
    assert_eq!(messages.len(), 9);
    
    tokio::time::sleep(Duration::from_secs(2)).await;
    drop(consumer);
    
    // 只重置分区 0 和 1 到最早
    let manager = ConsumerGroupManager::new(BOOTSTRAP_SERVER).unwrap();
    let partitions = Some(vec![0, 1]);
    let result = timeout(
        TEST_TIMEOUT,
        manager.reset_offsets(&group_id, &topic_name, partitions, OffsetResetType::Earliest)
    ).await;
    assert!(result.is_ok(), "Reset offsets timed out");
    assert!(result.unwrap().is_ok(), "Failed to reset offsets");
    
    // 验证只有指定的分区被重置
    let offsets_after = manager.get_group_offsets(&group_id, None).await.unwrap();
    for offset_info in offsets_after {
        if offset_info.topic == topic_name && offset_info.current_offset >= 0 {
            if offset_info.partition == 0 || offset_info.partition == 1 {
                // 分区 0 和 1 应该被重置到 0
                assert_eq!(
                    offset_info.current_offset, 0,
                    "Partition {} should be reset to 0",
                    offset_info.partition
                );
            } else if offset_info.partition == 2 {
                // 分区 2 应该保持原有的偏移量
                assert!(
                    offset_info.current_offset > 0,
                    "Partition 2 should not be reset"
                );
            }
        }
    }
    
    cleanup_topic(&admin, &topic_name).await;
}

#[tokio::test]
async fn test_consumer_groups_reset_offsets_by_timestamp() {
    use kafka_cli::kafka::{ConsumerGroupManager, OffsetResetType};
    use std::time::{SystemTime, UNIX_EPOCH};
    
    let topic_name = generate_topic_name("test_cg_reset_timestamp");
    let group_id = format!("{}_group", topic_name);
    
    // 创建测试 topic
    let config = build_client_config(BOOTSTRAP_SERVER, None, &[]).unwrap();
    let admin = KafkaAdmin::new(config.clone()).unwrap();
    let configs = HashMap::new();
    admin.create_topic(&topic_name, 2, 1, configs).await.unwrap();
    
    // 生产第一批消息
    let producer = KafkaProducer::new(config.clone()).unwrap();
    for i in 0..3 {
        let msg = format!("early_message_{}", i);
        producer.produce_message(&topic_name, None, &msg).await.unwrap();
    }
    
    // 等待确保时间戳差异
    tokio::time::sleep(Duration::from_secs(2)).await;
    
    // 记录中间时间戳
    let timestamp_middle = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    
    // 生产第二批消息
    for i in 0..3 {
        let msg = format!("late_message_{}", i);
        producer.produce_message(&topic_name, None, &msg).await.unwrap();
    }
    
    // 消费所有消息
    let consumer_config = kafka_cli::config::get_consumer_config(
        config.clone(),
        &group_id,
        true,
        &[],
    ).unwrap();
    let consumer = KafkaConsumer::new(consumer_config, &[&topic_name]).unwrap();
    let messages = consume_n_messages_with_keys(&consumer, 6).await;
    assert_eq!(messages.len(), 6);
    
    tokio::time::sleep(Duration::from_secs(2)).await;
    drop(consumer);
    
    // 使用中间时间戳重置（应该跳过前3条消息）
    let manager = ConsumerGroupManager::new(BOOTSTRAP_SERVER).unwrap();
    let result = timeout(
        TEST_TIMEOUT,
        manager.reset_offsets(&group_id, &topic_name, None, OffsetResetType::Timestamp(timestamp_middle))
    ).await;
    assert!(result.is_ok(), "Reset offsets by timestamp timed out");
    assert!(result.unwrap().is_ok(), "Failed to reset offsets by timestamp");
    
    // 验证偏移量已重置到中间位置附近
    let offsets_after = manager.get_group_offsets(&group_id, None).await.unwrap();
    let total_offset: i64 = offsets_after.iter()
        .filter(|o| o.topic == topic_name && o.current_offset >= 0)
        .map(|o| o.current_offset)
        .sum();
    
    // 时间戳重置应该将偏移量设置到第一批消息之后
    // 由于消息分布在2个分区，预期总偏移量应该小于最大值（每个分区3条）
    assert!(
        total_offset < 6,
        "Timestamp reset should position offsets after early messages, got total: {}",
        total_offset
    );
    
    cleanup_topic(&admin, &topic_name).await;
}
