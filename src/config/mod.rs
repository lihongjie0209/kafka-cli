use anyhow::{Context, Result};
use rdkafka::ClientConfig;
use std::collections::HashMap;
use std::fs;

/// Load configuration from a properties file
pub fn load_config_file(path: &str) -> Result<HashMap<String, String>> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("Failed to read config file: {}", path))?;
    
    let mut config = HashMap::new();
    
    for line in contents.lines() {
        let line = line.trim();
        
        // Skip empty lines and comments
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        
        // Parse key=value pairs
        if let Some((key, value)) = line.split_once('=') {
            config.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    
    Ok(config)
}

/// Parse key=value pairs from command line arguments
pub fn parse_properties(properties: &[String]) -> HashMap<String, String> {
    let mut config = HashMap::new();
    
    for prop in properties {
        if let Some((key, value)) = prop.split_once('=') {
            config.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    
    config
}

/// Build a Kafka ClientConfig from various sources
pub fn build_client_config(
    bootstrap_servers: &str,
    command_config: Option<&str>,
    properties: &[String],
) -> Result<ClientConfig> {
    let mut config = ClientConfig::new();
    
    // Set bootstrap servers
    config.set("bootstrap.servers", bootstrap_servers);
    
    // Load from config file if provided
    if let Some(config_file) = command_config {
        let file_config = load_config_file(config_file)?;
        for (key, value) in file_config {
            config.set(key, value);
        }
    }
    
    // Apply command line properties (highest priority)
    let props = parse_properties(properties);
    for (key, value) in props {
        config.set(key, value);
    }
    
    Ok(config)
}

/// Get common consumer configuration (overload for testing)
pub fn get_consumer_config(
    config: ClientConfig,
    group_id: &str,
    from_beginning: bool,
    properties: &[String],
) -> Result<ClientConfig> {
    let mut config = config;
    
    config.set("group.id", group_id);
    
    if from_beginning {
        config.set("auto.offset.reset", "earliest");
    } else {
        config.set("auto.offset.reset", "latest");
    }
    
    let props = parse_properties(properties);
    for (key, value) in props {
        config.set(key, value);
    }
    
    config.set("enable.auto.commit", "true");
    
    Ok(config)
}

/// Get common consumer configuration (CLI version)
pub fn get_consumer_config_cli(
    bootstrap_servers: &str,
    group_id: Option<&str>,
    command_config: Option<&str>,
    properties: &[String],
    from_beginning: bool,
) -> Result<ClientConfig> {
    let mut config = build_client_config(bootstrap_servers, command_config, properties)?;
    
    // Set consumer group if provided
    if let Some(group) = group_id {
        config.set("group.id", group);
    } else {
        // Generate a unique group ID for one-time consumption
        let unique_id = format!("kafka-cli-consumer-{}", chrono::Utc::now().timestamp());
        config.set("group.id", unique_id);
    }
    
    // Set auto offset reset
    if from_beginning {
        config.set("auto.offset.reset", "earliest");
    } else {
        config.set("auto.offset.reset", "latest");
    }
    
    // Enable auto commit by default
    config.set("enable.auto.commit", "true");
    
    Ok(config)
}

/// Get common producer configuration
pub fn get_producer_config(
    bootstrap_servers: &str,
    command_config: Option<&str>,
    properties: &[String],
    compression_type: Option<&str>,
) -> Result<ClientConfig> {
    let mut config = build_client_config(bootstrap_servers, command_config, properties)?;
    
    // Set compression if specified
    if let Some(compression) = compression_type {
        config.set("compression.type", compression);
    }
    
    // Set some reasonable defaults
    config.set("message.timeout.ms", "30000");
    
    Ok(config)
}
