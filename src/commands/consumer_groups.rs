use anyhow::{Result, anyhow};
use crate::cli::{ConsumerGroupsArgs, ConsumerGroupAction};
use crate::kafka::{ConsumerGroupManager, OffsetResetType};

pub async fn handle_consumer_groups_command(args: ConsumerGroupsArgs) -> Result<()> {
    let manager = ConsumerGroupManager::new(&args.bootstrap_server)
        .map_err(|e| anyhow!("Failed to create consumer group manager: {}", e))?;
    
    match args.action {
        ConsumerGroupAction::List => {
            handle_list_groups(&manager).await?;
        }
        
        ConsumerGroupAction::Describe { group } => {
            for group_id in &group {
                handle_describe_group(&manager, group_id).await?;
            }
        }
        
        ConsumerGroupAction::Delete { group } => {
            for group_id in &group {
                handle_delete_group(&manager, group_id).await?;
            }
        }
        
        ConsumerGroupAction::ResetOffsets {
            group,
            topic,
            to_earliest,
            to_latest,
            to_offset,
            to_datetime,
            partitions,
            execute,
        } => {
            handle_reset_offsets(
                &manager,
                &group,
                &topic,
                to_earliest,
                to_latest,
                to_offset,
                to_datetime,
                partitions,
                execute,
            )
            .await?;
        }
    }
    
    Ok(())
}

async fn handle_list_groups(manager: &ConsumerGroupManager) -> Result<()> {
    let groups = manager
        .list_groups()
        .await
        .map_err(|e| anyhow!("Failed to list consumer groups: {}", e))?;
    
    if groups.is_empty() {
        println!("No consumer groups found");
        return Ok(());
    }
    
    println!("Consumer Groups:");
    for group in groups {
        println!("  {}", group);
    }
    
    Ok(())
}

async fn handle_describe_group(manager: &ConsumerGroupManager, group_id: &str) -> Result<()> {
    let group_info = manager
        .describe_group(group_id)
        .await
        .map_err(|e| anyhow!("Failed to describe consumer group '{}': {}", group_id, e))?;
    
    println!("\nConsumer Group: {}", group_info.name);
    println!("  State: {}", group_info.state);
    println!("  Members: {}", group_info.members.len());
    
    for (i, member) in group_info.members.iter().enumerate() {
        println!("\n  Member {}:", i + 1);
        println!("    Member ID: {}", member.member_id);
        println!("    Client ID: {}", member.client_id);
        println!("    Host: {}", member.host);
    }
    
    // 获取偏移量信息
    match manager.get_group_offsets(group_id, None).await {
        Ok(offsets) if !offsets.is_empty() => {
            // 过滤掉内部 topics 和未提交偏移量的分区
            let relevant_offsets: Vec<_> = offsets
                .into_iter()
                .filter(|o| !o.topic.starts_with("__") && o.current_offset >= 0)
                .collect();
            
            if !relevant_offsets.is_empty() {
                println!("\n  Offsets:");
                println!("    {:<30} {:<10} {:<15} {:<15} {:<10}", 
                         "TOPIC", "PARTITION", "CURRENT-OFFSET", "LOG-END-OFFSET", "LAG");
                
                for offset_info in relevant_offsets {
                    println!("    {:<30} {:<10} {:<15} {:<15} {:<10}",
                             offset_info.topic,
                             offset_info.partition,
                             offset_info.current_offset,
                             offset_info.log_end_offset,
                             offset_info.lag);
                }
            } else {
                println!("\n  No committed offsets found");
            }
        }
        Ok(_) => {
            println!("\n  No committed offsets found");
        }
        Err(e) => {
            log::warn!("Failed to fetch offsets for group '{}': {}", group_id, e);
        }
    }
    
    Ok(())
}

async fn handle_delete_group(manager: &ConsumerGroupManager, group_id: &str) -> Result<()> {
    match manager.delete_group(group_id).await {
        Ok(_) => {
            println!("\nConsumer group '{}' marked for deletion", group_id);
            println!("\nNote: The group will be automatically removed by Kafka after the retention period.");
            println!("This happens when:");
            println!("  1. All members have left the group");
            println!("  2. The offsets.retention.minutes period has expired");
            Ok(())
        }
        Err(e) => {
            Err(anyhow!("Failed to delete consumer group '{}': {}", group_id, e))
        }
    }
}

async fn handle_reset_offsets(
    manager: &ConsumerGroupManager,
    group_id: &str,
    topic: &str,
    to_earliest: bool,
    to_latest: bool,
    to_offset: Option<i64>,
    to_datetime: Option<i64>,
    partitions: Option<Vec<i32>>,
    execute: bool,
) -> Result<()> {
    // 确定重置类型
    let reset_type = if to_earliest {
        OffsetResetType::Earliest
    } else if to_latest {
        OffsetResetType::Latest
    } else if let Some(offset) = to_offset {
        OffsetResetType::Offset(offset)
    } else if let Some(timestamp) = to_datetime {
        OffsetResetType::Timestamp(timestamp)
    } else {
        return Err(anyhow!("Must specify one of: --to-earliest, --to-latest, --to-offset, or --to-datetime"));
    };
    
    // 准备显示信息
    let partition_info = if let Some(ref parts) = partitions {
        if parts.is_empty() {
            "all partitions".to_string()
        } else {
            format!("partitions: {:?}", parts)
        }
    } else {
        "all partitions".to_string()
    };
    
    if !execute {
        // Dry-run: 显示将要执行的操作
        println!("\n{}", "=".repeat(70));
        println!("DRY RUN - No offsets will be changed");
        println!("{}", "=".repeat(70));
        println!("\nWould reset offsets for:");
        println!("  Consumer Group: {}", group_id);
        println!("  Topic: {}", topic);
        println!("  Partitions: {}", partition_info);
        
        match reset_type {
            OffsetResetType::Earliest => println!("  Reset Type: Earliest (beginning of log)"),
            OffsetResetType::Latest => println!("  Reset Type: Latest (end of log)"),
            OffsetResetType::Offset(o) => println!("  Reset Type: Specific offset ({})", o),
            OffsetResetType::Timestamp(ts) => {
                use chrono::{Local, TimeZone};
                let dt = Local.timestamp_millis_opt(ts).unwrap();
                println!("  Reset Type: By timestamp");
                println!("    Timestamp: {} ms", ts);
                println!("    DateTime: {}", dt.format("%Y-%m-%d %H:%M:%S %Z"));
            }
        }
        
        println!("\n{}", "=".repeat(70));
        println!("Add --execute flag to perform the actual reset");
        println!("{}", "=".repeat(70));
        return Ok(());
    }
    
    // 执行重置
    println!("\nResetting offsets...");
    manager
        .reset_offsets(group_id, topic, partitions, reset_type.clone())
        .await
        .map_err(|e| anyhow!("Failed to reset offsets: {}", e))?;
    
    println!("\n{}", "=".repeat(70));
    println!("Successfully reset offsets for consumer group '{}'", group_id);
    println!("{}", "=".repeat(70));
    println!("  Topic: {}", topic);
    println!("  Partitions: {}", partition_info);
    
    match reset_type {
        OffsetResetType::Earliest => println!("  Reset Type: Earliest"),
        OffsetResetType::Latest => println!("  Reset Type: Latest"),
        OffsetResetType::Offset(o) => println!("  Reset Type: Offset {}", o),
        OffsetResetType::Timestamp(ts) => {
            use chrono::{Local, TimeZone};
            let dt = Local.timestamp_millis_opt(ts).unwrap();
            println!("  Reset Type: Timestamp");
            println!("    DateTime: {}", dt.format("%Y-%m-%d %H:%M:%S %Z"));
        }
    }
    
    Ok(())
}
