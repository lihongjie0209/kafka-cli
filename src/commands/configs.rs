use anyhow::Result;
use crate::cli::{ConfigsArgs, ConfigAction};
use crate::config;
use crate::kafka::KafkaAdmin;

pub async fn handle_configs_command(args: ConfigsArgs) -> Result<()> {
    let client_config = config::build_client_config(
        &args.bootstrap_server,
        args.command_config.as_deref(),
        &[],
    )?;
    
    let admin = KafkaAdmin::new(client_config)?;
    
    match args.action {
        ConfigAction::Describe {
            entity_type,
            entity_name,
        } => {
            admin.describe_configs(&entity_type, &entity_name).await?;
        }
        
        ConfigAction::Alter {
            entity_type,
            entity_name,
            add_config,
            delete_config,
        } => {
            let configs_to_set = config::parse_properties(&add_config);
            admin.alter_configs(&entity_type, &entity_name, configs_to_set, delete_config).await?;
        }
    }
    
    Ok(())
}
