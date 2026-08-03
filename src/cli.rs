//! Command-line interface definitions and Kafka script compatibility dispatch.

use std::{ffi::OsString, path::PathBuf, time::Duration};

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::output::OutputFormat;

fn parse_consumer_timeout(value: &str) -> Result<u64, String> {
    let timeout = value
        .parse::<i64>()
        .map_err(|error| format!("invalid timeout '{value}': {error}"))?;
    Ok(u64::try_from(timeout).unwrap_or(u64::MAX))
}

/// Native Kafka command-line client.
#[derive(Debug, Parser)]
#[command(name = "kafka", version, about, propagate_version = true)]
pub struct Cli {
    /// Comma-separated Kafka bootstrap brokers.
    #[arg(short = 'b', long, global = true, env = "KAFKA_CLI_BOOTSTRAP_SERVER")]
    pub bootstrap_server: Option<String>,

    /// Kafka Java-compatible client properties file.
    #[arg(short = 'c', long, global = true, env = "KAFKA_CLI_COMMAND_CONFIG")]
    pub command_config: Option<PathBuf>,

    /// Request timeout in milliseconds.
    #[arg(long, global = true)]
    pub timeout_ms: Option<u64>,

    /// Output encoding.
    #[arg(long, global = true, value_enum, default_value_t)]
    pub output: OutputFormat,

    /// Increase diagnostic verbosity.
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Command to execute.
    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    /// Parses arguments, selecting a compatibility command from the executable name.
    #[must_use]
    pub fn parse_compatible() -> Self {
        let mut args: Vec<OsString> = std::env::args_os().collect();
        let executable = args
            .first()
            .and_then(|arg| std::path::Path::new(arg).file_name())
            .and_then(|name| name.to_str())
            .unwrap_or("kafka");
        if let Some(command) = compatibility_command(executable) {
            args.insert(1, OsString::from(command));
            rewrite_legacy_action(&mut args, command);
        }
        Self::parse_from(args)
    }

    /// Returns the configured timeout.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        Duration::from_millis(match self.timeout_ms {
            Some(timeout_ms) => timeout_ms,
            None => 30_000,
        })
    }
}

fn compatibility_command(executable: &str) -> Option<&'static str> {
    let name = executable.strip_suffix(".sh").unwrap_or(executable);
    match name {
        "kafka-topics" => Some("topics"),
        "kafka-console-producer" => Some("produce"),
        "kafka-console-consumer" => Some("consume"),
        "kafka-consumer-groups" => Some("groups"),
        "kafka-groups" => Some("all-groups"),
        "kafka-configs" => Some("configs"),
        "kafka-get-offsets" => Some("offsets"),
        "kafka-acls" => Some("acls"),
        "kafka-reassign-partitions" => Some("reassign"),
        "kafka-delete-records" => Some("delete-records"),
        "kafka-leader-election" => Some("leader-election"),
        "kafka-log-dirs" => Some("log-dirs"),
        "kafka-broker-api-versions" => Some("api-versions"),
        "kafka-cluster" => Some("cluster"),
        "kafka-client-metrics" => Some("client-metrics"),
        "kafka-features" => Some("features"),
        "kafka-transactions" => Some("transactions"),
        "kafka-metadata-quorum" => Some("metadata-quorum"),
        "kafka-delegation-tokens" => Some("delegation-tokens"),
        _ => None,
    }
}

fn rewrite_legacy_action(args: &mut Vec<OsString>, command: &str) {
    let deprecated_configs: &[&str] = match command {
        "produce" => &["--producer.config"],
        "consume" => &["--consumer.config"],
        "leader-election" => &["--admin.config"],
        "cluster" => &["--config"],
        _ => &[],
    };
    for &deprecated in deprecated_configs {
        for argument in args.iter_mut() {
            let Some(value) = argument.to_str() else {
                continue;
            };
            if value == deprecated {
                *argument = OsString::from("--command-config");
            } else if let Some(path) = value
                .strip_prefix(deprecated)
                .and_then(|tail| tail.strip_prefix('='))
            {
                *argument = OsString::from(format!("--command-config={path}"));
            }
        }
    }
    if command == "configs" {
        rewrite_legacy_config_entities(args);
    }
    let candidates: &[(&str, &str, bool)] = match command {
        "topics" => &[
            ("--create", "create", false),
            ("--delete", "delete", false),
            ("--describe", "describe", false),
            ("--alter", "alter", false),
            ("--list", "list", false),
        ],
        "groups" => &[
            ("--validate-regex", "validate-regex", false),
            ("--describe", "describe", false),
            ("--delete-offsets", "delete-offsets", true),
            ("--reset-offsets", "reset-offsets", false),
            ("--delete", "delete", true),
            ("--list", "list", false),
        ],
        "all-groups" => &[("--list", "list", false)],
        "configs" => &[
            ("--describe", "describe", false),
            ("--alter", "alter", true),
        ],
        "acls" => &[
            ("--add", "add", true),
            ("--remove", "remove", false),
            ("--list", "list", false),
        ],
        "reassign" => &[
            ("--generate", "generate", false),
            ("--execute", "execute", true),
            ("--verify", "verify", false),
            ("--cancel", "cancel", true),
            ("--list", "list", false),
        ],
        "client-metrics" => &[
            ("--alter", "alter", true),
            ("--delete", "delete", true),
            ("--describe", "describe", false),
            ("--list", "list", false),
        ],
        "delegation-tokens" => &[
            ("--create", "create", false),
            ("--renew", "renew", false),
            ("--expire", "expire", false),
            ("--describe", "describe", false),
        ],
        _ => &[],
    };
    for (flag, action, execute) in candidates {
        if let Some(index) = args.iter().position(|arg| arg == flag) {
            args.remove(index);
            args.insert(2, OsString::from(action));
            if *execute && !args.iter().any(|arg| arg == "--execute") {
                args.insert(3, OsString::from("--execute"));
            }
            return;
        }
    }
    if args.iter().any(|arg| arg == "--execute") {
        return;
    }
    match command {
        "delete-records" | "leader-election" => {
            args.insert(2, OsString::from("--execute"));
        }
        "cluster" => {
            if let Some(index) = args.iter().position(|arg| arg == "unregister") {
                args.insert(index + 1, OsString::from("--execute"));
            }
        }
        _ => {}
    }
}

fn rewrite_legacy_config_entities(args: &mut Vec<OsString>) {
    let mut types = Vec::new();
    let mut selectors = Vec::new();
    let mut remove = vec![false; args.len()];
    let mut index = 0;
    while index < args.len() {
        let Some(value) = args[index].to_str() else {
            index += 1;
            continue;
        };
        if value == "--entity-type" && index + 1 < args.len() {
            if let Some(kind) = args[index + 1].to_str() {
                types.push(kind.to_owned());
                remove[index] = true;
                remove[index + 1] = true;
                index += 2;
                continue;
            }
        } else if let Some(kind) = value.strip_prefix("--entity-type=") {
            types.push(kind.to_owned());
            remove[index] = true;
        } else if value == "--entity-name" && index + 1 < args.len() {
            if let Some(name) = args[index + 1].to_str() {
                selectors.push(Some(name.to_owned()));
                remove[index] = true;
                remove[index + 1] = true;
                index += 2;
                continue;
            }
        } else if let Some(name) = value.strip_prefix("--entity-name=") {
            selectors.push(Some(name.to_owned()));
            remove[index] = true;
        } else if value == "--entity-default" {
            selectors.push(None);
            remove[index] = true;
        }
        index += 1;
    }
    if types.len() < 2 || types.len() != selectors.len() || !selectors.iter().any(Option::is_none) {
        return;
    }
    let mut replacements = Vec::new();
    for (kind, selector) in types.iter().zip(selectors) {
        let named = match kind.as_str() {
            "topics" | "topic" => "--topic",
            "clients" | "client" => "--client",
            "users" | "user" => "--user",
            "brokers" | "broker" => "--broker",
            "broker-loggers" | "broker-logger" => "--broker-logger",
            "ips" | "ip" => "--ip",
            "client-metrics" | "client-metric" => "--client-metrics",
            "groups" | "group" => "--group",
            _ => return,
        };
        if let Some(name) = selector {
            replacements.push(OsString::from(named));
            replacements.push(OsString::from(name));
        } else {
            let defaults = match kind.as_str() {
                "clients" | "client" => "--client-defaults",
                "users" | "user" => "--user-defaults",
                "brokers" | "broker" => "--broker-defaults",
                "ips" | "ip" => "--ip-defaults",
                _ => return,
            };
            replacements.push(OsString::from(defaults));
        }
    }
    let mut retained = args
        .drain(..)
        .enumerate()
        .filter_map(|(index, argument)| (!remove[index]).then_some(argument))
        .collect::<Vec<_>>();
    retained.extend(replacements);
    *args = retained;
}

/// Top-level command suite.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Manage topics.
    Topics(TopicsArgs),
    /// Produce records from stdin.
    Produce(ProduceArgs),
    /// Consume records to stdout.
    Consume(ConsumeArgs),
    /// Inspect and manage consumer groups.
    Groups(GroupsArgs),
    /// List groups of every Kafka group type.
    AllGroups(AllGroupsArgs),
    /// Inspect and alter dynamic configuration.
    Configs(ConfigsArgs),
    /// Query partition offsets.
    Offsets(OffsetsArgs),
    /// Inspect and manage ACLs.
    Acls(AclsArgs),
    /// Manage partition reassignments.
    Reassign(ReassignArgs),
    /// Delete records before specified offsets.
    DeleteRecords(DeleteRecordsArgs),
    /// Trigger partition leader election.
    LeaderElection(LeaderElectionArgs),
    /// Inspect broker log directories.
    LogDirs(LogDirsArgs),
    /// Inspect broker Kafka protocol versions.
    ApiVersions(ApiVersionsArgs),
    /// Inspect and manage cluster metadata.
    Cluster(ClusterArgs),
    /// Inspect and manage client metrics subscriptions.
    ClientMetrics(ClientMetricsArgs),
    /// Inspect and manage Kafka feature levels.
    Features(FeaturesArgs),
    /// Analyze and recover transactional producer state.
    Transactions(TransactionsArgs),
    /// Inspect and modify the `KRaft` metadata quorum.
    MetadataQuorum(MetadataQuorumArgs),
    /// Create, inspect, renew, and expire delegation tokens.
    DelegationTokens(DelegationTokensArgs),
}

#[derive(Debug, Args)]
pub struct DelegationTokensArgs {
    #[command(subcommand)]
    pub action: DelegationTokenAction,
}

#[derive(Debug, Subcommand)]
pub enum DelegationTokenAction {
    /// Create a delegation token.
    Create {
        #[arg(long = "owner-principal")]
        owner_principal: Vec<String>,
        #[arg(long = "renewer-principal")]
        renewer_principal: Vec<String>,
        #[arg(long = "max-life-time-period", allow_hyphen_values = true)]
        max_life_time_period: i64,
    },
    /// Renew a delegation token.
    Renew {
        #[arg(long)]
        hmac: String,
        #[arg(long = "renew-time-period", allow_hyphen_values = true)]
        renew_time_period: i64,
    },
    /// Expire a delegation token.
    Expire {
        #[arg(long)]
        hmac: String,
        #[arg(long = "expiry-time-period", allow_hyphen_values = true)]
        expiry_time_period: i64,
    },
    /// Describe visible delegation tokens.
    Describe {
        #[arg(long = "owner-principal")]
        owner_principal: Vec<String>,
    },
}

#[derive(Debug, Args)]
pub struct MetadataQuorumArgs {
    /// Connect directly to a `KRaft` controller listener.
    #[arg(long, conflicts_with = "bootstrap_server")]
    pub bootstrap_controller: Option<String>,
    #[command(subcommand)]
    pub action: MetadataQuorumAction,
}

#[derive(Debug, Subcommand)]
pub enum MetadataQuorumAction {
    /// Describe quorum status or replication state.
    Describe {
        #[arg(
            long,
            required_unless_present = "replication",
            conflicts_with = "replication"
        )]
        status: bool,
        #[arg(long, required_unless_present = "status", conflicts_with = "status")]
        replication: bool,
        #[arg(long, requires = "replication")]
        human_readable: bool,
    },
    /// Add the controller described by the command config file.
    AddController {
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove a controller from the voter set.
    RemoveController {
        #[arg(short = 'i', long)]
        controller_id: i32,
        #[arg(short = 'd', long)]
        controller_directory_id: String,
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Args)]
pub struct TransactionsArgs {
    #[command(subcommand)]
    pub action: TransactionAction,
}

#[derive(Debug, Subcommand)]
pub enum TransactionAction {
    /// List transactions known to every broker coordinator.
    List {
        #[arg(long)]
        duration_filter: Option<i64>,
        #[arg(long)]
        transactional_id_pattern: Option<String>,
    },
    /// Describe one transactional ID.
    Describe {
        #[arg(long)]
        transactional_id: String,
    },
    /// Describe active producers for one topic-partition.
    DescribeProducers {
        #[arg(long)]
        broker_id: Option<i32>,
        #[arg(long)]
        topic: String,
        #[arg(long)]
        partition: i32,
    },
    /// Abort an open transaction on one topic-partition.
    Abort {
        #[arg(long)]
        topic: String,
        #[arg(long)]
        partition: i32,
        #[arg(long, conflicts_with_all = ["producer_id", "producer_epoch", "coordinator_epoch"])]
        start_offset: Option<i64>,
        #[arg(long, requires_all = ["producer_epoch", "coordinator_epoch"])]
        producer_id: Option<i64>,
        #[arg(long, requires_all = ["producer_id", "coordinator_epoch"])]
        producer_epoch: Option<i16>,
        #[arg(long, requires_all = ["producer_id", "producer_epoch"])]
        coordinator_epoch: Option<i32>,
    },
    /// Locate open transactions no longer owned by a coordinator.
    FindHanging {
        #[arg(long)]
        broker_id: Option<i32>,
        #[arg(long, default_value_t = 15)]
        max_transaction_timeout: i32,
        #[arg(long)]
        topic: Option<String>,
        #[arg(long, requires = "topic")]
        partition: Option<i32>,
    },
    /// Fence the producer and force termination of its current transaction.
    #[command(name = "forceTerminateTransaction")]
    ForceTerminateTransaction {
        #[arg(long = "transactionalId")]
        transactional_id: String,
    },
}

#[derive(Debug, Args)]
pub struct FeaturesArgs {
    /// Connect directly to a `KRaft` controller listener.
    #[arg(long, conflicts_with = "bootstrap_server")]
    pub bootstrap_controller: Option<String>,
    #[command(subcommand)]
    pub action: FeatureAction,
}

#[derive(Debug, Subcommand)]
pub enum FeatureAction {
    /// Describe supported and finalized feature levels.
    Describe {
        #[arg(long)]
        node_id: Option<i32>,
    },
    /// Upgrade one or more feature levels.
    Upgrade {
        #[arg(long, conflicts_with = "release_version")]
        metadata: Option<String>,
        #[arg(long, conflicts_with_all = ["metadata", "feature"])]
        release_version: Option<String>,
        #[arg(long)]
        feature: Vec<String>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Downgrade one or more feature levels.
    Downgrade {
        #[arg(long, conflicts_with = "release_version")]
        metadata: Option<String>,
        #[arg(long, conflicts_with_all = ["metadata", "feature"])]
        release_version: Option<String>,
        #[arg(long)]
        feature: Vec<String>,
        #[arg(long)]
        r#unsafe: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// Disable features by setting their levels to zero.
    Disable {
        #[arg(long, required = true)]
        feature: Vec<String>,
        #[arg(long)]
        r#unsafe: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// Show feature defaults for a Kafka release version.
    VersionMapping {
        #[arg(long)]
        release_version: Option<String>,
    },
    /// Show dependencies for feature levels.
    FeatureDependencies {
        #[arg(long, required = true)]
        feature: Vec<String>,
    },
}

#[derive(Debug, Args)]
pub struct ClientMetricsArgs {
    #[command(subcommand)]
    pub action: ClientMetricsAction,
}

#[derive(Debug, Subcommand)]
pub enum ClientMetricsAction {
    /// List client metrics resource names.
    List,
    /// Describe one or all client metrics resources.
    Describe {
        #[arg(long)]
        name: Option<String>,
    },
    /// Alter a client metrics resource.
    Alter {
        #[arg(
            long,
            required_unless_present = "generate_name",
            conflicts_with = "generate_name"
        )]
        name: Option<String>,
        #[arg(long)]
        generate_name: bool,
        #[arg(long, allow_hyphen_values = true)]
        interval: Option<String>,
        #[arg(long)]
        r#match: Vec<String>,
        #[arg(long)]
        metrics: Vec<String>,
        #[arg(long)]
        execute: bool,
    },
    /// Delete all dynamic configuration for a client metrics resource.
    Delete {
        #[arg(long)]
        name: String,
        #[arg(long)]
        execute: bool,
    },
}

#[derive(Debug, Args)]
pub struct TopicsArgs {
    #[command(subcommand)]
    pub action: TopicAction,
}

#[derive(Debug, Subcommand)]
pub enum TopicAction {
    List(ListTopicArgs),
    Describe(DescribeTopicArgs),
    Create(CreateTopicArgs),
    Alter(AlterTopicArgs),
    Delete(DeleteTopicArgs),
}

#[derive(Debug, Args)]
pub struct ListTopicArgs {
    /// Topic name or regular expression. Omit to select all topics.
    #[arg(long)]
    pub topic: Option<String>,
    /// Exclude Kafka internal topics.
    #[arg(long)]
    pub exclude_internal: bool,
}

#[derive(Debug, Args)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Kafka exposes these mutually exclusive partition report modes as flags"
)]
pub struct DescribeTopicArgs {
    /// Topic name or regular expression. Omit to select all topics.
    #[arg(long)]
    pub topic: Option<String>,
    /// Kafka topic UUID. A non-zero ID takes precedence over --topic.
    #[arg(long)]
    pub topic_id: Option<String>,
    /// Exclude Kafka internal topics.
    #[arg(long)]
    pub exclude_internal: bool,
    /// Only show partitions whose ISR is smaller than the replica set.
    #[arg(long)]
    pub under_replicated_partitions: bool,
    /// Only show partitions that currently have no leader.
    #[arg(long)]
    pub unavailable_partitions: bool,
    /// Only show partitions whose ISR is smaller than min.insync.replicas.
    #[arg(long, conflicts_with_all = ["at_min_isr_partitions", "topics_with_overrides"])]
    pub under_min_isr_partitions: bool,
    /// Only show partitions whose ISR equals min.insync.replicas.
    #[arg(long, conflicts_with_all = ["under_min_isr_partitions", "topics_with_overrides"])]
    pub at_min_isr_partitions: bool,
    /// Only show topics with dynamic topic-level configuration overrides.
    #[arg(long, conflicts_with_all = ["under_min_isr_partitions", "at_min_isr_partitions", "under_replicated_partitions", "unavailable_partitions"])]
    pub topics_with_overrides: bool,
    /// Succeed when no topic matches the name or ID.
    #[arg(long)]
    pub if_exists: bool,
    /// Maximum partitions requested per `DescribeTopicPartitions` response.
    #[arg(long, value_parser = clap::value_parser!(i32).range(1..))]
    pub partition_size_limit_per_response: Option<i32>,
}

#[derive(Debug, Args)]
pub struct DeleteTopicArgs {
    /// Topic name or regular expression.
    #[arg(long)]
    pub topic: String,
    /// Succeed when no topic matches the expression.
    #[arg(long)]
    pub if_exists: bool,
}

#[derive(Debug, Args)]
pub struct CreateTopicArgs {
    #[arg(long)]
    pub topic: String,
    /// Partition count. Omit to use the broker's default.
    #[arg(long, conflicts_with = "replica_assignment")]
    pub partitions: Option<i32>,
    /// Replication factor. Omit to use the broker's default.
    #[arg(long, conflicts_with = "replica_assignment")]
    pub replication_factor: Option<i32>,
    /// Manual assignments such as 1:2,2:3 (one comma-separated entry per partition).
    #[arg(long, conflicts_with_all = ["partitions", "replication_factor"])]
    pub replica_assignment: Option<String>,
    /// Topic configuration in key=value form.
    #[arg(long = "config")]
    pub configs: Vec<String>,
    #[arg(long)]
    pub if_not_exists: bool,
}

#[derive(Debug, Args)]
pub struct AlterTopicArgs {
    #[arg(long)]
    pub topic: String,
    /// New total partition count.
    #[arg(long)]
    pub partitions: i32,
    /// Full manual assignment including existing and newly added partitions.
    #[arg(long)]
    pub replica_assignment: Option<String>,
    #[arg(long)]
    pub if_exists: bool,
}

#[derive(Debug, Args)]
pub struct ProduceArgs {
    #[arg(long)]
    pub topic: String,
    /// Kafka reader class; only the built-in `LineMessageReader` is available natively.
    #[arg(long, default_value = "org.apache.kafka.tools.LineMessageReader")]
    pub line_reader: String,
    #[arg(long)]
    pub key_separator: Option<char>,
    #[arg(long)]
    pub parse_key: bool,
    #[arg(
        long,
        visible_alias = "compression-codec",
        num_args = 0..=1,
        default_missing_value = "gzip",
        default_value = "none",
        value_parser = ["none", "gzip", "snappy", "lz4", "zstd"]
    )]
    pub compression_type: String,
    #[arg(long, visible_alias = "request-required-acks")]
    pub acks: Option<String>,
    /// Wait for each delivery before reading and sending the next record.
    #[arg(long)]
    pub sync: bool,
    #[arg(long)]
    pub batch_size: Option<usize>,
    /// Deprecated Kafka alias for batch-size; takes precedence when both are supplied.
    #[arg(long)]
    pub max_partition_memory_bytes: Option<usize>,
    #[arg(long)]
    pub message_send_max_retries: Option<u32>,
    #[arg(long)]
    pub retry_backoff_ms: Option<u64>,
    /// Maximum batching delay; maps to librdkafka linger.ms.
    #[arg(long = "timeout")]
    pub linger_ms: Option<u64>,
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    pub request_timeout_ms: Option<u64>,
    #[arg(long)]
    pub metadata_expiry_ms: Option<u64>,
    /// Maximum time to wait for space in the local producer queue.
    #[arg(long)]
    pub max_block_ms: Option<u64>,
    /// Maximum buffered producer memory in bytes.
    #[arg(long)]
    pub max_memory_bytes: Option<usize>,
    #[arg(long)]
    pub socket_buffer_size: Option<i32>,
    /// Parse each input line as {"key":...,"value":...,"partition":...,"headers":{...}}.
    #[arg(long)]
    pub json: bool,
    /// Default `LineMessageReader` property in key=value form.
    #[arg(
        long = "reader-property",
        conflicts_with = "deprecated_reader_properties"
    )]
    pub reader_properties: Vec<String>,
    /// Deprecated alias for --reader-property.
    #[arg(long = "property", conflicts_with = "reader_properties")]
    pub deprecated_reader_properties: Vec<String>,
    /// Java properties file used to configure the default `LineMessageReader`.
    #[arg(long)]
    pub reader_config: Option<PathBuf>,
    /// Producer property in key=value form; overrides --command-config.
    #[arg(long = "command-property", conflicts_with = "deprecated_properties")]
    pub properties: Vec<String>,
    /// Deprecated alias for --command-property.
    #[arg(long = "producer-property", conflicts_with = "properties")]
    pub deprecated_properties: Vec<String>,
}

impl ProduceArgs {
    pub(crate) fn reader_properties(&self) -> &[String] {
        if self.reader_properties.is_empty() {
            &self.deprecated_reader_properties
        } else {
            &self.reader_properties
        }
    }

    pub(crate) fn properties(&self) -> &[String] {
        if self.properties.is_empty() {
            &self.deprecated_properties
        } else {
            &self.properties
        }
    }
}

#[derive(Debug, Args)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Kafka console consumer exposes independent formatting and error policy flags"
)]
pub struct ConsumeArgs {
    #[arg(long, required_unless_present = "include", conflicts_with = "include")]
    pub topic: Option<String>,
    /// Full-match librdkafka regular expression selecting topics.
    #[arg(long, conflicts_with_all = ["topic", "partition", "offset"])]
    pub include: Option<String>,
    #[arg(long)]
    pub group: Option<String>,
    /// Kafka formatter class; only the built-in `DefaultMessageFormatter` is available natively.
    #[arg(
        long,
        default_value = "org.apache.kafka.tools.consumer.DefaultMessageFormatter"
    )]
    pub formatter: String,
    /// Kafka key deserializer class used by the default formatter.
    #[arg(long)]
    pub key_deserializer: Option<String>,
    /// Kafka value deserializer class used by the default formatter.
    #[arg(long)]
    pub value_deserializer: Option<String>,
    #[arg(long, requires = "topic", conflicts_with = "group", value_parser = clap::value_parser!(i32).range(0..))]
    pub partition: Option<i32>,
    /// Numeric offset, `earliest`, or `latest`; valid only with --partition.
    #[arg(long, requires = "partition", conflicts_with = "from_beginning")]
    pub offset: Option<String>,
    #[arg(long, conflicts_with = "offset")]
    pub from_beginning: bool,
    #[arg(long, allow_negative_numbers = true)]
    pub max_messages: Option<i32>,
    /// Exit successfully after this many milliseconds without a message.
    #[arg(
        long,
        allow_negative_numbers = true,
        value_parser = parse_consumer_timeout
    )]
    pub timeout_ms: Option<u64>,
    #[arg(long, value_parser = ["read_uncommitted", "read_committed"])]
    pub isolation_level: Option<String>,
    /// Continue after a Kafka message/poll error.
    #[arg(long)]
    pub skip_message_on_error: bool,
    #[arg(long)]
    pub json: bool,
    #[arg(long)]
    pub print_key: bool,
    #[arg(long, default_value = "\t")]
    pub key_separator: String,
    /// `DefaultMessageFormatter` property in key=value form.
    #[arg(
        long = "formatter-property",
        conflicts_with = "deprecated_formatter_properties"
    )]
    pub formatter_properties: Vec<String>,
    /// Deprecated alias for --formatter-property.
    #[arg(long = "property", conflicts_with = "formatter_properties")]
    pub deprecated_formatter_properties: Vec<String>,
    /// Java properties file used to configure the default message formatter.
    #[arg(long)]
    pub formatter_config: Option<PathBuf>,
    /// Consumer property in key=value form; overrides --command-config.
    #[arg(long = "command-property", conflicts_with = "deprecated_properties")]
    pub properties: Vec<String>,
    /// Deprecated alias for --command-property.
    #[arg(long = "consumer-property", conflicts_with = "properties")]
    pub deprecated_properties: Vec<String>,
}

impl ConsumeArgs {
    pub(crate) fn formatter_properties(&self) -> &[String] {
        if self.formatter_properties.is_empty() {
            &self.deprecated_formatter_properties
        } else {
            &self.formatter_properties
        }
    }

    pub(crate) fn properties(&self) -> &[String] {
        if self.properties.is_empty() {
            &self.deprecated_properties
        } else {
            &self.properties
        }
    }
}

#[derive(Debug, Args)]
pub struct GroupsArgs {
    /// Kafka `ConsumerGroupCommand` request/stabilization timeout in milliseconds.
    #[arg(long = "timeout", global = true)]
    pub timeout_ms: Option<u64>,
    #[command(subcommand)]
    pub action: GroupAction,
}

#[derive(Debug, Args)]
pub struct AllGroupsArgs {
    #[command(subcommand)]
    pub action: AllGroupsAction,
}

#[derive(Debug, Subcommand)]
pub enum AllGroupsAction {
    /// List Classic, Consumer, Share, and Streams groups.
    List {
        /// Filter by Kafka group type.
        #[arg(long, value_enum)]
        group_type: Option<AllGroupType>,
        /// Filter by the exact protocol type.
        #[arg(long)]
        protocol: Option<String>,
        /// Include all kinds of consumer groups.
        #[arg(long, conflicts_with_all = ["group_type", "protocol", "share", "streams"])]
        consumer: bool,
        /// Include Share groups only.
        #[arg(long, conflicts_with_all = ["group_type", "protocol", "consumer", "streams"])]
        share: bool,
        /// Include Streams groups only.
        #[arg(long, conflicts_with_all = ["group_type", "protocol", "consumer", "share"])]
        streams: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AllGroupType {
    Classic,
    Consumer,
    Share,
    Streams,
}

impl GroupsArgs {
    pub(crate) fn timeout(&self, default: Duration) -> Duration {
        self.timeout_ms.map_or(default, Duration::from_millis)
    }
}

#[derive(Debug, Subcommand)]
pub enum GroupAction {
    ValidateRegex {
        regex: String,
    },
    List {
        /// Include state and optionally filter by comma-separated group states.
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        state: Option<String>,
        /// Include type and optionally filter by comma-separated group types.
        #[arg(long = "type", num_args = 0..=1, default_missing_value = "")]
        group_type: Option<String>,
    },
    Describe {
        #[arg(
            long,
            required_unless_present = "all_groups",
            conflicts_with = "all_groups"
        )]
        group: Vec<String>,
        #[arg(long)]
        all_groups: bool,
        /// Show group members instead of committed offsets.
        #[arg(long, conflicts_with_all = ["state", "offsets"])]
        members: bool,
        /// Show group state and coordinator-level metadata.
        #[arg(long, conflicts_with_all = ["members", "offsets"])]
        state: bool,
        /// Explicitly select the default committed-offset view.
        #[arg(long, conflicts_with_all = ["members", "state"])]
        offsets: bool,
    },
    Delete {
        #[arg(
            long,
            required_unless_present = "all_groups",
            conflicts_with = "all_groups"
        )]
        group: Vec<String>,
        #[arg(long)]
        all_groups: bool,
        #[arg(long)]
        execute: bool,
    },
    DeleteOffsets {
        #[arg(long)]
        group: String,
        #[arg(long, required = true)]
        topic: Vec<String>,
        #[arg(long)]
        execute: bool,
    },
    ResetOffsets(ResetOffsetsArgs),
}

#[derive(Debug, Args)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Kafka-compatible reset target flags are intentionally mutually exclusive"
)]
pub struct ResetOffsetsArgs {
    #[arg(
        long,
        required_unless_present = "all_groups",
        conflicts_with = "all_groups"
    )]
    pub group: Vec<String>,
    #[arg(long)]
    pub all_groups: bool,
    #[arg(
        long,
        required_unless_present_any = ["all_topics", "from_file"],
        conflicts_with = "all_topics"
    )]
    pub topic: Vec<String>,
    #[arg(long, conflicts_with = "from_file")]
    pub all_topics: bool,
    #[arg(long, conflicts_with_all = ["to_latest", "to_offset", "shift_by", "to_current", "to_datetime", "by_duration", "from_file"])]
    pub to_earliest: bool,
    #[arg(long, conflicts_with_all = ["to_earliest", "to_offset", "shift_by", "to_current", "to_datetime", "by_duration", "from_file"])]
    pub to_latest: bool,
    #[arg(long, conflicts_with_all = ["to_earliest", "to_latest", "shift_by", "to_current", "to_datetime", "by_duration", "from_file"])]
    pub to_offset: Option<i64>,
    #[arg(long, conflicts_with_all = ["to_earliest", "to_latest", "to_offset", "to_current", "to_datetime", "by_duration", "from_file"])]
    pub shift_by: Option<i64>,
    /// Keep each partition at its currently committed offset.
    #[arg(long, conflicts_with_all = ["to_earliest", "to_latest", "to_offset", "shift_by", "to_datetime", "by_duration", "from_file"])]
    pub to_current: bool,
    /// Reset to offsets at an RFC 3339 or YYYY-MM-DDTHH:MM:SS.sss UTC datetime.
    #[arg(long, conflicts_with_all = ["to_earliest", "to_latest", "to_offset", "shift_by", "to_current", "by_duration", "from_file"])]
    pub to_datetime: Option<String>,
    /// Reset by an ISO-8601 duration before now, for example PT1H30M.
    #[arg(long, conflicts_with_all = ["to_earliest", "to_latest", "to_offset", "shift_by", "to_current", "to_datetime", "from_file"])]
    pub by_duration: Option<String>,
    /// Import Kafka's headerless topic,partition,offset or group,topic,partition,offset CSV.
    #[arg(long, conflicts_with_all = ["topic", "all_topics"])]
    pub from_file: Option<PathBuf>,
    /// Export the planned offsets using Kafka's headerless CSV format.
    #[arg(long)]
    pub export: bool,
    #[arg(long, conflicts_with = "dry_run")]
    pub execute: bool,
    /// Preview the reset plan without changing committed offsets (the default).
    #[arg(long, conflicts_with = "execute")]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct ConfigsArgs {
    #[command(subcommand)]
    pub action: ConfigAction,
}

#[derive(Debug, Args, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Kafka ConfigCommand exposes four independent default-entity selector flags"
)]
pub struct ConfigEntityArgs {
    #[arg(long, value_enum)]
    pub entity_type: Vec<ConfigEntityType>,
    #[arg(long)]
    pub entity_name: Vec<String>,
    #[arg(long, conflicts_with = "entity_name")]
    pub entity_default: bool,
    #[arg(long)]
    pub topic: Option<String>,
    #[arg(long)]
    pub client: Option<String>,
    #[arg(long)]
    pub user: Option<String>,
    #[arg(long)]
    pub broker: Option<String>,
    #[arg(long)]
    pub broker_logger: Option<String>,
    #[arg(long)]
    pub ip: Option<String>,
    #[arg(long)]
    pub client_metrics: Option<String>,
    #[arg(long)]
    pub group: Option<String>,
    #[arg(long)]
    pub client_defaults: bool,
    #[arg(long)]
    pub user_defaults: bool,
    #[arg(long)]
    pub broker_defaults: bool,
    #[arg(long)]
    pub ip_defaults: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub enum ConfigEntityType {
    #[value(name = "topics", alias = "topic")]
    Topic,
    #[value(name = "brokers", alias = "broker")]
    Broker,
    #[value(name = "groups", alias = "group")]
    Group,
    #[value(name = "users", alias = "user")]
    User,
    #[value(name = "clients", alias = "client")]
    Client,
    #[value(name = "ips", alias = "ip")]
    Ip,
    #[value(name = "broker-loggers", alias = "broker-logger")]
    BrokerLogger,
    #[value(name = "client-metrics", alias = "client-metric")]
    ClientMetrics,
}

#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    Describe {
        #[command(flatten)]
        entity: ConfigEntityArgs,
        /// Include inherited/static/default configurations, not only dynamic overrides.
        #[arg(long)]
        all: bool,
    },
    Alter {
        #[command(flatten)]
        entity: ConfigEntityArgs,
        #[arg(long = "add-config")]
        add: Vec<String>,
        #[arg(long = "add-config-file", conflicts_with = "add")]
        add_file: Option<PathBuf>,
        #[arg(long = "delete-config", value_delimiter = ',')]
        delete: Vec<String>,
        #[arg(long)]
        execute: bool,
    },
}

#[derive(Debug, Args)]
pub struct OffsetsArgs {
    #[arg(long)]
    pub topic: Option<String>,
    /// Kafka-compatible topic/partition patterns such as events:0-3,audit:1.
    #[arg(long, conflicts_with_all = ["topic", "partitions"])]
    pub topic_partitions: Option<String>,
    #[arg(long, conflicts_with = "topic_partitions")]
    pub partitions: Option<String>,
    #[arg(long)]
    pub exclude_internal_topics: bool,
    #[arg(
        long,
        value_enum,
        default_value_t,
        conflicts_with = "timestamp",
        allow_hyphen_values = true
    )]
    pub time: OffsetTime,
    /// Return the first offset whose record timestamp is at least this Unix millisecond value.
    #[arg(long)]
    pub timestamp: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum OffsetTime {
    #[value(alias = "-2")]
    Earliest,
    #[default]
    #[value(alias = "-1")]
    Latest,
    #[value(name = "max-timestamp", alias = "-3")]
    MaxTimestamp,
    #[value(name = "earliest-local", alias = "-4")]
    EarliestLocal,
    #[value(name = "latest-tiered", alias = "-5")]
    LatestTiered,
    #[value(name = "earliest-pending-upload", alias = "-6")]
    EarliestPendingUpload,
}

#[derive(Debug, Args)]
pub struct AclsArgs {
    #[command(subcommand)]
    pub action: AclAction,
}

#[derive(Debug, Subcommand)]
pub enum AclAction {
    List(AclFilterArgs),
    Add(AclMutationArgs),
    Remove(AclMutationArgs),
}

#[derive(Debug, Args)]
pub struct AclFilterArgs {
    #[arg(long)]
    pub topic: Vec<String>,
    #[arg(long)]
    pub group: Vec<String>,
    #[arg(long)]
    pub cluster: bool,
    #[arg(long)]
    pub transactional_id: Vec<String>,
    #[arg(long)]
    pub delegation_token: Vec<String>,
    #[arg(long)]
    pub principal: Vec<String>,
    #[arg(long, value_enum, default_value_t)]
    pub resource_pattern_type: AclResourcePattern,
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum AclResourcePattern {
    Any,
    #[default]
    Literal,
    Match,
    Prefixed,
}

#[derive(Debug, Args)]
#[allow(clippy::struct_excessive_bools)]
pub struct AclMutationArgs {
    #[command(flatten)]
    pub filter: AclFilterArgs,
    #[arg(long, conflicts_with_all = ["producer", "consumer"])]
    pub operation: Vec<String>,
    /// Infer producer ACL operations for --topic and optional --transactional-id.
    #[arg(long)]
    pub producer: bool,
    /// Infer consumer ACL operations for --topic and --group.
    #[arg(long)]
    pub consumer: bool,
    /// Add the cluster `IdempotentWrite` ACL to the producer role.
    #[arg(long, requires = "producer")]
    pub idempotent: bool,
    #[arg(long)]
    pub host: Option<String>,
    #[arg(long = "allow-principal")]
    pub allow_principal: Vec<String>,
    #[arg(long = "deny-principal")]
    pub deny_principal: Vec<String>,
    #[arg(long = "allow-host")]
    pub allow_host: Vec<String>,
    #[arg(long = "deny-host")]
    pub deny_host: Vec<String>,
    #[arg(long, visible_alias = "force")]
    pub execute: bool,
}

#[derive(Debug, Args)]
pub struct ReassignArgs {
    #[command(subcommand)]
    pub action: ReassignAction,
}

#[derive(Debug, Subcommand)]
pub enum ReassignAction {
    Generate {
        #[arg(long)]
        topics_to_move_json_file: PathBuf,
        #[arg(long)]
        broker_list: String,
        #[arg(long)]
        disable_rack_aware: bool,
    },
    Execute {
        #[arg(long)]
        reassignment_json_file: PathBuf,
        /// Allow starting this plan while another reassignment is active.
        #[arg(long)]
        additional: bool,
        /// Reject any partition whose target replication factor differs from its current value.
        #[arg(long)]
        disallow_replication_factor_change: bool,
        /// Inter-broker replication throttle in bytes per second.
        #[arg(long)]
        throttle: Option<u64>,
        /// Replica log-directory movement throttle in bytes per second.
        #[arg(long)]
        replica_alter_log_dirs_throttle: Option<u64>,
        #[arg(long)]
        execute: bool,
    },
    Verify {
        #[arg(long)]
        reassignment_json_file: PathBuf,
        #[arg(long)]
        preserve_throttles: bool,
    },
    Cancel {
        #[arg(long)]
        reassignment_json_file: PathBuf,
        #[arg(long)]
        preserve_throttles: bool,
        #[arg(long)]
        execute: bool,
    },
    List,
}

#[derive(Debug, Args)]
pub struct DeleteRecordsArgs {
    #[arg(long)]
    pub offset_json_file: PathBuf,
    #[arg(long)]
    pub execute: bool,
}

#[derive(Debug, Args)]
pub struct LeaderElectionArgs {
    #[arg(long, value_enum)]
    pub election_type: ElectionType,
    #[arg(long, conflicts_with_all = ["all_topic_partitions", "path_to_json_file"])]
    pub topic: Option<String>,
    #[arg(long)]
    pub partition: Option<i32>,
    #[arg(long, conflicts_with_all = ["topic", "partition", "path_to_json_file"])]
    pub all_topic_partitions: bool,
    #[arg(long, conflicts_with_all = ["topic", "partition", "all_topic_partitions"])]
    pub path_to_json_file: Option<PathBuf>,
    #[arg(long)]
    pub execute: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ElectionType {
    Preferred,
    Unclean,
}

#[derive(Debug, Args)]
pub struct LogDirsArgs {
    /// Kafka-compatible action flag; log-dirs has only one action.
    #[arg(long)]
    pub describe: bool,
    #[arg(long)]
    pub broker_list: Option<String>,
    #[arg(long)]
    pub topic_list: Option<String>,
}

#[derive(Debug, Args)]
pub struct ApiVersionsArgs {
    #[arg(long)]
    pub broker: Option<i32>,
}

#[derive(Debug, Args)]
pub struct ClusterArgs {
    #[command(subcommand)]
    pub action: ClusterAction,
}

#[derive(Debug, Subcommand)]
pub enum ClusterAction {
    #[command(name = "cluster-id", visible_alias = "id")]
    Id,
    ListEndpoints {
        /// Include fenced brokers in the endpoint listing (Kafka 4.1+).
        #[arg(long)]
        include_fenced_brokers: bool,
    },
    ApiVersions,
    Unregister {
        #[arg(short = 'i', long)]
        id: i32,
        #[arg(long)]
        execute: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! parses_command_family {
        ($name:ident, $($argument:literal),+ $(,)?) => {
            #[test]
            fn $name() {
                let result = Cli::try_parse_from([
                    "kafka",
                    "--bootstrap-server",
                    "localhost:9092",
                    $($argument),+
                ]);
                assert!(result.is_ok(), "parse failed: {result:?}");
            }
        };
    }

    #[test]
    fn cli_should_parse_topic_list() {
        let result = Cli::try_parse_from([
            "kafka",
            "topics",
            "list",
            "--bootstrap-server",
            "localhost:9092",
        ]);
        assert!(result.is_ok(), "parse failed: {result:?}");
    }

    #[test]
    fn compatibility_command_should_accept_sh_suffix() {
        assert_eq!(compatibility_command("kafka-topics.sh"), Some("topics"));
        assert_eq!(compatibility_command("kafka-groups.sh"), Some("all-groups"));
        assert_eq!(
            compatibility_command("kafka-client-metrics.sh"),
            Some("client-metrics")
        );
        assert_eq!(compatibility_command("kafka-features.sh"), Some("features"));
    }

    #[test]
    fn legacy_mutations_should_preserve_immediate_execution() {
        for (command, flag, action) in [
            ("groups", "--delete", "delete"),
            ("groups", "--delete-offsets", "delete-offsets"),
            ("configs", "--alter", "alter"),
            ("acls", "--add", "add"),
            ("reassign", "--execute", "execute"),
            ("reassign", "--cancel", "cancel"),
            ("client-metrics", "--alter", "alter"),
            ("client-metrics", "--delete", "delete"),
        ] {
            let mut arguments = vec![
                OsString::from("kafka-compatible"),
                OsString::from(command),
                OsString::from(flag),
            ];
            rewrite_legacy_action(&mut arguments, command);
            assert_eq!(arguments[2], action, "wrong action for {flag}");
            assert_eq!(arguments[3], "--execute", "{flag} became a preview");
        }
    }

    #[test]
    fn legacy_configs_should_rewrite_mixed_generic_defaults() {
        let mut arguments = vec![
            OsString::from("kafka-configs.sh"),
            OsString::from("configs"),
            OsString::from("--bootstrap-server"),
            OsString::from("localhost:9092"),
            OsString::from("--alter"),
            OsString::from("--entity-type"),
            OsString::from("users"),
            OsString::from("--entity-default"),
            OsString::from("--entity-type"),
            OsString::from("clients"),
            OsString::from("--entity-name"),
            OsString::from("billing"),
            OsString::from("--add-config"),
            OsString::from("request_percentage=25"),
        ];
        rewrite_legacy_action(&mut arguments, "configs");

        let cli = Cli::try_parse_from(arguments).expect("rewritten mixed quota command");
        let Command::Configs(ConfigsArgs {
            action: ConfigAction::Alter { entity, .. },
        }) = cli.command
        else {
            panic!("expected configs alter command");
        };
        assert!(entity.user_defaults && entity.client.as_deref() == Some("billing"));
    }

    #[test]
    fn legacy_read_actions_should_not_gain_execution() {
        let mut arguments = vec![
            OsString::from("kafka-topics.sh"),
            OsString::from("topics"),
            OsString::from("--describe"),
        ];
        rewrite_legacy_action(&mut arguments, "topics");
        assert_eq!(arguments[2], "describe");
        assert!(!arguments.iter().any(|argument| argument == "--execute"));
    }

    #[test]
    fn single_action_legacy_mutations_should_execute() {
        for command in ["delete-records", "leader-election"] {
            let mut arguments = vec![OsString::from("kafka-compatible"), OsString::from(command)];
            rewrite_legacy_action(&mut arguments, command);
            assert_eq!(arguments[2], "--execute", "{command} became a preview");
        }

        let mut cluster = vec![
            OsString::from("kafka-cluster.sh"),
            OsString::from("cluster"),
            OsString::from("--bootstrap-server"),
            OsString::from("localhost:9092"),
            OsString::from("unregister"),
            OsString::from("--id"),
            OsString::from("1"),
        ];
        rewrite_legacy_action(&mut cluster, "cluster");
        let unregister = cluster
            .iter()
            .position(|argument| argument == "unregister")
            .expect("unregister argument");
        assert_eq!(cluster[unregister + 1], "--execute");
    }

    #[test]
    fn deprecated_config_options_should_map_to_command_config() {
        for (command, deprecated, expected) in [
            ("produce", "--producer.config", "--command-config"),
            ("consume", "--consumer.config", "--command-config"),
            ("leader-election", "--admin.config", "--command-config"),
            (
                "cluster",
                "--config=client.properties",
                "--command-config=client.properties",
            ),
        ] {
            let mut arguments = vec![
                OsString::from("kafka-compatible"),
                OsString::from(command),
                OsString::from(deprecated),
                OsString::from("client.properties"),
            ];
            rewrite_legacy_action(&mut arguments, command);
            assert!(arguments.iter().any(|argument| argument == expected));
            assert!(!arguments.iter().any(|argument| argument == deprecated));
        }
    }

    #[test]
    fn console_should_reject_deprecated_and_current_config_together() {
        for (command, deprecated) in [
            ("produce", "--producer.config"),
            ("consume", "--consumer.config"),
        ] {
            let mut arguments = vec![
                OsString::from("kafka-compatible"),
                OsString::from(command),
                OsString::from("--topic"),
                OsString::from("events"),
                OsString::from("--command-config"),
                OsString::from("current.properties"),
                OsString::from(deprecated),
                OsString::from("deprecated.properties"),
            ];
            rewrite_legacy_action(&mut arguments, command);

            assert!(
                Cli::try_parse_from(arguments).is_err(),
                "accepted conflicting config options for {command}"
            );
        }
    }

    #[test]
    fn compression_codec_without_value_should_default_to_gzip() {
        let cli = Cli::try_parse_from([
            "kafka",
            "--bootstrap-server",
            "localhost:9092",
            "produce",
            "--topic",
            "events",
            "--compression-codec",
            "--sync",
        ])
        .expect("producer compression codec");
        let Command::Produce(args) = cli.command else {
            panic!("expected produce command");
        };
        assert_eq!(args.compression_type, "gzip");
    }

    #[test]
    fn producer_should_reject_deprecated_and_current_properties_together() {
        for pair in [
            ["--command-property", "--producer-property"],
            ["--reader-property", "--property"],
        ] {
            let result = Cli::try_parse_from([
                "kafka",
                "--bootstrap-server",
                "localhost:9092",
                "produce",
                "--topic",
                "events",
                pair[0],
                "first=value",
                pair[1],
                "second=value",
            ]);

            assert!(result.is_err(), "accepted conflicting options {pair:?}");
        }
    }

    #[test]
    fn consumer_should_reject_deprecated_and_current_properties_together() {
        for pair in [
            ["--command-property", "--consumer-property"],
            ["--formatter-property", "--property"],
        ] {
            let result = Cli::try_parse_from([
                "kafka",
                "--bootstrap-server",
                "localhost:9092",
                "consume",
                "--topic",
                "events",
                pair[0],
                "first=value",
                pair[1],
                "second=value",
            ]);

            assert!(result.is_err(), "accepted conflicting options {pair:?}");
        }
    }

    #[test]
    fn consumer_should_accept_kafka_unbounded_numeric_sentinels() {
        let cli = Cli::try_parse_from([
            "kafka",
            "consume",
            "--topic",
            "events",
            "--max-messages",
            "-1",
            "--timeout-ms",
            "-1",
        ])
        .expect("Kafka unbounded consumer sentinels");
        let Command::Consume(consumer) = cli.command else {
            panic!("expected consume command");
        };

        assert_eq!(
            (consumer.max_messages, consumer.timeout_ms),
            (Some(-1), Some(u64::MAX))
        );
    }

    #[test]
    fn consumer_should_have_no_idle_timeout_by_default() {
        let cli = Cli::try_parse_from(["kafka", "consume", "--topic", "events"])
            .expect("consumer defaults");
        let Command::Consume(consumer) = cli.command else {
            panic!("expected consume command");
        };

        assert_eq!(consumer.timeout_ms, None);
    }

    #[test]
    fn deprecated_console_properties_should_keep_their_values() {
        let producer = Cli::try_parse_from([
            "kafka",
            "produce",
            "--topic",
            "events",
            "--producer-property",
            "acks=1",
            "--property",
            "parse.key=true",
        ])
        .expect("deprecated producer properties");
        let Command::Produce(producer) = producer.command else {
            panic!("expected produce command");
        };
        assert_eq!(producer.properties(), &["acks=1"]);
        assert_eq!(producer.reader_properties(), &["parse.key=true"]);

        let consumer = Cli::try_parse_from([
            "kafka",
            "consume",
            "--topic",
            "events",
            "--consumer-property",
            "fetch.min.bytes=2",
            "--property",
            "print.key=true",
        ])
        .expect("deprecated consumer properties");
        let Command::Consume(consumer) = consumer.command else {
            panic!("expected consume command");
        };
        assert_eq!(consumer.properties(), &["fetch.min.bytes=2"]);
        assert_eq!(consumer.formatter_properties(), &["print.key=true"]);
    }

    #[test]
    fn consumer_offset_should_require_manual_partition() {
        let missing_partition = Cli::try_parse_from([
            "kafka",
            "--bootstrap-server",
            "localhost:9092",
            "consume",
            "--topic",
            "events",
            "--offset",
            "earliest",
        ]);
        assert!(missing_partition.is_err());

        let conflicting_group = Cli::try_parse_from([
            "kafka",
            "--bootstrap-server",
            "localhost:9092",
            "consume",
            "--topic",
            "events",
            "--partition",
            "0",
            "--group",
            "orders",
        ]);
        assert!(conflicting_group.is_err());
    }

    #[test]
    fn topic_create_should_use_broker_defaults_when_counts_are_omitted() {
        let cli = Cli::try_parse_from(["kafka", "topics", "create", "--topic", "broker-defaults"])
            .expect("topic create defaults");
        let Command::Topics(TopicsArgs {
            action: TopicAction::Create(create),
        }) = cli.command
        else {
            panic!("expected topics create command");
        };

        assert_eq!((create.partitions, create.replication_factor), (None, None));
    }

    #[test]
    fn topic_create_should_reject_counts_with_manual_assignment() {
        for option in ["--partitions", "--replication-factor"] {
            let result = Cli::try_parse_from([
                "kafka",
                "topics",
                "create",
                "--topic",
                "manual",
                "--replica-assignment",
                "1",
                option,
                "1",
            ]);

            assert!(result.is_err(), "accepted {option} with manual assignment");
        }
    }

    #[test]
    fn topic_actions_should_reject_describe_only_options() {
        for arguments in [
            vec!["list", "--under-replicated-partitions"],
            vec!["delete", "--topic", "events", "--exclude-internal"],
        ] {
            let mut command = vec!["kafka", "topics"];
            command.extend(arguments);

            assert!(Cli::try_parse_from(command).is_err());
        }
    }

    #[test]
    fn topic_describe_should_accept_original_selection_options() {
        let result = Cli::try_parse_from([
            "kafka",
            "topics",
            "describe",
            "--topic",
            "events",
            "--topic-id",
            "AAAAAAAAAAAAAAAAAAAAAA",
            "--if-exists",
            "--partition-size-limit-per-response",
            "500",
        ]);

        assert!(result.is_ok(), "describe options failed: {result:?}");
    }

    parses_command_family!(produce_family_parses, "produce", "--topic", "events");
    parses_command_family!(consume_family_parses, "consume", "--topic", "events");
    parses_command_family!(
        consume_include_parses,
        "consume",
        "--include",
        "events-.*",
        "--timeout-ms",
        "1000",
        "--isolation-level",
        "read_committed",
        "--skip-message-on-error"
    );
    parses_command_family!(groups_family_parses, "groups", "list");
    parses_command_family!(
        all_groups_family_parses,
        "all-groups",
        "list",
        "--group-type",
        "share",
        "--protocol",
        "share"
    );

    #[test]
    fn all_groups_named_filters_should_be_mutually_exclusive() {
        for filters in [
            ["--consumer", "--share"],
            ["--consumer", "--streams"],
            ["--share", "--streams"],
        ] {
            let result = Cli::try_parse_from([
                "kafka",
                "--bootstrap-server",
                "localhost:9092",
                "all-groups",
                "list",
                filters[0],
                filters[1],
            ]);

            assert!(result.is_err(), "accepted conflicting filters: {filters:?}");
        }
    }

    #[test]
    fn all_groups_should_reject_unknown_group_type() {
        let result = Cli::try_parse_from([
            "kafka",
            "--bootstrap-server",
            "localhost:9092",
            "all-groups",
            "list",
            "--group-type",
            "unknown",
        ]);

        assert!(result.is_err(), "accepted unknown group type");
    }
    parses_command_family!(
        groups_original_timeout_parses,
        "groups",
        "list",
        "--timeout",
        "5000"
    );
    parses_command_family!(
        groups_validate_regex_parses,
        "groups",
        "validate-regex",
        "orders-.*"
    );
    parses_command_family!(
        groups_reset_from_file_parses,
        "groups",
        "reset-offsets",
        "--group",
        "payments",
        "--from-file",
        "reset.csv",
        "--export"
    );
    parses_command_family!(
        configs_family_parses,
        "configs",
        "describe",
        "--entity-type",
        "topic",
        "--entity-name",
        "events"
    );
    parses_command_family!(
        configs_add_file_parses,
        "configs",
        "alter",
        "--entity-type",
        "topics",
        "--entity-name",
        "events",
        "--add-config-file",
        "topic.properties"
    );
    parses_command_family!(
        configs_all_entities_parses,
        "configs",
        "describe",
        "--entity-type",
        "topics",
        "--all"
    );
    parses_command_family!(offsets_family_parses, "offsets", "--topic", "events");
    #[test]
    fn configs_delete_should_split_kafka_comma_separated_keys() {
        let cli = Cli::try_parse_from([
            "kafka",
            "configs",
            "alter",
            "--entity-type",
            "topics",
            "--entity-name",
            "events",
            "--delete-config",
            "retention.ms,cleanup.policy",
        ])
        .expect("comma-separated delete configs");
        let Command::Configs(ConfigsArgs {
            action: ConfigAction::Alter { delete, .. },
        }) = cli.command
        else {
            panic!("expected configs alter command");
        };

        assert_eq!(delete, ["retention.ms", "cleanup.policy"]);
    }
    #[test]
    fn offsets_should_parse_kafka_tiered_time_aliases() {
        for value in [
            "earliest-local",
            "-4",
            "latest-tiered",
            "-5",
            "earliest-pending-upload",
            "-6",
        ] {
            let result =
                Cli::try_parse_from(["kafka", "offsets", "--topic", "events", "--time", value]);

            assert!(result.is_ok(), "rejected --time {value}: {result:?}");
        }
    }
    parses_command_family!(
        offsets_max_timestamp_parses,
        "offsets",
        "--topic",
        "events",
        "--time",
        "max-timestamp"
    );
    parses_command_family!(
        offsets_numeric_time_alias_parses,
        "offsets",
        "--topic",
        "events",
        "--time",
        "-3"
    );
    parses_command_family!(acls_family_parses, "acls", "list");
    parses_command_family!(acls_force_alias_parses, "acls", "remove", "--force");
    parses_command_family!(reassign_family_parses, "reassign", "list");
    parses_command_family!(
        configs_user_client_quota_parses,
        "configs",
        "alter",
        "--entity-type",
        "users",
        "--entity-type",
        "clients",
        "--entity-name",
        "alice",
        "--entity-name",
        "billing",
        "--add-config",
        "request_percentage=25"
    );
    parses_command_family!(
        configs_specific_mixed_default_quota_parses,
        "configs",
        "alter",
        "--user-defaults",
        "--client",
        "billing",
        "--add-config",
        "request_percentage=25"
    );
    parses_command_family!(
        delete_records_family_parses,
        "delete-records",
        "--offset-json-file",
        "offsets.json"
    );
    parses_command_family!(
        leader_election_family_parses,
        "leader-election",
        "--election-type",
        "preferred",
        "--all-topic-partitions"
    );
    parses_command_family!(log_dirs_family_parses, "log-dirs");
    parses_command_family!(api_versions_family_parses, "api-versions");
    parses_command_family!(cluster_family_parses, "cluster", "cluster-id");
    parses_command_family!(client_metrics_family_parses, "client-metrics", "list");
    parses_command_family!(features_family_parses, "features", "describe");
    parses_command_family!(transactions_list_family_parses, "transactions", "list");
    parses_command_family!(
        delegation_tokens_create_family_parses,
        "delegation-tokens",
        "create",
        "--max-life-time-period",
        "-1",
        "--renewer-principal",
        "User:alice"
    );
    parses_command_family!(
        delegation_tokens_renew_family_parses,
        "delegation-tokens",
        "renew",
        "--hmac",
        "AA==",
        "--renew-time-period",
        "-1"
    );
    parses_command_family!(
        delegation_tokens_expire_family_parses,
        "delegation-tokens",
        "expire",
        "--hmac",
        "AA==",
        "--expiry-time-period",
        "-1"
    );
    parses_command_family!(
        delegation_tokens_describe_family_parses,
        "delegation-tokens",
        "describe",
        "--owner-principal",
        "User:alice"
    );
    parses_command_family!(
        metadata_quorum_status_family_parses,
        "metadata-quorum",
        "describe",
        "--status"
    );
    parses_command_family!(
        metadata_quorum_replication_family_parses,
        "metadata-quorum",
        "describe",
        "--replication",
        "--human-readable"
    );
    parses_command_family!(
        metadata_quorum_add_family_parses,
        "metadata-quorum",
        "add-controller",
        "--dry-run"
    );
    parses_command_family!(
        metadata_quorum_remove_family_parses,
        "metadata-quorum",
        "remove-controller",
        "-i",
        "2",
        "-d",
        "AAAAAAAAAAAAAAAAAAAAAA",
        "--dry-run"
    );
    parses_command_family!(
        transactions_describe_family_parses,
        "transactions",
        "describe",
        "--transactional-id",
        "orders"
    );
    parses_command_family!(
        transactions_describe_producers_family_parses,
        "transactions",
        "describe-producers",
        "--broker-id",
        "1",
        "--topic",
        "orders",
        "--partition",
        "0"
    );
    parses_command_family!(
        transactions_abort_family_parses,
        "transactions",
        "abort",
        "--topic",
        "orders",
        "--partition",
        "0",
        "--start-offset",
        "1"
    );
    parses_command_family!(
        transactions_find_hanging_family_parses,
        "transactions",
        "find-hanging",
        "--broker-id",
        "1"
    );
    parses_command_family!(
        transactions_force_terminate_family_parses,
        "transactions",
        "forceTerminateTransaction",
        "--transactionalId",
        "orders"
    );
    parses_command_family!(
        features_upgrade_family_parses,
        "features",
        "upgrade",
        "--release-version",
        "4.3-IV0",
        "--dry-run"
    );
    parses_command_family!(
        features_downgrade_family_parses,
        "features",
        "downgrade",
        "--metadata",
        "4.2-IV1",
        "--unsafe"
    );
    parses_command_family!(
        features_disable_family_parses,
        "features",
        "disable",
        "--feature",
        "group.version"
    );
    parses_command_family!(
        features_version_mapping_family_parses,
        "features",
        "version-mapping",
        "--release-version",
        "4.3-IV0"
    );
    parses_command_family!(
        features_dependencies_family_parses,
        "features",
        "feature-dependencies",
        "--feature",
        "eligible.leader.replicas.version=1"
    );

    #[test]
    fn client_metrics_should_collect_repeated_match_and_metrics_values() {
        let cli = Cli::try_parse_from([
            "kafka",
            "--bootstrap-server",
            "localhost:9092",
            "client-metrics",
            "alter",
            "--name",
            "metrics",
            "--match",
            "client_id=a",
            "--match",
            "client_software_name=b",
            "--metrics",
            "org.apache.kafka.producer.",
            "--metrics",
            "org.apache.kafka.consumer.",
        ])
        .expect("repeated client metrics options");
        let Command::ClientMetrics(ClientMetricsArgs {
            action: ClientMetricsAction::Alter {
                r#match, metrics, ..
            },
        }) = cli.command
        else {
            panic!("expected client metrics alter command");
        };

        assert_eq!(r#match.len(), 2);
        assert_eq!(metrics.len(), 2);
    }

    #[test]
    fn cluster_short_options_parse() {
        let result = Cli::try_parse_from([
            "kafka",
            "-b",
            "localhost:9092",
            "-c",
            "client.properties",
            "cluster",
            "unregister",
            "-i",
            "1",
        ]);
        assert!(result.is_ok(), "parse failed: {result:?}");
    }

    parses_command_family!(
        cluster_fenced_endpoints_parses,
        "cluster",
        "list-endpoints",
        "--include-fenced-brokers"
    );
}
