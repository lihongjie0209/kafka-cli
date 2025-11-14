use anyhow::{Context, Result};
use rdkafka::consumer::{Consumer, StreamConsumer};
use rdkafka::{ClientConfig, Message};
use futures_util::StreamExt;

pub struct KafkaConsumer {
    pub consumer: StreamConsumer,
}

impl KafkaConsumer {
    pub fn new(config: ClientConfig, topics: &[&str]) -> Result<Self> {
        let consumer: StreamConsumer = config
            .create()
            .context("Failed to create consumer")?;
        
        if !topics.is_empty() {
            consumer
                .subscribe(topics)
                .context("Failed to subscribe to topics")?;
        }
        
        Ok(Self { consumer })
    }
    
    pub async fn consume_messages(
        &self,
        topic: &str,
        max_messages: Option<usize>,
        formatter: &str,
    ) -> Result<()> {
        self.consumer
            .subscribe(&[topic])
            .context("Failed to subscribe to topic")?;
        
        println!("Consuming messages from topic '{}' (Ctrl+C to exit)...", topic);
        
        let mut message_stream = self.consumer.stream();
        let mut count = 0;
        
        while let Some(message_result) = message_stream.next().await {
            match message_result {
                Ok(message) => {
                    self.print_message(&message, formatter)?;
                    count += 1;
                    
                    if let Some(max) = max_messages {
                        if count >= max {
                            println!("\nReached max messages: {}", max);
                            break;
                        }
                    }
                }
                Err(err) => {
                    eprintln!("Error consuming message: {}", err);
                }
            }
        }
        
        println!("\nConsumed {} message(s)", count);
        
        Ok(())
    }
    
    fn print_message(&self, message: &impl Message, formatter: &str) -> Result<()> {
        match formatter {
            "json" => {
                let key = message.key()
                    .map(|k| String::from_utf8_lossy(k).to_string())
                    .unwrap_or_default();
                
                let value = message.payload()
                    .map(|v| String::from_utf8_lossy(v).to_string())
                    .unwrap_or_default();
                
                let json = serde_json::json!({
                    "topic": message.topic(),
                    "partition": message.partition(),
                    "offset": message.offset(),
                    "timestamp": message.timestamp().to_millis(),
                    "key": key,
                    "value": value,
                });
                
                println!("{}", json);
            }
            _ => {
                // Default format
                let key = message.key()
                    .map(|k| String::from_utf8_lossy(k).to_string())
                    .unwrap_or_else(|| "null".to_string());
                
                let value = message.payload()
                    .map(|v| String::from_utf8_lossy(v).to_string())
                    .unwrap_or_default();
                
                println!("{}:{}", key, value);
            }
        }
        
        Ok(())
    }
}
