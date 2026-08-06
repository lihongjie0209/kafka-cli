//! Kafka log segment dumper (`kafka-dump-log.sh` / `DumpLogSegments`).

use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use bytes::Bytes;
use krafka::protocol::{Compression, LazyRecordBatch};

use crate::error::{Error, Result};

const RECORD_INDENT: &str = "|";
const DEFAULT_MAX_MESSAGE_SIZE: i32 = 5 * 1024 * 1024;

/// Options for dumping Kafka log segment files.
#[derive(Debug, Clone)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "DumpLogSegments exposes independent diagnostic mode flags"
)]
pub struct DumpLogOptions {
    pub files: Vec<PathBuf>,
    pub print_data_log: bool,
    pub verify_index_only: bool,
    pub index_sanity_check: bool,
    pub max_message_size: i32,
    pub max_bytes: i32,
    pub deep_iteration: bool,
    pub skip_record_metadata: bool,
    pub key_decoder: Option<String>,
    pub value_decoder: Option<String>,
    pub offsets_decoder: bool,
    pub transaction_log_decoder: bool,
    pub cluster_metadata_decoder: bool,
    pub remote_log_metadata_decoder: bool,
    pub share_group_state_decoder: bool,
}

impl DumpLogOptions {
    const fn should_print_data_log(&self) -> bool {
        self.print_data_log
            || self.offsets_decoder
            || self.transaction_log_decoder
            || self.cluster_metadata_decoder
            || self.remote_log_metadata_decoder
            || self.share_group_state_decoder
            || self.key_decoder.is_some()
            || self.value_decoder.is_some()
    }

    const fn is_deep_iteration(&self) -> bool {
        self.deep_iteration || self.should_print_data_log()
    }
}

/// Dump Kafka log, index, and time-index segment files.
pub fn dump_log_segments(opts: &DumpLogOptions) -> Result<()> {
    validate_options(opts)?;
    let mut index_mismatches: BTreeMap<String, BTreeMap<i64, i64>> = BTreeMap::new();
    let mut non_consecutive: BTreeMap<String, BTreeMap<i64, i64>> = BTreeMap::new();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let stderr = std::io::stderr();
    let mut err = stderr.lock();

    for path in &opts.files {
        writeln!(out, "Dumping {}", path.display())?;
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            writeln!(err, "Ignoring unknown file {}", path.display())?;
            continue;
        };
        let Some((_, suffix)) = name.rsplit_once('.') else {
            writeln!(err, "Ignoring unknown file {}", path.display())?;
            continue;
        };

        match suffix {
            "log" => dump_log_file(
                path,
                opts.should_print_data_log(),
                opts.is_deep_iteration(),
                opts.skip_record_metadata,
                opts.max_bytes,
                &mut non_consecutive,
                &mut out,
            )?,
            // Kafka binary bootstrap.checkpoint is record-batch based; text/JSON residuals are not.
            "checkpoint" => {
                if is_native_bootstrap_residual(name) || file_looks_like_text(path)? {
                    dump_non_binary_bootstrap_file(path, name, &mut out, &mut err)?;
                } else {
                    dump_log_file(
                        path,
                        opts.should_print_data_log(),
                        opts.is_deep_iteration(),
                        opts.skip_record_metadata,
                        opts.max_bytes,
                        &mut non_consecutive,
                        &mut out,
                    )?;
                }
            }
            "json" if is_native_bootstrap_residual(name) || file_looks_like_text(path)? => {
                dump_non_binary_bootstrap_file(path, name, &mut out, &mut err)?;
            }
            "index" => dump_offset_index(
                path,
                opts.index_sanity_check,
                opts.verify_index_only,
                opts.max_message_size,
                &mut index_mismatches,
                &mut out,
            )?,
            "timeindex" => dump_time_index(
                path,
                opts.index_sanity_check,
                opts.verify_index_only,
                &mut out,
                &mut err,
            )?,
            "txnindex" => dump_txnindex(path, &mut out)?,
            // KRaft/log snapshots named `<offset>-<epoch>.snapshot` embed record batches.
            // Producer-state snapshots are typically `<offset>.snapshot` (no epoch dash).
            "snapshot" if is_kraft_snapshot_name(name) => dump_log_file(
                path,
                opts.should_print_data_log(),
                opts.is_deep_iteration(),
                opts.skip_record_metadata,
                opts.max_bytes,
                &mut non_consecutive,
                &mut out,
            )?,
            "snapshot" => dump_producer_snapshot(path, &mut out, &mut err)?,
            _ => writeln!(err, "Ignoring unknown file {}", path.display())?,
        }
    }

    for (file, mismatches) in &index_mismatches {
        writeln!(err, "Mismatches in :{file}")?;
        for (index_offset, log_offset) in mismatches.iter().rev() {
            writeln!(
                err,
                "  Index offset: {index_offset}, log offset: {log_offset}"
            )?;
        }
    }
    for (file, pairs) in &non_consecutive {
        writeln!(err, "Non-consecutive offsets in {file}")?;
        for (prev, next) in pairs {
            writeln!(err, "  {prev} is followed by {next}")?;
        }
    }
    Ok(())
}

fn validate_options(opts: &DumpLogOptions) -> Result<()> {
    if opts.files.is_empty() {
        return Err(Error::Usage("Missing required argument \"files\"".into()));
    }
    if opts.max_message_size <= 0 {
        return Err(Error::Usage("--max-message-size must be positive".into()));
    }
    if opts.max_bytes <= 0 {
        return Err(Error::Usage("--max-bytes must be positive".into()));
    }
    for (flag, enabled) in [
        ("--offsets-decoder", opts.offsets_decoder),
        ("--transaction-log-decoder", opts.transaction_log_decoder),
        ("--cluster-metadata-decoder", opts.cluster_metadata_decoder),
        (
            "--remote-log-metadata-decoder",
            opts.remote_log_metadata_decoder,
        ),
        (
            "--share-group-state-decoder",
            opts.share_group_state_decoder,
        ),
    ] {
        if enabled {
            return Err(Error::Unsupported(format!(
                "{flag} requires Kafka coordinator record codecs not available natively"
            )));
        }
    }
    for (name, class) in [
        ("key", opts.key_decoder.as_deref()),
        ("value", opts.value_decoder.as_deref()),
    ] {
        if let Some(class) = class
            && !is_string_decoder(class)
        {
            return Err(Error::Unsupported(format!(
                "custom {name} decoder class '{class}' is not supported natively; only StringDecoder is available"
            )));
        }
    }
    Ok(())
}

fn is_string_decoder(class: &str) -> bool {
    class == "kafka.serializer.StringDecoder"
        || class == "org.apache.kafka.common.serialization.StringDeserializer"
        || class.ends_with(".StringDecoder")
        || class.eq_ignore_ascii_case("string")
}

fn dump_log_file(
    path: &Path,
    print_contents: bool,
    deep: bool,
    skip_record_metadata: bool,
    max_bytes: i32,
    non_consecutive: &mut BTreeMap<String, BTreeMap<i64, i64>>,
    out: &mut impl Write,
) -> Result<()> {
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        let extension = Path::new(name)
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default();
        if extension.eq_ignore_ascii_case("log") {
            if let Some(base) = name.split('.').next().and_then(|s| s.parse::<i64>().ok()) {
                writeln!(out, "Log starting offset: {base}")?;
            }
        } else if extension.eq_ignore_ascii_case("checkpoint") {
            writeln!(out, "Kafka binary bootstrap checkpoint (record batches)")?;
        } else if extension.eq_ignore_ascii_case("snapshot") {
            writeln!(out, "Snapshot file: {name}")?;
        }
    }

    let mut file = File::open(path)?;
    let file_len = file.metadata()?.len();
    let limit = u64::try_from(max_bytes).unwrap_or(u64::MAX).min(file_len);
    let mut buffer = vec![0_u8; usize::try_from(limit).unwrap_or(usize::MAX)];
    let mut read_total = 0_usize;
    while read_total < buffer.len() {
        match file.read(&mut buffer[read_total..])? {
            0 => break,
            n => read_total += n,
        }
    }
    buffer.truncate(read_total);

    let mut cursor = 0_usize;
    let mut last_offset = -1_i64;
    while cursor < buffer.len() {
        let remaining = &buffer[cursor..];
        if remaining.len() < 12 {
            break;
        }
        let batch_length = i32::from_be_bytes(remaining[8..12].try_into().unwrap_or([0; 4]));
        if batch_length < 49 {
            break;
        }
        let total = 12 + usize::try_from(batch_length).unwrap_or(usize::MAX);
        if remaining.len() < total {
            break;
        }
        let batch_bytes = &remaining[..total];
        let position = i64::try_from(cursor).unwrap_or(i64::MAX);
        match decode_and_print_batch(
            batch_bytes,
            position,
            deep,
            print_contents,
            skip_record_metadata,
            path,
            &mut last_offset,
            non_consecutive,
            out,
        ) {
            Ok(()) => cursor += total,
            Err(error) => {
                writeln!(out, "error decoding batch at position {position}: {error}")?;
                break;
            }
        }
    }
    let trailing = buffer.len().saturating_sub(cursor);
    if trailing > 0 && u64::try_from(cursor).unwrap_or(0) < limit {
        writeln!(
            out,
            "Found invalid bytes at the end of {} starting at byte offset {cursor}: {trailing} bytes remaining",
            path.display()
        )?;
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "batch dump mirrors DumpLogSegments print path"
)]
fn decode_and_print_batch(
    batch_bytes: &[u8],
    position: i64,
    deep: bool,
    print_contents: bool,
    skip_record_metadata: bool,
    path: &Path,
    last_offset: &mut i64,
    non_consecutive: &mut BTreeMap<String, BTreeMap<i64, i64>>,
    out: &mut impl Write,
) -> Result<()> {
    let mut buf = batch_bytes;
    let lazy = LazyRecordBatch::decode(&mut buf)
        .map_err(|error| Error::Config(format!("failed to decode record batch: {error}")))?;
    let last_batch_offset = lazy.base_offset + i64::from(lazy.last_offset_delta);
    let count = lazy.records_count;
    let is_control = lazy.attributes.is_control_batch;
    let is_transactional = lazy.attributes.is_transactional;
    let compress = compression_name(lazy.attributes.compression);
    let ts_type = if lazy.attributes.to_i16() & 0x08 != 0 {
        "LogAppendTime"
    } else {
        "CreateTime"
    };
    let crc = u32::from_be_bytes(batch_bytes[17..21].try_into().unwrap_or([0; 4]));
    // Decode already validated the batch CRC; treat successful decode as valid.
    let is_valid = true;
    let last_sequence = if lazy.base_sequence >= 0 {
        lazy.base_sequence.saturating_add(lazy.last_offset_delta)
    } else {
        -1
    };

    write!(
        out,
        "baseOffset: {} lastOffset: {last_batch_offset} count: {count} baseSequence: {} lastSequence: {last_sequence} producerId: {} producerEpoch: {} partitionLeaderEpoch: {} isTransactional: {is_transactional} isControl: {is_control} deleteHorizonMs: Optional.empty",
        lazy.base_offset,
        lazy.base_sequence,
        lazy.producer_id,
        lazy.producer_epoch,
        lazy.partition_leader_epoch,
    )?;
    writeln!(
        out,
        " position: {position} {ts_type}: {} size: {} magic: 2 compresscodec: {compress} crc: {crc} isvalid: {is_valid}",
        lazy.max_timestamp,
        batch_bytes.len(),
    )?;

    if !deep {
        return Ok(());
    }

    let records = lazy
        .decode_all()
        .map_err(|error| Error::Config(format!("failed to decode records in batch: {error}")))?;
    for record in records {
        let offset = lazy.base_offset + i64::from(record.offset_delta);
        if *last_offset >= 0 && offset != *last_offset + 1 {
            non_consecutive
                .entry(path.display().to_string())
                .or_default()
                .insert(*last_offset, offset);
        }
        *last_offset = offset;

        if !skip_record_metadata {
            let timestamp = lazy.base_timestamp + record.timestamp_delta;
            let key_size = record
                .key
                .as_ref()
                .map_or(-1, |k| i32::try_from(k.len()).unwrap_or(i32::MAX));
            let value_size = record
                .value
                .as_ref()
                .map_or(-1, |v| i32::try_from(v.len()).unwrap_or(i32::MAX));
            let sequence = if lazy.base_sequence >= 0 {
                lazy.base_sequence.saturating_add(record.offset_delta)
            } else {
                -1
            };
            let headers = record
                .headers
                .iter()
                .map(|header| String::from_utf8_lossy(&header.key).into_owned())
                .collect::<Vec<_>>()
                .join(",");
            write!(
                out,
                "{RECORD_INDENT} offset: {offset} {ts_type}: {timestamp} keySize: {key_size} valueSize: {value_size} sequence: {sequence} headerKeys: [{headers}]"
            )?;
            if is_control {
                write!(
                    out,
                    "{}",
                    format_control_record(record.key.as_ref(), record.value.as_ref())
                )?;
            }
        }

        if print_contents && !is_control {
            let prefix = if skip_record_metadata {
                format!("{RECORD_INDENT} ")
            } else {
                " ".into()
            };
            let key = decode_string_field(record.key.as_ref());
            let value = decode_string_field(record.value.as_ref());
            write!(out, "{prefix}key: {key} payload: {value}")?;
        }
        writeln!(out)?;
    }
    Ok(())
}

/// Format control-record key/value similar to Kafka `DumpLogSegments.printControlRecord`.
///
/// Key schema is always `Version:i16 Type:i16`. Known types:
/// - `ABORT(0)` / `COMMIT(1)`: `EndTxnMarker` value `Version:i16 CoordinatorEpoch:i32`
/// - `LEADER_CHANGE(2)`, `SNAPSHOT_HEADER(3)`, `SNAPSHOT_FOOTER(4)`, `KRAFT_VERSION(5)`,
///   `KRAFT_VOTERS(6)`: named type plus best-effort version-prefixed field dump
fn format_control_record(key: Option<&Bytes>, value: Option<&Bytes>) -> String {
    let Some(key) = key else {
        return " controlType: control-record(missing-key)".into();
    };
    if key.len() < 4 {
        return format!(
            " controlType: control-record(invalid-key-len={})",
            key.len()
        );
    }
    let key_version = i16::from_be_bytes([key[0], key[1]]);
    let type_id = i16::from_be_bytes([key[2], key[3]]);
    let type_name = control_record_type_name(type_id);

    match type_id {
        0 | 1 => {
            // EndTransactionMarker
            let epoch = value.and_then(parse_end_txn_coordinator_epoch);
            epoch.map_or_else(
                || {
                    format!(
                        " endTxnMarker: {type_name} (unparseable value; keyVersion={key_version})"
                    )
                },
                |coordinator_epoch| {
                    format!(" endTxnMarker: {type_name} coordinatorEpoch: {coordinator_epoch}")
                },
            )
        }
        2 => format_named_control("LeaderChange", type_id, key_version, value),
        3 => format_named_control("SnapshotHeader", type_id, key_version, value),
        4 => format_named_control("SnapshotFooter", type_id, key_version, value),
        5 => format_named_control("KRaftVersion", type_id, key_version, value),
        6 => format_named_control("KRaftVoters", type_id, key_version, value),
        _ => format!(" controlType: {type_name}({type_id})"),
    }
}

const fn control_record_type_name(type_id: i16) -> &'static str {
    match type_id {
        0 => "ABORT",
        1 => "COMMIT",
        2 => "LEADER_CHANGE",
        3 => "SNAPSHOT_HEADER",
        4 => "SNAPSHOT_FOOTER",
        5 => "KRAFT_VERSION",
        6 => "KRAFT_VOTERS",
        _ => "UNKNOWN",
    }
}

fn parse_end_txn_coordinator_epoch(value: &Bytes) -> Option<i32> {
    // Version-prefixed EndTxnMarker: i16 version + i32 coordinatorEpoch
    if value.len() < 6 {
        return None;
    }
    let version = i16::from_be_bytes([value[0], value[1]]);
    if version < 0 {
        return None;
    }
    Some(i32::from_be_bytes([value[2], value[3], value[4], value[5]]))
}

fn format_named_control(
    label: &str,
    type_id: i16,
    key_version: i16,
    value: Option<&Bytes>,
) -> String {
    match value {
        None => format!(" {label}: typeId={type_id} keyVersion={key_version} value=null"),
        Some(bytes) if bytes.is_empty() => {
            format!(" {label}: typeId={type_id} keyVersion={key_version} valueBytes=0")
        }
        Some(bytes) if bytes.len() >= 2 => {
            let value_version = i16::from_be_bytes([bytes[0], bytes[1]]);
            // SnapshotHeaderRecord commonly carries lastContainedLogTimestamp after version.
            if type_id == 3 && bytes.len() >= 10 {
                let ts = i64::from_be_bytes(bytes[2..10].try_into().unwrap_or([0; 8]));
                return format!(
                    " {label} version: {value_version} lastContainedLogTimestamp: {ts}"
                );
            }
            // LeaderChangeMessage: version + leaderId (i32) is the stable prefix.
            if type_id == 2 && bytes.len() >= 6 {
                let leader_id = i32::from_be_bytes(bytes[2..6].try_into().unwrap_or([0; 4]));
                return format!(
                    " {label}: version={value_version} leaderId={leader_id} valueBytes={}",
                    bytes.len()
                );
            }
            // KRaftVersionRecord: version + kRaftVersion (i16)
            if type_id == 5 && bytes.len() >= 4 {
                let kraft_version = i16::from_be_bytes([bytes[2], bytes[3]]);
                return format!(" {label} version: {value_version} kRaftVersion: {kraft_version}");
            }
            format!(
                " {label}: version={value_version} valueBytes={}",
                bytes.len()
            )
        }
        Some(bytes) => format!(
            " {label}: typeId={type_id} keyVersion={key_version} valueBytes={}",
            bytes.len()
        ),
    }
}

fn decode_string_field(bytes: Option<&Bytes>) -> String {
    bytes.map_or_else(
        || "null".into(),
        |bytes| {
            std::str::from_utf8(bytes)
                .map_or_else(|_| format!("<{} binary bytes>", bytes.len()), str::to_owned)
        },
    )
}

#[expect(
    clippy::missing_const_for_fn,
    reason = "Compression is non-exhaustive so a wildcard arm is required"
)]
fn compression_name(compression: Compression) -> &'static str {
    match compression {
        Compression::None => "none",
        Compression::Gzip => "gzip",
        Compression::Snappy => "snappy",
        Compression::Lz4 => "lz4",
        Compression::Zstd => "zstd",
        _ => "unknown",
    }
}

fn dump_offset_index(
    path: &Path,
    sanity_only: bool,
    verify_only: bool,
    max_message_size: i32,
    mismatches: &mut BTreeMap<String, BTreeMap<i64, i64>>,
    out: &mut impl Write,
) -> Result<()> {
    let base_offset = base_offset_from_name(path)?;
    let mut data = Vec::new();
    File::open(path)?.read_to_end(&mut data)?;
    if data.is_empty() {
        writeln!(out, "{} is empty.", path.display())?;
        return Ok(());
    }
    if data.len() % 8 != 0 {
        return Err(Error::Config(format!(
            "corrupt index {}: length {} is not a multiple of 8",
            path.display(),
            data.len()
        )));
    }
    if sanity_only {
        let mut prev_rel = -1_i32;
        for chunk in data.chunks_exact(8) {
            let rel = i32::from_be_bytes(chunk[0..4].try_into().unwrap());
            let pos = i32::from_be_bytes(chunk[4..8].try_into().unwrap());
            if rel < 0 || pos < 0 || rel < prev_rel {
                return Err(Error::Config(format!(
                    "index {} failed sanity check",
                    path.display()
                )));
            }
            if rel == 0 && prev_rel >= 0 {
                break;
            }
            prev_rel = rel;
        }
        writeln!(out, "{} passed sanity check.", path.display())?;
        return Ok(());
    }

    let log_path = path.with_extension("log");
    let log_bytes = if log_path.exists() {
        let mut bytes = Vec::new();
        File::open(&log_path)?.read_to_end(&mut bytes)?;
        Some(bytes)
    } else {
        None
    };

    for (i, chunk) in data.chunks_exact(8).enumerate() {
        let relative = i32::from_be_bytes(chunk[0..4].try_into().unwrap());
        let position = i32::from_be_bytes(chunk[4..8].try_into().unwrap());
        let offset = base_offset + i64::from(relative);
        if relative == 0 && i > 0 {
            break;
        }
        if let Some(log_bytes) = &log_bytes {
            let pos = usize::try_from(position).unwrap_or(usize::MAX);
            if pos < log_bytes.len() {
                let max_size =
                    usize::try_from(max_message_size).unwrap_or(DEFAULT_MAX_MESSAGE_SIZE as usize);
                let end = (pos + max_size).min(log_bytes.len());
                if let Some(last) = first_batch_last_offset(&log_bytes[pos..end])
                    && last != offset
                {
                    mismatches
                        .entry(path.display().to_string())
                        .or_default()
                        .insert(offset, last);
                }
            }
        }
        if !verify_only {
            writeln!(out, "offset: {offset} position: {position}")?;
        }
    }
    Ok(())
}

fn dump_time_index(
    path: &Path,
    sanity_only: bool,
    verify_only: bool,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<()> {
    let base_offset = base_offset_from_name(path)?;
    let mut data = Vec::new();
    File::open(path)?.read_to_end(&mut data)?;
    if data.is_empty() {
        writeln!(out, "{} is empty.", path.display())?;
        return Ok(());
    }
    if data.len() % 12 != 0 {
        return Err(Error::Config(format!(
            "corrupt time index {}: length {} is not a multiple of 12",
            path.display(),
            data.len()
        )));
    }
    if sanity_only {
        writeln!(out, "{} passed sanity check.", path.display())?;
        return Ok(());
    }
    let mut prev_ts = i64::MIN;
    for chunk in data.chunks_exact(12) {
        let timestamp = i64::from_be_bytes(chunk[0..8].try_into().unwrap());
        let relative = i32::from_be_bytes(chunk[8..12].try_into().unwrap());
        let offset = base_offset + i64::from(relative);
        if timestamp == 0 && relative == 0 && prev_ts != i64::MIN {
            break;
        }
        if timestamp < prev_ts {
            writeln!(
                err,
                "Out of order timestamp in {}: {timestamp} after {prev_ts}",
                path.display()
            )?;
        }
        if !verify_only {
            writeln!(out, "timestamp: {timestamp} offset: {offset}")?;
        }
        prev_ts = timestamp;
    }
    Ok(())
}

fn base_offset_from_name(path: &Path) -> Result<i64> {
    path.file_name()
        .and_then(|n| n.to_str())
        .and_then(|name| name.split('.').next())
        .and_then(|base| base.parse().ok())
        .ok_or_else(|| Error::Usage(format!("cannot parse base offset from {}", path.display())))
}

fn first_batch_last_offset(bytes: &[u8]) -> Option<i64> {
    if bytes.len() < 27 {
        return None;
    }
    let base = i64::from_be_bytes(bytes[0..8].try_into().ok()?);
    let last_delta = i32::from_be_bytes(bytes[23..27].try_into().ok()?);
    Some(base + i64::from(last_delta))
}

fn is_native_bootstrap_residual(name: &str) -> bool {
    name == "kafka-cli-bootstrap.residual.json"
        || name.ends_with(".residual.json")
        || name == "kafka-cli-bootstrap.residual"
}

fn file_looks_like_text(path: &Path) -> Result<bool> {
    let mut file = File::open(path)?;
    let mut buf = [0_u8; 16];
    let n = file.read(&mut buf)?;
    if n == 0 {
        return Ok(false);
    }
    let trimmed: Vec<u8> = buf[..n]
        .iter()
        .copied()
        .skip_while(u8::is_ascii_whitespace)
        .collect();
    Ok(matches!(trimmed.first(), Some(b'{' | b'[' | b'#' | b'"')))
}

fn dump_non_binary_bootstrap_file(
    path: &Path,
    name: &str,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<()> {
    let mut body = String::new();
    File::open(path)?.read_to_string(&mut body)?;
    if is_native_bootstrap_residual(name) {
        writeln!(
            out,
            "Native bootstrap residual marker (not a Kafka binary bootstrap.checkpoint):"
        )?;
    } else if name == "bootstrap.checkpoint" {
        writeln!(
            err,
            "warning: {} looks like text/JSON, but Kafka expects a binary BatchFileReader bootstrap.checkpoint; dumping as residual text (will not parse as record batches)",
            path.display()
        )?;
        writeln!(
            out,
            "Non-binary bootstrap.checkpoint residual (Kafka cannot load this as bootstrap metadata):"
        )?;
    } else {
        writeln!(out, "Text residual file (not record-batch format):")?;
    }
    for line in body.lines() {
        writeln!(out, "  {line}")?;
    }
    Ok(())
}

/// `KRaft` metadata snapshots use names like `00000000000000000000-0000000000.snapshot`.
fn is_kraft_snapshot_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".snapshot") else {
        return false;
    };
    // offset-epoch form has a single dash between two numeric components.
    let Some((left, right)) = stem.split_once('-') else {
        return false;
    };
    !left.is_empty()
        && !right.is_empty()
        && left.chars().all(|c| c.is_ascii_digit())
        && right.chars().all(|c| c.is_ascii_digit())
}

/// `AbortedTxn` v0: version(i16) + 4×i64 fields = 34 bytes (Kafka `TransactionIndex`).
const ABORTED_TXN_RECORD_SIZE: usize = 2 + 8 * 4;
/// Single producer entry size in `ProducerSnapshot` v1 (non-flexible).
const PRODUCER_SNAPSHOT_ENTRY_SIZE: usize = 8 + 2 + 4 + 8 + 4 + 8 + 4 + 8;

fn dump_txnindex(path: &Path, out: &mut impl Write) -> Result<()> {
    let base_offset = base_offset_from_name(path).unwrap_or(0);
    let mut data = Vec::new();
    File::open(path)?.read_to_end(&mut data)?;
    if data.is_empty() {
        writeln!(out, "{} is empty.", path.display())?;
        return Ok(());
    }
    if !data.len().is_multiple_of(ABORTED_TXN_RECORD_SIZE) {
        return Err(Error::Config(format!(
            "corrupt txnindex {}: length {} is not a multiple of {ABORTED_TXN_RECORD_SIZE}",
            path.display(),
            data.len()
        )));
    }
    writeln!(
        out,
        "Transaction index starting offset: {base_offset} ({} entries)",
        data.len() / ABORTED_TXN_RECORD_SIZE
    )?;
    for chunk in data.chunks_exact(ABORTED_TXN_RECORD_SIZE) {
        let version = i16::from_be_bytes(chunk[0..2].try_into().unwrap());
        if version != 0 {
            return Err(Error::Config(format!(
                "unexpected aborted transaction version {version} in {}",
                path.display()
            )));
        }
        let producer_id = i64::from_be_bytes(chunk[2..10].try_into().unwrap());
        let first_offset = i64::from_be_bytes(chunk[10..18].try_into().unwrap());
        let last_offset = i64::from_be_bytes(chunk[18..26].try_into().unwrap());
        let last_stable_offset = i64::from_be_bytes(chunk[26..34].try_into().unwrap());
        writeln!(
            out,
            "version: {version} producerId: {producer_id} firstOffset: {first_offset} lastOffset: {last_offset} lastStableOffset: {last_stable_offset}"
        )?;
    }
    Ok(())
}

/// Producer snapshot: version(i16) + `ProducerSnapshot` v1 non-flexible body.
fn dump_producer_snapshot(path: &Path, out: &mut impl Write, err: &mut impl Write) -> Result<()> {
    let mut data = Vec::new();
    File::open(path)?.read_to_end(&mut data)?;
    if data.is_empty() {
        writeln!(out, "{} is empty.", path.display())?;
        return Ok(());
    }
    if data.len() < 6 {
        writeln!(
            err,
            "producer snapshot {} is too short ({} bytes); unsupported or corrupt",
            path.display(),
            data.len()
        )?;
        return Ok(());
    }
    let version = i16::from_be_bytes(data[0..2].try_into().unwrap());
    if version != 1 {
        writeln!(
            err,
            "producer snapshot {}: unsupported version {version} (only v1 is dumped natively)",
            path.display()
        )?;
        return Ok(());
    }
    let crc = u32::from_be_bytes(data[2..6].try_into().unwrap());
    if data.len() < 10 {
        writeln!(
            err,
            "producer snapshot {} truncated after CRC",
            path.display()
        )?;
        return Ok(());
    }
    let entry_count = i32::from_be_bytes(data[6..10].try_into().unwrap());
    if entry_count < 0 {
        return Err(Error::Config(format!(
            "producer snapshot {} has negative entry count",
            path.display()
        )));
    }
    let expected = 10 + PRODUCER_SNAPSHOT_ENTRY_SIZE * usize::try_from(entry_count).unwrap_or(0);
    if data.len() < expected {
        writeln!(
            err,
            "producer snapshot {} truncated: have {} bytes, need {expected} for {entry_count} entries",
            path.display(),
            data.len()
        )?;
        return Ok(());
    }
    writeln!(
        out,
        "Producer snapshot version: {version} crc: {crc} entries: {entry_count}"
    )?;
    let mut offset = 10;
    for _ in 0..entry_count {
        let chunk = &data[offset..offset + PRODUCER_SNAPSHOT_ENTRY_SIZE];
        let producer_id = i64::from_be_bytes(chunk[0..8].try_into().unwrap());
        let epoch = i16::from_be_bytes(chunk[8..10].try_into().unwrap());
        let last_sequence = i32::from_be_bytes(chunk[10..14].try_into().unwrap());
        let last_offset = i64::from_be_bytes(chunk[14..22].try_into().unwrap());
        let offset_delta = i32::from_be_bytes(chunk[22..26].try_into().unwrap());
        let timestamp = i64::from_be_bytes(chunk[26..34].try_into().unwrap());
        let coordinator_epoch = i32::from_be_bytes(chunk[34..38].try_into().unwrap());
        let current_txn_first = i64::from_be_bytes(chunk[38..46].try_into().unwrap());
        write!(
            out,
            "producerId: {producer_id} producerEpoch: {epoch} coordinatorEpoch: {coordinator_epoch} currentTxnFirstOffset: {current_txn_first} lastTimestamp: {timestamp}"
        )?;
        if last_offset >= 0 {
            let first_seq = last_sequence.saturating_sub(offset_delta);
            write!(
                out,
                " firstSequence: {first_seq} lastSequence: {last_sequence} lastOffset: {last_offset} offsetDelta: {offset_delta} timestamp: {timestamp}"
            )?;
        }
        writeln!(out)?;
        offset += PRODUCER_SNAPSHOT_ENTRY_SIZE;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use krafka::protocol::{Record, RecordBatch};
    use tempfile::TempDir;

    fn sample_log_bytes() -> Vec<u8> {
        let mut batch = RecordBatch::new();
        batch.base_offset = 10;
        batch.last_offset_delta = 1;
        batch.base_timestamp = 1_700_000_000_000;
        batch.max_timestamp = 1_700_000_000_001;
        batch.add_record(
            Record::new(
                Some(Bytes::from_static(b"k0")),
                Some(Bytes::from_static(b"v0")),
            )
            .with_offset_delta(0),
        );
        batch.add_record(
            Record::new(
                Some(Bytes::from_static(b"k1")),
                Some(Bytes::from_static(b"v1")),
            )
            .with_offset_delta(1)
            .with_timestamp_delta(1),
        );
        batch.encode().expect("encode batch").to_vec()
    }

    #[test]
    fn dump_log_should_print_batch_and_records() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("00000000000000000010.log");
        std::fs::write(&path, sample_log_bytes()).expect("write log");
        let mut out = Vec::new();
        let mut non_consecutive = BTreeMap::new();
        dump_log_file(
            &path,
            true,
            true,
            false,
            i32::MAX,
            &mut non_consecutive,
            &mut out,
        )
        .expect("dump");
        let text = String::from_utf8(out).expect("utf8");
        assert!(text.contains("Log starting offset: 10"));
        assert!(text.contains("baseOffset: 10"));
        assert!(text.contains("lastOffset: 11"));
        assert!(text.contains("key: k0 payload: v0"));
        assert!(text.contains("key: k1 payload: v1"));
    }

    #[test]
    fn string_decoder_detection_should_accept_kafka_defaults() {
        assert!(is_string_decoder(
            "org.apache.kafka.common.serialization.StringDeserializer"
        ));
        assert!(is_string_decoder("kafka.serializer.StringDecoder"));
        assert!(!is_string_decoder("com.example.CustomDecoder"));
    }

    fn control_batch_bytes(type_id: i16, value: &[u8]) -> Vec<u8> {
        let mut key = Vec::new();
        key.extend_from_slice(&0_i16.to_be_bytes());
        key.extend_from_slice(&type_id.to_be_bytes());
        let mut batch = RecordBatch::new();
        batch.base_offset = 100;
        batch.last_offset_delta = 0;
        batch.base_timestamp = 1_700_000_000_000;
        batch.max_timestamp = 1_700_000_000_000;
        batch.attributes.is_control_batch = true;
        batch.attributes.is_transactional = type_id == 0 || type_id == 1;
        batch.producer_id = 42;
        batch.producer_epoch = 1;
        batch.base_sequence = 0;
        batch.add_record(
            Record::new(Some(Bytes::from(key)), Some(Bytes::copy_from_slice(value)))
                .with_offset_delta(0),
        );
        batch.encode().expect("encode control batch").to_vec()
    }

    #[test]
    fn format_control_record_should_decode_end_txn_markers() {
        let mut value = Vec::new();
        value.extend_from_slice(&0_i16.to_be_bytes());
        value.extend_from_slice(&7_i32.to_be_bytes());
        let key = {
            let mut k = Vec::new();
            k.extend_from_slice(&0_i16.to_be_bytes());
            k.extend_from_slice(&1_i16.to_be_bytes()); // COMMIT
            Bytes::from(k)
        };
        let text = format_control_record(Some(&key), Some(&Bytes::from(value)));
        assert!(text.contains("endTxnMarker: COMMIT"));
        assert!(text.contains("coordinatorEpoch: 7"));
    }

    #[test]
    fn dump_log_should_print_end_txn_control_records() {
        let mut value = Vec::new();
        value.extend_from_slice(&0_i16.to_be_bytes());
        value.extend_from_slice(&11_i32.to_be_bytes());
        let dir = TempDir::new().expect("temp");
        let path = dir.path().join("00000000000000000100.log");
        std::fs::write(&path, control_batch_bytes(0, &value)).expect("write");
        let mut out = Vec::new();
        let mut non_consecutive = BTreeMap::new();
        dump_log_file(
            &path,
            true,
            true,
            false,
            i32::MAX,
            &mut non_consecutive,
            &mut out,
        )
        .expect("dump");
        let text = String::from_utf8(out).expect("utf8");
        assert!(text.contains("isControl: true"));
        assert!(text.contains("endTxnMarker: ABORT"));
        assert!(text.contains("coordinatorEpoch: 11"));
    }

    #[test]
    fn dump_log_should_print_leader_change_prefix() {
        let mut value = Vec::new();
        value.extend_from_slice(&0_i16.to_be_bytes()); // version
        value.extend_from_slice(&3_i32.to_be_bytes()); // leaderId
        let dir = TempDir::new().expect("temp");
        let path = dir.path().join("00000000000000000200.log");
        std::fs::write(&path, control_batch_bytes(2, &value)).expect("write");
        let mut out = Vec::new();
        let mut non_consecutive = BTreeMap::new();
        dump_log_file(
            &path,
            false,
            true,
            false,
            i32::MAX,
            &mut non_consecutive,
            &mut out,
        )
        .expect("dump");
        let text = String::from_utf8(out).expect("utf8");
        assert!(text.contains("LeaderChange:"));
        assert!(text.contains("leaderId=3"));
    }

    #[test]
    fn dump_txnindex_should_print_aborted_transactions() {
        let dir = TempDir::new().expect("temp");
        let path = dir.path().join("00000000000000000000.txnindex");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0_i16.to_be_bytes()); // version
        bytes.extend_from_slice(&7_i64.to_be_bytes()); // producerId
        bytes.extend_from_slice(&10_i64.to_be_bytes()); // firstOffset
        bytes.extend_from_slice(&20_i64.to_be_bytes()); // lastOffset
        bytes.extend_from_slice(&25_i64.to_be_bytes()); // lastStableOffset
        std::fs::write(&path, &bytes).expect("write txnindex");
        let mut out = Vec::new();
        dump_txnindex(&path, &mut out).expect("dump");
        let text = String::from_utf8(out).expect("utf8");
        assert!(text.contains(
            "version: 0 producerId: 7 firstOffset: 10 lastOffset: 20 lastStableOffset: 25"
        ));
    }

    #[test]
    fn dump_producer_snapshot_should_print_entries() {
        let dir = TempDir::new().expect("temp");
        let path = dir.path().join("00000000000000000042.snapshot");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1_i16.to_be_bytes()); // version
        bytes.extend_from_slice(&0xAABB_CCDD_u32.to_be_bytes()); // crc placeholder
        bytes.extend_from_slice(&1_i32.to_be_bytes()); // entry count
        bytes.extend_from_slice(&99_i64.to_be_bytes()); // producerId
        bytes.extend_from_slice(&3_i16.to_be_bytes()); // epoch
        bytes.extend_from_slice(&15_i32.to_be_bytes()); // lastSequence
        bytes.extend_from_slice(&100_i64.to_be_bytes()); // lastOffset
        bytes.extend_from_slice(&5_i32.to_be_bytes()); // offsetDelta
        bytes.extend_from_slice(&1_700_000_000_000_i64.to_be_bytes()); // timestamp
        bytes.extend_from_slice(&1_i32.to_be_bytes()); // coordinatorEpoch
        bytes.extend_from_slice(&(-1_i64).to_be_bytes()); // currentTxnFirstOffset
        std::fs::write(&path, &bytes).expect("write snapshot");
        let mut out = Vec::new();
        let mut err = Vec::new();
        dump_producer_snapshot(&path, &mut out, &mut err).expect("dump");
        let text = String::from_utf8(out).expect("utf8");
        assert!(text.contains("Producer snapshot version: 1"));
        assert!(text.contains("producerId: 99"));
        assert!(text.contains("lastSequence: 15"));
        assert!(text.contains("lastOffset: 100"));
        assert!(err.is_empty(), "stderr: {}", String::from_utf8_lossy(&err));
    }

    #[test]
    fn kraft_snapshot_name_detection() {
        assert!(is_kraft_snapshot_name(
            "00000000000000000000-0000000000.snapshot"
        ));
        assert!(!is_kraft_snapshot_name("00000000000000000042.snapshot"));
    }

    #[test]
    fn dump_should_diagnose_text_bootstrap_checkpoint_without_batch_parse() {
        let dir = TempDir::new().expect("temp");
        let path = dir.path().join("bootstrap.checkpoint");
        std::fs::write(
            &path,
            "{\n  \"format\": \"partial-native-bootstrap-v1\"\n}\n",
        )
        .expect("write json");
        let opts = DumpLogOptions {
            files: vec![path],
            print_data_log: false,
            verify_index_only: false,
            index_sanity_check: false,
            max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
            max_bytes: i32::MAX,
            deep_iteration: false,
            skip_record_metadata: false,
            key_decoder: None,
            value_decoder: None,
            offsets_decoder: false,
            transaction_log_decoder: false,
            cluster_metadata_decoder: false,
            remote_log_metadata_decoder: false,
            share_group_state_decoder: false,
        };
        // Exercise the real entry point with stderr/stdout capture via dump helpers path.
        let mut out = Vec::new();
        let mut err = Vec::new();
        dump_non_binary_bootstrap_file(&opts.files[0], "bootstrap.checkpoint", &mut out, &mut err)
            .expect("diagnose");
        let stdout = String::from_utf8(out).expect("utf8");
        let stderr = String::from_utf8(err).expect("utf8");
        assert!(stdout.contains("Non-binary bootstrap.checkpoint residual"));
        assert!(stdout.contains("partial-native-bootstrap-v1"));
        assert!(stderr.contains("BatchFileReader") || stderr.contains("binary"));
        assert!(!stdout.contains("Found invalid bytes"));
    }

    #[test]
    fn dump_native_residual_json_should_print_marker() {
        let dir = TempDir::new().expect("temp");
        let path = dir.path().join("kafka-cli-bootstrap.residual.json");
        std::fs::write(
            &path,
            "{\n  \"format\": \"partial-native-bootstrap-v1\"\n}\n",
        )
        .expect("write");
        let mut out = Vec::new();
        let mut err = Vec::new();
        dump_non_binary_bootstrap_file(
            &path,
            "kafka-cli-bootstrap.residual.json",
            &mut out,
            &mut err,
        )
        .expect("dump residual");
        let stdout = String::from_utf8(out).expect("utf8");
        assert!(stdout.contains("Native bootstrap residual marker"));
        assert!(err.is_empty());
    }

    /// Writes a synthetic `.log` for real binary dump-log smoke tests when
    /// `KAFKA_CLI_DUMP_LOG_FIXTURE` is set to a destination path.
    #[test]
    fn write_dump_log_fixture_when_env_set() {
        let Ok(dest) = std::env::var("KAFKA_CLI_DUMP_LOG_FIXTURE") else {
            return;
        };
        let path = PathBuf::from(dest);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir fixture parent");
        }
        std::fs::write(&path, sample_log_bytes()).expect("write fixture log");
        assert!(path.is_file());
    }

    fn default_opts(files: Vec<PathBuf>) -> DumpLogOptions {
        DumpLogOptions {
            files,
            print_data_log: false,
            verify_index_only: false,
            index_sanity_check: false,
            max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
            max_bytes: i32::MAX,
            deep_iteration: false,
            skip_record_metadata: false,
            key_decoder: None,
            value_decoder: None,
            offsets_decoder: false,
            transaction_log_decoder: false,
            cluster_metadata_decoder: false,
            remote_log_metadata_decoder: false,
            share_group_state_decoder: false,
        }
    }

    #[test]
    fn validate_options_should_reject_empty_files_and_non_positive_limits() {
        let err = validate_options(&default_opts(vec![])).expect_err("empty files");
        assert!(err.to_string().contains("files"), "{err}");

        let mut opts = default_opts(vec![PathBuf::from("x.log")]);
        opts.max_message_size = 0;
        let err = validate_options(&opts).expect_err("max-message-size");
        assert!(err.to_string().contains("max-message-size"), "{err}");

        opts.max_message_size = 1024;
        opts.max_bytes = -1;
        let err = validate_options(&opts).expect_err("max-bytes");
        assert!(err.to_string().contains("max-bytes"), "{err}");
    }

    #[test]
    fn validate_options_should_reject_coordinator_decoders_and_custom_classes() {
        let mut opts = default_opts(vec![PathBuf::from("x.log")]);
        opts.offsets_decoder = true;
        let err = validate_options(&opts).expect_err("offsets decoder");
        assert!(err.to_string().contains("offsets-decoder"), "{err}");

        opts.offsets_decoder = false;
        opts.cluster_metadata_decoder = true;
        let err = validate_options(&opts).expect_err("cluster decoder");
        assert!(
            err.to_string().contains("cluster-metadata-decoder"),
            "{err}"
        );

        opts.cluster_metadata_decoder = false;
        opts.key_decoder = Some("com.example.CustomKey".into());
        let err = validate_options(&opts).expect_err("custom key");
        assert!(err.to_string().contains("CustomKey"), "{err}");

        opts.key_decoder = Some("kafka.serializer.StringDecoder".into());
        validate_options(&opts).expect("string decoder ok");
    }

    #[test]
    fn dump_log_options_should_enable_print_and_deep_from_decoder_flags() {
        let mut opts = default_opts(vec![PathBuf::from("x.log")]);
        assert!(!opts.should_print_data_log());
        assert!(!opts.is_deep_iteration());
        opts.print_data_log = true;
        assert!(opts.should_print_data_log());
        assert!(opts.is_deep_iteration());
        opts.print_data_log = false;
        opts.deep_iteration = true;
        assert!(opts.is_deep_iteration());
        opts.deep_iteration = false;
        opts.value_decoder = Some("string".into());
        assert!(opts.should_print_data_log());
        assert!(opts.is_deep_iteration());
    }

    #[test]
    fn format_control_record_should_handle_missing_key_and_unknown_type() {
        assert!(format_control_record(None, None).contains("missing-key"));
        let short = Bytes::from_static(&[0, 1]);
        assert!(format_control_record(Some(&short), None).contains("invalid-key-len"));
        let mut key = Vec::new();
        key.extend_from_slice(&0_i16.to_be_bytes());
        key.extend_from_slice(&99_i16.to_be_bytes());
        let text = format_control_record(Some(&Bytes::from(key)), None);
        assert!(text.contains("UNKNOWN") || text.contains("99"), "{text}");
    }

    #[test]
    fn format_named_control_should_decode_snapshot_header_and_kraft_version() {
        // SNAPSHOT_HEADER: version + lastContainedLogTimestamp
        let mut value = Vec::new();
        value.extend_from_slice(&0_i16.to_be_bytes());
        value.extend_from_slice(&1_700_000_000_000_i64.to_be_bytes());
        let mut key = Vec::new();
        key.extend_from_slice(&0_i16.to_be_bytes());
        key.extend_from_slice(&3_i16.to_be_bytes());
        let text = format_control_record(Some(&Bytes::from(key)), Some(&Bytes::from(value)));
        assert!(text.contains("SnapshotHeader"), "{text}");
        assert!(text.contains("lastContainedLogTimestamp"), "{text}");

        let mut value = Vec::new();
        value.extend_from_slice(&0_i16.to_be_bytes());
        value.extend_from_slice(&1_i16.to_be_bytes());
        let mut key = Vec::new();
        key.extend_from_slice(&0_i16.to_be_bytes());
        key.extend_from_slice(&5_i16.to_be_bytes());
        let text = format_control_record(Some(&Bytes::from(key)), Some(&Bytes::from(value)));
        assert!(text.contains("KRaftVersion"), "{text}");
        assert!(text.contains("kRaftVersion: 1"), "{text}");
    }

    #[test]
    fn dump_offset_index_should_print_entries_and_empty_file() {
        let dir = TempDir::new().expect("temp");
        let empty = dir.path().join("00000000000000000000.index");
        std::fs::write(&empty, []).expect("empty");
        let mut out = Vec::new();
        let mut mismatches = BTreeMap::new();
        dump_offset_index(
            &empty,
            false,
            false,
            DEFAULT_MAX_MESSAGE_SIZE,
            &mut mismatches,
            &mut out,
        )
        .expect("empty dump");
        assert!(
            String::from_utf8_lossy(&out).contains("is empty"),
            "{}",
            String::from_utf8_lossy(&out)
        );

        let path = dir.path().join("00000000000000000010.index");
        let mut bytes = Vec::new();
        // relative offset 0, position 0
        bytes.extend_from_slice(&0_i32.to_be_bytes());
        bytes.extend_from_slice(&0_i32.to_be_bytes());
        // relative offset 2, position 100
        bytes.extend_from_slice(&2_i32.to_be_bytes());
        bytes.extend_from_slice(&100_i32.to_be_bytes());
        std::fs::write(&path, &bytes).expect("write index");
        let mut out = Vec::new();
        dump_offset_index(
            &path,
            false,
            false,
            DEFAULT_MAX_MESSAGE_SIZE,
            &mut mismatches,
            &mut out,
        )
        .expect("index dump");
        let text = String::from_utf8(out).expect("utf8");
        assert!(text.contains("offset: 10"), "{text}");
        assert!(text.contains("offset: 12"), "{text}");
        assert!(text.contains("position: 100"), "{text}");
    }

    #[test]
    fn dump_offset_index_should_fail_sanity_on_decreasing_offsets() {
        let dir = TempDir::new().expect("temp");
        let path = dir.path().join("00000000000000000000.index");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&5_i32.to_be_bytes());
        bytes.extend_from_slice(&0_i32.to_be_bytes());
        bytes.extend_from_slice(&3_i32.to_be_bytes()); // decreasing relative
        bytes.extend_from_slice(&8_i32.to_be_bytes());
        std::fs::write(&path, &bytes).expect("write");
        let mut out = Vec::new();
        let mut mismatches = BTreeMap::new();
        let err = dump_offset_index(
            &path,
            true,
            false,
            DEFAULT_MAX_MESSAGE_SIZE,
            &mut mismatches,
            &mut out,
        )
        .expect_err("sanity");
        assert!(err.to_string().contains("sanity"), "{err}");
    }

    #[test]
    fn dump_time_index_should_print_timestamp_and_offset() {
        let dir = TempDir::new().expect("temp");
        let path = dir.path().join("00000000000000000020.timeindex");
        let mut bytes = Vec::new();
        // timestamp, relative offset
        bytes.extend_from_slice(&1_700_000_000_000_i64.to_be_bytes());
        bytes.extend_from_slice(&0_i32.to_be_bytes());
        bytes.extend_from_slice(&1_700_000_000_100_i64.to_be_bytes());
        bytes.extend_from_slice(&5_i32.to_be_bytes());
        std::fs::write(&path, &bytes).expect("write");
        let mut out = Vec::new();
        let mut err = Vec::new();
        dump_time_index(&path, false, false, &mut out, &mut err).expect("timeindex");
        let text = String::from_utf8(out).expect("utf8");
        assert!(text.contains("timestamp: 1700000000000"), "{text}");
        assert!(
            text.contains("offset: 20") || text.contains("offset: 25"),
            "{text}"
        );
    }

    #[test]
    fn dump_log_segments_entry_should_reject_missing_files_arg() {
        let err = dump_log_segments(&default_opts(vec![])).expect_err("usage");
        assert!(matches!(err, Error::Usage(_)), "{err}");
    }

    #[test]
    fn base_offset_and_residual_name_helpers() {
        let path = PathBuf::from("/tmp/00000000000000000123.index");
        assert_eq!(base_offset_from_name(&path).expect("base"), 123);
        assert!(is_native_bootstrap_residual(
            "kafka-cli-bootstrap.residual.json"
        ));
        assert!(!is_native_bootstrap_residual("bootstrap.checkpoint"));
        assert!(is_kraft_snapshot_name(
            "00000000000000000000-0000000001.snapshot"
        ));
    }
}
