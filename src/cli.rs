//! Command-line interface definitions and Kafka script compatibility dispatch.

use std::{ffi::OsString, path::PathBuf, time::Duration};

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::output::OutputFormat;

/// Native Kafka command-line client.
#[derive(Debug, Parser)]
#[command(name = "kafka", version, about, propagate_version = true)]
pub struct Cli {
    /// Comma-separated Kafka bootstrap brokers.
    #[arg(long, global = true, env = "KAFKA_CLI_BOOTSTRAP_SERVER")]
    pub bootstrap_server: Option<String>,

    /// Kafka Java-compatible client properties file.
    #[arg(long, global = true, env = "KAFKA_CLI_COMMAND_CONFIG")]
    pub command_config: Option<PathBuf>,

    /// Request timeout in milliseconds.
    #[arg(long, global = true, default_value_t = 30_000)]
    pub timeout_ms: u64,

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
        Duration::from_millis(self.timeout_ms)
    }
}

fn compatibility_command(executable: &str) -> Option<&'static str> {
    let name = executable.strip_suffix(".sh").unwrap_or(executable);
    match name {
        "kafka-topics" => Some("topics"),
        "kafka-console-producer" => Some("produce"),
        "kafka-console-consumer" => Some("consume"),
        "kafka-consumer-groups" => Some("groups"),
        "kafka-configs" => Some("configs"),
        "kafka-get-offsets" => Some("offsets"),
        "kafka-acls" => Some("acls"),
        "kafka-reassign-partitions" => Some("reassign"),
        "kafka-delete-records" => Some("delete-records"),
        "kafka-leader-election" => Some("leader-election"),
        "kafka-log-dirs" => Some("log-dirs"),
        "kafka-broker-api-versions" => Some("api-versions"),
        "kafka-cluster" => Some("cluster"),
        _ => None,
    }
}

fn rewrite_legacy_action(args: &mut Vec<OsString>, command: &str) {
    let candidates: &[(&str, &str)] = match command {
        "topics" => &[
            ("--create", "create"),
            ("--delete", "delete"),
            ("--describe", "describe"),
            ("--alter", "alter"),
            ("--list", "list"),
        ],
        "groups" => &[
            ("--validate-regex", "validate-regex"),
            ("--describe", "describe"),
            ("--delete-offsets", "delete-offsets"),
            ("--reset-offsets", "reset-offsets"),
            ("--delete", "delete"),
            ("--list", "list"),
        ],
        "configs" => &[("--describe", "describe"), ("--alter", "alter")],
        "acls" => &[("--add", "add"), ("--remove", "remove"), ("--list", "list")],
        "reassign" => &[
            ("--generate", "generate"),
            ("--execute", "execute"),
            ("--verify", "verify"),
            ("--cancel", "cancel"),
            ("--list", "list"),
        ],
        _ => &[],
    };
    for (flag, action) in candidates {
        if let Some(index) = args.iter().position(|arg| arg == flag) {
            args.remove(index);
            args.insert(2, OsString::from(action));
            return;
        }
    }
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
}

#[derive(Debug, Args)]
pub struct TopicsArgs {
    #[command(subcommand)]
    pub action: TopicAction,
}

#[derive(Debug, Subcommand)]
pub enum TopicAction {
    List(TopicSelector),
    Describe(TopicSelector),
    Create(CreateTopicArgs),
    Alter(AlterTopicArgs),
    Delete(DeleteTopicArgs),
}

#[derive(Debug, Args)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Kafka exposes these mutually exclusive partition report modes as flags"
)]
pub struct TopicSelector {
    /// Topic name or regular expression. Omit to select all topics.
    #[arg(long, conflicts_with = "topic_id")]
    pub topic: Option<String>,
    /// Kafka topic UUID. Supported by the describe action.
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
}

#[derive(Debug, Args)]
pub struct DeleteTopicArgs {
    #[command(flatten)]
    pub selector: TopicSelector,
    /// Succeed when no topic matches the expression.
    #[arg(long)]
    pub if_exists: bool,
}

#[derive(Debug, Args)]
pub struct CreateTopicArgs {
    #[arg(long)]
    pub topic: String,
    #[arg(long, default_value_t = 1)]
    pub partitions: i32,
    /// Replication factor. Omit to use the broker's default.
    #[arg(long)]
    pub replication_factor: Option<i32>,
    /// Manual assignments such as 1:2,2:3 (one comma-separated entry per partition).
    #[arg(long, conflicts_with = "replication_factor")]
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
    #[arg(long)]
    pub key_separator: Option<char>,
    #[arg(long)]
    pub parse_key: bool,
    #[arg(long, visible_alias = "compression-codec", default_value = "none")]
    pub compression_type: String,
    #[arg(long, visible_alias = "request-required-acks", default_value = "all")]
    pub acks: String,
    /// Wait for each delivery before reading and sending the next record.
    #[arg(long)]
    pub sync: bool,
    #[arg(long)]
    pub batch_size: Option<usize>,
    #[arg(long)]
    pub message_send_max_retries: Option<u32>,
    #[arg(long)]
    pub retry_backoff_ms: Option<u64>,
    /// Maximum batching delay; maps to librdkafka linger.ms.
    #[arg(long = "timeout")]
    pub linger_ms: Option<u64>,
    #[arg(long)]
    pub request_timeout_ms: Option<u64>,
    #[arg(long)]
    pub metadata_expiry_ms: Option<u64>,
    /// Maximum buffered producer memory in bytes.
    #[arg(long)]
    pub max_memory_bytes: Option<usize>,
    #[arg(long)]
    pub socket_buffer_size: Option<i32>,
    /// Parse each input line as {"key":...,"value":...,"partition":...,"headers":{...}}.
    #[arg(long)]
    pub json: bool,
    /// Default `LineMessageReader` property in key=value form.
    #[arg(long = "property")]
    pub reader_properties: Vec<String>,
    /// Producer property in key=value form; overrides --command-config.
    #[arg(long = "command-property", visible_alias = "producer-property")]
    pub properties: Vec<String>,
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
    #[arg(long)]
    pub partition: Option<i32>,
    #[arg(long)]
    pub offset: Option<i64>,
    #[arg(long)]
    pub from_beginning: bool,
    #[arg(long)]
    pub max_messages: Option<u64>,
    /// Exit successfully after this many milliseconds without a message.
    #[arg(long)]
    pub timeout_ms: Option<u64>,
    #[arg(long, value_parser = ["read_uncommitted", "read_committed"], default_value = "read_uncommitted")]
    pub isolation_level: String,
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
    #[arg(long = "property", visible_alias = "formatter-property")]
    pub formatter_properties: Vec<String>,
    /// Consumer property in key=value form; overrides --command-config.
    #[arg(long = "command-property", visible_alias = "consumer-property")]
    pub properties: Vec<String>,
}

#[derive(Debug, Args)]
pub struct GroupsArgs {
    #[command(subcommand)]
    pub action: GroupAction,
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

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ConfigEntityType {
    #[value(name = "topics", alias = "topic")]
    Topic,
    #[value(name = "brokers", alias = "broker")]
    Broker,
    #[value(name = "groups", alias = "group")]
    Group,
    #[value(name = "users", alias = "user")]
    User,
}

#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    Describe {
        #[arg(long, value_enum)]
        entity_type: ConfigEntityType,
        #[arg(long)]
        entity_name: Option<String>,
        /// Include inherited/static/default configurations, not only dynamic overrides.
        #[arg(long)]
        all: bool,
    },
    Alter {
        #[arg(long, value_enum)]
        entity_type: ConfigEntityType,
        #[arg(long)]
        entity_name: String,
        #[arg(long = "add-config")]
        add: Vec<String>,
        #[arg(long = "add-config-file", conflicts_with = "add")]
        add_file: Option<PathBuf>,
        #[arg(long = "delete-config")]
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
    pub topic: Option<String>,
    #[arg(long)]
    pub group: Option<String>,
    #[arg(long)]
    pub cluster: bool,
    #[arg(long)]
    pub transactional_id: Option<String>,
    #[arg(long)]
    pub delegation_token: Option<String>,
    #[arg(long)]
    pub principal: Option<String>,
    #[arg(long, value_enum, default_value_t)]
    pub resource_pattern_type: AclResourcePattern,
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum AclResourcePattern {
    Any,
    #[default]
    Literal,
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
    #[arg(long)]
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
        #[arg(long)]
        execute: bool,
    },
    Verify {
        #[arg(long)]
        reassignment_json_file: PathBuf,
    },
    Cancel {
        #[arg(long)]
        reassignment_json_file: PathBuf,
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
    Id,
    ListEndpoints,
    ApiVersions,
    Unregister {
        #[arg(long)]
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
    parses_command_family!(reassign_family_parses, "reassign", "list");
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
    parses_command_family!(cluster_family_parses, "cluster", "id");
}
