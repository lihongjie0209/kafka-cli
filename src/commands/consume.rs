use anyhow::Result;
use crate::cli::ConsumeArgs;
use crate::config;
use crate::kafka::KafkaConsumer;

pub async fn handle_consume_command(args: ConsumeArgs) -> Result<()> {
    let client_config = config::get_consumer_config_cli(
        &args.bootstrap_server,
        args.group.as_deref(),
        args.command_config.as_deref(),
        &args.property,
        args.from_beginning,
    )?;
    
    let consumer = KafkaConsumer::new(client_config, &[&args.topic])?;
    
    consumer.consume_messages(
        &args.topic,
        args.max_messages,
        &args.formatter,
    ).await?;
    
    Ok(())
}
