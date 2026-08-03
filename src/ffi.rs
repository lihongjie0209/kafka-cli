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

/// A committed consumer-group offset returned by librdkafka.
#[derive(Debug)]
pub struct GroupOffsetEntry {
    pub topic: String,
    pub partition: i32,
    pub offset: i64,
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
            error: if partition.err == sys::rd_kafka_resp_err_t::RD_KAFKA_RESP_ERR_NO_ERROR {
                None
            } else {
                Some(unsafe { c_string(sys::rd_kafka_err2str(partition.err)) })
            },
        });
    }
    Ok(rows)
}

/// Triggers preferred or unclean leader election for one partition or all partitions.
pub fn elect_leaders(
    client: *mut sys::rd_kafka_t,
    unclean: bool,
    partition: Option<(&str, i32)>,
    timeout_ms: i32,
) -> Result<Vec<ElectionEntry>> {
    let topic = partition
        .map(|(topic, _)| CString::new(topic))
        .transpose()
        .map_err(|_| Error::Usage("topic contains a NUL byte".into()))?;
    let partitions = if let (Some((_, partition)), Some(topic)) = (partition, topic.as_ref()) {
        let list = unsafe { sys::rd_kafka_topic_partition_list_new(1) };
        if list.is_null() {
            return Err(Error::Config("failed to allocate partition list".into()));
        }
        unsafe { sys::rd_kafka_topic_partition_list_add(list, topic.as_ptr(), partition) };
        Some(PartitionList(list))
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
    topic: &str,
    partitions: &[i32],
    timeout_ms: i32,
) -> Result<()> {
    let group =
        CString::new(group).map_err(|_| Error::Usage("group contains a NUL byte".into()))?;
    let topic =
        CString::new(topic).map_err(|_| Error::Usage("topic contains a NUL byte".into()))?;
    let capacity =
        i32::try_from(partitions.len()).map_err(|_| Error::Usage("too many partitions".into()))?;
    let list = unsafe { sys::rd_kafka_topic_partition_list_new(capacity) };
    if list.is_null() {
        return Err(Error::Config("failed to allocate partition list".into()));
    }
    let list = PartitionList(list);
    for partition in partitions {
        unsafe { sys::rd_kafka_topic_partition_list_add(list.0, topic.as_ptr(), *partition) };
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
}
