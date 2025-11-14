use anyhow::{Context, Result};
use rdkafka::admin::{AdminClient, AdminOptions, NewTopic, ResourceSpecifier, AlterConfig, TopicReplication};
use rdkafka::client::DefaultClientContext;
use rdkafka::ClientConfig;
use std::collections::HashMap;
use std::time::Duration;

pub struct KafkaAdmin {
    client: AdminClient<DefaultClientContext>,
}

impl KafkaAdmin {
    pub fn new(config: ClientConfig) -> Result<Self> {
        let client: AdminClient<DefaultClientContext> = config
            .create()
            .context("Failed to create admin client")?;
        
        Ok(Self { client })
    }
    
    pub async fn list_topics(&self) -> Result<Vec<String>> {
        let metadata = self.client
            .inner()
            .fetch_metadata(None, Duration::from_secs(10))
            .context("Failed to fetch metadata")?;
        
        let topics: Vec<String> = metadata
            .topics()
            .iter()
            .map(|t| t.name().to_string())
            .filter(|name| !name.starts_with("__"))  // Filter internal topics
            .collect();
        
        Ok(topics)
    }
    
    pub async fn create_topic(
        &self,
        name: &str,
        partitions: i32,
        replication_factor: i32,
        config: HashMap<String, String>,
    ) -> Result<()> {
        let replication = TopicReplication::Fixed(replication_factor);
        let mut new_topic = NewTopic::new(name, partitions, replication);
        
        // Convert to Vec to properly handle string lifetimes
        let config_vec: Vec<(String, String)> = config.into_iter().collect();
        for (key, value) in &config_vec {
            new_topic = new_topic.set(key.as_str(), value.as_str());
        }
        
        let opts = AdminOptions::new().operation_timeout(Some(Duration::from_secs(30)));
        
        let results = self.client
            .create_topics(&[new_topic], &opts)
            .await
            .context("Failed to create topic")?;
        
        for result in results {
            match result {
                Ok(topic) => println!("Created topic: {}", topic),
                Err((topic, err)) => {
                    anyhow::bail!("Failed to create topic '{}': {}", topic, err);
                }
            }
        }
        
        Ok(())
    }
    
    pub async fn delete_topics(&self, topics: &[String]) -> Result<()> {
        let topic_refs: Vec<&str> = topics.iter().map(|s| s.as_str()).collect();
        let opts = AdminOptions::new().operation_timeout(Some(Duration::from_secs(30)));
        
        let results = self.client
            .delete_topics(&topic_refs, &opts)
            .await
            .context("Failed to delete topics")?;
        
        for result in results {
            match result {
                Ok(topic) => println!("Deleted topic: {}", topic),
                Err((topic, err)) => {
                    eprintln!("Failed to delete topic '{}': {}", topic, err);
                }
            }
        }
        
        Ok(())
    }
    
    pub async fn describe_topics(&self, topic_names: &[String]) -> Result<()> {
        let metadata = self.client
            .inner()
            .fetch_metadata(None, Duration::from_secs(10))
            .context("Failed to fetch metadata")?;
        
        for topic in metadata.topics() {
            if topic_names.is_empty() || topic_names.contains(&topic.name().to_string()) {
                println!("\nTopic: {}", topic.name());
                println!("  Partitions: {}", topic.partitions().len());
                
                for partition in topic.partitions() {
                    println!("    Partition {}: Leader: {}, Replicas: {:?}, ISR: {:?}",
                        partition.id(),
                        partition.leader(),
                        partition.replicas(),
                        partition.isr()
                    );
                }
            }
        }
        
        Ok(())
    }
    
    pub async fn describe_configs(&self, entity_type: &str, entity_name: &str) -> Result<()> {
        let resource = match entity_type.to_lowercase().as_str() {
            "topics" | "topic" => ResourceSpecifier::Topic(entity_name),
            "brokers" | "broker" => {
                let broker_id = entity_name.parse::<i32>()
                    .context("Broker ID must be a number")?;
                ResourceSpecifier::Broker(broker_id)
            }
            _ => anyhow::bail!("Unsupported entity type: {}", entity_type),
        };
        
        let opts = AdminOptions::new().operation_timeout(Some(Duration::from_secs(30)));
        
        let results = self.client
            .describe_configs(&[resource], &opts)
            .await
            .context("Failed to describe configs")?;
        
        for result in results {
            match result {
                Ok(config_resource) => {
                    println!("\nConfigs for {} '{}':", entity_type, entity_name);
                    for entry in config_resource.entries {
                        println!("  {} = {} (source: {:?}, read-only: {}, sensitive: {})",
                            entry.name,
                            entry.value.unwrap_or_default(),
                            entry.source,
                            entry.is_read_only,
                            entry.is_sensitive
                        );
                    }
                }
                Err(err) => {
                    anyhow::bail!("Failed to describe configs: {}", err);
                }
            }
        }
        
        Ok(())
    }
    
    pub async fn alter_configs(
        &self,
        entity_type: &str,
        entity_name: &str,
        configs_to_set: HashMap<String, String>,
        configs_to_delete: Vec<String>,
    ) -> Result<()> {
        let resource = match entity_type.to_lowercase().as_str() {
            "topics" | "topic" => ResourceSpecifier::Topic(entity_name),
            "brokers" | "broker" => {
                let broker_id = entity_name.parse::<i32>()
                    .context("Broker ID must be a number")?;
                ResourceSpecifier::Broker(broker_id)
            }
            _ => anyhow::bail!("Unsupported entity type: {}", entity_type),
        };
        
        // Collect all config entries first to handle lifetimes properly
        let mut all_keys = Vec::new();
        let mut all_values = Vec::new();
        
        // Add configs to set
        for (key, value) in configs_to_set {
            all_keys.push(key);
            all_values.push(value);
        }
        
        // Add configs to delete (empty string value)
        for key in configs_to_delete {
            all_keys.push(key);
            all_values.push(String::new());
        }
        
        // Create config map with proper lifetimes
        let mut config_map = HashMap::new();
        for (i, key) in all_keys.iter().enumerate() {
            config_map.insert(key.as_str(), all_values[i].as_str());
        }
        
        let alter_config = AlterConfig {
            specifier: resource,
            entries: config_map,
        };
        
        let opts = AdminOptions::new().operation_timeout(Some(Duration::from_secs(30)));
        
        let results = self.client
            .alter_configs(&[alter_config], &opts)
            .await
            .context("Failed to alter configs")?;
        
        for result in results {
            match result {
                Ok(_) => println!("Successfully altered configs for {} '{}'", entity_type, entity_name),
                Err(err) => {
                    anyhow::bail!("Failed to alter configs: {:?}", err);
                }
            }
        }
        
        Ok(())
    }
}
