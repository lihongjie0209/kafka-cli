use kafka_cli::cli::{Cli, Commands};
use kafka_cli::commands;

use anyhow::Result;
use clap::Parser;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logger
    env_logger::init();
    
    let cli = Cli::parse();
    
    let result = match cli.command {
        Commands::Topics(args) => commands::handle_topics_command(args).await,
        Commands::Produce(args) => commands::handle_produce_command(args).await,
        Commands::Consume(args) => commands::handle_consume_command(args).await,
        Commands::ConsumerGroups(args) => commands::handle_consumer_groups_command(args).await,
        Commands::Configs(args) => commands::handle_configs_command(args).await,
    };
    
    if let Err(e) = result {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
    
    Ok(())
}
