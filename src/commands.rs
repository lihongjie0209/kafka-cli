//! Kafka command implementations.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Write},
    path::Path,
    process,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use chrono::{DateTime, NaiveDateTime, Utc};
use futures::StreamExt;
use krafka::protocol::{
    AlterConfigOp, AlterableConfig, ApiKey, ConfigResourceType as ProtocolConfigResourceType,
    Decode, DescribableLogDirTopic, DescribeClusterRequest, DescribeClusterResponse,
    DescribeConfigsRequest, DescribeConfigsResource, IncrementalAlterConfigsRequest,
    IncrementalAlterConfigsResource, KafkaString, ListPartitionReassignmentsTopic,
    ReassignablePartition, ReassignableTopic, TaggedFields, TryEncode, VersionedDecode,
    VersionedEncode, versions,
};
use rdkafka::{
    Message, Offset,
    admin::{
        AdminClient, AdminOptions, ConfigSource, NewPartitions, NewTopic, OwnedResourceSpecifier,
        ResourceSpecifier, TopicReplication,
    },
    client::DefaultClientContext,
    consumer::{BaseConsumer, Consumer, StreamConsumer},
    error::{KafkaError, RDKafkaErrorCode},
    message::{BorrowedHeaders, Header, Headers, OwnedHeaders, ToBytes},
    producer::{DeliveryFuture, FutureProducer, FutureRecord},
    topic_partition_list::TopicPartitionList,
};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::{
    cli::{
        AclAction, Cli, ClusterAction, Command, ConfigAction, ConfigEntityType, DescribeTopicArgs,
        ElectionType, GroupAction, ListTopicArgs, OffsetTime, ReassignAction, ResetOffsetsArgs,
        TopicAction,
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

#[derive(Debug, Serialize)]
struct MutationRow {
    resource: String,
    status: String,
    error: Option<String>,
}

fn write_mutation_rows(format: OutputFormat, command: &str, rows: &[MutationRow]) -> Result<()> {
    output::write_value(format, command, &rows, |rows| {
        output::table(
            ["RESOURCE", "STATUS", "ERROR"],
            rows.iter().map(|row| {
                [
                    row.resource.clone(),
                    row.status.clone(),
                    row.error.as_deref().unwrap_or("-").to_owned(),
                ]
            }),
        )
    })
}

/// Executes one top-level command.
pub async fn execute(cli: Cli) -> Result<()> {
    if let Command::Groups(args) = &cli.command
        && let GroupAction::ValidateRegex { regex } = &args.action
    {
        return validate_group_regex(cli.output, regex);
    }
    let bootstrap = cli.bootstrap_server.as_deref().ok_or_else(|| {
        Error::Usage("--bootstrap-server is required (or set KAFKA_CLI_BOOTSTRAP_SERVER)".into())
    })?;
    let client_config = config::client_config(bootstrap, cli.command_config.as_deref())?;
    let command_config = cli.command_config.clone();
    let timeout = cli.timeout();
    let format = cli.output;
    let verbose = cli.verbose > 0;

    match cli.command {
        Command::Topics(args) => topics(&client_config, timeout, format, args.action).await,
        Command::Produce(args) => produce(client_config, args).await,
        Command::Consume(args) => consume(client_config, timeout, args).await,
        Command::Groups(args) => {
            groups(&client_config, timeout, format, args.action, verbose).await
        }
        Command::Configs(args) => {
            configs(
                &client_config,
                bootstrap,
                command_config.as_deref(),
                timeout,
                format,
                args.action,
            )
            .await
        }
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
            args.path_to_json_file.as_deref(),
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
    topic_id: String,
    partition: i32,
    leader: i32,
    replication_factor: usize,
    replicas: Vec<i32>,
    isr: Vec<i32>,
    configs: Vec<String>,
}

#[derive(Debug, Serialize)]
struct TopicConfigSummary {
    topic: String,
    configs: Vec<String>,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "internal normalized form of Kafka's independent topic describe filters"
)]
struct TopicSelector {
    topic: Option<String>,
    topic_id: Option<String>,
    exclude_internal: bool,
    under_replicated_partitions: bool,
    unavailable_partitions: bool,
    under_min_isr_partitions: bool,
    at_min_isr_partitions: bool,
    topics_with_overrides: bool,
}

impl From<ListTopicArgs> for TopicSelector {
    fn from(args: ListTopicArgs) -> Self {
        Self {
            topic: args.topic,
            topic_id: None,
            exclude_internal: args.exclude_internal,
            under_replicated_partitions: false,
            unavailable_partitions: false,
            under_min_isr_partitions: false,
            at_min_isr_partitions: false,
            topics_with_overrides: false,
        }
    }
}

impl From<&DescribeTopicArgs> for TopicSelector {
    fn from(args: &DescribeTopicArgs) -> Self {
        Self {
            topic: args.topic.clone(),
            topic_id: args.topic_id.clone(),
            exclude_internal: args.exclude_internal,
            under_replicated_partitions: args.under_replicated_partitions,
            unavailable_partitions: args.unavailable_partitions,
            under_min_isr_partitions: args.under_min_isr_partitions,
            at_min_isr_partitions: args.at_min_isr_partitions,
            topics_with_overrides: args.topics_with_overrides,
        }
    }
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
        TopicAction::List(args) => {
            let selector = TopicSelector::from(args);
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
        TopicAction::Describe(args) => {
            let selector = TopicSelector::from(&args);
            validate_topic_id(selector.topic_id.as_deref())?;
            let consumer = base_consumer(config)?;
            let candidate_topic_names = {
                let metadata = consumer.fetch_metadata(None, timeout)?;
                select_topics(&metadata, &selector)?
                    .into_iter()
                    .map(|topic| topic.name().to_owned())
                    .collect::<Vec<_>>()
            };
            let topic_identities = if candidate_topic_names.is_empty() {
                Vec::new()
            } else {
                let client = admin(config)?;
                ffi::describe_topic_identities(
                    client.inner().native_ptr(),
                    &candidate_topic_names,
                    duration_ms(timeout)?,
                )?
            };
            let topic_ids = topic_identities
                .into_iter()
                .filter(|identity| {
                    selector
                        .topic_id
                        .as_deref()
                        .filter(|topic_id| is_nonzero_topic_id(topic_id))
                        .is_none_or(|topic_id| topic_id == identity.id)
                })
                .map(|identity| (identity.name, identity.id))
                .collect::<BTreeMap<_, _>>();
            if selector
                .topic_id
                .as_deref()
                .is_some_and(is_nonzero_topic_id)
                && topic_ids.is_empty()
                && !args.if_exists
            {
                return Err(Error::Usage("no topic matched --topic-id".into()));
            }
            if selector.topic.is_some()
                && !selector
                    .topic_id
                    .as_deref()
                    .is_some_and(is_nonzero_topic_id)
                && topic_ids.is_empty()
                && !args.if_exists
            {
                return Err(Error::Usage("no topic matched --topic".into()));
            }
            let selected_topic_names = topic_ids.keys().cloned().collect::<Vec<_>>();
            let resources = selected_topic_names
                .iter()
                .map(|topic| ResourceSpecifier::Topic(topic))
                .collect::<Vec<_>>();
            let topic_configs = if resources.is_empty() {
                Vec::new()
            } else {
                admin(config)?
                    .describe_configs(
                        &resources,
                        &AdminOptions::new().request_timeout(Some(timeout)),
                    )
                    .await?
                    .into_iter()
                    .map(|result| result.map_err(|code| Error::Config(code.to_string())))
                    .collect::<Result<Vec<_>>>()?
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
            let effective_configs = topic_configs
                .iter()
                .filter_map(|resource| {
                    let OwnedResourceSpecifier::Topic(topic) = &resource.specifier else {
                        return None;
                    };
                    let mut configs = resource
                        .entries
                        .iter()
                        .filter(|entry| entry.source != ConfigSource::Default)
                        .map(|entry| {
                            format!(
                                "{}={}",
                                entry.name,
                                entry.value.as_deref().unwrap_or("null")
                            )
                        })
                        .collect::<Vec<_>>();
                    configs.sort();
                    Some((topic.as_str(), configs))
                })
                .collect::<BTreeMap<_, _>>();
            let live_brokers = metadata
                .brokers()
                .iter()
                .map(rdkafka::metadata::MetadataBroker::id)
                .collect::<BTreeSet<_>>();
            let selector = &selector;
            let live_brokers = &live_brokers;
            let effective_configs = &effective_configs;
            let rows = select_topics(&metadata, selector)?
                .into_iter()
                .filter(|topic| topic_ids.contains_key(topic.name()))
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
                            topic_id: topic_ids.get(topic.name()).cloned().unwrap_or_default(),
                            partition: partition.id(),
                            leader: partition.leader(),
                            replication_factor: partition.replicas().len(),
                            replicas: partition.replicas().to_vec(),
                            isr: partition.isr().to_vec(),
                            configs: effective_configs
                                .get(topic.name())
                                .cloned()
                                .unwrap_or_default(),
                        })
                })
                .collect::<Vec<_>>();
            output::write_value(format, "topics.describe", &rows, |rows| {
                output::table(
                    [
                        "TOPIC",
                        "TOPIC_ID",
                        "PARTITION",
                        "LEADER",
                        "REPLICATION_FACTOR",
                        "REPLICAS",
                        "ISR",
                        "CONFIGS",
                    ],
                    rows.iter().map(|row| {
                        [
                            row.topic.clone(),
                            row.topic_id.clone(),
                            row.partition.to_string(),
                            row.leader.to_string(),
                            row.replication_factor.to_string(),
                            csv_numbers(&row.replicas),
                            csv_numbers(&row.isr),
                            row.configs.join(","),
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
            validate_topic_creation_counts(args.partitions, args.replication_factor)?;
            let assignment_refs = assignments
                .as_ref()
                .map(|items| items.iter().map(Vec::as_slice).collect::<Vec<&[i32]>>());
            let (partition_count, replication) = assignment_refs.as_ref().map_or_else(
                || {
                    (
                        args.partitions.unwrap_or(-1),
                        TopicReplication::Fixed(args.replication_factor.unwrap_or(-1)),
                    )
                },
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
            let failures = topic_results(format, "topics.create", result, args.if_not_exists)?;
            if failures == 0 {
                Ok(())
            } else {
                Err(Error::Partial {
                    failed: failures,
                    total: 1,
                })
            }
        }
        TopicAction::Alter(args) => {
            let metadata = base_consumer(config)?.fetch_metadata(None, timeout)?;
            let selector = TopicSelector {
                topic: Some(args.topic.clone()),
                topic_id: None,
                exclude_internal: false,
                under_replicated_partitions: false,
                unavailable_partitions: false,
                under_min_isr_partitions: false,
                at_min_isr_partitions: false,
                topics_with_overrides: false,
            };
            let selected = select_topics(&metadata, &selector)?
                .into_iter()
                .map(|topic| (topic.name().to_owned(), topic.partitions().len()))
                .collect::<Vec<_>>();
            drop(metadata);
            if selected.is_empty() {
                if args.if_exists {
                    return write_mutation_rows(
                        format,
                        "topics.alter",
                        &[MutationRow {
                            resource: args.topic,
                            status: "NO_MATCH".into(),
                            error: None,
                        }],
                    );
                }
                return Err(Error::Usage("no topics matched --topic".into()));
            }
            let assignments = args
                .replica_assignment
                .as_deref()
                .map(parse_replica_assignment)
                .transpose()?;
            let target = usize::try_from(args.partitions)
                .ok()
                .filter(|partitions| *partitions > 0)
                .ok_or_else(|| Error::Usage("--partitions must be greater than zero".into()))?;
            if assignments
                .as_ref()
                .is_some_and(|assignments| assignments.len() != target)
            {
                return Err(Error::Usage(
                    "--replica-assignment must cover every target partition".into(),
                ));
            }
            let new_assignments = selected
                .iter()
                .map(|(topic, existing)| {
                    if *existing >= target {
                        return Err(Error::Usage(format!(
                            "topic {topic} already has {existing} partitions; target must be larger"
                        )));
                    }
                    Ok(assignments.as_ref().map(|assignments| {
                        assignments[*existing..]
                            .iter()
                            .map(Vec::as_slice)
                            .collect::<Vec<&[i32]>>()
                    }))
                })
                .collect::<Result<Vec<_>>>()?;
            let requests = selected
                .iter()
                .zip(&new_assignments)
                .map(|((topic, _), assignments)| {
                    let request = NewPartitions::new(topic, target);
                    match assignments {
                        Some(assignments) => request.assign(assignments),
                        None => request,
                    }
                })
                .collect::<Vec<_>>();
            let result = admin(config)?
                .create_partitions(
                    &requests,
                    &AdminOptions::new().operation_timeout(Some(timeout)),
                )
                .await?;
            let failures = topic_results(format, "topics.alter", result, false)?;
            if failures == 0 {
                Ok(())
            } else {
                Err(Error::Partial {
                    failed: failures,
                    total: selected.len(),
                })
            }
        }
        TopicAction::Delete(args) => {
            let expression = args.topic;
            let consumer = base_consumer(config)?;
            let metadata = consumer.fetch_metadata(None, timeout)?;
            let selector = TopicSelector {
                topic: Some(expression),
                topic_id: None,
                exclude_internal: false,
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
                    return write_mutation_rows(
                        format,
                        "topics.delete",
                        &[MutationRow {
                            resource: "topic-selector".into(),
                            status: "NO_MATCH".into(),
                            error: None,
                        }],
                    );
                }
                return Err(Error::Usage("no topics matched --topic".into()));
            }
            let result = admin(config)?
                .delete_topics(&topics, &AdminOptions::new())
                .await?;
            let failures = topic_results(format, "topics.delete", result, false)?;
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
    let expression = if selector
        .topic_id
        .as_deref()
        .is_some_and(is_nonzero_topic_id)
    {
        None
    } else {
        selector.topic.as_deref()
    };
    let pattern = expression.map(topic_pattern).transpose()?;
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

const ZERO_TOPIC_ID: &str = "AAAAAAAAAAAAAAAAAAAAAA";

fn is_nonzero_topic_id(topic_id: &str) -> bool {
    topic_id != ZERO_TOPIC_ID
}

fn validate_topic_id(topic_id: Option<&str>) -> Result<()> {
    let Some(topic_id) = topic_id else {
        return Ok(());
    };
    if topic_id.len() != 22
        || !topic_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'-' | b'_'))
    {
        return Err(Error::Usage(format!(
            "invalid topic ID '{topic_id}'; expected a 22-character Kafka UUID"
        )));
    }
    Ok(())
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

fn validate_topic_creation_counts(
    partitions: Option<i32>,
    replication_factor: Option<i32>,
) -> Result<()> {
    if partitions.is_some_and(|partitions| partitions < 1) {
        return Err(Error::Usage(
            "--partitions must be greater than zero".into(),
        ));
    }
    if replication_factor
        .is_some_and(|replication_factor| !(1..=i32::from(i16::MAX)).contains(&replication_factor))
    {
        return Err(Error::Usage(format!(
            "--replication-factor must be between 1 and {}",
            i16::MAX
        )));
    }
    Ok(())
}

fn topic_results(
    format: OutputFormat,
    command: &str,
    results: Vec<std::result::Result<String, (String, rdkafka::types::RDKafkaErrorCode)>>,
    ignore_exists: bool,
) -> Result<usize> {
    let rows = results
        .into_iter()
        .map(|result| match result {
            Ok(name) => MutationRow {
                resource: name,
                status: "OK".into(),
                error: None,
            },
            Err((name, code))
                if ignore_exists
                    && code == rdkafka::types::RDKafkaErrorCode::TopicAlreadyExists =>
            {
                MutationRow {
                    resource: name,
                    status: "ALREADY_EXISTS".into(),
                    error: None,
                }
            }
            Err((name, code)) => MutationRow {
                resource: name,
                status: "FAILED".into(),
                error: Some(code.to_string()),
            },
        })
        .collect::<Vec<_>>();
    let failures = rows.iter().filter(|row| row.error.is_some()).count();
    write_mutation_rows(format, command, &rows)?;
    Ok(failures)
}

async fn produce(mut config: rdkafka::ClientConfig, args: crate::cli::ProduceArgs) -> Result<()> {
    apply_client_properties(&mut config, args.properties())?;
    let max_block_ms = configure_producer(&mut config, &args)?;
    let reader = line_reader_options(&args)?;
    let producer: FutureProducer = config.create()?;
    let input = io::read_to_string(io::stdin())?;
    let mut deliveries = Vec::new();
    for (index, line) in input.lines().enumerate() {
        let input = producer_input(line, args.json, &reader).map_err(|error| {
            Error::Usage(format!(
                "invalid producer input on line {}: {error}",
                index + 1
            ))
        })?;
        let mut record = FutureRecord::to(&args.topic);
        if let Some(value) = &input.value {
            record = record.payload(value);
        }
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
        if args.sync {
            producer
                .send(record, Duration::from_millis(max_block_ms))
                .await
                .map_err(|(error, _)| Error::Kafka(error))?;
        } else {
            deliveries.push(
                enqueue_with_timeout(&producer, record, Duration::from_millis(max_block_ms))
                    .await?,
            );
        }
    }
    for delivery in deliveries {
        delivery
            .await
            .map_err(|_| Error::Config("producer delivery channel was canceled".into()))?
            .map_err(|(error, _)| Error::Kafka(error))?;
    }
    Ok(())
}

async fn enqueue_with_timeout<K, P>(
    producer: &FutureProducer,
    mut record: FutureRecord<'_, K, P>,
    timeout: Duration,
) -> Result<DeliveryFuture>
where
    K: ToBytes + Sync + ?Sized,
    P: ToBytes + Sync + ?Sized,
{
    let started = Instant::now();
    loop {
        match producer.send_result(record) {
            Ok(delivery) => return Ok(delivery),
            Err((KafkaError::MessageProduction(RDKafkaErrorCode::QueueFull), returned))
                if started.elapsed() < timeout =>
            {
                record = returned;
                tokio::time::sleep(
                    timeout
                        .saturating_sub(started.elapsed())
                        .min(Duration::from_millis(100)),
                )
                .await;
            }
            Err((error, _)) => return Err(Error::Kafka(error)),
        }
    }
}

fn configure_producer(
    config: &mut rdkafka::ClientConfig,
    args: &crate::cli::ProduceArgs,
) -> Result<u64> {
    let property_max_block = config
        .get("max.block.ms")
        .map(|value| parse_u64("max.block.ms", value))
        .transpose()?;
    config.remove("max.block.ms");
    if let Some(buffer_memory) = config.get("buffer.memory") {
        let bytes = parse_positive_u64("buffer.memory", buffer_memory)?;
        config.set(
            "queue.buffering.max.kbytes",
            bytes.div_ceil(1024).to_string(),
        );
        config.remove("buffer.memory");
    }
    if let Some(send_buffer) = config.get("send.buffer.bytes").map(str::to_owned) {
        config.set("socket.send.buffer.bytes", send_buffer);
        config.remove("send.buffer.bytes");
    }

    config.set("compression.type", &args.compression_type);
    merge_producer_option(config, "acks", args.acks.as_deref(), "-1");
    merge_producer_option(
        config,
        "batch.size",
        args.batch_size.map(|value| value.to_string()).as_deref(),
        "16384",
    );
    if let Some(value) = args.max_partition_memory_bytes {
        config.set("batch.size", value.to_string());
    }
    merge_producer_option(
        config,
        "message.send.max.retries",
        args.message_send_max_retries
            .map(|value| value.to_string())
            .as_deref(),
        "3",
    );
    merge_producer_option(
        config,
        "retry.backoff.ms",
        args.retry_backoff_ms
            .map(|value| value.to_string())
            .as_deref(),
        "100",
    );
    merge_producer_option(
        config,
        "linger.ms",
        args.linger_ms.map(|value| value.to_string()).as_deref(),
        "1000",
    );
    merge_producer_option(
        config,
        "request.timeout.ms",
        args.request_timeout_ms
            .map(|value| value.to_string())
            .as_deref(),
        "1500",
    );
    merge_producer_option(
        config,
        "metadata.max.age.ms",
        args.metadata_expiry_ms
            .map(|value| value.to_string())
            .as_deref(),
        "300000",
    );
    merge_producer_option(
        config,
        "queue.buffering.max.kbytes",
        args.max_memory_bytes
            .map(|value| value.div_ceil(1024).to_string())
            .as_deref(),
        "32768",
    );
    merge_producer_option(
        config,
        "socket.send.buffer.bytes",
        args.socket_buffer_size
            .map(|value| value.to_string())
            .as_deref(),
        "102400",
    );
    if config.get("client.id").is_none() {
        config.set("client.id", "console-producer");
    }
    Ok(args.max_block_ms.or(property_max_block).unwrap_or(60_000))
}

fn merge_producer_option(
    config: &mut rdkafka::ClientConfig,
    key: &str,
    explicit: Option<&str>,
    default: &str,
) {
    if let Some(value) = explicit {
        config.set(key, value);
    } else if config.get(key).is_none() {
        config.set(key, default);
    }
}

fn parse_positive_u64(key: &str, value: &str) -> Result<u64> {
    let value = parse_u64(key, value)?;
    if value == 0 {
        Err(Error::Usage(format!("{key} must be a positive integer")))
    } else {
        Ok(value)
    }
}

fn parse_u64(key: &str, value: &str) -> Result<u64> {
    value
        .parse::<u64>()
        .map_err(|_| Error::Usage(format!("{key} must be a non-negative integer")))
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct ProducerInput {
    #[serde(default)]
    key: Option<String>,
    value: Option<String>,
    #[serde(default)]
    partition: Option<i32>,
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_producer_headers")]
    headers: Vec<(String, Option<String>)>,
}

fn deserialize_producer_headers<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<(String, Option<String>)>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    BTreeMap::<String, Option<String>>::deserialize(deserializer)
        .map(|headers| headers.into_iter().collect())
}

struct LineReaderOptions {
    parse_key: bool,
    key_separator: String,
    parse_headers: bool,
    headers_delimiter: String,
    headers_separator: Regex,
    headers_key_separator: String,
    ignore_error: bool,
    null_marker: Option<String>,
}

fn line_reader_options(args: &crate::cli::ProduceArgs) -> Result<LineReaderOptions> {
    if args.line_reader != "org.apache.kafka.tools.LineMessageReader" {
        return Err(Error::Unsupported(format!(
            "Java line reader class {} cannot be loaded by the native client",
            args.line_reader
        )));
    }
    let properties = component_properties(args.reader_config.as_deref(), args.reader_properties())?;
    let value = |key: &str| properties.get(key);
    let parse_key = value("parse.key").map_or(args.parse_key, |value| bool_property(value));
    let key_separator = value("key.separator")
        .cloned()
        .or_else(|| args.key_separator.map(|separator| separator.to_string()))
        .unwrap_or_else(|| "\t".into());
    let parse_headers = value("parse.headers").is_some_and(|value| bool_property(value));
    let headers_delimiter = value("headers.delimiter")
        .cloned()
        .unwrap_or_else(|| "\t".into());
    let headers_separator_value = value("headers.separator").map_or(",", String::as_str);
    let headers_separator = Regex::new(headers_separator_value).map_err(|error| {
        Error::Usage(format!(
            "invalid headers.separator regular expression: {error}"
        ))
    })?;
    let headers_key_separator = value("headers.key.separator")
        .cloned()
        .unwrap_or_else(|| ":".into());
    let ignore_error = value("ignore.error").is_some_and(|value| bool_property(value));
    let null_marker = value("null.marker").cloned();
    for (left_name, left, right_name, right) in [
        (
            "headers.delimiter",
            headers_delimiter.as_str(),
            "headers.separator",
            headers_separator_value,
        ),
        (
            "headers.delimiter",
            headers_delimiter.as_str(),
            "headers.key.separator",
            headers_key_separator.as_str(),
        ),
        (
            "headers.separator",
            headers_separator_value,
            "headers.key.separator",
            headers_key_separator.as_str(),
        ),
    ] {
        if left == right {
            return Err(Error::Usage(format!(
                "{left_name} and {right_name} may not be equal"
            )));
        }
    }
    if let Some(marker) = &null_marker {
        for (name, separator) in [
            ("key.separator", key_separator.as_str()),
            ("headers.delimiter", headers_delimiter.as_str()),
            ("headers.separator", headers_separator_value),
            ("headers.key.separator", headers_key_separator.as_str()),
        ] {
            if marker == separator {
                return Err(Error::Usage(format!(
                    "null.marker and {name} may not be equal"
                )));
            }
        }
    }
    Ok(LineReaderOptions {
        parse_key,
        key_separator,
        parse_headers,
        headers_delimiter,
        headers_separator,
        headers_key_separator,
        ignore_error,
        null_marker,
    })
}

fn bool_property(value: &str) -> bool {
    value.trim().eq_ignore_ascii_case("true")
}

fn producer_input(line: &str, json: bool, options: &LineReaderOptions) -> Result<ProducerInput> {
    if json {
        let input: ProducerInput = serde_json::from_str(line)?;
        if input.partition.is_some_and(|partition| partition < 0) {
            return Err(Error::Usage("partition must be non-negative".into()));
        }
        Ok(input)
    } else {
        let (headers, remaining) = parse_line_field(
            options.parse_headers,
            line,
            &options.headers_delimiter,
            options.ignore_error,
            "headers delimiter",
        )?;
        let (key, value) = parse_line_field(
            options.parse_key,
            remaining,
            &options.key_separator,
            options.ignore_error,
            "key separator",
        )?;
        let headers = headers
            .map(|headers| parse_line_headers(headers, options))
            .transpose()?
            .unwrap_or_default();
        Ok(ProducerInput {
            key: nullable_field(key, options.null_marker.as_deref()),
            value: nullable_field(Some(value), options.null_marker.as_deref()),
            partition: None,
            headers,
        })
    }
}

fn parse_line_field<'a>(
    enabled: bool,
    line: &'a str,
    separator: &str,
    ignore_error: bool,
    name: &str,
) -> Result<(Option<&'a str>, &'a str)> {
    if !enabled {
        return Ok((None, line));
    }
    line.find(separator).map_or_else(
        || {
            if ignore_error {
                Ok((None, line))
            } else {
                Err(Error::Usage(format!("no {name} found in input line")))
            }
        },
        |index| Ok((Some(&line[..index]), &line[index + separator.len()..])),
    )
}

fn parse_line_headers(
    headers: &str,
    options: &LineReaderOptions,
) -> Result<Vec<(String, Option<String>)>> {
    if options.null_marker.as_deref() == Some(headers) {
        return Ok(Vec::new());
    }
    options
        .headers_separator
        .split(headers)
        .map(|pair| {
            if let Some(index) = pair.find(&options.headers_key_separator) {
                let key = &pair[..index];
                if options.null_marker.as_deref() == Some(key) {
                    return Err(Error::Usage("header key cannot equal null.marker".into()));
                }
                let value = &pair[index + options.headers_key_separator.len()..];
                Ok((
                    key.to_owned(),
                    nullable_field(Some(value), options.null_marker.as_deref()),
                ))
            } else if options.ignore_error {
                Ok((pair.to_owned(), None))
            } else {
                Err(Error::Usage(format!(
                    "no header key separator found in pair '{pair}'"
                )))
            }
        })
        .collect()
}

fn nullable_field(value: Option<&str>, marker: Option<&str>) -> Option<String> {
    value.and_then(|value| (Some(value) != marker).then(|| value.to_owned()))
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
    configure_consumer(&mut config, &args)?;
    let formatter = message_formatter_options(&args)?;
    let manual_offset = args
        .partition
        .map(|_| consumer_offset(args.offset.as_deref(), args.from_beginning))
        .transpose()?;
    let consumer: StreamConsumer = config.create()?;
    if let Some(partition) = args.partition {
        let topic = args
            .topic
            .as_deref()
            .ok_or_else(|| Error::Usage("--topic is required with --partition".into()))?;
        let mut assignment = TopicPartitionList::new();
        assignment.add_partition_offset(topic, partition, manual_offset.unwrap_or(Offset::End))?;
        consumer.assign(&assignment)?;
    } else if let Some(include) = args.include.as_deref() {
        // librdkafka compiles subscription patterns as POSIX ERE. Validating with
        // Rust's regex engine first would reject a different language than the
        // engine that actually performs the subscription.
        let pattern = consumer_include_pattern(include);
        consumer.subscribe(&[&pattern])?;
    } else {
        let topic = args
            .topic
            .as_deref()
            .ok_or_else(|| Error::Usage("consume requires --topic or --include".into()))?;
        consumer.subscribe(&[topic])?;
    }

    let mut stream = consumer.stream();
    let mut received = 0_i64;
    while should_consume_more(args.max_messages, received) {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            message = next_consumer_message(&mut stream, args.timeout_ms) => {
                let Some(message) = message? else { break };
                let message = match message {
                    Ok(message) => message,
                    Err(error) if args.skip_message_on_error => {
                        eprintln!("skipping consumer error: {error}");
                        continue;
                    }
                    Err(error) => return Err(Error::Kafka(error)),
                };
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
                    write_formatted_message(&message, &formatter)?;
                }
                received += 1;
            }
        }
    }
    Ok(())
}

fn consumer_include_pattern(include: &str) -> String {
    format!("^({include})$")
}

fn should_consume_more(max_messages: Option<i32>, received: i64) -> bool {
    match max_messages {
        None | Some(-1) => true,
        Some(max_messages) => received < i64::from(max_messages),
    }
}

static CONSUMER_GROUP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn configure_consumer(
    config: &mut rdkafka::ClientConfig,
    args: &crate::cli::ConsumeArgs,
) -> Result<()> {
    let file_group = config.get("group.id").map(str::to_owned);
    let inline = parse_pairs(args.properties())?;
    let inline_group = inline
        .iter()
        .rev()
        .find(|(key, _)| key == "group.id")
        .map(|(_, value)| value.clone());
    let groups = [
        args.group.as_ref(),
        file_group.as_ref(),
        inline_group.as_ref(),
    ]
    .into_iter()
    .flatten()
    .collect::<BTreeSet<_>>();
    if groups.len() > 1 {
        return Err(Error::Usage(format!(
            "group ids supplied by --group, --command-config, and --command-property must match: {}",
            groups
                .into_iter()
                .map(|group| format!("'{group}'"))
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    if args.partition.is_some() && !groups.is_empty() {
        return Err(Error::Usage(
            "--group/group.id and --partition cannot be specified together".into(),
        ));
    }

    for (key, value) in inline {
        config.set(key, value);
    }
    if config.get("client.id").is_none() {
        config.set("client.id", "console-consumer");
    }
    if let Some(group) = args.group.as_deref().or_else(|| config.get("group.id")) {
        let group = group.to_owned();
        config.set("group.id", group);
    } else {
        config.set("group.id", ephemeral_consumer_group());
        if config.get("enable.auto.commit").is_none() {
            config.set("enable.auto.commit", "false");
        }
    }

    if args.from_beginning {
        if config
            .get("auto.offset.reset")
            .is_some_and(|value| value != "earliest")
        {
            return Err(Error::Usage(
                "--from-beginning conflicts with auto.offset.reset other than earliest".into(),
            ));
        }
        config.set("auto.offset.reset", "earliest");
    } else if config.get("auto.offset.reset").is_none() {
        config.set("auto.offset.reset", "latest");
    }
    if let Some(isolation) = &args.isolation_level {
        config.set("isolation.level", isolation);
    } else if config.get("isolation.level").is_none() {
        config.set("isolation.level", "read_uncommitted");
    }
    Ok(())
}

fn ephemeral_consumer_group() -> String {
    format!(
        "console-consumer-{}-{}",
        process::id(),
        CONSUMER_GROUP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

fn consumer_offset(value: Option<&str>, from_beginning: bool) -> Result<Offset> {
    match value {
        Some(value) if value.eq_ignore_ascii_case("earliest") => Ok(Offset::Beginning),
        Some(value) if value.eq_ignore_ascii_case("latest") => Ok(Offset::End),
        Some(value) => value
            .parse::<i64>()
            .ok()
            .filter(|offset| *offset >= 0)
            .map(Offset::Offset)
            .ok_or_else(|| {
                Error::Usage(format!(
                    "invalid offset '{value}'; expected earliest, latest, or a non-negative integer"
                ))
            }),
        None if from_beginning => Ok(Offset::Beginning),
        None => Ok(Offset::End),
    }
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "mirrors Kafka DefaultMessageFormatter's independent print switches"
)]
struct MessageFormatterOptions {
    print_timestamp: bool,
    print_partition: bool,
    print_offset: bool,
    print_delivery: bool,
    print_epoch: bool,
    print_headers: bool,
    print_key: bool,
    print_value: bool,
    key_separator: Vec<u8>,
    line_separator: Vec<u8>,
    headers_separator: Vec<u8>,
    null_literal: Vec<u8>,
    key_deserializer: NativeDeserializer,
    value_deserializer: NativeDeserializer,
    headers_deserializer: NativeDeserializer,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum NativeDeserializer {
    #[default]
    Raw,
    Utf8String,
}

fn native_deserializer(class: Option<&str>, field: &str) -> Result<NativeDeserializer> {
    match class {
        None | Some("") => Ok(NativeDeserializer::Raw),
        Some("org.apache.kafka.common.serialization.StringDeserializer") => {
            Ok(NativeDeserializer::Utf8String)
        }
        Some(class) => Err(Error::Unsupported(format!(
            "Java {field} deserializer class {class} cannot be loaded by the native client"
        ))),
    }
}

fn deserialize_for_display(
    bytes: Option<&[u8]>,
    deserializer: NativeDeserializer,
    null_literal: &[u8],
) -> Vec<u8> {
    let bytes = bytes.unwrap_or(null_literal);
    match deserializer {
        NativeDeserializer::Raw => bytes.to_vec(),
        NativeDeserializer::Utf8String => String::from_utf8_lossy(bytes).into_owned().into_bytes(),
    }
}

fn is_utf8_encoding(value: &str) -> bool {
    value
        .chars()
        .filter(|character| !matches!(character, '-' | '_'))
        .collect::<String>()
        .eq_ignore_ascii_case("UTF8")
}

fn message_formatter_options(args: &crate::cli::ConsumeArgs) -> Result<MessageFormatterOptions> {
    if args.formatter != "org.apache.kafka.tools.consumer.DefaultMessageFormatter" {
        return Err(Error::Unsupported(format!(
            "Java formatter class {} cannot be loaded by the native client",
            args.formatter
        )));
    }
    let properties = component_properties(
        args.formatter_config.as_deref(),
        args.formatter_properties(),
    )?;
    let value = |key: &str| properties.get(key);
    let key_deserializer = native_deserializer(
        value("key.deserializer")
            .map(String::as_str)
            .or(args.key_deserializer.as_deref()),
        "key",
    )?;
    let value_deserializer = native_deserializer(
        value("value.deserializer")
            .map(String::as_str)
            .or(args.value_deserializer.as_deref()),
        "value",
    )?;
    let headers_deserializer =
        native_deserializer(value("headers.deserializer").map(String::as_str), "headers")?;
    for (key, applies) in [
        (
            "deserializer.encoding",
            key_deserializer == NativeDeserializer::Utf8String
                || value_deserializer == NativeDeserializer::Utf8String,
        ),
        (
            "key.deserializer.encoding",
            key_deserializer == NativeDeserializer::Utf8String,
        ),
        (
            "value.deserializer.encoding",
            value_deserializer == NativeDeserializer::Utf8String,
        ),
    ] {
        if applies && let Some(encoding) = value(key).filter(|encoding| !is_utf8_encoding(encoding))
        {
            return Err(Error::Unsupported(format!(
                "StringDeserializer encoding {encoding} is not supported by the native client"
            )));
        }
    }
    let flag = |key: &str, default: bool| value(key).map_or(default, |value| bool_property(value));
    Ok(MessageFormatterOptions {
        print_timestamp: flag("print.timestamp", false),
        print_partition: flag("print.partition", false),
        print_offset: flag("print.offset", false),
        print_delivery: flag("print.delivery", false),
        print_epoch: flag("print.epoch", false),
        print_headers: flag("print.headers", false),
        print_key: flag("print.key", args.print_key),
        print_value: flag("print.value", true),
        key_separator: value("key.separator")
            .map_or_else(|| args.key_separator.as_bytes(), String::as_bytes)
            .to_vec(),
        line_separator: value("line.separator")
            .map_or(b"\n".as_slice(), String::as_bytes)
            .to_vec(),
        headers_separator: value("headers.separator")
            .map_or(b",".as_slice(), String::as_bytes)
            .to_vec(),
        null_literal: value("null.literal")
            .map_or(b"null".as_slice(), String::as_bytes)
            .to_vec(),
        key_deserializer,
        value_deserializer,
        headers_deserializer,
    })
}

fn write_formatted_message(
    message: &rdkafka::message::BorrowedMessage<'_>,
    options: &MessageFormatterOptions,
) -> Result<()> {
    let mut fields = Vec::<Vec<u8>>::new();
    if options.print_timestamp {
        fields.push(match message.timestamp() {
            rdkafka::Timestamp::NotAvailable => b"NO_TIMESTAMP".to_vec(),
            rdkafka::Timestamp::CreateTime(timestamp) => {
                format!("CreateTime:{timestamp}").into_bytes()
            }
            rdkafka::Timestamp::LogAppendTime(timestamp) => {
                format!("LogAppendTime:{timestamp}").into_bytes()
            }
        });
    }
    if options.print_partition {
        fields.push(format!("Partition:{}", message.partition()).into_bytes());
    }
    if options.print_offset {
        fields.push(format!("Offset:{}", message.offset()).into_bytes());
    }
    if options.print_delivery {
        fields.push(b"Delivery:NOT_PRESENT".to_vec());
    }
    if options.print_epoch {
        fields.push(b"Epoch:NOT_PRESENT".to_vec());
    }
    if options.print_headers {
        fields.push(formatted_headers(message.headers(), options));
    }
    if options.print_key {
        fields.push(deserialize_for_display(
            message.key(),
            options.key_deserializer,
            &options.null_literal,
        ));
    }
    if options.print_value {
        fields.push(deserialize_for_display(
            message.payload(),
            options.value_deserializer,
            &options.null_literal,
        ));
    }
    let mut stdout = io::stdout().lock();
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            stdout.write_all(&options.key_separator)?;
        }
        stdout.write_all(field)?;
    }
    if options.print_value {
        stdout.write_all(&options.line_separator)?;
    }
    Ok(())
}

fn formatted_headers(
    headers: Option<&rdkafka::message::BorrowedHeaders>,
    options: &MessageFormatterOptions,
) -> Vec<u8> {
    let Some(headers) = headers.filter(|headers| headers.count() > 0) else {
        return b"NO_HEADERS".to_vec();
    };
    let mut result = Vec::new();
    for (index, header) in headers.iter().enumerate() {
        if index > 0 {
            result.extend_from_slice(&options.headers_separator);
        }
        result.extend_from_slice(header.key.as_bytes());
        result.push(b':');
        result.extend_from_slice(&deserialize_for_display(
            header.value,
            options.headers_deserializer,
            &options.null_literal,
        ));
    }
    result
}

async fn next_consumer_message<'a>(
    stream: &mut (
             impl futures::Stream<
        Item = std::result::Result<
            rdkafka::message::BorrowedMessage<'a>,
            rdkafka::error::KafkaError,
        >,
    > + Unpin
         ),
    timeout_ms: Option<u64>,
) -> Result<
    Option<std::result::Result<rdkafka::message::BorrowedMessage<'a>, rdkafka::error::KafkaError>>,
> {
    if let Some(timeout_ms) = timeout_ms.filter(|value| *value != u64::MAX) {
        tokio::time::timeout(Duration::from_millis(timeout_ms), stream.next())
            .await
            .map_or_else(|_| Ok(None), Ok)
    } else {
        Ok(stream.next().await)
    }
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

async fn groups(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    format: OutputFormat,
    action: GroupAction,
    verbose: bool,
) -> Result<()> {
    match action {
        GroupAction::ValidateRegex { .. } => Err(Error::Usage(
            "validate-regex must be handled before client configuration".into(),
        )),
        GroupAction::List { state, group_type } => list_groups(
            config,
            timeout,
            format,
            state.as_deref(),
            group_type.as_deref(),
        ),
        GroupAction::Describe {
            group,
            all_groups,
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
            let groups = resolve_group_names(config, timeout, &group, all_groups)?;
            describe_group_details(config, timeout, format, &groups, mode, verbose)
        }
        GroupAction::Delete {
            group,
            all_groups,
            execute,
        } => {
            let groups = resolve_group_names(config, timeout, &group, all_groups)?;
            if groups.is_empty() {
                return Err(Error::Usage("no consumer groups matched".into()));
            }
            if !execute {
                let rows = groups
                    .into_iter()
                    .map(|group| GroupDeleteRow {
                        group,
                        status: "PREVIEW".into(),
                        error: None,
                    })
                    .collect::<Vec<_>>();
                return write_group_delete_rows(format, &rows);
            }
            let names = groups.iter().map(String::as_str).collect::<Vec<_>>();
            let results = admin(config)?
                .delete_groups(&names, &AdminOptions::new())
                .await?;
            let failures = results.iter().filter(|result| result.is_err()).count();
            let rows = results
                .into_iter()
                .map(|result| match result {
                    Ok(group) => GroupDeleteRow {
                        group,
                        status: "DELETED".into(),
                        error: None,
                    },
                    Err((group, error)) => GroupDeleteRow {
                        group,
                        status: "FAILED".into(),
                        error: Some(error.to_string()),
                    },
                })
                .collect::<Vec<_>>();
            write_group_delete_rows(format, &rows)?;
            if failures == 0 {
                Ok(())
            } else {
                Err(Error::Partial {
                    failed: failures,
                    total: names.len(),
                })
            }
        }
        GroupAction::ResetOffsets(args) => reset_offsets(config, timeout, format, &args),
        GroupAction::DeleteOffsets {
            group,
            topic,
            execute,
        } => delete_group_offsets_command(config, timeout, format, &group, &topic, execute),
    }
}

#[derive(Debug, Serialize)]
struct GroupDeleteRow {
    group: String,
    status: String,
    error: Option<String>,
}

fn write_group_delete_rows(format: OutputFormat, rows: &[GroupDeleteRow]) -> Result<()> {
    output::write_value(format, "groups.delete", &rows, |rows| {
        output::table(
            ["GROUP", "STATUS", "ERROR"],
            rows.iter().map(|row| {
                [
                    row.group.clone(),
                    row.status.clone(),
                    row.error.as_deref().unwrap_or("-").to_owned(),
                ]
            }),
        )
    })
}

#[derive(Debug, Serialize)]
struct DeleteOffsetRow {
    group: String,
    topic: String,
    partition: i32,
}

fn delete_group_offsets_command(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    format: OutputFormat,
    group: &str,
    topics: &[String],
    execute: bool,
) -> Result<()> {
    let selections = resolve_topic_partition_selections(config, timeout, topics)?;
    if !execute {
        let rows = selections
            .iter()
            .flat_map(|(topic, partitions)| {
                partitions.iter().map(|partition| DeleteOffsetRow {
                    group: group.to_owned(),
                    topic: topic.clone(),
                    partition: *partition,
                })
            })
            .collect::<Vec<_>>();
        return output::write_value(format, "groups.delete-offsets.preview", &rows, |rows| {
            output::table(
                ["GROUP", "TOPIC", "PARTITION"],
                rows.iter().map(|row| {
                    [
                        row.group.clone(),
                        row.topic.clone(),
                        row.partition.to_string(),
                    ]
                }),
            )
        });
    }
    let admin = admin(config)?;
    crate::ffi::delete_group_offsets(
        admin.inner().native_ptr(),
        group,
        &selections,
        duration_ms(timeout)?,
    )
}

fn resolve_topic_partition_selections(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    values: &[String],
) -> Result<Vec<(String, Vec<i32>)>> {
    let selections = parse_reset_topics(values)?;
    let consumer = base_consumer(config)?;
    selections
        .into_iter()
        .map(|(topic, selected)| {
            let metadata = consumer.fetch_metadata(Some(&topic), timeout)?;
            let described = metadata
                .topics()
                .iter()
                .find(|candidate| candidate.name() == topic && candidate.error().is_none())
                .ok_or_else(|| Error::Usage(format!("topic {topic} not found")))?;
            let existing = described
                .partitions()
                .iter()
                .map(rdkafka::metadata::MetadataPartition::id)
                .collect::<BTreeSet<_>>();
            let partitions = selected.unwrap_or_else(|| existing.clone());
            if let Some(missing) = partitions.difference(&existing).next() {
                return Err(Error::Usage(format!(
                    "partition {topic}:{missing} does not exist"
                )));
            }
            Ok((topic, partitions.into_iter().collect()))
        })
        .collect()
}

#[derive(Debug, Serialize)]
struct RegexValidationRow<'a> {
    regex: &'a str,
    valid: bool,
    error: Option<String>,
}

fn validate_group_regex(format: OutputFormat, regex: &str) -> Result<()> {
    let validation = Regex::new(regex);
    let row = RegexValidationRow {
        regex,
        valid: validation.is_ok(),
        error: validation.err().map(|error| error.to_string()),
    };
    output::write_value(format, "groups.validate-regex", &row, |row| {
        output::table(
            ["REGEX", "VALID", "ERROR"],
            [[
                row.regex.to_owned(),
                row.valid.to_string(),
                row.error.as_deref().unwrap_or("-").to_owned(),
            ]],
        )
    })
}

fn list_groups(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    format: OutputFormat,
    states: Option<&str>,
    types: Option<&str>,
) -> Result<()> {
    let states = states
        .map(parse_group_states)
        .transpose()?
        .unwrap_or_default();
    let types = types
        .map(parse_group_types)
        .transpose()?
        .unwrap_or_default();
    let client = admin(config)?;
    let rows = ffi::list_consumer_groups(
        client.inner().native_ptr(),
        &states,
        &types,
        duration_ms(timeout)?,
    )?;
    output::write_value(format, "groups.list", &rows, |rows| {
        output::table(
            ["GROUP", "TYPE", "STATE", "SIMPLE"],
            rows.iter().map(|row| {
                [
                    row.group.clone(),
                    row.group_type.clone(),
                    row.state.clone(),
                    row.is_simple.to_string(),
                ]
            }),
        )
    })
}

fn resolve_group_names(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    explicit: &[String],
    all_groups: bool,
) -> Result<Vec<String>> {
    if !all_groups {
        return Ok(explicit.to_vec());
    }
    let client = admin(config)?;
    Ok(
        ffi::list_consumer_groups(client.inner().native_ptr(), &[], &[], duration_ms(timeout)?)?
            .into_iter()
            .map(|group| group.group)
            .collect(),
    )
}

fn parse_group_states(value: &str) -> Result<Vec<ffi::ConsumerGroupState>> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    value
        .split(',')
        .map(|state| match normalized_group_filter(state).as_str() {
            "preparingrebalance" => Ok(ffi::ConsumerGroupState::PreparingRebalance),
            "completingrebalance" => Ok(ffi::ConsumerGroupState::CompletingRebalance),
            "stable" => Ok(ffi::ConsumerGroupState::Stable),
            "dead" => Ok(ffi::ConsumerGroupState::Dead),
            "empty" => Ok(ffi::ConsumerGroupState::Empty),
            _ => Err(Error::Usage(format!(
                "unknown consumer-group state: {state}"
            ))),
        })
        .collect()
}

fn parse_group_types(value: &str) -> Result<Vec<ffi::ConsumerGroupType>> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    value
        .split(',')
        .map(
            |group_type| match normalized_group_filter(group_type).as_str() {
                "consumer" => Ok(ffi::ConsumerGroupType::Consumer),
                "classic" => Ok(ffi::ConsumerGroupType::Classic),
                _ => Err(Error::Usage(format!(
                    "unknown consumer-group type: {group_type}"
                ))),
            },
        )
        .collect()
}

fn normalized_group_filter(value: &str) -> String {
    value
        .chars()
        .filter(|character| !matches!(character, '-' | '_' | ' '))
        .flat_map(char::to_lowercase)
        .collect()
}

#[derive(Serialize)]
struct GroupOffsetRow {
    group: String,
    topic: String,
    partition: i32,
    leader_epoch: Option<i32>,
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
    partitions: usize,
    assignment: String,
    target_assignment: String,
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
    groups: &[String],
    mode: GroupDescribeMode,
    verbose: bool,
) -> Result<()> {
    if groups.is_empty() {
        return Err(Error::Usage("no consumer groups matched".into()));
    }
    match mode {
        GroupDescribeMode::State => {
            return describe_groups(config, timeout, format, groups);
        }
        GroupDescribeMode::Members => {
            return describe_group_members(config, timeout, format, groups, verbose);
        }
        GroupDescribeMode::Offsets => {}
    }
    let admin = admin(config)?;
    let offsets = groups
        .iter()
        .map(|group| {
            crate::ffi::list_consumer_group_offsets(
                admin.inner().native_ptr(),
                group,
                duration_ms(timeout)?,
            )
            .map(|offsets| (group, offsets))
        })
        .collect::<Result<Vec<_>>>()?;
    drop(admin);
    let consumer = base_consumer(config)?;
    let rows = offsets
        .into_iter()
        .flat_map(|(group, offsets)| {
            offsets
                .into_iter()
                .filter(|offset| offset.offset >= 0)
                .map(|offset| {
                    let log_end_offset = consumer
                        .fetch_watermarks(&offset.topic, offset.partition, timeout)
                        .map(|(_, high)| high)
                        .ok();
                    let lag = group_offset_lag(offset.offset, log_end_offset);
                    GroupOffsetRow {
                        group: group.clone(),
                        topic: offset.topic,
                        partition: offset.partition,
                        leader_epoch: offset.leader_epoch,
                        committed_offset: offset.offset,
                        log_end_offset,
                        lag,
                        error: offset.error,
                    }
                })
        })
        .collect::<Vec<_>>();
    output::write_value(format, "groups.describe.offsets", &rows, |rows| {
        group_offsets_table(rows, verbose)
    })
}

fn group_offsets_table(rows: &[GroupOffsetRow], verbose: bool) -> String {
    if verbose {
        output::table(
            [
                "GROUP",
                "TOPIC",
                "PARTITION",
                "LEADER_EPOCH",
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
                    row.leader_epoch
                        .map_or_else(|| "-".into(), |value| value.to_string()),
                    row.committed_offset.to_string(),
                    row.log_end_offset
                        .map_or_else(|| "-".into(), |value| value.to_string()),
                    row.lag
                        .map_or_else(|| "-".into(), |value| value.to_string()),
                    row.error.as_deref().unwrap_or("-").to_owned(),
                ]
            }),
        )
    } else {
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
    }
}

fn describe_group_members(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    format: OutputFormat,
    groups: &[String],
    verbose: bool,
) -> Result<()> {
    let client = admin(config)?;
    let groups =
        ffi::describe_consumer_groups(client.inner().native_ptr(), groups, duration_ms(timeout)?)?;
    let rows = groups
        .iter()
        .flat_map(|description| {
            description
                .members
                .iter()
                .map(move |member| GroupMemberRow {
                    group: description.group.clone(),
                    member_id: member.member_id.clone(),
                    instance_id: member.instance_id.clone(),
                    client_id: member.client_id.clone(),
                    host: member.host.clone(),
                    partitions: member.assignment.len(),
                    assignment: group_partitions(&member.assignment),
                    target_assignment: group_partitions(&member.target_assignment),
                })
        })
        .collect::<Vec<_>>();
    output::write_value(format, "groups.describe.members", &rows, |rows| {
        group_members_table(rows, verbose)
    })
}

fn group_members_table(rows: &[GroupMemberRow], verbose: bool) -> String {
    if verbose {
        output::table(
            [
                "GROUP",
                "MEMBER_ID",
                "INSTANCE_ID",
                "CLIENT_ID",
                "HOST",
                "PARTITIONS",
                "ASSIGNMENT",
                "TARGET_ASSIGNMENT",
            ],
            rows.iter().map(|row| {
                [
                    row.group.clone(),
                    row.member_id.clone(),
                    row.instance_id.as_deref().unwrap_or("-").to_owned(),
                    row.client_id.clone(),
                    row.host.clone(),
                    row.partitions.to_string(),
                    row.assignment.clone(),
                    row.target_assignment.clone(),
                ]
            }),
        )
    } else {
        output::table(
            [
                "GROUP",
                "MEMBER_ID",
                "INSTANCE_ID",
                "CLIENT_ID",
                "HOST",
                "PARTITIONS",
            ],
            rows.iter().map(|row| {
                [
                    row.group.clone(),
                    row.member_id.clone(),
                    row.instance_id.as_deref().unwrap_or("-").to_owned(),
                    row.client_id.clone(),
                    row.host.clone(),
                    row.partitions.to_string(),
                ]
            }),
        )
    }
}

fn group_partitions(partitions: &[ffi::ConsumerGroupPartition]) -> String {
    partitions
        .iter()
        .map(|partition| format!("{}:{}", partition.topic, partition.partition))
        .collect::<Vec<_>>()
        .join(",")
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
    groups: &[String],
) -> Result<()> {
    let client = admin(config)?;
    let rows =
        ffi::describe_consumer_groups(client.inner().native_ptr(), groups, duration_ms(timeout)?)?;
    output::write_value(format, "groups.describe.state", &rows, |rows| {
        output::table(
            [
                "GROUP",
                "TYPE",
                "STATE",
                "ASSIGNOR",
                "MEMBERS",
                "COORDINATOR_ID",
                "COORDINATOR",
            ],
            rows.iter().map(|row| {
                [
                    row.group.clone(),
                    row.group_type.clone(),
                    row.state.clone(),
                    row.assignor.clone(),
                    row.members.len().to_string(),
                    row.coordinator_id.to_string(),
                    row.coordinator.clone(),
                ]
            }),
        )
    })
}

#[expect(
    clippy::too_many_lines,
    reason = "reset planning keeps the mutually exclusive Kafka reset strategies together"
)]
fn reset_offsets(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    format: OutputFormat,
    args: &ResetOffsetsArgs,
) -> Result<()> {
    validate_reset_target(args)?;
    let groups = resolve_group_names(config, timeout, &args.group, args.all_groups)?;
    if groups.is_empty() {
        return Err(Error::Usage("no consumer groups matched".into()));
    }
    let (groups, group_errors) = resettable_groups(config, timeout, &groups)?;
    if let Some(path) = args.from_file.as_deref() {
        let rows = if groups.is_empty() {
            Vec::new()
        } else {
            read_reset_plan(path, &groups, args.group.len() == 1, config, timeout)?
        };
        if args.execute {
            execute_reset_rows(config, timeout, &rows)?;
        }
        return write_reset_rows(
            format,
            &rows,
            args.export,
            args.group.len() == 1,
            &group_errors,
        );
    }
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
    let mut rows = Vec::new();
    for group in groups {
        let mut consumer_config = config.clone();
        consumer_config.set("group.id", &group);
        let consumer: BaseConsumer = consumer_config.create()?;
        let topics = if args.all_topics {
            let admin = admin(config)?;
            let topics = ffi::list_consumer_group_offsets(
                admin.inner().native_ptr(),
                &group,
                duration_ms(timeout)?,
            )?
            .into_iter()
            .map(|offset| offset.topic)
            .collect::<BTreeSet<_>>();
            if topics.is_empty() {
                continue;
            }
            topics
                .into_iter()
                .map(|topic| (topic, None))
                .collect::<BTreeMap<_, _>>()
        } else {
            parse_reset_topics(&args.topic)?
        };
        let mut planned_offsets = Vec::new();
        for (topic_name, selected) in topics {
            let metadata = consumer.fetch_metadata(Some(&topic_name), timeout)?;
            let topic = metadata
                .topics()
                .iter()
                .find(|topic| topic.name() == topic_name)
                .ok_or_else(|| Error::Usage(format!("topic {topic_name} not found")))?;
            let partitions = topic
                .partitions()
                .iter()
                .filter(|partition| {
                    selected
                        .as_ref()
                        .is_none_or(|selected| selected.contains(&partition.id()))
                })
                .collect::<Vec<_>>();
            if let Some(selected) = &selected {
                let existing = partitions
                    .iter()
                    .map(|partition| partition.id())
                    .collect::<BTreeSet<_>>();
                if let Some(missing) = selected.difference(&existing).next() {
                    return Err(Error::Usage(format!(
                        "partition {topic_name}:{missing} does not exist"
                    )));
                }
            }
            let mut requested = TopicPartitionList::new();
            for partition in &partitions {
                requested.add_partition(&topic_name, partition.id());
            }
            let committed = if args.shift_by.is_some() || args.to_current {
                Some(consumer.committed_offsets(requested.clone(), timeout)?)
            } else {
                None
            };
            let timestamp_offsets = if let Some(timestamp) = timestamp {
                let mut timestamp_request = TopicPartitionList::new();
                for partition in &partitions {
                    timestamp_request.add_partition_offset(
                        &topic_name,
                        partition.id(),
                        Offset::Offset(timestamp),
                    )?;
                }
                Some(consumer.offsets_for_times(timestamp_request, timeout)?)
            } else {
                None
            };
            for partition in partitions {
                let (low, high) =
                    consumer.fetch_watermarks(&topic_name, partition.id(), timeout)?;
                let target = reset_target(
                    args,
                    committed.as_ref(),
                    timestamp_offsets.as_ref(),
                    &topic_name,
                    partition.id(),
                    low,
                    high,
                )?;
                rows.push(ResetOffsetRow {
                    group: group.clone(),
                    topic: topic_name.clone(),
                    partition: partition.id(),
                    new_offset: target,
                });
                planned_offsets.push((topic_name.clone(), partition.id(), target));
            }
        }
        if args.execute {
            let admin = admin(config)?;
            ffi::alter_consumer_group_offsets(
                admin.inner().native_ptr(),
                &group,
                &planned_offsets,
                duration_ms(timeout)?,
            )?;
        }
    }
    write_reset_rows(
        format,
        &rows,
        args.export,
        args.group.len() == 1,
        &group_errors,
    )
}

fn validate_reset_target(args: &ResetOffsetsArgs) -> Result<()> {
    if args.to_earliest
        || args.to_latest
        || args.to_offset.is_some()
        || args.shift_by.is_some()
        || args.to_current
        || args.to_datetime.is_some()
        || args.by_duration.is_some()
        || args.from_file.is_some()
    {
        Ok(())
    } else {
        Err(Error::Usage("choose one reset target".into()))
    }
}

fn resettable_groups(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    selected: &[String],
) -> Result<(Vec<String>, Vec<String>)> {
    let client = admin(config)?;
    let states =
        ffi::list_consumer_groups(client.inner().native_ptr(), &[], &[], duration_ms(timeout)?)?
            .into_iter()
            .map(|listing| (listing.group, listing.state))
            .collect::<BTreeMap<_, _>>();
    Ok(classify_resettable_groups(selected, &states))
}

fn classify_resettable_groups(
    selected: &[String],
    states: &BTreeMap<String, String>,
) -> (Vec<String>, Vec<String>) {
    let mut groups = Vec::new();
    let mut errors = Vec::new();
    for group in selected {
        match states.get(group).map(String::as_str) {
            None | Some("Empty" | "Dead") => groups.push(group.clone()),
            Some(state) => errors.push(format!(
                "assignments can only be reset if group '{group}' is inactive; current state is {state}"
            )),
        }
    }
    (groups, errors)
}

fn parse_reset_topics(values: &[String]) -> Result<BTreeMap<String, Option<BTreeSet<i32>>>> {
    let mut topics: BTreeMap<String, Option<BTreeSet<i32>>> = BTreeMap::new();
    for value in values {
        let (topic, partitions) = if let Some((topic, partitions)) = value.split_once(':') {
            if topic.is_empty() || partitions.is_empty() || partitions.contains(':') {
                return Err(Error::Usage(format!(
                    "invalid reset topic {value}; expected topic:partition,partition"
                )));
            }
            let partitions = partitions
                .split(',')
                .map(|partition| {
                    partition.parse::<i32>().map_err(|_| {
                        Error::Usage(format!("invalid partition {partition} in {value}"))
                    })
                })
                .collect::<Result<BTreeSet<_>>>()?;
            if partitions.iter().any(|partition| *partition < 0) {
                return Err(Error::Usage(format!(
                    "partitions in {value} must be non-negative"
                )));
            }
            (topic.to_owned(), Some(partitions))
        } else {
            if value.is_empty() {
                return Err(Error::Usage("reset topic must not be empty".into()));
            }
            (value.clone(), None)
        };
        topics
            .entry(topic)
            .and_modify(|current| match (&mut *current, &partitions) {
                (Some(current), Some(partitions)) => current.extend(partitions),
                (current, None) => *current = None,
                (None, Some(_)) => {}
            })
            .or_insert(partitions);
    }
    Ok(topics)
}

fn write_reset_rows(
    format: OutputFormat,
    rows: &[ResetOffsetRow],
    export: bool,
    single_group: bool,
    errors: &[String],
) -> Result<()> {
    if export {
        let mut writer = csv::WriterBuilder::new()
            .has_headers(false)
            .from_writer(Vec::new());
        for row in rows {
            let result = if single_group {
                writer.serialize((&row.topic, row.partition, row.new_offset))
            } else {
                writer.serialize((&row.group, &row.topic, row.partition, row.new_offset))
            };
            result.map_err(|error| Error::Usage(format!("cannot export reset CSV: {error}")))?;
        }
        let bytes = writer
            .into_inner()
            .map_err(|error| Error::Usage(format!("cannot finish reset CSV: {error}")))?;
        let csv = String::from_utf8(bytes)
            .map_err(|error| Error::Usage(format!("reset CSV is not UTF-8: {error}")))?;
        print!("{csv}");
        for error in errors {
            eprintln!("Error: {error}");
        }
        return Ok(());
    }
    output::write_value_with_errors(format, "groups.reset-offsets", &rows, errors, |rows| {
        output::table(
            ["GROUP", "TOPIC", "PARTITION", "NEW_OFFSET"],
            rows.iter().map(|row| {
                [
                    row.group.clone(),
                    row.topic.clone(),
                    row.partition.to_string(),
                    row.new_offset.to_string(),
                ]
            }),
        )
    })
}

#[derive(Debug, Serialize)]
struct ResetOffsetRow {
    group: String,
    topic: String,
    partition: i32,
    new_offset: i64,
}

fn read_reset_plan(
    path: &Path,
    groups: &[String],
    single_group: bool,
    config: &rdkafka::ClientConfig,
    timeout: Duration,
) -> Result<Vec<ResetOffsetRow>> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .from_path(path)
        .map_err(|error| Error::Usage(format!("cannot read reset CSV: {error}")))?;
    let selected = groups.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let consumer = base_consumer(config)?;
    let mut seen = BTreeSet::new();
    let mut rows = Vec::new();
    for (index, record) in reader.records().enumerate() {
        let record = record.map_err(|error| {
            Error::Usage(format!("invalid reset CSV line {}: {error}", index + 1))
        })?;
        let (group, topic, partition, requested) = if single_group && record.len() == 3 {
            (
                groups[0].clone(),
                record[0].to_owned(),
                parse_csv_number::<i32>(&record[1], index, "partition")?,
                parse_csv_number::<i64>(&record[2], index, "offset")?,
            )
        } else if record.len() == 4 {
            (
                record[0].to_owned(),
                record[1].to_owned(),
                parse_csv_number::<i32>(&record[2], index, "partition")?,
                parse_csv_number::<i64>(&record[3], index, "offset")?,
            )
        } else {
            return Err(Error::Usage(format!(
                "reset CSV line {} must contain {} columns",
                index + 1,
                if single_group { "3 or 4" } else { "4" }
            )));
        };
        if !selected.contains(group.as_str()) {
            return Err(Error::Usage(format!(
                "reset CSV group {group} was not selected"
            )));
        }
        if topic.is_empty() || partition < 0 || requested < 0 {
            return Err(Error::Usage(format!(
                "reset CSV line {} requires non-empty topic and non-negative partition/offset",
                index + 1
            )));
        }
        if !seen.insert((group.clone(), topic.clone(), partition)) {
            return Err(Error::Usage(format!(
                "duplicate reset CSV target {group}:{topic}:{partition}"
            )));
        }
        let (low, high) = consumer.fetch_watermarks(&topic, partition, timeout)?;
        rows.push(ResetOffsetRow {
            group,
            topic,
            partition,
            new_offset: requested.clamp(low, high),
        });
    }
    if rows.is_empty() {
        return Err(Error::Usage("reset CSV is empty".into()));
    }
    Ok(rows)
}

fn parse_csv_number<T>(value: &str, index: usize, name: &str) -> Result<T>
where
    T: std::str::FromStr,
{
    value
        .parse()
        .map_err(|_| Error::Usage(format!("invalid {name} on reset CSV line {}", index + 1)))
}

fn execute_reset_rows(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    rows: &[ResetOffsetRow],
) -> Result<()> {
    let admin = admin(config)?;
    for (group, rows) in &rows.iter().fold(
        BTreeMap::<&str, Vec<(String, i32, i64)>>::new(),
        |mut groups, row| {
            groups.entry(&row.group).or_default().push((
                row.topic.clone(),
                row.partition,
                row.new_offset,
            ));
            groups
        },
    ) {
        ffi::alter_consumer_group_offsets(
            admin.inner().native_ptr(),
            group,
            rows,
            duration_ms(timeout)?,
        )?;
    }
    Ok(())
}

fn reset_target(
    args: &ResetOffsetsArgs,
    committed: Option<&TopicPartitionList>,
    timestamp_offsets: Option<&TopicPartitionList>,
    topic: &str,
    partition: i32,
    low: i64,
    high: i64,
) -> Result<i64> {
    if args.to_earliest {
        Ok(low)
    } else if args.to_latest {
        Ok(high)
    } else if let Some(value) = args.to_offset {
        Ok(value.clamp(low, high))
    } else if let Some(shift) = args.shift_by {
        let current = committed_offset(committed, topic, partition);
        shifted_offset(current, shift, low, high, partition)
    } else if args.to_current {
        committed_offset(committed, topic, partition).ok_or_else(|| {
            Error::Usage(format!(
                "partition {topic}:{partition} has no committed offset"
            ))
        })
    } else if timestamp_offsets.is_some() {
        Ok(committed_offset(timestamp_offsets, topic, partition).unwrap_or(high))
    } else {
        Err(Error::Usage("choose one reset target".into()))
    }
}

fn committed_offset(
    offsets: Option<&TopicPartitionList>,
    topic: &str,
    partition: i32,
) -> Option<i64> {
    offsets
        .and_then(|offsets| offsets.find_partition(topic, partition))
        .and_then(|partition| partition.offset().to_raw())
        .filter(|offset| *offset >= 0)
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

#[expect(
    clippy::too_many_lines,
    reason = "branches mirror Kafka's resource, quota, and SCRAM config backends"
)]
async fn configs(
    config: &rdkafka::ClientConfig,
    bootstrap: &str,
    command_config: Option<&Path>,
    timeout: Duration,
    format: OutputFormat,
    action: ConfigAction,
) -> Result<()> {
    match action {
        ConfigAction::Describe {
            entity_type,
            entity_name,
            entity_default,
            all,
        } => {
            validate_config_entity_types(&entity_type)?;
            validate_config_entity_names(&entity_type, &entity_name)?;
            if quota_entity_types(&entity_type) {
                return describe_quota_configs(
                    config,
                    bootstrap,
                    command_config,
                    timeout,
                    format,
                    &entity_type,
                    &entity_name,
                    entity_default,
                )
                .await;
            }
            let (entity_type, entity_name) =
                single_resource_entity(&entity_type, &entity_name, entity_default, false)?;
            if protocol_config_resource_type(entity_type).is_some()
                && (!matches!(entity_type, ConfigEntityType::Broker)
                    || entity_name.as_deref() == Some(""))
            {
                return describe_protocol_configs(
                    bootstrap,
                    command_config,
                    timeout,
                    format,
                    entity_type,
                    entity_name,
                    all,
                )
                .await;
            }
            describe_resource_configs(config, timeout, format, entity_type, entity_name, all).await
        }
        ConfigAction::Alter {
            entity_type,
            entity_name,
            entity_default,
            add,
            add_file,
            delete,
            execute,
        } => {
            let delete = normalize_config_deletions(delete);
            let pairs = if let Some(path) = add_file.as_deref() {
                let mut pairs = config::load_properties(path)?
                    .into_iter()
                    .collect::<Vec<_>>();
                pairs.sort_by(|left, right| left.0.cmp(&right.0));
                pairs
            } else {
                parse_pairs(&add)?
            };
            if pairs.is_empty() && delete.is_empty() {
                return Err(Error::Usage(
                    "provide --add-config, --add-config-file, or --delete-config".into(),
                ));
            }
            validate_config_entity_types(&entity_type)?;
            validate_config_entity_names(&entity_type, &entity_name)?;
            validate_config_keys(&pairs)?;
            validate_quota_change_names(&entity_type, &pairs, &delete)?;
            if quota_entity_types(&entity_type) && !only_scram_changes(&pairs, &delete) {
                return alter_quota_configs(
                    bootstrap,
                    command_config,
                    timeout,
                    format,
                    &entity_type,
                    &entity_name,
                    entity_default,
                    &pairs,
                    &delete,
                    execute,
                )
                .await;
            }
            let (entity_type, entity_name) =
                single_resource_entity(&entity_type, &entity_name, entity_default, true)?;
            if matches!(entity_type, ConfigEntityType::User) {
                return alter_user_scram(
                    config,
                    timeout,
                    format,
                    entity_name.as_deref().ok_or_else(|| {
                        Error::Usage("SCRAM alteration requires --entity-name".into())
                    })?,
                    &pairs,
                    &delete,
                    execute,
                );
            }
            if !execute {
                return config_change_preview(format, &pairs, &delete);
            }
            if protocol_config_resource_type(entity_type).is_some()
                && (!matches!(entity_type, ConfigEntityType::Broker)
                    || entity_name.as_deref() == Some(""))
            {
                return alter_protocol_config(
                    bootstrap,
                    command_config,
                    timeout,
                    format,
                    entity_type,
                    entity_name.as_deref().ok_or_else(|| {
                        Error::Usage("resource alteration requires --entity-name".into())
                    })?,
                    &pairs,
                    &delete,
                )
                .await;
            }
            let admin = admin(config)?;
            crate::ffi::incremental_alter_config(
                admin.inner().native_ptr(),
                native_resource_type(entity_type),
                entity_name.as_deref().ok_or_else(|| {
                    Error::Usage("resource alteration requires --entity-name".into())
                })?,
                &pairs,
                &delete,
                duration_ms(timeout)?,
            )
        }
    }
}

fn normalize_config_deletions(delete: Vec<String>) -> Vec<String> {
    delete
        .into_iter()
        .map(|key| key.trim().to_owned())
        .collect()
}

fn validate_config_entity_types(types: &[ConfigEntityType]) -> Result<()> {
    let distinct = types.iter().copied().collect::<BTreeSet<_>>();
    if distinct.len() != types.len() {
        return Err(Error::Usage("duplicate --entity-type values".into()));
    }
    if types.len() > 1
        && distinct != BTreeSet::from([ConfigEntityType::User, ConfigEntityType::Client])
    {
        return Err(Error::Usage(
            "only users and clients may be specified together".into(),
        ));
    }
    Ok(())
}

fn validate_config_entity_names(types: &[ConfigEntityType], names: &[String]) -> Result<()> {
    if types.len() == 1
        && matches!(
            types[0],
            ConfigEntityType::Broker | ConfigEntityType::BrokerLogger
        )
    {
        for name in names {
            name.parse::<i32>().map_err(|_| {
                Error::Usage(format!(
                    "the entity name for {} must be a valid integer broker ID: {name}",
                    config_entity_type_name(types[0])
                ))
            })?;
        }
    }
    Ok(())
}

fn validate_config_keys(add: &[(String, String)]) -> Result<()> {
    for (key, _) in add {
        if !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'$' | b'.' | b'_' | b'-'))
        {
            return Err(Error::Usage(format!(
                "invalid character found in config key: {key}"
            )));
        }
    }
    Ok(())
}

fn quota_entity_types(types: &[ConfigEntityType]) -> bool {
    !types.is_empty()
        && types.iter().all(|kind| {
            matches!(
                kind,
                ConfigEntityType::User | ConfigEntityType::Client | ConfigEntityType::Ip
            )
        })
}

fn only_scram_changes(add: &[(String, String)], delete: &[String]) -> bool {
    add.iter()
        .map(|(key, _)| key)
        .chain(delete)
        .all(|key| key.to_ascii_uppercase().starts_with("SCRAM-SHA-"))
}

fn validate_quota_change_names(
    types: &[ConfigEntityType],
    add: &[(String, String)],
    delete: &[String],
) -> Result<()> {
    if !quota_entity_types(types) {
        return Ok(());
    }
    let keys = add.iter().map(|(key, _)| key).chain(delete);
    let mut scram = Vec::new();
    let mut quota = Vec::new();
    let mut unknown = Vec::new();
    for key in keys {
        if key.to_ascii_uppercase().starts_with("SCRAM-SHA-") {
            scram.push(key.as_str());
        } else if matches!(
            key.as_str(),
            "producer_byte_rate"
                | "consumer_byte_rate"
                | "request_percentage"
                | "controller_mutation_rate"
                | "connection_creation_rate"
        ) {
            quota.push(key.as_str());
        } else {
            unknown.push(key.as_str());
        }
    }
    if !unknown.is_empty() {
        return Err(Error::Usage(format!(
            "unexpected quota config name(s): {}",
            unknown.join(",")
        )));
    }
    if types.contains(&ConfigEntityType::Ip)
        && quota.iter().any(|key| *key != "connection_creation_rate")
    {
        return Err(Error::Usage(
            "IP entities only support connection_creation_rate".into(),
        ));
    }
    if !types.contains(&ConfigEntityType::Ip) && quota.contains(&"connection_creation_rate") {
        return Err(Error::Usage(
            "connection_creation_rate is only valid for IP entities".into(),
        ));
    }
    if (!scram.is_empty() && types != [ConfigEntityType::User])
        || (!scram.is_empty() && !quota.is_empty())
    {
        return Err(Error::Usage(
            "SCRAM credentials require a single named user and cannot be altered with quota configs"
                .into(),
        ));
    }
    Ok(())
}

fn single_resource_entity(
    types: &[ConfigEntityType],
    names: &[String],
    entity_default: bool,
    require_name: bool,
) -> Result<(ConfigEntityType, Option<String>)> {
    if types.len() != 1 {
        return Err(Error::Usage(
            "multiple --entity-type values are only valid for user/client quota entities".into(),
        ));
    }
    if entity_default {
        if matches!(types[0], ConfigEntityType::Broker) {
            return Ok((types[0], Some(String::new())));
        }
        return Err(Error::Usage(
            "--entity-default is only valid for quota entities and brokers".into(),
        ));
    }
    if names.len() > 1 || (require_name && names.len() != 1) {
        return Err(Error::Usage(
            "specify exactly one --entity-name for this resource type".into(),
        ));
    }
    Ok((types[0], names.first().cloned()))
}

const fn quota_entity_name(kind: ConfigEntityType) -> Option<&'static str> {
    match kind {
        ConfigEntityType::User => Some("user"),
        ConfigEntityType::Client => Some("client-id"),
        ConfigEntityType::Ip => Some("ip"),
        _ => None,
    }
}

fn quota_entities<'a>(
    types: &'a [ConfigEntityType],
    names: &'a [String],
    entity_default: bool,
    require_names: bool,
) -> Result<Vec<(&'static str, Option<&'a str>)>> {
    if !quota_entity_types(types) {
        return Err(Error::Usage(
            "quota entities must be users, clients, or ips".into(),
        ));
    }
    if types.iter().copied().collect::<BTreeSet<_>>().len() != types.len() {
        return Err(Error::Usage("duplicate --entity-type values".into()));
    }
    if entity_default {
        if types.len() != 1 || !names.is_empty() {
            return Err(Error::Usage(
                "--entity-default requires exactly one entity type and no entity name".into(),
            ));
        }
        return Ok(vec![(
            quota_entity_name(types[0]).ok_or_else(|| Error::Usage("invalid quota type".into()))?,
            None,
        )]);
    }
    if require_names && names.len() != types.len() {
        return Err(Error::Usage(
            "exactly one --entity-name is required for every --entity-type".into(),
        ));
    }
    if !names.is_empty() && names.len() != types.len() {
        return Err(Error::Usage(
            "exactly one --entity-name must be specified for every --entity-type".into(),
        ));
    }
    types
        .iter()
        .enumerate()
        .map(|(index, kind)| {
            Ok((
                quota_entity_name(*kind)
                    .ok_or_else(|| Error::Usage("invalid quota type".into()))?,
                names.get(index).map(String::as_str),
            ))
        })
        .collect()
}

#[derive(Debug, Serialize)]
struct QuotaConfigRow {
    entity: String,
    config_type: String,
    name: String,
    value: String,
}

fn quota_entity_label(components: &[krafka::admin::QuotaEntityComponent]) -> String {
    let mut values = components
        .iter()
        .map(|component| {
            format!(
                "{}={}",
                component.entity_type,
                component.entity_name.as_deref().unwrap_or("<default>")
            )
        })
        .collect::<Vec<_>>();
    values.sort();
    values.join(",")
}

#[expect(
    clippy::too_many_arguments,
    reason = "arguments mirror Kafka's quota entity filter and shared command context"
)]
async fn describe_quota_configs(
    config: &rdkafka::ClientConfig,
    bootstrap: &str,
    command_config: Option<&Path>,
    timeout: Duration,
    format: OutputFormat,
    types: &[ConfigEntityType],
    names: &[String],
    entity_default: bool,
) -> Result<()> {
    let entities = quota_entities(types, names, entity_default, false)?;
    let components = entities
        .iter()
        .map(|(kind, name)| {
            (
                *kind,
                if entity_default {
                    1
                } else if name.is_some() {
                    0
                } else {
                    2
                },
                *name,
            )
        })
        .collect::<Vec<_>>();
    let client = config::protocol_admin(bootstrap, timeout, command_config).await?;
    let result = client.describe_client_quotas(&components, true).await?;
    drop(client);
    if let Some(error) = result.error {
        return Err(Error::Config(error));
    }
    let mut rows = result
        .entries
        .iter()
        .flat_map(|entry| {
            let entity = quota_entity_label(&entry.entity);
            entry.values.iter().map(move |value| QuotaConfigRow {
                entity: entity.clone(),
                config_type: "quota".into(),
                name: value.key.clone(),
                value: value.value.to_string(),
            })
        })
        .collect::<Vec<_>>();
    if types == [ConfigEntityType::User] && !entity_default {
        let admin = admin(config)?;
        let users = names.first().cloned().into_iter().collect::<Vec<_>>();
        rows.extend(
            ffi::describe_user_scram_credentials(
                admin.inner().native_ptr(),
                &users,
                duration_ms(timeout)?,
            )?
            .into_iter()
            .map(|credential| QuotaConfigRow {
                entity: format!("user={}", credential.user),
                config_type: "scram".into(),
                name: scram_mechanism_name(credential.mechanism).into(),
                value: credential.iterations.to_string(),
            }),
        );
    }
    output::write_value(format, "configs.describe-quota", &rows, |rows| {
        output::table(
            ["ENTITY", "CONFIG_TYPE", "NAME", "VALUE"],
            rows.iter().map(|row| {
                [
                    row.entity.clone(),
                    row.config_type.clone(),
                    row.name.clone(),
                    row.value.clone(),
                ]
            }),
        )
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "arguments mirror Kafka's quota alteration contract and shared command context"
)]
async fn alter_quota_configs(
    bootstrap: &str,
    command_config: Option<&Path>,
    timeout: Duration,
    format: OutputFormat,
    types: &[ConfigEntityType],
    names: &[String],
    entity_default: bool,
    add: &[(String, String)],
    delete: &[String],
    execute: bool,
) -> Result<()> {
    let entities = quota_entities(types, names, entity_default, true)?;
    let parsed = add
        .iter()
        .map(|(key, value)| {
            let value = value.parse::<f64>().map_err(|_| {
                Error::Usage(format!(
                    "cannot parse quota configuration value for {key}: {value}"
                ))
            })?;
            if !value.is_finite() {
                return Err(Error::Usage(format!(
                    "quota value for {key} must be finite"
                )));
            }
            Ok((key.as_str(), Some(value)))
        })
        .chain(delete.iter().map(|key| Ok((key.as_str(), None))))
        .collect::<Result<Vec<_>>>()?;
    if !execute {
        return config_change_preview(format, add, delete);
    }
    let client = config::protocol_admin(bootstrap, timeout, command_config).await?;
    let filters = entities
        .iter()
        .map(|(kind, name)| (*kind, i8::from(name.is_none()), *name))
        .collect::<Vec<_>>();
    let existing = client.describe_client_quotas(&filters, true).await?;
    if let Some(error) = existing.error {
        return Err(Error::Config(error));
    }
    let existing_keys = existing
        .entries
        .iter()
        .flat_map(|entry| entry.values.iter().map(|value| value.key.as_str()))
        .collect::<BTreeSet<_>>();
    let missing = delete
        .iter()
        .filter(|key| !existing_keys.contains(key.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(Error::Config(format!(
            "invalid quota config(s): {}",
            missing.join(",")
        )));
    }
    let alteration = krafka::admin::QuotaAlteration {
        entity: entities,
        ops: parsed,
    };
    let results = client.alter_client_quotas(&[alteration], false).await?;
    drop(client);
    let rows = results
        .iter()
        .map(|result| MutationRow {
            resource: quota_entity_label(&result.entity),
            status: if result.error.is_some() {
                "FAILED"
            } else {
                "ALTERED"
            }
            .into(),
            error: result.error.clone(),
        })
        .collect::<Vec<_>>();
    let failures = rows.iter().filter(|row| row.error.is_some()).count();
    write_mutation_rows(format, "configs.alter-quota", &rows)?;
    if failures == 0 {
        Ok(())
    } else {
        Err(Error::Partial {
            failed: failures,
            total: rows.len(),
        })
    }
}

const fn protocol_config_resource_type(
    kind: ConfigEntityType,
) -> Option<ProtocolConfigResourceType> {
    match kind {
        ConfigEntityType::Broker => Some(ProtocolConfigResourceType::Broker),
        ConfigEntityType::BrokerLogger => Some(ProtocolConfigResourceType::BrokerLogger),
        ConfigEntityType::ClientMetrics => Some(ProtocolConfigResourceType::ClientMetrics),
        _ => None,
    }
}

async fn protocol_config_broker(
    client: &krafka::admin::AdminClient,
    kind: ConfigEntityType,
    name: &str,
) -> Result<(i32, String)> {
    let cluster = client.describe_cluster().await?;
    let broker_id = if matches!(kind, ConfigEntityType::BrokerLogger)
        || (matches!(kind, ConfigEntityType::Broker) && !name.is_empty())
    {
        name.parse::<i32>()
            .map_err(|_| Error::Usage("broker-logger entity name must be a broker ID".into()))?
    } else {
        cluster.controller_id
    };
    let broker = cluster
        .brokers
        .iter()
        .find(|broker| broker.broker_id == broker_id)
        .ok_or_else(|| Error::Usage(format!("broker {broker_id} was not described")))?;
    Ok((broker_id, format!("{}:{}", broker.host, broker.port)))
}

#[expect(
    clippy::too_many_lines,
    reason = "request routing, protocol decoding, and structured output form one config operation"
)]
async fn describe_protocol_configs(
    bootstrap: &str,
    command_config: Option<&Path>,
    timeout: Duration,
    format: OutputFormat,
    kind: ConfigEntityType,
    name: Option<String>,
    all: bool,
) -> Result<()> {
    let resource_type = protocol_config_resource_type(kind)
        .ok_or_else(|| Error::Usage("invalid protocol config resource".into()))?;
    let client = config::protocol_admin(bootstrap, timeout, command_config).await?;
    let names = if let Some(name) = name {
        vec![name]
    } else if matches!(kind, ConfigEntityType::ClientMetrics) {
        client.list_client_metrics_resources().await?
    } else if matches!(kind, ConfigEntityType::Broker) {
        client
            .describe_cluster()
            .await?
            .brokers
            .into_iter()
            .map(|broker| broker.broker_id.to_string())
            .collect()
    } else {
        return Err(Error::Usage(
            "broker-logger describe requires --entity-name".into(),
        ));
    };
    let routing_name = names.first().map_or("", String::as_str);
    let (broker_id, address) = protocol_config_broker(&client, kind, routing_name).await?;
    let connection = client
        .pool()
        .get_connection_by_id(broker_id, &address)
        .await?;
    let request = DescribeConfigsRequest {
        resources: names
            .iter()
            .map(|name| DescribeConfigsResource {
                resource_type,
                resource_name: name.clone(),
                config_names: None,
            })
            .collect(),
        include_synonyms: all,
        include_documentation: false,
    };
    let version = connection
        .negotiate_api_version(
            ApiKey::DescribeConfigs,
            versions::DESCRIBE_CONFIGS_MAX,
            versions::DESCRIBE_CONFIGS_MIN,
        )
        .await
        .ok_or_else(|| Error::Unsupported("broker does not support DescribeConfigs".into()))?;
    let mut response = connection
        .send_request(ApiKey::DescribeConfigs, version, |buffer| {
            request.encode_versioned(version, buffer)
        })
        .await?;
    drop(connection);
    drop(client);
    let response =
        krafka::protocol::DescribeConfigsResponse::decode_versioned(version, &mut response)?;
    let mut rows = Vec::new();
    for resource in response.results {
        if !resource.error_code.is_ok() {
            return Err(Error::Config(
                resource
                    .error_message
                    .unwrap_or_else(|| format!("{:?}", resource.error_code)),
            ));
        }
        rows.extend(resource.configs.into_iter().filter_map(|entry| {
            (all || protocol_dynamic_config_source(kind, entry.config_source)).then(|| {
                ConfigDescriptionRow {
                    entity_type: config_entity_type_name(kind).into(),
                    entity_name: resource.resource_name.clone(),
                    name: entry.name,
                    value: entry.value,
                    source: entry.config_source.to_string(),
                    sensitive: entry.is_sensitive,
                }
            })
        }));
    }
    output::write_value(format, "configs.describe", &rows, |rows| {
        output::table(
            [
                "ENTITY_TYPE",
                "ENTITY_NAME",
                "NAME",
                "VALUE",
                "SOURCE",
                "SENSITIVE",
            ],
            rows.iter().map(|row| {
                [
                    row.entity_type.clone(),
                    row.entity_name.clone(),
                    row.name.clone(),
                    row.value.as_deref().unwrap_or("null").to_owned(),
                    row.source.clone(),
                    row.sensitive.to_string(),
                ]
            }),
        )
    })
}

const fn protocol_dynamic_config_source(kind: ConfigEntityType, source: i8) -> bool {
    matches!(
        (kind, source),
        (ConfigEntityType::Broker, 3)
            | (ConfigEntityType::BrokerLogger, 6)
            | (ConfigEntityType::ClientMetrics, 7)
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "arguments mirror a Kafka IncrementalAlterConfigs resource and command context"
)]
async fn alter_protocol_config(
    bootstrap: &str,
    command_config: Option<&Path>,
    timeout: Duration,
    format: OutputFormat,
    kind: ConfigEntityType,
    name: &str,
    add: &[(String, String)],
    delete: &[String],
) -> Result<()> {
    let resource_type = protocol_config_resource_type(kind)
        .ok_or_else(|| Error::Usage("invalid protocol config resource".into()))?;
    let client = config::protocol_admin(bootstrap, timeout, command_config).await?;
    let (broker_id, address) = protocol_config_broker(&client, kind, name).await?;
    let connection = client
        .pool()
        .get_connection_by_id(broker_id, &address)
        .await?;
    let request = IncrementalAlterConfigsRequest {
        resources: vec![IncrementalAlterConfigsResource {
            resource_type,
            resource_name: name.into(),
            configs: add
                .iter()
                .map(|(key, value)| AlterableConfig {
                    name: key.clone(),
                    config_operation: AlterConfigOp::Set,
                    value: Some(value.clone()),
                })
                .chain(delete.iter().map(|key| AlterableConfig {
                    name: key.clone(),
                    config_operation: AlterConfigOp::Delete,
                    value: None,
                }))
                .collect(),
        }],
        validate_only: false,
    };
    let version = connection
        .negotiate_api_version(
            ApiKey::IncrementalAlterConfigs,
            versions::INCREMENTAL_ALTER_CONFIGS_MAX,
            versions::INCREMENTAL_ALTER_CONFIGS_MIN,
        )
        .await
        .ok_or_else(|| {
            Error::Unsupported("broker does not support IncrementalAlterConfigs".into())
        })?;
    let mut response = connection
        .send_request(ApiKey::IncrementalAlterConfigs, version, |buffer| {
            request.encode_versioned(version, buffer)
        })
        .await?;
    drop(connection);
    drop(client);
    let response = krafka::protocol::IncrementalAlterConfigsResponse::decode_versioned(
        version,
        &mut response,
    )?;
    let rows = response
        .results
        .into_iter()
        .map(|result| MutationRow {
            resource: format!("{}:{}", config_entity_type_name(kind), result.resource_name),
            status: if result.error_code.is_ok() {
                "ALTERED"
            } else {
                "FAILED"
            }
            .into(),
            error: (!result.error_code.is_ok()).then(|| {
                result
                    .error_message
                    .unwrap_or_else(|| format!("{:?}", result.error_code))
            }),
        })
        .collect::<Vec<_>>();
    let failures = rows.iter().filter(|row| row.error.is_some()).count();
    write_mutation_rows(format, "configs.alter", &rows)?;
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
struct ConfigChangeRow<'a> {
    operation: &'static str,
    name: &'a str,
    value: Option<&'a str>,
}

fn config_change_preview(
    format: OutputFormat,
    pairs: &[(String, String)],
    delete: &[String],
) -> Result<()> {
    let rows = pairs
        .iter()
        .map(|(key, value)| ConfigChangeRow {
            operation: "SET",
            name: key,
            value: Some(value.as_str()),
        })
        .chain(delete.iter().map(|key| ConfigChangeRow {
            operation: "DELETE",
            name: key,
            value: None,
        }))
        .collect::<Vec<_>>();
    output::write_value(format, "configs.alter.preview", &rows, |rows| {
        output::table(
            ["OPERATION", "NAME", "VALUE"],
            rows.iter().map(|row| {
                [
                    row.operation.to_owned(),
                    row.name.to_owned(),
                    row.value.unwrap_or("-").to_owned(),
                ]
            }),
        )
    })
}

#[derive(Debug, Serialize)]
struct ConfigDescriptionRow {
    entity_type: String,
    entity_name: String,
    name: String,
    value: Option<String>,
    source: String,
    sensitive: bool,
}

async fn describe_resource_configs(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    format: OutputFormat,
    entity_type: ConfigEntityType,
    entity_name: Option<String>,
    all: bool,
) -> Result<()> {
    let names = match entity_name {
        Some(name) => vec![name],
        None => config_entity_names(config, timeout, entity_type)?,
    };
    let specifiers = names
        .iter()
        .map(|name| resource(entity_type, name))
        .collect::<Result<Vec<_>>>()?;
    let results = admin(config)?
        .describe_configs(
            &specifiers,
            &AdminOptions::new().request_timeout(Some(timeout)),
        )
        .await?;
    let resources = results
        .into_iter()
        .map(|item| item.map_err(|code| Error::Config(code.to_string())))
        .collect::<Result<Vec<_>>>()?;
    let kind = config_entity_type_name(entity_type);
    let rows = resources
        .into_iter()
        .flat_map(|resource| {
            let entity_name = owned_resource_name(&resource.specifier);
            resource.entries.into_iter().filter_map(move |entry| {
                (all || dynamic_config_source(&entry.source)).then(|| ConfigDescriptionRow {
                    entity_type: kind.into(),
                    entity_name: entity_name.clone(),
                    name: entry.name,
                    value: entry.value,
                    source: format!("{:?}", entry.source),
                    sensitive: entry.is_sensitive,
                })
            })
        })
        .collect::<Vec<_>>();
    output::write_value(format, "configs.describe", &rows, |rows| {
        output::table(
            [
                "ENTITY_TYPE",
                "ENTITY_NAME",
                "NAME",
                "VALUE",
                "SOURCE",
                "SENSITIVE",
            ],
            rows.iter().map(|row| {
                [
                    row.entity_type.clone(),
                    row.entity_name.clone(),
                    row.name.clone(),
                    row.value.as_deref().unwrap_or("null").to_owned(),
                    row.source.clone(),
                    row.sensitive.to_string(),
                ]
            }),
        )
    })
}

fn config_entity_names(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    kind: ConfigEntityType,
) -> Result<Vec<String>> {
    let consumer = base_consumer(config)?;
    match kind {
        ConfigEntityType::Topic => Ok(consumer
            .fetch_metadata(None, timeout)?
            .topics()
            .iter()
            .filter(|topic| topic.error().is_none())
            .map(|topic| topic.name().to_owned())
            .collect()),
        ConfigEntityType::Broker => Ok(consumer
            .fetch_metadata(None, timeout)?
            .brokers()
            .iter()
            .map(|broker| broker.id().to_string())
            .collect()),
        ConfigEntityType::Group => {
            let admin = admin(config)?;
            Ok(ffi::list_consumer_groups(
                admin.inner().native_ptr(),
                &[],
                &[],
                duration_ms(timeout)?,
            )?
            .into_iter()
            .map(|group| group.group)
            .collect())
        }
        ConfigEntityType::User => Err(Error::Usage(
            "SCRAM users must use the credential Admin API".into(),
        )),
        ConfigEntityType::Client | ConfigEntityType::Ip => Err(Error::Usage(
            "client and IP entities must use the quota Admin API".into(),
        )),
        ConfigEntityType::BrokerLogger | ConfigEntityType::ClientMetrics => Err(Error::Usage(
            "this resource type must use the protocol config API".into(),
        )),
    }
}

const fn config_entity_type_name(kind: ConfigEntityType) -> &'static str {
    match kind {
        ConfigEntityType::Topic => "topics",
        ConfigEntityType::Broker => "brokers",
        ConfigEntityType::Group => "groups",
        ConfigEntityType::User => "users",
        ConfigEntityType::Client => "clients",
        ConfigEntityType::Ip => "ips",
        ConfigEntityType::BrokerLogger => "broker-loggers",
        ConfigEntityType::ClientMetrics => "client-metrics",
    }
}

fn owned_resource_name(specifier: &OwnedResourceSpecifier) -> String {
    match specifier {
        OwnedResourceSpecifier::Topic(name) | OwnedResourceSpecifier::Group(name) => name.clone(),
        OwnedResourceSpecifier::Broker(id) => id.to_string(),
    }
}

const fn dynamic_config_source(source: &ConfigSource) -> bool {
    matches!(
        source,
        ConfigSource::DynamicTopic
            | ConfigSource::DynamicBroker
            | ConfigSource::DynamicDefaultBroker
    )
}

fn alter_user_scram(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    format: OutputFormat,
    user: &str,
    add: &[(String, String)],
    delete: &[String],
    execute: bool,
) -> Result<()> {
    let changes = parse_scram_changes(add, delete)?;
    let rows = changes
        .iter()
        .map(|change| match change {
            ffi::ScramCredentialAlteration::Upsert {
                mechanism,
                iterations,
                ..
            } => MutationRow {
                resource: format!("{user}:{}", scram_mechanism_name(*mechanism)),
                status: if execute {
                    format!("UPSERTED ({iterations} iterations)")
                } else {
                    format!("PREVIEW UPSERT ({iterations} iterations)")
                },
                error: None,
            },
            ffi::ScramCredentialAlteration::Delete { mechanism } => MutationRow {
                resource: format!("{user}:{}", scram_mechanism_name(*mechanism)),
                status: if execute {
                    "DELETED".into()
                } else {
                    "PREVIEW DELETE".into()
                },
                error: None,
            },
        })
        .collect::<Vec<_>>();
    if execute {
        let client = admin(config)?;
        ffi::alter_user_scram_credentials(
            client.inner().native_ptr(),
            user,
            &changes,
            duration_ms(timeout)?,
        )?;
    }
    write_mutation_rows(format, "configs.alter.scram", &rows)
}

const fn native_resource_type(kind: ConfigEntityType) -> rdkafka_sys::rd_kafka_ResourceType_t {
    match kind {
        ConfigEntityType::Topic => rdkafka_sys::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_TOPIC,
        ConfigEntityType::Broker => rdkafka_sys::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_BROKER,
        ConfigEntityType::Group => rdkafka_sys::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_GROUP,
        ConfigEntityType::User => rdkafka_sys::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_UNKNOWN,
        ConfigEntityType::Client
        | ConfigEntityType::Ip
        | ConfigEntityType::BrokerLogger
        | ConfigEntityType::ClientMetrics => {
            rdkafka_sys::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_UNKNOWN
        }
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
        ConfigEntityType::Client | ConfigEntityType::Ip => {
            return Err(Error::Config(
                "client and IP configuration must use the quota Admin API".into(),
            ));
        }
        ConfigEntityType::BrokerLogger | ConfigEntityType::ClientMetrics => {
            return Err(Error::Config(
                "this resource type must use the protocol config API".into(),
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

fn offsets(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    format: OutputFormat,
    args: &crate::cli::OffsetsArgs,
) -> Result<()> {
    let config = offsets_client_config(config);
    let consumer = base_consumer(&config)?;
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
    let spec = if let Some(timestamp) = args.timestamp {
        if timestamp < 0 {
            return Err(Error::Usage("--timestamp must be non-negative".into()));
        }
        ffi::ListOffsetSpec::Timestamp(timestamp)
    } else {
        match args.time {
            OffsetTime::Earliest => ffi::ListOffsetSpec::Earliest,
            OffsetTime::Latest => ffi::ListOffsetSpec::Latest,
            OffsetTime::MaxTimestamp => ffi::ListOffsetSpec::MaxTimestamp,
            OffsetTime::EarliestLocal => ffi::ListOffsetSpec::EarliestLocal,
            OffsetTime::LatestTiered => ffi::ListOffsetSpec::LatestTiered,
            OffsetTime::EarliestPendingUpload => ffi::ListOffsetSpec::EarliestPendingUpload,
        }
    };
    let client = admin(&config)?;
    let rows = ffi::list_offsets(
        client.inner().native_ptr(),
        &targets,
        spec,
        duration_ms(timeout)?,
    )?;
    output::write_value(format, "offsets", &rows, |rows| {
        output::table(
            ["TOPIC", "PARTITION", "OFFSET", "TIMESTAMP", "ERROR"],
            rows.iter().map(|row| {
                [
                    row.topic.clone(),
                    row.partition.to_string(),
                    row.offset
                        .map_or_else(|| "N/A".into(), |value| value.to_string()),
                    row.timestamp
                        .map_or_else(|| "N/A".into(), |value| value.to_string()),
                    row.error.clone().unwrap_or_default(),
                ]
            }),
        )
    })
}

fn offsets_client_config(config: &rdkafka::ClientConfig) -> rdkafka::ClientConfig {
    let mut config = config.clone();
    config.set("client.id", "GetOffsetShell");
    config
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
    split_java_list(value)
        .into_iter()
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

fn split_java_list(value: &str) -> Vec<&str> {
    if value.is_empty() {
        return vec![""];
    }
    let mut items = value.split(',').collect::<Vec<_>>();
    while items.last() == Some(&"") {
        items.pop();
    }
    items
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
    format: OutputFormat,
    path: &Path,
    execute: bool,
) -> Result<()> {
    let input = read_delete_records(path)?;
    let mut offsets = TopicPartitionList::new();
    for item in &input.partitions {
        offsets.add_partition_offset(&item.topic, item.partition, Offset::Offset(item.offset))?;
    }
    if !execute {
        let rows = input
            .partitions
            .iter()
            .map(|item| MutationRow {
                resource: format!("{}:{}", item.topic, item.partition),
                status: format!("PREVIEW BEFORE {}", item.offset),
                error: None,
            })
            .collect::<Vec<_>>();
        return write_mutation_rows(format, "delete-records", &rows);
    }
    let result = admin(config)?
        .delete_records(
            &offsets,
            &AdminOptions::new().operation_timeout(Some(timeout)),
        )
        .await?;
    let rows = result
        .elements()
        .into_iter()
        .map(|element| MutationRow {
            resource: format!("{}:{}", element.topic(), element.partition()),
            status: format!("LOW_WATERMARK {:?}", element.offset()),
            error: element.error().err().map(|error| error.to_string()),
        })
        .collect::<Vec<_>>();
    write_mutation_rows(format, "delete-records", &rows)
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
    port: u16,
    rack: Option<String>,
    fenced: bool,
    endpoint_type: String,
}

fn broker_table(rows: &[BrokerRow]) -> String {
    output::table(
        ["ID", "HOST", "PORT", "RACK", "STATE", "ENDPOINT_TYPE"],
        rows.iter().map(|row| {
            [
                row.id.to_string(),
                row.host.clone(),
                row.port.to_string(),
                row.rack.as_deref().unwrap_or("-").to_owned(),
                if row.fenced { "fenced" } else { "unfenced" }.into(),
                row.endpoint_type.clone(),
            ]
        }),
    )
}

async fn fenced_cluster_rows(
    bootstrap: &str,
    command_config: Option<&Path>,
    timeout: Duration,
) -> Result<Vec<BrokerRow>> {
    let client = config::protocol_admin(bootstrap, timeout, command_config).await?;
    let cluster = client.describe_cluster().await?;
    let broker = cluster
        .brokers
        .first()
        .ok_or_else(|| Error::Config("cluster returned no reachable broker".into()))?;
    let connection = client
        .pool()
        .get_connection_by_id(
            broker.broker_id,
            &format!("{}:{}", broker.host, broker.port),
        )
        .await?;
    let version = connection
        .negotiate_api_version(ApiKey::DescribeCluster, 2, 2)
        .await
        .ok_or_else(|| {
            Error::Unsupported(
                "broker does not support fenced broker listing (DescribeCluster v2)".into(),
            )
        })?;
    let request = DescribeClusterRequest {
        include_cluster_authorized_operations: false,
        endpoint_type: 1,
        include_fenced_brokers: true,
    };
    let mut response = connection
        .send_request(ApiKey::DescribeCluster, version, |buffer| {
            request.encode_versioned(version, buffer)
        })
        .await?;
    drop(connection);
    drop(client);
    let response = DescribeClusterResponse::decode_versioned(version, &mut response)?;
    if !response.error_code.is_ok() {
        return Err(Error::Config(
            response
                .error_message
                .unwrap_or_else(|| format!("{:?}", response.error_code)),
        ));
    }
    response
        .brokers
        .into_iter()
        .map(|broker| {
            Ok(BrokerRow {
                id: broker.broker_id,
                host: broker.host,
                port: u16::try_from(broker.port).map_err(|_| {
                    Error::Config(format!("broker {} returned invalid port", broker.broker_id))
                })?,
                rack: broker.rack,
                fenced: broker.is_fenced,
                endpoint_type: "broker".into(),
            })
        })
        .collect()
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
            let client = admin(config)?;
            let id = ffi::describe_cluster(client.inner().native_ptr(), duration_ms(timeout)?)?
                .cluster_id;
            output::write_value(format, "cluster.id", &id, |id| {
                output::table(["CLUSTER_ID"], [[id.clone()]])
            })
        }
        ClusterAction::ListEndpoints {
            include_fenced_brokers,
        } => {
            let rows = if *include_fenced_brokers {
                fenced_cluster_rows(bootstrap, command_config, timeout).await?
            } else {
                let client = admin(config)?;
                ffi::describe_cluster(client.inner().native_ptr(), duration_ms(timeout)?)?
                    .nodes
                    .into_iter()
                    .map(|broker| BrokerRow {
                        id: broker.id,
                        host: broker.host,
                        port: broker.port,
                        rack: broker.rack,
                        fenced: false,
                        endpoint_type: "broker".into(),
                    })
                    .collect()
            };
            output::write_value(format, "cluster.list-endpoints", &rows, |rows| {
                broker_table(rows)
            })
        }
        ClusterAction::ApiVersions => {
            api_versions(bootstrap, command_config, timeout, format, None).await
        }
        ClusterAction::Unregister { id, execute } => {
            if !execute {
                return write_mutation_rows(
                    format,
                    "cluster.unregister",
                    &[MutationRow {
                        resource: format!("broker:{id}"),
                        status: "PREVIEW".into(),
                        error: None,
                    }],
                );
            }
            let client = config::protocol_admin(bootstrap, timeout, command_config).await?;
            unregister_broker(&client, *id).await?;
            write_mutation_rows(
                format,
                "cluster.unregister",
                &[MutationRow {
                    resource: format!("broker:{id}"),
                    status: "UNREGISTERED".into(),
                    error: None,
                }],
            )
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
            let rows = acl_list_rows(client.inner().native_ptr(), filter, timeout_ms)?;
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
                let rows = bindings.into_iter().map(acl_row).collect::<Vec<_>>();
                return output::write_value(format, "acls.add.preview", &rows, |rows| {
                    acl_table(rows)
                });
            }
            let result = ffi::create_acls(client.inner().native_ptr(), &bindings, timeout_ms)?;
            write_acl_mutation_result(
                format,
                "acls.add",
                &format!("CREATED {}", result.matched.saturating_sub(result.failures)),
                &result,
            )?;
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
            write_acl_mutation_result(
                format,
                "acls.remove",
                &format!("DELETED {}", result.matched),
                &result,
            )?;
            if result.failures == 0 {
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

fn acl_list_rows(
    client: *mut rdkafka_sys::rd_kafka_t,
    filter: &crate::cli::AclFilterArgs,
    timeout_ms: i32,
) -> Result<Vec<AclRow>> {
    let resources = acl_resources(filter)?;
    let resources = if resources.is_empty() {
        vec![(AclResourceType::Any, None)]
    } else {
        resources
            .into_iter()
            .map(|(resource_type, name)| (resource_type, Some(name)))
            .collect()
    };
    let principals = normalized_acl_values(&filter.principal, "principal")?;
    let mut seen = BTreeSet::new();
    let mut rows = Vec::new();
    for (resource_type, resource_name) in resources {
        let acl_filter = AclBindingFilter {
            resource_type,
            resource_name,
            pattern_type: acl_wire_pattern(filter.resource_pattern_type),
            principal: None,
            host: None,
            operation: AclOperation::Any,
            permission_type: AclPermissionType::Any,
        };
        for binding in ffi::describe_acls(client, &acl_filter, timeout_ms)? {
            if !principals.is_empty() && !principals.contains(&binding.principal) {
                continue;
            }
            let row = acl_row(binding);
            let key = (
                row.resource_type.clone(),
                row.resource_name.clone(),
                row.pattern_type.clone(),
                row.principal.clone(),
                row.host.clone(),
                row.operation.clone(),
                row.permission.clone(),
            );
            if seen.insert(key) {
                rows.push(row);
            }
        }
    }
    Ok(rows)
}

fn write_acl_mutation_result(
    format: OutputFormat,
    command: &str,
    status: &str,
    result: &ffi::AclMutationResult,
) -> Result<()> {
    let rows = [MutationRow {
        resource: "acl-bindings".into(),
        status: status.into(),
        error: None,
    }];
    output::write_value_with_errors(format, command, &rows, &result.errors, |rows| {
        output::table(
            ["RESOURCE", "STATUS", "ERROR"],
            rows.iter().map(|row| {
                [
                    row.resource.clone(),
                    row.status.clone(),
                    row.error.as_deref().unwrap_or("-").to_owned(),
                ]
            }),
        )
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

fn normalized_acl_values(values: &[String], label: &str) -> Result<BTreeSet<String>> {
    values
        .iter()
        .map(|value| {
            let value = value.trim();
            if value.is_empty() {
                Err(Error::Usage(format!("ACL {label} cannot be empty")))
            } else {
                Ok(value.to_owned())
            }
        })
        .collect()
}

fn acl_resources(filter: &crate::cli::AclFilterArgs) -> Result<Vec<(AclResourceType, String)>> {
    let mut resources = Vec::new();
    for topic in normalized_acl_values(&filter.topic, "topic")? {
        resources.push((AclResourceType::Topic, topic));
    }
    for group in normalized_acl_values(&filter.group, "group")? {
        resources.push((AclResourceType::Group, group));
    }
    if filter.cluster {
        resources.push((AclResourceType::Cluster, "kafka-cluster".into()));
    }
    for transactional_id in normalized_acl_values(&filter.transactional_id, "transactional ID")? {
        resources.push((AclResourceType::TransactionalId, transactional_id));
    }
    if !filter.delegation_token.is_empty() {
        return Err(Error::Unsupported(
            "librdkafka does not support delegation-token ACL resources".into(),
        ));
    }
    Ok(resources)
}

const fn acl_wire_pattern(pattern: crate::cli::AclResourcePattern) -> AclPatternType {
    match pattern {
        crate::cli::AclResourcePattern::Any => AclPatternType::Any,
        crate::cli::AclResourcePattern::Literal => AclPatternType::Literal,
        crate::cli::AclResourcePattern::Match => AclPatternType::Match,
        crate::cli::AclResourcePattern::Prefixed => AclPatternType::Prefixed,
    }
}

fn acl_bindings(
    mutation: &crate::cli::AclMutationArgs,
    operations: &[AclOperation],
) -> Result<Vec<AclBinding>> {
    if matches!(
        mutation.filter.resource_pattern_type,
        crate::cli::AclResourcePattern::Any | crate::cli::AclResourcePattern::Match
    ) {
        return Err(Error::Usage(
            "ACL creation requires literal or prefixed resource pattern type".into(),
        ));
    }
    validate_acl_entry_values(mutation)?;
    let resources = acl_mutation_resources(mutation, operations)?;
    let mut principals = normalized_acl_values(&mutation.allow_principal, "allow principal")?
        .into_iter()
        .map(|principal| (principal, AclPermissionType::Allow))
        .chain(
            normalized_acl_values(&mutation.deny_principal, "deny principal")?
                .into_iter()
                .map(|principal| (principal, AclPermissionType::Deny)),
        )
        .collect::<Vec<_>>();
    if principals.is_empty() {
        principals.extend(
            normalized_acl_values(&mutation.filter.principal, "principal")?
                .into_iter()
                .map(|principal| (principal, AclPermissionType::Allow)),
        );
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
                vec![mutation.host.as_deref().map_or("*", str::trim)]
            } else {
                configured_hosts.iter().map(String::as_str).collect()
            };
            for host in hosts {
                for operation in &operations {
                    bindings.push(AclBinding {
                        resource_type,
                        resource_name: resource_name.clone(),
                        pattern_type: acl_wire_pattern(mutation.filter.resource_pattern_type),
                        principal: principal.clone(),
                        host: host.trim().to_owned(),
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
    validate_acl_entry_values(mutation)?;
    let resources = if mutation.producer || mutation.consumer {
        acl_mutation_resources(mutation, operations)?
    } else {
        let resources = acl_resources(&mutation.filter)?;
        if resources.is_empty() {
            vec![(AclResourceType::Any, String::new(), operations.to_vec())]
        } else {
            resources
                .into_iter()
                .map(|(resource_type, resource_name)| {
                    (resource_type, resource_name, operations.to_vec())
                })
                .collect()
        }
    };
    let mut entries = acl_removal_entries(mutation)?;
    if entries.is_empty() {
        entries.extend(
            normalized_acl_values(&mutation.filter.principal, "principal")?
                .into_iter()
                .map(|principal| {
                    (
                        principal,
                        mutation.host.as_deref().map_or("*", str::trim).to_owned(),
                        AclPermissionType::Any,
                    )
                }),
        );
    }
    let mut filters = Vec::new();
    for (resource_type, resource_name, operations) in resources {
        if entries.is_empty() {
            filters.push(AclBindingFilter {
                resource_type,
                resource_name: (!resource_name.is_empty()).then(|| resource_name.clone()),
                pattern_type: acl_wire_pattern(mutation.filter.resource_pattern_type),
                principal: None,
                host: None,
                operation: AclOperation::Any,
                permission_type: AclPermissionType::Any,
            });
            continue;
        }
        let operations = if operations.is_empty() {
            vec![AclOperation::All]
        } else {
            operations
        };
        for (principal, host, permission) in &entries {
            for operation in &operations {
                filters.push(AclBindingFilter {
                    resource_type,
                    resource_name: (!resource_name.is_empty()).then(|| resource_name.clone()),
                    pattern_type: acl_wire_pattern(mutation.filter.resource_pattern_type),
                    principal: Some(principal.clone()),
                    host: Some(host.clone()),
                    operation: *operation,
                    permission_type: *permission,
                });
            }
        }
    }
    Ok(filters)
}

fn acl_removal_entries(
    mutation: &crate::cli::AclMutationArgs,
) -> Result<Vec<(String, String, AclPermissionType)>> {
    let allow_principals = normalized_acl_values(&mutation.allow_principal, "allow principal")?;
    let deny_principals = normalized_acl_values(&mutation.deny_principal, "deny principal")?;
    let allow_hosts = normalized_acl_values(&mutation.allow_host, "allow host")?;
    let deny_hosts = normalized_acl_values(&mutation.deny_host, "deny host")?;
    let default_host = mutation.host.as_deref().map_or("*", str::trim);
    let allowed = allow_principals.iter().flat_map(|principal| {
        let hosts = if allow_hosts.is_empty() {
            vec![default_host]
        } else {
            allow_hosts.iter().map(String::as_str).collect()
        };
        hosts
            .into_iter()
            .map(move |host| (principal.clone(), host.to_owned(), AclPermissionType::Allow))
    });
    let denied = deny_principals.iter().flat_map(|principal| {
        let hosts = if deny_hosts.is_empty() {
            vec![default_host]
        } else {
            deny_hosts.iter().map(String::as_str).collect()
        };
        hosts
            .into_iter()
            .map(move |host| (principal.clone(), host.to_owned(), AclPermissionType::Deny))
    });
    Ok(allowed.chain(denied).collect())
}

fn validate_acl_entry_values(mutation: &crate::cli::AclMutationArgs) -> Result<()> {
    normalized_acl_values(&mutation.allow_principal, "allow principal")?;
    normalized_acl_values(&mutation.deny_principal, "deny principal")?;
    normalized_acl_values(&mutation.allow_host, "allow host")?;
    normalized_acl_values(&mutation.deny_host, "deny host")?;
    if mutation
        .host
        .as_deref()
        .is_some_and(|host| host.trim().is_empty())
    {
        return Err(Error::Usage("ACL host cannot be empty".into()));
    }
    Ok(())
}

fn acl_mutation_resources(
    mutation: &crate::cli::AclMutationArgs,
    operations: &[AclOperation],
) -> Result<Vec<(AclResourceType, String, Vec<AclOperation>)>> {
    if !mutation.producer && !mutation.consumer {
        let resources = acl_resources(&mutation.filter)?;
        if resources.is_empty() {
            return Err(Error::Usage("an ACL resource selector is required".into()));
        }
        return Ok(resources
            .into_iter()
            .map(|(resource_type, resource_name)| {
                (resource_type, resource_name, operations.to_vec())
            })
            .collect());
    }
    if !mutation.deny_principal.is_empty() || !mutation.deny_host.is_empty() {
        return Err(Error::Usage(
            "role ACLs only support allow principals and hosts".into(),
        ));
    }
    let topics = normalized_acl_values(&mutation.filter.topic, "topic")?;
    if topics.is_empty() {
        return Err(Error::Usage(
            "--producer and --consumer require --topic".into(),
        ));
    }
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
    let mut resources = topics
        .into_iter()
        .map(|topic| (AclResourceType::Topic, topic, topic_operations.clone()))
        .collect::<Vec<_>>();
    if mutation.consumer {
        let groups = normalized_acl_values(&mutation.filter.group, "group")?;
        if groups.is_empty() {
            return Err(Error::Usage("--consumer requires --group".into()));
        }
        resources.extend(
            groups
                .into_iter()
                .map(|group| (AclResourceType::Group, group, vec![AclOperation::Read])),
        );
    }
    if mutation.producer {
        resources.extend(
            normalized_acl_values(&mutation.filter.transactional_id, "transactional ID")?
                .into_iter()
                .map(|transactional_id| {
                    (
                        AclResourceType::TransactionalId,
                        transactional_id,
                        vec![AclOperation::Write, AclOperation::Describe],
                    )
                }),
        );
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
            additional,
            disallow_replication_factor_change,
            throttle,
            replica_alter_log_dirs_throttle,
            execute,
        } => {
            let plan = read_reassignment(reassignment_json_file)?;
            if !execute {
                return write_reassignment_mutation_rows(
                    format,
                    "reassign.execute",
                    &plan,
                    "PREVIEW EXECUTE",
                );
            }
            let client = config::protocol_admin(bootstrap, timeout, command_config).await?;
            let active = client.list_partition_reassignments(None, timeout).await?;
            if !additional && reassignment_count(&active) != 0 {
                return Err(Error::Usage(
                    "cannot execute while a partition reassignment is active; use --additional to add to the existing reassignment"
                        .into(),
                ));
            }
            if *disallow_replication_factor_change {
                reject_replication_factor_changes(config, timeout, &plan)?;
            }
            apply_reassignment_throttles(
                config,
                timeout,
                &plan,
                &active,
                *throttle,
                *replica_alter_log_dirs_throttle,
            )?;
            let result = client
                .alter_partition_reassignments(reassignment_topics(&plan, false), timeout)
                .await?;
            let rows = reassignment_result_rows(&result, "STARTED");
            let failures = rows.iter().filter(|row| row.error.is_some()).count();
            if failures == 0 {
                let log_dir_rows = alter_reassignment_log_dirs(&client, &plan).await?;
                drop(client);
                let log_dir_failures = log_dir_rows
                    .iter()
                    .filter(|row| row.error.is_some())
                    .count();
                if log_dir_failures != 0 {
                    write_mutation_rows(format, "reassign.execute.log-dirs", &log_dir_rows)?;
                    return Err(Error::Partial {
                        failed: log_dir_failures,
                        total: plan.partitions.len(),
                    });
                }
                write_reassignment_mutation_rows(format, "reassign.execute", &plan, "STARTED")
            } else {
                drop(client);
                write_mutation_rows(format, "reassign.execute", &rows)?;
                Err(Error::Partial {
                    failed: failures,
                    total: plan.partitions.len(),
                })
            }
        }
        ReassignAction::Cancel {
            reassignment_json_file,
            preserve_throttles,
            execute,
        } => {
            let plan = read_reassignment(reassignment_json_file)?;
            if !execute {
                return write_reassignment_mutation_rows(
                    format,
                    "reassign.cancel",
                    &plan,
                    "PREVIEW CANCEL",
                );
            }
            let client = config::protocol_admin(bootstrap, timeout, command_config).await?;
            let result = client
                .alter_partition_reassignments(reassignment_topics(&plan, true), timeout)
                .await?;
            let brokers = client
                .describe_cluster()
                .await?
                .brokers
                .into_iter()
                .map(|broker| broker.broker_id)
                .collect::<BTreeSet<_>>();
            drop(client);
            let rows = reassignment_result_rows(&result, "CANCELLED");
            let failures = rows.iter().filter(|row| row.error.is_some()).count();
            if failures == 0 {
                if !preserve_throttles {
                    clear_reassignment_throttles(config, timeout, &plan, &brokers)?;
                }
                write_reassignment_mutation_rows(format, "reassign.cancel", &plan, "CANCELLED")
            } else {
                write_mutation_rows(format, "reassign.cancel", &rows)?;
                Err(Error::Partial {
                    failed: failures,
                    total: plan.partitions.len(),
                })
            }
        }
        ReassignAction::Verify {
            reassignment_json_file,
            preserve_throttles,
        } => {
            let plan = read_reassignment(reassignment_json_file)?;
            let client = config::protocol_admin(bootstrap, timeout, command_config).await?;
            let running = client.list_partition_reassignments(None, timeout).await?;
            let log_dirs_ongoing = reassignment_log_dirs_ongoing(&client, &plan).await?;
            let brokers = client
                .describe_cluster()
                .await?
                .brokers
                .into_iter()
                .map(|broker| broker.broker_id)
                .collect::<BTreeSet<_>>();
            drop(client);
            let current = current_replicas(config, timeout, &plan)?;
            let statuses = reassignment_statuses(&plan, &running, &current);
            if reassignment_count(&running) == 0 && !log_dirs_ongoing && !preserve_throttles {
                clear_reassignment_throttles(config, timeout, &plan, &brokers)?;
            }
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

fn reassignment_count(running: &[krafka::admin::PartitionReassignmentInfo]) -> usize {
    running.iter().map(|topic| topic.partitions.len()).sum()
}

fn reject_replication_factor_changes(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    plan: &ReassignmentFile,
) -> Result<()> {
    let current = current_replicas(config, timeout, plan)?;
    validate_replication_factors(plan, &current)
}

fn validate_replication_factors(
    plan: &ReassignmentFile,
    current: &BTreeMap<(String, i32), Vec<i32>>,
) -> Result<()> {
    let changes = plan
        .partitions
        .iter()
        .filter_map(|target| {
            let key = (target.topic.clone(), target.partition);
            current.get(&key).map_or_else(
                || {
                    Some(format!(
                        "{}:{} does not exist in cluster metadata",
                        target.topic, target.partition
                    ))
                },
                |replicas| {
                    (replicas.len() != target.replicas.len()).then(|| {
                        format!(
                            "{}:{} would change replication factor from {} to {}",
                            target.topic,
                            target.partition,
                            replicas.len(),
                            target.replicas.len()
                        )
                    })
                },
            )
        })
        .collect::<Vec<_>>();
    if changes.is_empty() {
        Ok(())
    } else {
        Err(Error::Usage(format!(
            "--disallow-replication-factor-change rejected the plan: {}",
            changes.join("; ")
        )))
    }
}

#[derive(Debug, Default)]
struct PartitionMove {
    sources: BTreeSet<i32>,
    destinations: BTreeSet<i32>,
}

type ReassignmentMoveMap = BTreeMap<(String, i32), PartitionMove>;

fn reassignment_move_map(
    plan: &ReassignmentFile,
    running: &[krafka::admin::PartitionReassignmentInfo],
    current: &BTreeMap<(String, i32), Vec<i32>>,
) -> Result<ReassignmentMoveMap> {
    let mut moves = ReassignmentMoveMap::new();
    for topic in running {
        for partition in &topic.partitions {
            let adding = partition
                .adding_replicas
                .iter()
                .copied()
                .collect::<BTreeSet<_>>();
            moves.insert(
                (topic.name.clone(), partition.partition_index),
                PartitionMove {
                    sources: partition
                        .replicas
                        .iter()
                        .copied()
                        .filter(|broker| !adding.contains(broker))
                        .collect(),
                    destinations: adding,
                },
            );
        }
    }
    for target in &plan.partitions {
        let key = (target.topic.clone(), target.partition);
        let sources = moves.get(&key).map_or_else(
            || {
                current
                    .get(&key)
                    .map(|replicas| replicas.iter().copied().collect())
            },
            |movement| Some(movement.sources.clone()),
        );
        let sources = sources.ok_or_else(|| {
            Error::Usage(format!(
                "{}:{} does not exist in cluster metadata",
                target.topic, target.partition
            ))
        })?;
        let destinations = target
            .replicas
            .iter()
            .copied()
            .filter(|broker| !sources.contains(broker))
            .collect();
        moves.insert(
            key,
            PartitionMove {
                sources,
                destinations,
            },
        );
    }
    Ok(moves)
}

fn checked_throttle(value: u64, option: &str) -> Result<String> {
    i64::try_from(value)
        .map(|value| value.to_string())
        .map_err(|_| Error::Usage(format!("{option} must not exceed {}", i64::MAX)))
}

fn apply_reassignment_throttles(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    plan: &ReassignmentFile,
    running: &[krafka::admin::PartitionReassignmentInfo],
    inter_broker: Option<u64>,
    log_dirs: Option<u64>,
) -> Result<()> {
    if inter_broker.is_none() && log_dirs.is_none() {
        return Ok(());
    }
    let current = current_replicas(config, timeout, plan)?;
    let moves = reassignment_move_map(plan, running, &current)?;
    let admin = admin(config)?;
    let timeout_ms = duration_ms(timeout)?;
    if let Some(rate) = inter_broker {
        let rate = checked_throttle(rate, "--throttle")?;
        let mut topic_values = BTreeMap::<String, (BTreeSet<String>, BTreeSet<String>)>::new();
        let mut brokers = BTreeSet::new();
        for ((topic, partition), movement) in &moves {
            let values = topic_values.entry(topic.clone()).or_default();
            for broker in &movement.sources {
                values.0.insert(format!("{partition}:{broker}"));
                brokers.insert(*broker);
            }
            for broker in &movement.destinations {
                values.1.insert(format!("{partition}:{broker}"));
                brokers.insert(*broker);
            }
        }
        for (topic, (leaders, followers)) in topic_values {
            ffi::incremental_alter_config(
                admin.inner().native_ptr(),
                rdkafka_sys::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_TOPIC,
                &topic,
                &[
                    (
                        "leader.replication.throttled.replicas".into(),
                        leaders.into_iter().collect::<Vec<_>>().join(","),
                    ),
                    (
                        "follower.replication.throttled.replicas".into(),
                        followers.into_iter().collect::<Vec<_>>().join(","),
                    ),
                ],
                &[],
                timeout_ms,
            )?;
        }
        for broker in brokers {
            ffi::incremental_alter_config(
                admin.inner().native_ptr(),
                rdkafka_sys::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_BROKER,
                &broker.to_string(),
                &[
                    ("leader.replication.throttled.rate".into(), rate.clone()),
                    ("follower.replication.throttled.rate".into(), rate.clone()),
                ],
                &[],
                timeout_ms,
            )?;
        }
    }
    if let Some(rate) = log_dirs {
        let rate = checked_throttle(rate, "--replica-alter-log-dirs-throttle")?;
        for broker in broker_log_dir_plan(plan).keys() {
            ffi::incremental_alter_config(
                admin.inner().native_ptr(),
                rdkafka_sys::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_BROKER,
                &broker.to_string(),
                &[(
                    "replica.alter.log.dirs.io.max.bytes.per.second".into(),
                    rate.clone(),
                )],
                &[],
                timeout_ms,
            )?;
        }
    }
    Ok(())
}

async fn reassignment_log_dirs_ongoing(
    client: &krafka::admin::AdminClient,
    plan: &ReassignmentFile,
) -> Result<bool> {
    let targets = plan
        .partitions
        .iter()
        .flat_map(|partition| {
            partition
                .replicas
                .iter()
                .zip(&partition.log_dirs)
                .filter(|(_, directory)| directory.as_str() != "any")
                .map(|(broker, _)| (partition.topic.as_str(), partition.partition, *broker))
        })
        .collect::<BTreeSet<_>>();
    if targets.is_empty() {
        return Ok(false);
    }
    let topics = reassignment_filters(plan)
        .into_iter()
        .map(|topic| DescribableLogDirTopic {
            topic: topic.name,
            partitions: topic.partition_indexes,
        })
        .collect();
    Ok(client
        .describe_log_dirs(Some(topics))
        .await?
        .iter()
        .any(|directory| {
            directory.topics.iter().any(|topic| {
                topic.partitions.iter().any(|partition| {
                    partition.is_future_key
                        && targets.contains(&(
                            topic.name.as_str(),
                            partition.partition_index,
                            directory.broker_id,
                        ))
                })
            })
        }))
}

fn clear_reassignment_throttles(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    plan: &ReassignmentFile,
    cluster_brokers: &BTreeSet<i32>,
) -> Result<()> {
    let admin = admin(config)?;
    let timeout_ms = duration_ms(timeout)?;
    let mut brokers = cluster_brokers.clone();
    let mut topics = BTreeSet::new();
    for partition in &plan.partitions {
        topics.insert(partition.topic.as_str());
        brokers.extend(&partition.replicas);
    }
    let topic_deletes = [
        "leader.replication.throttled.replicas".into(),
        "follower.replication.throttled.replicas".into(),
    ];
    for topic in topics {
        ffi::incremental_alter_config(
            admin.inner().native_ptr(),
            rdkafka_sys::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_TOPIC,
            topic,
            &[],
            &topic_deletes,
            timeout_ms,
        )?;
    }
    let broker_deletes = [
        "leader.replication.throttled.rate".into(),
        "follower.replication.throttled.rate".into(),
        "replica.alter.log.dirs.io.max.bytes.per.second".into(),
    ];
    for broker in brokers {
        ffi::incremental_alter_config(
            admin.inner().native_ptr(),
            rdkafka_sys::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_BROKER,
            &broker.to_string(),
            &[],
            &broker_deletes,
            timeout_ms,
        )?;
    }
    Ok(())
}

fn write_reassignment_mutation_rows(
    format: OutputFormat,
    command: &str,
    plan: &ReassignmentFile,
    status: &str,
) -> Result<()> {
    let rows = plan
        .partitions
        .iter()
        .map(|partition| MutationRow {
            resource: format!("{}:{}", partition.topic, partition.partition),
            status: status.into(),
            error: None,
        })
        .collect::<Vec<_>>();
    write_mutation_rows(format, command, &rows)
}

fn reassignment_result_rows(
    result: &krafka::admin::AlterReassignmentsResult,
    success_status: &str,
) -> Vec<MutationRow> {
    let mut rows = result
        .topics
        .iter()
        .flat_map(|topic| {
            topic.partitions.iter().map(|partition| MutationRow {
                resource: format!("{}:{}", topic.name, partition.partition_index),
                status: if partition.error.is_some() {
                    "FAILED".into()
                } else {
                    success_status.into()
                },
                error: partition.error.clone(),
            })
        })
        .collect::<Vec<_>>();
    if let Some(error) = &result.error {
        rows.push(MutationRow {
            resource: "reassignment-request".into(),
            status: "FAILED".into(),
            error: Some(error.clone()),
        });
    }
    rows
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
) -> Result<Vec<MutationRow>> {
    let moves = broker_log_dir_plan(plan);
    if moves.is_empty() {
        return Ok(Vec::new());
    }
    let cluster = client.describe_cluster().await?;
    let mut rows = Vec::new();
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
            let topic = decode_required_acl_string(&mut response, "log-dir topic")?;
            let partition_count = decode_acl_count(&mut response)?;
            for _ in 0..partition_count {
                let partition = i32::decode(&mut response)?;
                let error_code = i16::decode(&mut response)?;
                rows.push(MutationRow {
                    resource: format!("broker:{broker_id}:{topic}:{partition}"),
                    status: if error_code == 0 {
                        "LOG_DIR_MOVED".into()
                    } else {
                        "FAILED".into()
                    },
                    error: (error_code != 0).then(|| format!("Kafka error code {error_code}")),
                });
            }
        }
    }
    Ok(rows)
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
    path_to_json_file: Option<&Path>,
    execute: bool,
) -> Result<()> {
    let targets = match (all, topic, partition, path_to_json_file) {
        (true, None, None, None) => None,
        (false, Some(topic), Some(partition), None) => Some(vec![(topic.to_owned(), partition)]),
        (false, None, None, Some(path)) => Some(read_election_targets(path)?),
        _ => {
            return Err(Error::Usage(
                "use exactly one of --all-topic-partitions, --path-to-json-file, or both --topic and --partition".into(),
            ));
        }
    };
    if !execute {
        let preview = targets.as_ref().map_or_else(
            || {
                vec![ElectionTargetRow {
                    topic: "*".into(),
                    partition: None,
                }]
            },
            |targets| {
                targets
                    .iter()
                    .map(|(topic, partition)| ElectionTargetRow {
                        topic: topic.clone(),
                        partition: Some(*partition),
                    })
                    .collect()
            },
        );
        return output::write_value(format, "leader-election.preview", &preview, |rows| {
            output::table(
                ["TOPIC", "PARTITION"],
                rows.iter().map(|row| {
                    [
                        row.topic.clone(),
                        row.partition
                            .map_or_else(|| "ALL".into(), |value| value.to_string()),
                    ]
                }),
            )
        });
    }
    let admin = admin(config)?;
    let rows = crate::ffi::elect_leaders(
        admin.inner().native_ptr(),
        matches!(kind, ElectionType::Unclean),
        targets.as_deref(),
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

#[derive(Debug, Deserialize)]
struct ElectionTargetFile {
    partitions: Vec<ElectionTarget>,
}

#[derive(Debug, Deserialize)]
struct ElectionTarget {
    topic: String,
    partition: i32,
}

#[derive(Debug, Serialize)]
struct ElectionTargetRow {
    topic: String,
    partition: Option<i32>,
}

fn read_election_targets(path: &Path) -> Result<Vec<(String, i32)>> {
    let input: ElectionTargetFile = serde_json::from_reader(std::fs::File::open(path)?)?;
    if input.partitions.is_empty() {
        return Err(Error::Usage(
            "leader election partition list is empty".into(),
        ));
    }
    let mut seen = BTreeSet::new();
    input
        .partitions
        .into_iter()
        .map(|target| {
            if target.topic.is_empty() || target.partition < 0 {
                return Err(Error::Usage(
                    "leader election targets require a non-empty topic and non-negative partition"
                        .into(),
                ));
            }
            let key = (target.topic, target.partition);
            if !seen.insert(key.clone()) {
                return Err(Error::Usage(format!(
                    "duplicate leader election target {}:{}",
                    key.0, key.1
                )));
            }
            Ok(key)
        })
        .collect()
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

fn component_properties(
    path: Option<&Path>,
    inline: &[String],
) -> Result<BTreeMap<String, String>> {
    let mut properties = path
        .map(config::load_properties)
        .transpose()?
        .unwrap_or_default()
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    for (key, value) in parse_pairs(inline)? {
        properties.insert(key, value);
    }
    Ok(properties)
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
    if value == Some("") {
        return Ok(None);
    }
    value
        .map(|value| {
            split_java_list(value)
                .into_iter()
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

    fn topic_selector() -> TopicSelector {
        TopicSelector {
            topic: None,
            topic_id: None,
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
    fn component_properties_should_load_file_then_apply_inline_overrides() {
        let mut file = tempfile::NamedTempFile::new().expect("temporary properties file");
        writeln!(file, "parse.key=false\nkey.separator=:").expect("write properties");

        let properties = component_properties(
            Some(file.path()),
            &["parse.key=true".into(), "null.marker=NULL".into()],
        )
        .expect("component properties");

        assert_eq!(
            properties.get("parse.key").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            properties.get("key.separator").map(String::as_str),
            Some(":")
        );
        assert_eq!(
            properties.get("null.marker").map(String::as_str),
            Some("NULL")
        );
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
    fn quota_entities_should_pair_user_and_client_names() {
        let names = ["alice".to_owned(), "billing".to_owned()];
        let entities = quota_entities(
            &[ConfigEntityType::User, ConfigEntityType::Client],
            &names,
            false,
            true,
        )
        .expect("valid composite quota entity");

        assert_eq!(
            entities,
            [("user", Some("alice")), ("client-id", Some("billing"))]
        );
    }

    #[test]
    fn quota_entities_should_represent_default_ip() {
        assert_eq!(
            quota_entities(&[ConfigEntityType::Ip], &[], true, true)
                .expect("valid default IP quota"),
            [("ip", None)]
        );
    }

    #[test]
    fn config_entity_types_should_reject_duplicates() {
        assert!(matches!(
            validate_config_entity_types(&[ConfigEntityType::Topic, ConfigEntityType::Topic]),
            Err(Error::Usage(message)) if message.contains("duplicate")
        ));
    }

    #[test]
    fn config_broker_entity_name_should_require_an_integer() {
        assert!(matches!(
            validate_config_entity_names(
                &[ConfigEntityType::BrokerLogger],
                &["not-a-broker".into()]
            ),
            Err(Error::Usage(message)) if message.contains("integer broker ID")
        ));
    }

    #[test]
    fn config_keys_should_reject_characters_disallowed_by_kafka() {
        assert!(matches!(
            validate_config_keys(&[("retention ms".into(), "1000".into())]),
            Err(Error::Usage(message)) if message.contains("invalid character")
        ));
    }

    #[test]
    fn config_deletions_should_trim_each_comma_separated_key() {
        assert_eq!(
            normalize_config_deletions(vec![" retention.ms".into(), "segment.bytes ".into()]),
            ["retention.ms", "segment.bytes"]
        );
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
    fn topic_partition_pattern_should_drop_java_trailing_empty_rules() {
        let patterns = parse_topic_partition_patterns("events:0,").expect("trailing comma");

        assert!(patterns[0].matches("events", 0) && !patterns[0].matches("audit", 0));
    }

    #[test]
    fn partition_list_should_follow_java_trailing_empty_semantics() {
        assert_eq!(
            (
                parse_partitions(Some("0,")).expect("trailing comma"),
                parse_partitions(Some("")).expect("empty list"),
            ),
            (Some(vec![0]), None)
        );
    }

    #[test]
    fn offsets_client_should_use_original_client_id() {
        let mut config = rdkafka::ClientConfig::new();
        config.set("client.id", "custom");

        assert_eq!(
            offsets_client_config(&config).get("client.id"),
            Some("GetOffsetShell")
        );
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
    fn consumer_include_should_be_anchored_without_rust_regex_validation() {
        assert_eq!(
            consumer_include_pattern("integration-jso{,1}n"),
            "^(integration-jso{,1}n)$"
        );
    }

    #[test]
    fn topic_id_should_reject_malformed_kafka_uuid() {
        assert!(matches!(
            validate_topic_id(Some("not-a-topic-id")),
            Err(Error::Usage(_))
        ));
    }

    #[test]
    fn topic_id_should_accept_librdkafka_standard_base64_uuid() {
        assert!(validate_topic_id(Some("YcqKQkG1QC+w8OkFq/qppA")).is_ok());
    }

    #[test]
    fn zero_topic_id_should_not_override_topic_name() {
        assert!(!is_nonzero_topic_id(ZERO_TOPIC_ID));
    }

    #[test]
    fn topic_creation_should_reject_non_positive_partition_count() {
        assert!(matches!(
            validate_topic_creation_counts(Some(0), None),
            Err(Error::Usage(_))
        ));
    }

    #[test]
    fn topic_creation_should_reject_replication_factor_above_short_max() {
        assert!(matches!(
            validate_topic_creation_counts(None, Some(i32::from(i16::MAX) + 1)),
            Err(Error::Usage(_))
        ));
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
            line_reader: "org.apache.kafka.tools.LineMessageReader".into(),
            key_separator: None,
            parse_key: false,
            compression_type: "none".into(),
            acks: Some("all".into()),
            sync: false,
            batch_size: None,
            max_partition_memory_bytes: None,
            message_send_max_retries: None,
            retry_backoff_ms: None,
            linger_ms: None,
            request_timeout_ms: None,
            metadata_expiry_ms: None,
            max_block_ms: Some(60_000),
            max_memory_bytes: None,
            socket_buffer_size: None,
            json: true,
            reader_properties: Vec::new(),
            deprecated_reader_properties: Vec::new(),
            reader_config: None,
            properties: Vec::new(),
            deprecated_properties: Vec::new(),
        };
        let options = line_reader_options(&args).expect("reader options");
        let input = producer_input(
            r#"{"key":"order-1","value":"created","partition":2,"headers":{"trace":"abc","empty":null}}"#,
            true,
            &options,
        )
        .expect("valid JSON record");
        assert_eq!(input.key.as_deref(), Some("order-1"));
        assert_eq!(input.value.as_deref(), Some("created"));
        assert_eq!(input.partition, Some(2));
        assert!(
            input
                .headers
                .iter()
                .any(|(key, value)| key == "trace" && value.as_deref() == Some("abc"))
        );
        assert!(
            input
                .headers
                .iter()
                .any(|(key, value)| key == "empty" && value.is_none())
        );
    }

    #[test]
    fn producer_input_should_reject_negative_json_partition() {
        let args = crate::cli::ProduceArgs {
            topic: "events".into(),
            line_reader: "org.apache.kafka.tools.LineMessageReader".into(),
            key_separator: None,
            parse_key: false,
            compression_type: "none".into(),
            acks: Some("all".into()),
            sync: false,
            batch_size: None,
            max_partition_memory_bytes: None,
            message_send_max_retries: None,
            retry_backoff_ms: None,
            linger_ms: None,
            request_timeout_ms: None,
            metadata_expiry_ms: None,
            max_block_ms: Some(60_000),
            max_memory_bytes: None,
            socket_buffer_size: None,
            json: true,
            reader_properties: Vec::new(),
            deprecated_reader_properties: Vec::new(),
            reader_config: None,
            properties: Vec::new(),
            deprecated_properties: Vec::new(),
        };
        let options = line_reader_options(&args).expect("reader options");
        assert!(matches!(
            producer_input(r#"{"value":"bad","partition":-1}"#, true, &options),
            Err(Error::Usage(_))
        ));
    }

    #[test]
    fn producer_properties_should_parse_headers_key_and_null_marker() {
        let cli = Cli::try_parse_from([
            "kafka",
            "--bootstrap-server",
            "localhost:9092",
            "produce",
            "--topic",
            "events",
            "--property",
            "parse.headers=true",
            "--property",
            "headers.delimiter=|",
            "--property",
            "parse.key=true",
            "--property",
            "key.separator=|",
            "--property",
            "null.marker=NULL",
        ])
        .expect("producer properties");
        let Command::Produce(args) = cli.command else {
            panic!("expected produce command");
        };
        let options = line_reader_options(&args).expect("reader options");

        let input = producer_input("trace:abc,empty:NULL|order-1|created", false, &options)
            .expect("line input");

        assert_eq!(
            input,
            ProducerInput {
                key: Some("order-1".into()),
                value: Some("created".into()),
                partition: None,
                headers: vec![("trace".into(), Some("abc".into())), ("empty".into(), None)],
            }
        );
    }

    #[test]
    fn consumer_properties_should_configure_default_formatter() {
        let cli = Cli::try_parse_from([
            "kafka",
            "--bootstrap-server",
            "localhost:9092",
            "consume",
            "--topic",
            "events",
            "--property",
            "print.partition=true",
            "--property",
            "print.headers=true",
            "--property",
            "print.key=true",
            "--property",
            "key.separator=|",
            "--property",
            "null.literal=NULL",
        ])
        .expect("consumer properties");
        let Command::Consume(args) = cli.command else {
            panic!("expected consume command");
        };

        let options = message_formatter_options(&args).expect("formatter options");

        assert!(options.print_partition && options.print_headers && options.print_key);
        assert_eq!(options.key_separator, b"|");
        assert_eq!(options.null_literal, b"NULL");
    }

    #[test]
    fn string_deserializer_should_follow_kafka_utf8_display_semantics() {
        assert_eq!(
            native_deserializer(
                Some("org.apache.kafka.common.serialization.StringDeserializer"),
                "value"
            )
            .expect("string deserializer"),
            NativeDeserializer::Utf8String
        );
        assert_eq!(
            deserialize_for_display(
                Some(b"valid UTF-8"),
                NativeDeserializer::Utf8String,
                b"null"
            ),
            b"valid UTF-8"
        );
        assert_eq!(
            String::from_utf8(deserialize_for_display(
                Some(&[b'a', 0xff, b'b']),
                NativeDeserializer::Utf8String,
                b"null"
            ))
            .expect("lossy UTF-8 result"),
            "a\u{fffd}b"
        );
        assert!(matches!(
            native_deserializer(Some("example.CustomDeserializer"), "value"),
            Err(Error::Unsupported(message)) if message.contains("example.CustomDeserializer")
        ));
    }

    #[test]
    fn formatter_property_should_override_deserializer_option() {
        let cli = Cli::try_parse_from([
            "kafka",
            "--bootstrap-server",
            "localhost:9092",
            "consume",
            "--topic",
            "events",
            "--key-deserializer",
            "example.CustomDeserializer",
            "--formatter-property",
            "key.deserializer=org.apache.kafka.common.serialization.StringDeserializer",
            "--value-deserializer",
            "org.apache.kafka.common.serialization.StringDeserializer",
            "--formatter-property",
            "value.deserializer.encoding=UTF8",
        ])
        .expect("consumer deserializers");
        let Command::Consume(args) = cli.command else {
            panic!("expected consume command");
        };
        let options = message_formatter_options(&args).expect("formatter options");
        assert_eq!(options.key_deserializer, NativeDeserializer::Utf8String);
        assert_eq!(options.value_deserializer, NativeDeserializer::Utf8String);
    }

    #[test]
    fn console_component_classes_should_accept_kafka_defaults() {
        let producer = Cli::try_parse_from([
            "kafka",
            "--bootstrap-server",
            "localhost:9092",
            "produce",
            "--topic",
            "events",
            "--line-reader",
            "org.apache.kafka.tools.LineMessageReader",
        ])
        .expect("producer arguments");
        let Command::Produce(producer) = producer.command else {
            panic!("expected produce command");
        };
        line_reader_options(&producer).expect("default line reader");

        let consumer = Cli::try_parse_from([
            "kafka",
            "--bootstrap-server",
            "localhost:9092",
            "consume",
            "--topic",
            "events",
            "--formatter",
            "org.apache.kafka.tools.consumer.DefaultMessageFormatter",
        ])
        .expect("consumer arguments");
        let Command::Consume(consumer) = consumer.command else {
            panic!("expected consume command");
        };
        message_formatter_options(&consumer).expect("default formatter");
    }

    #[test]
    fn console_component_classes_should_reject_java_plugins_explicitly() {
        let producer = Cli::try_parse_from([
            "kafka",
            "--bootstrap-server",
            "localhost:9092",
            "produce",
            "--topic",
            "events",
            "--line-reader",
            "example.CustomReader",
        ])
        .expect("producer arguments");
        let Command::Produce(producer) = producer.command else {
            panic!("expected produce command");
        };
        assert!(matches!(
            line_reader_options(&producer),
            Err(Error::Unsupported(message)) if message.contains("example.CustomReader")
        ));

        let consumer = Cli::try_parse_from([
            "kafka",
            "--bootstrap-server",
            "localhost:9092",
            "consume",
            "--topic",
            "events",
            "--formatter",
            "example.CustomFormatter",
        ])
        .expect("consumer arguments");
        let Command::Consume(consumer) = consumer.command else {
            panic!("expected consume command");
        };
        assert!(matches!(
            message_formatter_options(&consumer),
            Err(Error::Unsupported(message)) if message.contains("example.CustomFormatter")
        ));
    }

    #[test]
    fn consumer_without_group_should_use_ephemeral_non_committing_group() {
        let cli = Cli::try_parse_from([
            "kafka",
            "--bootstrap-server",
            "localhost:9092",
            "consume",
            "--topic",
            "events",
        ])
        .expect("consumer arguments");
        let Command::Consume(args) = cli.command else {
            panic!("expected consume command");
        };
        let mut config = rdkafka::ClientConfig::new();

        configure_consumer(&mut config, &args).expect("consumer configuration");

        assert!(
            config
                .get("group.id")
                .is_some_and(|group| group.starts_with("console-consumer-"))
        );
        assert_eq!(config.get("enable.auto.commit"), Some("false"));
        assert_eq!(config.get("client.id"), Some("console-consumer"));
        assert_eq!(config.get("auto.offset.reset"), Some("latest"));
        assert_eq!(config.get("isolation.level"), Some("read_uncommitted"));
    }

    #[test]
    fn consumer_should_reject_conflicting_group_and_offset_reset_sources() {
        let cli = Cli::try_parse_from([
            "kafka",
            "--bootstrap-server",
            "localhost:9092",
            "consume",
            "--topic",
            "events",
            "--group",
            "cli-group",
            "--command-property",
            "group.id=property-group",
        ])
        .expect("consumer arguments");
        let Command::Consume(args) = cli.command else {
            panic!("expected consume command");
        };
        let mut config = rdkafka::ClientConfig::new();
        assert!(matches!(
            configure_consumer(&mut config, &args),
            Err(Error::Usage(message)) if message.contains("must match")
        ));

        let cli = Cli::try_parse_from([
            "kafka",
            "--bootstrap-server",
            "localhost:9092",
            "consume",
            "--topic",
            "events",
            "--from-beginning",
            "--command-property",
            "auto.offset.reset=latest",
        ])
        .expect("consumer arguments");
        let Command::Consume(args) = cli.command else {
            panic!("expected consume command");
        };
        let mut config = rdkafka::ClientConfig::new();
        assert!(matches!(
            configure_consumer(&mut config, &args),
            Err(Error::Usage(message)) if message.contains("auto.offset.reset")
        ));
    }

    #[test]
    fn consumer_offset_should_accept_kafka_names_and_reject_invalid_values() {
        assert_eq!(
            consumer_offset(Some("earliest"), false).expect("earliest"),
            Offset::Beginning
        );
        assert_eq!(
            consumer_offset(Some("LATEST"), false).expect("latest"),
            Offset::End
        );
        assert_eq!(
            consumer_offset(Some("42"), false).expect("numeric offset"),
            Offset::Offset(42)
        );
        assert!(matches!(
            consumer_offset(Some("-1"), false),
            Err(Error::Usage(_))
        ));
        assert!(matches!(
            consumer_offset(Some("middle"), false),
            Err(Error::Usage(_))
        ));
    }

    #[test]
    fn consumer_should_stop_before_polling_when_max_messages_is_zero() {
        assert!(!should_consume_more(Some(0), 0));
    }

    #[test]
    fn consumer_should_continue_without_limit_when_max_messages_is_minus_one() {
        assert!(should_consume_more(Some(-1), i64::MAX));
    }

    #[test]
    fn consumer_should_stop_at_positive_max_messages_limit() {
        assert!(!should_consume_more(Some(2), 2));
    }

    #[test]
    fn producer_options_should_map_to_librdkafka_configuration() {
        let cli = Cli::try_parse_from([
            "kafka",
            "--bootstrap-server",
            "localhost:9092",
            "produce",
            "--topic",
            "events",
            "--batch-size",
            "4096",
            "--max-partition-memory-bytes",
            "8192",
            "--message-send-max-retries",
            "7",
            "--retry-backoff-ms",
            "250",
            "--timeout",
            "10",
            "--request-timeout-ms",
            "5000",
            "--metadata-expiry-ms",
            "60000",
            "--max-block-ms",
            "1234",
            "--max-memory-bytes",
            "1048577",
            "--socket-buffer-size",
            "32768",
        ])
        .expect("producer options");
        let Command::Produce(args) = cli.command else {
            panic!("expected produce command");
        };
        let mut config = rdkafka::ClientConfig::new();
        let max_block_ms = configure_producer(&mut config, &args).expect("producer configuration");
        assert_eq!(config.get("batch.size"), Some("8192"));
        assert_eq!(max_block_ms, 1234);
        assert_eq!(config.get("message.send.max.retries"), Some("7"));
        assert_eq!(config.get("retry.backoff.ms"), Some("250"));
        assert_eq!(config.get("linger.ms"), Some("10"));
        assert_eq!(config.get("request.timeout.ms"), Some("5000"));
        assert_eq!(config.get("metadata.max.age.ms"), Some("60000"));
        assert_eq!(config.get("topic.metadata.refresh.interval.ms"), None);
        assert_eq!(config.get("queue.buffering.max.kbytes"), Some("1025"));
        assert_eq!(config.get("socket.send.buffer.bytes"), Some("32768"));
    }

    #[test]
    fn producer_defaults_and_properties_should_follow_kafka_precedence() {
        let cli = Cli::try_parse_from([
            "kafka",
            "--bootstrap-server",
            "localhost:9092",
            "produce",
            "--topic",
            "events",
            "--command-property",
            "acks=0",
            "--command-property",
            "linger.ms=25",
            "--command-property",
            "max.block.ms=42",
            "--command-property",
            "buffer.memory=2048",
            "--command-property",
            "send.buffer.bytes=4096",
            "--command-property",
            "compression.type=gzip",
            "--command-property",
            "client.id=custom-producer",
        ])
        .expect("producer properties");
        let Command::Produce(args) = cli.command else {
            panic!("expected produce command");
        };
        let mut config = rdkafka::ClientConfig::new();
        apply_client_properties(&mut config, &args.properties).expect("client properties");

        let max_block_ms = configure_producer(&mut config, &args).expect("producer configuration");

        assert_eq!(config.get("acks"), Some("0"));
        assert_eq!(config.get("linger.ms"), Some("25"));
        assert_eq!(max_block_ms, 42);
        assert_eq!(config.get("queue.buffering.max.kbytes"), Some("2"));
        assert_eq!(config.get("buffer.memory"), None);
        assert_eq!(config.get("socket.send.buffer.bytes"), Some("4096"));
        assert_eq!(config.get("send.buffer.bytes"), None);
        assert_eq!(config.get("compression.type"), Some("none"));
        assert_eq!(config.get("client.id"), Some("custom-producer"));
        assert_eq!(config.get("batch.size"), Some("16384"));
        assert_eq!(config.get("message.send.max.retries"), Some("3"));
        assert_eq!(config.get("request.timeout.ms"), Some("1500"));
        assert_eq!(config.get("metadata.max.age.ms"), Some("300000"));
    }

    #[test]
    fn explicit_producer_options_should_override_command_properties() {
        let cli = Cli::try_parse_from([
            "kafka",
            "--bootstrap-server",
            "localhost:9092",
            "produce",
            "--topic",
            "events",
            "--request-required-acks",
            "1",
            "--timeout",
            "50",
            "--max-block-ms",
            "75",
            "--command-property",
            "acks=0",
            "--command-property",
            "linger.ms=25",
            "--command-property",
            "max.block.ms=42",
        ])
        .expect("producer options");
        let Command::Produce(args) = cli.command else {
            panic!("expected produce command");
        };
        let mut config = rdkafka::ClientConfig::new();
        apply_client_properties(&mut config, &args.properties).expect("client properties");

        let max_block_ms = configure_producer(&mut config, &args).expect("producer configuration");

        assert_eq!(config.get("acks"), Some("1"));
        assert_eq!(config.get("linger.ms"), Some("50"));
        assert_eq!(max_block_ms, 75);
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
    fn consumer_group_filters_should_accept_kafka_names() {
        assert_eq!(
            parse_group_states("Stable,Preparing_Rebalance").expect("states"),
            [
                ffi::ConsumerGroupState::Stable,
                ffi::ConsumerGroupState::PreparingRebalance,
            ]
        );
        assert_eq!(
            parse_group_types("consumer,CLASSIC").expect("types"),
            [
                ffi::ConsumerGroupType::Consumer,
                ffi::ConsumerGroupType::Classic,
            ]
        );
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
    fn reset_offsets_should_reject_missing_target_before_broker_work() {
        let cli = Cli::try_parse_from([
            "kafka",
            "--bootstrap-server",
            "localhost:9092",
            "groups",
            "reset-offsets",
            "--group",
            "test-group",
            "--topic",
            "events",
        ])
        .expect("reset command parses");
        let Command::Groups(groups) = cli.command else {
            panic!("expected groups command");
        };
        let GroupAction::ResetOffsets(args) = groups.action else {
            panic!("expected reset-offsets action");
        };

        assert!(matches!(validate_reset_target(&args), Err(Error::Usage(_))));
    }

    #[test]
    fn reset_topics_should_merge_repeated_partition_selections() {
        let topics = parse_reset_topics(&[
            "events:0,2".into(),
            "events:1".into(),
            "orders:0".into(),
            "orders".into(),
        ])
        .expect("valid reset topics");
        assert_eq!(topics["events"], Some(BTreeSet::from([0, 1, 2])));
        assert_eq!(topics["orders"], None);
    }

    #[test]
    fn reset_topics_should_reject_negative_partitions() {
        assert!(parse_reset_topics(&["events:-1".into()]).is_err());
    }

    #[test]
    fn resettable_groups_should_retain_inactive_and_missing_groups() {
        let states = BTreeMap::from([
            ("empty".into(), "Empty".into()),
            ("dead".into(), "Dead".into()),
            ("active".into(), "Stable".into()),
        ]);
        let (groups, _) = classify_resettable_groups(
            &[
                "empty".into(),
                "dead".into(),
                "missing".into(),
                "active".into(),
            ],
            &states,
        );
        assert_eq!(groups, ["empty", "dead", "missing"]);
    }

    #[test]
    fn resettable_groups_should_report_active_group_state() {
        let states = BTreeMap::from([("active".into(), "Stable".into())]);
        let (_, errors) = classify_resettable_groups(&["active".into()], &states);
        assert_eq!(
            errors,
            [
                "assignments can only be reset if group 'active' is inactive; current state is Stable"
            ]
        );
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
    fn acl_bindings_should_expand_repeated_resource_selectors() {
        let cli = Cli::try_parse_from([
            "kafka",
            "--bootstrap-server",
            "localhost:9092",
            "acls",
            "add",
            "--topic",
            "orders",
            "--topic",
            "payments",
            "--group",
            "billing",
            "--allow-principal",
            "User:reader",
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
        let names = bindings
            .iter()
            .map(|binding| binding.resource_name.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(names, BTreeSet::from(["billing", "orders", "payments"]));
    }

    #[test]
    fn acl_resources_should_trim_and_deduplicate_repeated_values() {
        let cli = Cli::try_parse_from([
            "kafka",
            "--bootstrap-server",
            "localhost:9092",
            "acls",
            "list",
            "--topic",
            " orders ",
            "--topic",
            "orders",
        ])
        .expect("ACL command");
        let Command::Acls(args) = cli.command else {
            panic!("expected ACL command");
        };
        let AclAction::List(filter) = args.action else {
            panic!("expected ACL list");
        };
        assert_eq!(
            acl_resources(&filter).expect("resources"),
            [(AclResourceType::Topic, "orders".into())]
        );
    }

    #[test]
    fn acl_add_should_reject_filter_only_pattern_types() {
        for pattern in ["any", "match"] {
            let cli = Cli::try_parse_from([
                "kafka",
                "--bootstrap-server",
                "localhost:9092",
                "acls",
                "add",
                "--topic",
                "orders",
                "--resource-pattern-type",
                pattern,
                "--allow-principal",
                "User:reader",
            ])
            .expect("ACL command");
            let Command::Acls(args) = cli.command else {
                panic!("expected ACL command");
            };
            let AclAction::Add(mutation) = args.action else {
                panic!("expected ACL add");
            };
            assert!(matches!(acl_bindings(&mutation, &[]), Err(Error::Usage(_))));
        }
    }

    #[test]
    fn acl_removal_should_preserve_permission_specific_hosts() {
        let cli = Cli::try_parse_from([
            "kafka",
            "--bootstrap-server",
            "localhost:9092",
            "acls",
            "remove",
            "--topic",
            "orders",
            "--allow-principal",
            "User:reader",
            "--allow-host",
            "10.0.0.1",
            "--operation",
            "read",
        ])
        .expect("ACL command");
        let Command::Acls(args) = cli.command else {
            panic!("expected ACL command");
        };
        let AclAction::Remove(mutation) = args.action else {
            panic!("expected ACL remove");
        };
        let operations = acl_operations(&mutation.operation).expect("operations");
        let filters = acl_removal_filters(&mutation, &operations).expect("filters");
        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].principal.as_deref(), Some("User:reader"));
        assert_eq!(filters[0].host.as_deref(), Some("10.0.0.1"));
        assert_eq!(filters[0].operation, AclOperation::Read);
        assert_eq!(filters[0].permission_type, AclPermissionType::Allow);
    }

    #[test]
    fn acl_removal_without_principal_should_delete_all_matching_entries() {
        let cli = Cli::try_parse_from([
            "kafka",
            "--bootstrap-server",
            "localhost:9092",
            "acls",
            "remove",
            "--topic",
            "orders",
            "--resource-pattern-type",
            "match",
            "--operation",
            "read",
        ])
        .expect("ACL command");
        let Command::Acls(args) = cli.command else {
            panic!("expected ACL command");
        };
        let AclAction::Remove(mutation) = args.action else {
            panic!("expected ACL remove");
        };
        let operations = acl_operations(&mutation.operation).expect("operations");
        let filters = acl_removal_filters(&mutation, &operations).expect("filters");
        assert_eq!(filters.len(), 1);
        assert!(filters[0].principal.is_none());
        assert!(filters[0].host.is_none());
        assert_eq!(filters[0].pattern_type, AclPatternType::Match);
        assert_eq!(filters[0].operation, AclOperation::Any);
        assert_eq!(filters[0].permission_type, AclPermissionType::Any);
    }

    #[test]
    fn acl_removal_with_principal_should_default_to_all_operation() {
        let cli = Cli::try_parse_from([
            "kafka",
            "--bootstrap-server",
            "localhost:9092",
            "acls",
            "remove",
            "--topic",
            "orders",
            "--principal",
            "User:reader",
        ])
        .expect("ACL command");
        let Command::Acls(args) = cli.command else {
            panic!("expected ACL command");
        };
        let AclAction::Remove(mutation) = args.action else {
            panic!("expected ACL remove");
        };
        let filters = acl_removal_filters(&mutation, &[]).expect("filters");
        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].host.as_deref(), Some("*"));
        assert_eq!(filters[0].operation, AclOperation::All);
        assert_eq!(filters[0].permission_type, AclPermissionType::Any);
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
    fn leader_election_file_should_parse_and_reject_duplicates() {
        let mut file = tempfile::NamedTempFile::new().expect("temporary file");
        write!(
            file,
            r#"{{"partitions":[{{"topic":"events","partition":0}},{{"topic":"orders","partition":1}}]}}"#
        )
        .expect("write leader-election fixture");
        assert_eq!(
            read_election_targets(file.path()).expect("valid targets"),
            [("events".into(), 0), ("orders".into(), 1)]
        );

        let mut duplicate = tempfile::NamedTempFile::new().expect("temporary file");
        write!(
            duplicate,
            r#"{{"partitions":[{{"topic":"events","partition":0}},{{"topic":"events","partition":0}}]}}"#
        )
        .expect("write duplicate fixture");
        assert!(matches!(
            read_election_targets(duplicate.path()),
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
    fn replication_factor_guard_should_reject_changed_factor() {
        let plan = ReassignmentFile {
            version: 1,
            partitions: vec![ReassignmentPartition {
                topic: "events".into(),
                partition: 0,
                replicas: vec![1, 2, 3],
                log_dirs: Vec::new(),
            }],
        };
        let current = BTreeMap::from([(("events".into(), 0), vec![1, 2])]);

        assert!(matches!(
            validate_replication_factors(&plan, &current),
            Err(Error::Usage(message)) if message.contains("from 2 to 3")
        ));
    }

    #[test]
    fn replication_factor_guard_should_accept_replica_movement() {
        let plan = ReassignmentFile {
            version: 1,
            partitions: vec![ReassignmentPartition {
                topic: "events".into(),
                partition: 0,
                replicas: vec![2, 3],
                log_dirs: Vec::new(),
            }],
        };
        let current = BTreeMap::from([(("events".into(), 0), vec![1, 2])]);

        assert!(validate_replication_factors(&plan, &current).is_ok());
    }

    #[test]
    fn reassignment_throttle_map_should_include_source_and_new_destination() {
        let plan = ReassignmentFile {
            version: 1,
            partitions: vec![ReassignmentPartition {
                topic: "events".into(),
                partition: 0,
                replicas: vec![2, 3],
                log_dirs: Vec::new(),
            }],
        };
        let current = BTreeMap::from([(("events".into(), 0), vec![1, 2])]);

        let moves = reassignment_move_map(&plan, &[], &current).expect("valid move map");
        let movement = &moves[&("events".into(), 0)];
        assert_eq!(movement.sources, BTreeSet::from([1, 2]));
        assert_eq!(movement.destinations, BTreeSet::from([3]));
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
