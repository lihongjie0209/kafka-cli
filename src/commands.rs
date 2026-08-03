//! Kafka command implementations.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    io::{self, Write},
    net::ToSocketAddrs,
    path::Path,
    process,
    sync::{
        LazyLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use bytes::{Buf, BufMut, BytesMut};
use chrono::{DateTime, NaiveDateTime, Utc};
use futures::StreamExt;
use krafka::protocol::{
    AlterConfigOp, AlterableConfig, ApiKey, ApiVersionsRequest,
    ConfigResourceType as ProtocolConfigResourceType, Decode, DescribableLogDirTopic,
    DescribeClusterRequest, DescribeClusterResponse, DescribeConfigsRequest,
    DescribeConfigsResource, IncrementalAlterConfigsRequest, IncrementalAlterConfigsResource,
    KafkaString, ListPartitionReassignmentsTopic, ReassignablePartition, ReassignableTopic,
    TaggedFields, TryEncode, VersionedDecode, VersionedEncode, versions,
};
use krafka::share_consumer::{
    AcknowledgeType as ShareAcknowledgeType, AcknowledgementMode, ShareConsumer,
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
    producer::{DeliveryFuture, FutureProducer, FutureRecord, Producer},
    topic_partition_list::TopicPartitionList,
};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::{
    cli::{
        AclAction, AllGroupType, AllGroupsAction, Cli, ClientMetricsAction, ClusterAction, Command,
        ConfigAction, ConfigEntityArgs, ConfigEntityType, DelegationTokenAction, DescribeTopicArgs,
        ElectionType, FeatureAction, GroupAction, ListTopicArgs, MetadataQuorumAction, OffsetTime,
        ReassignAction, ResetOffsetsArgs, ShareConsumeArgs, ShareGroupAction,
        ShareGroupResetOffsetsArgs, StreamsApplicationResetArgs, StreamsGroupAction,
        StreamsGroupResetOffsetsArgs, TopicAction, TransactionAction,
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
#[expect(
    clippy::too_many_lines,
    clippy::large_stack_frames,
    reason = "top-level dispatch explicitly routes every Kafka command family"
)]
pub async fn execute(cli: Cli) -> Result<()> {
    if let Command::Groups(args) = &cli.command
        && let GroupAction::ValidateRegex { regex } = &args.action
    {
        return validate_group_regex(cli.output, regex);
    }
    if let Command::Features(args) = &cli.command
        && matches!(
            args.action,
            FeatureAction::VersionMapping { .. } | FeatureAction::FeatureDependencies { .. }
        )
    {
        return features_local(cli.output, &args.action);
    }
    if let Command::MetadataQuorum(args) = &cli.command
        && args.bootstrap_controller.is_some()
    {
        return Err(Error::Unsupported(
            "--bootstrap-controller requires controller-listener bootstrap, which the current native client does not expose"
                .into(),
        ));
    }
    if let Command::Features(args) = &cli.command
        && args.bootstrap_controller.is_some()
    {
        return Err(Error::Unsupported(
            "--bootstrap-controller requires controller-listener bootstrap, which the current native client does not expose"
                .into(),
        ));
    }
    let is_streams_application_reset = matches!(&cli.command, Command::StreamsApplicationReset(_));
    let bootstrap = cli
        .bootstrap_server
        .as_deref()
        .or_else(|| is_streams_application_reset.then_some("localhost:9092"))
        .ok_or_else(|| {
            Error::Usage(
                "--bootstrap-server is required (or set KAFKA_CLI_BOOTSTRAP_SERVER)".into(),
            )
        })?;
    let command_config = match &cli.command {
        Command::StreamsApplicationReset(args) => {
            args.config_file.as_ref().or(cli.command_config.as_ref())
        }
        _ => cli.command_config.as_ref(),
    }
    .cloned();
    let client_config = config::client_config(bootstrap, command_config.as_deref())?;
    let timeout = cli.timeout();
    let format = cli.output;
    let verbose = cli.verbose > 0;

    match cli.command {
        Command::Topics(args) => topics(&client_config, timeout, format, args.action).await,
        Command::Produce(args) => produce(client_config, args).await,
        Command::Consume(args) => consume(client_config, timeout, args).await,
        Command::ShareConsume(args) => {
            Box::pin(share_consume(
                bootstrap,
                command_config.as_deref(),
                timeout,
                args,
            ))
            .await
        }
        Command::Groups(args) => {
            Box::pin(groups(
                &client_config,
                bootstrap,
                command_config.as_deref(),
                args.timeout(timeout),
                format,
                args.action,
                verbose,
            ))
            .await
        }
        Command::AllGroups(args) => {
            list_all_groups(
                bootstrap,
                command_config.as_deref(),
                timeout,
                format,
                args.action,
            )
            .await
        }
        Command::ShareGroups(args) => {
            Box::pin(share_groups(
                &client_config,
                bootstrap,
                command_config.as_deref(),
                Duration::from_millis(args.timeout_ms),
                format,
                args.action,
                verbose,
            ))
            .await
        }
        Command::StreamsGroups(args) => {
            Box::pin(streams_groups(
                &client_config,
                bootstrap,
                command_config.as_deref(),
                Duration::from_millis(args.timeout_ms),
                format,
                args.action,
                verbose,
            ))
            .await
        }
        Command::StreamsApplicationReset(args) => {
            Box::pin(streams_application_reset(
                &client_config,
                bootstrap,
                command_config.as_deref(),
                timeout,
                format,
                &args,
            ))
            .await
        }
        Command::Configs(args) => {
            Box::pin(configs(
                &client_config,
                bootstrap,
                command_config.as_deref(),
                timeout,
                format,
                args.action,
            ))
            .await
        }
        Command::Offsets(args) => {
            offsets(
                &client_config,
                bootstrap,
                command_config.as_deref(),
                timeout,
                format,
                &args,
            )
            .await
        }
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
        Command::ClientMetrics(args) => {
            client_metrics(
                bootstrap,
                command_config.as_deref(),
                timeout,
                format,
                args.action,
            )
            .await
        }
        Command::Features(args) => {
            features(
                bootstrap,
                command_config.as_deref(),
                timeout,
                format,
                &args.action,
            )
            .await
        }
        Command::Transactions(args) => {
            transactions(
                &client_config,
                bootstrap,
                command_config.as_deref(),
                timeout,
                format,
                &args.action,
            )
            .await
        }
        Command::MetadataQuorum(args) => {
            Box::pin(metadata_quorum(
                bootstrap,
                command_config.as_deref(),
                timeout,
                format,
                &args.action,
            ))
            .await
        }
        Command::DelegationTokens(args) => {
            Box::pin(delegation_tokens(
                bootstrap,
                command_config.as_deref(),
                timeout,
                format,
                &args.action,
            ))
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

async fn share_consume(
    bootstrap: &str,
    command_config: Option<&Path>,
    request_timeout: Duration,
    args: ShareConsumeArgs,
) -> Result<()> {
    let formatter = message_formatter_options(&args)?;
    let mut properties = match command_config {
        Some(path) => config::load_properties(path)?,
        None => std::collections::HashMap::new(),
    };
    for (key, value) in parse_pairs(args.properties())? {
        properties.insert(key, value);
    }

    let configured_group = properties.get("group.id").map(String::as_str);
    if let (Some(argument), Some(configured)) = (args.group.as_deref(), configured_group)
        && argument != configured
    {
        return Err(Error::Usage(format!(
            "group ids supplied by --group and consumer properties must match: '{argument}', '{configured}'"
        )));
    }
    let group = args
        .group
        .as_deref()
        .or(configured_group)
        .unwrap_or("console-share-consumer");
    let client_id = properties
        .get("client.id")
        .map_or("console-share-consumer", String::as_str);

    let mut builder = ShareConsumer::builder()
        .bootstrap_servers(bootstrap)
        .group_id(group)
        .client_id(client_id)
        .acknowledgement_mode(AcknowledgementMode::Explicit)
        .request_timeout(
            share_duration_property(&properties, "request.timeout.ms")?.unwrap_or(request_timeout),
        );
    if let Some(auth) = config::protocol_auth(&properties)? {
        builder = builder.auth(auth);
    }
    if let Some(value) = share_i32_property(&properties, "max.poll.records")? {
        builder = builder.max_poll_records(value);
    } else if let Some(max_messages) = args.max_messages.filter(|value| *value > 0) {
        builder = builder.max_poll_records(max_messages);
    }
    if let Some(value) = share_i32_property(&properties, "fetch.max.wait.ms")? {
        builder = builder.fetch_max_wait_ms(value);
    }
    if let Some(value) = share_duration_property(&properties, "session.timeout.ms")? {
        builder = builder.session_timeout(value);
    }
    if let Some(value) = share_duration_property(&properties, "heartbeat.interval.ms")? {
        builder = builder.heartbeat_interval(value);
    }
    if let Some(value) = share_duration_property(&properties, "metadata.max.age.ms")? {
        builder = builder.metadata_max_age(value);
    }
    if let Some(value) = share_duration_property(&properties, "metadata.max.idle.ms")? {
        builder = builder.metadata_topic_cache_ttl(value);
    }
    if let Some(rack) = properties.get("client.rack") {
        builder = builder.client_rack(rack);
    }

    let consumer = builder.build().await?;
    consumer.subscribe(&[&args.topic]).await?;
    let acknowledgement = if args.reject {
        ShareAcknowledgeType::Reject
    } else if args.release {
        ShareAcknowledgeType::Release
    } else {
        ShareAcknowledgeType::Accept
    };
    let run_result = consume_share_records(&consumer, &formatter, &args, acknowledgement).await;
    let close_result = consumer.close().await.map_err(Error::from);
    drop(consumer);
    eprintln!(
        "Processed a total of {} messages",
        run_result.as_ref().map_or(0, |count| *count)
    );
    if args.enable_systest_events {
        println!("shutdown_complete");
    }
    run_result.map(|_| ()).and(close_result)
}

async fn consume_share_records(
    consumer: &ShareConsumer,
    formatter: &MessageFormatterOptions,
    args: &ShareConsumeArgs,
    acknowledgement: ShareAcknowledgeType,
) -> Result<i64> {
    let mut received = 0_i64;
    let mut idle_since = Instant::now();
    while should_consume_more(args.max_messages, received) {
        let finite_idle_timeout = args
            .timeout_ms
            .filter(|timeout| *timeout != u64::MAX)
            .map(Duration::from_millis);
        let poll_timeout = finite_idle_timeout.map_or(Duration::from_secs(1), |timeout| {
            timeout
                .saturating_sub(idle_since.elapsed())
                .min(Duration::from_secs(1))
        });
        let records = tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            result = consumer.poll(poll_timeout) => result?,
        };
        if records.is_empty() {
            if finite_idle_timeout.is_some_and(|timeout| idle_since.elapsed() >= timeout) {
                break;
            }
            continue;
        }
        idle_since = Instant::now();
        for record in records {
            if !should_consume_more(args.max_messages, received) {
                break;
            }
            let written = if args.json {
                write_share_json(&record)
            } else {
                write_formatted_share_message(&record, formatter)
            };
            match written {
                Ok(()) => consumer.acknowledge(&record, acknowledgement).await?,
                Err(error) if args.reject_message_on_error => {
                    eprintln!("error processing message, rejecting it: {error}");
                    consumer
                        .acknowledge(&record, ShareAcknowledgeType::Reject)
                        .await?;
                }
                Err(error) => return Err(error),
            }
            received += 1;
        }
        consumer.commit_sync().await?;
    }
    Ok(received)
}

fn share_i32_property(
    properties: &std::collections::HashMap<String, String>,
    key: &str,
) -> Result<Option<i32>> {
    properties
        .get(key)
        .map(|value| {
            value
                .parse::<i32>()
                .map_err(|error| Error::Config(format!("invalid {key} value '{value}': {error}")))
        })
        .transpose()
}

fn share_duration_property(
    properties: &std::collections::HashMap<String, String>,
    key: &str,
) -> Result<Option<Duration>> {
    properties
        .get(key)
        .map(|value| {
            value
                .parse::<u64>()
                .map(Duration::from_millis)
                .map_err(|error| Error::Config(format!("invalid {key} value '{value}': {error}")))
        })
        .transpose()
}

fn write_share_json(record: &krafka::consumer::ConsumerRecord) -> Result<()> {
    let headers = record
        .headers
        .iter()
        .map(|(key, value)| {
            (
                String::from_utf8_lossy(key).into_owned(),
                value
                    .as_deref()
                    .map(|value| String::from_utf8_lossy(value).into_owned()),
            )
        })
        .collect();
    output::write_json_line(&ConsumedRecord {
        topic: &record.topic,
        partition: record.partition,
        offset: record.offset,
        timestamp: Some(record.timestamp),
        key: record
            .key
            .as_deref()
            .map(|key| String::from_utf8_lossy(key).into_owned()),
        value: record
            .value
            .as_deref()
            .map(|value| String::from_utf8_lossy(value).into_owned()),
        headers,
    })
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

trait FormatterArgs {
    fn formatter(&self) -> &str;
    fn key_deserializer(&self) -> Option<&str>;
    fn value_deserializer(&self) -> Option<&str>;
    fn print_key(&self) -> bool;
    fn key_separator(&self) -> &str;
    fn formatter_config(&self) -> Option<&Path>;
    fn formatter_properties(&self) -> &[String];
}

impl FormatterArgs for crate::cli::ConsumeArgs {
    fn formatter(&self) -> &str {
        &self.formatter
    }

    fn key_deserializer(&self) -> Option<&str> {
        self.key_deserializer.as_deref()
    }

    fn value_deserializer(&self) -> Option<&str> {
        self.value_deserializer.as_deref()
    }

    fn print_key(&self) -> bool {
        self.print_key
    }

    fn key_separator(&self) -> &str {
        &self.key_separator
    }

    fn formatter_config(&self) -> Option<&Path> {
        self.formatter_config.as_deref()
    }

    fn formatter_properties(&self) -> &[String] {
        self.formatter_properties()
    }
}

impl FormatterArgs for ShareConsumeArgs {
    fn formatter(&self) -> &str {
        &self.formatter
    }

    fn key_deserializer(&self) -> Option<&str> {
        self.key_deserializer.as_deref()
    }

    fn value_deserializer(&self) -> Option<&str> {
        self.value_deserializer.as_deref()
    }

    fn print_key(&self) -> bool {
        self.print_key
    }

    fn key_separator(&self) -> &str {
        &self.key_separator
    }

    fn formatter_config(&self) -> Option<&Path> {
        self.formatter_config.as_deref()
    }

    fn formatter_properties(&self) -> &[String] {
        self.formatter_properties()
    }
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

fn message_formatter_options(args: &impl FormatterArgs) -> Result<MessageFormatterOptions> {
    if args.formatter() != "org.apache.kafka.tools.consumer.DefaultMessageFormatter" {
        return Err(Error::Unsupported(format!(
            "Java formatter class {} cannot be loaded by the native client",
            args.formatter()
        )));
    }
    let properties = component_properties(args.formatter_config(), args.formatter_properties())?;
    let value = |key: &str| properties.get(key);
    let key_deserializer = native_deserializer(
        value("key.deserializer")
            .map(String::as_str)
            .or_else(|| args.key_deserializer()),
        "key",
    )?;
    let value_deserializer = native_deserializer(
        value("value.deserializer")
            .map(String::as_str)
            .or_else(|| args.value_deserializer()),
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
        print_key: flag("print.key", args.print_key()),
        print_value: flag("print.value", true),
        key_separator: value("key.separator")
            .map_or_else(|| args.key_separator().as_bytes(), String::as_bytes)
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
        fields.push(formatted_leader_epoch(ffi::message_leader_epoch(message)));
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

fn write_formatted_share_message(
    record: &krafka::consumer::ConsumerRecord,
    options: &MessageFormatterOptions,
) -> Result<()> {
    let mut fields = Vec::<Vec<u8>>::new();
    if options.print_timestamp {
        let kind = if record.timestamp_type == 1 {
            "LogAppendTime"
        } else {
            "CreateTime"
        };
        fields.push(format!("{kind}:{}", record.timestamp).into_bytes());
    }
    if options.print_partition {
        fields.push(format!("Partition:{}", record.partition).into_bytes());
    }
    if options.print_offset {
        fields.push(format!("Offset:{}", record.offset).into_bytes());
    }
    if options.print_delivery {
        fields.push(record.delivery_count.map_or_else(
            || b"Delivery:NOT_PRESENT".to_vec(),
            |count| format!("Delivery:{count}").into_bytes(),
        ));
    }
    if options.print_epoch {
        fields.push(formatted_leader_epoch(record.leader_epoch));
    }
    if options.print_headers {
        fields.push(formatted_share_headers(&record.headers, options));
    }
    if options.print_key {
        fields.push(deserialize_for_display(
            record.key.as_deref(),
            options.key_deserializer,
            &options.null_literal,
        ));
    }
    if options.print_value {
        fields.push(deserialize_for_display(
            record.value.as_deref(),
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

fn formatted_leader_epoch(epoch: Option<i32>) -> Vec<u8> {
    epoch.map_or_else(
        || b"Epoch:NOT_PRESENT".to_vec(),
        |epoch| format!("Epoch:{epoch}").into_bytes(),
    )
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

fn formatted_share_headers(
    headers: &[(bytes::Bytes, Option<bytes::Bytes>)],
    options: &MessageFormatterOptions,
) -> Vec<u8> {
    if headers.is_empty() {
        return b"NO_HEADERS".to_vec();
    }
    let mut result = Vec::new();
    for (index, (key, value)) in headers.iter().enumerate() {
        if index > 0 {
            result.extend_from_slice(&options.headers_separator);
        }
        result.extend_from_slice(key);
        result.push(b':');
        result.extend_from_slice(&deserialize_for_display(
            value.as_deref(),
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
    bootstrap: &str,
    command_config: Option<&Path>,
    timeout: Duration,
    format: OutputFormat,
    action: GroupAction,
    verbose: bool,
) -> Result<()> {
    match action {
        GroupAction::ValidateRegex { .. } => Err(Error::Usage(
            "validate-regex must be handled before client configuration".into(),
        )),
        GroupAction::List { state, group_type } => {
            list_groups(
                config,
                bootstrap,
                command_config,
                timeout,
                format,
                state.as_deref(),
                group_type.as_deref(),
            )
            .await
        }
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
            let protocol = GroupProtocolContext {
                bootstrap,
                command_config,
            };
            describe_group_details(config, protocol, timeout, format, &groups, mode, verbose).await
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

#[derive(Debug, Clone, Serialize)]
struct AllGroupRow {
    group: String,
    group_type: String,
    protocol: String,
}

async fn list_all_groups(
    bootstrap: &str,
    command_config: Option<&Path>,
    timeout: Duration,
    format: OutputFormat,
    action: AllGroupsAction,
) -> Result<()> {
    let AllGroupsAction::List {
        group_type,
        protocol,
        consumer,
        share,
        streams,
    } = action;
    let client = config::protocol_admin(bootstrap, timeout, command_config).await?;
    let groups = client.list_consumer_groups().await?;
    drop(client);
    let mut rows = groups
        .into_iter()
        .map(|group| AllGroupRow {
            group: group.group_id,
            group_type: group
                .group_type
                .as_ref()
                .map_or_else(String::new, |kind| group_type_label(&kind.to_string())),
            protocol: group.protocol_type,
        })
        .filter(|row| {
            all_group_matches(
                row,
                group_type,
                protocol.as_deref(),
                consumer,
                share,
                streams,
            )
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| left.group.cmp(&right.group));
    output::write_value(format, "all-groups.list", &rows, |rows| {
        output::table(
            ["GROUP", "TYPE", "PROTOCOL"],
            rows.iter().map(|row| {
                [
                    row.group.clone(),
                    row.group_type.clone(),
                    row.protocol.clone(),
                ]
            }),
        )
    })
}

fn all_group_matches(
    row: &AllGroupRow,
    group_type: Option<AllGroupType>,
    protocol: Option<&str>,
    consumer: bool,
    share: bool,
    streams: bool,
) -> bool {
    let type_matches = group_type.is_none_or(|requested| {
        row.group_type.eq_ignore_ascii_case(match requested {
            AllGroupType::Classic => "classic",
            AllGroupType::Consumer => "consumer",
            AllGroupType::Share => "share",
            AllGroupType::Streams => "streams",
        })
    });
    let protocol_matches = protocol.is_none_or(|requested| row.protocol == requested);
    if group_type.is_some() || protocol.is_some() {
        return type_matches && protocol_matches;
    }
    if consumer {
        return row.protocol == "consumer"
            || row.protocol.is_empty()
            || row.group_type.eq_ignore_ascii_case("consumer");
    }
    if share {
        return row.group_type.eq_ignore_ascii_case("share");
    }
    if streams {
        return row.group_type.eq_ignore_ascii_case("streams");
    }
    true
}

fn group_type_label(group_type: &str) -> String {
    match group_type.to_ascii_lowercase().as_str() {
        "classic" => "Classic".into(),
        "consumer" => "Consumer".into(),
        "share" => "Share".into(),
        "streams" => "Streams".into(),
        _ => group_type.to_owned(),
    }
}

#[derive(Debug)]
struct ShareGroupDescriptionWithCoordinator {
    coordinator_id: i32,
    coordinator: String,
    description: krafka::protocol::ShareGroupDescription,
}

#[derive(Debug, Serialize)]
struct ShareGroupListRow {
    group: String,
    state: Option<String>,
}

#[derive(Debug)]
struct StreamsGroupDescriptionWithCoordinator {
    coordinator_id: i32,
    coordinator: String,
    description: StreamsGroupDescription,
}

#[derive(Debug)]
struct StreamsGroupDescription {
    group_id: String,
    group_state: String,
    group_epoch: i32,
    assignment_epoch: i32,
    topology: Option<StreamsTopology>,
    members: Vec<StreamsGroupMember>,
    topology_description: Option<StreamsTopologyDescription>,
    topology_description_status: Option<i8>,
    assignor: Option<String>,
}

#[derive(Debug)]
struct StreamsTopology {
    subtopologies: Vec<StreamsSubtopology>,
}

#[derive(Debug)]
struct StreamsSubtopology {
    id: String,
    source_topics: Vec<String>,
    repartition_source_topics: Vec<String>,
    state_changelog_topics: Vec<String>,
}

#[derive(Debug)]
struct StreamsGroupMember {
    member_id: String,
    member_epoch: i32,
    client_id: String,
    topology_epoch: i32,
    process_id: String,
    assignment: StreamsTaskAssignment,
    target_assignment: StreamsTaskAssignment,
    is_classic: bool,
}

#[derive(Debug, Default)]
struct StreamsTaskAssignment {
    active: Vec<StreamsTaskIds>,
    standby: Vec<StreamsTaskIds>,
    warmup: Vec<StreamsTaskIds>,
}

#[derive(Debug)]
struct StreamsTaskIds {
    subtopology_id: String,
    partitions: Vec<i32>,
}

#[derive(Debug)]
struct StreamsTopologyDescription {
    subtopologies: Vec<StreamsTopologyDescriptionSubtopology>,
    global_stores: Vec<StreamsTopologyGlobalStore>,
}

#[derive(Debug)]
struct StreamsTopologyDescriptionSubtopology {
    id: String,
    nodes: Vec<StreamsTopologyNode>,
}

#[derive(Debug)]
struct StreamsTopologyGlobalStore {
    source: StreamsTopologyNode,
    processor: StreamsTopologyNode,
}

#[derive(Debug)]
struct StreamsTopologyNode {
    name: String,
    node_type: i8,
    source_topics: Vec<String>,
    sink_topic: Option<String>,
    stores: Vec<String>,
    successors: Vec<String>,
}

#[derive(Debug, Serialize)]
struct StreamsGroupStateRow {
    group: String,
    coordinator: String,
    coordinator_id: i32,
    assignor: String,
    state: String,
    group_epoch: Option<i32>,
    assignment_epoch: Option<i32>,
    members: usize,
}

#[derive(Debug, Serialize)]
struct StreamsGroupMemberRow {
    group: String,
    member: String,
    process: String,
    client_id: String,
    assignments: String,
    member_protocol: Option<String>,
    member_epoch: Option<i32>,
    topology_epoch: Option<i32>,
    assignment_epoch: Option<i32>,
}

#[derive(Debug, Serialize)]
struct StreamsGroupOffsetRow {
    group: String,
    topic: String,
    partition: i32,
    current_offset: Option<i64>,
    leader_epoch: Option<i32>,
    log_end_offset: i64,
    lag: i64,
}

#[derive(Debug, Serialize)]
struct StreamsTopologyRow {
    group: String,
    scope: String,
    topology: String,
    node: String,
    node_type: String,
    source_topics: String,
    sink_topic: Option<String>,
    stores: String,
    successors: String,
}

async fn share_group_ids(client: &krafka::admin::AdminClient) -> Result<Vec<String>> {
    group_ids_by_type(client, "share").await
}

async fn group_ids_by_type(
    client: &krafka::admin::AdminClient,
    expected_type: &str,
) -> Result<Vec<String>> {
    let mut groups = client
        .list_consumer_groups()
        .await?
        .into_iter()
        .filter(|group| {
            group
                .group_type
                .as_ref()
                .is_some_and(|kind| kind.to_string().eq_ignore_ascii_case(expected_type))
        })
        .map(|group| group.group_id)
        .collect::<Vec<_>>();
    groups.sort();
    Ok(groups)
}

async fn list_streams_groups(
    client: &krafka::admin::AdminClient,
    format: OutputFormat,
    state: Option<&str>,
) -> Result<()> {
    let group_ids = group_ids_by_type(client, "streams").await?;
    let rows = if let Some(state_filter) = state {
        let states = if state_filter.is_empty() {
            BTreeSet::new()
        } else {
            parse_streams_group_states(state_filter)?
        };
        describe_streams_groups(client, &group_ids, false, false)
            .await?
            .into_iter()
            .filter(|group| states.is_empty() || states.contains(&group.description.group_state))
            .map(|group| ShareGroupListRow {
                group: group.description.group_id,
                state: Some(group.description.group_state),
            })
            .collect::<Vec<_>>()
    } else {
        group_ids
            .into_iter()
            .map(|group| ShareGroupListRow { group, state: None })
            .collect()
    };
    output::write_value(format, "streams-groups.list", &rows, |rows| {
        if rows.iter().any(|row| row.state.is_some()) {
            output::table(
                ["GROUP", "STATE"],
                rows.iter().map(|row| {
                    [
                        row.group.clone(),
                        row.state.as_deref().unwrap_or("-").to_owned(),
                    ]
                }),
            )
        } else {
            output::table(["GROUP"], rows.iter().map(|row| [row.group.clone()]))
        }
    })
}

fn parse_streams_group_states(value: &str) -> Result<BTreeSet<String>> {
    value
        .split(',')
        .map(str::trim)
        .map(|state| match state.to_ascii_lowercase().as_str() {
            "empty" => Ok("Empty".into()),
            "notready" => Ok("NotReady".into()),
            "stable" => Ok("Stable".into()),
            "assigning" => Ok("Assigning".into()),
            "reconciling" => Ok("Reconciling".into()),
            "dead" => Ok("Dead".into()),
            _ => Err(Error::Usage(format!(
                "invalid Streams group state '{state}'; expected Empty, NotReady, Stable, Assigning, Reconciling, or Dead"
            ))),
        })
        .collect()
}

fn streams_tasks(tasks: &[StreamsTaskIds], label: &str) -> String {
    tasks
        .iter()
        .map(|task| {
            let partitions = task
                .partitions
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            format!("{label}: {}:[{partitions}]", task.subtopology_id)
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn streams_assignment(
    assignment: &StreamsTaskAssignment,
    target: Option<&StreamsTaskAssignment>,
) -> String {
    let mut values = [
        streams_tasks(&assignment.active, "ACTIVE"),
        streams_tasks(&assignment.standby, "STANDBY"),
        streams_tasks(&assignment.warmup, "WARMUP"),
    ]
    .into_iter()
    .filter(|value| !value.is_empty())
    .collect::<Vec<_>>();
    if let Some(target) = target {
        values.extend(
            [
                streams_tasks(&target.active, "TARGET-ACTIVE"),
                streams_tasks(&target.standby, "TARGET-STANDBY"),
                streams_tasks(&target.warmup, "TARGET-WARMUP"),
            ]
            .into_iter()
            .filter(|value| !value.is_empty()),
        );
    }
    values.join("; ")
}

fn write_streams_group_states(
    format: OutputFormat,
    descriptions: Vec<StreamsGroupDescriptionWithCoordinator>,
    verbose: bool,
) -> Result<()> {
    let rows = descriptions
        .into_iter()
        .map(|group| StreamsGroupStateRow {
            group: group.description.group_id,
            coordinator: group.coordinator,
            coordinator_id: group.coordinator_id,
            assignor: group.description.assignor.unwrap_or_default(),
            state: group.description.group_state,
            group_epoch: verbose.then_some(group.description.group_epoch),
            assignment_epoch: verbose.then_some(group.description.assignment_epoch),
            members: group.description.members.len(),
        })
        .collect::<Vec<_>>();
    output::write_value(format, "streams-groups.describe.state", &rows, |rows| {
        if verbose {
            output::table(
                [
                    "GROUP",
                    "COORDINATOR (ID)",
                    "ASSIGNOR",
                    "STATE",
                    "GROUP-EPOCH",
                    "TARGET-ASSIGNMENT-EPOCH",
                    "#MEMBERS",
                ],
                rows.iter().map(|row| {
                    [
                        row.group.clone(),
                        format!("{} ({})", row.coordinator, row.coordinator_id),
                        row.assignor.clone(),
                        row.state.clone(),
                        row.group_epoch
                            .map_or_else(|| "-".into(), |v| v.to_string()),
                        row.assignment_epoch
                            .map_or_else(|| "-".into(), |v| v.to_string()),
                        row.members.to_string(),
                    ]
                }),
            )
        } else {
            output::table(
                ["GROUP", "COORDINATOR (ID)", "ASSIGNOR", "STATE", "#MEMBERS"],
                rows.iter().map(|row| {
                    [
                        row.group.clone(),
                        format!("{} ({})", row.coordinator, row.coordinator_id),
                        row.assignor.clone(),
                        row.state.clone(),
                        row.members.to_string(),
                    ]
                }),
            )
        }
    })
}

fn write_streams_group_members(
    format: OutputFormat,
    descriptions: Vec<StreamsGroupDescriptionWithCoordinator>,
    verbose: bool,
) -> Result<()> {
    let rows = descriptions
        .into_iter()
        .flat_map(|group| {
            let group_id = group.description.group_id;
            let assignment_epoch = group.description.assignment_epoch;
            group.description.members.into_iter().map(move |member| {
                let target = verbose.then_some(&member.target_assignment);
                StreamsGroupMemberRow {
                    group: group_id.clone(),
                    member: member.member_id,
                    process: member.process_id,
                    client_id: member.client_id,
                    assignments: streams_assignment(&member.assignment, target),
                    member_protocol: verbose.then(|| {
                        if member.is_classic {
                            "classic"
                        } else {
                            "streams"
                        }
                        .into()
                    }),
                    member_epoch: verbose.then_some(member.member_epoch),
                    topology_epoch: verbose.then_some(member.topology_epoch),
                    assignment_epoch: verbose.then_some(assignment_epoch),
                }
            })
        })
        .collect::<Vec<_>>();
    output::write_value(format, "streams-groups.describe.members", &rows, |rows| {
        if verbose {
            output::table(
                [
                    "GROUP",
                    "TARGET-ASSIGNMENT-EPOCH",
                    "TOPOLOGY-EPOCH",
                    "MEMBER",
                    "MEMBER-PROTOCOL",
                    "MEMBER-EPOCH",
                    "PROCESS",
                    "CLIENT-ID",
                    "ASSIGNMENTS",
                ],
                rows.iter().map(|row| {
                    [
                        row.group.clone(),
                        row.assignment_epoch
                            .map_or_else(|| "-".into(), |v| v.to_string()),
                        row.topology_epoch
                            .map_or_else(|| "-".into(), |v| v.to_string()),
                        row.member.clone(),
                        row.member_protocol.as_deref().unwrap_or("-").into(),
                        row.member_epoch
                            .map_or_else(|| "-".into(), |v| v.to_string()),
                        row.process.clone(),
                        row.client_id.clone(),
                        row.assignments.clone(),
                    ]
                }),
            )
        } else {
            output::table(
                ["GROUP", "MEMBER", "PROCESS", "CLIENT-ID", "ASSIGNMENTS"],
                rows.iter().map(|row| {
                    [
                        row.group.clone(),
                        row.member.clone(),
                        row.process.clone(),
                        row.client_id.clone(),
                        row.assignments.clone(),
                    ]
                }),
            )
        }
    })
}

fn streams_active_partitions(description: &StreamsGroupDescription) -> BTreeSet<(String, i32)> {
    let Some(topology) = &description.topology else {
        return BTreeSet::new();
    };
    let source_topics = topology
        .subtopologies
        .iter()
        .map(|subtopology| (&subtopology.id, &subtopology.source_topics))
        .collect::<BTreeMap<_, _>>();
    description
        .members
        .iter()
        .flat_map(|member| &member.assignment.active)
        .flat_map(|task| {
            source_topics
                .get(&task.subtopology_id)
                .into_iter()
                .flat_map(move |topics| {
                    topics.iter().flat_map(move |topic| {
                        task.partitions
                            .iter()
                            .map(move |partition| (topic.clone(), *partition))
                    })
                })
        })
        .collect()
}

fn streams_internal_topics(description: &StreamsGroupDescription) -> BTreeSet<String> {
    let Some(topology) = &description.topology else {
        return BTreeSet::new();
    };
    let sources = topology
        .subtopologies
        .iter()
        .flat_map(|subtopology| &subtopology.source_topics)
        .collect::<BTreeSet<_>>();
    topology
        .subtopologies
        .iter()
        .flat_map(|subtopology| {
            subtopology
                .repartition_source_topics
                .iter()
                .chain(&subtopology.state_changelog_topics)
        })
        .filter(|topic| !sources.contains(topic))
        .filter(|topic| {
            topic.starts_with(&format!("{}-", description.group_id))
                && (topic.ends_with("-repartition") || topic.ends_with("-changelog"))
        })
        .cloned()
        .collect()
}

fn write_streams_group_offsets(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    format: OutputFormat,
    descriptions: Vec<StreamsGroupDescriptionWithCoordinator>,
    verbose: bool,
) -> Result<()> {
    let admin = admin(config)?;
    let committed = descriptions
        .iter()
        .map(|group| {
            ffi::list_consumer_group_offsets(
                admin.inner().native_ptr(),
                &group.description.group_id,
                duration_ms(timeout)?,
            )
            .map(|offsets| (group.description.group_id.clone(), offsets))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    drop(admin);
    let consumer = base_consumer(config)?;
    let mut rows = Vec::new();
    for group in descriptions {
        let committed = committed
            .get(&group.description.group_id)
            .into_iter()
            .flatten()
            .map(|offset| ((offset.topic.as_str(), offset.partition), offset))
            .collect::<BTreeMap<_, _>>();
        for (topic, partition) in streams_active_partitions(&group.description) {
            let (low, high) = consumer.fetch_watermarks(&topic, partition, timeout)?;
            let offset = committed.get(&(topic.as_str(), partition));
            let current_offset = offset
                .filter(|entry| entry.offset >= 0)
                .map(|entry| entry.offset);
            let leader_epoch = offset.and_then(|entry| entry.leader_epoch);
            let lag = high.saturating_sub(current_offset.unwrap_or(low));
            rows.push(StreamsGroupOffsetRow {
                group: group.description.group_id.clone(),
                topic,
                partition,
                current_offset,
                leader_epoch,
                log_end_offset: high,
                lag,
            });
        }
    }
    output::write_value(format, "streams-groups.describe.offsets", &rows, |rows| {
        if verbose {
            output::table(
                [
                    "GROUP",
                    "TOPIC",
                    "PARTITION",
                    "CURRENT-OFFSET",
                    "LEADER-EPOCH",
                    "LOG-END-OFFSET",
                    "OFFSET-LAG",
                ],
                rows.iter().map(|row| {
                    [
                        row.group.clone(),
                        row.topic.clone(),
                        row.partition.to_string(),
                        row.current_offset
                            .map_or_else(|| "-".into(), |v| v.to_string()),
                        row.leader_epoch
                            .map_or_else(|| "-".into(), |v| v.to_string()),
                        row.log_end_offset.to_string(),
                        row.lag.to_string(),
                    ]
                }),
            )
        } else {
            output::table(
                ["GROUP", "TOPIC", "PARTITION", "OFFSET-LAG"],
                rows.iter().map(|row| {
                    [
                        row.group.clone(),
                        row.topic.clone(),
                        row.partition.to_string(),
                        row.lag.to_string(),
                    ]
                }),
            )
        }
    })
}

fn streams_topology_node_row(
    group: &str,
    scope: &str,
    topology: &str,
    node: StreamsTopologyNode,
) -> StreamsTopologyRow {
    StreamsTopologyRow {
        group: group.into(),
        scope: scope.into(),
        topology: topology.into(),
        node: node.name,
        node_type: match node.node_type {
            1 => "SOURCE",
            2 => "PROCESSOR",
            3 => "SINK",
            _ => "UNKNOWN",
        }
        .into(),
        source_topics: node.source_topics.join(","),
        sink_topic: node.sink_topic,
        stores: node.stores.join(","),
        successors: node.successors.join(","),
    }
}

fn write_streams_topologies(
    format: OutputFormat,
    descriptions: Vec<StreamsGroupDescriptionWithCoordinator>,
) -> Result<()> {
    let mut rows = Vec::new();
    let mut errors = Vec::new();
    for group in descriptions {
        let group_id = group.description.group_id;
        match (
            group.description.topology_description_status,
            group.description.topology_description,
        ) {
            (Some(3), Some(topology)) => {
                for subtopology in topology.subtopologies {
                    rows.extend(subtopology.nodes.into_iter().map(|node| {
                        streams_topology_node_row(
                            &group_id,
                            "SUBTOPOLOGY",
                            &subtopology.id,
                            node,
                        )
                    }));
                }
                for (index, store) in topology.global_stores.into_iter().enumerate() {
                    let id = index.to_string();
                    rows.push(streams_topology_node_row(
                        &group_id,
                        "GLOBAL-SOURCE",
                        &id,
                        store.source,
                    ));
                    rows.push(streams_topology_node_row(
                        &group_id,
                        "GLOBAL-PROCESSOR",
                        &id,
                        store.processor,
                    ));
                }
            }
            (Some(1), _) => errors.push(format!(
                "no topology description is stored for Streams group '{group_id}'"
            )),
            (Some(2), _) => errors.push(format!(
                "broker failed to fetch the topology description for Streams group '{group_id}'"
            )),
            (status, _) => errors.push(format!(
                "no topology description is available for Streams group '{group_id}' (status: {status:?})"
            )),
        }
    }
    output::write_value_with_errors(
        format,
        "streams-groups.describe.topology",
        &rows,
        &errors,
        |rows| {
            output::table(
                [
                    "GROUP",
                    "SCOPE",
                    "TOPOLOGY",
                    "NODE",
                    "TYPE",
                    "SOURCE-TOPICS",
                    "SINK-TOPIC",
                    "STORES",
                    "SUCCESSORS",
                ],
                rows.iter().map(|row| {
                    [
                        row.group.clone(),
                        row.scope.clone(),
                        row.topology.clone(),
                        row.node.clone(),
                        row.node_type.clone(),
                        row.source_topics.clone(),
                        row.sink_topic.as_deref().unwrap_or("-").into(),
                        row.stores.clone(),
                        row.successors.clone(),
                    ]
                }),
            )
        },
    )
}

async fn group_coordinator_connection(
    client: &krafka::admin::AdminClient,
    group_id: &str,
) -> Result<(
    i32,
    String,
    std::sync::Arc<krafka::network::BrokerConnection>,
)> {
    let bootstrap_connection = delegation_broker_connection(client).await?;
    let version = bootstrap_connection
        .negotiate_api_version(ApiKey::FindCoordinator, 6, 1)
        .await
        .ok_or_else(|| Error::Unsupported("broker does not support FindCoordinator".into()))?;
    let request = krafka::protocol::FindCoordinatorRequest::for_group(group_id);
    let mut bytes = bootstrap_connection
        .send_request(ApiKey::FindCoordinator, version, |buffer| {
            request.encode_versioned(version, buffer)
        })
        .await?;
    drop(bootstrap_connection);
    let response =
        krafka::protocol::FindCoordinatorResponse::decode_versioned(version, &mut bytes)?;
    if !response.error_code.is_ok() {
        return Err(Error::Config(format!(
            "FindCoordinator failed for {group_id}: {:?}: {}",
            response.error_code,
            response.error_message.as_deref().unwrap_or("-")
        )));
    }
    let address = format!("{}:{}", response.host, response.port);
    let connection = client
        .pool()
        .get_connection_by_id(response.node_id, &address)
        .await?;
    Ok((response.node_id, address, connection))
}

fn decode_compact_strings(buffer: &mut impl Buf) -> Result<Vec<String>> {
    let count = decode_compact_len(buffer)?;
    (0..count).map(|_| decode_compact_string(buffer)).collect()
}

fn decode_streams_key_values(buffer: &mut impl Buf) -> Result<()> {
    let count = decode_compact_len(buffer)?;
    for _ in 0..count {
        let _key = decode_compact_string(buffer)?;
        let _value = decode_compact_string(buffer)?;
        skip_tagged_fields(buffer)?;
    }
    Ok(())
}

fn decode_streams_topic_infos(buffer: &mut impl Buf) -> Result<Vec<String>> {
    let count = decode_compact_len(buffer)?;
    let mut topics = Vec::with_capacity(count);
    for _ in 0..count {
        topics.push(decode_compact_string(buffer)?);
        let _partitions = i32::decode(buffer)?;
        let _replication_factor = i16::decode(buffer)?;
        decode_streams_key_values(buffer)?;
        skip_tagged_fields(buffer)?;
    }
    Ok(topics)
}

fn decode_streams_subtopologies(buffer: &mut impl Buf) -> Result<Vec<StreamsSubtopology>> {
    let encoded = decode_unsigned_varint(buffer)?;
    if encoded == 0 {
        return Ok(Vec::new());
    }
    let mut subtopologies = Vec::with_capacity(encoded - 1);
    for _ in 0..encoded - 1 {
        let id = decode_compact_string(buffer)?;
        let source_topics = decode_compact_strings(buffer)?;
        let _repartition_sink_topics = decode_compact_strings(buffer)?;
        let state_changelog_topics = decode_streams_topic_infos(buffer)?;
        let repartition_source_topics = decode_streams_topic_infos(buffer)?;
        skip_tagged_fields(buffer)?;
        subtopologies.push(StreamsSubtopology {
            id,
            source_topics,
            repartition_source_topics,
            state_changelog_topics,
        });
    }
    Ok(subtopologies)
}

fn decode_nullable_struct_presence(buffer: &mut impl Buf) -> Result<bool> {
    match i8::decode(buffer)? {
        -1 => Ok(false),
        1 => Ok(true),
        marker => Err(Error::Config(format!(
            "invalid nullable struct presence marker {marker}"
        ))),
    }
}

fn decode_streams_topology(buffer: &mut impl Buf) -> Result<Option<StreamsTopology>> {
    if !decode_nullable_struct_presence(buffer)? {
        return Ok(None);
    }
    let _epoch = i32::decode(buffer)?;
    let subtopologies = decode_streams_subtopologies(buffer)?;
    skip_tagged_fields(buffer)?;
    Ok(Some(StreamsTopology { subtopologies }))
}

fn decode_streams_task_ids(buffer: &mut impl Buf) -> Result<Vec<StreamsTaskIds>> {
    let count = decode_compact_len(buffer)?;
    let mut tasks = Vec::with_capacity(count);
    for _ in 0..count {
        let subtopology_id = decode_compact_string(buffer)?;
        let partition_count = decode_compact_len(buffer)?;
        let partitions = (0..partition_count)
            .map(|_| i32::decode(buffer).map_err(Error::from))
            .collect::<Result<Vec<_>>>()?;
        skip_tagged_fields(buffer)?;
        tasks.push(StreamsTaskIds {
            subtopology_id,
            partitions,
        });
    }
    Ok(tasks)
}

fn decode_streams_assignment(buffer: &mut impl Buf) -> Result<StreamsTaskAssignment> {
    let active = decode_streams_task_ids(buffer)?;
    let standby = decode_streams_task_ids(buffer)?;
    let warmup = decode_streams_task_ids(buffer)?;
    skip_tagged_fields(buffer)?;
    Ok(StreamsTaskAssignment {
        active,
        standby,
        warmup,
    })
}

fn skip_streams_task_offsets(buffer: &mut impl Buf) -> Result<()> {
    let count = decode_compact_len(buffer)?;
    for _ in 0..count {
        let _subtopology_id = decode_compact_string(buffer)?;
        let _partition = i32::decode(buffer)?;
        let _offset = i64::decode(buffer)?;
        skip_tagged_fields(buffer)?;
    }
    Ok(())
}

fn decode_streams_member(buffer: &mut impl Buf) -> Result<StreamsGroupMember> {
    let member_id = decode_compact_string(buffer)?;
    let member_epoch = i32::decode(buffer)?;
    let _instance_id = decode_nullable_compact_string(buffer)?;
    let _rack_id = decode_nullable_compact_string(buffer)?;
    let client_id = decode_compact_string(buffer)?;
    let _client_host = decode_compact_string(buffer)?;
    let topology_epoch = i32::decode(buffer)?;
    let process_id = decode_compact_string(buffer)?;
    if decode_nullable_struct_presence(buffer)? {
        let _host = decode_compact_string(buffer)?;
        let _port = i16::decode(buffer)?.cast_unsigned();
        skip_tagged_fields(buffer)?;
    }
    decode_streams_key_values(buffer)?;
    skip_streams_task_offsets(buffer)?;
    skip_streams_task_offsets(buffer)?;
    let assignment = decode_streams_assignment(buffer)?;
    let target_assignment = decode_streams_assignment(buffer)?;
    let is_classic = bool::decode(buffer)?;
    skip_tagged_fields(buffer)?;
    Ok(StreamsGroupMember {
        member_id,
        member_epoch,
        client_id,
        topology_epoch,
        process_id,
        assignment,
        target_assignment,
        is_classic,
    })
}

fn decode_streams_topology_node(buffer: &mut impl Buf) -> Result<StreamsTopologyNode> {
    let node = StreamsTopologyNode {
        name: decode_compact_string(buffer)?,
        node_type: i8::decode(buffer)?,
        source_topics: decode_compact_strings(buffer)?,
        sink_topic: decode_nullable_compact_string(buffer)?,
        stores: decode_compact_strings(buffer)?,
        successors: decode_compact_strings(buffer)?,
    };
    skip_tagged_fields(buffer)?;
    Ok(node)
}

fn decode_streams_topology_description(
    buffer: &mut impl Buf,
) -> Result<Option<StreamsTopologyDescription>> {
    if !decode_nullable_struct_presence(buffer)? {
        return Ok(None);
    }
    let subtopology_count = decode_compact_len(buffer)?;
    let mut subtopologies = Vec::with_capacity(subtopology_count);
    for _ in 0..subtopology_count {
        let id = decode_compact_string(buffer)?;
        let node_count = decode_compact_len(buffer)?;
        let nodes = (0..node_count)
            .map(|_| decode_streams_topology_node(buffer))
            .collect::<Result<Vec<_>>>()?;
        skip_tagged_fields(buffer)?;
        subtopologies.push(StreamsTopologyDescriptionSubtopology { id, nodes });
    }
    let store_count = decode_compact_len(buffer)?;
    let mut global_stores = Vec::with_capacity(store_count);
    for _ in 0..store_count {
        let source = decode_streams_topology_node(buffer)?;
        let processor = decode_streams_topology_node(buffer)?;
        skip_tagged_fields(buffer)?;
        global_stores.push(StreamsTopologyGlobalStore { source, processor });
    }
    skip_tagged_fields(buffer)?;
    Ok(Some(StreamsTopologyDescription {
        subtopologies,
        global_stores,
    }))
}

fn decode_streams_group_description(
    buffer: &mut impl Buf,
    version: i16,
) -> Result<(i16, Option<String>, StreamsGroupDescription)> {
    let error_code = i16::decode(buffer)?;
    let error_message = decode_nullable_compact_string(buffer)?;
    let group_id = decode_compact_string(buffer)?;
    let group_state = decode_compact_string(buffer)?;
    let group_epoch = i32::decode(buffer)?;
    let assignment_epoch = i32::decode(buffer)?;
    let topology = decode_streams_topology(buffer)?;
    let member_count = decode_compact_len(buffer)?;
    let members = (0..member_count)
        .map(|_| decode_streams_member(buffer))
        .collect::<Result<Vec<_>>>()?;
    let _authorized_operations = i32::decode(buffer)?;
    let (topology_description, topology_description_status, assignor) = if version >= 1 {
        (
            decode_streams_topology_description(buffer)?,
            Some(i8::decode(buffer)?),
            decode_nullable_compact_string(buffer)?,
        )
    } else {
        (None, None, None)
    };
    skip_tagged_fields(buffer)?;
    Ok((
        error_code,
        error_message,
        StreamsGroupDescription {
            group_id,
            group_state,
            group_epoch,
            assignment_epoch,
            topology,
            members,
            topology_description,
            topology_description_status,
            assignor,
        },
    ))
}

fn encode_streams_describe_request(
    group_id: &str,
    version: i16,
    include_topology_description: bool,
    buffer: &mut BytesMut,
) {
    buffer.put_u8(0); // flexible request-header tagged fields
    encode_unsigned_varint(2, buffer);
    encode_compact_string(group_id, buffer);
    buffer.put_u8(0); // include authorized operations
    if version >= 1 {
        buffer.put_u8(u8::from(include_topology_description));
    }
    buffer.put_u8(0); // request tagged fields
}

async fn describe_streams_groups(
    client: &krafka::admin::AdminClient,
    group_ids: &[String],
    include_topology_description: bool,
    allow_missing: bool,
) -> Result<Vec<StreamsGroupDescriptionWithCoordinator>> {
    let mut descriptions = Vec::with_capacity(group_ids.len());
    for group_id in group_ids {
        let (coordinator_id, coordinator, connection) =
            group_coordinator_connection(client, group_id).await?;
        let api_key = ApiKey::Unknown(89);
        let version = connection
            .negotiate_api_version(api_key, i16::from(include_topology_description), 0)
            .await
            .ok_or_else(|| {
                Error::Unsupported("broker does not support StreamsGroupDescribe v0".into())
            })?;
        let mut response = connection
            .send_request(api_key, version, |buffer| {
                encode_streams_describe_request(
                    group_id,
                    version,
                    include_topology_description,
                    buffer,
                );
                Ok(())
            })
            .await?;
        drop(connection);
        skip_tagged_fields(&mut response).map_err(|error| {
            Error::Config(format!(
                "StreamsGroupDescribe response header failed: {error}"
            ))
        })?;
        let _throttle_time_ms = i32::decode(&mut response).map_err(|error| {
            Error::Config(format!("StreamsGroupDescribe throttle failed: {error}"))
        })?;
        let count = decode_compact_len(&mut response).map_err(|error| {
            Error::Config(format!("StreamsGroupDescribe group array failed: {error}"))
        })?;
        if count != 1 {
            return Err(Error::Config(format!(
                "StreamsGroupDescribe returned {count} groups for one requested group"
            )));
        }
        let (error_code, error_message, mut description) =
            decode_streams_group_description(&mut response, version).map_err(|error| {
                Error::Config(format!("StreamsGroupDescribe group decode failed: {error}"))
            })?;
        skip_tagged_fields(&mut response).map_err(|error| {
            Error::Config(format!(
                "StreamsGroupDescribe response tags failed: {error}"
            ))
        })?;
        if error_code == krafka::error::ErrorCode::GroupIdNotFound.to_i16() && allow_missing {
            description.group_state = "Dead".into();
        } else if error_code != 0 {
            return Err(Error::Config(format!(
                "StreamsGroupDescribe failed for {group_id}: {}",
                error_message.unwrap_or_else(|| format!("Kafka error {error_code}"))
            )));
        }
        descriptions.push(StreamsGroupDescriptionWithCoordinator {
            coordinator_id,
            coordinator,
            description,
        });
    }
    descriptions.sort_by(|left, right| left.description.group_id.cmp(&right.description.group_id));
    Ok(descriptions)
}

async fn describe_share_groups(
    client: &krafka::admin::AdminClient,
    group_ids: &[String],
) -> Result<Vec<ShareGroupDescriptionWithCoordinator>> {
    let mut descriptions = Vec::with_capacity(group_ids.len());
    for group_id in group_ids {
        let (coordinator_id, coordinator, connection) =
            group_coordinator_connection(client, group_id).await?;
        let version = connection
            .negotiate_api_version(ApiKey::ShareGroupDescribe, 1, 1)
            .await
            .ok_or_else(|| {
                Error::Unsupported("broker does not support ShareGroupDescribe v1".into())
            })?;
        let request = krafka::protocol::ShareGroupDescribeRequest {
            group_ids: vec![group_id.clone()],
            include_authorized_operations: false,
        };
        let mut bytes = connection
            .send_request(ApiKey::ShareGroupDescribe, version, |buffer| {
                request.encode_versioned(version, buffer)
            })
            .await?;
        drop(connection);
        let response =
            krafka::protocol::ShareGroupDescribeResponse::decode_versioned(version, &mut bytes)?;
        let description = response
            .groups
            .into_iter()
            .next()
            .ok_or_else(|| Error::Config(format!("broker omitted Share group {group_id}")))?;
        if !description.error_code.is_ok() {
            return Err(Error::Config(format!(
                "ShareGroupDescribe failed for {group_id}: {:?}: {}",
                description.error_code,
                description.error_message.as_deref().unwrap_or("-")
            )));
        }
        descriptions.push(ShareGroupDescriptionWithCoordinator {
            coordinator_id,
            coordinator,
            description,
        });
    }
    descriptions.sort_by(|left, right| left.description.group_id.cmp(&right.description.group_id));
    Ok(descriptions)
}

fn parse_share_group_states(value: &str) -> Result<BTreeSet<String>> {
    value
        .split(',')
        .map(str::trim)
        .map(|state| match state.to_ascii_lowercase().as_str() {
            "empty" => Ok("Empty".into()),
            "stable" => Ok("Stable".into()),
            "dead" => Ok("Dead".into()),
            _ => Err(Error::Usage(format!(
                "invalid Share group state '{state}'; expected Empty, Stable, or Dead"
            ))),
        })
        .collect()
}

async fn list_share_groups(
    client: &krafka::admin::AdminClient,
    format: OutputFormat,
    state: Option<&str>,
) -> Result<()> {
    let group_ids = share_group_ids(client).await?;
    let rows: Vec<ShareGroupListRow> = if let Some(state_filter) = state {
        let states = if state_filter.is_empty() {
            BTreeSet::new()
        } else {
            parse_share_group_states(state_filter)?
        };
        describe_share_groups(client, &group_ids)
            .await?
            .into_iter()
            .filter(|group| states.is_empty() || states.contains(&group.description.group_state))
            .map(|group| ShareGroupListRow {
                group: group.description.group_id,
                state: Some(group.description.group_state),
            })
            .collect()
    } else {
        group_ids
            .into_iter()
            .map(|group| ShareGroupListRow { group, state: None })
            .collect()
    };
    output::write_value(format, "share-groups.list", &rows, |rows| {
        if rows.iter().any(|row| row.state.is_some()) {
            output::table(
                ["GROUP", "STATE"],
                rows.iter().map(|row| {
                    [
                        row.group.clone(),
                        row.state.as_deref().unwrap_or("-").to_owned(),
                    ]
                }),
            )
        } else {
            output::table(["GROUP"], rows.iter().map(|row| [row.group.clone()]))
        }
    })
}

#[derive(Debug, Serialize)]
struct ShareGroupStateRow {
    group: String,
    coordinator: String,
    coordinator_id: i32,
    state: String,
    group_epoch: Option<i32>,
    assignment_epoch: Option<i32>,
    members: usize,
}

#[derive(Debug, Serialize)]
struct ShareGroupMemberRow {
    group: String,
    consumer_id: String,
    host: String,
    client_id: String,
    partitions: usize,
    member_epoch: Option<i32>,
    assignment: String,
}

#[derive(Debug, Serialize)]
struct ShareGroupOffsetRow {
    group: String,
    topic: String,
    partition: i32,
    leader_epoch: Option<i32>,
    start_offset: Option<i64>,
    lag: Option<i64>,
    error: Option<String>,
}

async fn describe_share_group_offsets(
    client: &krafka::admin::AdminClient,
    group_ids: &[String],
) -> Result<Vec<ShareGroupOffsetRow>> {
    let mut rows = Vec::new();
    for group_id in group_ids {
        let (_, _, connection) = group_coordinator_connection(client, group_id).await?;
        let api_key = ApiKey::Unknown(90);
        let version = connection
            .negotiate_api_version(api_key, 1, 0)
            .await
            .ok_or_else(|| {
                Error::Unsupported("broker does not support DescribeShareGroupOffsets".into())
            })?;
        let mut response = connection
            .send_request(api_key, version, |buffer| {
                // krafka preserves unknown API keys, but conservatively emits a
                // non-flexible header for them. APIs 90-92 are flexible from v0,
                // so this byte completes the request header's tagged fields.
                buffer.put_u8(0);
                encode_unsigned_varint(2, buffer);
                encode_compact_string(group_id, buffer);
                buffer.put_u8(0); // nullable compact topics: null means all partitions
                buffer.put_u8(0); // group tagged fields
                buffer.put_u8(0); // top-level tagged fields
                Ok(())
            })
            .await?;
        drop(connection);
        // The corresponding flexible response-header tagged fields are left in
        // the payload because krafka treats this forward-compatible key as
        // unknown.
        skip_tagged_fields(&mut response)?;
        let _throttle_time_ms = i32::decode(&mut response)?;
        let group_count = decode_compact_len(&mut response)?;
        for _ in 0..group_count {
            let response_group = decode_compact_string(&mut response)?;
            let topic_count = decode_compact_len(&mut response)?;
            let mut group_rows = Vec::new();
            for _ in 0..topic_count {
                let topic = decode_compact_string(&mut response)?;
                if response.remaining() < 16 {
                    return Err(Error::Config(
                        "DescribeShareGroupOffsets response omitted topic ID bytes".into(),
                    ));
                }
                response.advance(16);
                let partition_count = decode_compact_len(&mut response)?;
                for _ in 0..partition_count {
                    let partition = i32::decode(&mut response)?;
                    let start_offset = i64::decode(&mut response)?;
                    let leader_epoch = i32::decode(&mut response)?;
                    let lag = if version >= 1 {
                        i64::decode(&mut response)?
                    } else {
                        -1
                    };
                    let error_code = i16::decode(&mut response)?;
                    let error_message = decode_nullable_compact_string(&mut response)?;
                    skip_tagged_fields(&mut response)?;
                    group_rows.push(ShareGroupOffsetRow {
                        group: response_group.clone(),
                        topic: topic.clone(),
                        partition,
                        leader_epoch: (leader_epoch >= 0).then_some(leader_epoch),
                        start_offset: (start_offset >= 0).then_some(start_offset),
                        lag: (lag >= 0).then_some(lag),
                        error: (error_code != 0).then(|| {
                            error_message.unwrap_or_else(|| format!("Kafka error {error_code}"))
                        }),
                    });
                }
                skip_tagged_fields(&mut response)?;
            }
            let error_code = i16::decode(&mut response)?;
            let error_message = decode_nullable_compact_string(&mut response)?;
            skip_tagged_fields(&mut response)?;
            if error_code != 0 {
                return Err(Error::Config(format!(
                    "DescribeShareGroupOffsets failed for {response_group}: {}",
                    error_message.unwrap_or_else(|| format!("Kafka error {error_code}"))
                )));
            }
            rows.extend(group_rows);
        }
        skip_tagged_fields(&mut response)?;
    }
    rows.sort_by(|left, right| {
        (&left.group, &left.topic, left.partition).cmp(&(
            &right.group,
            &right.topic,
            right.partition,
        ))
    });
    Ok(rows)
}

fn write_share_group_offsets(
    format: OutputFormat,
    rows: &[ShareGroupOffsetRow],
    verbose: bool,
) -> Result<()> {
    output::write_value(format, "share-groups.describe.offsets", &rows, |rows| {
        if verbose {
            output::table(
                [
                    "GROUP",
                    "TOPIC",
                    "PARTITION",
                    "LEADER-EPOCH",
                    "START-OFFSET",
                    "LAG",
                    "ERROR",
                ],
                rows.iter().map(|row| {
                    [
                        row.group.clone(),
                        row.topic.clone(),
                        row.partition.to_string(),
                        row.leader_epoch
                            .map_or_else(|| "-".into(), |v| v.to_string()),
                        row.start_offset
                            .map_or_else(|| "-".into(), |v| v.to_string()),
                        row.lag.map_or_else(|| "-".into(), |v| v.to_string()),
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
                    "START-OFFSET",
                    "LAG",
                    "ERROR",
                ],
                rows.iter().map(|row| {
                    [
                        row.group.clone(),
                        row.topic.clone(),
                        row.partition.to_string(),
                        row.start_offset
                            .map_or_else(|| "-".into(), |v| v.to_string()),
                        row.lag.map_or_else(|| "-".into(), |v| v.to_string()),
                        row.error.as_deref().unwrap_or("-").to_owned(),
                    ]
                }),
            )
        }
    })
}

async fn delete_one_group(
    client: &krafka::admin::AdminClient,
    group_id: &str,
) -> Result<Option<String>> {
    let (_, _, connection) = group_coordinator_connection(client, group_id).await?;
    let version = connection
        .negotiate_api_version(
            ApiKey::DeleteGroups,
            versions::DELETE_GROUPS_MAX,
            versions::DELETE_GROUPS_MIN,
        )
        .await
        .ok_or_else(|| Error::Unsupported("broker does not support DeleteGroups".into()))?;
    let request = krafka::protocol::DeleteGroupsRequest::new(vec![group_id.to_owned()]);
    let mut response = connection
        .send_request(ApiKey::DeleteGroups, version, |buffer| {
            request.encode_versioned(version, buffer)
        })
        .await?;
    drop(connection);
    let response =
        krafka::protocol::DeleteGroupsResponse::decode_versioned(version, &mut response)?;
    let result =
        response.results.into_iter().next().ok_or_else(|| {
            Error::Config(format!("broker omitted deletion result for {group_id}"))
        })?;
    Ok((!result.error_code.is_ok()).then(|| format!("{:?}", result.error_code)))
}

async fn delete_share_groups(
    client: &krafka::admin::AdminClient,
    format: OutputFormat,
    requested: Vec<String>,
    all_groups: bool,
    execute: bool,
) -> Result<()> {
    let available = share_group_ids(client).await?;
    let groups = if all_groups {
        available.clone()
    } else {
        requested
    };
    if groups.is_empty() {
        return Err(Error::Usage("no Share groups matched".into()));
    }
    let available = available.into_iter().collect::<BTreeSet<_>>();
    let existing = groups
        .iter()
        .filter(|group| available.contains(*group))
        .cloned()
        .collect::<Vec<_>>();
    let descriptions = describe_share_groups(client, &existing).await?;
    let states = descriptions
        .into_iter()
        .map(|group| (group.description.group_id, group.description.group_state))
        .collect::<BTreeMap<_, _>>();
    let mut rows = Vec::with_capacity(groups.len());
    for group in groups {
        let validation_error = if available.contains(&group) {
            states.get(&group).and_then(|state| {
                (state != "Empty").then(|| format!("Share group is not EMPTY (state: {state})"))
            })
        } else {
            Some(format!("Group '{group}' is not a Share group"))
        };
        if let Some(error) = validation_error {
            rows.push(MutationRow {
                resource: group,
                status: "FAILED".into(),
                error: Some(error),
            });
        } else if !execute {
            rows.push(MutationRow {
                resource: group,
                status: "PREVIEW".into(),
                error: None,
            });
        } else {
            let error = delete_one_group(client, &group).await?;
            rows.push(MutationRow {
                resource: group,
                status: if error.is_some() { "FAILED" } else { "DELETED" }.into(),
                error,
            });
        }
    }
    let failures = rows.iter().filter(|row| row.error.is_some()).count();
    write_mutation_rows(format, "share-groups.delete", &rows)?;
    if failures == 0 {
        Ok(())
    } else {
        Err(Error::Partial {
            failed: failures,
            total: rows.len(),
        })
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Kafka Streams delete exposes independent selection, internal-topic, and execution controls"
)]
async fn delete_streams_groups(
    config: &rdkafka::ClientConfig,
    client: &krafka::admin::AdminClient,
    timeout: Duration,
    format: OutputFormat,
    requested: Vec<String>,
    all_groups: bool,
    delete_all_internal_topics: bool,
    execute: bool,
) -> Result<()> {
    let available = group_ids_by_type(client, "streams").await?;
    let groups = if all_groups {
        available.clone()
    } else {
        requested
    };
    if groups.is_empty() {
        return Err(Error::Usage("no Streams groups matched".into()));
    }
    let available = available.into_iter().collect::<BTreeSet<_>>();
    let existing = groups
        .iter()
        .filter(|group| available.contains(*group))
        .cloned()
        .collect::<Vec<_>>();
    let descriptions = describe_streams_groups(client, &existing, false, false).await?;
    let details = descriptions
        .into_iter()
        .map(|group| (group.description.group_id.clone(), group.description))
        .collect::<BTreeMap<_, _>>();
    let mut rows = Vec::new();
    for group in groups {
        let validation_error = details.get(&group).map_or_else(
            || Some(format!("Group '{group}' is not a Streams group")),
            |description| {
                (description.group_state != "Empty").then(|| {
                    format!(
                        "Streams group is not EMPTY (state: {})",
                        description.group_state
                    )
                })
            },
        );
        if let Some(error) = validation_error {
            rows.push(MutationRow {
                resource: group,
                status: "FAILED".into(),
                error: Some(error),
            });
            continue;
        }
        let internal_topics = if delete_all_internal_topics {
            streams_internal_topics(&details[&group])
        } else {
            BTreeSet::new()
        };
        if !execute {
            rows.push(MutationRow {
                resource: group.clone(),
                status: "PREVIEW".into(),
                error: None,
            });
            rows.extend(internal_topics.into_iter().map(|topic| MutationRow {
                resource: topic,
                status: "PREVIEW".into(),
                error: None,
            }));
            continue;
        }
        let error = delete_one_group(client, &group).await?;
        let group_deleted = error.is_none();
        rows.push(MutationRow {
            resource: group,
            status: if group_deleted { "DELETED" } else { "FAILED" }.into(),
            error,
        });
        if group_deleted && !internal_topics.is_empty() {
            let topics = internal_topics.into_iter().collect::<Vec<_>>();
            let topic_refs = topics.iter().map(String::as_str).collect::<Vec<_>>();
            let options = AdminOptions::new().request_timeout(Some(timeout));
            let results = admin(config)?.delete_topics(&topic_refs, &options).await?;
            for (topic, result) in topics.into_iter().zip(results) {
                let error = result.err().map(|(_, code)| format!("{code:?}"));
                rows.push(MutationRow {
                    resource: topic,
                    status: if error.is_none() { "DELETED" } else { "FAILED" }.into(),
                    error,
                });
            }
        }
    }
    let failures = rows.iter().filter(|row| row.error.is_some()).count();
    write_mutation_rows(format, "streams-groups.delete", &rows)?;
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
struct StreamsOffsetMutationRow {
    group: String,
    topic: String,
    partition: i32,
    status: String,
}

#[expect(
    clippy::too_many_arguments,
    reason = "Kafka Streams offset deletion combines clients, selection scope, and execution controls"
)]
async fn delete_streams_group_offsets(
    config: &rdkafka::ClientConfig,
    client: &krafka::admin::AdminClient,
    timeout: Duration,
    format: OutputFormat,
    group: &str,
    input_topics: &[String],
    all_input_topics: bool,
    execute: bool,
) -> Result<()> {
    let admin = admin(config)?;
    let committed =
        ffi::list_consumer_group_offsets(admin.inner().native_ptr(), group, duration_ms(timeout)?)?;
    let committed_set = committed
        .iter()
        .map(|offset| (offset.topic.clone(), offset.partition))
        .collect::<BTreeSet<_>>();
    let selections = if all_input_topics {
        let group_ids = [group.to_owned()];
        let description = describe_streams_groups(client, &group_ids, false, false)
            .await?
            .pop()
            .ok_or_else(|| Error::Config(format!("broker omitted Streams group {group}")))?;
        let source_topics = description
            .description
            .topology
            .into_iter()
            .flat_map(|topology| topology.subtopologies)
            .flat_map(|subtopology| subtopology.source_topics)
            .collect::<BTreeSet<_>>();
        let mut selections = BTreeMap::<String, Vec<i32>>::new();
        for (topic, partition) in &committed_set {
            if source_topics.contains(topic) {
                selections
                    .entry(topic.clone())
                    .or_default()
                    .push(*partition);
            }
        }
        selections.into_iter().collect::<Vec<_>>()
    } else {
        let selections = resolve_topic_partition_selections(config, timeout, input_topics)?;
        let missing = selections
            .iter()
            .flat_map(|(topic, partitions)| {
                partitions
                    .iter()
                    .map(move |partition| (topic.clone(), *partition))
            })
            .filter(|partition| !committed_set.contains(partition))
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(Error::Usage(format!(
                "one or more partitions are not part of Streams group '{group}': {missing:?}"
            )));
        }
        selections
    };
    if selections.is_empty() {
        return Err(Error::Usage(format!(
            "no input-topic offsets matched Streams group '{group}'"
        )));
    }
    let status = if execute { "DELETED" } else { "PREVIEW" };
    if execute {
        ffi::delete_group_offsets(
            admin.inner().native_ptr(),
            group,
            &selections,
            duration_ms(timeout)?,
        )?;
    }
    drop(admin);
    let rows = selections
        .into_iter()
        .flat_map(|(topic, partitions)| {
            partitions
                .into_iter()
                .map(move |partition| StreamsOffsetMutationRow {
                    group: group.into(),
                    topic: topic.clone(),
                    partition,
                    status: status.into(),
                })
        })
        .collect::<Vec<_>>();
    output::write_value(format, "streams-groups.delete-offsets", &rows, |rows| {
        output::table(
            ["GROUP", "TOPIC", "PARTITION", "STATUS"],
            rows.iter().map(|row| {
                [
                    row.group.clone(),
                    row.topic.clone(),
                    row.partition.to_string(),
                    row.status.clone(),
                ]
            }),
        )
    })
}

fn streams_reset_target_args(args: &StreamsGroupResetOffsetsArgs) -> ResetOffsetsArgs {
    ResetOffsetsArgs {
        group: args.group.clone(),
        all_groups: args.all_groups,
        topic: args.input_topic.clone(),
        all_topics: args.all_input_topics,
        to_earliest: args.to_earliest,
        to_latest: args.to_latest,
        to_offset: args.to_offset,
        shift_by: args.shift_by,
        to_current: args.to_current,
        to_datetime: args.to_datetime.clone(),
        by_duration: args.by_duration.clone(),
        from_file: args.from_file.clone(),
        export: args.export,
        execute: args.execute,
        dry_run: args.dry_run,
    }
}

fn write_streams_reset_rows(
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
        let csv = String::from_utf8(
            writer
                .into_inner()
                .map_err(|error| Error::Usage(format!("cannot finish reset CSV: {error}")))?,
        )
        .map_err(|error| Error::Usage(format!("reset CSV is not UTF-8: {error}")))?;
        print!("{csv}");
        for error in errors {
            eprintln!("Error: {error}");
        }
        return Ok(());
    }
    output::write_value_with_errors(
        format,
        "streams-groups.reset-offsets",
        &rows,
        errors,
        |rows| {
            output::table(
                ["GROUP", "TOPIC", "PARTITION", "NEW-OFFSET"],
                rows.iter().map(|row| {
                    [
                        row.group.clone(),
                        row.topic.clone(),
                        row.partition.to_string(),
                        row.new_offset.to_string(),
                    ]
                }),
            )
        },
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "Streams reset keeps Kafka's mutually exclusive strategies and per-group topology scope together"
)]
async fn reset_streams_group_offsets(
    config: &rdkafka::ClientConfig,
    client: &krafka::admin::AdminClient,
    timeout: Duration,
    format: OutputFormat,
    args: &StreamsGroupResetOffsetsArgs,
) -> Result<()> {
    let target_args = streams_reset_target_args(args);
    validate_reset_target(&target_args)?;
    let groups = if args.all_groups {
        group_ids_by_type(client, "streams").await?
    } else {
        args.group.clone()
    };
    if groups.is_empty() {
        return Err(Error::Usage("no Streams groups matched".into()));
    }
    let descriptions = describe_streams_groups(client, &groups, false, true).await?;
    let mut errors = Vec::new();
    let mut inactive = BTreeMap::new();
    for group in descriptions {
        if matches!(group.description.group_state.as_str(), "Empty" | "Dead") {
            inactive.insert(group.description.group_id.clone(), group.description);
        } else {
            errors.push(format!(
                "assignments can only be reset if Streams group '{}' is inactive; current state is {}",
                group.description.group_id, group.description.group_state
            ));
        }
    }
    let inactive_ids = inactive.keys().cloned().collect::<Vec<_>>();
    if let Some(path) = args.from_file.as_deref() {
        let rows = if inactive_ids.is_empty() {
            Vec::new()
        } else {
            read_reset_plan(path, &inactive_ids, args.group.len() == 1, config, timeout)?
        };
        if args.execute {
            execute_reset_rows(config, timeout, &rows)?;
        }
        return write_streams_reset_rows(
            format,
            &rows,
            args.export,
            args.group.len() == 1,
            &errors,
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
    for (group_id, description) in &inactive {
        let topics = if args.all_input_topics {
            description
                .topology
                .as_ref()
                .into_iter()
                .flat_map(|topology| &topology.subtopologies)
                .flat_map(|subtopology| &subtopology.source_topics)
                .cloned()
                .map(|topic| (topic, None))
                .collect::<BTreeMap<_, _>>()
        } else {
            parse_reset_topics(&args.input_topic)?
        };
        let mut consumer_config = config.clone();
        consumer_config.set("group.id", group_id);
        let consumer: BaseConsumer = consumer_config.create()?;
        let mut planned = Vec::new();
        for (topic_name, selected) in topics {
            let metadata = consumer.fetch_metadata(Some(&topic_name), timeout)?;
            let topic = metadata
                .topics()
                .iter()
                .find(|topic| topic.name() == topic_name && topic.error().is_none())
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
                let mut request = TopicPartitionList::new();
                for partition in &partitions {
                    request.add_partition_offset(
                        &topic_name,
                        partition.id(),
                        Offset::Offset(timestamp),
                    )?;
                }
                Some(consumer.offsets_for_times(request, timeout)?)
            } else {
                None
            };
            for partition in partitions {
                let (low, high) =
                    consumer.fetch_watermarks(&topic_name, partition.id(), timeout)?;
                let new_offset = reset_target(
                    &target_args,
                    committed.as_ref(),
                    timestamp_offsets.as_ref(),
                    &topic_name,
                    partition.id(),
                    low,
                    high,
                )?;
                rows.push(ResetOffsetRow {
                    group: group_id.clone(),
                    topic: topic_name.clone(),
                    partition: partition.id(),
                    new_offset,
                });
                planned.push((topic_name.clone(), partition.id(), new_offset));
            }
        }
        if args.execute {
            let admin = admin(config)?;
            ffi::alter_consumer_group_offsets(
                admin.inner().native_ptr(),
                group_id,
                &planned,
                duration_ms(timeout)?,
            )?;
        }
    }
    if args.execute && (!args.delete_internal_topic.is_empty() || args.delete_all_internal_topics) {
        let admin = admin(config)?;
        for description in inactive.values() {
            let topics = if args.delete_all_internal_topics {
                streams_internal_topics(description)
            } else {
                args.delete_internal_topic.iter().cloned().collect()
            };
            if !topics.is_empty() {
                let topics = topics.iter().map(String::as_str).collect::<Vec<_>>();
                let results = admin
                    .delete_topics(&topics, &AdminOptions::new().request_timeout(Some(timeout)))
                    .await?;
                for result in results {
                    if let Err((topic, code)) = result {
                        errors.push(format!(
                            "deleting internal topic '{topic}' failed: {code:?}"
                        ));
                    }
                }
            }
        }
    }
    write_streams_reset_rows(format, &rows, args.export, args.group.len() == 1, &errors)
}

#[derive(Debug, Serialize)]
struct ShareGroupTopicMutationRow {
    group: String,
    topic: String,
    status: String,
    error: Option<String>,
}

async fn delete_share_group_offsets(
    client: &krafka::admin::AdminClient,
    format: OutputFormat,
    group_id: &str,
    topics: &[String],
    execute: bool,
) -> Result<()> {
    let topics = topics
        .iter()
        .map(|topic| topic.trim())
        .filter(|topic| !topic.is_empty())
        .collect::<BTreeSet<_>>();
    if topics.is_empty() {
        return Err(Error::Usage(
            "at least one non-empty topic is required".into(),
        ));
    }
    if !execute {
        let rows = topics
            .iter()
            .map(|topic| ShareGroupTopicMutationRow {
                group: group_id.into(),
                topic: (*topic).into(),
                status: "PREVIEW".into(),
                error: None,
            })
            .collect::<Vec<_>>();
        return write_share_group_topic_mutations(format, "share-groups.delete-offsets", &rows);
    }
    let (_, _, connection) = group_coordinator_connection(client, group_id).await?;
    let api_key = ApiKey::Unknown(92);
    let version = connection
        .negotiate_api_version(api_key, 0, 0)
        .await
        .ok_or_else(|| {
            Error::Unsupported("broker does not support DeleteShareGroupOffsets".into())
        })?;
    let mut response = connection
        .send_request(api_key, version, |buffer| {
            buffer.put_u8(0); // flexible request-header tagged fields
            encode_compact_string(group_id, buffer);
            encode_unsigned_varint(topics.len() + 1, buffer);
            for topic in &topics {
                encode_compact_string(topic, buffer);
                buffer.put_u8(0);
            }
            buffer.put_u8(0);
            Ok(())
        })
        .await?;
    drop(connection);
    skip_tagged_fields(&mut response)?; // flexible response-header tagged fields
    let _throttle_time_ms = i32::decode(&mut response)?;
    let top_error = i16::decode(&mut response)?;
    let top_message = decode_nullable_compact_string(&mut response)?;
    let count = decode_compact_len(&mut response)?;
    let mut rows = Vec::with_capacity(count);
    for _ in 0..count {
        let topic = decode_compact_string(&mut response)?;
        if response.remaining() < 16 {
            return Err(Error::Config(
                "DeleteShareGroupOffsets response omitted topic ID bytes".into(),
            ));
        }
        response.advance(16);
        let error_code = i16::decode(&mut response)?;
        let error_message = decode_nullable_compact_string(&mut response)?;
        skip_tagged_fields(&mut response)?;
        rows.push(ShareGroupTopicMutationRow {
            group: group_id.into(),
            topic,
            status: if error_code == 0 { "DELETED" } else { "FAILED" }.into(),
            error: (error_code != 0)
                .then(|| error_message.unwrap_or_else(|| format!("Kafka error {error_code}"))),
        });
    }
    skip_tagged_fields(&mut response)?;
    if top_error != 0 {
        return Err(Error::Config(format!(
            "DeleteShareGroupOffsets failed for {group_id}: {}",
            top_message.unwrap_or_else(|| format!("Kafka error {top_error}"))
        )));
    }
    let failures = rows.iter().filter(|row| row.error.is_some()).count();
    write_share_group_topic_mutations(format, "share-groups.delete-offsets", &rows)?;
    if failures == 0 {
        Ok(())
    } else {
        Err(Error::Partial {
            failed: failures,
            total: rows.len(),
        })
    }
}

fn write_share_group_topic_mutations(
    format: OutputFormat,
    command: &str,
    rows: &[ShareGroupTopicMutationRow],
) -> Result<()> {
    output::write_value(format, command, &rows, |rows| {
        output::table(
            ["GROUP", "TOPIC", "STATUS", "ERROR"],
            rows.iter().map(|row| {
                [
                    row.group.clone(),
                    row.topic.clone(),
                    row.status.clone(),
                    row.error.as_deref().unwrap_or("-").into(),
                ]
            }),
        )
    })
}

fn validate_share_reset_target(args: &ShareGroupResetOffsetsArgs) -> Result<()> {
    if args.to_earliest
        || args.to_latest
        || args.to_offset.is_some()
        || args.to_current
        || args.to_datetime.is_some()
        || args.from_file.is_some()
    {
        Ok(())
    } else {
        Err(Error::Usage("choose one Share group reset target".into()))
    }
}

async fn alter_share_group_offsets(
    client: &krafka::admin::AdminClient,
    group_id: &str,
    rows: &[ResetOffsetRow],
) -> Result<Vec<String>> {
    let mut topics = BTreeMap::<&str, Vec<&ResetOffsetRow>>::new();
    for row in rows {
        topics.entry(&row.topic).or_default().push(row);
    }
    let (_, _, connection) = group_coordinator_connection(client, group_id).await?;
    let api_key = ApiKey::Unknown(91);
    let version = connection
        .negotiate_api_version(api_key, 0, 0)
        .await
        .ok_or_else(|| {
            Error::Unsupported("broker does not support AlterShareGroupOffsets".into())
        })?;
    let mut response = connection
        .send_request(api_key, version, |buffer| {
            buffer.put_u8(0); // flexible request-header tagged fields
            encode_compact_string(group_id, buffer);
            encode_unsigned_varint(topics.len() + 1, buffer);
            for (topic, partitions) in &topics {
                encode_compact_string(topic, buffer);
                encode_unsigned_varint(partitions.len() + 1, buffer);
                for row in partitions {
                    buffer.put_i32(row.partition);
                    buffer.put_i64(row.new_offset);
                    buffer.put_u8(0);
                }
                buffer.put_u8(0);
            }
            buffer.put_u8(0);
            Ok(())
        })
        .await?;
    drop(connection);
    skip_tagged_fields(&mut response)?; // flexible response-header tagged fields
    let _throttle_time_ms = i32::decode(&mut response)?;
    let top_error = i16::decode(&mut response)?;
    let top_message = decode_nullable_compact_string(&mut response)?;
    let count = decode_compact_len(&mut response)?;
    let mut errors = Vec::new();
    for _ in 0..count {
        let topic = decode_compact_string(&mut response)?;
        if response.remaining() < 16 {
            return Err(Error::Config(
                "AlterShareGroupOffsets response omitted topic ID bytes".into(),
            ));
        }
        response.advance(16);
        let partition_count = decode_compact_len(&mut response)?;
        for _ in 0..partition_count {
            let partition = i32::decode(&mut response)?;
            let error_code = i16::decode(&mut response)?;
            let error_message = decode_nullable_compact_string(&mut response)?;
            skip_tagged_fields(&mut response)?;
            if error_code != 0 {
                errors.push(format!(
                    "{group_id}:{topic}:{partition}: {}",
                    error_message.unwrap_or_else(|| format!("Kafka error {error_code}"))
                ));
            }
        }
        skip_tagged_fields(&mut response)?;
    }
    skip_tagged_fields(&mut response)?;
    if top_error != 0 {
        return Err(Error::Config(format!(
            "AlterShareGroupOffsets failed for {group_id}: {}",
            top_message.unwrap_or_else(|| format!("Kafka error {top_error}"))
        )));
    }
    Ok(errors)
}

fn share_reset_partitions(
    config: &rdkafka::ClientConfig,
    timeout: Duration,
    topics: &[String],
) -> Result<Vec<(String, i32)>> {
    Ok(resolve_topic_partition_selections(config, timeout, topics)?
        .into_iter()
        .flat_map(|(topic, partitions)| {
            partitions
                .into_iter()
                .map(move |partition| (topic.clone(), partition))
        })
        .collect())
}

#[expect(
    clippy::too_many_lines,
    reason = "Share reset planning keeps Kafka's mutually exclusive strategies and execution boundary together"
)]
async fn reset_share_group_offsets(
    config: &rdkafka::ClientConfig,
    client: &krafka::admin::AdminClient,
    timeout: Duration,
    format: OutputFormat,
    args: &ShareGroupResetOffsetsArgs,
) -> Result<()> {
    validate_share_reset_target(args)?;
    let share_groups = share_group_ids(client).await?;
    if share_groups.contains(&args.group) {
        let descriptions = describe_share_groups(client, std::slice::from_ref(&args.group)).await?;
        if let Some(description) = descriptions.first()
            && !matches!(
                description.description.group_state.as_str(),
                "Empty" | "Dead"
            )
        {
            return Err(Error::Usage(format!(
                "Share group '{}' is not empty (state: {})",
                args.group, description.description.group_state
            )));
        }
    }
    let current_offsets = if args.all_topics || args.to_current {
        describe_share_group_offsets(client, std::slice::from_ref(&args.group))
            .await?
            .into_iter()
            .filter_map(|row| {
                row.start_offset
                    .map(|offset| ((row.topic, row.partition), offset))
            })
            .collect::<BTreeMap<_, _>>()
    } else {
        BTreeMap::new()
    };
    let mut rows = if let Some(path) = args.from_file.as_deref() {
        read_reset_plan(
            path,
            std::slice::from_ref(&args.group),
            true,
            config,
            timeout,
        )?
    } else {
        let partitions = if args.all_topics {
            current_offsets
                .keys()
                .map(|(topic, partition)| (topic.clone(), *partition))
                .collect::<Vec<_>>()
        } else {
            share_reset_partitions(config, timeout, &args.topic)?
        };
        let consumer = base_consumer(config)?;
        let timestamp = args
            .to_datetime
            .as_deref()
            .map(parse_datetime_millis)
            .transpose()?;
        let timestamp_offsets = if let Some(timestamp) = timestamp {
            let mut requested = TopicPartitionList::new();
            for (topic, partition) in &partitions {
                requested.add_partition_offset(topic, *partition, Offset::Offset(timestamp))?;
            }
            Some(consumer.offsets_for_times(requested, timeout)?)
        } else {
            None
        };
        partitions
            .into_iter()
            .map(|(topic, partition)| {
                let (low, high) = consumer.fetch_watermarks(&topic, partition, timeout)?;
                let offset = if args.to_earliest {
                    low
                } else if args.to_latest {
                    high
                } else if let Some(offset) = args.to_offset {
                    offset.clamp(low, high)
                } else if args.to_current {
                    current_offsets
                        .get(&(topic.clone(), partition))
                        .copied()
                        .unwrap_or(high)
                } else if timestamp_offsets.is_some() {
                    committed_offset(timestamp_offsets.as_ref(), &topic, partition).unwrap_or(high)
                } else {
                    return Err(Error::Usage("choose one Share group reset target".into()));
                };
                Ok(ResetOffsetRow {
                    group: args.group.clone(),
                    topic,
                    partition,
                    new_offset: offset,
                })
            })
            .collect::<Result<Vec<_>>>()?
    };
    rows.sort_by(|left, right| (&left.topic, left.partition).cmp(&(&right.topic, right.partition)));
    let errors = if args.execute && !rows.is_empty() {
        alter_share_group_offsets(client, &args.group, &rows).await?
    } else {
        Vec::new()
    };
    write_reset_rows(format, &rows, args.export, true, &errors)?;
    if errors.is_empty() {
        Ok(())
    } else {
        Err(Error::Partial {
            failed: errors.len(),
            total: rows.len(),
        })
    }
}

fn share_assignment(assignment: &[krafka::protocol::ShareGroupDescribeTopicPartitions]) -> String {
    share_assignment_parts(
        assignment
            .iter()
            .map(|topic| (topic.topic_name.as_str(), topic.partitions.as_slice())),
    )
}

fn share_assignment_parts<'a>(assignment: impl Iterator<Item = (&'a str, &'a [i32])>) -> String {
    assignment
        .map(|(topic, assigned)| {
            let mut partitions = assigned.to_vec();
            partitions.sort_unstable();
            format!(
                "{}:{}",
                topic,
                partitions
                    .iter()
                    .map(i32::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            )
        })
        .collect::<Vec<_>>()
        .join(";")
}

#[expect(
    clippy::too_many_lines,
    reason = "Share describe dynamically selects Kafka's offsets, members, and state output schemas"
)]
async fn describe_share_group_details(
    client: &krafka::admin::AdminClient,
    format: OutputFormat,
    group_ids: &[String],
    members: bool,
    state: bool,
    verbose: bool,
) -> Result<()> {
    let descriptions = describe_share_groups(client, group_ids).await?;
    if members {
        let rows = descriptions
            .into_iter()
            .flat_map(|group| {
                group.description.members.into_iter().map(move |member| {
                    let partitions = member
                        .assignment
                        .iter()
                        .map(|topic| topic.partitions.len())
                        .sum();
                    ShareGroupMemberRow {
                        group: group.description.group_id.clone(),
                        consumer_id: member.member_id,
                        host: member.client_host,
                        client_id: member.client_id,
                        partitions,
                        member_epoch: verbose.then_some(member.member_epoch),
                        assignment: share_assignment(&member.assignment),
                    }
                })
            })
            .collect::<Vec<_>>();
        return output::write_value(format, "share-groups.describe.members", &rows, |rows| {
            if verbose {
                output::table(
                    [
                        "GROUP",
                        "CONSUMER-ID",
                        "HOST",
                        "CLIENT-ID",
                        "#PARTITIONS",
                        "MEMBER-EPOCH",
                        "ASSIGNMENT",
                    ],
                    rows.iter().map(|row| {
                        [
                            row.group.clone(),
                            row.consumer_id.clone(),
                            row.host.clone(),
                            row.client_id.clone(),
                            row.partitions.to_string(),
                            row.member_epoch
                                .map_or_else(|| "-".into(), |v| v.to_string()),
                            row.assignment.clone(),
                        ]
                    }),
                )
            } else {
                output::table(
                    [
                        "GROUP",
                        "CONSUMER-ID",
                        "HOST",
                        "CLIENT-ID",
                        "#PARTITIONS",
                        "ASSIGNMENT",
                    ],
                    rows.iter().map(|row| {
                        [
                            row.group.clone(),
                            row.consumer_id.clone(),
                            row.host.clone(),
                            row.client_id.clone(),
                            row.partitions.to_string(),
                            row.assignment.clone(),
                        ]
                    }),
                )
            }
        });
    }
    if state {
        let rows = descriptions
            .into_iter()
            .map(|group| ShareGroupStateRow {
                group: group.description.group_id,
                coordinator: group.coordinator,
                coordinator_id: group.coordinator_id,
                state: group.description.group_state,
                group_epoch: verbose.then_some(group.description.group_epoch),
                assignment_epoch: verbose.then_some(group.description.assignment_epoch),
                members: group.description.members.len(),
            })
            .collect::<Vec<_>>();
        return output::write_value(format, "share-groups.describe.state", &rows, |rows| {
            if verbose {
                output::table(
                    [
                        "GROUP",
                        "COORDINATOR (ID)",
                        "STATE",
                        "GROUP-EPOCH",
                        "ASSIGNMENT-EPOCH",
                        "#MEMBERS",
                    ],
                    rows.iter().map(|row| {
                        [
                            row.group.clone(),
                            format!("{} ({})", row.coordinator, row.coordinator_id),
                            row.state.clone(),
                            row.group_epoch
                                .map_or_else(|| "-".into(), |v| v.to_string()),
                            row.assignment_epoch
                                .map_or_else(|| "-".into(), |v| v.to_string()),
                            row.members.to_string(),
                        ]
                    }),
                )
            } else {
                output::table(
                    ["GROUP", "COORDINATOR (ID)", "STATE", "#MEMBERS"],
                    rows.iter().map(|row| {
                        [
                            row.group.clone(),
                            format!("{} ({})", row.coordinator, row.coordinator_id),
                            row.state.clone(),
                            row.members.to_string(),
                        ]
                    }),
                )
            }
        });
    }
    let rows = describe_share_group_offsets(client, group_ids).await?;
    write_share_group_offsets(format, &rows, verbose)
}

async fn share_groups(
    config: &rdkafka::ClientConfig,
    bootstrap: &str,
    command_config: Option<&Path>,
    timeout: Duration,
    format: OutputFormat,
    action: ShareGroupAction,
    verbose: bool,
) -> Result<()> {
    let client = config::protocol_admin(bootstrap, timeout, command_config).await?;
    match action {
        ShareGroupAction::List { state } => {
            list_share_groups(&client, format, state.as_deref()).await
        }
        ShareGroupAction::Describe {
            group,
            all_groups,
            members,
            state,
            offsets: _,
        } => {
            let group_ids = if all_groups {
                share_group_ids(&client).await?
            } else {
                group
            };
            describe_share_group_details(&client, format, &group_ids, members, state, verbose).await
        }
        ShareGroupAction::Delete {
            group,
            all_groups,
            execute,
        } => delete_share_groups(&client, format, group, all_groups, execute).await,
        ShareGroupAction::ResetOffsets(args) => {
            reset_share_group_offsets(config, &client, timeout, format, &args).await
        }
        ShareGroupAction::DeleteOffsets {
            group,
            topic,
            execute,
        } => delete_share_group_offsets(&client, format, &group, &topic, execute).await,
    }
}

#[expect(
    clippy::significant_drop_tightening,
    reason = "the protocol client remains available across every async command branch"
)]
async fn streams_groups(
    config: &rdkafka::ClientConfig,
    bootstrap: &str,
    command_config: Option<&Path>,
    timeout: Duration,
    format: OutputFormat,
    action: StreamsGroupAction,
    verbose: bool,
) -> Result<()> {
    let client = config::protocol_admin(bootstrap, timeout, command_config).await?;
    match action {
        StreamsGroupAction::List { state } => {
            list_streams_groups(&client, format, state.as_deref()).await
        }
        StreamsGroupAction::Describe {
            group,
            all_groups,
            members,
            state,
            offsets: _,
            topology,
        } => {
            let group_ids = if all_groups {
                group_ids_by_type(&client, "streams").await?
            } else {
                group
            };
            let descriptions =
                describe_streams_groups(&client, &group_ids, topology, false).await?;
            if topology {
                write_streams_topologies(format, descriptions)
            } else if members {
                write_streams_group_members(format, descriptions, verbose)
            } else if state {
                write_streams_group_states(format, descriptions, verbose)
            } else {
                write_streams_group_offsets(config, timeout, format, descriptions, verbose)
            }
        }
        StreamsGroupAction::Delete {
            group,
            all_groups,
            delete_all_internal_topics,
            execute,
        } => {
            delete_streams_groups(
                config,
                &client,
                timeout,
                format,
                group,
                all_groups,
                delete_all_internal_topics,
                execute,
            )
            .await
        }
        StreamsGroupAction::DeleteOffsets {
            group,
            input_topic,
            all_input_topics,
            execute,
        } => {
            delete_streams_group_offsets(
                config,
                &client,
                timeout,
                format,
                &group,
                &input_topic,
                all_input_topics,
                execute,
            )
            .await
        }
        StreamsGroupAction::ResetOffsets(args) => {
            reset_streams_group_offsets(config, &client, timeout, format, &args).await
        }
    }
}

#[derive(Debug, Serialize)]
struct StreamsApplicationResetRow {
    action: String,
    resource: String,
    partition: Option<i32>,
    offset: Option<i64>,
    status: String,
    error: Option<String>,
}

fn write_streams_application_reset_rows(
    format: OutputFormat,
    rows: &[StreamsApplicationResetRow],
) -> Result<()> {
    output::write_value(format, "streams-application-reset", &rows, |rows| {
        output::table(
            [
                "ACTION",
                "RESOURCE",
                "PARTITION",
                "OFFSET",
                "STATUS",
                "ERROR",
            ],
            rows.iter().map(|row| {
                [
                    row.action.clone(),
                    row.resource.clone(),
                    row.partition.map_or_else(|| "-".into(), |v| v.to_string()),
                    row.offset.map_or_else(|| "-".into(), |v| v.to_string()),
                    row.status.clone(),
                    row.error.as_deref().unwrap_or("-").to_owned(),
                ]
            }),
        )
    })
}

fn matches_streams_internal_topic_format(topic: &str) -> bool {
    static FOREIGN_KEY_TOPIC: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"-KTABLE-FK-JOIN-SUBSCRIPTION-(?:REGISTRATION|RESPONSE)-\d+-topic$")
            .expect("static Streams internal topic regex")
    });
    topic.ends_with("-changelog")
        || topic.ends_with("-repartition")
        || topic.ends_with("-subscription-registration-topic")
        || topic.ends_with("-subscription-response-topic")
        || FOREIGN_KEY_TOPIC.is_match(topic)
}

fn inferred_streams_internal_topics(
    application_id: &str,
    all_topics: impl IntoIterator<Item = String>,
    input_topics: &BTreeSet<String>,
    intermediate_topics: &BTreeSet<String>,
) -> BTreeSet<String> {
    let prefix = format!("{application_id}-");
    all_topics
        .into_iter()
        .filter(|topic| {
            topic.starts_with(&prefix)
                && !input_topics.contains(topic)
                && !intermediate_topics.contains(topic)
                && matches_streams_internal_topic_format(topic)
        })
        .collect()
}

fn read_streams_application_reset_plan(path: &Path) -> Result<BTreeMap<(String, i32), i64>> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .from_path(path)
        .map_err(|error| Error::Usage(format!("cannot read reset CSV: {error}")))?;
    let mut rows = BTreeMap::new();
    for (index, record) in reader.records().enumerate() {
        let record = record.map_err(|error| {
            Error::Usage(format!("invalid reset CSV line {}: {error}", index + 1))
        })?;
        if record.len() != 3 {
            return Err(Error::Usage(format!(
                "reset CSV line {} must contain TOPIC,PARTITION,OFFSET",
                index + 1
            )));
        }
        let topic = record[0].to_owned();
        let partition = parse_csv_number::<i32>(&record[1], index, "partition")?;
        let offset = parse_csv_number::<i64>(&record[2], index, "offset")?;
        if topic.is_empty() || partition < 0 || offset < 0 {
            return Err(Error::Usage(format!(
                "reset CSV line {} requires a topic and non-negative partition/offset",
                index + 1
            )));
        }
        if rows.insert((topic.clone(), partition), offset).is_some() {
            return Err(Error::Usage(format!(
                "duplicate reset CSV target {topic}:{partition}"
            )));
        }
    }
    if rows.is_empty() {
        return Err(Error::Usage("reset CSV is empty".into()));
    }
    Ok(rows)
}

async fn force_remove_consumer_group_members(
    client: &krafka::admin::AdminClient,
    group_id: &str,
    members: &[ffi::ConsumerGroupMember],
) -> Result<()> {
    let (_, _, connection) = group_coordinator_connection(client, group_id).await?;
    let version = connection
        .negotiate_api_version(ApiKey::LeaveGroup, 5, 3)
        .await
        .ok_or_else(|| Error::Unsupported("broker does not support batch LeaveGroup v3+".into()))?;
    let request = krafka::protocol::LeaveGroupRequest {
        group_id: group_id.into(),
        member_id: String::new(),
        members: members
            .iter()
            .map(|member| krafka::protocol::LeaveGroupMember {
                member_id: member.member_id.clone(),
                group_instance_id: member.instance_id.clone(),
                reason: (version >= 5).then(|| "streams application reset --force".into()),
            })
            .collect(),
    };
    let mut bytes = connection
        .send_request(ApiKey::LeaveGroup, version, |buffer| {
            request.encode_versioned(version, buffer)
        })
        .await?;
    let response = krafka::protocol::LeaveGroupResponse::decode_versioned(version, &mut bytes)?;
    if !response.error_code.is_ok() {
        return Err(Error::Config(format!(
            "LeaveGroup failed for {group_id}: {:?}",
            response.error_code
        )));
    }
    let failed = response
        .members
        .iter()
        .filter(|member| !member.error_code.is_ok())
        .map(|member| format!("{}: {:?}", member.member_id, member.error_code))
        .collect::<Vec<_>>();
    if failed.is_empty() {
        Ok(())
    } else {
        Err(Error::Config(format!(
            "LeaveGroup failed for members: {}",
            failed.join(", ")
        )))
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Kafka StreamsResetter deliberately performs member eviction, two offset policies, and internal-topic cleanup as one transaction-like workflow"
)]
async fn streams_application_reset(
    config: &rdkafka::ClientConfig,
    bootstrap: &str,
    command_config: Option<&Path>,
    timeout: Duration,
    format: OutputFormat,
    args: &StreamsApplicationResetArgs,
) -> Result<()> {
    let admin = admin(config)?;
    let listings =
        ffi::list_consumer_groups(admin.inner().native_ptr(), &[], &[], duration_ms(timeout)?)?;
    if listings
        .iter()
        .any(|group| group.group == args.application_id)
    {
        let groups = [args.application_id.clone()];
        let mut descriptions = ffi::describe_consumer_groups(
            admin.inner().native_ptr(),
            &groups,
            duration_ms(timeout)?,
        )?;
        let description = descriptions
            .pop()
            .ok_or_else(|| Error::Config("broker omitted consumer group description".into()))?;
        if !description.members.is_empty() {
            if !args.force {
                return Err(Error::Usage(format!(
                    "consumer group '{}' is still active with {} member(s); stop all application instances or use --force",
                    args.application_id,
                    description.members.len()
                )));
            }
            let protocol = config::protocol_admin(bootstrap, timeout, command_config).await?;
            force_remove_consumer_group_members(
                &protocol,
                &args.application_id,
                &description.members,
            )
            .await?;
        }
    }

    let input_topics = args
        .input_topics
        .iter()
        .map(|topic| topic.trim().to_owned())
        .filter(|topic| !topic.is_empty())
        .collect::<BTreeSet<_>>();
    let intermediate_topics = args
        .intermediate_topics
        .iter()
        .map(|topic| topic.trim().to_owned())
        .filter(|topic| !topic.is_empty())
        .collect::<BTreeSet<_>>();
    let reset_plan = if input_topics.is_empty() {
        None
    } else {
        args.from_file
            .as_deref()
            .map(read_streams_application_reset_plan)
            .transpose()?
    };
    let mut consumer_config = config.clone();
    consumer_config
        .set("group.id", &args.application_id)
        .set("enable.auto.commit", "false")
        .set("group.protocol", "classic");
    let consumer: BaseConsumer = consumer_config.create()?;
    let metadata = consumer.fetch_metadata(None, timeout)?;
    let available = metadata
        .topics()
        .iter()
        .filter(|topic| topic.error().is_none())
        .map(|topic| topic.name().to_owned())
        .collect::<BTreeSet<_>>();
    let inferred_internal = inferred_streams_internal_topics(
        &args.application_id,
        available.iter().cloned(),
        &input_topics,
        &intermediate_topics,
    );
    let internal_topics = if args.internal_topics.is_empty() {
        inferred_internal.clone()
    } else {
        let selected = args
            .internal_topics
            .iter()
            .map(|topic| topic.trim().to_owned())
            .collect::<BTreeSet<_>>();
        if let Some(invalid) = selected.difference(&inferred_internal).next() {
            return Err(Error::Usage(format!(
                "internal topic '{invalid}' is not an inferred internal topic for application '{}'",
                args.application_id
            )));
        }
        selected
    };

    let mut rows = Vec::new();
    let mut partitions = Vec::new();
    for topic in input_topics.union(&intermediate_topics) {
        if !available.contains(topic) {
            rows.push(StreamsApplicationResetRow {
                action: if input_topics.contains(topic) {
                    "RESET-OFFSET".into()
                } else {
                    "SEEK-TO-END".into()
                },
                resource: topic.clone(),
                partition: None,
                offset: None,
                status: "FAILED".into(),
                error: Some("topic not found".into()),
            });
            continue;
        }
        let topic_metadata = metadata
            .topics()
            .iter()
            .find(|metadata| metadata.name() == topic)
            .ok_or_else(|| Error::Config(format!("metadata omitted topic {topic}")))?;
        partitions.extend(
            topic_metadata
                .partitions()
                .iter()
                .map(|partition| (topic.clone(), partition.id(), input_topics.contains(topic))),
        );
    }
    let mut requested = TopicPartitionList::new();
    for (topic, partition, _) in &partitions {
        requested.add_partition(topic, *partition);
    }
    let committed = if args.shift_by.is_some() {
        Some(consumer.committed_offsets(requested.clone(), timeout)?)
    } else {
        None
    };
    let timestamp = if let Some(value) = args.to_datetime.as_deref() {
        Some(parse_datetime_millis(value)?)
    } else if let Some(value) = args.by_duration.as_deref() {
        Some(
            Utc::now()
                .timestamp_millis()
                .saturating_sub(parse_iso8601_duration_millis(value)?),
        )
    } else {
        None
    };
    let timestamp_offsets = if let Some(timestamp) = timestamp {
        let mut request = TopicPartitionList::new();
        for (topic, partition, is_input) in &partitions {
            if *is_input {
                request.add_partition_offset(topic, *partition, Offset::Offset(timestamp))?;
            }
        }
        Some(consumer.offsets_for_times(request, timeout)?)
    } else {
        None
    };
    let mut changes = Vec::new();
    for (topic, partition, is_input) in partitions {
        let (low, high) = consumer.fetch_watermarks(&topic, partition, timeout)?;
        let offset = if !is_input {
            high
        } else if let Some(plan) = &reset_plan {
            plan.get(&(topic.clone(), partition))
                .copied()
                .ok_or_else(|| {
                    Error::Usage(format!(
                        "reset CSV omits input partition {topic}:{partition}"
                    ))
                })?
                .clamp(low, high)
        } else if let Some(offset) = args.to_offset {
            offset.clamp(low, high)
        } else if args.to_latest {
            high
        } else if let Some(shift) = args.shift_by {
            let current = committed_offset(committed.as_ref(), &topic, partition).unwrap_or(high);
            current.saturating_add(shift).clamp(low, high)
        } else if timestamp_offsets.is_some() {
            committed_offset(timestamp_offsets.as_ref(), &topic, partition).unwrap_or(high)
        } else {
            low
        };
        changes.push((topic.clone(), partition, offset));
        rows.push(StreamsApplicationResetRow {
            action: if is_input {
                "RESET-OFFSET".into()
            } else {
                "SEEK-TO-END".into()
            },
            resource: topic,
            partition: Some(partition),
            offset: Some(offset),
            status: if args.dry_run { "PREVIEW" } else { "UPDATED" }.into(),
            error: None,
        });
    }
    if !args.dry_run && !changes.is_empty() {
        ffi::alter_consumer_group_offsets(
            admin.inner().native_ptr(),
            &args.application_id,
            &changes,
            duration_ms(timeout)?,
        )?;
    }
    if !internal_topics.is_empty() {
        let topics = internal_topics.into_iter().collect::<Vec<_>>();
        if args.dry_run {
            rows.extend(topics.into_iter().map(|topic| StreamsApplicationResetRow {
                action: "DELETE-INTERNAL-TOPIC".into(),
                resource: topic,
                partition: None,
                offset: None,
                status: "PREVIEW".into(),
                error: None,
            }));
        } else {
            let refs = topics.iter().map(String::as_str).collect::<Vec<_>>();
            let results = admin
                .delete_topics(&refs, &AdminOptions::new().request_timeout(Some(timeout)))
                .await?;
            for (topic, result) in topics.into_iter().zip(results) {
                let error = result.err().map(|(_, code)| format!("{code:?}"));
                rows.push(StreamsApplicationResetRow {
                    action: "DELETE-INTERNAL-TOPIC".into(),
                    resource: topic,
                    partition: None,
                    offset: None,
                    status: if error.is_none() { "DELETED" } else { "FAILED" }.into(),
                    error,
                });
            }
        }
    }
    let failures = rows.iter().filter(|row| row.error.is_some()).count();
    write_streams_application_reset_rows(format, &rows)?;
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

async fn list_groups(
    config: &rdkafka::ClientConfig,
    bootstrap: &str,
    command_config: Option<&Path>,
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
    let rows = if states.iter().any(|state| state.native().is_none()) {
        list_groups_with_protocol(bootstrap, command_config, timeout, &states, &types).await?
    } else {
        let native_states = states
            .iter()
            .filter_map(|state| state.native())
            .collect::<Vec<_>>();
        let client = admin(config)?;
        ffi::list_consumer_groups(
            client.inner().native_ptr(),
            &native_states,
            &types,
            duration_ms(timeout)?,
        )?
    };
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

async fn list_groups_with_protocol(
    bootstrap: &str,
    command_config: Option<&Path>,
    timeout: Duration,
    states: &[ConsumerGroupStateFilter],
    types: &[ffi::ConsumerGroupType],
) -> Result<Vec<ffi::ConsumerGroupListing>> {
    let client = config::protocol_admin(bootstrap, timeout, command_config).await?;
    let group_ids = client
        .list_consumer_groups()
        .await?
        .into_iter()
        .map(|group| group.group_id)
        .collect::<Vec<_>>();
    if group_ids.is_empty() {
        return Ok(Vec::new());
    }
    let descriptions = client.describe_consumer_groups(group_ids).await?;
    drop(client);
    let mut rows = descriptions
        .into_iter()
        .filter(|group| {
            (states.is_empty() || states.iter().any(|state| state.matches(&group.state)))
                && (types.is_empty()
                    || types
                        .iter()
                        .any(|group_type| group_type_matches(*group_type, &group.group_type)))
        })
        .map(|group| ffi::ConsumerGroupListing {
            group: group.group_id,
            state: group.state,
            group_type: group.group_type.to_string(),
            is_simple: group.protocol_type.as_deref().is_none_or(str::is_empty),
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| left.group.cmp(&right.group));
    Ok(rows)
}

const fn group_type_matches(
    requested: ffi::ConsumerGroupType,
    actual: &krafka::admin::GroupType,
) -> bool {
    matches!(
        (requested, actual),
        (
            ffi::ConsumerGroupType::Consumer,
            krafka::admin::GroupType::Consumer
        ) | (
            ffi::ConsumerGroupType::Classic,
            krafka::admin::GroupType::Classic
        )
    )
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConsumerGroupStateFilter {
    PreparingRebalance,
    CompletingRebalance,
    Stable,
    Dead,
    Empty,
    Assigning,
    Reconciling,
}

impl ConsumerGroupStateFilter {
    const fn native(self) -> Option<ffi::ConsumerGroupState> {
        match self {
            Self::PreparingRebalance => Some(ffi::ConsumerGroupState::PreparingRebalance),
            Self::CompletingRebalance => Some(ffi::ConsumerGroupState::CompletingRebalance),
            Self::Stable => Some(ffi::ConsumerGroupState::Stable),
            Self::Dead => Some(ffi::ConsumerGroupState::Dead),
            Self::Empty => Some(ffi::ConsumerGroupState::Empty),
            Self::Assigning | Self::Reconciling => None,
        }
    }

    fn matches(self, actual: &str) -> bool {
        let expected = match self {
            Self::PreparingRebalance => "preparingrebalance",
            Self::CompletingRebalance => "completingrebalance",
            Self::Stable => "stable",
            Self::Dead => "dead",
            Self::Empty => "empty",
            Self::Assigning => "assigning",
            Self::Reconciling => "reconciling",
        };
        normalized_group_filter(actual) == expected
    }
}

fn parse_group_states(value: &str) -> Result<Vec<ConsumerGroupStateFilter>> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    value
        .split(',')
        .map(|state| match normalized_group_filter(state).as_str() {
            "preparingrebalance" => Ok(ConsumerGroupStateFilter::PreparingRebalance),
            "completingrebalance" => Ok(ConsumerGroupStateFilter::CompletingRebalance),
            "stable" => Ok(ConsumerGroupStateFilter::Stable),
            "dead" => Ok(ConsumerGroupStateFilter::Dead),
            "empty" => Ok(ConsumerGroupStateFilter::Empty),
            "assigning" => Ok(ConsumerGroupStateFilter::Assigning),
            "reconciling" => Ok(ConsumerGroupStateFilter::Reconciling),
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
    current_epoch: Option<i32>,
    assignment: String,
    target_epoch: Option<i32>,
    target_assignment: String,
    upgraded: Option<bool>,
}

#[derive(Default)]
struct ProtocolGroupEpochs {
    group_epoch: Option<i32>,
    target_assignment_epoch: Option<i32>,
    member_epochs: BTreeMap<String, Option<i32>>,
    member_upgraded: BTreeMap<String, Option<bool>>,
}

async fn protocol_group_epochs(
    bootstrap: &str,
    command_config: Option<&Path>,
    timeout: Duration,
    groups: &[String],
) -> Result<BTreeMap<String, ProtocolGroupEpochs>> {
    let client = config::protocol_admin(bootstrap, timeout, command_config).await?;
    let descriptions = client.describe_consumer_groups(groups.to_vec()).await?;
    drop(client);
    Ok(descriptions
        .into_iter()
        .map(|description| {
            let epochs = ProtocolGroupEpochs {
                group_epoch: description.group_epoch,
                target_assignment_epoch: description.assignment_epoch,
                member_epochs: description
                    .members
                    .iter()
                    .map(|member| (member.member_id.clone(), member.member_epoch))
                    .collect(),
                member_upgraded: description
                    .members
                    .into_iter()
                    .map(|member| {
                        let upgraded = match member.member_type {
                            Some(0) => Some(false),
                            Some(1) => Some(true),
                            _ => None,
                        };
                        (member.member_id, upgraded)
                    })
                    .collect(),
            };
            (description.group_id, epochs)
        })
        .collect())
}

#[derive(Clone, Copy)]
enum GroupDescribeMode {
    Offsets,
    Members,
    State,
}

#[derive(Clone, Copy)]
struct GroupProtocolContext<'a> {
    bootstrap: &'a str,
    command_config: Option<&'a Path>,
}

async fn describe_group_details(
    config: &rdkafka::ClientConfig,
    protocol: GroupProtocolContext<'_>,
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
            return describe_groups(
                config,
                protocol.bootstrap,
                protocol.command_config,
                timeout,
                format,
                groups,
                verbose,
            )
            .await;
        }
        GroupDescribeMode::Members => {
            return describe_group_members(
                config,
                protocol.bootstrap,
                protocol.command_config,
                timeout,
                format,
                groups,
                verbose,
            )
            .await;
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

async fn describe_group_members(
    config: &rdkafka::ClientConfig,
    bootstrap: &str,
    command_config: Option<&Path>,
    timeout: Duration,
    format: OutputFormat,
    groups: &[String],
    verbose: bool,
) -> Result<()> {
    let client = admin(config)?;
    let groups =
        ffi::describe_consumer_groups(client.inner().native_ptr(), groups, duration_ms(timeout)?)?;
    drop(client);
    let epochs = if verbose {
        protocol_group_epochs(
            bootstrap,
            command_config,
            timeout,
            &groups
                .iter()
                .map(|group| group.group.clone())
                .collect::<Vec<_>>(),
        )
        .await?
    } else {
        BTreeMap::new()
    };
    let rows = groups
        .iter()
        .flat_map(|description| {
            let group_epochs = epochs.get(&description.group);
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
                    current_epoch: group_epochs
                        .and_then(|group| group.member_epochs.get(&member.member_id).copied())
                        .flatten(),
                    assignment: group_partitions(&member.assignment),
                    target_epoch: group_epochs.and_then(|group| group.target_assignment_epoch),
                    target_assignment: group_partitions(&member.target_assignment),
                    upgraded: group_epochs
                        .and_then(|group| group.member_upgraded.get(&member.member_id).copied())
                        .flatten(),
                })
        })
        .collect::<Vec<_>>();
    output::write_value(format, "groups.describe.members", &rows, |rows| {
        group_members_table(rows, verbose)
    })
}

fn group_members_table(rows: &[GroupMemberRow], verbose: bool) -> String {
    if verbose && has_migration_members(rows) {
        output::table(
            [
                "GROUP",
                "MEMBER_ID",
                "INSTANCE_ID",
                "CLIENT_ID",
                "HOST",
                "PARTITIONS",
                "CURRENT_EPOCH",
                "ASSIGNMENT",
                "TARGET_EPOCH",
                "TARGET_ASSIGNMENT",
                "UPGRADED",
            ],
            rows.iter().map(|row| {
                [
                    row.group.clone(),
                    row.member_id.clone(),
                    row.instance_id.as_deref().unwrap_or("-").to_owned(),
                    row.client_id.clone(),
                    row.host.clone(),
                    row.partitions.to_string(),
                    row.current_epoch
                        .map_or_else(|| "-".into(), |epoch| epoch.to_string()),
                    row.assignment.clone(),
                    row.target_epoch
                        .map_or_else(|| "-".into(), |epoch| epoch.to_string()),
                    row.target_assignment.clone(),
                    row.upgraded
                        .map_or_else(|| "-".into(), |upgraded| upgraded.to_string()),
                ]
            }),
        )
    } else if verbose {
        output::table(
            [
                "GROUP",
                "MEMBER_ID",
                "INSTANCE_ID",
                "CLIENT_ID",
                "HOST",
                "PARTITIONS",
                "CURRENT_EPOCH",
                "ASSIGNMENT",
                "TARGET_EPOCH",
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
                    row.current_epoch
                        .map_or_else(|| "-".into(), |epoch| epoch.to_string()),
                    row.assignment.clone(),
                    row.target_epoch
                        .map_or_else(|| "-".into(), |epoch| epoch.to_string()),
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

fn has_migration_members(rows: &[GroupMemberRow]) -> bool {
    let mut protocols = BTreeMap::<&str, (bool, bool)>::new();
    for row in rows {
        let entry = protocols.entry(&row.group).or_default();
        match row.upgraded {
            Some(false) => entry.0 = true,
            Some(true) => entry.1 = true,
            None => {}
        }
    }
    protocols
        .values()
        .any(|(has_classic, has_consumer)| *has_classic && *has_consumer)
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

#[derive(Serialize)]
struct GroupStateRow {
    group: String,
    group_type: String,
    state: String,
    assignor: String,
    members: usize,
    coordinator_id: i32,
    coordinator: String,
    group_epoch: Option<i32>,
    target_assignment_epoch: Option<i32>,
}

async fn describe_groups(
    config: &rdkafka::ClientConfig,
    bootstrap: &str,
    command_config: Option<&Path>,
    timeout: Duration,
    format: OutputFormat,
    groups: &[String],
    verbose: bool,
) -> Result<()> {
    let client = admin(config)?;
    let descriptions =
        ffi::describe_consumer_groups(client.inner().native_ptr(), groups, duration_ms(timeout)?)?;
    drop(client);
    let epochs = if verbose {
        protocol_group_epochs(bootstrap, command_config, timeout, groups).await?
    } else {
        BTreeMap::new()
    };
    let rows = descriptions
        .into_iter()
        .map(|description| {
            let epoch = epochs.get(&description.group);
            GroupStateRow {
                group: description.group,
                group_type: description.group_type,
                state: description.state,
                assignor: description.assignor,
                members: description.members.len(),
                coordinator_id: description.coordinator_id,
                coordinator: description.coordinator,
                group_epoch: epoch.and_then(|value| value.group_epoch),
                target_assignment_epoch: epoch.and_then(|value| value.target_assignment_epoch),
            }
        })
        .collect::<Vec<_>>();
    output::write_value(format, "groups.describe.state", &rows, |rows| {
        group_states_table(rows, verbose)
    })
}

fn group_states_table(rows: &[GroupStateRow], verbose: bool) -> String {
    if verbose {
        output::table(
            [
                "GROUP",
                "TYPE",
                "STATE",
                "ASSIGNOR",
                "GROUP_EPOCH",
                "TARGET_ASSIGNMENT_EPOCH",
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
                    row.group_epoch
                        .map_or_else(|| "-".into(), |epoch| epoch.to_string()),
                    row.target_assignment_epoch
                        .map_or_else(|| "-".into(), |epoch| epoch.to_string()),
                    row.members.to_string(),
                    row.coordinator_id.to_string(),
                    row.coordinator.clone(),
                ]
            }),
        )
    } else {
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
                    row.members.to_string(),
                    row.coordinator_id.to_string(),
                    row.coordinator.clone(),
                ]
            }),
        )
    }
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
            let group = groups.first().ok_or_else(|| {
                Error::Usage("reset CSV uses single-group rows but no group was selected".into())
            })?;
            (
                group.clone(),
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

#[derive(Clone, Copy)]
struct MetadataVersionSpec {
    level: i16,
    release: &'static str,
    version: &'static str,
    production: bool,
}

const METADATA_VERSIONS: &[MetadataVersionSpec] = &[
    MetadataVersionSpec {
        level: 7,
        release: "3.3",
        version: "3.3-IV3",
        production: true,
    },
    MetadataVersionSpec {
        level: 8,
        release: "3.4",
        version: "3.4-IV0",
        production: true,
    },
    MetadataVersionSpec {
        level: 9,
        release: "3.5",
        version: "3.5-IV0",
        production: true,
    },
    MetadataVersionSpec {
        level: 10,
        release: "3.5",
        version: "3.5-IV1",
        production: true,
    },
    MetadataVersionSpec {
        level: 11,
        release: "3.5",
        version: "3.5-IV2",
        production: true,
    },
    MetadataVersionSpec {
        level: 12,
        release: "3.6",
        version: "3.6-IV0",
        production: true,
    },
    MetadataVersionSpec {
        level: 13,
        release: "3.6",
        version: "3.6-IV1",
        production: true,
    },
    MetadataVersionSpec {
        level: 14,
        release: "3.6",
        version: "3.6-IV2",
        production: true,
    },
    MetadataVersionSpec {
        level: 15,
        release: "3.7",
        version: "3.7-IV0",
        production: true,
    },
    MetadataVersionSpec {
        level: 16,
        release: "3.7",
        version: "3.7-IV1",
        production: true,
    },
    MetadataVersionSpec {
        level: 17,
        release: "3.7",
        version: "3.7-IV2",
        production: true,
    },
    MetadataVersionSpec {
        level: 18,
        release: "3.7",
        version: "3.7-IV3",
        production: true,
    },
    MetadataVersionSpec {
        level: 19,
        release: "3.7",
        version: "3.7-IV4",
        production: true,
    },
    MetadataVersionSpec {
        level: 20,
        release: "3.8",
        version: "3.8-IV0",
        production: true,
    },
    MetadataVersionSpec {
        level: 21,
        release: "3.9",
        version: "3.9-IV0",
        production: true,
    },
    MetadataVersionSpec {
        level: 22,
        release: "4.0",
        version: "4.0-IV0",
        production: true,
    },
    MetadataVersionSpec {
        level: 23,
        release: "4.0",
        version: "4.0-IV1",
        production: true,
    },
    MetadataVersionSpec {
        level: 24,
        release: "4.0",
        version: "4.0-IV2",
        production: true,
    },
    MetadataVersionSpec {
        level: 25,
        release: "4.0",
        version: "4.0-IV3",
        production: true,
    },
    MetadataVersionSpec {
        level: 26,
        release: "4.1",
        version: "4.1-IV0",
        production: true,
    },
    MetadataVersionSpec {
        level: 27,
        release: "4.1",
        version: "4.1-IV1",
        production: true,
    },
    MetadataVersionSpec {
        level: 28,
        release: "4.2",
        version: "4.2-IV0",
        production: true,
    },
    MetadataVersionSpec {
        level: 29,
        release: "4.2",
        version: "4.2-IV1",
        production: true,
    },
    MetadataVersionSpec {
        level: 30,
        release: "4.3",
        version: "4.3-IV0",
        production: true,
    },
    MetadataVersionSpec {
        level: 31,
        release: "4.4",
        version: "4.4-IV0",
        production: false,
    },
    MetadataVersionSpec {
        level: 32,
        release: "4.4",
        version: "4.4-IV1",
        production: false,
    },
];

const PRODUCTION_FEATURES: &[&str] = &[
    "kraft.version",
    "transaction.version",
    "group.version",
    "eligible.leader.replicas.version",
    "share.version",
    "streams.version",
];

fn metadata_version(value: &str) -> Result<MetadataVersionSpec> {
    if let Some(version) = METADATA_VERSIONS
        .iter()
        .find(|version| version.version == value)
    {
        return Ok(*version);
    }
    let release = value.split('.').take(2).collect::<Vec<_>>().join(".");
    if let Some(version) = METADATA_VERSIONS
        .iter()
        .find(|version| version.version == release)
    {
        return Ok(*version);
    }
    METADATA_VERSIONS
        .iter()
        .rev()
        .find(|version| version.production && version.release == release)
        .copied()
        .ok_or_else(|| Error::Usage(format!("unknown metadata.version '{value}'")))
}

fn metadata_version_level(level: i16) -> Result<MetadataVersionSpec> {
    METADATA_VERSIONS
        .iter()
        .find(|version| version.level == level)
        .copied()
        .ok_or_else(|| Error::Usage(format!("unknown metadata.version {level}")))
}

fn feature_default_level(feature: &str, metadata_level: i16) -> i16 {
    match feature {
        "kraft.version" => i16::from(metadata_level >= 21),
        "transaction.version" => {
            if metadata_level >= 24 {
                2
            } else {
                0
            }
        }
        "group.version" => i16::from(metadata_level >= 22),
        "eligible.leader.replicas.version" => i16::from(metadata_level >= 26),
        "share.version" => {
            if metadata_level >= 31 {
                2
            } else {
                i16::from(metadata_level >= 28)
            }
        }
        "streams.version" => i16::from(metadata_level >= 29),
        _ => 0,
    }
}

fn validate_known_feature_level(feature: &str, level: i16) -> Result<()> {
    let maximum = match feature {
        "kraft.version"
        | "group.version"
        | "eligible.leader.replicas.version"
        | "streams.version" => 1,
        "transaction.version" | "share.version" => 2,
        _ => return Err(Error::Usage(format!("unknown feature: {feature}"))),
    };
    if (0..=maximum).contains(&level) {
        Ok(())
    } else {
        Err(Error::Usage(format!(
            "no feature {feature} with feature level {level}"
        )))
    }
}

#[derive(Debug, Serialize)]
struct FeatureMappingRow {
    feature: String,
    level: i16,
    release_version: Option<String>,
}

#[derive(Debug, Serialize)]
struct FeatureDependencyRow {
    feature: String,
    level: i16,
    dependency: Option<String>,
    dependency_level: Option<i16>,
    dependency_release: Option<String>,
}

fn features_local(format: OutputFormat, action: &FeatureAction) -> Result<()> {
    match action {
        FeatureAction::VersionMapping { release_version } => {
            let metadata = metadata_version(release_version.as_deref().unwrap_or("4.3-IV0"))?;
            let mut rows = vec![FeatureMappingRow {
                feature: "metadata.version".into(),
                level: metadata.level,
                release_version: Some(metadata.version.into()),
            }];
            rows.extend(PRODUCTION_FEATURES.iter().map(|feature| FeatureMappingRow {
                feature: (*feature).into(),
                level: feature_default_level(feature, metadata.level),
                release_version: None,
            }));
            output::write_value(format, "features.version-mapping", &rows, |rows| {
                output::table(
                    ["FEATURE", "LEVEL", "RELEASE_VERSION"],
                    rows.iter().map(|row| {
                        [
                            row.feature.clone(),
                            row.level.to_string(),
                            row.release_version.as_deref().unwrap_or("-").to_owned(),
                        ]
                    }),
                )
            })
        }
        FeatureAction::FeatureDependencies { feature } => {
            let parsed = parse_feature_levels(feature)?;
            let mut rows = Vec::new();
            for (name, level) in parsed {
                if name == "metadata.version" {
                    let metadata = metadata_version_level(level)?;
                    rows.push(FeatureDependencyRow {
                        feature: name,
                        level,
                        dependency: None,
                        dependency_level: None,
                        dependency_release: Some(metadata.version.into()),
                    });
                    continue;
                }
                validate_known_feature_level(&name, level)?;
                let dependency_level =
                    (name == "eligible.leader.replicas.version" && level == 1).then_some(23);
                rows.push(FeatureDependencyRow {
                    feature: name,
                    level,
                    dependency: dependency_level.map(|_| "metadata.version".into()),
                    dependency_level,
                    dependency_release: dependency_level
                        .map(metadata_version_level)
                        .transpose()?
                        .map(|version| version.version.into()),
                });
            }
            output::write_value(format, "features.feature-dependencies", &rows, |rows| {
                output::table(
                    [
                        "FEATURE",
                        "LEVEL",
                        "DEPENDENCY",
                        "DEPENDENCY_LEVEL",
                        "RELEASE_VERSION",
                    ],
                    rows.iter().map(|row| {
                        [
                            row.feature.clone(),
                            row.level.to_string(),
                            row.dependency.as_deref().unwrap_or("-").to_owned(),
                            row.dependency_level
                                .map_or_else(|| "-".into(), |level| level.to_string()),
                            row.dependency_release.as_deref().unwrap_or("-").to_owned(),
                        ]
                    }),
                )
            })
        }
        _ => Err(Error::Usage(
            "this features action requires a broker".into(),
        )),
    }
}

fn parse_feature_levels(values: &[String]) -> Result<BTreeMap<String, i16>> {
    let mut parsed = BTreeMap::new();
    for value in values {
        let (name, level) = value.split_once('=').ok_or_else(|| {
            Error::Usage(format!(
                "can't parse feature=level string {value}: equals sign not found"
            ))
        })?;
        let name = name.trim();
        let level = level.trim().parse::<i16>().map_err(|_| {
            Error::Usage(format!(
                "can't parse feature=level string {value}: invalid short level"
            ))
        })?;
        if name.is_empty() {
            return Err(Error::Usage(format!(
                "feature name cannot be empty in {value}"
            )));
        }
        if parsed.insert(name.to_owned(), level).is_some() {
            return Err(Error::Usage(format!(
                "feature {name} was specified more than once"
            )));
        }
    }
    Ok(parsed)
}

#[derive(Debug, Serialize)]
struct FeatureDescriptionRow {
    feature: String,
    supported_min_version: i16,
    supported_max_version: i16,
    finalized_version_level: i16,
    epoch: Option<i64>,
}

#[expect(
    clippy::too_many_lines,
    reason = "branches mirror Kafka FeatureCommand's describe and mutation result contracts"
)]
async fn features(
    bootstrap: &str,
    command_config: Option<&Path>,
    timeout: Duration,
    format: OutputFormat,
    action: &FeatureAction,
) -> Result<()> {
    match action {
        FeatureAction::Describe { node_id } => {
            if node_id.is_some_and(|node_id| node_id < 0) {
                return Err(Error::Usage("node ID must be non-negative".into()));
            }
            let client = config::protocol_admin(bootstrap, timeout, command_config).await?;
            let (supported, finalized, epoch) = if let Some(node_id) = node_id {
                let cluster = client.describe_cluster().await?;
                let endpoint = cluster
                    .brokers
                    .iter()
                    .find(|broker| broker.broker_id == *node_id)
                    .ok_or_else(|| Error::Usage(format!("node {node_id} was not described")))?;
                let address = format!("{}:{}", endpoint.host, endpoint.port);
                let connection = client
                    .pool()
                    .get_connection_by_id(*node_id, &address)
                    .await?;
                let request = ApiVersionsRequest::new()
                    .with_client_software("kafka-cli", env!("CARGO_PKG_VERSION"));
                let version = connection
                    .negotiate_api_version(ApiKey::ApiVersions, versions::API_VERSIONS_MAX, 3)
                    .await
                    .ok_or_else(|| {
                        Error::Unsupported(
                            "feature discovery requires broker ApiVersions v3+".into(),
                        )
                    })?;
                let mut bytes = connection
                    .send_request(ApiKey::ApiVersions, version, |buffer| {
                        request.encode_v3(buffer)
                    })
                    .await?;
                let response = krafka::protocol::ApiVersionsResponse::decode_v3(&mut bytes)?;
                if response.error_code != 0 {
                    return Err(Error::Config(format!(
                        "ApiVersions request to node {node_id} failed with error code {}",
                        response.error_code
                    )));
                }
                drop(connection);
                (
                    response.supported_features,
                    response.finalized_features,
                    response.finalized_features_epoch,
                )
            } else {
                let result = client.describe_features().await?;
                (
                    result.supported_features,
                    result.finalized_features,
                    result.finalized_features_epoch,
                )
            };
            drop(client);
            let finalized = finalized
                .into_iter()
                .map(|feature| (feature.name, feature.max_version_level))
                .collect::<BTreeMap<_, _>>();
            let mut rows = supported
                .into_iter()
                .map(|feature| FeatureDescriptionRow {
                    finalized_version_level: finalized.get(&feature.name).copied().unwrap_or(0),
                    feature: feature.name,
                    supported_min_version: feature.min_version,
                    supported_max_version: feature.max_version,
                    epoch: (epoch >= 0).then_some(epoch),
                })
                .collect::<Vec<_>>();
            rows.sort_by(|left, right| left.feature.cmp(&right.feature));
            output::write_value(format, "features.describe", &rows, |rows| {
                output::table(
                    [
                        "FEATURE",
                        "SUPPORTED_MIN_VERSION",
                        "SUPPORTED_MAX_VERSION",
                        "FINALIZED_VERSION_LEVEL",
                        "EPOCH",
                    ],
                    rows.iter().map(|row| {
                        [
                            row.feature.clone(),
                            feature_level_display(&row.feature, row.supported_min_version),
                            feature_level_display(&row.feature, row.supported_max_version),
                            feature_level_display(&row.feature, row.finalized_version_level),
                            row.epoch
                                .map_or_else(|| "-".into(), |epoch| epoch.to_string()),
                        ]
                    }),
                )
            })
        }
        FeatureAction::Upgrade { .. }
        | FeatureAction::Downgrade { .. }
        | FeatureAction::Disable { .. } => {
            let (operation, updates, dry_run) = feature_updates(action)?;
            let requested = updates
                .iter()
                .map(|update| (update.feature.clone(), update.max_version_level))
                .collect::<BTreeMap<_, _>>();
            let client = config::protocol_admin(bootstrap, timeout, command_config).await?;
            let result = client.update_features(updates, dry_run).await?;
            drop(client);
            let rows = result
                .results
                .into_iter()
                .map(|result| MutationRow {
                    resource: result.feature.clone(),
                    status: if result.error.is_some() {
                        "FAILED".into()
                    } else if dry_run {
                        format!(
                            "CAN {} TO {}",
                            operation.to_ascii_uppercase(),
                            requested.get(&result.feature).copied().unwrap_or_default()
                        )
                    } else if operation == "disable" {
                        "DISABLED".into()
                    } else {
                        format!(
                            "{}D TO {}",
                            operation.to_ascii_uppercase(),
                            requested.get(&result.feature).copied().unwrap_or_default()
                        )
                    },
                    error: result.error,
                })
                .collect::<Vec<_>>();
            let failures = rows.iter().filter(|row| row.error.is_some()).count();
            write_mutation_rows(format, &format!("features.{operation}"), &rows)?;
            if failures == 0 {
                Ok(())
            } else {
                Err(Error::Partial {
                    failed: failures,
                    total: rows.len(),
                })
            }
        }
        FeatureAction::VersionMapping { .. } | FeatureAction::FeatureDependencies { .. } => {
            features_local(format, action)
        }
    }
}

fn feature_level_display(feature: &str, level: i16) -> String {
    if feature == "metadata.version" {
        metadata_version_level(level).map_or_else(
            |_| format!("UNKNOWN {level}"),
            |version| version.version.to_owned(),
        )
    } else {
        level.to_string()
    }
}

fn feature_updates(
    action: &FeatureAction,
) -> Result<(&'static str, Vec<krafka::protocol::FeatureUpdateKey>, bool)> {
    let (operation, metadata, release_version, feature, unsafe_downgrade, dry_run) = match action {
        FeatureAction::Upgrade {
            metadata,
            release_version,
            feature,
            dry_run,
        } => (
            "upgrade",
            metadata,
            release_version,
            feature,
            false,
            *dry_run,
        ),
        FeatureAction::Downgrade {
            metadata,
            release_version,
            feature,
            r#unsafe,
            dry_run,
        } => (
            "downgrade",
            metadata,
            release_version,
            feature,
            *r#unsafe,
            *dry_run,
        ),
        FeatureAction::Disable {
            feature,
            r#unsafe,
            dry_run,
        } => {
            let mut seen = BTreeSet::new();
            let updates = feature
                .iter()
                .map(|name| {
                    if !seen.insert(name) {
                        return Err(Error::Usage(format!(
                            "feature {name} was specified more than once"
                        )));
                    }
                    Ok(if *r#unsafe {
                        krafka::protocol::FeatureUpdateKey::unsafe_downgrade(name, 0)
                    } else {
                        krafka::protocol::FeatureUpdateKey::delete(name)
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            return Ok(("disable", updates, *dry_run));
        }
        _ => return Err(Error::Usage("invalid feature update action".into())),
    };
    let mut levels = if let Some(release_version) = release_version {
        let metadata = metadata_version(release_version)?;
        let mut levels = BTreeMap::from([("metadata.version".to_owned(), metadata.level)]);
        levels.extend(PRODUCTION_FEATURES.iter().filter_map(|feature| {
            let level = feature_default_level(feature, metadata.level);
            (operation != "upgrade" || level > 0).then(|| ((*feature).to_owned(), level))
        }));
        levels
    } else {
        parse_feature_levels(feature)?
    };
    if let Some(metadata) = metadata {
        let version = metadata_version(metadata)?;
        if levels
            .insert("metadata.version".into(), version.level)
            .is_some()
        {
            return Err(Error::Usage(
                "feature metadata.version was specified more than once".into(),
            ));
        }
    }
    if levels.is_empty() {
        return Err(Error::Usage(format!(
            "you must specify at least one feature to {operation}"
        )));
    }
    let updates = levels
        .into_iter()
        .map(|(name, level)| {
            if operation == "upgrade" {
                krafka::protocol::FeatureUpdateKey::upgrade(name, level)
            } else if unsafe_downgrade {
                krafka::protocol::FeatureUpdateKey::unsafe_downgrade(name, level)
            } else {
                krafka::protocol::FeatureUpdateKey::safe_downgrade(name, level)
            }
        })
        .collect();
    Ok((operation, updates, dry_run))
}

#[derive(Debug, Serialize)]
struct TransactionListRow {
    transactional_id: String,
    coordinator: i32,
    producer_id: i64,
    transaction_state: String,
}

#[derive(Debug, Serialize)]
struct ProducerStateRow {
    producer_id: i64,
    producer_epoch: i32,
    latest_coordinator_epoch: i32,
    last_sequence: i32,
    last_timestamp: i64,
    current_transaction_start_offset: Option<i64>,
}

async fn transaction_list(
    client: &krafka::admin::AdminClient,
    duration_filter: Option<i64>,
    pattern: Option<&str>,
) -> Result<Vec<TransactionListRow>> {
    let cluster = client.describe_cluster().await?;
    let request = krafka::protocol::ListTransactionsRequest {
        state_filters: Vec::new(),
        producer_id_filters: Vec::new(),
        duration_filter: duration_filter.unwrap_or(-1),
        transactional_id_pattern: pattern.map(str::to_owned),
    };
    let mut rows = Vec::new();
    for broker in cluster.brokers {
        let address = format!("{}:{}", broker.host, broker.port);
        let connection = client
            .pool()
            .get_connection_by_id(broker.broker_id, &address)
            .await?;
        let version = connection
            .negotiate_api_version(
                ApiKey::ListTransactions,
                versions::LIST_TRANSACTIONS_MAX,
                versions::LIST_TRANSACTIONS_MIN,
            )
            .await
            .ok_or_else(|| Error::Unsupported("broker does not support ListTransactions".into()))?;
        if pattern.is_some() && version < 2 {
            return Err(Error::Unsupported(format!(
                "transactional-id-pattern requires ListTransactions v2; broker {} negotiated v{version}",
                broker.broker_id
            )));
        }
        let bytes = connection
            .send_request(ApiKey::ListTransactions, version, |buffer| {
                request.encode_versioned(version, buffer)
            })
            .await?;
        drop(connection);
        let response = krafka::protocol::ListTransactionsResponse::decode_versioned(
            version,
            &mut bytes.clone(),
        )?;
        if !response.error_code.is_ok() {
            return Err(Error::Config(format!(
                "ListTransactions failed on broker {}: {:?}",
                broker.broker_id, response.error_code
            )));
        }
        rows.extend(
            response
                .transaction_states
                .into_iter()
                .map(|entry| TransactionListRow {
                    transactional_id: entry.transactional_id,
                    coordinator: broker.broker_id,
                    producer_id: entry.producer_id,
                    transaction_state: entry.transaction_state,
                }),
        );
    }
    rows.sort_by(|left, right| left.transactional_id.cmp(&right.transactional_id));
    Ok(rows)
}

async fn transaction_coordinator(
    client: &krafka::admin::AdminClient,
    transactional_id: &str,
) -> Result<i32> {
    let cluster = client.describe_cluster().await?;
    let broker = cluster
        .brokers
        .first()
        .ok_or_else(|| Error::Config("cluster returned no brokers".into()))?;
    let address = format!("{}:{}", broker.host, broker.port);
    let connection = client
        .pool()
        .get_connection_by_id(broker.broker_id, &address)
        .await?;
    let version = connection
        .negotiate_api_version(ApiKey::FindCoordinator, 6, 1)
        .await
        .ok_or_else(|| Error::Unsupported("broker does not support FindCoordinator".into()))?;
    let request = krafka::protocol::FindCoordinatorRequest::for_transaction(transactional_id);
    let mut bytes = connection
        .send_request(ApiKey::FindCoordinator, version, |buffer| {
            request.encode_versioned(version, buffer)
        })
        .await?;
    drop(connection);
    let response =
        krafka::protocol::FindCoordinatorResponse::decode_versioned(version, &mut bytes)?;
    if !response.error_code.is_ok() {
        return Err(Error::Config(format!(
            "FindCoordinator failed for {transactional_id}: {:?}",
            response.error_code
        )));
    }
    Ok(response.node_id)
}

async fn transaction_producer_states(
    client: &krafka::admin::AdminClient,
    broker_id: Option<i32>,
    topic: &str,
    partition: i32,
) -> Result<Vec<ProducerStateRow>> {
    if partition < 0 {
        return Err(Error::Usage("partition must be non-negative".into()));
    }
    let result = if let Some(broker_id) = broker_id {
        let cluster = client.describe_cluster().await?;
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
        let request = krafka::protocol::DescribeProducersRequest {
            topics: vec![krafka::protocol::DescribeProducersTopicRequest {
                name: topic.into(),
                partition_indexes: vec![partition],
            }],
        };
        let version = connection
            .negotiate_api_version(ApiKey::DescribeProducers, 0, 0)
            .await
            .ok_or_else(|| {
                Error::Unsupported("broker does not support DescribeProducers".into())
            })?;
        let mut bytes = connection
            .send_request(ApiKey::DescribeProducers, version, |buffer| {
                request.encode_versioned(version, buffer)
            })
            .await?;
        drop(connection);
        krafka::protocol::DescribeProducersResponse::decode_versioned(version, &mut bytes)?.topics
    } else {
        client
            .describe_producers(&[(topic, &[partition])])
            .await?
            .into_iter()
            .map(|topic| krafka::protocol::DescribeProducersTopicResponse {
                name: topic.name,
                partitions: topic
                    .partitions
                    .into_iter()
                    .map(
                        |partition| krafka::protocol::DescribeProducersPartitionResponse {
                            partition_index: partition.partition_index,
                            error_code: krafka::error::ErrorCode::None,
                            error_message: partition.error,
                            active_producers: partition
                                .active_producers
                                .into_iter()
                                .map(|producer| krafka::protocol::ProducerState {
                                    producer_id: producer.producer_id,
                                    producer_epoch: producer.producer_epoch,
                                    last_sequence: producer.last_sequence,
                                    last_timestamp: producer.last_timestamp,
                                    coordinator_epoch: producer.coordinator_epoch,
                                    current_txn_start_offset: producer.current_txn_start_offset,
                                })
                                .collect(),
                        },
                    )
                    .collect(),
            })
            .collect()
    };
    let partition = result
        .into_iter()
        .flat_map(|topic| topic.partitions)
        .find(|candidate| candidate.partition_index == partition)
        .ok_or_else(|| {
            Error::Config(format!(
                "no producer state returned for {topic}-{partition}"
            ))
        })?;
    if let Some(error) = partition.error_message {
        return Err(Error::Config(error));
    }
    Ok(partition
        .active_producers
        .into_iter()
        .map(|producer| ProducerStateRow {
            producer_id: producer.producer_id,
            producer_epoch: producer.producer_epoch,
            latest_coordinator_epoch: producer.coordinator_epoch,
            last_sequence: producer.last_sequence,
            last_timestamp: producer.last_timestamp,
            current_transaction_start_offset: (producer.current_txn_start_offset >= 0)
                .then_some(producer.current_txn_start_offset),
        })
        .collect())
}

#[derive(Debug, Serialize)]
struct TransactionDescriptionRow {
    coordinator_id: i32,
    transactional_id: String,
    producer_id: i64,
    producer_epoch: i16,
    transaction_state: String,
    transaction_timeout_ms: i32,
    current_transaction_start_time_ms: Option<i64>,
    transaction_duration_ms: Option<i64>,
    topic_partitions: String,
}

#[derive(Debug, Serialize)]
struct HangingTransactionRow {
    topic: String,
    partition: i32,
    producer_id: i64,
    producer_epoch: i32,
    coordinator_epoch: i32,
    start_offset: i64,
    last_timestamp: i64,
    duration_minutes: i64,
}

#[expect(
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "branches mirror Kafka TransactionsCommand actions and share one protocol client"
)]
async fn transactions(
    client_config: &rdkafka::ClientConfig,
    bootstrap: &str,
    command_config: Option<&Path>,
    timeout: Duration,
    format: OutputFormat,
    action: &TransactionAction,
) -> Result<()> {
    let client = config::protocol_admin(bootstrap, timeout, command_config).await?;
    match action {
        TransactionAction::List {
            duration_filter,
            transactional_id_pattern,
        } => {
            let rows = transaction_list(
                &client,
                *duration_filter,
                transactional_id_pattern.as_deref(),
            )
            .await?;
            output::write_value(format, "transactions.list", &rows, |rows| {
                output::table(
                    [
                        "TransactionalId",
                        "Coordinator",
                        "ProducerId",
                        "TransactionState",
                    ],
                    rows.iter().map(|row| {
                        [
                            row.transactional_id.clone(),
                            row.coordinator.to_string(),
                            row.producer_id.to_string(),
                            row.transaction_state.clone(),
                        ]
                    }),
                )
            })
        }
        TransactionAction::Describe { transactional_id } => {
            let coordinator_id = transaction_coordinator(&client, transactional_id).await?;
            let description = client
                .describe_transactions(&[transactional_id.as_str()])
                .await?
                .into_iter()
                .next()
                .ok_or_else(|| {
                    Error::Config(format!("no description returned for {transactional_id}"))
                })?;
            if let Some(error) = description.error {
                return Err(Error::Config(error));
            }
            let now = Utc::now().timestamp_millis();
            let start = (description.start_time_ms >= 0).then_some(description.start_time_ms);
            let row = TransactionDescriptionRow {
                coordinator_id,
                transactional_id: description.transactional_id,
                producer_id: description.producer_id,
                producer_epoch: description.producer_epoch,
                transaction_state: description.state,
                transaction_timeout_ms: description.timeout_ms,
                current_transaction_start_time_ms: start,
                transaction_duration_ms: start.map(|start| now - start),
                topic_partitions: description
                    .topics
                    .iter()
                    .flat_map(|topic| {
                        topic
                            .partitions
                            .iter()
                            .map(|partition| format!("{}-{partition}", topic.topic))
                    })
                    .collect::<Vec<_>>()
                    .join(","),
            };
            output::write_value(format, "transactions.describe", &row, |row| {
                output::table(
                    [
                        "CoordinatorId",
                        "TransactionalId",
                        "ProducerId",
                        "ProducerEpoch",
                        "TransactionState",
                        "TransactionTimeoutMs",
                        "CurrentTransactionStartTimeMs",
                        "TransactionDurationMs",
                        "TopicPartitions",
                    ],
                    [[
                        row.coordinator_id.to_string(),
                        row.transactional_id.clone(),
                        row.producer_id.to_string(),
                        row.producer_epoch.to_string(),
                        row.transaction_state.clone(),
                        row.transaction_timeout_ms.to_string(),
                        row.current_transaction_start_time_ms
                            .map_or_else(|| "None".into(), |value| value.to_string()),
                        row.transaction_duration_ms
                            .map_or_else(|| "None".into(), |value| value.to_string()),
                        row.topic_partitions.clone(),
                    ]],
                )
            })
        }
        TransactionAction::DescribeProducers {
            broker_id,
            topic,
            partition,
        } => {
            let rows = transaction_producer_states(&client, *broker_id, topic, *partition).await?;
            output::write_value(format, "transactions.describe-producers", &rows, |rows| {
                output::table(
                    [
                        "ProducerId",
                        "ProducerEpoch",
                        "LatestCoordinatorEpoch",
                        "LastSequence",
                        "LastTimestamp",
                        "CurrentTransactionStartOffset",
                    ],
                    rows.iter().map(|row| {
                        [
                            row.producer_id.to_string(),
                            row.producer_epoch.to_string(),
                            row.latest_coordinator_epoch.to_string(),
                            row.last_sequence.to_string(),
                            row.last_timestamp.to_string(),
                            row.current_transaction_start_offset
                                .map_or_else(|| "None".into(), |value| value.to_string()),
                        ]
                    }),
                )
            })
        }
        TransactionAction::Abort {
            topic,
            partition,
            start_offset,
            producer_id,
            producer_epoch,
            coordinator_epoch,
        } => {
            let (producer_id, producer_epoch, coordinator_epoch) = if let Some(start_offset) =
                start_offset
            {
                let state = transaction_producer_states(&client, None, topic, *partition)
                    .await?
                    .into_iter()
                    .find(|state| state.current_transaction_start_offset == Some(*start_offset))
                    .ok_or_else(|| Error::Usage(format!("could not find any open transactions starting at offset {start_offset} on partition {topic}-{partition}")))?;
                (
                    state.producer_id,
                    i16::try_from(state.producer_epoch)
                        .map_err(|_| Error::Config("producer epoch exceeds i16".into()))?,
                    state.latest_coordinator_epoch.max(0),
                )
            } else if let (Some(producer_id), Some(producer_epoch), Some(coordinator_epoch)) =
                (producer_id, producer_epoch, coordinator_epoch)
            {
                (*producer_id, *producer_epoch, (*coordinator_epoch).max(0))
            } else {
                return Err(Error::Usage("the transaction must be identified with --start-offset or --producer-id, --producer-epoch, and --coordinator-epoch".into()));
            };
            let results = client
                .write_txn_markers(&[krafka::protocol::WritableTxnMarker {
                    producer_id,
                    producer_epoch,
                    transaction_result: false,
                    topics: vec![krafka::protocol::WritableTxnMarkerTopic {
                        name: topic.clone(),
                        partition_indexes: vec![*partition],
                    }],
                    coordinator_epoch,
                    transaction_version:
                        krafka::protocol::WritableTxnMarker::legacy_transaction_version(),
                }])
                .await?;
            let errors = results
                .iter()
                .flat_map(|result| &result.topics)
                .flat_map(|topic| &topic.partitions)
                .filter_map(|partition| partition.error.clone())
                .collect::<Vec<_>>();
            if !errors.is_empty() {
                return Err(Error::Partial {
                    failed: errors.len(),
                    total: 1,
                });
            }
            Ok(())
        }
        TransactionAction::ForceTerminateTransaction { transactional_id } => {
            let mut config = client_config.clone();
            config.set("transactional.id", transactional_id);
            let producer: FutureProducer = config.create()?;
            producer.init_transactions(timeout)?;
            Ok(())
        }
        TransactionAction::FindHanging {
            broker_id,
            max_transaction_timeout,
            topic,
            partition,
        } => {
            if topic.is_none() && broker_id.is_none() {
                return Err(Error::Usage(
                    "find-hanging requires either --topic or --broker-id".into(),
                ));
            }
            if *max_transaction_timeout < 0 {
                return Err(Error::Usage(
                    "max transaction timeout must be non-negative".into(),
                ));
            }
            let metadata_client: BaseConsumer = client_config.create()?;
            let metadata = metadata_client.fetch_metadata(topic.as_deref(), timeout)?;
            let mut targets = Vec::new();
            for metadata_topic in metadata.topics() {
                if topic
                    .as_ref()
                    .is_some_and(|name| name != metadata_topic.name())
                {
                    continue;
                }
                for metadata_partition in metadata_topic.partitions() {
                    if partition.is_some_and(|wanted| wanted != metadata_partition.id()) {
                        continue;
                    }
                    if broker_id
                        .is_some_and(|broker| !metadata_partition.replicas().contains(&broker))
                    {
                        continue;
                    }
                    targets.push((metadata_topic.name().to_owned(), metadata_partition.id()));
                }
            }
            let now = Utc::now().timestamp_millis();
            let threshold = i64::from(*max_transaction_timeout) * 60_000;
            let mut candidates = Vec::new();
            for (topic, partition) in targets {
                for state in
                    transaction_producer_states(&client, *broker_id, &topic, partition).await?
                {
                    if state.current_transaction_start_offset.is_some()
                        && now - state.last_timestamp > threshold
                    {
                        candidates.push((topic.clone(), partition, state));
                    }
                }
            }
            let producer_ids = candidates
                .iter()
                .map(|(_, _, state)| state.producer_id)
                .collect::<Vec<_>>();
            let listings = client
                .list_transactions(&[], &producer_ids, -1, None)
                .await?;
            let transaction_ids = listings
                .transactions
                .iter()
                .map(|entry| entry.transactional_id.as_str())
                .collect::<Vec<_>>();
            let descriptions = client.describe_transactions(&transaction_ids).await?;
            let mut rows = Vec::new();
            for (topic, partition, state) in candidates {
                let transactional_id = listings
                    .transactions
                    .iter()
                    .find(|entry| entry.producer_id == state.producer_id)
                    .map(|entry| entry.transactional_id.as_str());
                let still_owned = transactional_id
                    .and_then(|id| {
                        descriptions
                            .iter()
                            .find(|description| description.transactional_id == id)
                    })
                    .is_some_and(|description| {
                        description.topics.iter().any(|candidate| {
                            candidate.topic == topic && candidate.partitions.contains(&partition)
                        })
                    });
                if !still_owned {
                    rows.push(HangingTransactionRow {
                        topic,
                        partition,
                        producer_id: state.producer_id,
                        producer_epoch: state.producer_epoch,
                        coordinator_epoch: state.latest_coordinator_epoch,
                        start_offset: state.current_transaction_start_offset.unwrap_or(-1),
                        last_timestamp: state.last_timestamp,
                        duration_minutes: (now - state.last_timestamp) / 60_000,
                    });
                }
            }
            output::write_value(format, "transactions.find-hanging", &rows, |rows| {
                output::table(
                    [
                        "Topic",
                        "Partition",
                        "ProducerId",
                        "ProducerEpoch",
                        "CoordinatorEpoch",
                        "StartOffset",
                        "LastTimestamp",
                        "Duration(min)",
                    ],
                    rows.iter().map(|row| {
                        [
                            row.topic.clone(),
                            row.partition.to_string(),
                            row.producer_id.to_string(),
                            row.producer_epoch.to_string(),
                            row.coordinator_epoch.to_string(),
                            row.start_offset.to_string(),
                            row.last_timestamp.to_string(),
                            row.duration_minutes.to_string(),
                        ]
                    }),
                )
            })
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct QuorumReplica {
    node_id: i32,
    directory_id: String,
    log_end_offset: i64,
    last_fetch_timestamp: Option<i64>,
    last_caught_up_timestamp: Option<i64>,
}

#[derive(Debug, Clone)]
struct QuorumNode {
    node_id: i32,
    endpoints: Vec<String>,
}

#[derive(Debug)]
struct QuorumDescription {
    leader_id: i32,
    leader_epoch: i32,
    high_watermark: i64,
    voters: Vec<QuorumReplica>,
    observers: Vec<QuorumReplica>,
    nodes: Vec<QuorumNode>,
}

fn decode_unsigned_varint(buffer: &mut impl Buf) -> Result<usize> {
    let mut value = 0usize;
    for shift in (0..35).step_by(7) {
        if !buffer.has_remaining() {
            return Err(Error::Config("truncated unsigned varint".into()));
        }
        let byte = buffer.get_u8();
        value |= usize::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(Error::Config("unsigned varint is too long".into()))
}

fn decode_compact_len(buffer: &mut impl Buf) -> Result<usize> {
    decode_unsigned_varint(buffer)?
        .checked_sub(1)
        .ok_or_else(|| Error::Config("unexpected null compact value".into()))
}

fn decode_compact_string(buffer: &mut impl Buf) -> Result<String> {
    let length = decode_compact_len(buffer)?;
    if buffer.remaining() < length {
        return Err(Error::Config("truncated compact string".into()));
    }
    String::from_utf8(buffer.copy_to_bytes(length).to_vec())
        .map_err(|error| Error::Config(format!("invalid UTF-8: {error}")))
}

fn decode_nullable_compact_string(buffer: &mut impl Buf) -> Result<Option<String>> {
    let encoded = decode_unsigned_varint(buffer)?;
    if encoded == 0 {
        return Ok(None);
    }
    let length = encoded - 1;
    if buffer.remaining() < length {
        return Err(Error::Config("truncated nullable compact string".into()));
    }
    String::from_utf8(buffer.copy_to_bytes(length).to_vec())
        .map(Some)
        .map_err(|error| Error::Config(format!("invalid UTF-8: {error}")))
}

fn skip_tagged_fields(buffer: &mut impl Buf) -> Result<()> {
    let count = decode_unsigned_varint(buffer)?;
    for _ in 0..count {
        let _tag = decode_unsigned_varint(buffer)?;
        let size = decode_unsigned_varint(buffer)?;
        if buffer.remaining() < size {
            return Err(Error::Config("truncated tagged field".into()));
        }
        buffer.advance(size);
    }
    Ok(())
}

fn decode_kafka_uuid(buffer: &mut impl Buf) -> Result<String> {
    if buffer.remaining() < 16 {
        return Err(Error::Config("truncated Kafka UUID".into()));
    }
    Ok(URL_SAFE_NO_PAD.encode(buffer.copy_to_bytes(16)))
}

fn decode_quorum_replicas(buffer: &mut impl Buf, version: i16) -> Result<Vec<QuorumReplica>> {
    let count = decode_compact_len(buffer)?;
    let mut replicas = Vec::with_capacity(count);
    for _ in 0..count {
        if buffer.remaining() < 4 {
            return Err(Error::Config("truncated quorum replica".into()));
        }
        let node_id = buffer.get_i32();
        let directory_id = if version >= 2 {
            decode_kafka_uuid(buffer)?
        } else {
            ZERO_TOPIC_ID.into()
        };
        if buffer.remaining() < 8 {
            return Err(Error::Config("truncated quorum replica offset".into()));
        }
        let log_end_offset = buffer.get_i64();
        let (last_fetch_timestamp, last_caught_up_timestamp) = if version >= 1 {
            if buffer.remaining() < 16 {
                return Err(Error::Config("truncated quorum replica timestamps".into()));
            }
            let fetch = buffer.get_i64();
            let caught_up = buffer.get_i64();
            (
                (fetch >= 0).then_some(fetch),
                (caught_up >= 0).then_some(caught_up),
            )
        } else {
            (None, None)
        };
        skip_tagged_fields(buffer)?;
        replicas.push(QuorumReplica {
            node_id,
            directory_id,
            log_end_offset,
            last_fetch_timestamp,
            last_caught_up_timestamp,
        });
    }
    Ok(replicas)
}

fn decode_quorum_response(mut buffer: impl Buf, version: i16) -> Result<QuorumDescription> {
    if buffer.remaining() < 2 {
        return Err(Error::Config("truncated DescribeQuorum response".into()));
    }
    let top_error = buffer.get_i16();
    let top_message = if version >= 2 {
        decode_nullable_compact_string(&mut buffer)?
    } else {
        None
    };
    if top_error != 0 {
        return Err(Error::Config(top_message.unwrap_or_else(|| {
            format!("DescribeQuorum failed with error code {top_error}")
        })));
    }
    let topic_count = decode_compact_len(&mut buffer)?;
    let mut description = None;
    for _ in 0..topic_count {
        let topic = decode_compact_string(&mut buffer)?;
        let partition_count = decode_compact_len(&mut buffer)?;
        for _ in 0..partition_count {
            if buffer.remaining() < 6 {
                return Err(Error::Config("truncated quorum partition".into()));
            }
            let partition = buffer.get_i32();
            let error_code = buffer.get_i16();
            let error_message = if version >= 2 {
                decode_nullable_compact_string(&mut buffer)?
            } else {
                None
            };
            if error_code != 0 {
                return Err(Error::Config(error_message.unwrap_or_else(|| {
                    format!(
                        "DescribeQuorum failed for {topic}-{partition}: error code {error_code}"
                    )
                })));
            }
            if buffer.remaining() < 16 {
                return Err(Error::Config("truncated quorum partition state".into()));
            }
            let leader_id = buffer.get_i32();
            let leader_epoch = buffer.get_i32();
            let high_watermark = buffer.get_i64();
            let voters = decode_quorum_replicas(&mut buffer, version)?;
            let observers = decode_quorum_replicas(&mut buffer, version)?;
            skip_tagged_fields(&mut buffer)?;
            if topic == "__cluster_metadata" && partition == 0 {
                description = Some(QuorumDescription {
                    leader_id,
                    leader_epoch,
                    high_watermark,
                    voters,
                    observers,
                    nodes: Vec::new(),
                });
            }
        }
        skip_tagged_fields(&mut buffer)?;
    }
    let mut nodes = Vec::new();
    if version >= 2 {
        for _ in 0..decode_compact_len(&mut buffer)? {
            if buffer.remaining() < 4 {
                return Err(Error::Config("truncated quorum node".into()));
            }
            let node_id = buffer.get_i32();
            let mut endpoints = Vec::new();
            for _ in 0..decode_compact_len(&mut buffer)? {
                let name = decode_compact_string(&mut buffer)?;
                let host = decode_compact_string(&mut buffer)?;
                if buffer.remaining() < 2 {
                    return Err(Error::Config("truncated quorum listener port".into()));
                }
                let port = buffer.get_u16();
                skip_tagged_fields(&mut buffer)?;
                let host = if host.contains(':') {
                    format!("[{host}]")
                } else {
                    host
                };
                endpoints.push(format!("{name}://{host}:{port}"));
            }
            skip_tagged_fields(&mut buffer)?;
            nodes.push(QuorumNode { node_id, endpoints });
        }
    }
    skip_tagged_fields(&mut buffer)?;
    let mut description = description
        .ok_or_else(|| Error::Config("metadata quorum partition was not returned".into()))?;
    description.nodes = nodes;
    Ok(description)
}

async fn describe_metadata_quorum(
    client: &krafka::admin::AdminClient,
) -> Result<QuorumDescription> {
    let connection = client.get_controller_connection().await?;
    let version = connection
        .negotiate_api_version(ApiKey::DescribeQuorum, 2, 0)
        .await
        .ok_or_else(|| Error::Unsupported("controller does not support DescribeQuorum".into()))?;
    let request = krafka::protocol::DescribeQuorumRequest {
        topics: vec![krafka::protocol::DescribeQuorumTopicRequest {
            topic_name: "__cluster_metadata".into(),
            partitions: vec![krafka::protocol::DescribeQuorumPartitionRequest {
                partition_index: 0,
            }],
        }],
    };
    let bytes = connection
        .send_request(ApiKey::DescribeQuorum, version, |buffer| {
            request.encode_v0(buffer)
        })
        .await?;
    drop(connection);
    decode_quorum_response(bytes, version)
}

fn encode_unsigned_varint(mut value: usize, buffer: &mut BytesMut) {
    loop {
        let mut byte = u8::try_from(value & 0x7f).unwrap_or_default();
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        buffer.put_u8(byte);
        if value == 0 {
            return;
        }
    }
}

fn encode_compact_string(value: &str, buffer: &mut BytesMut) {
    encode_unsigned_varint(value.len() + 1, buffer);
    buffer.put_slice(value.as_bytes());
}

fn encode_nullable_compact_string(value: Option<&str>, buffer: &mut BytesMut) {
    if let Some(value) = value {
        encode_compact_string(value, buffer);
    } else {
        buffer.put_u8(0);
    }
}

fn parse_kafka_uuid(value: &str, field: &str) -> Result<[u8; 16]> {
    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|error| Error::Usage(format!("failed to parse {field}: {error}")))?;
    decoded
        .try_into()
        .map_err(|_| Error::Usage(format!("failed to parse {field}: expected a Kafka UUID")))
}

#[derive(Debug, Clone)]
struct ControllerEndpoint {
    name: String,
    host: String,
    port: u16,
}

fn parse_controller_endpoint(value: &str) -> Result<ControllerEndpoint> {
    let (name, address) = value
        .split_once("://")
        .ok_or_else(|| Error::Config(format!("invalid controller listener: {value}")))?;
    let (host, port) = address
        .rsplit_once(':')
        .ok_or_else(|| Error::Config(format!("controller listener has no port: {value}")))?;
    let host = host.trim_matches(['[', ']']);
    let port = port
        .parse::<u16>()
        .map_err(|error| Error::Config(format!("invalid controller listener port: {error}")))?;
    Ok(ControllerEndpoint {
        name: name.to_ascii_uppercase(),
        host: if host.is_empty() {
            "localhost".into()
        } else {
            host.into()
        },
        port,
    })
}

fn controller_from_properties(path: &Path) -> Result<(i32, [u8; 16], Vec<ControllerEndpoint>)> {
    let properties = config::load_properties(path)?;
    let controller_id = properties
        .get("node.id")
        .ok_or_else(|| Error::Config("node.id not found in controller configuration".into()))?
        .parse::<i32>()
        .map_err(|error| Error::Config(format!("invalid node.id: {error}")))?;
    if controller_id < 0 {
        return Err(Error::Config("node.id was negative".into()));
    }
    if !properties
        .get("process.roles")
        .is_some_and(|roles| roles.split(',').any(|role| role.trim() == "controller"))
    {
        return Err(Error::Config(
            "process.roles did not contain controller".into(),
        ));
    }
    let metadata_directory = properties
        .get("metadata.log.dir")
        .cloned()
        .or_else(|| {
            properties
                .get("log.dirs")
                .and_then(|dirs| dirs.split(',').next().map(str::trim).map(str::to_owned))
        })
        .ok_or_else(|| Error::Config("neither metadata.log.dir nor log.dirs was found".into()))?;
    let meta_properties =
        config::load_properties(&Path::new(&metadata_directory).join("meta.properties"))?;
    let directory_id = parse_kafka_uuid(
        meta_properties
            .get("directory.id")
            .ok_or_else(|| Error::Config("directory.id not found in meta.properties".into()))?,
        "directory.id",
    )?;
    let listener_names = properties
        .get("controller.listener.names")
        .ok_or_else(|| Error::Config("controller.listener.names was not found".into()))?
        .split(',')
        .map(|name| name.trim().to_ascii_uppercase())
        .collect::<BTreeSet<_>>();
    let mut listeners = BTreeMap::new();
    for key in ["listeners", "advertised.listeners"] {
        if let Some(values) = properties.get(key) {
            for value in values.split(',') {
                let endpoint = parse_controller_endpoint(value.trim())?;
                listeners.insert(endpoint.name.clone(), endpoint);
            }
        }
    }
    let endpoints = listener_names
        .into_iter()
        .map(|name| {
            listeners.get(&name).cloned().ok_or_else(|| {
                Error::Config(format!(
                    "cannot find controller listener information for {name}"
                ))
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((controller_id, directory_id, endpoints))
}

async fn alter_raft_voter(
    client: &krafka::admin::AdminClient,
    api_key: i16,
    timeout: Duration,
    encode: impl FnOnce(&mut BytesMut, i16),
) -> Result<()> {
    let connection = client.get_controller_connection().await?;
    let key = ApiKey::Unknown(api_key);
    let maximum = i16::from(api_key == 80);
    let version = connection
        .negotiate_api_version(key, maximum, 0)
        .await
        .ok_or_else(|| Error::Unsupported(format!("controller does not support API {api_key}")))?;
    let mut bytes = connection
        .send_request_with_timeout(key, version, timeout, |buffer| {
            encode(buffer, version);
            Ok(())
        })
        .await?;
    drop(connection);
    if bytes.remaining() < 6 {
        return Err(Error::Config("truncated raft voter response".into()));
    }
    let _throttle_time_ms = bytes.get_i32();
    let error_code = bytes.get_i16();
    let message = decode_nullable_compact_string(&mut bytes)?;
    skip_tagged_fields(&mut bytes)?;
    if error_code != 0 {
        return Err(Error::Config(message.unwrap_or_else(|| {
            format!("raft voter request failed with error code {error_code}")
        })));
    }
    Ok(())
}

fn relative_timestamp(timestamp: Option<i64>, human_readable: bool, now: i64) -> Result<String> {
    let Some(timestamp) = timestamp else {
        return Ok("-1".into());
    };
    if !human_readable {
        return Ok(timestamp.to_string());
    }
    if timestamp <= 0 || timestamp > now {
        return Err(Error::Config(format!(
            "cannot compute relative quorum timestamp {timestamp}; possible system clock drift"
        )));
    }
    Ok(format!("{} ms ago", now - timestamp))
}

#[derive(Debug, Serialize)]
struct QuorumReplicationRow {
    node_id: i32,
    directory_id: String,
    log_end_offset: i64,
    lag: i64,
    last_fetch_timestamp: String,
    last_caught_up_timestamp: String,
    status: String,
}

#[derive(Debug, Serialize)]
struct QuorumMutationRow {
    action: String,
    controller_id: i32,
    directory_id: String,
    endpoints: Vec<String>,
    dry_run: bool,
}

fn quorum_member_json(description: &QuorumDescription, members: &[QuorumReplica]) -> String {
    let values = members
        .iter()
        .map(|member| {
            let endpoints = description
                .nodes
                .iter()
                .find(|node| node.node_id == member.node_id)
                .map_or(&[][..], |node| node.endpoints.as_slice());
            serde_json::json!({
                "id": member.node_id,
                "directoryId": (member.directory_id != ZERO_TOPIC_ID).then_some(&member.directory_id),
                "endpoints": endpoints,
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&values).unwrap_or_else(|_| "[]".into())
}

#[expect(
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "branches mirror Kafka MetadataQuorumCommand actions and share one protocol client"
)]
async fn metadata_quorum(
    bootstrap: &str,
    command_config: Option<&Path>,
    timeout: Duration,
    format: OutputFormat,
    action: &MetadataQuorumAction,
) -> Result<()> {
    let client = config::protocol_admin(bootstrap, timeout, command_config).await?;
    match action {
        MetadataQuorumAction::Describe {
            status,
            replication,
            human_readable,
        } => {
            let description = describe_metadata_quorum(&client).await?;
            if *status {
                let cluster_id = client.describe_cluster().await?.cluster_id;
                let leader = description
                    .voters
                    .iter()
                    .find(|voter| voter.node_id == description.leader_id)
                    .ok_or_else(|| Error::Config("quorum leader was not in voter set".into()))?;
                let slowest = description
                    .voters
                    .iter()
                    .min_by_key(|voter| voter.log_end_offset)
                    .ok_or_else(|| Error::Config("metadata quorum has no voters".into()))?;
                let max_lag_time = if leader.node_id == slowest.node_id {
                    0
                } else {
                    leader
                        .last_caught_up_timestamp
                        .zip(slowest.last_caught_up_timestamp)
                        .map_or(-1, |(leader, follower)| leader - follower)
                };
                let rows = vec![
                    ("ClusterId".to_owned(), cluster_id),
                    ("LeaderId".into(), description.leader_id.to_string()),
                    ("LeaderEpoch".into(), description.leader_epoch.to_string()),
                    (
                        "HighWatermark".into(),
                        description.high_watermark.to_string(),
                    ),
                    (
                        "MaxFollowerLag".into(),
                        (leader.log_end_offset - slowest.log_end_offset).to_string(),
                    ),
                    ("MaxFollowerLagTimeMs".into(), max_lag_time.to_string()),
                    (
                        "CurrentVoters".into(),
                        quorum_member_json(&description, &description.voters),
                    ),
                    (
                        "CurrentObservers".into(),
                        quorum_member_json(&description, &description.observers),
                    ),
                ];
                output::write_value(format, "metadata-quorum.describe-status", &rows, |rows| {
                    output::table(["FIELD", "VALUE"], rows.iter().cloned().map(Into::into))
                })
            } else if *replication {
                let leader = description
                    .voters
                    .iter()
                    .find(|voter| voter.node_id == description.leader_id)
                    .ok_or_else(|| Error::Config("quorum leader was not in voter set".into()))?;
                let now = Utc::now().timestamp_millis();
                let mut replicas = description
                    .voters
                    .iter()
                    .map(|voter| {
                        (
                            voter,
                            if voter.node_id == description.leader_id {
                                "Leader"
                            } else {
                                "Follower"
                            },
                        )
                    })
                    .chain(
                        description
                            .observers
                            .iter()
                            .map(|observer| (observer, "Observer")),
                    )
                    .collect::<Vec<_>>();
                replicas.sort_by_key(|(replica, status)| (*status != "Leader", replica.node_id));
                let rows = replicas
                    .into_iter()
                    .map(|(replica, status)| {
                        Ok(QuorumReplicationRow {
                            node_id: replica.node_id,
                            directory_id: replica.directory_id.clone(),
                            log_end_offset: replica.log_end_offset,
                            lag: leader.log_end_offset - replica.log_end_offset,
                            last_fetch_timestamp: relative_timestamp(
                                replica.last_fetch_timestamp,
                                *human_readable,
                                now,
                            )?,
                            last_caught_up_timestamp: relative_timestamp(
                                replica.last_caught_up_timestamp,
                                *human_readable,
                                now,
                            )?,
                            status: status.into(),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                output::write_value(
                    format,
                    "metadata-quorum.describe-replication",
                    &rows,
                    |rows| {
                        output::table(
                            [
                                "NodeId",
                                "DirectoryId",
                                "LogEndOffset",
                                "Lag",
                                "LastFetchTimestamp",
                                "LastCaughtUpTimestamp",
                                "Status",
                            ],
                            rows.iter().map(|row| {
                                [
                                    row.node_id.to_string(),
                                    row.directory_id.clone(),
                                    row.log_end_offset.to_string(),
                                    row.lag.to_string(),
                                    row.last_fetch_timestamp.clone(),
                                    row.last_caught_up_timestamp.clone(),
                                    row.status.clone(),
                                ]
                            }),
                        )
                    },
                )
            } else {
                Err(Error::Usage(
                    "one of --status or --replication is required".into(),
                ))
            }
        }
        MetadataQuorumAction::AddController { dry_run } => {
            let path = command_config.ok_or_else(|| {
                Error::Usage("--command-config is required for add-controller".into())
            })?;
            let (controller_id, directory_id, endpoints) = controller_from_properties(path)?;
            let cluster_id = client.describe_cluster().await?.cluster_id;
            if !dry_run {
                alter_raft_voter(&client, 80, timeout, |buffer, version| {
                    encode_nullable_compact_string(Some(&cluster_id), buffer);
                    buffer.put_i32(i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX));
                    buffer.put_i32(controller_id);
                    buffer.put_slice(&directory_id);
                    encode_unsigned_varint(endpoints.len() + 1, buffer);
                    for endpoint in &endpoints {
                        encode_compact_string(&endpoint.name, buffer);
                        encode_compact_string(&endpoint.host, buffer);
                        buffer.put_u16(endpoint.port);
                        buffer.put_u8(0);
                    }
                    if version >= 1 {
                        buffer.put_u8(1);
                    }
                    buffer.put_u8(0);
                })
                .await?;
            }
            let row = QuorumMutationRow {
                action: "add".into(),
                controller_id,
                directory_id: URL_SAFE_NO_PAD.encode(directory_id),
                endpoints: endpoints
                    .iter()
                    .map(|endpoint| {
                        format!("{}://{}:{}", endpoint.name, endpoint.host, endpoint.port)
                    })
                    .collect(),
                dry_run: *dry_run,
            };
            output::write_value(format, "metadata-quorum.add-controller", &row, |row| {
                output::table(
                    [
                        "ACTION",
                        "CONTROLLER_ID",
                        "DIRECTORY_ID",
                        "ENDPOINTS",
                        "STATUS",
                    ],
                    [[
                        row.action.clone(),
                        row.controller_id.to_string(),
                        row.directory_id.clone(),
                        row.endpoints.join(", "),
                        if row.dry_run {
                            "DRY_RUN".into()
                        } else {
                            "ADDED".into()
                        },
                    ]],
                )
            })
        }
        MetadataQuorumAction::RemoveController {
            controller_id,
            controller_directory_id,
            dry_run,
        } => {
            if *controller_id < 0 {
                return Err(Error::Usage(format!(
                    "invalid negative --controller-id: {controller_id}"
                )));
            }
            let directory_id =
                parse_kafka_uuid(controller_directory_id, "--controller-directory-id")?;
            let cluster_id = client.describe_cluster().await?.cluster_id;
            if !dry_run {
                alter_raft_voter(&client, 81, timeout, |buffer, _| {
                    encode_nullable_compact_string(Some(&cluster_id), buffer);
                    buffer.put_i32(*controller_id);
                    buffer.put_slice(&directory_id);
                    buffer.put_u8(0);
                })
                .await?;
            }
            let row = QuorumMutationRow {
                action: "remove".into(),
                controller_id: *controller_id,
                directory_id: controller_directory_id.clone(),
                endpoints: Vec::new(),
                dry_run: *dry_run,
            };
            output::write_value(format, "metadata-quorum.remove-controller", &row, |row| {
                output::table(
                    ["ACTION", "CONTROLLER_ID", "DIRECTORY_ID", "STATUS"],
                    [[
                        row.action.clone(),
                        row.controller_id.to_string(),
                        row.directory_id.clone(),
                        if row.dry_run {
                            "DRY_RUN".into()
                        } else {
                            "REMOVED".into()
                        },
                    ]],
                )
            })
        }
    }
}

fn parse_kafka_principals(values: &[String], option: &str) -> Result<Vec<(String, String)>> {
    values
        .iter()
        .map(|value| {
            let value = value.trim();
            let (principal_type, principal_name) = value.split_once(':').ok_or_else(|| {
                Error::Usage(format!(
                    "invalid {option} '{value}'; expected principalType:name"
                ))
            })?;
            if principal_type.is_empty() || principal_name.is_empty() {
                return Err(Error::Usage(format!(
                    "invalid {option} '{value}'; principal type and name must be non-empty"
                )));
            }
            Ok((principal_type.to_owned(), principal_name.to_owned()))
        })
        .collect()
}

fn delegation_timestamp(timestamp: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(timestamp).map_or_else(
        || timestamp.to_string(),
        |timestamp| timestamp.format("%Y-%m-%dT%H:%M").to_string(),
    )
}

#[derive(Debug, Serialize)]
struct DelegationTokenRow {
    token_id: String,
    hmac: String,
    owner: String,
    requester: String,
    renewers: String,
    issue_date: String,
    expiry_date: String,
    max_date: String,
}

fn delegation_token_row(token: krafka::protocol::DelegationTokenInfo) -> DelegationTokenRow {
    let owner = format!("{}:{}", token.principal_type, token.principal_name);
    let requester = token
        .token_requester_principal_type
        .zip(token.token_requester_principal_name)
        .map_or_else(
            || owner.clone(),
            |(principal_type, principal_name)| format!("{principal_type}:{principal_name}"),
        );
    DelegationTokenRow {
        token_id: token.token_id,
        hmac: STANDARD.encode(token.hmac),
        owner,
        requester,
        renewers: token
            .renewers
            .iter()
            .map(|renewer| format!("{}:{}", renewer.principal_type, renewer.principal_name))
            .collect::<Vec<_>>()
            .join(","),
        issue_date: delegation_timestamp(token.issue_timestamp_ms),
        expiry_date: delegation_timestamp(token.expiry_timestamp_ms),
        max_date: delegation_timestamp(token.max_timestamp_ms),
    }
}

fn write_delegation_tokens(
    format: OutputFormat,
    command: &str,
    rows: &[DelegationTokenRow],
) -> Result<()> {
    output::write_value(format, command, &rows, |rows| {
        output::table(
            [
                "TOKENID",
                "HMAC",
                "OWNER",
                "REQUESTER",
                "RENEWERS",
                "ISSUEDATE",
                "EXPIRYDATE",
                "MAXDATE",
            ],
            rows.iter().map(|row| {
                [
                    row.token_id.clone(),
                    row.hmac.clone(),
                    row.owner.clone(),
                    row.requester.clone(),
                    row.renewers.clone(),
                    row.issue_date.clone(),
                    row.expiry_date.clone(),
                    row.max_date.clone(),
                ]
            }),
        )
    })
}

async fn delegation_broker_connection(
    client: &krafka::admin::AdminClient,
) -> Result<std::sync::Arc<krafka::network::BrokerConnection>> {
    let cluster = client.describe_cluster().await?;
    let broker = cluster
        .brokers
        .first()
        .ok_or_else(|| Error::Config("cluster returned no brokers".into()))?;
    Ok(client
        .pool()
        .get_connection_by_id(
            broker.broker_id,
            &format!("{}:{}", broker.host, broker.port),
        )
        .await?)
}

#[derive(Debug, Serialize)]
struct DelegationTokenExpiryRow {
    action: String,
    expiry_timestamp_ms: i64,
    expiry_date: String,
}

#[expect(
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "branches mirror Kafka DelegationTokenCommand's four actions"
)]
async fn delegation_tokens(
    bootstrap: &str,
    command_config: Option<&Path>,
    timeout: Duration,
    format: OutputFormat,
    action: &DelegationTokenAction,
) -> Result<()> {
    let command_config = command_config.ok_or_else(|| {
        Error::Usage("--command-config is required for delegation token operations".into())
    })?;
    let client = config::protocol_admin(bootstrap, timeout, Some(command_config)).await?;
    match action {
        DelegationTokenAction::Create {
            owner_principal,
            renewer_principal,
            max_life_time_period,
        } => {
            if *max_life_time_period < -1 {
                return Err(Error::Usage(
                    "--max-life-time-period must be -1 or non-negative".into(),
                ));
            }
            let owners = parse_kafka_principals(owner_principal, "--owner-principal")?;
            if owners.len() > 1 {
                return Err(Error::Usage(
                    "--owner-principal may be supplied at most once for create".into(),
                ));
            }
            let renewers = parse_kafka_principals(renewer_principal, "--renewer-principal")?;
            let request = krafka::protocol::CreateDelegationTokenRequest {
                renewers: renewers
                    .iter()
                    .map(
                        |(principal_type, principal_name)| krafka::protocol::CreatableRenewer {
                            principal_type: principal_type.clone(),
                            principal_name: principal_name.clone(),
                        },
                    )
                    .collect(),
                max_lifetime_ms: *max_life_time_period,
                owner_principal_type: owners.first().map(|owner| owner.0.clone()),
                owner_principal_name: owners.first().map(|owner| owner.1.clone()),
            };
            let connection = client.get_controller_connection().await?;
            let version = connection
                .negotiate_api_version(
                    ApiKey::CreateDelegationToken,
                    versions::CREATE_DELEGATION_TOKEN_MAX,
                    versions::CREATE_DELEGATION_TOKEN_MIN,
                )
                .await
                .ok_or_else(|| {
                    Error::Unsupported("broker does not support CreateDelegationToken".into())
                })?;
            if !owners.is_empty() && version < 3 {
                return Err(Error::Unsupported(format!(
                    "--owner-principal requires CreateDelegationToken v3; controller negotiated v{version}"
                )));
            }
            let mut bytes = connection
                .send_request(ApiKey::CreateDelegationToken, version, |buffer| {
                    request.encode_versioned(version, buffer)
                })
                .await?;
            drop(connection);
            let response = krafka::protocol::CreateDelegationTokenResponse::decode_versioned(
                version, &mut bytes,
            )?;
            if !response.error_code.is_ok() {
                return Err(Error::Config(format!(
                    "CreateDelegationToken failed: {:?}",
                    response.error_code
                )));
            }
            let row = delegation_token_row(krafka::protocol::DelegationTokenInfo {
                principal_type: response.principal_type,
                principal_name: response.principal_name,
                token_requester_principal_type: response.token_requester_principal_type,
                token_requester_principal_name: response.token_requester_principal_name,
                issue_timestamp_ms: response.issue_timestamp_ms,
                expiry_timestamp_ms: response.expiry_timestamp_ms,
                max_timestamp_ms: response.max_timestamp_ms,
                token_id: response.token_id,
                hmac: response.hmac,
                renewers: renewers
                    .into_iter()
                    .map(|(principal_type, principal_name)| {
                        krafka::protocol::DelegationTokenRenewer {
                            principal_type,
                            principal_name,
                        }
                    })
                    .collect(),
            });
            write_delegation_tokens(format, "delegation-tokens.create", &[row])
        }
        DelegationTokenAction::Describe { owner_principal } => {
            let owners = parse_kafka_principals(owner_principal, "--owner-principal")?;
            let request = krafka::protocol::DescribeDelegationTokenRequest {
                owners: (!owners.is_empty()).then(|| {
                    owners
                        .into_iter()
                        .map(|(principal_type, principal_name)| {
                            krafka::protocol::DescribeDelegationTokenOwner {
                                principal_type,
                                principal_name,
                            }
                        })
                        .collect()
                }),
            };
            let connection = delegation_broker_connection(&client).await?;
            let version = connection
                .negotiate_api_version(
                    ApiKey::DescribeDelegationToken,
                    versions::DESCRIBE_DELEGATION_TOKEN_MAX,
                    versions::DESCRIBE_DELEGATION_TOKEN_MIN,
                )
                .await
                .ok_or_else(|| {
                    Error::Unsupported("broker does not support DescribeDelegationToken".into())
                })?;
            let mut bytes = connection
                .send_request(ApiKey::DescribeDelegationToken, version, |buffer| {
                    request.encode_versioned(version, buffer)
                })
                .await?;
            drop(connection);
            let response = krafka::protocol::DescribeDelegationTokenResponse::decode_versioned(
                version, &mut bytes,
            )?;
            if !response.error_code.is_ok() {
                return Err(Error::Config(format!(
                    "DescribeDelegationToken failed: {:?}",
                    response.error_code
                )));
            }
            let rows = response
                .tokens
                .into_iter()
                .map(delegation_token_row)
                .collect::<Vec<_>>();
            write_delegation_tokens(format, "delegation-tokens.describe", &rows)
        }
        DelegationTokenAction::Renew {
            hmac,
            renew_time_period,
        } => {
            if *renew_time_period < -1 {
                return Err(Error::Usage(
                    "--renew-time-period must be -1 or non-negative".into(),
                ));
            }
            let hmac = STANDARD
                .decode(hmac)
                .map_err(|error| Error::Usage(format!("invalid --hmac Base64: {error}")))?;
            let connection = delegation_broker_connection(&client).await?;
            let request = krafka::protocol::RenewDelegationTokenRequest {
                hmac: hmac.into(),
                renew_period_ms: *renew_time_period,
            };
            let version = connection
                .negotiate_api_version(
                    ApiKey::RenewDelegationToken,
                    versions::RENEW_DELEGATION_TOKEN_MAX,
                    versions::RENEW_DELEGATION_TOKEN_MIN,
                )
                .await
                .ok_or_else(|| {
                    Error::Unsupported("broker does not support RenewDelegationToken".into())
                })?;
            let mut bytes = connection
                .send_request(ApiKey::RenewDelegationToken, version, |buffer| {
                    request.encode_versioned(version, buffer)
                })
                .await?;
            drop(connection);
            let response = krafka::protocol::RenewDelegationTokenResponse::decode_versioned(
                version, &mut bytes,
            )?;
            if !response.error_code.is_ok() {
                return Err(Error::Config(format!(
                    "RenewDelegationToken failed: {:?}",
                    response.error_code
                )));
            }
            let row = DelegationTokenExpiryRow {
                action: "renew".into(),
                expiry_timestamp_ms: response.expiry_timestamp_ms,
                expiry_date: delegation_timestamp(response.expiry_timestamp_ms),
            };
            output::write_value(format, "delegation-tokens.renew", &row, |row| {
                output::table(
                    ["ACTION", "EXPIRY_TIMESTAMP_MS", "EXPIRY_DATE"],
                    [[
                        row.action.clone(),
                        row.expiry_timestamp_ms.to_string(),
                        row.expiry_date.clone(),
                    ]],
                )
            })
        }
        DelegationTokenAction::Expire {
            hmac,
            expiry_time_period,
        } => {
            if *expiry_time_period < -1 {
                return Err(Error::Usage(
                    "--expiry-time-period must be -1 or non-negative".into(),
                ));
            }
            let hmac = STANDARD
                .decode(hmac)
                .map_err(|error| Error::Usage(format!("invalid --hmac Base64: {error}")))?;
            let result = client
                .expire_delegation_token(
                    &hmac,
                    (*expiry_time_period >= 0).then(|| {
                        Duration::from_millis(
                            u64::try_from(*expiry_time_period).unwrap_or(u64::MAX),
                        )
                    }),
                )
                .await?;
            if let Some(error) = result.error {
                return Err(Error::Config(format!(
                    "ExpireDelegationToken failed: {error}"
                )));
            }
            let row = DelegationTokenExpiryRow {
                action: "expire".into(),
                expiry_timestamp_ms: result.expiry_timestamp_ms,
                expiry_date: delegation_timestamp(result.expiry_timestamp_ms),
            };
            output::write_value(format, "delegation-tokens.expire", &row, |row| {
                output::table(
                    ["ACTION", "EXPIRY_TIMESTAMP_MS", "EXPIRY_DATE"],
                    [[
                        row.action.clone(),
                        row.expiry_timestamp_ms.to_string(),
                        row.expiry_date.clone(),
                    ]],
                )
            })
        }
    }
}

#[derive(Debug, Serialize)]
struct ClientMetricsResourceRow {
    name: String,
}

#[expect(
    clippy::too_many_lines,
    reason = "branches mirror Kafka ClientMetricsCommand's four action contracts"
)]
async fn client_metrics(
    bootstrap: &str,
    command_config: Option<&Path>,
    timeout: Duration,
    format: OutputFormat,
    action: ClientMetricsAction,
) -> Result<()> {
    match action {
        ClientMetricsAction::List => {
            let client = config::protocol_admin(bootstrap, timeout, command_config).await?;
            let mut names = client.list_client_metrics_resources().await?;
            names.sort();
            drop(client);
            let rows = names
                .into_iter()
                .map(|name| ClientMetricsResourceRow { name })
                .collect::<Vec<_>>();
            output::write_value(format, "client-metrics.list", &rows, |rows| {
                output::table(["NAME"], rows.iter().map(|row| [row.name.clone()]))
            })
        }
        ClientMetricsAction::Describe { name } => {
            describe_protocol_configs(
                bootstrap,
                command_config,
                timeout,
                format,
                "client-metrics.describe",
                ConfigEntityType::ClientMetrics,
                name,
                false,
            )
            .await
        }
        ClientMetricsAction::Alter {
            name,
            generate_name,
            interval,
            r#match,
            metrics,
            execute,
        } => {
            if let Some(value) = interval.as_deref().filter(|value| !value.is_empty()) {
                value.parse::<i32>().map_err(|_| {
                    Error::Usage(
                        "invalid interval value; enter an integer, or leave empty to reset".into(),
                    )
                })?;
            }
            let name = if generate_name {
                kafka_random_uuid()
            } else {
                name.ok_or_else(|| {
                    Error::Usage("one of --name or --generate-name must be specified".into())
                })?
            };
            let changes = [
                ("interval.ms", interval),
                ("match", (!r#match.is_empty()).then(|| r#match.join(","))),
                ("metrics", (!metrics.is_empty()).then(|| metrics.join(","))),
            ];
            let additions = changes
                .iter()
                .filter_map(|(key, value)| {
                    value
                        .as_ref()
                        .filter(|value| !value.is_empty())
                        .map(|value| ((*key).to_owned(), value.clone()))
                })
                .collect::<Vec<_>>();
            let deletions = changes
                .iter()
                .filter(|(_, value)| value.as_ref().is_some_and(String::is_empty))
                .map(|(key, _)| (*key).to_owned())
                .collect::<Vec<_>>();
            if !execute {
                return config_change_preview(
                    format,
                    "client-metrics.alter.preview",
                    &additions,
                    &deletions,
                );
            }
            alter_protocol_config(
                bootstrap,
                command_config,
                timeout,
                format,
                "client-metrics.alter",
                ConfigEntityType::ClientMetrics,
                &name,
                &additions,
                &deletions,
            )
            .await
        }
        ClientMetricsAction::Delete { name, execute } => {
            let keys =
                client_metrics_config_keys(bootstrap, command_config, timeout, &name).await?;
            if !execute {
                return config_change_preview(format, "client-metrics.delete.preview", &[], &keys);
            }
            alter_protocol_config(
                bootstrap,
                command_config,
                timeout,
                format,
                "client-metrics.delete",
                ConfigEntityType::ClientMetrics,
                &name,
                &[],
                &keys,
            )
            .await
        }
    }
}

fn kafka_random_uuid() -> String {
    loop {
        let encoded = URL_SAFE_NO_PAD.encode(uuid::Uuid::new_v4().as_bytes());
        if !encoded.contains('-') {
            return encoded;
        }
    }
}

async fn client_metrics_config_keys(
    bootstrap: &str,
    command_config: Option<&Path>,
    timeout: Duration,
    name: &str,
) -> Result<Vec<String>> {
    let client = config::protocol_admin(bootstrap, timeout, command_config).await?;
    let (broker_id, address) =
        protocol_config_broker(&client, ConfigEntityType::ClientMetrics, name).await?;
    let connection = client
        .pool()
        .get_connection_by_id(broker_id, &address)
        .await?;
    let request = DescribeConfigsRequest {
        resources: vec![DescribeConfigsResource {
            resource_type: ProtocolConfigResourceType::ClientMetrics,
            resource_name: name.to_owned(),
            config_names: None,
        }],
        include_synonyms: false,
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
    let resource =
        response.results.into_iter().next().ok_or_else(|| {
            Error::Config(format!("broker did not describe client metrics {name}"))
        })?;
    if !resource.error_code.is_ok() {
        return Err(Error::Config(
            resource
                .error_message
                .unwrap_or_else(|| format!("{:?}", resource.error_code)),
        ));
    }
    Ok(resource
        .configs
        .into_iter()
        .map(|entry| entry.name)
        .collect())
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
        ConfigAction::Describe { entity, all } => {
            let ResolvedConfigEntities {
                types: entity_type,
                names: entity_name,
                entity_default,
                embedded_defaults,
            } = resolve_config_entities(entity)?;
            validate_config_entity_types(&entity_type)?;
            validate_config_entity_names(&entity_type, &entity_name, false, embedded_defaults)?;
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
                    "configs.describe",
                    entity_type,
                    entity_name,
                    all,
                )
                .await;
            }
            describe_resource_configs(config, timeout, format, entity_type, entity_name, all).await
        }
        ConfigAction::Alter {
            entity,
            add,
            add_file,
            delete,
            execute,
        } => {
            let ResolvedConfigEntities {
                types: entity_type,
                names: entity_name,
                entity_default,
                embedded_defaults,
            } = resolve_config_entities(entity)?;
            let delete = normalize_config_deletions(delete);
            let pairs = if let Some(path) = add_file.as_deref() {
                let mut pairs = config::load_properties(path)?
                    .into_iter()
                    .collect::<Vec<_>>();
                pairs.sort_by(|left, right| left.0.cmp(&right.0));
                pairs
            } else {
                parse_config_additions(&add)?
            };
            if pairs.is_empty() && delete.is_empty() {
                return Err(Error::Usage(
                    "provide --add-config, --add-config-file, or --delete-config".into(),
                ));
            }
            validate_config_entity_types(&entity_type)?;
            validate_config_entity_names(&entity_type, &entity_name, true, embedded_defaults)?;
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
                return config_change_preview(format, "configs.alter.preview", &pairs, &delete);
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
                    "configs.alter",
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

struct ResolvedConfigEntities {
    types: Vec<ConfigEntityType>,
    names: Vec<String>,
    entity_default: bool,
    embedded_defaults: bool,
}

fn resolve_config_entities(entity: ConfigEntityArgs) -> Result<ResolvedConfigEntities> {
    let ConfigEntityArgs {
        entity_type,
        entity_name,
        entity_default,
        topic,
        client,
        user,
        broker,
        broker_logger,
        ip,
        client_metrics,
        group,
        client_defaults,
        user_defaults,
        broker_defaults,
        ip_defaults,
    } = entity;
    let generic = !entity_type.is_empty() || !entity_name.is_empty() || entity_default;
    let specific = topic.is_some()
        || client.is_some()
        || user.is_some()
        || broker.is_some()
        || broker_logger.is_some()
        || ip.is_some()
        || client_metrics.is_some()
        || group.is_some()
        || client_defaults
        || user_defaults
        || broker_defaults
        || ip_defaults;
    if generic && specific {
        return Err(Error::Usage(
            "--entity-{type,name,default} cannot be combined with specific entity flags".into(),
        ));
    }
    if generic {
        if entity_type.is_empty() {
            return Err(Error::Usage(
                "at least one --entity-type must be specified".into(),
            ));
        }
        return Ok(ResolvedConfigEntities {
            types: entity_type,
            names: entity_name,
            entity_default,
            embedded_defaults: false,
        });
    }

    let mut selections = Vec::new();
    for (kind, name) in [
        (ConfigEntityType::Topic, topic),
        (ConfigEntityType::Client, client),
        (ConfigEntityType::User, user),
        (ConfigEntityType::Broker, broker),
        (ConfigEntityType::BrokerLogger, broker_logger),
        (ConfigEntityType::Ip, ip),
        (ConfigEntityType::ClientMetrics, client_metrics),
        (ConfigEntityType::Group, group),
    ] {
        if let Some(name) = name {
            selections.push((kind, name));
        }
    }
    for (kind, selected) in [
        (ConfigEntityType::Client, client_defaults),
        (ConfigEntityType::User, user_defaults),
        (ConfigEntityType::Broker, broker_defaults),
        (ConfigEntityType::Ip, ip_defaults),
    ] {
        if selected {
            selections.push((kind, String::new()));
        }
    }
    if selections.is_empty() {
        return Err(Error::Usage("at least one entity must be specified".into()));
    }
    let only_default = selections.len() == 1 && selections[0].1.is_empty();
    Ok(ResolvedConfigEntities {
        types: selections.iter().map(|(kind, _)| *kind).collect(),
        names: if only_default {
            Vec::new()
        } else {
            selections.into_iter().map(|(_, name)| name).collect()
        },
        entity_default: only_default,
        embedded_defaults: !only_default,
    })
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

fn validate_config_entity_names(
    types: &[ConfigEntityType],
    names: &[String],
    altering: bool,
    embedded_defaults: bool,
) -> Result<()> {
    if !altering && types == [ConfigEntityType::BrokerLogger] && names.is_empty() {
        return Err(Error::Usage(
            "broker-logger describe requires --entity-name".into(),
        ));
    }
    if altering && !embedded_defaults && names.iter().any(String::is_empty) {
        return Err(Error::Usage(
            "--entity-name cannot be empty with --alter; use --entity-default".into(),
        ));
    }
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
    if types.len() == 1 && matches!(types[0], ConfigEntityType::Ip) {
        for name in names.iter().filter(|name| !name.is_empty()) {
            let mut addresses = (name.as_str(), 0).to_socket_addrs().map_err(|_| {
                Error::Usage(format!(
                    "the entity name for ips must be a valid IP or resolvable host: {name}"
                ))
            })?;
            if addresses.next().is_none() {
                return Err(Error::Usage(format!(
                    "the entity name for ips must be a valid IP or resolvable host: {name}"
                )));
            }
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
                names
                    .get(index)
                    .and_then(|name| (!name.is_empty()).then_some(name.as_str())),
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
                if entity_default || (names.len() == types.len() && name.is_none()) {
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
    if types == [ConfigEntityType::User]
        && !entity_default
        && names.first().is_none_or(|name| !name.is_empty())
    {
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
        return config_change_preview(format, "configs.alter.preview", add, delete);
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
    clippy::too_many_arguments,
    reason = "request routing, output identity, protocol decoding, and shared command context form one config operation"
)]
async fn describe_protocol_configs(
    bootstrap: &str,
    command_config: Option<&Path>,
    timeout: Duration,
    format: OutputFormat,
    output_command: &str,
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
    output::write_value(format, output_command, &rows, |rows| {
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
    output_command: &str,
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
    write_mutation_rows(format, output_command, &rows)?;
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
    output_command: &str,
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
    output::write_value(format, output_command, &rows, |rows| {
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
            .unwrap_or(value);
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

async fn offsets(
    config: &rdkafka::ClientConfig,
    bootstrap: &str,
    command_config: Option<&Path>,
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
    let rows = if let Some(timestamp) = tiered_offset_timestamp(spec) {
        protocol_list_offsets(bootstrap, command_config, timeout, &targets, timestamp).await?
    } else {
        let client = admin(&config)?;
        ffi::list_offsets(
            client.inner().native_ptr(),
            &targets,
            spec,
            duration_ms(timeout)?,
        )?
    };
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

const fn tiered_offset_timestamp(spec: ffi::ListOffsetSpec) -> Option<i64> {
    match spec {
        ffi::ListOffsetSpec::EarliestLocal => Some(-4),
        ffi::ListOffsetSpec::LatestTiered => Some(-5),
        ffi::ListOffsetSpec::EarliestPendingUpload => Some(-6),
        _ => None,
    }
}

async fn protocol_list_offsets(
    bootstrap: &str,
    command_config: Option<&Path>,
    timeout: Duration,
    targets: &[(String, i32)],
    timestamp: i64,
) -> Result<Vec<ffi::ListOffsetEntry>> {
    let mut grouped = BTreeMap::<&str, Vec<i32>>::new();
    for (topic, partition) in targets {
        grouped.entry(topic).or_default().push(*partition);
    }
    let topic_partitions = grouped
        .iter()
        .map(|(topic, partitions)| (*topic, partitions.as_slice()))
        .collect::<Vec<_>>();
    let client = config::protocol_admin(bootstrap, timeout, command_config).await?;
    let results = client
        .list_offsets(
            &topic_partitions,
            krafka::admin::OffsetSpec::Timestamp(timestamp),
        )
        .await?;
    drop(client);
    let mut rows = results
        .into_iter()
        .filter_map(|result| {
            if result.offset == -1 && result.error.is_none() {
                return None;
            }
            Some(ffi::ListOffsetEntry {
                topic: result.topic,
                partition: result.partition,
                offset: (result.offset >= 0).then_some(result.offset),
                timestamp: (result.timestamp >= 0).then_some(result.timestamp),
                error: result.error,
            })
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left.topic
            .cmp(&right.topic)
            .then(left.partition.cmp(&right.partition))
    });
    Ok(rows)
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
            let (bindings, already_exists) =
                missing_acl_bindings(client.inner().native_ptr(), bindings, timeout_ms)?;
            let result = if bindings.is_empty() {
                ffi::AclMutationResult {
                    matched: 0,
                    failures: 0,
                    errors: Vec::new(),
                }
            } else {
                ffi::create_acls(client.inner().native_ptr(), &bindings, timeout_ms)?
            };
            write_acl_mutation_result(
                format,
                "acls.add",
                &format!(
                    "CREATED {}; ALREADY_EXISTS {already_exists}",
                    result.matched.saturating_sub(result.failures)
                ),
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

fn missing_acl_bindings(
    client: *mut rdkafka_sys::rd_kafka_t,
    requested: Vec<AclBinding>,
    timeout_ms: i32,
) -> Result<(Vec<AclBinding>, usize)> {
    let mut queried_resources = HashSet::new();
    let mut existing = HashSet::new();
    for binding in &requested {
        let resource = (
            binding.resource_type,
            binding.resource_name.clone(),
            binding.pattern_type,
        );
        if queried_resources.insert(resource) {
            existing.extend(ffi::describe_acls(
                client,
                &AclBindingFilter {
                    resource_type: binding.resource_type,
                    resource_name: Some(binding.resource_name.clone()),
                    pattern_type: binding.pattern_type,
                    principal: None,
                    host: None,
                    operation: AclOperation::Any,
                    permission_type: AclPermissionType::Any,
                },
                timeout_ms,
            )?);
        }
    }
    Ok(filter_missing_acl_bindings(requested, &existing))
}

fn filter_missing_acl_bindings(
    requested: Vec<AclBinding>,
    existing: &HashSet<AclBinding>,
) -> (Vec<AclBinding>, usize) {
    let requested_count = requested.len();
    let missing = requested
        .into_iter()
        .filter(|binding| !existing.contains(binding))
        .collect::<Vec<_>>();
    let already_exists = requested_count.saturating_sub(missing.len());
    (missing, already_exists)
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
    let mut operations = Vec::new();
    for value in values {
        let operation = match value.trim().to_ascii_lowercase().as_str() {
            "all" => AclOperation::All,
            "read" => AclOperation::Read,
            "write" => AclOperation::Write,
            "create" => AclOperation::Create,
            "delete" => AclOperation::Delete,
            "alter" => AclOperation::Alter,
            "describe" => AclOperation::Describe,
            "cluster-action" => AclOperation::ClusterAction,
            "describe-configs" => AclOperation::DescribeConfigs,
            "alter-configs" => AclOperation::AlterConfigs,
            "idempotent-write" => AclOperation::IdempotentWrite,
            "two-phase-commit" | "create-tokens" | "describe-tokens" => {
                return Err(Error::Unsupported(format!(
                    "librdkafka 2.12 does not support ACL operation: {value}"
                )));
            }
            _ => return Err(Error::Usage(format!("unknown ACL operation: {value}"))),
        };
        if !operations.contains(&operation) {
            operations.push(operation);
        }
    }
    Ok(operations)
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
    validate_acl_resource_operations(&resources)?;
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
    let allow_hosts = normalized_acl_values(&mutation.allow_host, "allow host")?;
    let deny_hosts = normalized_acl_values(&mutation.deny_host, "deny host")?;
    let mut bindings = Vec::new();
    for (resource_type, resource_name, operations) in resources {
        for (principal, permission) in &principals {
            let configured_hosts = if *permission == AclPermissionType::Allow {
                &allow_hosts
            } else {
                &deny_hosts
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
    if !entries.is_empty() {
        validate_acl_resource_operations(&resources)?;
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

fn validate_acl_resource_operations(
    resources: &[(AclResourceType, String, Vec<AclOperation>)],
) -> Result<()> {
    for (resource_type, _, operations) in resources {
        if let Some(operation) = operations
            .iter()
            .find(|operation| !acl_operation_supported(*resource_type, **operation))
        {
            return Err(Error::Usage(format!(
                "ACL resource type {resource_type:?} does not support operation {operation:?}"
            )));
        }
    }
    Ok(())
}

const fn acl_operation_supported(resource_type: AclResourceType, operation: AclOperation) -> bool {
    if matches!(operation, AclOperation::Any | AclOperation::All) {
        return true;
    }
    match resource_type {
        AclResourceType::Any => true,
        AclResourceType::Topic => matches!(
            operation,
            AclOperation::Read
                | AclOperation::Write
                | AclOperation::Create
                | AclOperation::Delete
                | AclOperation::Alter
                | AclOperation::Describe
                | AclOperation::DescribeConfigs
                | AclOperation::AlterConfigs
        ),
        AclResourceType::Group => matches!(
            operation,
            AclOperation::Read
                | AclOperation::Delete
                | AclOperation::Describe
                | AclOperation::DescribeConfigs
                | AclOperation::AlterConfigs
        ),
        AclResourceType::Cluster => matches!(
            operation,
            AclOperation::Create
                | AclOperation::Alter
                | AclOperation::Describe
                | AclOperation::ClusterAction
                | AclOperation::DescribeConfigs
                | AclOperation::AlterConfigs
                | AclOperation::IdempotentWrite
        ),
        AclResourceType::TransactionalId => {
            matches!(operation, AclOperation::Write | AclOperation::Describe)
        }
    }
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
    if mutation.consumer
        && !mutation.producer
        && (mutation.filter.cluster || !mutation.filter.transactional_id.is_empty())
    {
        return Err(Error::Usage(
            "--consumer cannot be combined with --cluster or --transactional-id unless --producer is also set"
                .into(),
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

fn parse_config_additions(values: &[String]) -> Result<Vec<(String, String)>> {
    let mut configs = BTreeMap::new();
    for value in values {
        for entry in split_outside_brackets(value, ',')? {
            let Some((key, value)) = split_once_outside_brackets(entry, '=')? else {
                return Err(Error::Usage(
                    "all configs must use key=value or key=[value1,value2]".into(),
                ));
            };
            let key = key.trim();
            let value = value.trim();
            let value = value
                .strip_prefix('[')
                .and_then(|value| value.strip_suffix(']'))
                .unwrap_or(value);
            configs.insert(key.to_owned(), value.to_owned());
        }
    }
    Ok(configs.into_iter().collect())
}

fn split_outside_brackets(value: &str, delimiter: char) -> Result<Vec<&str>> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut bracketed = false;
    for (index, character) in value.char_indices() {
        match character {
            '[' if bracketed => {
                return Err(Error::Usage(
                    "nested config brackets are not supported".into(),
                ));
            }
            '[' => bracketed = true,
            ']' if !bracketed => {
                return Err(Error::Usage("unmatched closing config bracket".into()));
            }
            ']' => bracketed = false,
            _ if character == delimiter && !bracketed => {
                parts.push(&value[start..index]);
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    if bracketed {
        return Err(Error::Usage("unclosed config bracket".into()));
    }
    parts.push(&value[start..]);
    while parts.len() > 1 && parts.last().is_some_and(|part| part.is_empty()) {
        parts.pop();
    }
    Ok(parts)
}

fn split_once_outside_brackets(value: &str, delimiter: char) -> Result<Option<(&str, &str)>> {
    let mut bracketed = false;
    for (index, character) in value.char_indices() {
        match character {
            '[' if bracketed => {
                return Err(Error::Usage(
                    "nested config brackets are not supported".into(),
                ));
            }
            '[' => bracketed = true,
            ']' if !bracketed => {
                return Err(Error::Usage("unmatched closing config bracket".into()));
            }
            ']' => bracketed = false,
            _ if character == delimiter && !bracketed => {
                let next = index + character.len_utf8();
                return Ok(Some((&value[..index], &value[next..])));
            }
            _ => {}
        }
    }
    if bracketed {
        return Err(Error::Usage("unclosed config bracket".into()));
    }
    Ok(None)
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
    fn generated_client_metrics_name_should_match_kafka_uuid_format() {
        let name = kafka_random_uuid();

        assert_eq!(name.len(), 22);
        assert!(!name.contains('-'));
        assert_eq!(
            URL_SAFE_NO_PAD
                .decode(name)
                .expect("Kafka UUID encoding")
                .len(),
            16
        );
    }

    #[test]
    fn metadata_version_should_match_kafka_release_aliases() {
        assert_eq!(metadata_version("3.7").expect("3.7 alias").level, 19);
        assert_eq!(metadata_version("3.7.2").expect("3.7 patch").level, 19);
        assert_eq!(
            metadata_version("3.7-IV2.foo")
                .expect("Kafka truncates after the second dot-separated component")
                .level,
            17
        );
        assert_eq!(
            metadata_version("4.4-IV1").expect("unstable exact").level,
            32
        );
        assert!(metadata_version("4.4").is_err());
    }

    #[test]
    fn feature_defaults_should_match_kafka_4_4_mapping() {
        assert_eq!(feature_default_level("transaction.version", 24), 2);
        assert_eq!(feature_default_level("share.version", 30), 1);
        assert_eq!(feature_default_level("share.version", 31), 2);
        assert_eq!(feature_default_level("streams.version", 29), 1);
    }

    #[test]
    fn feature_levels_should_trim_and_reject_duplicates() {
        let parsed = parse_feature_levels(&[" metadata.version = 30 ".into()])
            .expect("trimmed feature level");
        assert_eq!(parsed.get("metadata.version"), Some(&30));
        assert!(
            parse_feature_levels(&["group.version=0".into(), "group.version=1".into()]).is_err()
        );
    }

    #[test]
    fn feature_release_upgrade_should_include_non_zero_release_defaults() {
        let (_, updates, dry_run) = feature_updates(&FeatureAction::Upgrade {
            metadata: None,
            release_version: Some("3.9-IV0".into()),
            feature: Vec::new(),
            dry_run: true,
        })
        .expect("release upgrade");

        assert!(dry_run);
        assert!(updates.iter().any(|update| {
            update.feature == "metadata.version" && update.max_version_level == 21
        }));
        assert!(
            updates.iter().any(|update| {
                update.feature == "kraft.version" && update.max_version_level == 1
            })
        );
        assert!(
            !updates
                .iter()
                .any(|update| update.feature == "transaction.version")
        );
        assert!(updates.iter().all(|update| {
            update.upgrade_type == krafka::protocol::FeatureUpgradeType::Upgrade
        }));
    }

    #[test]
    fn feature_downgrade_and_disable_should_select_kafka_upgrade_types() {
        let (_, safe, _) = feature_updates(&FeatureAction::Downgrade {
            metadata: Some("3.7-IV2".into()),
            release_version: None,
            feature: Vec::new(),
            r#unsafe: false,
            dry_run: false,
        })
        .expect("safe downgrade");
        assert_eq!(
            safe[0].upgrade_type,
            krafka::protocol::FeatureUpgradeType::SafeDowngrade
        );

        let (_, unsafe_updates, _) = feature_updates(&FeatureAction::Disable {
            feature: vec!["group.version".into()],
            r#unsafe: true,
            dry_run: false,
        })
        .expect("unsafe disable");
        assert_eq!(unsafe_updates[0].max_version_level, 0);
        assert_eq!(
            unsafe_updates[0].upgrade_type,
            krafka::protocol::FeatureUpgradeType::UnsafeDowngrade
        );
        assert!(
            feature_updates(&FeatureAction::Disable {
                feature: vec!["group.version".into(), "group.version".into()],
                r#unsafe: false,
                dry_run: false,
            })
            .is_err()
        );
    }

    #[test]
    fn quorum_v2_decoder_should_preserve_timestamps_directory_and_endpoints() {
        let mut buffer = BytesMut::new();
        buffer.put_i16(0);
        buffer.put_u8(0);
        buffer.put_u8(2);
        encode_compact_string("__cluster_metadata", &mut buffer);
        buffer.put_u8(2);
        buffer.put_i32(0);
        buffer.put_i16(0);
        buffer.put_u8(0);
        buffer.put_i32(1);
        buffer.put_i32(9);
        buffer.put_i64(42);
        buffer.put_u8(2);
        buffer.put_i32(1);
        buffer.put_slice(&[7; 16]);
        buffer.put_i64(44);
        buffer.put_i64(1_000);
        buffer.put_i64(900);
        buffer.put_u8(0);
        buffer.put_u8(1);
        buffer.put_u8(0);
        buffer.put_u8(0);
        buffer.put_u8(2);
        buffer.put_i32(1);
        buffer.put_u8(2);
        encode_compact_string("CONTROLLER", &mut buffer);
        encode_compact_string("controller.example", &mut buffer);
        buffer.put_u16(9093);
        buffer.put_u8(0);
        buffer.put_u8(0);
        buffer.put_u8(0);

        let description = decode_quorum_response(buffer.freeze(), 2).expect("quorum v2");

        assert_eq!(description.leader_id, 1);
        assert_eq!(description.voters[0].log_end_offset, 44);
        assert_eq!(description.voters[0].last_fetch_timestamp, Some(1_000));
        assert_eq!(description.voters[0].directory_id, "BwcHBwcHBwcHBwcHBwcHBw");
        assert_eq!(
            description.nodes[0].endpoints,
            ["CONTROLLER://controller.example:9093"]
        );
    }

    #[test]
    fn controller_properties_should_match_kafka_server_config_precedence() {
        let directory = tempfile::TempDir::new().expect("controller directory");
        std::fs::write(
            directory.path().join("meta.properties"),
            "directory.id=AAAAAAAAAAAAAAAAAAAAAA\n",
        )
        .expect("meta.properties");
        let config = tempfile::NamedTempFile::new().expect("controller config");
        std::fs::write(
            config.path(),
            format!(
                "node.id=2\nprocess.roles=broker,controller\nmetadata.log.dir={}\ncontroller.listener.names=CONTROLLER\nlisteners=CONTROLLER://:9093\nadvertised.listeners=CONTROLLER://controller.example:19093\n",
                directory.path().display()
            ),
        )
        .expect("controller config");

        let (node_id, directory_id, endpoints) =
            controller_from_properties(config.path()).expect("controller properties");

        assert_eq!(node_id, 2);
        assert_eq!(directory_id, [0; 16]);
        assert_eq!(endpoints[0].host, "controller.example");
        assert_eq!(endpoints[0].port, 19093);
    }

    #[test]
    fn quorum_relative_timestamp_should_reject_clock_drift() {
        assert_eq!(
            relative_timestamp(Some(900), true, 1_000).expect("relative timestamp"),
            "100 ms ago"
        );
        assert!(relative_timestamp(Some(1_001), true, 1_000).is_err());
        assert_eq!(
            relative_timestamp(None, false, 1_000).expect("missing timestamp"),
            "-1"
        );
    }

    #[test]
    fn delegation_principals_should_trim_and_validate_type_and_name() {
        assert_eq!(
            parse_kafka_principals(&[" User:alice ".into()], "--owner-principal")
                .expect("principal"),
            [("User".into(), "alice".into())]
        );
        assert!(parse_kafka_principals(&["alice".into()], "--owner-principal").is_err());
        assert!(parse_kafka_principals(&["User:".into()], "--owner-principal").is_err());
    }

    #[test]
    fn delegation_token_row_should_preserve_requester_renewers_and_standard_hmac() {
        let row = delegation_token_row(krafka::protocol::DelegationTokenInfo {
            principal_type: "User".into(),
            principal_name: "owner".into(),
            token_requester_principal_type: Some("User".into()),
            token_requester_principal_name: Some("requester".into()),
            issue_timestamp_ms: 0,
            expiry_timestamp_ms: 60_000,
            max_timestamp_ms: 120_000,
            token_id: "token-1".into(),
            hmac: bytes::Bytes::from_static(&[0xfb, 0xff]),
            renewers: vec![krafka::protocol::DelegationTokenRenewer {
                principal_type: "User".into(),
                principal_name: "renewer".into(),
            }],
        });

        assert_eq!(row.owner, "User:owner");
        assert_eq!(row.requester, "User:requester");
        assert_eq!(row.renewers, "User:renewer");
        assert_eq!(row.hmac, "+/8=");
    }

    #[test]
    fn parse_pairs_should_retain_equals_in_value() {
        let pairs = parse_pairs(&["password=a=b".into()]).expect("valid pair");
        assert_eq!(pairs, [("password".into(), "a=b".into())]);
    }

    #[test]
    fn config_additions_should_parse_grouped_comma_values() {
        let pairs =
            parse_config_additions(&["cleanup.policy=[compact,delete],retention.ms=1000".into()])
                .expect("grouped config list");

        assert_eq!(
            pairs,
            [
                ("cleanup.policy".into(), "compact,delete".into()),
                ("retention.ms".into(), "1000".into()),
            ]
        );
    }

    #[test]
    fn config_additions_should_retain_equals_inside_grouped_value() {
        let pairs = parse_config_additions(&["listener=[first=a,second=b]".into()])
            .expect("equals in grouped config");

        assert_eq!(pairs, [("listener".into(), "first=a,second=b".into())]);
    }

    #[test]
    fn config_additions_should_reject_unclosed_bracket() {
        assert!(matches!(
            parse_config_additions(&["cleanup.policy=[compact,delete".into()]),
            Err(Error::Usage(message)) if message.contains("unclosed")
        ));
    }

    #[test]
    fn config_additions_should_use_last_duplicate_value() {
        let pairs = parse_config_additions(&["retention.ms=1,retention.ms=2".into()])
            .expect("duplicate config keys");

        assert_eq!(pairs, [("retention.ms".into(), "2".into())]);
    }

    #[test]
    fn config_additions_should_feed_normalized_scram_value() {
        let additions =
            parse_config_additions(&["SCRAM-SHA-512=[iterations=4096,password=secret]".into()])
                .expect("SCRAM config addition");

        assert!(parse_scram_changes(&additions, &[]).is_ok());
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
    fn config_entities_should_resolve_mixed_default_user_and_named_client() {
        let resolved = resolve_config_entities(ConfigEntityArgs {
            client: Some("billing".into()),
            user_defaults: true,
            ..ConfigEntityArgs::default()
        })
        .expect("mixed quota selection");

        assert_eq!(
            (resolved.types, resolved.names, resolved.embedded_defaults),
            (
                vec![ConfigEntityType::Client, ConfigEntityType::User],
                vec!["billing".to_owned(), String::new()],
                true,
            )
        );
    }

    #[test]
    fn quota_entities_should_pair_named_client_and_default_user() {
        let names = ["billing".to_owned(), String::new()];
        let entities = quota_entities(
            &[ConfigEntityType::Client, ConfigEntityType::User],
            &names,
            false,
            true,
        )
        .expect("mixed quota entity");

        assert_eq!(entities, [("client-id", Some("billing")), ("user", None)]);
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
                &["not-a-broker".into()],
                false,
                false,
            ),
            Err(Error::Usage(message)) if message.contains("integer broker ID")
        ));
    }

    #[test]
    fn config_ip_entity_name_should_accept_ip_literal() {
        let result = validate_config_entity_names(
            &[ConfigEntityType::Ip],
            &["127.0.0.1".into()],
            false,
            false,
        );

        assert!(result.is_ok(), "IP validation failed: {result:?}");
    }

    #[test]
    fn config_ip_entity_name_should_reject_invalid_host() {
        assert!(matches!(
            validate_config_entity_names(
                &[ConfigEntityType::Ip],
                &["invalid host name with spaces".into()],
                false,
                false,
            ),
            Err(Error::Usage(message)) if message.contains("valid IP or resolvable host")
        ));
    }

    #[test]
    fn config_alter_should_reject_empty_entity_name() {
        assert!(matches!(
            validate_config_entity_names(
                &[ConfigEntityType::User],
                &[String::new()],
                true,
                false,
            ),
            Err(Error::Usage(message)) if message.contains("--entity-default")
        ));
    }

    #[test]
    fn config_broker_logger_describe_should_require_entity_name() {
        assert!(matches!(
            validate_config_entity_names(
                &[ConfigEntityType::BrokerLogger],
                &[],
                false,
                false,
            ),
            Err(Error::Usage(message)) if message.contains("requires --entity-name")
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
    fn tiered_offset_timestamp_should_match_kafka_protocol_sentinels() {
        assert_eq!(
            [
                tiered_offset_timestamp(ffi::ListOffsetSpec::EarliestLocal),
                tiered_offset_timestamp(ffi::ListOffsetSpec::LatestTiered),
                tiered_offset_timestamp(ffi::ListOffsetSpec::EarliestPendingUpload),
            ],
            [Some(-4), Some(-5), Some(-6)]
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
    fn formatted_leader_epoch_should_render_present_epoch() {
        assert_eq!(formatted_leader_epoch(Some(12)), b"Epoch:12");
    }

    #[test]
    fn formatted_leader_epoch_should_render_missing_epoch() {
        assert_eq!(formatted_leader_epoch(None), b"Epoch:NOT_PRESENT");
    }

    #[test]
    fn share_consumer_formatter_should_preserve_delivery_and_headers() {
        let cli = Cli::try_parse_from([
            "kafka",
            "share-consume",
            "--topic",
            "events",
            "--formatter-property",
            "print.delivery=true",
            "--formatter-property",
            "print.headers=true",
            "--formatter-property",
            "headers.separator=|",
        ])
        .expect("share consumer formatter");
        let Command::ShareConsume(args) = cli.command else {
            panic!("expected share-consume command");
        };
        let options = message_formatter_options(&args).expect("formatter options");

        assert!(options.print_delivery);
        assert!(options.print_headers);
        assert_eq!(
            formatted_share_headers(
                &[
                    (
                        bytes::Bytes::from_static(b"trace"),
                        Some(bytes::Bytes::from_static(b"abc"))
                    ),
                    (bytes::Bytes::from_static(b"empty"), None),
                ],
                &options,
            ),
            b"trace:abc|empty:null"
        );
    }

    #[test]
    fn share_consumer_numeric_properties_should_validate_before_connecting() {
        let mut properties = std::collections::HashMap::new();
        properties.insert("max.poll.records".to_owned(), "invalid".to_owned());
        assert!(matches!(
            share_i32_property(&properties, "max.poll.records"),
            Err(Error::Config(message)) if message.contains("max.poll.records")
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
                ConsumerGroupStateFilter::Stable,
                ConsumerGroupStateFilter::PreparingRebalance,
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
    fn consumer_group_state_filter_should_accept_kafka_4_consumer_states() {
        assert_eq!(
            parse_group_states("Assigning,Reconciling").expect("Kafka 4 states"),
            [
                ConsumerGroupStateFilter::Assigning,
                ConsumerGroupStateFilter::Reconciling,
            ]
        );
    }

    #[test]
    fn consumer_group_state_filter_should_reject_not_ready_like_kafka_consumer_groups() {
        assert!(matches!(
            parse_group_states("NotReady"),
            Err(Error::Usage(message)) if message.contains("NotReady")
        ));
    }

    #[test]
    fn group_state_verbose_table_should_include_consumer_protocol_epochs() {
        let table = group_states_table(
            &[GroupStateRow {
                group: "orders".into(),
                group_type: "Consumer".into(),
                state: "Stable".into(),
                assignor: "uniform".into(),
                members: 1,
                coordinator_id: 1,
                coordinator: "broker:9092".into(),
                group_epoch: Some(7),
                target_assignment_epoch: Some(6),
            }],
            true,
        );

        assert!(table.contains("GROUP_EPOCH") && table.contains('7'));
    }

    #[test]
    fn group_member_verbose_table_should_include_current_and_target_epochs() {
        let table = group_members_table(
            &[GroupMemberRow {
                group: "orders".into(),
                member_id: "member-1".into(),
                instance_id: None,
                client_id: "client-1".into(),
                host: "/127.0.0.1".into(),
                partitions: 1,
                current_epoch: Some(9),
                assignment: "orders:0".into(),
                target_epoch: Some(8),
                target_assignment: "orders:0".into(),
                upgraded: None,
            }],
            true,
        );

        assert!(table.contains("CURRENT_EPOCH") && table.contains('9'));
    }

    #[test]
    fn group_member_verbose_table_should_show_upgraded_for_migration_group() {
        let row = |member_id: &str, upgraded| GroupMemberRow {
            group: "orders".into(),
            member_id: member_id.into(),
            instance_id: None,
            client_id: member_id.into(),
            host: "/127.0.0.1".into(),
            partitions: 0,
            current_epoch: None,
            assignment: String::new(),
            target_epoch: None,
            target_assignment: String::new(),
            upgraded: Some(upgraded),
        };
        let table = group_members_table(&[row("classic", false), row("consumer", true)], true);

        assert!(table.contains("UPGRADED"));
    }

    #[test]
    fn group_member_verbose_table_should_omit_upgraded_for_unknown_type() {
        let table = group_members_table(
            &[GroupMemberRow {
                group: "orders".into(),
                member_id: "member-1".into(),
                instance_id: None,
                client_id: "client-1".into(),
                host: "/127.0.0.1".into(),
                partitions: 0,
                current_epoch: None,
                assignment: String::new(),
                target_epoch: None,
                target_assignment: String::new(),
                upgraded: None,
            }],
            true,
        );

        assert!(!table.contains("UPGRADED"));
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
    fn acl_bindings_should_reject_operation_unsupported_by_resource() {
        let cli = Cli::try_parse_from([
            "kafka",
            "--bootstrap-server",
            "localhost:9092",
            "acls",
            "add",
            "--group",
            "billing",
            "--allow-principal",
            "User:reader",
            "--operation",
            "write",
        ])
        .expect("ACL command");
        let Command::Acls(args) = cli.command else {
            panic!("expected ACL command");
        };
        let AclAction::Add(mutation) = args.action else {
            panic!("expected ACL add");
        };
        let operations = acl_operations(&mutation.operation).expect("operations");
        let error = acl_bindings(&mutation, &operations).expect_err("unsupported operation");
        assert!(
            error
                .to_string()
                .contains("Group does not support operation Write")
        );
    }

    #[test]
    fn acl_bindings_should_accept_group_config_operations() {
        let cli = Cli::try_parse_from([
            "kafka",
            "--bootstrap-server",
            "localhost:9092",
            "acls",
            "add",
            "--group",
            "billing",
            "--allow-principal",
            "User:operator",
            "--operation",
            "alter-configs",
        ])
        .expect("ACL command");
        let Command::Acls(args) = cli.command else {
            panic!("expected ACL command");
        };
        let AclAction::Add(mutation) = args.action else {
            panic!("expected ACL add");
        };
        let operations = acl_operations(&mutation.operation).expect("operations");
        assert!(acl_bindings(&mutation, &operations).is_ok());
    }

    #[test]
    fn acl_consumer_role_should_reject_transactional_resource_without_producer() {
        let cli = Cli::try_parse_from([
            "kafka",
            "--bootstrap-server",
            "localhost:9092",
            "acls",
            "add",
            "--consumer",
            "--topic",
            "orders",
            "--group",
            "billing",
            "--transactional-id",
            "billing-tx",
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
        let error = acl_bindings(&mutation, &[]).expect_err("invalid role resources");
        assert!(error.to_string().contains("unless --producer is also set"));
    }

    #[test]
    fn acl_operations_should_report_librdkafka_new_operation_boundary() {
        let error = acl_operations(&["two-phase-commit".into()])
            .expect_err("unsupported librdkafka operation");
        assert!(error.to_string().contains("librdkafka 2.12"));
    }

    #[test]
    fn acl_operations_should_trim_and_deduplicate_values() {
        assert_eq!(
            acl_operations(&[" read ".into(), "READ".into()]).expect("operations"),
            [AclOperation::Read]
        );
    }

    #[test]
    fn filter_missing_acl_bindings_should_skip_existing_exact_binding() {
        let existing_binding = AclBinding {
            resource_type: AclResourceType::Topic,
            resource_name: "orders".into(),
            pattern_type: AclPatternType::Literal,
            principal: "User:reader".into(),
            host: "*".into(),
            operation: AclOperation::Read,
            permission_type: AclPermissionType::Allow,
        };
        let missing_binding = AclBinding {
            principal: "User:writer".into(),
            ..existing_binding.clone()
        };
        let (missing, already_exists) = filter_missing_acl_bindings(
            vec![existing_binding.clone(), missing_binding.clone()],
            &HashSet::from([existing_binding]),
        );
        assert_eq!((missing, already_exists), (vec![missing_binding], 1));
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
    fn reset_csv_should_return_usage_error_when_single_group_is_not_selected() {
        let mut file = tempfile::NamedTempFile::new().expect("temporary reset CSV");
        writeln!(file, "events,0,1").expect("write reset CSV");
        let mut config = rdkafka::ClientConfig::new();
        config.set("bootstrap.servers", "127.0.0.1:1");

        let error = read_reset_plan(file.path(), &[], true, &config, Duration::from_millis(1))
            .expect_err("missing selected group");

        assert!(error.to_string().contains("no group was selected"));
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

    fn listed_group(group_type: &str, protocol: &str) -> AllGroupRow {
        AllGroupRow {
            group: "test-group".into(),
            group_type: group_type.into(),
            protocol: protocol.into(),
        }
    }

    #[test]
    fn all_groups_consumer_filter_should_include_simple_classic_group() {
        let row = listed_group("Classic", "");

        assert!(all_group_matches(&row, None, None, true, false, false));
    }

    #[test]
    fn all_groups_consumer_filter_should_exclude_share_group() {
        let row = listed_group("Share", "share");

        assert!(!all_group_matches(&row, None, None, true, false, false));
    }

    #[test]
    fn all_groups_type_and_protocol_filters_should_both_match() {
        let row = listed_group("Share", "share");

        assert!(all_group_matches(
            &row,
            Some(AllGroupType::Share),
            Some("share"),
            false,
            false,
            false,
        ));
    }

    #[test]
    fn all_groups_type_and_protocol_filters_should_reject_protocol_mismatch() {
        let row = listed_group("Share", "share");

        assert!(!all_group_matches(
            &row,
            Some(AllGroupType::Share),
            Some("consumer"),
            false,
            false,
            false,
        ));
    }

    #[test]
    fn share_group_states_should_accept_all_original_values_case_insensitively() {
        let states = parse_share_group_states("stable, EMPTY,Dead").expect("valid states");

        assert_eq!(
            states,
            BTreeSet::from(["Dead".into(), "Empty".into(), "Stable".into()])
        );
    }

    #[test]
    fn share_group_states_should_reject_consumer_only_state() {
        let error = parse_share_group_states("PreparingRebalance").expect_err("invalid state");

        assert!(error.to_string().contains("Empty, Stable, or Dead"));
    }

    #[test]
    fn share_assignment_should_sort_partitions() {
        let partitions = [2, 0, 1];

        assert_eq!(
            share_assignment_parts(std::iter::once(("events", partitions.as_slice()))),
            "events:0,1,2"
        );
    }

    #[test]
    fn streams_group_states_should_accept_all_original_values_case_insensitively() {
        let states = parse_streams_group_states("empty,NOTREADY,Stable,Assigning,reconciling,DEAD")
            .expect("valid states");

        assert_eq!(states.len(), 6);
    }

    #[test]
    fn streams_group_states_should_reject_consumer_only_state() {
        let error = parse_streams_group_states("PreparingRebalance").expect_err("invalid state");

        assert!(error.to_string().contains("NotReady"));
    }

    #[test]
    fn streams_application_reset_should_match_only_kafka_internal_topic_formats() {
        let topics = [
            "app-store-changelog".into(),
            "app-join-repartition".into(),
            "app-KTABLE-FK-JOIN-SUBSCRIPTION-RESPONSE-12-topic".into(),
            "app-output".into(),
            "other-store-changelog".into(),
        ];

        let inferred =
            inferred_streams_internal_topics("app", topics, &BTreeSet::new(), &BTreeSet::new());

        assert_eq!(
            inferred,
            BTreeSet::from([
                "app-KTABLE-FK-JOIN-SUBSCRIPTION-RESPONSE-12-topic".into(),
                "app-join-repartition".into(),
                "app-store-changelog".into(),
            ])
        );
    }

    #[test]
    fn streams_application_reset_should_exclude_named_input_from_internal_topics() {
        let topic = "app-input-changelog".to_owned();

        let inferred = inferred_streams_internal_topics(
            "app",
            [topic.clone()],
            &BTreeSet::from([topic]),
            &BTreeSet::new(),
        );

        assert!(inferred.is_empty());
    }

    #[test]
    fn streams_application_reset_plan_should_reject_duplicate_partition() {
        let mut file = tempfile::NamedTempFile::new().expect("temporary reset plan");
        writeln!(file, "input,0,1\ninput,0,2").expect("write reset plan");

        let error = read_streams_application_reset_plan(file.path()).expect_err("duplicate");

        assert!(error.to_string().contains("duplicate reset CSV target"));
    }

    #[test]
    fn streams_group_describe_request_should_not_tag_primitive_group_ids() {
        let mut buffer = BytesMut::new();

        encode_streams_describe_request("g", 0, false, &mut buffer);

        assert_eq!(buffer.as_ref(), [0, 2, 2, b'g', 0, 0]);
    }

    #[test]
    fn streams_group_v0_decoder_should_accept_broker_null_topology_marker() {
        let bytes = STANDARD
            .decode("AAAAAAACAEUnR3JvdXAgbWlzc2luZy1zdHJlYW1zLWdyb3VwIG5vdCBmb3VuZC4WbWlzc2luZy1zdHJlYW1zLWdyb3VwAQAAAAAAAAAA/wGAAAAAAAA=")
            .expect("Kafka 4.3 response fixture");
        let mut buffer = bytes.as_slice();
        skip_tagged_fields(&mut buffer).expect("response header tags");
        i32::decode(&mut buffer).expect("throttle time");
        assert_eq!(decode_compact_len(&mut buffer).expect("group count"), 1);

        let (error_code, _, description) =
            decode_streams_group_description(&mut buffer, 0).expect("decode missing group");

        assert_eq!(error_code, 69);
        assert!(description.topology.is_none());
    }

    fn encode_empty_streams_assignment(buffer: &mut BytesMut) {
        buffer.put_u8(1); // active tasks
        buffer.put_u8(1); // standby tasks
        buffer.put_u8(1); // warmup tasks
        buffer.put_u8(0); // assignment tagged fields
    }

    #[test]
    fn streams_group_v0_decoder_should_preserve_topology_and_member_assignment() {
        let mut buffer = BytesMut::new();
        buffer.put_i16(0);
        buffer.put_u8(0);
        encode_compact_string("streams-app", &mut buffer);
        encode_compact_string("Stable", &mut buffer);
        buffer.put_i32(4);
        buffer.put_i32(3);
        buffer.put_u8(1); // non-null topology
        buffer.put_i32(7);
        buffer.put_u8(2); // one subtopology
        encode_compact_string("sub-0", &mut buffer);
        buffer.put_u8(2);
        encode_compact_string("input", &mut buffer);
        buffer.put_u8(1); // no repartition sink topics
        buffer.put_u8(2); // one changelog topic
        encode_compact_string("streams-app-store-changelog", &mut buffer);
        buffer.put_i32(0);
        buffer.put_i16(1);
        buffer.put_u8(1); // no topic configs
        buffer.put_u8(0);
        buffer.put_u8(1); // no repartition source topics
        buffer.put_u8(0); // subtopology tagged fields
        buffer.put_u8(0); // topology tagged fields
        buffer.put_u8(2); // one member
        encode_compact_string("member-1", &mut buffer);
        buffer.put_i32(2);
        buffer.put_u8(0); // instance ID
        buffer.put_u8(0); // rack ID
        encode_compact_string("client-1", &mut buffer);
        encode_compact_string("/127.0.0.1", &mut buffer);
        buffer.put_i32(7);
        encode_compact_string("process-1", &mut buffer);
        buffer.put_u8(0xff); // null endpoint
        buffer.put_u8(1); // client tags
        buffer.put_u8(1); // task offsets
        buffer.put_u8(1); // task end offsets
        buffer.put_u8(2); // one active task
        encode_compact_string("sub-0", &mut buffer);
        buffer.put_u8(2); // one partition
        buffer.put_i32(0);
        buffer.put_u8(0); // task tagged fields
        buffer.put_u8(1); // standby tasks
        buffer.put_u8(1); // warmup tasks
        buffer.put_u8(0); // assignment tagged fields
        encode_empty_streams_assignment(&mut buffer);
        buffer.put_u8(0); // is classic
        buffer.put_u8(0); // member tagged fields
        buffer.put_i32(i32::MIN);
        buffer.put_u8(0); // group tagged fields

        let (_, _, description) =
            decode_streams_group_description(&mut buffer, 0).expect("decode v0 description");

        assert_eq!(
            description.topology.expect("topology").subtopologies[0].source_topics,
            ["input"]
        );
        assert_eq!(description.members[0].assignment.active[0].partitions, [0]);
    }

    fn encode_streams_topology_node(buffer: &mut BytesMut, name: &str, node_type: i8) {
        encode_compact_string(name, buffer);
        buffer.put_i8(node_type);
        buffer.put_u8(2);
        encode_compact_string("input", buffer);
        buffer.put_u8(0); // sink topic
        buffer.put_u8(1); // stores
        buffer.put_u8(1); // successors
        buffer.put_u8(0); // tagged fields
    }

    #[test]
    fn streams_group_v1_decoder_should_preserve_topology_description_and_assignor() {
        let mut buffer = BytesMut::new();
        buffer.put_i16(0);
        buffer.put_u8(0);
        encode_compact_string("streams-app", &mut buffer);
        encode_compact_string("Empty", &mut buffer);
        buffer.put_i32(4);
        buffer.put_i32(3);
        buffer.put_u8(0xff); // null topology metadata
        buffer.put_u8(1); // members
        buffer.put_i32(i32::MIN);
        buffer.put_u8(1); // topology description
        buffer.put_u8(2); // one subtopology
        encode_compact_string("sub-0", &mut buffer);
        buffer.put_u8(2); // one node
        encode_streams_topology_node(&mut buffer, "source", 1);
        buffer.put_u8(0); // subtopology tagged fields
        buffer.put_u8(1); // global stores
        buffer.put_u8(0); // topology description tagged fields
        buffer.put_i8(3);
        encode_compact_string("uniform", &mut buffer);
        buffer.put_u8(0); // group tagged fields

        let (_, _, description) =
            decode_streams_group_description(&mut buffer, 1).expect("decode v1 description");

        assert_eq!(description.assignor.as_deref(), Some("uniform"));
        assert_eq!(
            description
                .topology_description
                .expect("topology description")
                .subtopologies[0]
                .nodes[0]
                .name,
            "source"
        );
    }
}
