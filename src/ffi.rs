//! Narrow safe wrappers for librdkafka admin APIs not exposed by rust-rdkafka.

#![expect(
    unsafe_code,
    reason = "librdkafka's ACL and leader-election APIs are only available through C FFI"
)]

use std::{
    ffi::{CStr, CString},
    os::raw::c_char,
    ptr,
};

use crate::error::{Error, Result};
use rdkafka_sys as sys;

/// Result for a requested partition leader election.
#[derive(Debug, serde::Serialize)]
pub struct ElectionEntry {
    pub topic: String,
    pub partition: i32,
    pub error: Option<String>,
    pub noop: bool,
}

struct Queue(*mut sys::rd_kafka_queue_t);
impl Drop for Queue {
    fn drop(&mut self) {
        unsafe { sys::rd_kafka_queue_destroy(self.0) };
    }
}

struct Options(*mut sys::rd_kafka_AdminOptions_t);
impl Drop for Options {
    fn drop(&mut self) {
        unsafe { sys::rd_kafka_AdminOptions_destroy(self.0) };
    }
}

struct Event(*mut sys::rd_kafka_event_t);
impl Drop for Event {
    fn drop(&mut self) {
        unsafe { sys::rd_kafka_event_destroy(self.0) };
    }
}

struct PartitionList(*mut sys::rd_kafka_topic_partition_list_t);
impl Drop for PartitionList {
    fn drop(&mut self) {
        unsafe { sys::rd_kafka_topic_partition_list_destroy(self.0) };
    }
}

struct Election(*mut sys::rd_kafka_ElectLeaders_t);
impl Drop for Election {
    fn drop(&mut self) {
        unsafe { sys::rd_kafka_ElectLeaders_destroy(self.0) };
    }
}

struct ConfigResource(*mut sys::rd_kafka_ConfigResource_t);
impl Drop for ConfigResource {
    fn drop(&mut self) {
        unsafe { sys::rd_kafka_ConfigResource_destroy(self.0) };
    }
}

struct DeleteGroupOffsets(*mut sys::rd_kafka_DeleteConsumerGroupOffsets_t);
impl Drop for DeleteGroupOffsets {
    fn drop(&mut self) {
        unsafe { sys::rd_kafka_DeleteConsumerGroupOffsets_destroy(self.0) };
    }
}

struct ListGroupOffsets(*mut sys::rd_kafka_ListConsumerGroupOffsets_t);
impl Drop for ListGroupOffsets {
    fn drop(&mut self) {
        unsafe { sys::rd_kafka_ListConsumerGroupOffsets_destroy(self.0) };
    }
}

struct AlterGroupOffsets(*mut sys::rd_kafka_AlterConsumerGroupOffsets_t);
impl Drop for AlterGroupOffsets {
    fn drop(&mut self) {
        unsafe { sys::rd_kafka_AlterConsumerGroupOffsets_destroy(self.0) };
    }
}

struct NativeAcl(*mut sys::rd_kafka_AclBinding_t);
impl Drop for NativeAcl {
    fn drop(&mut self) {
        unsafe { sys::rd_kafka_AclBinding_destroy(self.0) };
    }
}

struct ScramAlteration(*mut sys::rd_kafka_UserScramCredentialAlteration_t);
impl Drop for ScramAlteration {
    fn drop(&mut self) {
        unsafe { sys::rd_kafka_UserScramCredentialAlteration_destroy(self.0) };
    }
}

struct TopicCollection(*mut sys::rd_kafka_TopicCollection_t);
impl Drop for TopicCollection {
    fn drop(&mut self) {
        unsafe { sys::rd_kafka_TopicCollection_destroy(self.0) };
    }
}

/// ACL resource types supported by librdkafka.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AclResourceType {
    Any,
    Topic,
    Group,
    Cluster,
    TransactionalId,
}

/// ACL resource pattern types supported by librdkafka.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AclPatternType {
    Any,
    Match,
    Literal,
    Prefixed,
}

/// Kafka ACL operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AclOperation {
    Any,
    All,
    Read,
    Write,
    Create,
    Delete,
    Alter,
    Describe,
    ClusterAction,
    DescribeConfigs,
    AlterConfigs,
    IdempotentWrite,
}

/// Kafka ACL permission types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AclPermissionType {
    Any,
    Deny,
    Allow,
}

/// One concrete ACL binding.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AclBinding {
    pub resource_type: AclResourceType,
    pub resource_name: String,
    pub pattern_type: AclPatternType,
    pub principal: String,
    pub host: String,
    pub operation: AclOperation,
    pub permission_type: AclPermissionType,
}

/// Filter accepted by librdkafka's `DescribeAcls` and `DeleteAcls` APIs.
#[derive(Debug, Clone)]
pub struct AclBindingFilter {
    pub resource_type: AclResourceType,
    pub resource_name: Option<String>,
    pub pattern_type: AclPatternType,
    pub principal: Option<String>,
    pub host: Option<String>,
    pub operation: AclOperation,
    pub permission_type: AclPermissionType,
}

/// Outcome of a bulk ACL mutation.
#[derive(Debug, Clone)]
pub struct AclMutationResult {
    pub matched: usize,
    pub failures: usize,
    pub errors: Vec<String>,
}

/// SCRAM mechanisms supported by Kafka and librdkafka.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum ScramMechanism {
    Sha256,
    Sha512,
}

/// One user's configured SCRAM credential metadata.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScramCredentialDescription {
    pub user: String,
    pub mechanism: ScramMechanism,
    pub iterations: i32,
}

/// One requested SCRAM credential alteration.
#[derive(Debug, Clone)]
pub enum ScramCredentialAlteration {
    Upsert {
        mechanism: ScramMechanism,
        iterations: i32,
        password: Vec<u8>,
    },
    Delete {
        mechanism: ScramMechanism,
    },
}

/// Topic name and Kafka topic UUID returned by librdkafka.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TopicIdentity {
    pub name: String,
    pub id: String,
    pub is_internal: bool,
}

/// Offset lookup mode accepted by librdkafka's `ListOffsets` API.
#[derive(Debug, Clone, Copy)]
pub enum ListOffsetSpec {
    Earliest,
    Latest,
    MaxTimestamp,
    EarliestLocal,
    LatestTiered,
    EarliestPendingUpload,
    Timestamp(i64),
}

fn list_offset_spec_value(spec: ListOffsetSpec) -> i64 {
    match spec {
        ListOffsetSpec::Earliest => {
            i64::from(sys::rd_kafka_OffsetSpec_t::RD_KAFKA_OFFSET_SPEC_EARLIEST as i32)
        }
        ListOffsetSpec::Latest => {
            i64::from(sys::rd_kafka_OffsetSpec_t::RD_KAFKA_OFFSET_SPEC_LATEST as i32)
        }
        ListOffsetSpec::MaxTimestamp => {
            i64::from(sys::rd_kafka_OffsetSpec_t::RD_KAFKA_OFFSET_SPEC_MAX_TIMESTAMP as i32)
        }
        ListOffsetSpec::EarliestLocal => -4,
        ListOffsetSpec::LatestTiered => -5,
        ListOffsetSpec::EarliestPendingUpload => -6,
        ListOffsetSpec::Timestamp(timestamp) => timestamp,
    }
}

fn ensure_supported_list_offset_spec(spec: ListOffsetSpec) -> Result<()> {
    let name = match spec {
        ListOffsetSpec::EarliestLocal => "earliest-local (-4)",
        ListOffsetSpec::LatestTiered => "latest-tiered (-5)",
        ListOffsetSpec::EarliestPendingUpload => "earliest-pending-upload (-6)",
        _ => return Ok(()),
    };
    Err(Error::Config(format!(
        "librdkafka 2.12 does not support the {name} ListOffsets query"
    )))
}

/// Offset and timestamp returned for one topic partition.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ListOffsetEntry {
    pub topic: String,
    pub partition: i32,
    pub offset: Option<i64>,
    pub timestamp: Option<i64>,
    pub error: Option<String>,
}

/// Returns the partition leader epoch carried by a successfully consumed message.
#[must_use]
pub fn message_leader_epoch(message: &rdkafka::message::BorrowedMessage<'_>) -> Option<i32> {
    // SAFETY: BorrowedMessage owns a valid librdkafka message pointer for at least this borrow,
    // and callers only receive BorrowedMessage values produced by successful consumer polls.
    let epoch = unsafe { sys::rd_kafka_message_leader_epoch(message.ptr()) };
    (epoch >= 0).then_some(epoch)
}

/// Consumer-group states supported by librdkafka list filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumerGroupState {
    PreparingRebalance,
    CompletingRebalance,
    Stable,
    Dead,
    Empty,
}

/// Consumer-group protocol types supported by librdkafka.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumerGroupType {
    Consumer,
    Classic,
}

/// One entry returned by librdkafka's `ListConsumerGroups` API.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConsumerGroupListing {
    pub group: String,
    pub state: String,
    pub group_type: String,
    pub is_simple: bool,
}

/// Topic partition assigned to a consumer-group member.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConsumerGroupPartition {
    pub topic: String,
    pub partition: i32,
}

/// One member returned by librdkafka's `DescribeConsumerGroups` API.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConsumerGroupMember {
    pub member_id: String,
    pub instance_id: Option<String>,
    pub client_id: String,
    pub host: String,
    pub assignment: Vec<ConsumerGroupPartition>,
    pub target_assignment: Vec<ConsumerGroupPartition>,
}

/// Full consumer-group description returned by librdkafka.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConsumerGroupDescription {
    pub group: String,
    pub state: String,
    pub group_type: String,
    pub assignor: String,
    pub coordinator_id: i32,
    pub coordinator: String,
    pub is_simple: bool,
    pub members: Vec<ConsumerGroupMember>,
}

/// Broker node returned by librdkafka's `DescribeCluster` API.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ClusterNode {
    pub id: i32,
    pub host: String,
    pub port: u16,
    pub rack: Option<String>,
    pub is_controller: bool,
}

/// Cluster identity and nodes returned by librdkafka.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ClusterDescription {
    pub cluster_id: String,
    pub controller_id: i32,
    pub nodes: Vec<ClusterNode>,
}

/// A committed consumer-group offset returned by librdkafka.
#[derive(Debug)]
pub struct GroupOffsetEntry {
    pub topic: String,
    pub partition: i32,
    pub offset: i64,
    pub leader_epoch: Option<i32>,
    pub error: Option<String>,
}

fn queue(client: *mut sys::rd_kafka_t) -> Result<Queue> {
    let queue = unsafe { sys::rd_kafka_queue_new(client) };
    if queue.is_null() {
        Err(Error::Config(
            "librdkafka failed to allocate an admin queue".into(),
        ))
    } else {
        Ok(Queue(queue))
    }
}

fn options(
    client: *mut sys::rd_kafka_t,
    operation: sys::rd_kafka_admin_op_t,
    timeout_ms: i32,
) -> Result<Options> {
    let options = unsafe { sys::rd_kafka_AdminOptions_new(client, operation) };
    if options.is_null() {
        return Err(Error::Unsupported(format!(
            "librdkafka does not support {operation:?}"
        )));
    }
    let options = Options(options);
    let mut error = [0 as c_char; 512];
    let code = unsafe {
        sys::rd_kafka_AdminOptions_set_request_timeout(
            options.0,
            timeout_ms,
            error.as_mut_ptr(),
            error.len(),
        )
    };
    if code != sys::rd_kafka_resp_err_t::RD_KAFKA_RESP_ERR_NO_ERROR {
        return Err(Error::Config(c_buffer(&error)));
    }
    Ok(options)
}

fn poll(queue: &Queue, timeout_ms: i32) -> Result<Event> {
    let event = unsafe { sys::rd_kafka_queue_poll(queue.0, timeout_ms) };
    if event.is_null() {
        return Err(Error::Config("Kafka admin request timed out".into()));
    }
    let event = Event(event);
    let code = unsafe { sys::rd_kafka_event_error(event.0) };
    if code != sys::rd_kafka_resp_err_t::RD_KAFKA_RESP_ERR_NO_ERROR {
        let message = unsafe { c_string(sys::rd_kafka_event_error_string(event.0)) };
        return Err(Error::Config(message));
    }
    Ok(event)
}

/// Describes the cluster through librdkafka's Admin API.
pub fn describe_cluster(
    client: *mut sys::rd_kafka_t,
    timeout_ms: i32,
) -> Result<ClusterDescription> {
    let queue = queue(client)?;
    let options = options(
        client,
        sys::rd_kafka_admin_op_t::RD_KAFKA_ADMIN_OP_DESCRIBECLUSTER,
        timeout_ms,
    )?;
    unsafe { sys::rd_kafka_DescribeCluster(client, options.0, queue.0) };
    let event = poll(&queue, timeout_ms)?;
    let result = unsafe { sys::rd_kafka_event_DescribeCluster_result(event.0) };
    if result.is_null() {
        return Err(Error::Config(
            "broker returned an invalid DescribeCluster response".into(),
        ));
    }
    let controller = unsafe { sys::rd_kafka_DescribeCluster_result_controller(result) };
    let controller_id = if controller.is_null() {
        -1
    } else {
        unsafe { sys::rd_kafka_Node_id(controller) }
    };
    let mut count = 0;
    let nodes = unsafe { sys::rd_kafka_DescribeCluster_result_nodes(result, &raw mut count) };
    if count > 0 && nodes.is_null() {
        return Err(Error::Config("broker returned a null node array".into()));
    }
    let mut rows = Vec::with_capacity(count);
    for index in 0..count {
        let node = unsafe { *nodes.add(index) };
        if node.is_null() {
            return Err(Error::Config("broker returned a null cluster node".into()));
        }
        let id = unsafe { sys::rd_kafka_Node_id(node) };
        rows.push(ClusterNode {
            id,
            host: unsafe { c_string(sys::rd_kafka_Node_host(node)) },
            port: unsafe { sys::rd_kafka_Node_port(node) },
            rack: unsafe { optional_c_string_from_ptr(sys::rd_kafka_Node_rack(node)) },
            is_controller: id == controller_id,
        });
    }
    Ok(ClusterDescription {
        cluster_id: unsafe { c_string(sys::rd_kafka_DescribeCluster_result_cluster_id(result)) },
        controller_id,
        nodes: rows,
    })
}

/// Lists consumer groups with broker-side state and type filters.
pub fn list_consumer_groups(
    client: *mut sys::rd_kafka_t,
    states: &[ConsumerGroupState],
    types: &[ConsumerGroupType],
    timeout_ms: i32,
) -> Result<Vec<ConsumerGroupListing>> {
    let queue = queue(client)?;
    let options = options(
        client,
        sys::rd_kafka_admin_op_t::RD_KAFKA_ADMIN_OP_LISTCONSUMERGROUPS,
        timeout_ms,
    )?;
    let native_states = states
        .iter()
        .copied()
        .map(native_consumer_group_state)
        .collect::<Vec<_>>();
    if !native_states.is_empty() {
        let error = unsafe {
            sys::rd_kafka_AdminOptions_set_match_consumer_group_states(
                options.0,
                native_states.as_ptr(),
                native_states.len(),
            )
        };
        check_owned_error(error)?;
    }
    let native_types = types
        .iter()
        .copied()
        .map(native_consumer_group_type)
        .collect::<Vec<_>>();
    if !native_types.is_empty() {
        let error = unsafe {
            sys::rd_kafka_AdminOptions_set_match_consumer_group_types(
                options.0,
                native_types.as_ptr(),
                native_types.len(),
            )
        };
        check_owned_error(error)?;
    }
    unsafe { sys::rd_kafka_ListConsumerGroups(client, options.0, queue.0) };
    let event = poll(&queue, timeout_ms)?;
    let result = unsafe { sys::rd_kafka_event_ListConsumerGroups_result(event.0) };
    if result.is_null() {
        return Err(Error::Config(
            "broker returned an invalid ListConsumerGroups response".into(),
        ));
    }
    let mut error_count = 0;
    let errors =
        unsafe { sys::rd_kafka_ListConsumerGroups_result_errors(result, &raw mut error_count) };
    if error_count > 0 && errors.is_null() {
        return Err(Error::Config(
            "broker returned a null consumer-group error array".into(),
        ));
    }
    for index in 0..error_count {
        let error = unsafe { *errors.add(index) };
        if native_error_failed(error) {
            return Err(Error::Config(unsafe {
                c_string(sys::rd_kafka_error_string(error))
            }));
        }
    }
    let mut count = 0;
    let listings = unsafe { sys::rd_kafka_ListConsumerGroups_result_valid(result, &raw mut count) };
    if count > 0 && listings.is_null() {
        return Err(Error::Config(
            "broker returned a null consumer-group listing array".into(),
        ));
    }
    let mut rows = Vec::with_capacity(count);
    for index in 0..count {
        let listing = unsafe { *listings.add(index) };
        if listing.is_null() {
            return Err(Error::Config(
                "broker returned a null consumer-group listing".into(),
            ));
        }
        rows.push(ConsumerGroupListing {
            group: unsafe { c_string(sys::rd_kafka_ConsumerGroupListing_group_id(listing)) },
            state: consumer_group_state_name(unsafe {
                sys::rd_kafka_ConsumerGroupListing_state(listing)
            })
            .to_owned(),
            group_type: consumer_group_type_name(unsafe {
                sys::rd_kafka_ConsumerGroupListing_type(listing)
            })
            .to_owned(),
            is_simple: unsafe {
                sys::rd_kafka_ConsumerGroupListing_is_simple_consumer_group(listing)
            } != 0,
        });
    }
    Ok(rows)
}

/// Describes consumer groups and decodes member assignments through librdkafka.
pub fn describe_consumer_groups(
    client: *mut sys::rd_kafka_t,
    groups: &[String],
    timeout_ms: i32,
) -> Result<Vec<ConsumerGroupDescription>> {
    let groups = groups
        .iter()
        .map(|group| {
            CString::new(group.as_str())
                .map_err(|_| Error::Usage("consumer group contains a NUL byte".into()))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut pointers = groups
        .iter()
        .map(|group| group.as_ptr())
        .collect::<Vec<_>>();
    let queue = queue(client)?;
    let options = options(
        client,
        sys::rd_kafka_admin_op_t::RD_KAFKA_ADMIN_OP_DESCRIBECONSUMERGROUPS,
        timeout_ms,
    )?;
    unsafe {
        sys::rd_kafka_DescribeConsumerGroups(
            client,
            pointers.as_mut_ptr(),
            pointers.len(),
            options.0,
            queue.0,
        );
    }
    let event = poll(&queue, timeout_ms)?;
    let result = unsafe { sys::rd_kafka_event_DescribeConsumerGroups_result(event.0) };
    if result.is_null() {
        return Err(Error::Config(
            "broker returned an invalid DescribeConsumerGroups response".into(),
        ));
    }
    let mut count = 0;
    let descriptions =
        unsafe { sys::rd_kafka_DescribeConsumerGroups_result_groups(result, &raw mut count) };
    if count > 0 && descriptions.is_null() {
        return Err(Error::Config(
            "broker returned a null consumer-group description array".into(),
        ));
    }
    let mut rows = Vec::with_capacity(count);
    for index in 0..count {
        let description = unsafe { *descriptions.add(index) };
        rows.push(unsafe { consumer_group_description_from_native(description) }?);
    }
    Ok(rows)
}

unsafe fn consumer_group_description_from_native(
    description: *const sys::rd_kafka_ConsumerGroupDescription_t,
) -> Result<ConsumerGroupDescription> {
    if description.is_null() {
        return Err(Error::Config(
            "broker returned a null consumer-group description".into(),
        ));
    }
    let error = unsafe { sys::rd_kafka_ConsumerGroupDescription_error(description) };
    if native_error_failed(error) {
        return Err(Error::Config(unsafe {
            c_string(sys::rd_kafka_error_string(error))
        }));
    }
    let coordinator = unsafe { sys::rd_kafka_ConsumerGroupDescription_coordinator(description) };
    let member_count = unsafe { sys::rd_kafka_ConsumerGroupDescription_member_count(description) };
    let members = (0..member_count)
        .map(|index| unsafe {
            consumer_group_member_from_native(sys::rd_kafka_ConsumerGroupDescription_member(
                description,
                index,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ConsumerGroupDescription {
        group: unsafe { c_string(sys::rd_kafka_ConsumerGroupDescription_group_id(description)) },
        state: consumer_group_state_name(unsafe {
            sys::rd_kafka_ConsumerGroupDescription_state(description)
        })
        .to_owned(),
        group_type: consumer_group_type_name(unsafe {
            sys::rd_kafka_ConsumerGroupDescription_type(description)
        })
        .to_owned(),
        assignor: unsafe {
            c_string(sys::rd_kafka_ConsumerGroupDescription_partition_assignor(
                description,
            ))
        },
        coordinator_id: if coordinator.is_null() {
            -1
        } else {
            unsafe { sys::rd_kafka_Node_id(coordinator) }
        },
        coordinator: if coordinator.is_null() {
            String::new()
        } else {
            format!(
                "{}:{}",
                unsafe { c_string(sys::rd_kafka_Node_host(coordinator)) },
                unsafe { sys::rd_kafka_Node_port(coordinator) }
            )
        },
        is_simple: unsafe {
            sys::rd_kafka_ConsumerGroupDescription_is_simple_consumer_group(description)
        } != 0,
        members,
    })
}

unsafe fn consumer_group_member_from_native(
    member: *const sys::rd_kafka_MemberDescription_t,
) -> Result<ConsumerGroupMember> {
    if member.is_null() {
        return Err(Error::Config(
            "broker returned a null consumer-group member".into(),
        ));
    }
    Ok(ConsumerGroupMember {
        member_id: unsafe { c_string(sys::rd_kafka_MemberDescription_consumer_id(member)) },
        instance_id: unsafe {
            optional_c_string_from_ptr(sys::rd_kafka_MemberDescription_group_instance_id(member))
        },
        client_id: unsafe { c_string(sys::rd_kafka_MemberDescription_client_id(member)) },
        host: unsafe { c_string(sys::rd_kafka_MemberDescription_host(member)) },
        assignment: unsafe {
            member_assignment_partitions(sys::rd_kafka_MemberDescription_assignment(member))
        }?,
        target_assignment: unsafe {
            member_assignment_partitions(sys::rd_kafka_MemberDescription_target_assignment(member))
        }?,
    })
}

unsafe fn member_assignment_partitions(
    assignment: *const sys::rd_kafka_MemberAssignment_t,
) -> Result<Vec<ConsumerGroupPartition>> {
    if assignment.is_null() {
        return Ok(Vec::new());
    }
    let partitions = unsafe { sys::rd_kafka_MemberAssignment_partitions(assignment) };
    if partitions.is_null() {
        return Ok(Vec::new());
    }
    let count = usize::try_from(unsafe { (*partitions).cnt })
        .map_err(|_| Error::Config("invalid member assignment partition count".into()))?;
    let mut rows = Vec::with_capacity(count);
    for index in 0..count {
        let partition = unsafe { &*(*partitions).elems.add(index) };
        rows.push(ConsumerGroupPartition {
            topic: unsafe { c_string(partition.topic) },
            partition: partition.partition,
        });
    }
    Ok(rows)
}

unsafe fn optional_c_string_from_ptr(pointer: *const c_char) -> Option<String> {
    (!pointer.is_null()).then(|| unsafe { c_string(pointer) })
}

fn check_owned_error(error: *mut sys::rd_kafka_error_t) -> Result<()> {
    if error.is_null() {
        return Ok(());
    }
    let failed = native_error_failed(error);
    let message = failed.then(|| unsafe { c_string(sys::rd_kafka_error_string(error)) });
    unsafe { sys::rd_kafka_error_destroy(error) };
    message.map_or(Ok(()), |message| Err(Error::Config(message)))
}

const fn native_consumer_group_state(
    value: ConsumerGroupState,
) -> sys::rd_kafka_consumer_group_state_t {
    match value {
        ConsumerGroupState::PreparingRebalance => {
            sys::rd_kafka_consumer_group_state_t::RD_KAFKA_CONSUMER_GROUP_STATE_PREPARING_REBALANCE
        }
        ConsumerGroupState::CompletingRebalance => {
            sys::rd_kafka_consumer_group_state_t::RD_KAFKA_CONSUMER_GROUP_STATE_COMPLETING_REBALANCE
        }
        ConsumerGroupState::Stable => {
            sys::rd_kafka_consumer_group_state_t::RD_KAFKA_CONSUMER_GROUP_STATE_STABLE
        }
        ConsumerGroupState::Dead => {
            sys::rd_kafka_consumer_group_state_t::RD_KAFKA_CONSUMER_GROUP_STATE_DEAD
        }
        ConsumerGroupState::Empty => {
            sys::rd_kafka_consumer_group_state_t::RD_KAFKA_CONSUMER_GROUP_STATE_EMPTY
        }
    }
}

const fn native_consumer_group_type(
    value: ConsumerGroupType,
) -> sys::rd_kafka_consumer_group_type_t {
    match value {
        ConsumerGroupType::Consumer => {
            sys::rd_kafka_consumer_group_type_t::RD_KAFKA_CONSUMER_GROUP_TYPE_CONSUMER
        }
        ConsumerGroupType::Classic => {
            sys::rd_kafka_consumer_group_type_t::RD_KAFKA_CONSUMER_GROUP_TYPE_CLASSIC
        }
    }
}

const fn consumer_group_state_name(value: sys::rd_kafka_consumer_group_state_t) -> &'static str {
    match value {
        sys::rd_kafka_consumer_group_state_t::RD_KAFKA_CONSUMER_GROUP_STATE_PREPARING_REBALANCE => {
            "PreparingRebalance"
        }
        sys::rd_kafka_consumer_group_state_t::RD_KAFKA_CONSUMER_GROUP_STATE_COMPLETING_REBALANCE => {
            "CompletingRebalance"
        }
        sys::rd_kafka_consumer_group_state_t::RD_KAFKA_CONSUMER_GROUP_STATE_STABLE => "Stable",
        sys::rd_kafka_consumer_group_state_t::RD_KAFKA_CONSUMER_GROUP_STATE_DEAD => "Dead",
        sys::rd_kafka_consumer_group_state_t::RD_KAFKA_CONSUMER_GROUP_STATE_EMPTY => "Empty",
        _ => "Unknown",
    }
}

const fn consumer_group_type_name(value: sys::rd_kafka_consumer_group_type_t) -> &'static str {
    match value {
        sys::rd_kafka_consumer_group_type_t::RD_KAFKA_CONSUMER_GROUP_TYPE_CONSUMER => "Consumer",
        sys::rd_kafka_consumer_group_type_t::RD_KAFKA_CONSUMER_GROUP_TYPE_CLASSIC => "Classic",
        _ => "Unknown",
    }
}

/// Resolves offsets through librdkafka's `ListOffsets` Admin API.
pub fn list_offsets(
    client: *mut sys::rd_kafka_t,
    targets: &[(String, i32)],
    spec: ListOffsetSpec,
    timeout_ms: i32,
) -> Result<Vec<ListOffsetEntry>> {
    ensure_supported_list_offset_spec(spec)?;
    let capacity =
        i32::try_from(targets.len()).map_err(|_| Error::Usage("too many offset targets".into()))?;
    let list = unsafe { sys::rd_kafka_topic_partition_list_new(capacity) };
    if list.is_null() {
        return Err(Error::Config(
            "failed to allocate ListOffsets partition list".into(),
        ));
    }
    let list = PartitionList(list);
    let requested_offset = list_offset_spec_value(spec);
    for (topic, partition) in targets {
        let topic = CString::new(topic.as_str())
            .map_err(|_| Error::Usage("topic name contains a NUL byte".into()))?;
        let element =
            unsafe { sys::rd_kafka_topic_partition_list_add(list.0, topic.as_ptr(), *partition) };
        if element.is_null() {
            return Err(Error::Config(
                "failed to add a ListOffsets partition".into(),
            ));
        }
        unsafe { (*element).offset = requested_offset };
    }
    let queue = queue(client)?;
    let options = options(
        client,
        sys::rd_kafka_admin_op_t::RD_KAFKA_ADMIN_OP_LISTOFFSETS,
        timeout_ms,
    )?;
    unsafe { sys::rd_kafka_ListOffsets(client, list.0, options.0, queue.0) };
    let event = poll(&queue, timeout_ms)?;
    let result = unsafe { sys::rd_kafka_event_ListOffsets_result(event.0) };
    if result.is_null() {
        return Err(Error::Config(
            "broker returned an invalid ListOffsets response".into(),
        ));
    }
    let mut count = 0;
    let infos = unsafe { sys::rd_kafka_ListOffsets_result_infos(result, &raw mut count) };
    if count > 0 && infos.is_null() {
        return Err(Error::Config(
            "broker returned a null ListOffsets result array".into(),
        ));
    }
    let mut rows = Vec::with_capacity(count);
    for index in 0..count {
        let info = unsafe { *infos.add(index) };
        if info.is_null() {
            return Err(Error::Config(
                "broker returned a null ListOffsets result".into(),
            ));
        }
        let partition = unsafe { sys::rd_kafka_ListOffsetsResultInfo_topic_partition(info) };
        if partition.is_null() {
            return Err(Error::Config(
                "broker returned a null ListOffsets topic partition".into(),
            ));
        }
        let topic = unsafe { c_string((*partition).topic) };
        let partition_id = unsafe { (*partition).partition };
        if unsafe { (*partition).err } != sys::rd_kafka_resp_err_t::RD_KAFKA_RESP_ERR_NO_ERROR {
            rows.push(ListOffsetEntry {
                topic,
                partition: partition_id,
                offset: None,
                timestamp: None,
                error: Some(unsafe { c_string(sys::rd_kafka_err2str((*partition).err)) }),
            });
            continue;
        }
        let offset = unsafe { (*partition).offset };
        if offset == -1 {
            continue;
        }
        rows.push(ListOffsetEntry {
            topic,
            partition: partition_id,
            offset: Some(offset),
            timestamp: Some(unsafe { sys::rd_kafka_ListOffsetsResultInfo_timestamp(info) }),
            error: None,
        });
    }
    rows.sort_by(|left, right| {
        left.topic
            .cmp(&right.topic)
            .then(left.partition.cmp(&right.partition))
    });
    Ok(rows)
}

/// Describes topic identities through librdkafka's `DescribeTopics` Admin API.
pub fn describe_topic_identities(
    client: *mut sys::rd_kafka_t,
    topics: &[String],
    timeout_ms: i32,
) -> Result<Vec<TopicIdentity>> {
    let topics = topics
        .iter()
        .map(|topic| {
            CString::new(topic.as_str())
                .map_err(|_| Error::Usage("topic name contains a NUL byte".into()))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut names = topics
        .iter()
        .map(|topic| topic.as_ptr())
        .collect::<Vec<_>>();
    let collection =
        unsafe { sys::rd_kafka_TopicCollection_of_topic_names(names.as_mut_ptr(), names.len()) };
    if collection.is_null() {
        return Err(Error::Config(
            "librdkafka failed to create a topic collection".into(),
        ));
    }
    let collection = TopicCollection(collection);
    let queue = queue(client)?;
    let options = options(
        client,
        sys::rd_kafka_admin_op_t::RD_KAFKA_ADMIN_OP_DESCRIBETOPICS,
        timeout_ms,
    )?;
    unsafe { sys::rd_kafka_DescribeTopics(client, collection.0, options.0, queue.0) };
    let event = poll(&queue, timeout_ms)?;
    let result = unsafe { sys::rd_kafka_event_DescribeTopics_result(event.0) };
    if result.is_null() {
        return Err(Error::Config(
            "broker returned an invalid DescribeTopics response".into(),
        ));
    }
    let mut count = 0;
    let descriptions =
        unsafe { sys::rd_kafka_DescribeTopics_result_topics(result, &raw mut count) };
    if count > 0 && descriptions.is_null() {
        return Err(Error::Config(
            "broker returned a null topic-description array".into(),
        ));
    }
    let mut rows = Vec::with_capacity(count);
    for index in 0..count {
        let description = unsafe { *descriptions.add(index) };
        if description.is_null() {
            return Err(Error::Config(
                "broker returned a null topic description".into(),
            ));
        }
        let error = unsafe { sys::rd_kafka_TopicDescription_error(description) };
        if native_error_failed(error) {
            return Err(Error::Config(unsafe {
                c_string(sys::rd_kafka_error_string(error))
            }));
        }
        let uuid = unsafe { sys::rd_kafka_TopicDescription_topic_id(description) };
        if uuid.is_null() {
            return Err(Error::Config("broker returned a null topic ID".into()));
        }
        rows.push(TopicIdentity {
            name: unsafe { c_string(sys::rd_kafka_TopicDescription_name(description)) },
            id: unsafe { c_string(sys::rd_kafka_Uuid_base64str(uuid)) },
            is_internal: unsafe { sys::rd_kafka_TopicDescription_is_internal(description) } != 0,
        });
    }
    Ok(rows)
}

/// Describes SCRAM credential metadata for the selected users.
pub fn describe_user_scram_credentials(
    client: *mut sys::rd_kafka_t,
    users: &[String],
    timeout_ms: i32,
) -> Result<Vec<ScramCredentialDescription>> {
    let users = users
        .iter()
        .map(|user| {
            CString::new(user.as_str())
                .map_err(|_| Error::Usage("SCRAM user contains a NUL byte".into()))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut pointers = users.iter().map(|user| user.as_ptr()).collect::<Vec<_>>();
    let queue = queue(client)?;
    let options = options(
        client,
        sys::rd_kafka_admin_op_t::RD_KAFKA_ADMIN_OP_DESCRIBEUSERSCRAMCREDENTIALS,
        timeout_ms,
    )?;
    unsafe {
        sys::rd_kafka_DescribeUserScramCredentials(
            client,
            pointers.as_mut_ptr(),
            pointers.len(),
            options.0,
            queue.0,
        );
    }
    let event = poll(&queue, timeout_ms)?;
    let result = unsafe { sys::rd_kafka_event_DescribeUserScramCredentials_result(event.0) };
    if result.is_null() {
        return Err(Error::Config(
            "broker returned an invalid DescribeUserScramCredentials response".into(),
        ));
    }
    let mut count = 0;
    let descriptions = unsafe {
        sys::rd_kafka_DescribeUserScramCredentials_result_descriptions(result, &raw mut count)
    };
    if count > 0 && descriptions.is_null() {
        return Err(Error::Config(
            "broker returned a null SCRAM description array".into(),
        ));
    }
    let mut rows = Vec::new();
    for index in 0..count {
        let description = unsafe { *descriptions.add(index) };
        if description.is_null() {
            return Err(Error::Config(
                "broker returned a null SCRAM description".into(),
            ));
        }
        let error = unsafe { sys::rd_kafka_UserScramCredentialsDescription_error(description) };
        if native_error_failed(error) {
            return Err(Error::Config(unsafe {
                c_string(sys::rd_kafka_error_string(error))
            }));
        }
        let user = unsafe {
            c_string(sys::rd_kafka_UserScramCredentialsDescription_user(
                description,
            ))
        };
        let credential_count = unsafe {
            sys::rd_kafka_UserScramCredentialsDescription_scramcredentialinfo_count(description)
        };
        for credential_index in 0..credential_count {
            let credential = unsafe {
                sys::rd_kafka_UserScramCredentialsDescription_scramcredentialinfo(
                    description,
                    credential_index,
                )
            };
            if credential.is_null() {
                return Err(Error::Config(
                    "broker returned a null SCRAM credential".into(),
                ));
            }
            rows.push(ScramCredentialDescription {
                user: user.clone(),
                mechanism: scram_mechanism_from_native(unsafe {
                    sys::rd_kafka_ScramCredentialInfo_mechanism(credential)
                })?,
                iterations: unsafe { sys::rd_kafka_ScramCredentialInfo_iterations(credential) },
            });
        }
    }
    Ok(rows)
}

/// Upserts and deletes one user's SCRAM credentials.
pub fn alter_user_scram_credentials(
    client: *mut sys::rd_kafka_t,
    user: &str,
    changes: &[ScramCredentialAlteration],
    timeout_ms: i32,
) -> Result<()> {
    let user =
        CString::new(user).map_err(|_| Error::Usage("SCRAM user contains a NUL byte".into()))?;
    let native = changes
        .iter()
        .map(|change| {
            let alteration = match change {
                ScramCredentialAlteration::Upsert {
                    mechanism,
                    iterations,
                    password,
                } => unsafe {
                    sys::rd_kafka_UserScramCredentialUpsertion_new(
                        user.as_ptr(),
                        native_scram_mechanism(*mechanism),
                        *iterations,
                        password.as_ptr(),
                        password.len(),
                        ptr::null(),
                        0,
                    )
                },
                ScramCredentialAlteration::Delete { mechanism } => unsafe {
                    sys::rd_kafka_UserScramCredentialDeletion_new(
                        user.as_ptr(),
                        native_scram_mechanism(*mechanism),
                    )
                },
            };
            if alteration.is_null() {
                Err(Error::Config(
                    "librdkafka failed to construct a SCRAM alteration".into(),
                ))
            } else {
                Ok(ScramAlteration(alteration))
            }
        })
        .collect::<Result<Vec<_>>>()?;
    let mut pointers = native.iter().map(|change| change.0).collect::<Vec<_>>();
    let queue = queue(client)?;
    let options = options(
        client,
        sys::rd_kafka_admin_op_t::RD_KAFKA_ADMIN_OP_ALTERUSERSCRAMCREDENTIALS,
        timeout_ms,
    )?;
    unsafe {
        sys::rd_kafka_AlterUserScramCredentials(
            client,
            pointers.as_mut_ptr(),
            pointers.len(),
            options.0,
            queue.0,
        );
    }
    let event = poll(&queue, timeout_ms)?;
    let result = unsafe { sys::rd_kafka_event_AlterUserScramCredentials_result(event.0) };
    if result.is_null() {
        return Err(Error::Config(
            "broker returned an invalid AlterUserScramCredentials response".into(),
        ));
    }
    let mut count = 0;
    let responses =
        unsafe { sys::rd_kafka_AlterUserScramCredentials_result_responses(result, &raw mut count) };
    if count > 0 && responses.is_null() {
        return Err(Error::Config(
            "broker returned a null SCRAM alteration response array".into(),
        ));
    }
    for index in 0..count {
        let response = unsafe { *responses.add(index) };
        if response.is_null() {
            return Err(Error::Config(
                "broker returned a null SCRAM alteration response".into(),
            ));
        }
        let error =
            unsafe { sys::rd_kafka_AlterUserScramCredentials_result_response_error(response) };
        if native_error_failed(error) {
            return Err(Error::Config(unsafe {
                c_string(sys::rd_kafka_error_string(error))
            }));
        }
    }
    Ok(())
}

const fn native_scram_mechanism(value: ScramMechanism) -> sys::rd_kafka_ScramMechanism_t {
    match value {
        ScramMechanism::Sha256 => sys::rd_kafka_ScramMechanism_t::RD_KAFKA_SCRAM_MECHANISM_SHA_256,
        ScramMechanism::Sha512 => sys::rd_kafka_ScramMechanism_t::RD_KAFKA_SCRAM_MECHANISM_SHA_512,
    }
}

fn scram_mechanism_from_native(value: sys::rd_kafka_ScramMechanism_t) -> Result<ScramMechanism> {
    match value {
        sys::rd_kafka_ScramMechanism_t::RD_KAFKA_SCRAM_MECHANISM_SHA_256 => {
            Ok(ScramMechanism::Sha256)
        }
        sys::rd_kafka_ScramMechanism_t::RD_KAFKA_SCRAM_MECHANISM_SHA_512 => {
            Ok(ScramMechanism::Sha512)
        }
        _ => Err(Error::Unsupported(
            "librdkafka returned an unknown SCRAM mechanism".into(),
        )),
    }
}

fn native_error_failed(error: *const sys::rd_kafka_error_t) -> bool {
    !error.is_null()
        && unsafe { sys::rd_kafka_error_code(error) }
            != sys::rd_kafka_resp_err_t::RD_KAFKA_RESP_ERR_NO_ERROR
}

/// Creates ACL bindings through librdkafka's Admin API.
pub fn create_acls(
    client: *mut sys::rd_kafka_t,
    bindings: &[AclBinding],
    timeout_ms: i32,
) -> Result<AclMutationResult> {
    let native = bindings
        .iter()
        .map(native_acl_binding)
        .collect::<Result<Vec<_>>>()?;
    let mut pointers = native.iter().map(|binding| binding.0).collect::<Vec<_>>();
    let queue = queue(client)?;
    let options = options(
        client,
        sys::rd_kafka_admin_op_t::RD_KAFKA_ADMIN_OP_CREATEACLS,
        timeout_ms,
    )?;
    unsafe {
        sys::rd_kafka_CreateAcls(
            client,
            pointers.as_mut_ptr(),
            pointers.len(),
            options.0,
            queue.0,
        );
    }
    let event = poll(&queue, timeout_ms)?;
    let result = unsafe { sys::rd_kafka_event_CreateAcls_result(event.0) };
    if result.is_null() {
        return Err(Error::Config(
            "broker returned an invalid CreateAcls response".into(),
        ));
    }
    let mut count = 0;
    let results = unsafe { sys::rd_kafka_CreateAcls_result_acls(result, &raw mut count) };
    let errors = acl_result_errors(results, count, "ACL creation")?;
    Ok(AclMutationResult {
        matched: count,
        failures: errors.len(),
        errors,
    })
}

/// Describes ACL bindings through librdkafka's Admin API.
pub fn describe_acls(
    client: *mut sys::rd_kafka_t,
    filter: &AclBindingFilter,
    timeout_ms: i32,
) -> Result<Vec<AclBinding>> {
    let filter = native_acl_filter(filter)?;
    let queue = queue(client)?;
    let options = options(
        client,
        sys::rd_kafka_admin_op_t::RD_KAFKA_ADMIN_OP_DESCRIBEACLS,
        timeout_ms,
    )?;
    unsafe { sys::rd_kafka_DescribeAcls(client, filter.0, options.0, queue.0) };
    let event = poll(&queue, timeout_ms)?;
    let result = unsafe { sys::rd_kafka_event_DescribeAcls_result(event.0) };
    if result.is_null() {
        return Err(Error::Config(
            "broker returned an invalid DescribeAcls response".into(),
        ));
    }
    let mut count = 0;
    let bindings = unsafe { sys::rd_kafka_DescribeAcls_result_acls(result, &raw mut count) };
    if count > 0 && bindings.is_null() {
        return Err(Error::Config(
            "broker returned a null DescribeAcls binding array".into(),
        ));
    }
    (0..count)
        .map(|index| unsafe { acl_binding_from_native(*bindings.add(index)) })
        .collect()
}

/// Deletes ACL bindings through librdkafka's Admin API.
pub fn delete_acls(
    client: *mut sys::rd_kafka_t,
    filters: &[AclBindingFilter],
    timeout_ms: i32,
) -> Result<AclMutationResult> {
    let native = filters
        .iter()
        .map(native_acl_filter)
        .collect::<Result<Vec<_>>>()?;
    let mut pointers = native.iter().map(|filter| filter.0).collect::<Vec<_>>();
    let queue = queue(client)?;
    let options = options(
        client,
        sys::rd_kafka_admin_op_t::RD_KAFKA_ADMIN_OP_DELETEACLS,
        timeout_ms,
    )?;
    unsafe {
        sys::rd_kafka_DeleteAcls(
            client,
            pointers.as_mut_ptr(),
            pointers.len(),
            options.0,
            queue.0,
        );
    }
    let event = poll(&queue, timeout_ms)?;
    let result = unsafe { sys::rd_kafka_event_DeleteAcls_result(event.0) };
    if result.is_null() {
        return Err(Error::Config(
            "broker returned an invalid DeleteAcls response".into(),
        ));
    }
    let mut response_count = 0;
    let responses =
        unsafe { sys::rd_kafka_DeleteAcls_result_responses(result, &raw mut response_count) };
    if response_count > 0 && responses.is_null() {
        return Err(Error::Config(
            "broker returned a null DeleteAcls response array".into(),
        ));
    }
    let mut matched = 0;
    let mut errors = Vec::new();
    for index in 0..response_count {
        let response = unsafe { *responses.add(index) };
        if response.is_null() {
            return Err(Error::Config("broker returned a null ACL result".into()));
        }
        let error = unsafe { sys::rd_kafka_DeleteAcls_result_response_error(response) };
        if !error.is_null() {
            errors.push(format!("ACL deletion failed: {}", unsafe {
                c_string(sys::rd_kafka_error_string(error))
            }));
        }
        let mut matching_count = 0;
        let matching = unsafe {
            sys::rd_kafka_DeleteAcls_result_response_matching_acls(
                response,
                &raw mut matching_count,
            )
        };
        matched += matching_count;
        if matching_count > 0 && matching.is_null() {
            return Err(Error::Config(
                "broker returned a null deleted ACL array".into(),
            ));
        }
        for matching_index in 0..matching_count {
            let binding = unsafe { *matching.add(matching_index) };
            if binding.is_null() {
                return Err(Error::Config("broker returned a null ACL binding".into()));
            }
            let error = unsafe { sys::rd_kafka_AclBinding_error(binding) };
            if !error.is_null() {
                errors.push(format!("ACL binding deletion failed: {}", unsafe {
                    c_string(sys::rd_kafka_error_string(error))
                }));
            }
        }
    }
    Ok(AclMutationResult {
        matched,
        failures: errors.len(),
        errors,
    })
}

fn native_acl_binding(binding: &AclBinding) -> Result<NativeAcl> {
    native_acl(
        binding.resource_type,
        Some(binding.resource_name.as_str()),
        binding.pattern_type,
        Some(binding.principal.as_str()),
        Some(binding.host.as_str()),
        binding.operation,
        binding.permission_type,
        false,
    )
}

fn native_acl_filter(filter: &AclBindingFilter) -> Result<NativeAcl> {
    native_acl(
        filter.resource_type,
        filter.resource_name.as_deref(),
        filter.pattern_type,
        filter.principal.as_deref(),
        filter.host.as_deref(),
        filter.operation,
        filter.permission_type,
        true,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "mirrors librdkafka's ACL constructor"
)]
fn native_acl(
    resource_type: AclResourceType,
    resource_name: Option<&str>,
    pattern_type: AclPatternType,
    principal: Option<&str>,
    host: Option<&str>,
    operation: AclOperation,
    permission_type: AclPermissionType,
    filter: bool,
) -> Result<NativeAcl> {
    let resource_name = optional_c_string(resource_name, "ACL resource name")?;
    let principal = optional_c_string(principal, "ACL principal")?;
    let host = optional_c_string(host, "ACL host")?;
    let mut error = [0 as c_char; 512];
    let constructor = if filter {
        sys::rd_kafka_AclBindingFilter_new
    } else {
        sys::rd_kafka_AclBinding_new
    };
    let binding = unsafe {
        constructor(
            native_acl_resource(resource_type),
            resource_name
                .as_ref()
                .map_or(ptr::null(), |value| value.as_ptr()),
            native_acl_pattern(pattern_type),
            principal
                .as_ref()
                .map_or(ptr::null(), |value| value.as_ptr()),
            host.as_ref().map_or(ptr::null(), |value| value.as_ptr()),
            native_acl_operation(operation),
            native_acl_permission(permission_type),
            error.as_mut_ptr(),
            error.len(),
        )
    };
    if binding.is_null() {
        Err(Error::Config(c_buffer(&error)))
    } else {
        Ok(NativeAcl(binding))
    }
}

fn optional_c_string(value: Option<&str>, field: &str) -> Result<Option<CString>> {
    value
        .map(|value| {
            CString::new(value).map_err(|_| Error::Usage(format!("{field} contains a NUL byte")))
        })
        .transpose()
}

fn acl_result_errors(
    results: *mut *const sys::rd_kafka_acl_result_t,
    count: usize,
    operation: &str,
) -> Result<Vec<String>> {
    if count > 0 && results.is_null() {
        return Err(Error::Config(format!(
            "broker returned a null {operation} result array"
        )));
    }
    let mut errors = Vec::new();
    for index in 0..count {
        let result = unsafe { *results.add(index) };
        if result.is_null() {
            return Err(Error::Config(format!(
                "broker returned a null {operation} result"
            )));
        }
        let error = unsafe { sys::rd_kafka_acl_result_error(result) };
        if !error.is_null() {
            errors.push(format!("{operation} failed: {}", unsafe {
                c_string(sys::rd_kafka_error_string(error))
            }));
        }
    }
    Ok(errors)
}

unsafe fn acl_binding_from_native(
    binding: *const sys::rd_kafka_AclBinding_t,
) -> Result<AclBinding> {
    if binding.is_null() {
        return Err(Error::Config("broker returned a null ACL binding".into()));
    }
    let error = unsafe { sys::rd_kafka_AclBinding_error(binding) };
    if !error.is_null() {
        return Err(Error::Config(unsafe {
            c_string(sys::rd_kafka_error_string(error))
        }));
    }
    Ok(AclBinding {
        resource_type: unsafe {
            acl_resource_from_native(sys::rd_kafka_AclBinding_restype(binding))
        }?,
        resource_name: unsafe { c_string(sys::rd_kafka_AclBinding_name(binding)) },
        pattern_type: unsafe {
            acl_pattern_from_native(sys::rd_kafka_AclBinding_resource_pattern_type(binding))
        }?,
        principal: unsafe { c_string(sys::rd_kafka_AclBinding_principal(binding)) },
        host: unsafe { c_string(sys::rd_kafka_AclBinding_host(binding)) },
        operation: unsafe {
            acl_operation_from_native(sys::rd_kafka_AclBinding_operation(binding))
        }?,
        permission_type: unsafe {
            acl_permission_from_native(sys::rd_kafka_AclBinding_permission_type(binding))
        }?,
    })
}

const fn native_acl_resource(value: AclResourceType) -> sys::rd_kafka_ResourceType_t {
    match value {
        AclResourceType::Any => sys::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_ANY,
        AclResourceType::Topic => sys::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_TOPIC,
        AclResourceType::Group => sys::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_GROUP,
        AclResourceType::Cluster => sys::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_BROKER,
        AclResourceType::TransactionalId => {
            sys::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_TRANSACTIONAL_ID
        }
    }
}

const fn native_acl_pattern(value: AclPatternType) -> sys::rd_kafka_ResourcePatternType_t {
    match value {
        AclPatternType::Any => sys::rd_kafka_ResourcePatternType_t::RD_KAFKA_RESOURCE_PATTERN_ANY,
        AclPatternType::Match => {
            sys::rd_kafka_ResourcePatternType_t::RD_KAFKA_RESOURCE_PATTERN_MATCH
        }
        AclPatternType::Literal => {
            sys::rd_kafka_ResourcePatternType_t::RD_KAFKA_RESOURCE_PATTERN_LITERAL
        }
        AclPatternType::Prefixed => {
            sys::rd_kafka_ResourcePatternType_t::RD_KAFKA_RESOURCE_PATTERN_PREFIXED
        }
    }
}

const fn native_acl_operation(value: AclOperation) -> sys::rd_kafka_AclOperation_t {
    match value {
        AclOperation::Any => sys::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_ANY,
        AclOperation::All => sys::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_ALL,
        AclOperation::Read => sys::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_READ,
        AclOperation::Write => sys::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_WRITE,
        AclOperation::Create => sys::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_CREATE,
        AclOperation::Delete => sys::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_DELETE,
        AclOperation::Alter => sys::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_ALTER,
        AclOperation::Describe => sys::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_DESCRIBE,
        AclOperation::ClusterAction => {
            sys::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_CLUSTER_ACTION
        }
        AclOperation::DescribeConfigs => {
            sys::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_DESCRIBE_CONFIGS
        }
        AclOperation::AlterConfigs => {
            sys::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_ALTER_CONFIGS
        }
        AclOperation::IdempotentWrite => {
            sys::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_IDEMPOTENT_WRITE
        }
    }
}

const fn native_acl_permission(value: AclPermissionType) -> sys::rd_kafka_AclPermissionType_t {
    match value {
        AclPermissionType::Any => {
            sys::rd_kafka_AclPermissionType_t::RD_KAFKA_ACL_PERMISSION_TYPE_ANY
        }
        AclPermissionType::Deny => {
            sys::rd_kafka_AclPermissionType_t::RD_KAFKA_ACL_PERMISSION_TYPE_DENY
        }
        AclPermissionType::Allow => {
            sys::rd_kafka_AclPermissionType_t::RD_KAFKA_ACL_PERMISSION_TYPE_ALLOW
        }
    }
}

fn acl_resource_from_native(value: sys::rd_kafka_ResourceType_t) -> Result<AclResourceType> {
    match value {
        sys::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_ANY => Ok(AclResourceType::Any),
        sys::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_TOPIC => Ok(AclResourceType::Topic),
        sys::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_GROUP => Ok(AclResourceType::Group),
        sys::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_BROKER => Ok(AclResourceType::Cluster),
        sys::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_TRANSACTIONAL_ID => {
            Ok(AclResourceType::TransactionalId)
        }
        _ => Err(Error::Unsupported(
            "librdkafka returned an unknown ACL resource type".into(),
        )),
    }
}

fn acl_pattern_from_native(value: sys::rd_kafka_ResourcePatternType_t) -> Result<AclPatternType> {
    match value {
        sys::rd_kafka_ResourcePatternType_t::RD_KAFKA_RESOURCE_PATTERN_ANY => {
            Ok(AclPatternType::Any)
        }
        sys::rd_kafka_ResourcePatternType_t::RD_KAFKA_RESOURCE_PATTERN_MATCH => {
            Ok(AclPatternType::Match)
        }
        sys::rd_kafka_ResourcePatternType_t::RD_KAFKA_RESOURCE_PATTERN_LITERAL => {
            Ok(AclPatternType::Literal)
        }
        sys::rd_kafka_ResourcePatternType_t::RD_KAFKA_RESOURCE_PATTERN_PREFIXED => {
            Ok(AclPatternType::Prefixed)
        }
        _ => Err(Error::Unsupported(
            "librdkafka returned an unknown ACL pattern type".into(),
        )),
    }
}

fn acl_operation_from_native(value: sys::rd_kafka_AclOperation_t) -> Result<AclOperation> {
    match value {
        sys::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_ANY => Ok(AclOperation::Any),
        sys::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_ALL => Ok(AclOperation::All),
        sys::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_READ => Ok(AclOperation::Read),
        sys::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_WRITE => Ok(AclOperation::Write),
        sys::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_CREATE => Ok(AclOperation::Create),
        sys::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_DELETE => Ok(AclOperation::Delete),
        sys::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_ALTER => Ok(AclOperation::Alter),
        sys::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_DESCRIBE => Ok(AclOperation::Describe),
        sys::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_CLUSTER_ACTION => {
            Ok(AclOperation::ClusterAction)
        }
        sys::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_DESCRIBE_CONFIGS => {
            Ok(AclOperation::DescribeConfigs)
        }
        sys::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_ALTER_CONFIGS => {
            Ok(AclOperation::AlterConfigs)
        }
        sys::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_IDEMPOTENT_WRITE => {
            Ok(AclOperation::IdempotentWrite)
        }
        _ => Err(Error::Unsupported(
            "librdkafka returned an unknown ACL operation".into(),
        )),
    }
}

fn acl_permission_from_native(
    value: sys::rd_kafka_AclPermissionType_t,
) -> Result<AclPermissionType> {
    match value {
        sys::rd_kafka_AclPermissionType_t::RD_KAFKA_ACL_PERMISSION_TYPE_ANY => {
            Ok(AclPermissionType::Any)
        }
        sys::rd_kafka_AclPermissionType_t::RD_KAFKA_ACL_PERMISSION_TYPE_DENY => {
            Ok(AclPermissionType::Deny)
        }
        sys::rd_kafka_AclPermissionType_t::RD_KAFKA_ACL_PERMISSION_TYPE_ALLOW => {
            Ok(AclPermissionType::Allow)
        }
        _ => Err(Error::Unsupported(
            "librdkafka returned an unknown ACL permission type".into(),
        )),
    }
}

/// Lists every committed offset for one consumer group.
pub fn list_consumer_group_offsets(
    client: *mut sys::rd_kafka_t,
    group: &str,
    timeout_ms: i32,
) -> Result<Vec<GroupOffsetEntry>> {
    let group = CString::new(group)
        .map_err(|_| Error::Usage("consumer group contains a NUL byte".into()))?;
    let request =
        unsafe { sys::rd_kafka_ListConsumerGroupOffsets_new(group.as_ptr(), ptr::null()) };
    if request.is_null() {
        return Err(Error::Config(
            "failed to create ListConsumerGroupOffsets request".into(),
        ));
    }
    let request = ListGroupOffsets(request);
    let mut requests = [request.0];
    let queue = queue(client)?;
    let options = options(
        client,
        sys::rd_kafka_admin_op_t::RD_KAFKA_ADMIN_OP_LISTCONSUMERGROUPOFFSETS,
        timeout_ms,
    )?;
    unsafe {
        sys::rd_kafka_ListConsumerGroupOffsets(
            client,
            requests.as_mut_ptr(),
            requests.len(),
            options.0,
            queue.0,
        );
    }
    let event = poll(&queue, timeout_ms)?;
    let result = unsafe { sys::rd_kafka_event_ListConsumerGroupOffsets_result(event.0) };
    if result.is_null() {
        return Err(Error::Config(
            "broker returned an invalid ListConsumerGroupOffsets response".into(),
        ));
    }
    let mut group_count = 0;
    let groups = unsafe {
        sys::rd_kafka_ListConsumerGroupOffsets_result_groups(result, &raw mut group_count)
    };
    if group_count != 1 || groups.is_null() {
        return Err(Error::Config(format!(
            "expected one consumer-group offset result, received {group_count}"
        )));
    }
    let group_result = unsafe { *groups };
    let group_error = unsafe { sys::rd_kafka_group_result_error(group_result) };
    if !group_error.is_null() {
        return Err(Error::Config(unsafe {
            c_string(sys::rd_kafka_error_string(group_error))
        }));
    }
    let partitions = unsafe { sys::rd_kafka_group_result_partitions(group_result) };
    if partitions.is_null() {
        return Ok(Vec::new());
    }
    let count = usize::try_from(unsafe { (*partitions).cnt })
        .map_err(|_| Error::Config("invalid partition count in offset response".into()))?;
    let mut rows = Vec::with_capacity(count);
    for index in 0..count {
        let partition = unsafe { &*(*partitions).elems.add(index) };
        rows.push(GroupOffsetEntry {
            topic: unsafe { c_string(partition.topic) },
            partition: partition.partition,
            offset: partition.offset,
            leader_epoch: match unsafe { sys::rd_kafka_topic_partition_get_leader_epoch(partition) }
            {
                epoch if epoch >= 0 => Some(epoch),
                _ => None,
            },
            error: if partition.err == sys::rd_kafka_resp_err_t::RD_KAFKA_RESP_ERR_NO_ERROR {
                None
            } else {
                Some(unsafe { c_string(sys::rd_kafka_err2str(partition.err)) })
            },
        });
    }
    Ok(rows)
}

/// Alters committed offsets for one consumer group through librdkafka Admin API.
pub fn alter_consumer_group_offsets(
    client: *mut sys::rd_kafka_t,
    group: &str,
    offsets: &[(String, i32, i64)],
    timeout_ms: i32,
) -> Result<()> {
    let group = CString::new(group)
        .map_err(|_| Error::Usage("consumer group contains a NUL byte".into()))?;
    let capacity = i32::try_from(offsets.len())
        .map_err(|_| Error::Usage("too many consumer-group offsets".into()))?;
    let list = unsafe { sys::rd_kafka_topic_partition_list_new(capacity) };
    if list.is_null() {
        return Err(Error::Config(
            "failed to allocate consumer-group offset list".into(),
        ));
    }
    let list = PartitionList(list);
    for (topic, partition, offset) in offsets {
        let topic = CString::new(topic.as_str())
            .map_err(|_| Error::Usage("topic contains a NUL byte".into()))?;
        let element =
            unsafe { sys::rd_kafka_topic_partition_list_add(list.0, topic.as_ptr(), *partition) };
        if element.is_null() {
            return Err(Error::Config(
                "failed to add a consumer-group offset".into(),
            ));
        }
        unsafe { (*element).offset = *offset };
    }
    let request = unsafe { sys::rd_kafka_AlterConsumerGroupOffsets_new(group.as_ptr(), list.0) };
    if request.is_null() {
        return Err(Error::Config(
            "failed to construct AlterConsumerGroupOffsets request".into(),
        ));
    }
    let request = AlterGroupOffsets(request);
    let mut requests = [request.0];
    let queue = queue(client)?;
    let options = options(
        client,
        sys::rd_kafka_admin_op_t::RD_KAFKA_ADMIN_OP_ALTERCONSUMERGROUPOFFSETS,
        timeout_ms,
    )?;
    unsafe {
        sys::rd_kafka_AlterConsumerGroupOffsets(
            client,
            requests.as_mut_ptr(),
            requests.len(),
            options.0,
            queue.0,
        );
    }
    let event = poll(&queue, timeout_ms)?;
    let result = unsafe { sys::rd_kafka_event_AlterConsumerGroupOffsets_result(event.0) };
    if result.is_null() {
        return Err(Error::Config(
            "broker returned an invalid AlterConsumerGroupOffsets response".into(),
        ));
    }
    let mut count = 0;
    let groups =
        unsafe { sys::rd_kafka_AlterConsumerGroupOffsets_result_groups(result, &raw mut count) };
    if count > 0 && groups.is_null() {
        return Err(Error::Config(
            "broker returned a null altered group-offset array".into(),
        ));
    }
    for index in 0..count {
        let group = unsafe { *groups.add(index) };
        if group.is_null() {
            return Err(Error::Config(
                "broker returned a null altered group-offset result".into(),
            ));
        }
        let error = unsafe { sys::rd_kafka_group_result_error(group) };
        if native_error_failed(error) {
            return Err(Error::Config(unsafe {
                c_string(sys::rd_kafka_error_string(error))
            }));
        }
        let partitions = unsafe { sys::rd_kafka_group_result_partitions(group) };
        if !partitions.is_null() {
            let partition_count = usize::try_from(unsafe { (*partitions).cnt })
                .map_err(|_| Error::Config("invalid altered offset count".into()))?;
            for partition_index in 0..partition_count {
                let partition = unsafe { &*(*partitions).elems.add(partition_index) };
                if partition.err != sys::rd_kafka_resp_err_t::RD_KAFKA_RESP_ERR_NO_ERROR {
                    return Err(Error::Config(unsafe {
                        c_string(sys::rd_kafka_err2str(partition.err))
                    }));
                }
            }
        }
    }
    Ok(())
}

/// Triggers preferred or unclean leader election for selected partitions or all partitions.
pub fn elect_leaders(
    client: *mut sys::rd_kafka_t,
    unclean: bool,
    partitions: Option<&[(String, i32)]>,
    timeout_ms: i32,
) -> Result<Vec<ElectionEntry>> {
    let partitions = if let Some(partitions) = partitions {
        let capacity = i32::try_from(partitions.len())
            .map_err(|_| Error::Usage("too many leader election targets".into()))?;
        let list = unsafe { sys::rd_kafka_topic_partition_list_new(capacity) };
        if list.is_null() {
            return Err(Error::Config("failed to allocate partition list".into()));
        }
        let list = PartitionList(list);
        for (topic, partition) in partitions {
            let topic = CString::new(topic.as_str())
                .map_err(|_| Error::Usage("topic contains a NUL byte".into()))?;
            unsafe { sys::rd_kafka_topic_partition_list_add(list.0, topic.as_ptr(), *partition) };
        }
        Some(list)
    } else {
        None
    };
    let election_type = if unclean {
        sys::rd_kafka_ElectionType_t::RD_KAFKA_ELECTION_TYPE_UNCLEAN
    } else {
        sys::rd_kafka_ElectionType_t::RD_KAFKA_ELECTION_TYPE_PREFERRED
    };
    let election = unsafe {
        sys::rd_kafka_ElectLeaders_new(
            election_type,
            partitions.as_ref().map_or(ptr::null_mut(), |list| list.0),
        )
    };
    if election.is_null() {
        return Err(Error::Config(
            "failed to construct leader election request".into(),
        ));
    }
    let election = Election(election);
    let queue = queue(client)?;
    let options = options(
        client,
        sys::rd_kafka_admin_op_t::RD_KAFKA_ADMIN_OP_ELECTLEADERS,
        timeout_ms,
    )?;
    unsafe { sys::rd_kafka_ElectLeaders(client, election.0, options.0, queue.0) };
    let event = poll(&queue, timeout_ms)?;
    let result = unsafe { sys::rd_kafka_event_ElectLeaders_result(event.0) };
    if result.is_null() {
        return Err(Error::Config(
            "broker returned an invalid ElectLeaders response".into(),
        ));
    }
    let mut count = 0;
    let responses = unsafe { sys::rd_kafka_ElectLeaders_result_partitions(result, &raw mut count) };
    let mut rows = Vec::with_capacity(count);
    for index in 0..count {
        let response = unsafe { *responses.add(index) };
        let topic_partition = unsafe { sys::rd_kafka_topic_partition_result_partition(response) };
        let error = unsafe { sys::rd_kafka_topic_partition_result_error(response) };
        let noop = !error.is_null() && is_election_noop(unsafe { sys::rd_kafka_error_code(error) });
        rows.push(ElectionEntry {
            topic: if topic_partition.is_null() {
                String::new()
            } else {
                unsafe { c_string((*topic_partition).topic) }
            },
            partition: if topic_partition.is_null() {
                -1
            } else {
                unsafe { (*topic_partition).partition }
            },
            error: if error.is_null() || noop {
                None
            } else {
                Some(unsafe { c_string(sys::rd_kafka_error_string(error)) })
            },
            noop,
        });
    }
    Ok(rows)
}

const fn is_election_noop(code: sys::rd_kafka_resp_err_t) -> bool {
    matches!(
        code,
        sys::rd_kafka_resp_err_t::RD_KAFKA_RESP_ERR_ELECTION_NOT_NEEDED
    )
}

/// Applies SET and DELETE operations through Kafka's `IncrementalAlterConfigs` API.
pub fn incremental_alter_config(
    client: *mut sys::rd_kafka_t,
    resource_type: sys::rd_kafka_ResourceType_t,
    resource_name: &str,
    entries: &[(String, String)],
    deletes: &[String],
    timeout_ms: i32,
) -> Result<()> {
    let name = CString::new(resource_name)
        .map_err(|_| Error::Usage("resource name contains a NUL byte".into()))?;
    let resource = unsafe { sys::rd_kafka_ConfigResource_new(resource_type, name.as_ptr()) };
    if resource.is_null() {
        return Err(Error::Config("failed to create config resource".into()));
    }
    let resource = ConfigResource(resource);
    for (key, value) in entries {
        let key = CString::new(key.as_str())
            .map_err(|_| Error::Usage("config key contains a NUL byte".into()))?;
        let value = CString::new(value.as_str())
            .map_err(|_| Error::Usage("config value contains a NUL byte".into()))?;
        let error = unsafe {
            sys::rd_kafka_ConfigResource_add_incremental_config(
                resource.0,
                key.as_ptr(),
                sys::rd_kafka_AlterConfigOpType_t::RD_KAFKA_ALTER_CONFIG_OP_TYPE_SET,
                value.as_ptr(),
            )
        };
        if !error.is_null() {
            let message = unsafe { c_string(sys::rd_kafka_error_string(error)) };
            unsafe { sys::rd_kafka_error_destroy(error) };
            return Err(Error::Config(message));
        }
    }
    for key in deletes {
        let key = CString::new(key.as_str())
            .map_err(|_| Error::Usage("config key contains a NUL byte".into()))?;
        let error = unsafe {
            sys::rd_kafka_ConfigResource_add_incremental_config(
                resource.0,
                key.as_ptr(),
                sys::rd_kafka_AlterConfigOpType_t::RD_KAFKA_ALTER_CONFIG_OP_TYPE_DELETE,
                ptr::null(),
            )
        };
        if !error.is_null() {
            let message = unsafe { c_string(sys::rd_kafka_error_string(error)) };
            unsafe { sys::rd_kafka_error_destroy(error) };
            return Err(Error::Config(message));
        }
    }
    let mut resources = [resource.0];
    let queue = queue(client)?;
    let options = options(
        client,
        sys::rd_kafka_admin_op_t::RD_KAFKA_ADMIN_OP_INCREMENTALALTERCONFIGS,
        timeout_ms,
    )?;
    unsafe {
        sys::rd_kafka_IncrementalAlterConfigs(
            client,
            resources.as_mut_ptr(),
            1,
            options.0,
            queue.0,
        );
    }
    let event = poll(&queue, timeout_ms)?;
    let result = unsafe { sys::rd_kafka_event_IncrementalAlterConfigs_result(event.0) };
    if result.is_null() {
        return Err(Error::Config(
            "broker returned an invalid IncrementalAlterConfigs response".into(),
        ));
    }
    let mut count = 0;
    let responses =
        unsafe { sys::rd_kafka_IncrementalAlterConfigs_result_resources(result, &raw mut count) };
    for index in 0..count {
        let response = unsafe { *responses.add(index) };
        let code = unsafe { sys::rd_kafka_ConfigResource_error(response) };
        if code != sys::rd_kafka_resp_err_t::RD_KAFKA_RESP_ERR_NO_ERROR {
            return Err(Error::Config(unsafe {
                c_string(sys::rd_kafka_ConfigResource_error_string(response))
            }));
        }
    }
    Ok(())
}

/// Deletes committed offsets for the selected consumer group partitions.
pub fn delete_group_offsets(
    client: *mut sys::rd_kafka_t,
    group: &str,
    selections: &[(String, Vec<i32>)],
    timeout_ms: i32,
) -> Result<()> {
    let group =
        CString::new(group).map_err(|_| Error::Usage("group contains a NUL byte".into()))?;
    let capacity = i32::try_from(
        selections
            .iter()
            .map(|(_, values)| values.len())
            .sum::<usize>(),
    )
    .map_err(|_| Error::Usage("too many partitions".into()))?;
    let list = unsafe { sys::rd_kafka_topic_partition_list_new(capacity) };
    if list.is_null() {
        return Err(Error::Config("failed to allocate partition list".into()));
    }
    let list = PartitionList(list);
    for (topic, partitions) in selections {
        let topic = CString::new(topic.as_str())
            .map_err(|_| Error::Usage("topic contains a NUL byte".into()))?;
        for partition in partitions {
            unsafe { sys::rd_kafka_topic_partition_list_add(list.0, topic.as_ptr(), *partition) };
        }
    }
    let request = unsafe { sys::rd_kafka_DeleteConsumerGroupOffsets_new(group.as_ptr(), list.0) };
    if request.is_null() {
        return Err(Error::Config(
            "failed to construct group offset deletion".into(),
        ));
    }
    let request = DeleteGroupOffsets(request);
    let mut requests = [request.0];
    let queue = queue(client)?;
    let options = options(
        client,
        sys::rd_kafka_admin_op_t::RD_KAFKA_ADMIN_OP_DELETECONSUMERGROUPOFFSETS,
        timeout_ms,
    )?;
    unsafe {
        sys::rd_kafka_DeleteConsumerGroupOffsets(
            client,
            requests.as_mut_ptr(),
            1,
            options.0,
            queue.0,
        );
    }
    let event = poll(&queue, timeout_ms)?;
    let result = unsafe { sys::rd_kafka_event_DeleteConsumerGroupOffsets_result(event.0) };
    if result.is_null() {
        return Err(Error::Config(
            "broker returned an invalid DeleteConsumerGroupOffsets response".into(),
        ));
    }
    let mut count = 0;
    let groups =
        unsafe { sys::rd_kafka_DeleteConsumerGroupOffsets_result_groups(result, &raw mut count) };
    for index in 0..count {
        let group = unsafe { *groups.add(index) };
        let error = unsafe { sys::rd_kafka_group_result_error(group) };
        if !error.is_null() {
            return Err(Error::Config(unsafe {
                c_string(sys::rd_kafka_error_string(error))
            }));
        }
    }
    Ok(())
}

fn c_buffer(buffer: &[c_char]) -> String {
    unsafe { CStr::from_ptr(buffer.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

unsafe fn c_string(pointer: *const c_char) -> String {
    if pointer.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(pointer) }
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn election_not_needed_should_be_a_successful_noop() {
        assert!(is_election_noop(
            sys::rd_kafka_resp_err_t::RD_KAFKA_RESP_ERR_ELECTION_NOT_NEEDED
        ));
        assert!(!is_election_noop(
            sys::rd_kafka_resp_err_t::RD_KAFKA_RESP_ERR_CLUSTER_AUTHORIZATION_FAILED
        ));
    }

    #[test]
    fn acl_types_should_match_librdkafka_discriminants() {
        assert_eq!(
            native_acl_resource(AclResourceType::Cluster),
            sys::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_BROKER
        );
        assert_eq!(
            native_acl_pattern(AclPatternType::Literal),
            sys::rd_kafka_ResourcePatternType_t::RD_KAFKA_RESOURCE_PATTERN_LITERAL
        );
        assert_eq!(
            native_acl_operation(AclOperation::IdempotentWrite),
            sys::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_IDEMPOTENT_WRITE
        );
        assert_eq!(
            acl_pattern_from_native(
                sys::rd_kafka_ResourcePatternType_t::RD_KAFKA_RESOURCE_PATTERN_MATCH
            )
            .expect("match pattern"),
            AclPatternType::Match
        );
    }

    #[test]
    fn list_offset_specs_should_use_kafka_protocol_sentinels() {
        assert_eq!(
            [
                list_offset_spec_value(ListOffsetSpec::EarliestLocal),
                list_offset_spec_value(ListOffsetSpec::LatestTiered),
                list_offset_spec_value(ListOffsetSpec::EarliestPendingUpload),
            ],
            [-4, -5, -6]
        );
    }

    #[test]
    fn tiered_list_offset_specs_should_report_librdkafka_boundary() {
        for spec in [
            ListOffsetSpec::EarliestLocal,
            ListOffsetSpec::LatestTiered,
            ListOffsetSpec::EarliestPendingUpload,
        ] {
            let error = ensure_supported_list_offset_spec(spec).expect_err("unsupported spec");
            assert!(error.to_string().contains("librdkafka 2.12"));
        }
        ensure_supported_list_offset_spec(ListOffsetSpec::MaxTimestamp)
            .expect("supported offset spec");
    }
}
