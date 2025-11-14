use anyhow::{Context, Result};
use rdkafka::producer::{FutureProducer, FutureRecord, Producer};
use rdkafka::ClientConfig;
use std::io::{self, BufRead};
use std::time::Duration;

pub struct KafkaProducer {
    producer: FutureProducer,
}

impl KafkaProducer {
    pub fn new(config: ClientConfig) -> Result<Self> {
        let producer: FutureProducer = config
            .create()
            .context("Failed to create producer")?;
        
        Ok(Self { producer })
    }
    
    pub async fn produce_from_stdin(
        &self,
        topic: &str,
        key_separator: Option<&str>,
    ) -> Result<()> {
        let stdin = io::stdin();
        let separator = key_separator.unwrap_or("\t");
        let mut message_count = 0;
        
        println!("Reading messages from stdin (Ctrl+C to exit)...");
        
        for line in stdin.lock().lines() {
            let line = line.context("Failed to read line from stdin")?;
            
            if line.is_empty() {
                continue;
            }
            
            let (key, value) = if let Some(pos) = line.find(separator) {
                let (k, v) = line.split_at(pos);
                (Some(k), &v[separator.len()..])
            } else {
                (None, line.as_str())
            };
            
            let mut record = FutureRecord::to(topic).payload(value);
            
            if let Some(k) = key {
                record = record.key(k);
            }
            
            match self.producer.send(record, Duration::from_secs(30)).await {
                Ok(delivery) => {
                    message_count += 1;
                    log::debug!("Message sent to partition {} at offset {}", delivery.partition, delivery.offset);
                }
                Err((err, _)) => {
                    eprintln!("Failed to send message: {}", err);
                }
            }
        }
        
        println!("Sent {} message(s)", message_count);
        
        Ok(())
    }
    
    pub async fn produce_message(
        &self,
        topic: &str,
        key: Option<&str>,
        value: &str,
    ) -> Result<(i32, i64)> {
        let mut record = FutureRecord::to(topic).payload(value);
        
        if let Some(k) = key {
            record = record.key(k);
        }
        
        let delivery = self.producer
            .send(record, Duration::from_secs(30))
            .await
            .map_err(|(err, _)| err)
            .context("Failed to send message")?;
        
        Ok((delivery.partition, delivery.offset))
    }
    
    pub async fn flush(&self) -> Result<()> {
        self.producer
            .flush(Duration::from_secs(30))
            .context("Failed to flush producer")
    }
}
