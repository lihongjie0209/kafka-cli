//! Kafka command implementations.

use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::Path,
    time::Duration,
};

use chrono::{DateTime, NaiveDateTime, Utc};
use futures::StreamExt;
use krafka::protocol::{
    ApiKey, Decode, DescribableLogDirTopic, KafkaString, ListPartitionReassignmentsTopic,
    ReassignablePartition, ReassignableTopic, TaggedFields, TryEncode,
};
use rdkafka::{
    Message, Offset,
    admin::{
        AdminClient, AdminOptions, ConfigSource, NewPartitions, NewTopic, OwnedResourceSpecifier,
        ResourceSpecifier, TopicReplication,
    },
    client::DefaultClientContext,
    consumer::{BaseConsumer, CommitMode, Consumer, StreamConsumer},
    message::{BorrowedHeaders, Header, Headers, OwnedHeaders},
    producer::{FutureProducer, FutureRecord},
    topic_partition_list::TopicPartitionList,
};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::{
    cli::{
        AclAction, Cli, ClusterAction, Command, ConfigAction, ConfigEntityType, ElectionType,
        GroupAction, OffsetTime, ReassignAction, ResetOffsetsArgs, TopicAction, TopicSelector,
    },
    config,
    error::{Error, Result},
    ffi::{
        self, AclBinding, AclBindingFilter, AclOperation, AclPatternType, AclPermissionType,
        AclResourceType,
    },
    output::{self, OutputFormat},
};

type Admin = AdminClient<DefaultClientContext>;

/// Executes one top-level command.
pub async fn execute(cli: Cli) -> Result<()> {
    let bootstrap = cli.bootstrap_server.as_deref().ok_or_else(|| {
        Error::Usage("--bootstrap-server is required (or set KAFKA_CLI_BOOTSTRAP_SERVER)".into())
    })?;
    let client_config = config::client_config(bootstrap, cli.command_config.as_deref())?;
    let command_config = cli.command_config.clone();
    let timeout = cli.timeout();
    let format = cli.output;

    match cli.command {
        Command::Topics(args) => topics(&client_config, timeout, format, args.action).await,
        Command::Produce(args) => produce(client_config, timeout, args).await,
        Command::Consume(args) => consume(client_config, timeout, args).await,
        Command::Groups(args) => groups(&client_config, timeout, format, args.action).await,
        Command::Configs(args) => configs(&client_config, timeout, format, args.action).await,
        Command::Offsets(args) => offsets(&client_config, timeout, format, &args),
        Command::DeleteRecords(args) => {
            delete_records(
                &client_config,
                timeout,
                format,
                &args.offset_json_file,
                args.execute,
            )
            .await
        }
        Command::ApiVersions(args) => {
            api_versions(
                bootstrap,
                command_config.as_deref(),
                timeout,
                format,
                args.broker,
            )
            .await
        }
        Command::Cluster(args) => {
            cluster(
                &client_config,
                bootstrap,
                command_config.as_deref(),
                timeout,
                format,
                &args.action,
            )
            .await
        }
        Command::Acls(args) => acls(&client_config, timeout, format, &args.action),
        Command::Reassign(args) => {
            Box::pin(reassign(
                &client_config,
                bootstrap,
                command_config.as_deref(),
                timeout,
                format,
                &args.action,
            ))
            .await
        }
        Command::LeaderElection(args) => leader_election(
            &client_config,
            timeout,
            format,
            args.election_type,
            args.topic.as_deref(),
            args.partition,
            args.all_topic_partitions,
            args.execute,
        ),
        Command::LogDirs(args) => {
            log_dirs(
                bootstrap,
                command_config.as_deref(),
                timeout,
                format,
                args.broker_list.as_deref(),
                args.topic_list.as_deref(),
            )
            .await
        }
    }
}

fn admin(config: &rdkafka::ClientConfig) -> Result<Admin> {
    Ok(config.create()?)
}

fn base_consumer(config: &rdkafka::ClientConfig) -> Result<BaseConsumer> {
    Ok(config.create()?)
}

#[derive(Debug, Serialize)]
struct TopicSummary {
    name: String,
    partitions: usize,
    replication_factor: usize,
}

#[derive(Debug, Serialize)]
struct PartitionSummary {
    topic: String,
    partition: i32,
    leader: i32,
    replicas: Vec<i32>,
    isr: Vec<i32>,
}

#[derive(Debug, Serialize)]
struct TopicConfigSummary {
    topic: String,
    configs: Vec<String>,
}

// This dispatcher mirrors the five Kafka topic actions; splitting it would
// obscure the shared metadata and admin result handling.
#[expect(clippy::too_many_lines)]
async fn topics(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    format: OutputFormat,
    action: TopicAction,
) -> Result<()> {
    match action {
        TopicAction::List(selector) => {
            let consumer = base_consumer(config)?;
            let metadata = consumer.fetch_metadata(None, timeout)?;
            let topics = select_topics(&metadata, &selector)?
                .into_iter()
                .map(|topic| TopicSummary {
                    name: topic.name().to_owned(),
                    partitions: topic.partitions().len(),
                    replication_factor: topic
                        .partitions()
                        .first()
                        .map_or(0, |partition| partition.replicas().len()),
                })
                .collect::<Vec<_>>();
            output::write_value(format, "topics.list", &topics, |rows| {
                output::table(["TOPIC"], rows.iter().map(|row| [row.name.clone()]))
            })
        }
        TopicAction::Describe(selector) => {
            let consumer = base_consumer(config)?;
            let selected_topic_names = {
                let metadata = consumer.fetch_metadata(None, timeout)?;
                select_topics(&metadata, &selector)?
                    .into_iter()
                    .map(|topic| topic.name().to_owned())
                    .collect::<Vec<_>>()
            };
            let topic_configs = if selector.under_min_isr_partitions
                || selector.at_min_isr_partitions
                || selector.topics_with_overrides
            {
                let resources = selected_topic_names
                    .iter()
                    .map(|topic| ResourceSpecifier::Topic(topic))
                    .collect::<Vec<_>>();
                admin(config)?
                    .describe_configs(
                        &resources,
                        &AdminOptions::new().request_timeout(Some(timeout)),
                    )
                    .await?
                    .into_iter()
                    .map(|result| result.map_err(|code| Error::Config(code.to_string())))
                    .collect::<Result<Vec<_>>>()?
            } else {
                Vec::new()
            };
            if selector.topics_with_overrides {
                let rows = topic_configs
                    .iter()
                    .filter_map(|resource| {
                        let OwnedResourceSpecifier::Topic(topic) = &resource.specifier else {
                            return None;
                        };
                        let configs = resource
                            .entries
                            .iter()
                            .filter(|entry| entry.source == ConfigSource::DynamicTopic)
                            .map(|entry| {
                                format!(
                                    "{}={}",
                                    entry.name,
                                    entry.value.as_deref().unwrap_or("null")
                                )
                            })
                            .collect::<Vec<_>>();
                        (!configs.is_empty()).then(|| TopicConfigSummary {
                            topic: topic.clone(),
                            configs,
                        })
                    })
                    .collect::<Vec<_>>();
                return output::write_value(format, "topics.describe.overrides", &rows, |rows| {
                    output::table(
                        ["TOPIC", "CONFIGS"],
                        rows.iter()
                            .map(|row| [row.topic.clone(), row.configs.join(",")]),
                    )
                });
            }
            let metadata = consumer.fetch_metadata(None, timeout)?;
            let selected_topics = select_topics(&metadata, &selector)?;
            let min_isr = topic_configs
                .iter()
                .filter_map(|resource| {
                    let OwnedResourceSpecifier::Topic(topic) = &resource.specifier else {
                        return None;
                    };
                    resource
                        .entries
                        .iter()
                        .find(|entry| entry.name == "min.insync.replicas")
                        .and_then(|entry| entry.value.as_deref())
                        .and_then(|value| value.parse::<usize>().ok())
                        .map(|value| (topic.as_str(), value))
                })
                .collect::<BTreeMap<_, _>>();
            let live_brokers = metadata
                .brokers()
                .iter()
                .map(rdkafka::metadata::MetadataBroker::id)
                .collect::<BTreeSet<_>>();
            let selector = &selector;
            let live_brokers = &live_brokers;
            let rows = selected_topics
                .into_iter()
                .flat_map(|topic| {
                    let configured_min_isr = min_isr.get(topic.name()).copied();
                    topic
                        .partitions()
                        .iter()
                        .filter(move |partition| {
                            topic_partition_matches(
                                selector,
                                partition.isr().len(),
                                partition.replicas().len(),
                                configured_min_isr,
                                live_brokers.contains(&partition.leader()),
                            )
                        })
                        .map(|partition| PartitionSummary {
                            topic: topic.name().to_owned(),
                            partition: partition.id(),
                            leader: partition.leader(),
                            replicas: partition.replicas().to_vec(),
                            isr: partition.isr().to_vec(),
                        })
                })
                .collect::<Vec<_>>();
            output::write_value(format, "topics.describe", &rows, |rows| {
                output::table(
                    ["TOPIC", "PARTITION", "LEADER", "REPLICAS", "ISR"],
                    rows.iter().map(|row| {
                        [
                            row.topic.clone(),
                            row.partition.to_string(),
                            row.leader.to_string(),
                            csv_numbers(&row.replicas),
                            csv_numbers(&row.isr),
                        ]
                    }),
                )
            })
        }
        TopicAction::Create(args) => {
            let assignments = args
                .replica_assignment
                .as_deref()
                .map(parse_replica_assignment)
                .transpose()?;
            if assignments.as_ref().is_some_and(|items| {
                args.partitions != 1 && usize::try_from(args.partitions) != Ok(items.len())
            }) {
                return Err(Error::Usage(
                    "--partitions must match the number of replica assignments".into(),
                ));
            }
            let assignment_refs = assignments
                .as_ref()
                .map(|items| items.iter().map(Vec::as_slice).collect::<Vec<&[i32]>>());
            let (partition_count, replication) = assignment_refs.as_ref().map_or(
                (
                    args.partitions,
                    TopicReplication::Fixed(args.replication_factor),
                ),
                |items| {
                    (
                        i32::try_from(items.len()).unwrap_or(i32::MAX),
                        TopicReplication::Variable(items),
                    )
                },
            );
            let mut topic = NewTopic::new(&args.topic, partition_count, replication);
            let configs = parse_pairs(&args.configs)?;
            for (key, value) in &configs {
                topic = topic.set(key, value);
            }
            let result = admin(config)?
                .create_topics(
                    &[topic],
                    &AdminOptions::new().operation_timeout(Some(timeout)),
                )
                .await?;
            let failures = topic_results(result, args.if_not_exists);
            if failures == 0 {
                println!("Created topic {}.", args.topic);
                Ok(())
            } else {
                Err(Error::Partial {
                    failed: failures,
                    total: 1,
                })
            }
        }
        TopicAction::Alter(args) => {
            let metadata = if args.if_exists || args.replica_assignment.is_some() {
                Some(base_consumer(config)?.fetch_metadata(Some(&args.topic), timeout)?)
            } else {
                None
            };
            if args.if_exists
                && metadata.as_ref().is_some_and(|metadata| {
                    metadata
                        .topics()
                        .first()
                        .is_none_or(|topic| topic.error().is_some())
                })
            {
                println!("Topic {} does not exist.", args.topic);
                return Ok(());
            }
            let assignments = args
                .replica_assignment
                .as_deref()
                .map(parse_replica_assignment)
                .transpose()?;
            let new_assignment_refs = if let Some(assignments) = &assignments {
                let existing = metadata
                    .as_ref()
                    .and_then(|metadata| metadata.topics().first())
                    .filter(|topic| topic.error().is_none())
                    .ok_or_else(|| Error::Usage(format!("topic {} not found", args.topic)))?
                    .partitions()
                    .len();
                let target = usize::try_from(args.partitions)
                    .map_err(|_| Error::Usage("--partitions must be greater than zero".into()))?;
                if assignments.len() != target || existing >= target {
                    return Err(Error::Usage(
                        "--replica-assignment must cover every partition and add at least one"
                            .into(),
                    ));
                }
                Some(
                    assignments[existing..]
                        .iter()
                        .map(Vec::as_slice)
                        .collect::<Vec<&[i32]>>(),
                )
            } else {
                None
            };
            let mut request = NewPartitions::new(
                &args.topic,
                usize::try_from(args.partitions)
                    .map_err(|_| Error::Usage("--partitions must be greater than zero".into()))?,
            );
            if let Some(assignments) = new_assignment_refs.as_ref() {
                request = request.assign(assignments);
            }
            let result = admin(config)?
                .create_partitions(
                    &[request],
                    &AdminOptions::new().operation_timeout(Some(timeout)),
                )
                .await?;
            let failures = topic_results(result, false);
            if failures == 0 {
                Ok(())
            } else {
                Err(Error::Partial {
                    failed: failures,
                    total: 1,
                })
            }
        }
        TopicAction::Delete(args) => {
            let expression = args
                .selector
                .topic
                .ok_or_else(|| Error::Usage("--topic is required for delete".into()))?;
            let consumer = base_consumer(config)?;
            let metadata = consumer.fetch_metadata(None, timeout)?;
            let selector = TopicSelector {
                topic: Some(expression),
                exclude_internal: args.selector.exclude_internal,
                under_replicated_partitions: false,
                unavailable_partitions: false,
                under_min_isr_partitions: false,
                at_min_isr_partitions: false,
                topics_with_overrides: false,
            };
            let topics = select_topics(&metadata, &selector)?
                .into_iter()
                .map(rdkafka::metadata::MetadataTopic::name)
                .collect::<Vec<_>>();
            if topics.is_empty() {
                if args.if_exists {
                    println!("No matching topics exist.");
                    return Ok(());
                }
                return Err(Error::Usage("no topics matched --topic".into()));
            }
            let result = admin(config)?
                .delete_topics(&topics, &AdminOptions::new())
                .await?;
            let failures = topic_results(result, false);
            if failures == 0 {
                Ok(())
            } else {
                Err(Error::Partial {
                    failed: failures,
                    total: topics.len(),
                })
            }
        }
    }
}

fn select_topics<'a>(
    metadata: &'a rdkafka::metadata::Metadata,
    selector: &TopicSelector,
) -> Result<Vec<&'a rdkafka::metadata::MetadataTopic>> {
    let pattern = selector.topic.as_deref().map(topic_pattern).transpose()?;
    Ok(metadata
        .topics()
        .iter()
        .filter(|topic| {
            (!selector.exclude_internal || !topic.name().starts_with("__"))
                && pattern
                    .as_ref()
                    .is_none_or(|pattern| pattern.is_match(topic.name()))
        })
        .collect())
}

fn topic_partition_matches(
    selector: &TopicSelector,
    isr_count: usize,
    replica_count: usize,
    min_isr: Option<usize>,
    leader_is_available: bool,
) -> bool {
    (!selector.under_replicated_partitions || isr_count < replica_count)
        && (!selector.unavailable_partitions || !leader_is_available)
        && (!selector.under_min_isr_partitions || min_isr.is_some_and(|min| isr_count < min))
        && (!selector.at_min_isr_partitions || min_isr == Some(isr_count))
}

fn topic_pattern(expression: &str) -> Result<Regex> {
    Regex::new(&format!("^(?:{expression})$"))
        .map_err(|error| Error::Usage(format!("invalid topic regular expression: {error}")))
}

fn topic_results(
    results: Vec<std::result::Result<String, (String, rdkafka::types::RDKafkaErrorCode)>>,
    ignore_exists: bool,
) -> usize {
    results
        .into_iter()
        .filter_map(|result| match result {
            Ok(name) => {
                println!("{name}: OK");
                None
            }
            Err((name, code))
                if ignore_exists
                    && code == rdkafka::types::RDKafkaErrorCode::TopicAlreadyExists =>
            {
                println!("{name}: already exists");
                None
            }
            Err((name, code)) => {
                eprintln!("{name}: {code}");
                Some(())
            }
        })
        .count()
}

async fn produce(
    mut config: rdkafka::ClientConfig,
    timeout: Duration,
    args: crate::cli::ProduceArgs,
) -> Result<()> {
    apply_client_properties(&mut config, &args.properties)?;
    config.set("compression.type", &args.compression_type);
    config.set("acks", &args.acks);
    let producer: FutureProducer = config.create()?;
    let input = io::read_to_string(io::stdin())?;
    for (index, line) in input.lines().enumerate() {
        let input = producer_input(line, &args).map_err(|error| {
            Error::Usage(format!(
                "invalid producer input on line {}: {error}",
                index + 1
            ))
        })?;
        let mut record = FutureRecord::to(&args.topic).payload(&input.value);
        if let Some(key) = &input.key {
            record = record.key(key);
        }
        if let Some(partition) = input.partition {
            record = record.partition(partition);
        }
        if !input.headers.is_empty() {
            let mut headers = OwnedHeaders::new_with_capacity(input.headers.len());
            for (key, value) in &input.headers {
                headers = headers.insert(Header {
                    key,
                    value: value.as_deref(),
                });
            }
            record = record.headers(headers);
        }
        producer
            .send(record, timeout)
            .await
            .map_err(|(error, _)| Error::Kafka(error))?;
    }
    Ok(())
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct ProducerInput {
    #[serde(default)]
    key: Option<String>,
    value: String,
    #[serde(default)]
    partition: Option<i32>,
    #[serde(default)]
    headers: BTreeMap<String, Option<String>>,
}

fn producer_input(line: &str, args: &crate::cli::ProduceArgs) -> Result<ProducerInput> {
    if args.json {
        let input: ProducerInput = serde_json::from_str(line)?;
        if input.partition.is_some_and(|partition| partition < 0) {
            return Err(Error::Usage("partition must be non-negative".into()));
        }
        Ok(input)
    } else {
        let (key, value) = if args.parse_key {
            let separator = args.key_separator.unwrap_or('\t');
            line.split_once(separator)
                .map_or((None, line), |(key, value)| (Some(key.to_owned()), value))
        } else {
            (None, line)
        };
        Ok(ProducerInput {
            key,
            value: value.to_owned(),
            partition: None,
            headers: BTreeMap::new(),
        })
    }
}

#[derive(Serialize)]
struct ConsumedRecord<'a> {
    topic: &'a str,
    partition: i32,
    offset: i64,
    timestamp: Option<i64>,
    key: Option<String>,
    value: Option<String>,
    headers: BTreeMap<String, Option<String>>,
}

async fn consume(
    mut config: rdkafka::ClientConfig,
    _timeout: Duration,
    args: crate::cli::ConsumeArgs,
) -> Result<()> {
    apply_client_properties(&mut config, &args.properties)?;
    config.set("group.id", args.group.as_deref().unwrap_or("kafka-cli"));
    config.set("enable.auto.commit", "true");
    config.set(
        "auto.offset.reset",
        if args.from_beginning {
            "earliest"
        } else {
            "latest"
        },
    );
    let consumer: StreamConsumer = config.create()?;
    if let Some(partition) = args.partition {
        let mut assignment = TopicPartitionList::new();
        assignment.add_partition_offset(
            &args.topic,
            partition,
            args.offset.map_or(Offset::Beginning, Offset::Offset),
        )?;
        consumer.assign(&assignment)?;
    } else {
        consumer.subscribe(&[&args.topic])?;
    }

    let mut stream = consumer.stream();
    let mut received = 0_u64;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            message = stream.next() => {
                let Some(message) = message else { break };
                let message = message?;
                if args.json {
                    let record = ConsumedRecord {
                        topic: message.topic(),
                        partition: message.partition(),
                        offset: message.offset(),
                        timestamp: message.timestamp().to_millis(),
                        key: message.key().map(|key| String::from_utf8_lossy(key).into_owned()),
                        value: message.payload().map(|value| String::from_utf8_lossy(value).into_owned()),
                        headers: collect_headers(message.headers()),
                    };
                    output::write_json_line(&record)?;
                } else {
                    if args.print_key {
                        print!("{}{}", message.key_view::<str>().and_then(std::result::Result::ok).unwrap_or("null"), args.key_separator);
                    }
                    println!("{}", message.payload_view::<str>().and_then(std::result::Result::ok).unwrap_or(""));
                }
                received += 1;
                if args.max_messages.is_some_and(|max| received >= max) { break; }
            }
        }
    }
    Ok(())
}

fn collect_headers(headers: Option<&BorrowedHeaders>) -> BTreeMap<String, Option<String>> {
    let mut result = BTreeMap::new();
    if let Some(headers) = headers {
        for header in headers.iter() {
            result.insert(
                header.key.to_owned(),
                header
                    .value
                    .map(|value| String::from_utf8_lossy(value).into_owned()),
            );
        }
    }
    result
}

fn apply_client_properties(
    config: &mut rdkafka::ClientConfig,
    properties: &[String],
) -> Result<()> {
    for (key, value) in parse_pairs(properties)? {
        config.set(key, value);
    }
    Ok(())
}

#[derive(Serialize)]
struct GroupRow {
    group: String,
    state: String,
    protocol: String,
    members: usize,
}

async fn groups(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    format: OutputFormat,
    action: GroupAction,
) -> Result<()> {
    match action {
        GroupAction::List => describe_groups(config, timeout, format, None),
        GroupAction::Describe {
            group,
            members,
            state,
            offsets: _,
        } => {
            let mode = if members {
                GroupDescribeMode::Members
            } else if state {
                GroupDescribeMode::State
            } else {
                GroupDescribeMode::Offsets
            };
            describe_group_details(config, timeout, format, &group, mode)
        }
        GroupAction::Delete { group, execute } => {
            if !execute {
                println!("Would delete consumer groups: {}", group.join(","));
                return Ok(());
            }
            let names = group.iter().map(String::as_str).collect::<Vec<_>>();
            let results = admin(config)?
                .delete_groups(&names, &AdminOptions::new())
                .await?;
            let failures = results.iter().filter(|result| result.is_err()).count();
            for result in results {
                println!("{result:?}");
            }
            if failures == 0 {
                Ok(())
            } else {
                Err(Error::Partial {
                    failed: failures,
                    total: names.len(),
                })
            }
        }
        GroupAction::ResetOffsets(args) => reset_offsets(config, timeout, &args),
        GroupAction::DeleteOffsets {
            group,
            topic,
            execute,
        } => {
            let (topic_name, selected) = topic
                .split_once(':')
                .map_or((topic.as_str(), None), |(name, partitions)| {
                    (name, Some(partitions))
                });
            let partitions = if let Some(selected) = selected {
                parse_partitions(Some(selected))?.unwrap_or_default()
            } else {
                let metadata = base_consumer(config)?.fetch_metadata(Some(topic_name), timeout)?;
                metadata
                    .topics()
                    .first()
                    .ok_or_else(|| Error::Usage(format!("topic {topic_name} not found")))?
                    .partitions()
                    .iter()
                    .map(rdkafka::metadata::MetadataPartition::id)
                    .collect()
            };
            if !execute {
                println!(
                    "Would delete committed offsets for group {group}: {topic_name}:{}",
                    csv_numbers(&partitions)
                );
                return Ok(());
            }
            let admin = admin(config)?;
            crate::ffi::delete_group_offsets(
                admin.inner().native_ptr(),
                &group,
                topic_name,
                &partitions,
                duration_ms(timeout)?,
            )
        }
    }
}

#[derive(Serialize)]
struct GroupOffsetRow {
    group: String,
    topic: String,
    partition: i32,
    committed_offset: i64,
    log_end_offset: Option<i64>,
    lag: Option<i64>,
    error: Option<String>,
}

#[derive(Serialize)]
struct GroupMemberRow {
    group: String,
    member_id: String,
    instance_id: Option<String>,
    client_id: String,
    host: String,
    assignment: String,
}

#[derive(Clone, Copy)]
enum GroupDescribeMode {
    Offsets,
    Members,
    State,
}

fn describe_group_details(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    format: OutputFormat,
    group: &str,
    mode: GroupDescribeMode,
) -> Result<()> {
    match mode {
        GroupDescribeMode::State => {
            return describe_groups(config, timeout, format, Some(group));
        }
        GroupDescribeMode::Members => {
            return describe_group_members(config, timeout, format, group);
        }
        GroupDescribeMode::Offsets => {}
    }
    let admin = admin(config)?;
    let offsets = crate::ffi::list_consumer_group_offsets(
        admin.inner().native_ptr(),
        group,
        duration_ms(timeout)?,
    )?;
    drop(admin);
    let consumer = base_consumer(config)?;
    let rows = offsets
        .into_iter()
        .filter(|offset| offset.offset >= 0)
        .map(|offset| {
            let log_end_offset = consumer
                .fetch_watermarks(&offset.topic, offset.partition, timeout)
                .map(|(_, high)| high)
                .ok();
            let lag = group_offset_lag(offset.offset, log_end_offset);
            GroupOffsetRow {
                group: group.to_owned(),
                topic: offset.topic,
                partition: offset.partition,
                committed_offset: offset.offset,
                log_end_offset,
                lag,
                error: offset.error,
            }
        })
        .collect::<Vec<_>>();
    output::write_value(format, "groups.describe.offsets", &rows, |rows| {
        output::table(
            [
                "GROUP",
                "TOPIC",
                "PARTITION",
                "CURRENT_OFFSET",
                "LOG_END_OFFSET",
                "LAG",
                "ERROR",
            ],
            rows.iter().map(|row| {
                [
                    row.group.clone(),
                    row.topic.clone(),
                    row.partition.to_string(),
                    row.committed_offset.to_string(),
                    row.log_end_offset
                        .map_or_else(|| "-".into(), |value| value.to_string()),
                    row.lag
                        .map_or_else(|| "-".into(), |value| value.to_string()),
                    row.error.as_deref().unwrap_or("-").to_owned(),
                ]
            }),
        )
    })
}

fn describe_group_members(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    format: OutputFormat,
    group: &str,
) -> Result<()> {
    let groups = base_consumer(config)?.fetch_group_list(Some(group), timeout)?;
    let rows = groups
        .groups()
        .iter()
        .flat_map(|description| {
            description
                .members()
                .iter()
                .map(move |member| GroupMemberRow {
                    group: description.name().to_owned(),
                    member_id: member.id().to_owned(),
                    instance_id: None,
                    client_id: member.client_id().to_owned(),
                    host: member.client_host().to_owned(),
                    assignment: member
                        .assignment()
                        .map_or_else(String::new, |bytes| format!("{} bytes", bytes.len())),
                })
        })
        .collect::<Vec<_>>();
    output::write_value(format, "groups.describe.members", &rows, |rows| {
        output::table(
            [
                "GROUP",
                "MEMBER_ID",
                "INSTANCE_ID",
                "CLIENT_ID",
                "HOST",
                "ASSIGNMENT",
            ],
            rows.iter().map(|row| {
                [
                    row.group.clone(),
                    row.member_id.clone(),
                    row.instance_id.as_deref().unwrap_or("-").to_owned(),
                    row.client_id.clone(),
                    row.host.clone(),
                    row.assignment.clone(),
                ]
            }),
        )
    })
}

fn group_offset_lag(committed_offset: i64, log_end_offset: Option<i64>) -> Option<i64> {
    (committed_offset >= 0)
        .then_some(log_end_offset)
        .flatten()
        .map(|end| end.saturating_sub(committed_offset).max(0))
}

fn describe_groups(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    format: OutputFormat,
    group: Option<&str>,
) -> Result<()> {
    let list = base_consumer(config)?.fetch_group_list(group, timeout)?;
    let rows = list
        .groups()
        .iter()
        .map(|group| GroupRow {
            group: group.name().to_owned(),
            state: group.state().to_owned(),
            protocol: group.protocol().to_owned(),
            members: group.members().len(),
        })
        .collect::<Vec<_>>();
    output::write_value(format, "groups.describe", &rows, |rows| {
        output::table(
            ["GROUP", "STATE", "PROTOCOL", "MEMBERS"],
            rows.iter().map(|row| {
                [
                    row.group.clone(),
                    row.state.clone(),
                    row.protocol.clone(),
                    row.members.to_string(),
                ]
            }),
        )
    })
}

fn reset_offsets(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    args: &ResetOffsetsArgs,
) -> Result<()> {
    let mut config = config.clone();
    config.set("group.id", &args.group);
    let consumer: BaseConsumer = config.create()?;
    let metadata = consumer.fetch_metadata(Some(&args.topic), timeout)?;
    let topic = metadata
        .topics()
        .first()
        .ok_or_else(|| Error::Usage(format!("topic {} not found", args.topic)))?;
    let committed = if args.shift_by.is_some() || args.to_current {
        let mut requested = TopicPartitionList::new();
        for partition in topic.partitions() {
            requested.add_partition(&args.topic, partition.id());
        }
        Some(consumer.committed_offsets(requested, timeout)?)
    } else {
        None
    };
    let timestamp = if let Some(datetime) = args.to_datetime.as_deref() {
        Some(parse_datetime_millis(datetime)?)
    } else if let Some(duration) = args.by_duration.as_deref() {
        Some(
            Utc::now()
                .timestamp_millis()
                .saturating_sub(parse_iso8601_duration_millis(duration)?),
        )
    } else {
        None
    };
    let timestamp_offsets = if let Some(timestamp) = timestamp {
        let mut requested = TopicPartitionList::new();
        for partition in topic.partitions() {
            requested.add_partition_offset(
                &args.topic,
                partition.id(),
                Offset::Offset(timestamp),
            )?;
        }
        Some(consumer.offsets_for_times(requested, timeout)?)
    } else {
        None
    };
    let mut offsets = TopicPartitionList::new();
    for partition in topic.partitions() {
        let (low, high) = consumer.fetch_watermarks(&args.topic, partition.id(), timeout)?;
        let target = if args.to_earliest {
            low
        } else if args.to_latest {
            high
        } else if let Some(value) = args.to_offset {
            value.clamp(low, high)
        } else if let Some(shift) = args.shift_by {
            let current = committed
                .as_ref()
                .and_then(|offsets| offsets.find_partition(&args.topic, partition.id()))
                .and_then(|partition| partition.offset().to_raw())
                .filter(|offset| *offset >= 0);
            shifted_offset(current, shift, low, high, partition.id())?
        } else if args.to_current {
            committed
                .as_ref()
                .and_then(|offsets| offsets.find_partition(&args.topic, partition.id()))
                .and_then(|partition| partition.offset().to_raw())
                .filter(|offset| *offset >= 0)
                .ok_or_else(|| {
                    Error::Usage(format!(
                        "partition {} has no committed offset",
                        partition.id()
                    ))
                })?
        } else if timestamp_offsets.is_some() {
            timestamp_offsets
                .as_ref()
                .and_then(|offsets| offsets.find_partition(&args.topic, partition.id()))
                .and_then(|partition| partition.offset().to_raw())
                .filter(|offset| *offset >= 0)
                .unwrap_or(high)
        } else {
            return Err(Error::Usage("choose one reset target".into()));
        };
        println!("{}:{} -> {}", args.topic, partition.id(), target);
        offsets.add_partition_offset(&args.topic, partition.id(), Offset::Offset(target))?;
    }
    if args.execute {
        consumer.commit(&offsets, CommitMode::Sync)?;
    }
    Ok(())
}

fn parse_datetime_millis(value: &str) -> Result<i64> {
    DateTime::parse_from_rfc3339(value)
        .map(|datetime| datetime.timestamp_millis())
        .or_else(|_| {
            NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f")
                .map(|datetime| datetime.and_utc().timestamp_millis())
        })
        .map_err(|_| {
            Error::Usage("--to-datetime must be RFC 3339 or YYYY-MM-DDTHH:MM:SS.sss".into())
        })
}

fn parse_iso8601_duration_millis(value: &str) -> Result<i64> {
    let value = value
        .strip_prefix('P')
        .ok_or_else(|| Error::Usage("--by-duration must start with P".into()))?;
    let (days, time) = value.split_once('T').map_or((value, ""), |parts| parts);
    let days = if days.is_empty() {
        0
    } else {
        days.strip_suffix('D')
            .ok_or_else(|| Error::Usage("unsupported ISO-8601 duration date component".into()))?
            .parse::<i64>()
            .map_err(|_| Error::Usage("invalid ISO-8601 day duration".into()))?
    };
    let mut rest = time;
    let mut total_seconds = days
        .checked_mul(86_400)
        .ok_or_else(|| Error::Usage("duration is too large".into()))?;
    for (suffix, multiplier) in [('H', 3_600_i64), ('M', 60_i64), ('S', 1_i64)] {
        if let Some(index) = rest.find(suffix) {
            let amount = rest[..index]
                .parse::<i64>()
                .map_err(|_| Error::Usage(format!("invalid ISO-8601 {suffix} duration")))?;
            total_seconds = total_seconds
                .checked_add(
                    amount
                        .checked_mul(multiplier)
                        .ok_or_else(|| Error::Usage("duration is too large".into()))?,
                )
                .ok_or_else(|| Error::Usage("duration is too large".into()))?;
            rest = &rest[index + 1..];
        }
    }
    if !rest.is_empty() || total_seconds < 0 {
        return Err(Error::Usage("invalid ISO-8601 duration".into()));
    }
    total_seconds
        .checked_mul(1_000)
        .ok_or_else(|| Error::Usage("duration is too large".into()))
}

fn shifted_offset(
    current: Option<i64>,
    shift: i64,
    low: i64,
    high: i64,
    partition: i32,
) -> Result<i64> {
    current
        .map(|offset| offset.saturating_add(shift).clamp(low, high))
        .ok_or_else(|| {
            Error::Usage(format!(
                "cannot shift partition {partition}: no committed offset"
            ))
        })
}

async fn configs(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    format: OutputFormat,
    action: ConfigAction,
) -> Result<()> {
    match action {
        ConfigAction::Describe {
            entity_type,
            entity_name,
        } => {
            if matches!(entity_type, ConfigEntityType::User) {
                return describe_user_scram(config, timeout, format, entity_name);
            }
            let specifier = resource(entity_type, &entity_name)?;
            let results = admin(config)?
                .describe_configs(
                    &[specifier],
                    &AdminOptions::new().request_timeout(Some(timeout)),
                )
                .await?;
            let rows = results
                .into_iter()
                .map(|item| item.map_err(|code| Error::Config(code.to_string())))
                .collect::<Result<Vec<_>>>()?;
            output::write_value(
                format,
                "configs.describe",
                &rows
                    .iter()
                    .flat_map(|resource| {
                        resource
                            .entries
                            .iter()
                            .map(|entry| (&entry.name, &entry.value, entry.is_sensitive))
                    })
                    .collect::<Vec<_>>(),
                |rows| {
                    output::table(
                        ["NAME", "VALUE", "SENSITIVE"],
                        rows.iter().map(|(name, value, sensitive)| {
                            [
                                (*name).clone(),
                                value.as_deref().unwrap_or("null").to_owned(),
                                sensitive.to_string(),
                            ]
                        }),
                    )
                },
            )
        }
        ConfigAction::Alter {
            entity_type,
            entity_name,
            add,
            delete,
            execute,
        } => {
            let pairs = parse_pairs(&add)?;
            if pairs.is_empty() && delete.is_empty() {
                return Err(Error::Usage(
                    "provide --add-config or --delete-config".into(),
                ));
            }
            if matches!(entity_type, ConfigEntityType::User) {
                return alter_user_scram(config, timeout, &entity_name, &pairs, &delete, execute);
            }
            if !execute {
                if !pairs.is_empty() {
                    println!(
                        "Would set {}",
                        pairs
                            .iter()
                            .map(|(key, value)| format!("{key}={value}"))
                            .collect::<Vec<_>>()
                            .join(",")
                    );
                }
                if !delete.is_empty() {
                    println!("Would delete {}", delete.join(","));
                }
                return Ok(());
            }
            let admin = admin(config)?;
            crate::ffi::incremental_alter_config(
                admin.inner().native_ptr(),
                native_resource_type(entity_type),
                &entity_name,
                &pairs,
                &delete,
                duration_ms(timeout)?,
            )
        }
    }
}

fn describe_user_scram(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    format: OutputFormat,
    user: String,
) -> Result<()> {
    let client = admin(config)?;
    let rows = ffi::describe_user_scram_credentials(
        client.inner().native_ptr(),
        &[user],
        duration_ms(timeout)?,
    )?;
    output::write_value(format, "configs.describe-user", &rows, |rows| {
        output::table(
            ["USER", "MECHANISM", "ITERATIONS"],
            rows.iter().map(|row| {
                [
                    row.user.clone(),
                    scram_mechanism_name(row.mechanism).to_owned(),
                    row.iterations.to_string(),
                ]
            }),
        )
    })
}

fn alter_user_scram(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    user: &str,
    add: &[(String, String)],
    delete: &[String],
    execute: bool,
) -> Result<()> {
    let changes = parse_scram_changes(add, delete)?;
    if !execute {
        for change in &changes {
            match change {
                ffi::ScramCredentialAlteration::Upsert {
                    mechanism,
                    iterations,
                    ..
                } => println!(
                    "Would set {} with {iterations} iterations",
                    scram_mechanism_name(*mechanism)
                ),
                ffi::ScramCredentialAlteration::Delete { mechanism } => {
                    println!("Would delete {}", scram_mechanism_name(*mechanism));
                }
            }
        }
        return Ok(());
    }
    let client = admin(config)?;
    ffi::alter_user_scram_credentials(
        client.inner().native_ptr(),
        user,
        &changes,
        duration_ms(timeout)?,
    )
}

const fn native_resource_type(kind: ConfigEntityType) -> rdkafka_sys::rd_kafka_ResourceType_t {
    match kind {
        ConfigEntityType::Topic => rdkafka_sys::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_TOPIC,
        ConfigEntityType::Broker => rdkafka_sys::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_BROKER,
        ConfigEntityType::Group => rdkafka_sys::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_GROUP,
        ConfigEntityType::User => rdkafka_sys::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_UNKNOWN,
    }
}

fn resource(kind: ConfigEntityType, name: &str) -> Result<ResourceSpecifier<'_>> {
    Ok(match kind {
        ConfigEntityType::Topic => ResourceSpecifier::Topic(name),
        ConfigEntityType::Group => ResourceSpecifier::Group(name),
        ConfigEntityType::Broker => ResourceSpecifier::Broker(
            name.parse()
                .map_err(|_| Error::Usage("broker entity name must be an integer".into()))?,
        ),
        ConfigEntityType::User => {
            return Err(Error::Config(
                "user configuration must use the SCRAM Admin API".into(),
            ));
        }
    })
}

fn parse_scram_changes(
    add: &[(String, String)],
    delete: &[String],
) -> Result<Vec<ffi::ScramCredentialAlteration>> {
    let mut mechanisms = BTreeSet::new();
    let mut changes = Vec::new();
    for (name, value) in add {
        let mechanism = parse_scram_mechanism(name)?;
        if !mechanisms.insert(mechanism_key(mechanism)) {
            return Err(Error::Usage(format!(
                "duplicate SCRAM alteration for {}",
                scram_mechanism_name(mechanism)
            )));
        }
        let body = value
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
            .ok_or_else(|| {
                Error::Usage(format!(
                    "{} must use [iterations=N,password=secret]",
                    scram_mechanism_name(mechanism)
                ))
            })?;
        let properties = body
            .split(',')
            .map(|item| {
                item.split_once('=').ok_or_else(|| {
                    Error::Usage(format!("invalid SCRAM credential property: {item}"))
                })
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let password = properties
            .get("password")
            .filter(|password| !password.is_empty())
            .ok_or_else(|| Error::Usage("SCRAM credential requires password".into()))?;
        let iterations = properties.get("iterations").map_or(Ok(4096), |value| {
            value
                .parse::<i32>()
                .map_err(|_| Error::Usage(format!("invalid SCRAM iteration count: {value}")))
        })?;
        if iterations < 4096 {
            return Err(Error::Usage(
                "SCRAM iteration count must be at least 4096".into(),
            ));
        }
        if properties
            .keys()
            .any(|key| !matches!(*key, "iterations" | "password"))
        {
            return Err(Error::Usage(
                "SCRAM credentials only accept iterations and password".into(),
            ));
        }
        changes.push(ffi::ScramCredentialAlteration::Upsert {
            mechanism,
            iterations,
            password: password.as_bytes().to_vec(),
        });
    }
    for name in delete {
        let mechanism = parse_scram_mechanism(name)?;
        if !mechanisms.insert(mechanism_key(mechanism)) {
            return Err(Error::Usage(format!(
                "duplicate SCRAM alteration for {}",
                scram_mechanism_name(mechanism)
            )));
        }
        changes.push(ffi::ScramCredentialAlteration::Delete { mechanism });
    }
    Ok(changes)
}

fn parse_scram_mechanism(value: &str) -> Result<ffi::ScramMechanism> {
    match value.to_ascii_uppercase().as_str() {
        "SCRAM-SHA-256" => Ok(ffi::ScramMechanism::Sha256),
        "SCRAM-SHA-512" => Ok(ffi::ScramMechanism::Sha512),
        _ => Err(Error::Usage(format!(
            "unsupported user config {value}; expected SCRAM-SHA-256 or SCRAM-SHA-512"
        ))),
    }
}

const fn mechanism_key(value: ffi::ScramMechanism) -> u8 {
    match value {
        ffi::ScramMechanism::Sha256 => 1,
        ffi::ScramMechanism::Sha512 => 2,
    }
}

const fn scram_mechanism_name(value: ffi::ScramMechanism) -> &'static str {
    match value {
        ffi::ScramMechanism::Sha256 => "SCRAM-SHA-256",
        ffi::ScramMechanism::Sha512 => "SCRAM-SHA-512",
    }
}

#[derive(Serialize)]
struct OffsetRow {
    topic: String,
    partition: i32,
    offset: i64,
}

fn offsets(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    format: OutputFormat,
    args: &crate::cli::OffsetsArgs,
) -> Result<()> {
    let consumer = base_consumer(config)?;
    let metadata = consumer.fetch_metadata(None, timeout)?;
    let patterns = args
        .topic_partitions
        .as_deref()
        .map(parse_topic_partition_patterns)
        .transpose()?;
    let topic_filter = args.topic.as_deref().map(topic_pattern).transpose()?;
    let selected = parse_partitions(args.partitions.as_deref())?;
    let targets = metadata
        .topics()
        .iter()
        .filter(|topic| !args.exclude_internal_topics || !topic.name().starts_with("__"))
        .flat_map(|topic| {
            let topic_name = topic.name();
            let topic_filter = &topic_filter;
            let patterns = &patterns;
            let selected = &selected;
            topic
                .partitions()
                .iter()
                .filter(move |partition| {
                    patterns.as_ref().map_or_else(
                        || {
                            topic_filter
                                .as_ref()
                                .is_none_or(|pattern| pattern.is_match(topic_name))
                                && selected
                                    .as_ref()
                                    .is_none_or(|ids| ids.contains(&partition.id()))
                        },
                        |patterns| {
                            patterns
                                .iter()
                                .any(|pattern| pattern.matches(topic_name, partition.id()))
                        },
                    )
                })
                .map(move |partition| (topic_name.to_owned(), partition.id()))
        })
        .collect::<Vec<_>>();
    if targets.is_empty() {
        return Err(Error::Usage(
            "no topic-partitions matched the supplied filters".into(),
        ));
    }
    let timestamp_offsets = if let Some(timestamp) = args.timestamp {
        if timestamp < 0 {
            return Err(Error::Usage("--timestamp must be non-negative".into()));
        }
        let mut timestamps = TopicPartitionList::new();
        for (topic, partition) in &targets {
            timestamps.add_partition_offset(topic, *partition, Offset::Offset(timestamp))?;
        }
        Some(consumer.offsets_for_times(timestamps, timeout)?)
    } else {
        None
    };
    let mut rows = Vec::new();
    for (topic, partition) in targets {
        let offset = if let Some(timestamp_offsets) = &timestamp_offsets {
            timestamp_offsets
                .find_partition(&topic, partition)
                .and_then(|element| element.offset().to_raw())
                .unwrap_or(-1)
        } else {
            let (low, high) = consumer.fetch_watermarks(&topic, partition, timeout)?;
            match args.time {
                OffsetTime::Earliest => low,
                OffsetTime::Latest => high,
            }
        };
        rows.push(OffsetRow {
            topic,
            partition,
            offset,
        });
    }
    output::write_value(format, "offsets", &rows, |rows| {
        output::table(
            ["TOPIC", "PARTITION", "OFFSET"],
            rows.iter().map(|row| {
                [
                    row.topic.clone(),
                    row.partition.to_string(),
                    row.offset.to_string(),
                ]
            }),
        )
    })
}

struct TopicPartitionPattern {
    topic: Regex,
    start: Option<i32>,
    end: Option<i32>,
}

impl TopicPartitionPattern {
    fn matches(&self, topic: &str, partition: i32) -> bool {
        self.topic.is_match(topic)
            && self.start.is_none_or(|start| partition >= start)
            && self.end.is_none_or(|end| partition < end)
    }
}

fn parse_topic_partition_patterns(value: &str) -> Result<Vec<TopicPartitionPattern>> {
    value
        .split(',')
        .map(|item| {
            let (topic, partitions) = item
                .split_once(':')
                .map_or((item, None), |(topic, range)| (topic, Some(range)));
            let topic = topic_pattern(if topic.is_empty() { ".*" } else { topic })?;
            let (start, end) = match partitions {
                None | Some("") => (None, None),
                Some(range) if range.contains('-') => {
                    let (start, end) = range
                        .split_once('-')
                        .ok_or_else(|| Error::Usage(format!("invalid partition range: {range}")))?;
                    (
                        parse_optional_partition_bound(start)?,
                        parse_optional_partition_bound(end)?,
                    )
                }
                Some(partition) => {
                    let partition = partition
                        .parse::<i32>()
                        .map_err(|_| Error::Usage(format!("invalid partition: {partition}")))?;
                    (Some(partition), Some(partition.saturating_add(1)))
                }
            };
            if start.is_some_and(|start| end.is_some_and(|end| start >= end)) {
                return Err(Error::Usage("partition range must be increasing".into()));
            }
            Ok(TopicPartitionPattern { topic, start, end })
        })
        .collect()
}

fn parse_optional_partition_bound(value: &str) -> Result<Option<i32>> {
    if value.is_empty() {
        Ok(None)
    } else {
        value
            .parse::<i32>()
            .map(Some)
            .map_err(|_| Error::Usage(format!("invalid partition bound: {value}")))
    }
}

#[derive(Debug, Deserialize)]
struct DeleteRecordsFile {
    #[serde(default)]
    version: Option<u8>,
    partitions: Vec<DeleteRecordsPartition>,
}
#[derive(Debug, Deserialize)]
struct DeleteRecordsPartition {
    topic: String,
    partition: i32,
    offset: i64,
}

async fn delete_records(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    _format: OutputFormat,
    path: &Path,
    execute: bool,
) -> Result<()> {
    let input = read_delete_records(path)?;
    let mut offsets = TopicPartitionList::new();
    for item in &input.partitions {
        println!("{}:{} before {}", item.topic, item.partition, item.offset);
        offsets.add_partition_offset(&item.topic, item.partition, Offset::Offset(item.offset))?;
    }
    if !execute {
        return Ok(());
    }
    let result = admin(config)?
        .delete_records(
            &offsets,
            &AdminOptions::new().operation_timeout(Some(timeout)),
        )
        .await?;
    for element in result.elements() {
        println!(
            "{}:{} {:?}",
            element.topic(),
            element.partition(),
            element.offset()
        );
    }
    Ok(())
}

fn read_delete_records(path: &Path) -> Result<DeleteRecordsFile> {
    let input: DeleteRecordsFile = serde_json::from_reader(std::fs::File::open(path)?)?;
    if input.version.is_some_and(|version| version != 1) {
        return Err(Error::Usage("delete-records JSON version must be 1".into()));
    }
    if input.partitions.is_empty() {
        return Err(Error::Usage(
            "delete-records partition list cannot be empty".into(),
        ));
    }
    let mut targets = BTreeSet::new();
    for partition in &input.partitions {
        if partition.topic.is_empty() || partition.partition < 0 || partition.offset < 0 {
            return Err(Error::Usage(
                "delete-records entries require a topic and non-negative partition/offset".into(),
            ));
        }
        if !targets.insert((partition.topic.as_str(), partition.partition)) {
            return Err(Error::Usage(format!(
                "duplicate delete-records target {}:{}",
                partition.topic, partition.partition
            )));
        }
    }
    Ok(input)
}

#[derive(Debug, Serialize)]
struct ApiVersionRow {
    broker: i32,
    host: String,
    api: String,
    api_key: i16,
    min_version: i16,
    max_version: i16,
}

async fn api_versions(
    bootstrap: &str,
    command_config: Option<&Path>,
    timeout: Duration,
    format: OutputFormat,
    broker: Option<i32>,
) -> Result<()> {
    let client = config::protocol_admin(bootstrap, timeout, command_config).await?;
    let cluster = client.describe_cluster().await?;
    let mut rows = Vec::new();
    for endpoint in cluster
        .brokers
        .iter()
        .filter(|endpoint| broker.is_none_or(|id| id == endpoint.broker_id))
    {
        let address = format!("{}:{}", endpoint.host, endpoint.port);
        let connection = client
            .pool()
            .get_connection_by_id(endpoint.broker_id, &address)
            .await?;
        for (api_key, min_version, max_version) in broker_api_versions(&connection).await? {
            rows.push(ApiVersionRow {
                broker: endpoint.broker_id,
                host: address.clone(),
                api: format!("{:?}", ApiKey::from(api_key)),
                api_key,
                min_version,
                max_version,
            });
        }
        drop(connection);
    }
    drop(cluster);
    drop(client);
    output::write_value(format, "api-versions", &rows, |rows| {
        output::table(
            ["BROKER", "HOST", "API", "KEY", "MIN", "MAX"],
            rows.iter().map(|row| {
                [
                    row.broker.to_string(),
                    row.host.clone(),
                    row.api.clone(),
                    row.api_key.to_string(),
                    row.min_version.to_string(),
                    row.max_version.to_string(),
                ]
            }),
        )
    })
}

async fn broker_api_versions(
    connection: &krafka::network::BrokerConnection,
) -> Result<Vec<(i16, i16, i16)>> {
    let mut response = connection
        .send_request(ApiKey::ApiVersions, 0, |_| Ok(()))
        .await?;
    let error_code = i16::decode(&mut response)?;
    if error_code != 0 {
        return Err(Error::Config(format!(
            "ApiVersions failed with Kafka error code {error_code}"
        )));
    }
    let count = decode_acl_count(&mut response)?;
    let mut versions = Vec::with_capacity(count);
    for _ in 0..count {
        versions.push((
            i16::decode(&mut response)?,
            i16::decode(&mut response)?,
            i16::decode(&mut response)?,
        ));
    }
    Ok(versions)
}

#[derive(Serialize)]
struct BrokerRow {
    id: i32,
    host: String,
    port: i32,
}

fn broker_table(rows: &[BrokerRow]) -> String {
    output::table(
        ["ID", "HOST", "PORT"],
        rows.iter()
            .map(|row| [row.id.to_string(), row.host.clone(), row.port.to_string()]),
    )
}

async fn cluster(
    config: &rdkafka::ClientConfig,
    bootstrap: &str,
    command_config: Option<&Path>,
    timeout: Duration,
    format: OutputFormat,
    action: &ClusterAction,
) -> Result<()> {
    match action {
        ClusterAction::Id => {
            let id = admin(config)?
                .inner()
                .fetch_cluster_id(timeout)
                .ok_or_else(|| Error::Unsupported("broker did not return a cluster ID".into()))?;
            output::write_value(format, "cluster.id", &id, |id| {
                output::table(["CLUSTER_ID"], [[id.clone()]])
            })
        }
        ClusterAction::ListEndpoints => {
            let client = base_consumer(config)?;
            let metadata = client.fetch_metadata(None, timeout)?;
            let rows = metadata
                .brokers()
                .iter()
                .map(|broker| BrokerRow {
                    id: broker.id(),
                    host: broker.host().to_owned(),
                    port: broker.port(),
                })
                .collect::<Vec<_>>();
            output::write_value(format, "cluster.list-endpoints", &rows, |rows| {
                broker_table(rows)
            })
        }
        ClusterAction::ApiVersions => {
            api_versions(bootstrap, command_config, timeout, format, None).await
        }
        ClusterAction::Unregister { id, execute } => {
            if !execute {
                println!("Would unregister broker {id}");
                return Ok(());
            }
            let client = config::protocol_admin(bootstrap, timeout, command_config).await?;
            unregister_broker(&client, *id).await
        }
    }
}

async fn unregister_broker(client: &krafka::admin::AdminClient, broker_id: i32) -> Result<()> {
    let connection = client.get_controller_connection().await?;
    let mut response = connection
        .send_request(ApiKey::UnregisterBroker, 0, |buffer| {
            broker_id.try_encode(buffer)?;
            TaggedFields::default().try_encode(buffer)
        })
        .await?;
    drop(connection);
    let _throttle_time_ms = i32::decode(&mut response)?;
    let error_code = i16::decode(&mut response)?;
    let error_message = KafkaString::decode_compact(&mut response)?.0;
    let _tagged_fields = TaggedFields::decode(&mut response)?;
    if error_code == 0 {
        println!("Broker {broker_id} is no longer registered.");
        Ok(())
    } else {
        Err(Error::Config(error_message.unwrap_or_else(|| {
            format!("UnregisterBroker failed with Kafka error code {error_code}")
        })))
    }
}

fn acls(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    format: OutputFormat,
    action: &AclAction,
) -> Result<()> {
    let client = admin(config)?;
    let timeout_ms = duration_ms(timeout)?;
    match action {
        AclAction::List(filter) => {
            let rows = ffi::describe_acls(
                client.inner().native_ptr(),
                &acl_filter(filter, None)?,
                timeout_ms,
            )?
            .into_iter()
            .map(acl_row)
            .collect::<Vec<_>>();
            output::write_value(format, "acls.list", &rows, |rows| acl_table(rows))
        }
        AclAction::Add(mutation) => {
            let operations =
                if mutation.operation.is_empty() && !mutation.producer && !mutation.consumer {
                    vec![AclOperation::All]
                } else {
                    acl_operations(&mutation.operation)?
                };
            let bindings = acl_bindings(mutation, &operations)?;
            if !mutation.execute {
                println!("Would create {} ACL binding(s)", bindings.len());
                return Ok(());
            }
            let result = ffi::create_acls(client.inner().native_ptr(), &bindings, timeout_ms)?;
            if result.failures == 0 {
                Ok(())
            } else {
                Err(Error::Partial {
                    failed: result.failures,
                    total: bindings.len(),
                })
            }
        }
        AclAction::Remove(mutation) => {
            let operations = acl_operations(&mutation.operation)?;
            let filters = acl_removal_filters(mutation, &operations)?;
            if !mutation.execute {
                let mut rows = Vec::new();
                for filter in filters {
                    rows.extend(
                        ffi::describe_acls(client.inner().native_ptr(), &filter, timeout_ms)?
                            .into_iter()
                            .map(acl_row),
                    );
                }
                return output::write_value(format, "acls.remove.preview", &rows, |rows| {
                    acl_table(rows)
                });
            }
            let total = filters.len();
            let result = ffi::delete_acls(client.inner().native_ptr(), &filters, timeout_ms)?;
            if result.failures == 0 {
                println!("Deleted {} ACL binding(s).", result.matched);
                Ok(())
            } else {
                Err(Error::Partial {
                    failed: result.failures,
                    total,
                })
            }
        }
    }
}

fn acl_filter(
    filter: &crate::cli::AclFilterArgs,
    operation: Option<AclOperation>,
) -> Result<AclBindingFilter> {
    let (resource_type, name) = acl_resource(filter)?;
    Ok(AclBindingFilter {
        resource_type,
        resource_name: name,
        pattern_type: acl_wire_pattern(filter.resource_pattern_type),
        principal: filter.principal.clone(),
        host: None,
        operation: operation.unwrap_or(AclOperation::Any),
        permission_type: AclPermissionType::Any,
    })
}

fn acl_operations(values: &[String]) -> Result<Vec<AclOperation>> {
    values
        .iter()
        .map(|value| match value.to_ascii_lowercase().as_str() {
            "all" => Ok(AclOperation::All),
            "read" => Ok(AclOperation::Read),
            "write" => Ok(AclOperation::Write),
            "create" => Ok(AclOperation::Create),
            "delete" => Ok(AclOperation::Delete),
            "alter" => Ok(AclOperation::Alter),
            "describe" => Ok(AclOperation::Describe),
            "cluster-action" => Ok(AclOperation::ClusterAction),
            "describe-configs" => Ok(AclOperation::DescribeConfigs),
            "alter-configs" => Ok(AclOperation::AlterConfigs),
            "idempotent-write" => Ok(AclOperation::IdempotentWrite),
            _ => Err(Error::Usage(format!("unknown ACL operation: {value}"))),
        })
        .collect()
}

fn acl_resource(filter: &crate::cli::AclFilterArgs) -> Result<(AclResourceType, Option<String>)> {
    let mut resources = Vec::new();
    if let Some(topic) = &filter.topic {
        resources.push((AclResourceType::Topic, topic.clone()));
    }
    if let Some(group) = &filter.group {
        resources.push((AclResourceType::Group, group.clone()));
    }
    if filter.cluster {
        resources.push((AclResourceType::Cluster, "kafka-cluster".into()));
    }
    if let Some(transactional_id) = &filter.transactional_id {
        resources.push((AclResourceType::TransactionalId, transactional_id.clone()));
    }
    if filter.delegation_token.is_some() {
        return Err(Error::Unsupported(
            "librdkafka does not support delegation-token ACL resources".into(),
        ));
    }
    match resources.len() {
        0 => Ok((AclResourceType::Any, None)),
        1 => resources.pop().map_or_else(
            || Err(Error::Config("ACL resource selection was lost".into())),
            |(kind, name)| Ok((kind, Some(name))),
        ),
        _ => Err(Error::Usage(
            "ACL requests accept exactly one resource selector".into(),
        )),
    }
}

const fn acl_wire_pattern(pattern: crate::cli::AclResourcePattern) -> AclPatternType {
    match pattern {
        crate::cli::AclResourcePattern::Any => AclPatternType::Any,
        crate::cli::AclResourcePattern::Literal => AclPatternType::Literal,
        crate::cli::AclResourcePattern::Prefixed => AclPatternType::Prefixed,
    }
}

fn acl_bindings(
    mutation: &crate::cli::AclMutationArgs,
    operations: &[AclOperation],
) -> Result<Vec<AclBinding>> {
    let resources = acl_mutation_resources(mutation, operations)?;
    let mut principals = mutation
        .allow_principal
        .iter()
        .map(|principal| (principal.as_str(), AclPermissionType::Allow))
        .chain(
            mutation
                .deny_principal
                .iter()
                .map(|principal| (principal.as_str(), AclPermissionType::Deny)),
        )
        .collect::<Vec<_>>();
    if principals.is_empty()
        && let Some(principal) = &mutation.filter.principal
    {
        principals.push((principal, AclPermissionType::Allow));
    }
    if principals.is_empty() {
        return Err(Error::Usage(
            "provide --allow-principal or --deny-principal".into(),
        ));
    }
    let mut bindings = Vec::new();
    for (resource_type, resource_name, operations) in resources {
        for (principal, permission) in &principals {
            let configured_hosts = if *permission == AclPermissionType::Allow {
                &mutation.allow_host
            } else {
                &mutation.deny_host
            };
            let hosts = if configured_hosts.is_empty() {
                vec![mutation.host.as_deref().unwrap_or("*")]
            } else {
                configured_hosts.iter().map(String::as_str).collect()
            };
            for host in hosts {
                for operation in &operations {
                    bindings.push(AclBinding {
                        resource_type,
                        resource_name: resource_name.clone(),
                        pattern_type: acl_wire_pattern(mutation.filter.resource_pattern_type),
                        principal: (*principal).to_owned(),
                        host: host.to_owned(),
                        operation: *operation,
                        permission_type: *permission,
                    });
                }
            }
        }
    }
    Ok(bindings)
}

fn acl_removal_filters(
    mutation: &crate::cli::AclMutationArgs,
    operations: &[AclOperation],
) -> Result<Vec<AclBindingFilter>> {
    let resources = if mutation.producer || mutation.consumer {
        acl_mutation_resources(mutation, operations)?
    } else {
        let (resource_type, resource_name) = acl_resource(&mutation.filter)?;
        vec![(
            resource_type,
            resource_name.unwrap_or_default(),
            if operations.is_empty() {
                vec![AclOperation::Any]
            } else {
                operations.to_vec()
            },
        )]
    };
    let mut principals = mutation
        .allow_principal
        .iter()
        .map(|principal| (Some(principal.as_str()), AclPermissionType::Allow))
        .chain(
            mutation
                .deny_principal
                .iter()
                .map(|principal| (Some(principal.as_str()), AclPermissionType::Deny)),
        )
        .collect::<Vec<_>>();
    if principals.is_empty() {
        principals.push((mutation.filter.principal.as_deref(), AclPermissionType::Any));
    }
    let mut filters = Vec::new();
    for (resource_type, resource_name, operations) in resources {
        for (principal, permission) in &principals {
            for operation in &operations {
                filters.push(AclBindingFilter {
                    resource_type,
                    resource_name: (!resource_name.is_empty()).then(|| resource_name.clone()),
                    pattern_type: acl_wire_pattern(mutation.filter.resource_pattern_type),
                    principal: principal.map(str::to_owned),
                    host: mutation.host.clone(),
                    operation: *operation,
                    permission_type: *permission,
                });
            }
        }
    }
    Ok(filters)
}

fn acl_mutation_resources(
    mutation: &crate::cli::AclMutationArgs,
    operations: &[AclOperation],
) -> Result<Vec<(AclResourceType, String, Vec<AclOperation>)>> {
    if !mutation.producer && !mutation.consumer {
        let (resource_type, resource_name) = acl_resource(&mutation.filter)?;
        return Ok(vec![(
            resource_type,
            resource_name
                .ok_or_else(|| Error::Usage("an ACL resource selector is required".into()))?,
            operations.to_vec(),
        )]);
    }
    if !mutation.deny_principal.is_empty() || !mutation.deny_host.is_empty() {
        return Err(Error::Usage(
            "role ACLs only support allow principals and hosts".into(),
        ));
    }
    let topic = mutation
        .filter
        .topic
        .clone()
        .ok_or_else(|| Error::Usage("--producer and --consumer require --topic".into()))?;
    let mut topic_operations = Vec::new();
    if mutation.producer {
        topic_operations.extend([
            AclOperation::Write,
            AclOperation::Describe,
            AclOperation::Create,
        ]);
    }
    if mutation.consumer {
        for operation in [AclOperation::Read, AclOperation::Describe] {
            if !topic_operations.contains(&operation) {
                topic_operations.push(operation);
            }
        }
    }
    let mut resources = vec![(AclResourceType::Topic, topic, topic_operations)];
    if mutation.consumer {
        resources.push((
            AclResourceType::Group,
            mutation
                .filter
                .group
                .clone()
                .ok_or_else(|| Error::Usage("--consumer requires --group".into()))?,
            vec![AclOperation::Read],
        ));
    }
    if mutation.producer
        && let Some(transactional_id) = &mutation.filter.transactional_id
    {
        resources.push((
            AclResourceType::TransactionalId,
            transactional_id.clone(),
            vec![AclOperation::Write, AclOperation::Describe],
        ));
    }
    if mutation.idempotent {
        resources.push((
            AclResourceType::Cluster,
            "kafka-cluster".into(),
            vec![AclOperation::IdempotentWrite],
        ));
    }
    Ok(resources)
}

fn acl_row(binding: AclBinding) -> AclRow {
    AclRow {
        resource_type: format!("{:?}", binding.resource_type),
        resource_name: binding.resource_name,
        pattern_type: format!("{:?}", binding.pattern_type),
        principal: binding.principal,
        host: binding.host,
        operation: format!("{:?}", binding.operation),
        permission: format!("{:?}", binding.permission_type),
    }
}

fn decode_acl_count(buffer: &mut bytes::Bytes) -> Result<usize> {
    let count = i32::decode(buffer)?;
    usize::try_from(count).map_err(|_| Error::Config("negative Kafka array length".into()))
}

fn decode_required_acl_string(buffer: &mut bytes::Bytes, field: &str) -> Result<String> {
    KafkaString::decode(buffer)?
        .0
        .ok_or_else(|| Error::Config(format!("broker returned null {field}")))
}

#[derive(Debug, Serialize)]
struct AclRow {
    resource_type: String,
    resource_name: String,
    pattern_type: String,
    principal: String,
    host: String,
    operation: String,
    permission: String,
}

fn acl_table(rows: &[AclRow]) -> String {
    output::table(
        [
            "RESOURCE",
            "NAME",
            "PATTERN",
            "PRINCIPAL",
            "HOST",
            "OPERATION",
            "PERMISSION",
        ],
        rows.iter().map(|row| {
            [
                row.resource_type.clone(),
                row.resource_name.clone(),
                row.pattern_type.clone(),
                row.principal.clone(),
                row.host.clone(),
                row.operation.clone(),
                row.permission.clone(),
            ]
        }),
    )
}

fn duration_ms(duration: Duration) -> Result<i32> {
    i32::try_from(duration.as_millis())
        .map_err(|_| Error::Usage("timeout exceeds librdkafka's supported range".into()))
}

#[derive(Debug, Deserialize)]
struct TopicsToMoveFile {
    topics: Vec<TopicToMove>,
}

#[derive(Debug, Deserialize)]
struct TopicToMove {
    topic: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct ReassignmentFile {
    version: u8,
    partitions: Vec<ReassignmentPartition>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ReassignmentPartition {
    topic: String,
    partition: i32,
    replicas: Vec<i32>,
    #[serde(default)]
    log_dirs: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ReassignmentStatus {
    topic: String,
    partition: i32,
    replicas: Vec<i32>,
    adding_replicas: Vec<i32>,
    removing_replicas: Vec<i32>,
    complete: bool,
}

#[expect(
    clippy::too_many_lines,
    reason = "the branches correspond directly to Kafka's reassignment actions"
)]
async fn reassign(
    config: &rdkafka::ClientConfig,
    bootstrap: &str,
    command_config: Option<&Path>,
    timeout: Duration,
    format: OutputFormat,
    action: &ReassignAction,
) -> Result<()> {
    match action {
        ReassignAction::Generate {
            topics_to_move_json_file,
            broker_list,
            disable_rack_aware,
        } => {
            generate_reassignment(
                config,
                bootstrap,
                command_config,
                timeout,
                format,
                topics_to_move_json_file,
                broker_list,
                *disable_rack_aware,
            )
            .await
        }
        ReassignAction::Execute {
            reassignment_json_file,
            execute,
        } => {
            let plan = read_reassignment(reassignment_json_file)?;
            if !execute {
                println!(
                    "Would execute {} partition reassignment(s)",
                    plan.partitions.len()
                );
                return Ok(());
            }
            let client = config::protocol_admin(bootstrap, timeout, command_config).await?;
            let result = client
                .alter_partition_reassignments(reassignment_topics(&plan, false), timeout)
                .await?;
            let failures = result
                .topics
                .iter()
                .flat_map(|topic| &topic.partitions)
                .filter(|partition| partition.error.is_some())
                .count()
                + usize::from(result.error.is_some());
            if failures == 0 {
                let log_dir_failures = alter_reassignment_log_dirs(&client, &plan).await?;
                drop(client);
                if log_dir_failures != 0 {
                    return Err(Error::Partial {
                        failed: log_dir_failures,
                        total: plan.partitions.len(),
                    });
                }
                println!("Successfully started partition reassignment.");
                Ok(())
            } else {
                drop(client);
                Err(Error::Partial {
                    failed: failures,
                    total: plan.partitions.len(),
                })
            }
        }
        ReassignAction::Cancel {
            reassignment_json_file,
            execute,
        } => {
            let plan = read_reassignment(reassignment_json_file)?;
            if !execute {
                println!(
                    "Would cancel {} partition reassignment(s)",
                    plan.partitions.len()
                );
                return Ok(());
            }
            let client = config::protocol_admin(bootstrap, timeout, command_config).await?;
            let result = client
                .alter_partition_reassignments(reassignment_topics(&plan, true), timeout)
                .await?;
            drop(client);
            let failures = result
                .topics
                .iter()
                .flat_map(|topic| &topic.partitions)
                .filter(|partition| partition.error.is_some())
                .count()
                + usize::from(result.error.is_some());
            if failures == 0 {
                println!("Successfully cancelled partition reassignment.");
                Ok(())
            } else {
                Err(Error::Partial {
                    failed: failures,
                    total: plan.partitions.len(),
                })
            }
        }
        ReassignAction::Verify {
            reassignment_json_file,
        } => {
            let plan = read_reassignment(reassignment_json_file)?;
            let filters = reassignment_filters(&plan);
            let client = config::protocol_admin(bootstrap, timeout, command_config).await?;
            let running = client
                .list_partition_reassignments(Some(filters), timeout)
                .await?;
            drop(client);
            let current = current_replicas(config, timeout, &plan)?;
            let statuses = reassignment_statuses(&plan, &running, &current);
            output::write_value(format, "reassign.verify", &statuses, |rows| {
                reassignment_table(rows)
            })
        }
        ReassignAction::List => {
            let client = config::protocol_admin(bootstrap, timeout, command_config).await?;
            let running = client.list_partition_reassignments(None, timeout).await?;
            drop(client);
            let statuses = running
                .into_iter()
                .flat_map(|topic| {
                    topic
                        .partitions
                        .into_iter()
                        .map(move |partition| ReassignmentStatus {
                            topic: topic.name.clone(),
                            partition: partition.partition_index,
                            replicas: partition.replicas,
                            adding_replicas: partition.adding_replicas,
                            removing_replicas: partition.removing_replicas,
                            complete: false,
                        })
                })
                .collect::<Vec<_>>();
            output::write_value(format, "reassign.list", &statuses, |rows| {
                reassignment_table(rows)
            })
        }
    }
}

type BrokerLogDirPlan = BTreeMap<i32, BTreeMap<String, BTreeMap<String, Vec<i32>>>>;

fn broker_log_dir_plan(plan: &ReassignmentFile) -> BrokerLogDirPlan {
    let mut moves = BrokerLogDirPlan::new();
    for partition in &plan.partitions {
        for (broker, log_dir) in partition.replicas.iter().zip(&partition.log_dirs) {
            if log_dir != "any" {
                moves
                    .entry(*broker)
                    .or_default()
                    .entry(log_dir.clone())
                    .or_default()
                    .entry(partition.topic.clone())
                    .or_default()
                    .push(partition.partition);
            }
        }
    }
    moves
}

async fn alter_reassignment_log_dirs(
    client: &krafka::admin::AdminClient,
    plan: &ReassignmentFile,
) -> Result<usize> {
    let moves = broker_log_dir_plan(plan);
    if moves.is_empty() {
        return Ok(0);
    }
    let cluster = client.describe_cluster().await?;
    let mut failures = 0;
    for (broker_id, directories) in moves {
        let broker = cluster
            .brokers
            .iter()
            .find(|broker| broker.broker_id == broker_id)
            .ok_or_else(|| Error::Usage(format!("broker {broker_id} was not described")))?;
        let address = format!("{}:{}", broker.host, broker.port);
        let connection = client
            .pool()
            .get_connection_by_id(broker_id, &address)
            .await?;
        let directory_count = i32::try_from(directories.len())
            .map_err(|_| Error::Usage("too many replica log directories".into()))?;
        let mut response = connection
            .send_request(ApiKey::AlterReplicaLogDirs, 1, |buffer| {
                directory_count.try_encode(buffer)?;
                for (directory, topics) in &directories {
                    KafkaString(Some(directory.clone())).try_encode(buffer)?;
                    i32::try_from(topics.len())
                        .map_err(|_| krafka::KrafkaError::config("too many log-dir topics"))?
                        .try_encode(buffer)?;
                    for (topic, partitions) in topics {
                        KafkaString(Some(topic.clone())).try_encode(buffer)?;
                        i32::try_from(partitions.len())
                            .map_err(|_| {
                                krafka::KrafkaError::config("too many log-dir partitions")
                            })?
                            .try_encode(buffer)?;
                        for partition in partitions {
                            partition.try_encode(buffer)?;
                        }
                    }
                }
                Ok(())
            })
            .await?;
        drop(connection);
        let _throttle_time_ms = i32::decode(&mut response)?;
        let topic_count = decode_acl_count(&mut response)?;
        for _ in 0..topic_count {
            let _topic = decode_required_acl_string(&mut response, "log-dir topic")?;
            let partition_count = decode_acl_count(&mut response)?;
            for _ in 0..partition_count {
                let _partition = i32::decode(&mut response)?;
                if i16::decode(&mut response)? != 0 {
                    failures += 1;
                }
            }
        }
    }
    Ok(failures)
}

fn read_reassignment(path: &Path) -> Result<ReassignmentFile> {
    let plan: ReassignmentFile = serde_json::from_reader(std::fs::File::open(path)?)?;
    if plan.version != 1 {
        return Err(Error::Usage(format!(
            "unsupported reassignment JSON version: {}",
            plan.version
        )));
    }
    if plan.partitions.is_empty() {
        return Err(Error::Usage(
            "partition reassignment list cannot be empty".into(),
        ));
    }
    let mut targets = BTreeSet::new();
    for partition in &plan.partitions {
        if partition.topic.is_empty() || partition.partition < 0 {
            return Err(Error::Usage(
                "reassignment topic must be non-empty and partition must be non-negative".into(),
            ));
        }
        if !targets.insert((partition.topic.as_str(), partition.partition)) {
            return Err(Error::Usage(format!(
                "duplicate reassignment target {}:{}",
                partition.topic, partition.partition
            )));
        }
        if partition.replicas.is_empty() {
            return Err(Error::Usage(format!(
                "replica list cannot be empty for {}:{}",
                partition.topic, partition.partition
            )));
        }
        let mut replicas = partition.replicas.clone();
        replicas.sort_unstable();
        replicas.dedup();
        if replicas.len() != partition.replicas.len() {
            return Err(Error::Usage(format!(
                "replica list contains duplicates for {}:{}",
                partition.topic, partition.partition
            )));
        }
        if !partition.log_dirs.is_empty() && partition.log_dirs.len() != partition.replicas.len() {
            return Err(Error::Usage(format!(
                "log_dirs count must match replicas for {}:{}",
                partition.topic, partition.partition
            )));
        }
    }
    Ok(plan)
}

fn reassignment_topics(plan: &ReassignmentFile, cancel: bool) -> Vec<ReassignableTopic> {
    let mut topics: BTreeMap<String, Vec<ReassignablePartition>> = BTreeMap::new();
    for partition in &plan.partitions {
        topics
            .entry(partition.topic.clone())
            .or_default()
            .push(ReassignablePartition {
                partition_index: partition.partition,
                replicas: (!cancel).then(|| partition.replicas.clone()),
            });
    }
    topics
        .into_iter()
        .map(|(name, partitions)| ReassignableTopic { name, partitions })
        .collect()
}

fn reassignment_filters(plan: &ReassignmentFile) -> Vec<ListPartitionReassignmentsTopic> {
    let mut topics: BTreeMap<String, Vec<i32>> = BTreeMap::new();
    for partition in &plan.partitions {
        topics
            .entry(partition.topic.clone())
            .or_default()
            .push(partition.partition);
    }
    topics
        .into_iter()
        .map(
            |(name, partition_indexes)| ListPartitionReassignmentsTopic {
                name,
                partition_indexes,
            },
        )
        .collect()
}

fn reassignment_statuses(
    plan: &ReassignmentFile,
    running: &[krafka::admin::PartitionReassignmentInfo],
    current: &BTreeMap<(String, i32), Vec<i32>>,
) -> Vec<ReassignmentStatus> {
    plan.partitions
        .iter()
        .map(|target| {
            let active = running
                .iter()
                .find(|topic| topic.name == target.topic)
                .and_then(|topic| {
                    topic
                        .partitions
                        .iter()
                        .find(|partition| partition.partition_index == target.partition)
                });
            let current_replicas = current
                .get(&(target.topic.clone(), target.partition))
                .cloned()
                .unwrap_or_default();
            ReassignmentStatus {
                topic: target.topic.clone(),
                partition: target.partition,
                replicas: active.map_or_else(
                    || current_replicas.clone(),
                    |partition| partition.replicas.clone(),
                ),
                adding_replicas: active
                    .map(|partition| partition.adding_replicas.clone())
                    .unwrap_or_default(),
                removing_replicas: active
                    .map(|partition| partition.removing_replicas.clone())
                    .unwrap_or_default(),
                complete: active.is_none() && current_replicas == target.replicas,
            }
        })
        .collect()
}

fn current_replicas(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    plan: &ReassignmentFile,
) -> Result<BTreeMap<(String, i32), Vec<i32>>> {
    let metadata = base_consumer(config)?.fetch_metadata(None, timeout)?;
    let targets = plan
        .partitions
        .iter()
        .map(|partition| (partition.topic.as_str(), partition.partition))
        .collect::<BTreeSet<_>>();
    Ok(metadata
        .topics()
        .iter()
        .flat_map(|topic| {
            topic
                .partitions()
                .iter()
                .filter(|partition| targets.contains(&(topic.name(), partition.id())))
                .map(|partition| {
                    (
                        (topic.name().to_owned(), partition.id()),
                        partition.replicas().to_vec(),
                    )
                })
        })
        .collect())
}

fn reassignment_table(rows: &[ReassignmentStatus]) -> String {
    output::table(
        [
            "TOPIC",
            "PARTITION",
            "STATUS",
            "REPLICAS",
            "ADDING",
            "REMOVING",
        ],
        rows.iter().map(|row| {
            [
                row.topic.clone(),
                row.partition.to_string(),
                if row.complete {
                    "COMPLETED".into()
                } else {
                    "IN_PROGRESS".into()
                },
                csv_numbers(&row.replicas),
                csv_numbers(&row.adding_replicas),
                csv_numbers(&row.removing_replicas),
            ]
        }),
    )
}

#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "arguments and phases mirror Kafka's generate-assignment command contract"
)]
async fn generate_reassignment(
    config: &rdkafka::ClientConfig,
    bootstrap: &str,
    command_config: Option<&Path>,
    timeout: Duration,
    format: OutputFormat,
    path: &Path,
    broker_list: &str,
    disable_rack_aware: bool,
) -> Result<()> {
    let input: TopicsToMoveFile = serde_json::from_reader(std::fs::File::open(path)?)?;
    let brokers = parse_partitions(Some(broker_list))?
        .filter(|brokers| !brokers.is_empty())
        .ok_or_else(|| Error::Usage("--broker-list must not be empty".into()))?;
    if brokers.iter().collect::<BTreeSet<_>>().len() != brokers.len() {
        return Err(Error::Usage(
            "--broker-list contains duplicate broker IDs".into(),
        ));
    }
    if input
        .topics
        .iter()
        .map(|topic| topic.topic.as_str())
        .collect::<BTreeSet<_>>()
        .len()
        != input.topics.len()
    {
        return Err(Error::Usage(
            "topics-to-move JSON contains duplicate topics".into(),
        ));
    }
    let metadata = base_consumer(config)?.fetch_metadata(None, timeout)?;
    let known_brokers = metadata
        .brokers()
        .iter()
        .map(rdkafka::metadata::MetadataBroker::id)
        .collect::<Vec<_>>();
    if let Some(unknown) = brokers.iter().find(|id| !known_brokers.contains(id)) {
        return Err(Error::Usage(format!(
            "broker {unknown} is not in cluster metadata"
        )));
    }
    let protocol = config::protocol_admin(bootstrap, timeout, command_config).await?;
    let cluster = protocol.describe_cluster().await?;
    drop(protocol);
    let broker_racks = brokers
        .iter()
        .map(|broker_id| {
            cluster
                .brokers
                .iter()
                .find(|broker| broker.broker_id == *broker_id)
                .map(|broker| {
                    (
                        *broker_id,
                        (!disable_rack_aware).then(|| broker.rack.clone()).flatten(),
                    )
                })
                .ok_or_else(|| Error::Usage(format!("broker {broker_id} was not described")))
        })
        .collect::<Result<Vec<_>>>()?;
    let rackless = broker_racks
        .iter()
        .filter(|(_, rack)| rack.is_none())
        .count();
    if !disable_rack_aware && rackless != 0 && rackless != broker_racks.len() {
        return Err(Error::Usage(
            "not all brokers have rack information; use --disable-rack-aware".into(),
        ));
    }
    let mut partitions = Vec::new();
    for selected in &input.topics {
        let topic = metadata
            .topics()
            .iter()
            .find(|topic| topic.name() == selected.topic)
            .ok_or_else(|| Error::Usage(format!("topic {} not found", selected.topic)))?;
        let replication_factor = topic
            .partitions()
            .first()
            .map_or(0, |partition| partition.replicas().len());
        let assignments = striped_replica_assignment(
            &broker_racks,
            topic.partitions().len(),
            replication_factor,
        )?;
        for (partition, replicas) in topic.partitions().iter().zip(assignments) {
            if replication_factor > brokers.len() {
                return Err(Error::Usage(format!(
                    "topic {} requires {replication_factor} distinct brokers, but only {} were supplied",
                    selected.topic,
                    brokers.len()
                )));
            }
            partitions.push(ReassignmentPartition {
                topic: selected.topic.clone(),
                partition: partition.id(),
                log_dirs: vec!["any".into(); replicas.len()],
                replicas,
            });
        }
    }
    let proposal = ReassignmentFile {
        version: 1,
        partitions,
    };
    output::write_value(format, "reassign.generate", &proposal, |proposal| {
        output::table(
            ["TOPIC", "PARTITION", "REPLICAS", "LOG_DIRS"],
            proposal.partitions.iter().map(|partition| {
                [
                    partition.topic.clone(),
                    partition.partition.to_string(),
                    csv_numbers(&partition.replicas),
                    partition.log_dirs.join(","),
                ]
            }),
        )
    })
}

fn striped_replica_assignment(
    brokers: &[(i32, Option<String>)],
    partition_count: usize,
    replication_factor: usize,
) -> Result<Vec<Vec<i32>>> {
    if replication_factor == 0 {
        return Err(Error::Usage("replication factor must be positive".into()));
    }
    if replication_factor > brokers.len() {
        return Err(Error::Usage(format!(
            "replication factor {replication_factor} exceeds {} available brokers",
            brokers.len()
        )));
    }
    let mut racks = BTreeMap::<Option<String>, Vec<i32>>::new();
    for (broker, rack) in brokers {
        racks.entry(rack.clone()).or_default().push(*broker);
    }
    for rack in racks.values_mut() {
        rack.sort_unstable();
    }
    let max_rack_size = racks.values().map(Vec::len).max().unwrap_or_default();
    let mut striped = Vec::with_capacity(brokers.len());
    for index in 0..max_rack_size {
        for rack in racks.values() {
            if let Some(broker) = rack.get(index) {
                striped.push(*broker);
            }
        }
    }
    let rack_by_broker = brokers
        .iter()
        .map(|(broker, rack)| (*broker, rack.clone()))
        .collect::<BTreeMap<_, _>>();
    let desired_distinct_racks = replication_factor.min(racks.len());
    let mut assignments = Vec::with_capacity(partition_count);
    for partition in 0..partition_count {
        let start = partition % striped.len();
        let leader = striped[start];
        let mut replicas = vec![leader];
        let mut used_racks = BTreeSet::from([rack_by_broker[&leader].clone()]);
        for offset in 1..striped.len() {
            let candidate = striped[(start + offset) % striped.len()];
            let rack = &rack_by_broker[&candidate];
            if used_racks.len() < desired_distinct_racks && used_racks.insert(rack.clone()) {
                replicas.push(candidate);
                if replicas.len() == replication_factor {
                    break;
                }
            }
        }
        if replicas.len() < replication_factor {
            for offset in 1..striped.len() {
                let candidate = striped[(start + offset) % striped.len()];
                if !replicas.contains(&candidate) {
                    replicas.push(candidate);
                    if replicas.len() == replication_factor {
                        break;
                    }
                }
            }
        }
        assignments.push(replicas);
    }
    Ok(assignments)
}

#[expect(
    clippy::too_many_arguments,
    reason = "arguments map directly to the leader-election CLI contract"
)]
fn leader_election(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    format: OutputFormat,
    kind: ElectionType,
    topic: Option<&str>,
    partition: Option<i32>,
    all: bool,
    execute: bool,
) -> Result<()> {
    let target = match (all, topic, partition) {
        (true, None, None) => None,
        (false, Some(topic), Some(partition)) => Some((topic, partition)),
        _ => {
            return Err(Error::Usage(
                "use --all-topic-partitions or both --topic and --partition".into(),
            ));
        }
    };
    if !execute {
        match target {
            Some((topic, partition)) => {
                println!("Would trigger leader election for {topic}:{partition}");
            }
            None => println!("Would trigger leader election for all topic partitions"),
        }
        return Ok(());
    }
    let admin = admin(config)?;
    let rows = crate::ffi::elect_leaders(
        admin.inner().native_ptr(),
        matches!(kind, ElectionType::Unclean),
        target,
        duration_ms(timeout)?,
    )?;
    let failures = rows.iter().filter(|row| row.error.is_some()).count();
    output::write_value(format, "leader-election", &rows, |rows| {
        output::table(
            ["TOPIC", "PARTITION", "ERROR"],
            rows.iter().map(|row| {
                [
                    row.topic.clone(),
                    row.partition.to_string(),
                    row.error.as_deref().unwrap_or("-").to_owned(),
                ]
            }),
        )
    })?;
    if failures == 0 {
        Ok(())
    } else {
        Err(Error::Partial {
            failed: failures,
            total: rows.len(),
        })
    }
}

#[derive(Debug, Serialize)]
struct LogDirRow {
    broker: i32,
    log_dir: String,
    error: Option<String>,
    total_bytes: i64,
    usable_bytes: i64,
    topic: String,
    partition: i32,
    size: i64,
    offset_lag: i64,
    is_future: bool,
}

async fn log_dirs(
    bootstrap: &str,
    command_config: Option<&Path>,
    timeout: Duration,
    format: OutputFormat,
    brokers: Option<&str>,
    topics: Option<&str>,
) -> Result<()> {
    let broker_filter = parse_partitions(brokers)?.unwrap_or_default();
    let client = config::protocol_admin(bootstrap, timeout, command_config).await?;
    let topic_filter = if let Some(topics) = topics {
        let topic_names = parse_topic_names(topics)?;
        let metadata = client.describe_topics(&topic_names).await?;
        let mut filter = Vec::with_capacity(topic_names.len());
        for topic in topic_names {
            let info = metadata
                .get(&topic)
                .ok_or_else(|| Error::Usage(format!("topic does not exist: {topic}")))?;
            let mut partitions = info.partitions.keys().copied().collect::<Vec<_>>();
            partitions.sort_unstable();
            filter.push(DescribableLogDirTopic { topic, partitions });
        }
        Some(filter)
    } else {
        None
    };
    let directories = client.describe_log_dirs(topic_filter).await?;
    drop(client);
    let rows = directories
        .into_iter()
        .filter(|directory| {
            broker_filter.is_empty() || broker_filter.contains(&directory.broker_id)
        })
        .flat_map(|directory| {
            directory.topics.into_iter().flat_map(move |topic| {
                let log_dir = directory.log_dir.clone();
                let error = directory.error.clone();
                topic
                    .partitions
                    .into_iter()
                    .map(move |partition| LogDirRow {
                        broker: directory.broker_id,
                        log_dir: log_dir.clone(),
                        error: error.clone(),
                        total_bytes: directory.total_bytes,
                        usable_bytes: directory.usable_bytes,
                        topic: topic.name.clone(),
                        partition: partition.partition_index,
                        size: partition.partition_size,
                        offset_lag: partition.offset_lag,
                        is_future: partition.is_future_key,
                    })
            })
        })
        .collect::<Vec<_>>();
    output::write_value(format, "log-dirs", &rows, |rows| {
        output::table(
            [
                "BROKER",
                "LOG_DIR",
                "TOPIC",
                "PARTITION",
                "SIZE",
                "OFFSET_LAG",
                "FUTURE",
                "ERROR",
            ],
            rows.iter().map(|row| {
                [
                    row.broker.to_string(),
                    row.log_dir.clone(),
                    row.topic.clone(),
                    row.partition.to_string(),
                    row.size.to_string(),
                    row.offset_lag.to_string(),
                    row.is_future.to_string(),
                    row.error.as_deref().unwrap_or("-").to_owned(),
                ]
            }),
        )
    })
}

fn parse_topic_names(value: &str) -> Result<Vec<String>> {
    let topics = value
        .split(',')
        .map(str::trim)
        .filter(|topic| !topic.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if topics.is_empty() {
        Err(Error::Usage("topic list must not be empty".into()))
    } else {
        Ok(topics)
    }
}

fn parse_pairs(values: &[String]) -> Result<Vec<(String, String)>> {
    values
        .iter()
        .map(|value| {
            value
                .split_once('=')
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .ok_or_else(|| Error::Usage(format!("expected key=value, got {value}")))
        })
        .collect()
}

fn parse_replica_assignment(value: &str) -> Result<Vec<Vec<i32>>> {
    let assignments = value
        .split(',')
        .enumerate()
        .map(|(partition, brokers)| {
            let brokers = brokers
                .split(':')
                .map(str::trim)
                .map(|broker| {
                    broker.parse::<i32>().map_err(|_| {
                        Error::Usage(format!(
                            "invalid broker ID in partition {partition}: {broker}"
                        ))
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            if brokers.is_empty() {
                return Err(Error::Usage(format!(
                    "partition {partition} has no replicas"
                )));
            }
            let unique = brokers.iter().copied().collect::<BTreeSet<_>>();
            if unique.len() != brokers.len() {
                return Err(Error::Usage(format!(
                    "partition {partition} contains duplicate brokers"
                )));
            }
            Ok(brokers)
        })
        .collect::<Result<Vec<_>>>()?;
    let replication_factor = assignments.first().map_or(0, Vec::len);
    if assignments.is_empty()
        || replication_factor == 0
        || assignments
            .iter()
            .any(|brokers| brokers.len() != replication_factor)
    {
        return Err(Error::Usage(
            "all partitions must have the same non-zero replication factor".into(),
        ));
    }
    Ok(assignments)
}

fn parse_partitions(value: Option<&str>) -> Result<Option<Vec<i32>>> {
    value
        .map(|value| {
            value
                .split(',')
                .map(|item| {
                    item.parse::<i32>()
                        .map_err(|_| Error::Usage(format!("invalid partition: {item}")))
                })
                .collect()
        })
        .transpose()
}

fn csv_numbers(values: &[i32]) -> String {
    values
        .iter()
        .map(i32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;
    use std::io::Write as _;

    fn topic_selector() -> TopicSelector {
        TopicSelector {
            topic: None,
            exclude_internal: false,
            under_replicated_partitions: false,
            unavailable_partitions: false,
            under_min_isr_partitions: false,
            at_min_isr_partitions: false,
            topics_with_overrides: false,
        }
    }

    #[test]
    fn parse_pairs_should_retain_equals_in_value() {
        let pairs = parse_pairs(&["password=a=b".into()]).expect("valid pair");
        assert_eq!(pairs, [("password".into(), "a=b".into())]);
    }

    #[test]
    fn scram_changes_should_parse_upserts_and_deletes_without_exposing_passwords() {
        let changes = parse_scram_changes(
            &[(
                "SCRAM-SHA-256".into(),
                "[iterations=8192,password=a=b]".into(),
            )],
            &["SCRAM-SHA-512".into()],
        )
        .expect("valid SCRAM changes");

        assert!(matches!(
            &changes[0],
            ffi::ScramCredentialAlteration::Upsert {
                mechanism: ffi::ScramMechanism::Sha256,
                iterations: 8192,
                password,
            } if password == b"a=b"
        ));
        assert!(matches!(
            changes[1],
            ffi::ScramCredentialAlteration::Delete {
                mechanism: ffi::ScramMechanism::Sha512,
            }
        ));
    }

    #[test]
    fn scram_changes_should_reject_weak_iterations_and_duplicate_mechanisms() {
        assert!(matches!(
            parse_scram_changes(
                &[(
                    "SCRAM-SHA-256".into(),
                    "[iterations=1024,password=secret]".into(),
                )],
                &[],
            ),
            Err(Error::Usage(_))
        ));
        assert!(matches!(
            parse_scram_changes(
                &[("SCRAM-SHA-256".into(), "[password=secret]".into(),)],
                &["SCRAM-SHA-256".into()],
            ),
            Err(Error::Usage(_))
        ));
    }

    #[test]
    fn client_properties_should_override_existing_configuration() {
        let mut config = rdkafka::ClientConfig::new();
        config.set("client.id", "before");

        apply_client_properties(&mut config, &["client.id=after".into()])
            .expect("valid client property");

        assert_eq!(config.get("client.id"), Some("after"));
    }

    #[test]
    fn replica_assignment_should_validate_brokers_and_replication_factor() {
        assert_eq!(
            parse_replica_assignment("1:2, 2:3").expect("valid assignment"),
            [vec![1, 2], vec![2, 3]]
        );
        assert!(matches!(
            parse_replica_assignment("1:1,1:2"),
            Err(Error::Usage(_))
        ));
        assert!(matches!(
            parse_replica_assignment("1,1:2"),
            Err(Error::Usage(_))
        ));
    }

    #[test]
    fn parse_partitions_should_reject_non_integer() {
        let result = parse_partitions(Some("0,nope"));
        assert!(matches!(result, Err(Error::Usage(_))));
    }

    #[test]
    fn topic_partition_pattern_should_match_bounded_ranges() {
        let patterns = parse_topic_partition_patterns("events:1-3,audit:5-")
            .expect("valid topic-partition patterns");

        assert!(patterns[0].matches("events", 2) && patterns[1].matches("audit", 8));
    }

    #[test]
    fn topic_partition_pattern_should_reject_reversed_ranges() {
        assert!(matches!(
            parse_topic_partition_patterns("events:3-1"),
            Err(Error::Usage(_))
        ));
    }

    #[test]
    fn parse_topic_names_should_trim_and_reject_empty_lists() {
        assert_eq!(
            parse_topic_names(" orders, payments ").expect("valid topics"),
            ["orders", "payments"]
        );
        assert!(matches!(parse_topic_names(", ,"), Err(Error::Usage(_))));
    }

    #[test]
    fn topic_pattern_should_use_java_style_full_match() {
        let pattern = topic_pattern("events-.*").expect("valid expression");
        assert!(pattern.is_match("events-orders"));
        assert!(!pattern.is_match("archived-events-orders"));
    }

    #[test]
    fn topic_pattern_should_reject_invalid_expression() {
        assert!(matches!(topic_pattern("["), Err(Error::Usage(_))));
    }

    #[test]
    fn topic_partition_should_match_when_isr_is_under_minimum() {
        let selector = TopicSelector {
            under_min_isr_partitions: true,
            ..topic_selector()
        };

        assert!(topic_partition_matches(&selector, 1, 3, Some(2), true));
    }

    #[test]
    fn topic_partition_should_match_when_isr_is_at_minimum() {
        let selector = TopicSelector {
            at_min_isr_partitions: true,
            ..topic_selector()
        };

        assert!(topic_partition_matches(&selector, 2, 3, Some(2), true));
    }

    #[test]
    fn topic_partition_should_report_a_leader_missing_from_live_brokers() {
        let selector = TopicSelector {
            unavailable_partitions: true,
            ..topic_selector()
        };

        assert!(topic_partition_matches(&selector, 1, 1, None, false));
    }

    #[test]
    fn producer_input_should_parse_json_key_partition_and_headers() {
        let args = crate::cli::ProduceArgs {
            topic: "events".into(),
            key_separator: None,
            parse_key: false,
            compression_type: "none".into(),
            acks: "all".into(),
            json: true,
            properties: Vec::new(),
        };
        let input = producer_input(
            r#"{"key":"order-1","value":"created","partition":2,"headers":{"trace":"abc","empty":null}}"#,
            &args,
        )
        .expect("valid JSON record");
        assert_eq!(input.key.as_deref(), Some("order-1"));
        assert_eq!(input.value, "created");
        assert_eq!(input.partition, Some(2));
        assert_eq!(input.headers["trace"].as_deref(), Some("abc"));
        assert_eq!(input.headers["empty"], None);
    }

    #[test]
    fn producer_input_should_reject_negative_json_partition() {
        let args = crate::cli::ProduceArgs {
            topic: "events".into(),
            key_separator: None,
            parse_key: false,
            compression_type: "none".into(),
            acks: "all".into(),
            json: true,
            properties: Vec::new(),
        };
        assert!(matches!(
            producer_input(r#"{"value":"bad","partition":-1}"#, &args),
            Err(Error::Usage(_))
        ));
    }

    #[test]
    fn shifted_offset_should_use_committed_offset_and_clamp_to_log_range() {
        assert_eq!(shifted_offset(Some(5), 3, 0, 10, 0).expect("shift"), 8);
        assert_eq!(shifted_offset(Some(5), 20, 0, 10, 0).expect("shift"), 10);
        assert!(matches!(
            shifted_offset(None, 1, 0, 10, 2),
            Err(Error::Usage(message)) if message.contains("partition 2")
        ));
    }

    #[test]
    fn group_offset_lag_should_use_log_end_and_clamp_at_zero() {
        assert_eq!(group_offset_lag(12, Some(10)), Some(0));
    }

    #[test]
    fn group_offset_lag_should_be_unknown_without_a_commit() {
        assert_eq!(group_offset_lag(-1, Some(10)), None);
    }

    #[test]
    fn reset_time_specs_should_parse_kafka_formats() {
        assert_eq!(
            parse_datetime_millis("2026-08-03T12:30:45.123Z").expect("datetime"),
            1_785_760_245_123
        );
        assert_eq!(
            parse_iso8601_duration_millis("P1DT2H3M4S").expect("duration"),
            93_784_000
        );
        assert!(parse_iso8601_duration_millis("1 hour").is_err());
    }

    #[test]
    fn acl_bindings_should_expand_allow_and_deny_principals() {
        let cli = Cli::try_parse_from([
            "kafka",
            "--bootstrap-server",
            "localhost:9092",
            "acls",
            "add",
            "--topic",
            "orders",
            "--allow-principal",
            "User:reader",
            "--deny-principal",
            "User:blocked",
            "--allow-host",
            "10.0.0.1",
            "--operation",
            "read",
        ])
        .expect("ACL command");
        let Command::Acls(args) = cli.command else {
            panic!("expected ACL command");
        };
        let AclAction::Add(mutation) = args.action else {
            panic!("expected ACL add");
        };
        let operations = acl_operations(&mutation.operation).expect("operations");
        let bindings = acl_bindings(&mutation, &operations).expect("bindings");
        assert_eq!(bindings.len(), 2);
        assert!(bindings.iter().any(|binding| {
            binding.principal == "User:reader"
                && binding.host == "10.0.0.1"
                && binding.permission_type == AclPermissionType::Allow
        }));
        assert!(bindings.iter().any(|binding| {
            binding.principal == "User:blocked"
                && binding.host == "*"
                && binding.permission_type == AclPermissionType::Deny
        }));
    }

    #[test]
    fn delete_records_should_reject_duplicate_targets() {
        let mut file = tempfile::NamedTempFile::new().expect("temporary file");
        write!(
            file,
            r#"{{"version":1,"partitions":[{{"topic":"events","partition":0,"offset":1}},{{"topic":"events","partition":0,"offset":2}}]}}"#
        )
        .expect("write delete-records fixture");
        assert!(matches!(
            read_delete_records(file.path()),
            Err(Error::Usage(message)) if message.contains("duplicate")
        ));
    }

    #[test]
    fn striped_assignment_should_spread_leaders_and_racks() {
        let brokers = vec![
            (1, Some("rack-a".into())),
            (2, Some("rack-b".into())),
            (3, Some("rack-c".into())),
            (4, Some("rack-a".into())),
            (5, Some("rack-b".into())),
            (6, Some("rack-c".into())),
        ];
        let assignments = striped_replica_assignment(&brokers, 6, 3).expect("valid assignment");
        let rack_by_broker = brokers.into_iter().collect::<BTreeMap<_, _>>();
        assert_eq!(
            assignments
                .iter()
                .map(|replicas| replicas[0])
                .collect::<BTreeSet<_>>()
                .len(),
            6
        );
        for replicas in assignments {
            assert_eq!(replicas.iter().collect::<BTreeSet<_>>().len(), 3);
            assert_eq!(
                replicas
                    .iter()
                    .map(|broker| &rack_by_broker[broker])
                    .collect::<BTreeSet<_>>()
                    .len(),
                3
            );
        }
    }

    #[test]
    fn striped_assignment_should_reject_excessive_replication_factor() {
        assert!(matches!(
            striped_replica_assignment(&[(1, None), (2, None)], 1, 3),
            Err(Error::Usage(_))
        ));
    }

    #[test]
    fn broker_log_dir_plan_should_route_each_replica_to_its_broker() {
        let plan = ReassignmentFile {
            version: 1,
            partitions: vec![ReassignmentPartition {
                topic: "events".into(),
                partition: 0,
                replicas: vec![1, 2, 3],
                log_dirs: vec!["/data/a".into(), "any".into(), "/data/c".into()],
            }],
        };
        let moves = broker_log_dir_plan(&plan);
        assert_eq!(moves[&1]["/data/a"]["events"], [0]);
        assert_eq!(moves[&3]["/data/c"]["events"], [0]);
        assert!(!moves.contains_key(&2));
    }

    #[test]
    fn reassignment_should_accept_omitted_log_dirs() {
        let mut file = tempfile::NamedTempFile::new().expect("temporary file");
        write!(
            file,
            r#"{{"version":1,"partitions":[{{"topic":"events","partition":0,"replicas":[1]}}]}}"#
        )
        .expect("write reassignment");
        let plan = read_reassignment(file.path()).expect("valid reassignment");
        assert!(plan.partitions[0].log_dirs.is_empty());
    }

    #[test]
    fn reassignment_should_reject_duplicate_partition_targets() {
        let mut file = tempfile::NamedTempFile::new().expect("temporary file");
        write!(
            file,
            r#"{{"version":1,"partitions":[{{"topic":"events","partition":0,"replicas":[1]}},{{"topic":"events","partition":0,"replicas":[2]}}]}}"#
        )
        .expect("write reassignment");
        assert!(matches!(
            read_reassignment(file.path()),
            Err(Error::Usage(_))
        ));
    }

    #[test]
    fn reassignment_verify_should_compare_final_metadata() {
        let plan = ReassignmentFile {
            version: 1,
            partitions: vec![ReassignmentPartition {
                topic: "events".into(),
                partition: 0,
                replicas: vec![2, 1],
                log_dirs: Vec::new(),
            }],
        };
        let mut current = BTreeMap::new();
        current.insert(("events".into(), 0), vec![1, 2]);
        let status = reassignment_statuses(&plan, &[], &current);
        assert!(!status[0].complete, "replica order is significant");

        current.insert(("events".into(), 0), vec![2, 1]);
        let status = reassignment_statuses(&plan, &[], &current);
        assert!(status[0].complete);
        assert_eq!(status[0].replicas, [2, 1]);
    }
}
