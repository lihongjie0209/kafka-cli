use anyhow::Result;
use crate::cli::{TopicsArgs, TopicAction};
use crate::config;
use crate::kafka::KafkaAdmin;

pub async fn handle_topics_command(args: TopicsArgs) -> Result<()> {
    let client_config = config::build_client_config(
        &args.bootstrap_server,
        args.command_config.as_deref(),
        &[],
    )?;
    
    let admin = KafkaAdmin::new(client_config)?;
    
    match args.action {
        TopicAction::List => {
            let topics = admin.list_topics().await?;
            
            if topics.is_empty() {
                println!("No topics found");
            } else {
                println!("Topics:");
                for topic in topics {
                    println!("  {}", topic);
                }
            }
        }
        
        TopicAction::Create {
            topic,
            partitions,
            replication_factor,
            config: topic_config,
        } => {
            let config_map = config::parse_properties(&topic_config);
            admin.create_topic(&topic, partitions, replication_factor, config_map).await?;
            println!("Topic '{}' created successfully", topic);
        }
        
        TopicAction::Describe { topic } => {
            admin.describe_topics(&topic).await?;
        }
        
        TopicAction::Delete { topic } => {
            admin.delete_topics(&topic).await?;
        }
        
        TopicAction::Alter {
            topic,
            add_config,
            delete_config,
        } => {
            let configs_to_set = config::parse_properties(&add_config);
            admin.alter_configs("topic", &topic, configs_to_set, delete_config).await?;
        }
    }
    
    Ok(())
}
