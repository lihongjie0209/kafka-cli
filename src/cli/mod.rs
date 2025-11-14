use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "kafka-cli")]
#[command(version = "0.1.0")]
#[command(about = "A cross-platform Kafka CLI tool", long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Manage Kafka topics (create, list, describe, delete, alter)
    Topics(TopicsArgs),
    
    /// Produce messages to Kafka topics
    Produce(ProduceArgs),
    
    /// Consume messages from Kafka topics
    Consume(ConsumeArgs),
    
    /// Manage consumer groups
    ConsumerGroups(ConsumerGroupsArgs),
    
    /// Manage Kafka configurations
    Configs(ConfigsArgs),
}

#[derive(Parser)]
pub struct TopicsArgs {
    /// Bootstrap server(s) to connect to
    #[arg(long, default_value = "localhost:9092")]
    pub bootstrap_server: String,
    
    /// Command configuration file
    #[arg(long)]
    pub command_config: Option<String>,
    
    #[command(subcommand)]
    pub action: TopicAction,
}

#[derive(Subcommand)]
pub enum TopicAction {
    /// List all topics
    List,
    
    /// Create a new topic
    Create {
        /// Topic name
        #[arg(long)]
        topic: String,
        
        /// Number of partitions
        #[arg(long, default_value = "1")]
        partitions: i32,
        
        /// Replication factor
        #[arg(long, default_value = "1")]
        replication_factor: i32,
        
        /// Topic configuration (key=value format)
        #[arg(long)]
        config: Vec<String>,
    },
    
    /// Describe topic(s)
    Describe {
        /// Topic name(s) to describe
        #[arg(long)]
        topic: Vec<String>,
    },
    
    /// Delete topic(s)
    Delete {
        /// Topic name(s) to delete
        #[arg(long)]
        topic: Vec<String>,
    },
    
    /// Alter topic configuration
    Alter {
        /// Topic name
        #[arg(long)]
        topic: String,
        
        /// Add or update configuration (key=value format)
        #[arg(long)]
        add_config: Vec<String>,
        
        /// Delete configuration key
        #[arg(long)]
        delete_config: Vec<String>,
    },
}

#[derive(Parser)]
pub struct ProduceArgs {
    /// Bootstrap server(s) to connect to
    #[arg(long, default_value = "localhost:9092")]
    pub bootstrap_server: String,
    
    /// Topic to produce to
    #[arg(long)]
    pub topic: String,
    
    /// Message key separator (default: tab)
    #[arg(long)]
    pub key_separator: Option<String>,
    
    /// Property in key=value format
    #[arg(long)]
    pub property: Vec<String>,
    
    /// Compression type
    #[arg(long)]
    pub compression_type: Option<String>,
    
    /// Command configuration file
    #[arg(long)]
    pub command_config: Option<String>,
}

#[derive(Parser)]
pub struct ConsumeArgs {
    /// Bootstrap server(s) to connect to
    #[arg(long, default_value = "localhost:9092")]
    pub bootstrap_server: String,
    
    /// Topic to consume from
    #[arg(long)]
    pub topic: String,
    
    /// Consumer group ID
    #[arg(long)]
    pub group: Option<String>,
    
    /// Consume from beginning
    #[arg(long)]
    pub from_beginning: bool,
    
    /// Maximum number of messages to consume
    #[arg(long)]
    pub max_messages: Option<usize>,
    
    /// Property in key=value format
    #[arg(long)]
    pub property: Vec<String>,
    
    /// Command configuration file
    #[arg(long)]
    pub command_config: Option<String>,
    
    /// Output format (default, json)
    #[arg(long, default_value = "default")]
    pub formatter: String,
}

#[derive(Parser)]
pub struct ConsumerGroupsArgs {
    /// Bootstrap server(s) to connect to
    #[arg(long, default_value = "localhost:9092")]
    pub bootstrap_server: String,
    
    /// Command configuration file
    #[arg(long)]
    pub command_config: Option<String>,
    
    #[command(subcommand)]
    pub action: ConsumerGroupAction,
}

#[derive(Subcommand)]
pub enum ConsumerGroupAction {
    /// List all consumer groups
    List,
    
    /// Describe consumer group(s)
    Describe {
        /// Consumer group ID(s)
        #[arg(long)]
        group: Vec<String>,
    },
    
    /// Delete consumer group(s)
    Delete {
        /// Consumer group ID(s)
        #[arg(long)]
        group: Vec<String>,
    },
    
    /// Reset consumer group offsets
    ResetOffsets {
        /// Consumer group ID
        #[arg(long)]
        group: String,
        
        /// Topic to reset
        #[arg(long)]
        topic: String,
        
        /// Reset to earliest offset
        #[arg(long)]
        to_earliest: bool,
        
        /// Reset to latest offset
        #[arg(long)]
        to_latest: bool,
        
        /// Reset to specific offset
        #[arg(long)]
        to_offset: Option<i64>,
        
        /// Reset to offset by timestamp (milliseconds since epoch)
        #[arg(long)]
        to_datetime: Option<i64>,
        
        /// Specific partitions to reset (comma-separated, e.g., 0,1,2)
        #[arg(long, value_delimiter = ',')]
        partitions: Option<Vec<i32>>,
        
        /// Execute the reset (dry-run if not specified)
        #[arg(long)]
        execute: bool,
    },
}

#[derive(Parser)]
pub struct ConfigsArgs {
    /// Bootstrap server(s) to connect to
    #[arg(long, default_value = "localhost:9092")]
    pub bootstrap_server: String,
    
    /// Command configuration file
    #[arg(long)]
    pub command_config: Option<String>,
    
    #[command(subcommand)]
    pub action: ConfigAction,
}

#[derive(Subcommand)]
pub enum ConfigAction {
    /// Describe configurations
    Describe {
        /// Entity type (topics, brokers, clients)
        #[arg(long)]
        entity_type: String,
        
        /// Entity name
        #[arg(long)]
        entity_name: String,
    },
    
    /// Alter configurations
    Alter {
        /// Entity type (topics, brokers, clients)
        #[arg(long)]
        entity_type: String,
        
        /// Entity name
        #[arg(long)]
        entity_name: String,
        
        /// Add or update configuration (key=value format)
        #[arg(long)]
        add_config: Vec<String>,
        
        /// Delete configuration key
        #[arg(long)]
        delete_config: Vec<String>,
    },
}
