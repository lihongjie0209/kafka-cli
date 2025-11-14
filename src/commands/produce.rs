use anyhow::Result;
use crate::cli::ProduceArgs;
use crate::config;
use crate::kafka::KafkaProducer;

pub async fn handle_produce_command(args: ProduceArgs) -> Result<()> {
    let client_config = config::get_producer_config(
        &args.bootstrap_server,
        args.command_config.as_deref(),
        &args.property,
        args.compression_type.as_deref(),
    )?;
    
    let producer = KafkaProducer::new(client_config)?;
    
    producer.produce_from_stdin(
        &args.topic,
        args.key_separator.as_deref(),
    ).await?;
    
    producer.flush().await?;
    
    Ok(())
}
