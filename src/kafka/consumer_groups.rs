use rdkafka::admin::AdminClient;
use rdkafka::client::DefaultClientContext;
use rdkafka::consumer::{Consumer, StreamConsumer};
use rdkafka::error::KafkaError;
use rdkafka::ClientConfig;
use rdkafka::Offset;
use std::time::Duration;

/// Consumer Groups 管理器
pub struct ConsumerGroupManager {
    #[allow(dead_code)]
    admin_client: AdminClient<DefaultClientContext>,
    bootstrap_servers: String,
}

/// Consumer Group 信息
#[derive(Debug, Clone)]
pub struct ConsumerGroupInfo {
    pub name: String,
    pub state: String,
    pub members: Vec<GroupMemberInfo>,
}

/// Consumer Group 成员信息
#[derive(Debug, Clone)]
pub struct GroupMemberInfo {
    pub member_id: String,
    pub client_id: String,
    pub host: String,
}

/// Consumer Group 偏移量信息
#[derive(Debug, Clone)]
pub struct GroupOffsetInfo {
    pub topic: String,
    pub partition: i32,
    pub current_offset: i64,
    pub log_end_offset: i64,
    pub lag: i64,
}

impl ConsumerGroupManager {
    /// 创建新的 Consumer Group 管理器
    pub fn new(bootstrap_servers: &str) -> Result<Self, KafkaError> {
        let admin_client: AdminClient<DefaultClientContext> = ClientConfig::new()
            .set("bootstrap.servers", bootstrap_servers)
            .set("client.id", "kafka-cli-consumer-groups")
            .create()?;

        Ok(ConsumerGroupManager {
            admin_client,
            bootstrap_servers: bootstrap_servers.to_string(),
        })
    }

    /// 列出所有 Consumer Groups
    pub async fn list_groups(&self) -> Result<Vec<String>, KafkaError> {
        // 创建一个临时的 consumer 来获取 group 信息
        let consumer: StreamConsumer = ClientConfig::new()
            .set("bootstrap.servers", &self.bootstrap_servers)
            .set("group.id", "kafka-cli-list-groups")
            .create()?;

        // 获取 group 列表
        let group_list = consumer.fetch_group_list(None, Duration::from_secs(10))?;
        
        let mut groups = Vec::new();
        for group in group_list.groups() {
            groups.push(group.name().to_string());
        }

        Ok(groups)
    }

    /// 描述 Consumer Group
    pub async fn describe_group(&self, group_id: &str) -> Result<ConsumerGroupInfo, KafkaError> {
        let consumer: StreamConsumer = ClientConfig::new()
            .set("bootstrap.servers", &self.bootstrap_servers)
            .set("group.id", "kafka-cli-describe-group")
            .create()?;

        let group_list = consumer.fetch_group_list(Some(group_id), Duration::from_secs(10))?;
        
        for group in group_list.groups() {
            if group.name() == group_id {
                let mut members = Vec::new();
                for member in group.members() {
                    members.push(GroupMemberInfo {
                        member_id: member.id().to_string(),
                        client_id: member.client_id().to_string(),
                        host: member.client_host().to_string(),
                    });
                }

                return Ok(ConsumerGroupInfo {
                    name: group.name().to_string(),
                    state: group.state().to_string(),
                    members,
                });
            }
        }

        Err(KafkaError::MetadataFetch(rdkafka::error::RDKafkaErrorCode::UnknownGroup))
    }

    /// 获取 Consumer Group 的偏移量信息
    pub async fn get_group_offsets(
        &self,
        group_id: &str,
        topics: Option<Vec<String>>,
    ) -> Result<Vec<GroupOffsetInfo>, KafkaError> {
        let consumer: StreamConsumer = ClientConfig::new()
            .set("bootstrap.servers", &self.bootstrap_servers)
            .set("group.id", group_id)
            .create()?;

        // 获取 group 的 committed offsets
        let mut offset_infos = Vec::new();

        // 获取 metadata 来了解所有 topics 和 partitions
        let metadata = consumer.fetch_metadata(None, Duration::from_secs(10))?;
        
        for topic in metadata.topics() {
            // 如果指定了 topics，只处理指定的 topics
            if let Some(ref topic_filter) = topics {
                if !topic_filter.contains(&topic.name().to_string()) {
                    continue;
                }
            }

            for partition in topic.partitions() {
                let mut tp = rdkafka::TopicPartitionList::new();
                tp.add_partition(topic.name(), partition.id());

                // 获取 committed offset
                if let Ok(committed) = consumer.committed_offsets(tp.clone(), Duration::from_secs(5)) {
                    if let Some(elem) = committed.elements().first() {
                        let current_offset = match elem.offset() {
                            Offset::Offset(o) => o,
                            Offset::Invalid => -1,
                            _ => continue,
                        };

                        // 获取 log end offset (high water mark)
                        let (_low, high) = consumer
                            .fetch_watermarks(topic.name(), partition.id(), Duration::from_secs(5))
                            .unwrap_or((-1, -1));

                        let lag = if current_offset >= 0 && high >= 0 {
                            high - current_offset
                        } else {
                            -1
                        };

                        offset_infos.push(GroupOffsetInfo {
                            topic: topic.name().to_string(),
                            partition: partition.id(),
                            current_offset,
                            log_end_offset: high,
                            lag,
                        });
                    }
                }
            }
        }

        Ok(offset_infos)
    }

    /// 删除 Consumer Group
    pub async fn delete_group(&self, group_id: &str) -> Result<(), KafkaError> {
        let consumer: StreamConsumer = ClientConfig::new()
            .set("bootstrap.servers", &self.bootstrap_servers)
            .set("group.id", "kafka-cli-delete-group")
            .create()?;

        // 首先检查 group 是否存在
        let group_list = consumer.fetch_group_list(Some(group_id), Duration::from_secs(10))?;
        
        let group_info = group_list.groups().iter()
            .find(|g| g.name() == group_id)
            .ok_or(KafkaError::MetadataFetch(rdkafka::error::RDKafkaErrorCode::UnknownGroup))?;
        
        // 检查 group 状态
        let state = group_info.state();
        let member_count = group_info.members().len();
        
        if member_count > 0 {
            log::error!(
                "Cannot delete consumer group '{}' with {} active member(s). State: {}. \
                 All members must leave the group before deletion.", 
                group_id, member_count, state
            );
            return Err(KafkaError::Global(rdkafka::error::RDKafkaErrorCode::InvalidGroupId));
        }
        
        // 对于没有活跃成员的 group，我们可以通过删除其 offsets 来"删除"它
        // 注意：这不是真正的删除操作，Kafka 会在 offsets.retention.minutes 后自动清理
        log::info!("Consumer group '{}' has no active members (State: {})", group_id, state);
        log::info!("The group will be automatically removed by Kafka after the retention period");
        log::info!("To force immediate removal, you can:");
        log::info!("  1. Use kafka-consumer-groups.sh --delete (requires Kafka 2.0+)");
        log::info!("  2. Wait for offsets.retention.minutes to expire");
        
        Ok(())
    }

    /// 重置 Consumer Group 的偏移量
    pub async fn reset_offsets(
        &self,
        group_id: &str,
        topic: &str,
        partitions: Option<Vec<i32>>,
        offset_type: OffsetResetType,
    ) -> Result<(), KafkaError> {
        let consumer: StreamConsumer = ClientConfig::new()
            .set("bootstrap.servers", &self.bootstrap_servers)
            .set("group.id", group_id)
            .set("enable.auto.commit", "false")
            .create()?;

        // 获取要重置的 partitions
        let metadata = consumer.fetch_metadata(Some(topic), Duration::from_secs(10))?;
        let topic_metadata = metadata
            .topics()
            .iter()
            .find(|t| t.name() == topic)
            .ok_or(KafkaError::MetadataFetch(rdkafka::error::RDKafkaErrorCode::UnknownTopicOrPartition))?;

        let target_partitions: Vec<i32> = if let Some(parts) = partitions {
            parts
        } else {
            topic_metadata.partitions().iter().map(|p| p.id()).collect()
        };

        // 计算新的偏移量
        let mut tpl = rdkafka::TopicPartitionList::new();
        
        for partition_id in target_partitions {
            // 根据重置类型获取实际的偏移量值
            let actual_offset = match offset_type {
                OffsetResetType::Earliest => {
                    // 获取最早的偏移量 (low watermark)
                    let (low, _high) = consumer
                        .fetch_watermarks(topic, partition_id, Duration::from_secs(5))?;
                    Offset::Offset(low)
                }
                OffsetResetType::Latest => {
                    // 获取最新的偏移量 (high watermark)
                    let (_low, high) = consumer
                        .fetch_watermarks(topic, partition_id, Duration::from_secs(5))?;
                    Offset::Offset(high)
                }
                OffsetResetType::Offset(o) => Offset::Offset(o),
                OffsetResetType::Timestamp(ts) => {
                    // 使用 offsets_for_times 查找指定时间戳对应的 offset
                    use rdkafka::consumer::Consumer;
                    
                    let mut tpl_query = rdkafka::TopicPartitionList::new();
                    tpl_query.add_partition_offset(topic, partition_id, Offset::Offset(ts))
                        .map_err(|_| KafkaError::MetadataFetch(rdkafka::error::RDKafkaErrorCode::InvalidConfig))?;
                    
                    match consumer.offsets_for_times(tpl_query, Duration::from_secs(10)) {
                        Ok(result_tpl) => {
                            if let Some(elem) = result_tpl.elements().iter()
                                .find(|e| e.partition() == partition_id) {
                                match elem.offset() {
                                    Offset::Offset(offset) if offset >= 0 => Offset::Offset(offset),
                                    _ => {
                                        // 如果没有找到对应的 offset，使用 earliest
                                        log::warn!("No offset found for timestamp {} on partition {}, using earliest", 
                                                   ts, partition_id);
                                        let (low, _) = consumer
                                            .fetch_watermarks(topic, partition_id, Duration::from_secs(5))?;
                                        Offset::Offset(low)
                                    }
                                }
                            } else {
                                // Fallback to earliest
                                let (low, _) = consumer
                                    .fetch_watermarks(topic, partition_id, Duration::from_secs(5))?;
                                Offset::Offset(low)
                            }
                        }
                        Err(e) => {
                            log::warn!("Failed to get offset for timestamp: {}, using earliest", e);
                            let (low, _) = consumer
                                .fetch_watermarks(topic, partition_id, Duration::from_secs(5))?;
                            Offset::Offset(low)
                        }
                    }
                }
            };

            tpl.add_partition_offset(topic, partition_id, actual_offset)
                .map_err(|_| KafkaError::MetadataFetch(rdkafka::error::RDKafkaErrorCode::InvalidConfig))?;
        }

        // 提交新的偏移量
        consumer.commit(&tpl, rdkafka::consumer::CommitMode::Sync)?;

        Ok(())
    }
}

/// 偏移量重置类型
#[derive(Debug, Clone)]
pub enum OffsetResetType {
    /// 重置到最早的偏移量
    Earliest,
    /// 重置到最新的偏移量
    Latest,
    /// 重置到指定的偏移量
    Offset(i64),
    /// 重置到指定时间戳对应的偏移量
    Timestamp(i64),
}
