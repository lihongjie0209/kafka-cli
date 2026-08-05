//! `KRaft` storage tooling (`kafka-storage.sh` / `StorageTool`).

use std::{
    collections::HashMap,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use uuid::Uuid;

use crate::error::{Error, Result};

const META_PROPERTIES: &str = "meta.properties";
/// Kafka reserves `bootstrap.checkpoint` for a **binary** `BatchFileReader` file.
/// Never write a non-binary marker under that name (`KafkaRaftServer` will fail to load it).
const KAFKA_BOOTSTRAP_CHECKPOINT: &str = "bootstrap.checkpoint";
/// Native residual marker: documents partial controller format without colliding with Kafka.
const NATIVE_BOOTSTRAP_RESIDUAL: &str = "kafka-cli-bootstrap.residual.json";
/// Metadata partition directory created for controller format (`KRaft` layout).
const CLUSTER_METADATA_DIR: &str = "__cluster_metadata-0";

/// Storage subcommands matching Kafka's `kafka-storage.sh`.
#[derive(Debug, Clone)]
pub enum StorageAction {
    /// Print a random Kafka-compatible UUID.
    RandomUuid,
    /// Describe `meta.properties` in configured log directories.
    Info { config: PathBuf },
    /// Format log directories with cluster/node metadata.
    Format {
        config: PathBuf,
        cluster_id: String,
        ignore_formatted: bool,
        release_version: Option<String>,
        feature: Vec<String>,
        standalone: bool,
        no_initial_controllers: bool,
        initial_controllers: Option<String>,
        add_scram: Vec<String>,
    },
    /// Offline metadata version → feature mapping (reuses local feature tables when possible).
    VersionMapping { release_version: Option<String> },
    /// Offline feature dependency lookup.
    FeatureDependencies { feature: Vec<String> },
}

/// Execute a storage tool action.
pub fn storage(action: StorageAction) -> Result<()> {
    match action {
        StorageAction::RandomUuid => {
            println!("{}", Uuid::new_v4());
            Ok(())
        }
        StorageAction::Info { config } => storage_info(&config),
        StorageAction::Format {
            config,
            cluster_id,
            ignore_formatted,
            release_version,
            feature,
            standalone,
            no_initial_controllers,
            initial_controllers,
            add_scram,
        } => storage_format(
            &config,
            &cluster_id,
            ignore_formatted,
            release_version.as_deref(),
            &feature,
            standalone,
            no_initial_controllers,
            initial_controllers.as_deref(),
            &add_scram,
        ),
        StorageAction::VersionMapping { release_version } => {
            // Defer to the existing offline features mapping command message if full tables
            // live there; provide a clear pointer rather than a silent empty response.
            let version = release_version.as_deref().unwrap_or("latest production");
            println!(
                "version-mapping for {version}: use `kafka features version-mapping` for the offline feature table; storage version-mapping is an alias of that offline map."
            );
            if let Some(release) = release_version {
                println!("requested release-version: {release}");
            }
            Ok(())
        }
        StorageAction::FeatureDependencies { feature } => {
            if feature.is_empty() {
                return Err(Error::Usage(
                    "--feature is required for feature-dependencies".into(),
                ));
            }
            println!(
                "feature-dependencies: use `kafka features feature-dependencies` for the offline dependency table; requested features: {}",
                feature.join(", ")
            );
            Ok(())
        }
    }
}

fn storage_info(config_path: &Path) -> Result<()> {
    let props = load_server_config(config_path)?;
    let kraft = !process_roles(&props).is_empty();
    let directories = log_directories(&props);
    let mut problems = Vec::new();
    let mut found = Vec::new();
    let mut prev: Option<MetaProperties> = None;

    for directory in &directories {
        let path = Path::new(directory);
        if !path.is_dir() {
            problems.push(format!("{directory} is not a directory"));
            continue;
        }
        found.push(directory.clone());
        let meta_path = path.join(META_PROPERTIES);
        if !meta_path.exists() {
            problems.push(format!("{directory} is not formatted"));
            continue;
        }
        let meta = read_meta_properties(&meta_path)?;
        println!("Found meta.properties file in {directory}:");
        print_meta(&meta);
        if let Some(previous) = &prev {
            if previous.cluster_id != meta.cluster_id {
                problems.push(format!(
                    "Inconsistent cluster.id between {directory} and a previous directory"
                ));
            }
            if previous.node_id != meta.node_id {
                problems.push(format!(
                    "Inconsistent node.id between {directory} and a previous directory"
                ));
            }
        }
        prev = Some(meta);
    }

    if found.is_empty() {
        problems.push("No log directories found in the configuration".into());
    }
    if kraft {
        println!("Found KRaft mode configuration.");
    } else {
        println!("Found ZooKeeper mode configuration (legacy). Formatting requires KRaft.");
    }
    if problems.is_empty() {
        println!("All of the log directories are acceptably formatted.");
        Ok(())
    } else {
        for problem in &problems {
            eprintln!("Problem: {problem}");
        }
        Err(Error::Config(format!(
            "{} problem(s) found in log directories",
            problems.len()
        )))
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "format mirrors Kafka StorageTool flags"
)]
fn storage_format(
    config_path: &Path,
    cluster_id: &str,
    ignore_formatted: bool,
    release_version: Option<&str>,
    feature: &[String],
    standalone: bool,
    no_initial_controllers: bool,
    initial_controllers: Option<&str>,
    add_scram: &[String],
) -> Result<()> {
    let props = load_server_config(config_path)?;
    let roles = process_roles(&props);
    if roles.is_empty() {
        return Err(Error::Usage(
            "The kafka configuration file appears to be for a legacy cluster. Formatting is only supported for clusters in KRaft mode.".into(),
        ));
    }
    let node_id = props
        .get("node.id")
        .or_else(|| props.get("broker.id"))
        .ok_or_else(|| Error::Config("node.id is required in KRaft mode".into()))?
        .parse::<i32>()
        .map_err(|error| Error::Config(format!("invalid node.id: {error}")))?;

    if !add_scram.is_empty() {
        return Err(Error::Unsupported(
            "--add-scram requires writing SCRAM records into the __cluster_metadata bootstrap log, which the native storage tool does not implement".into(),
        ));
    }
    let is_controller = roles.iter().any(|role| role == "controller");
    if is_controller
        && props
            .get("controller.quorum.voters")
            .is_none_or(String::is_empty)
        && !standalone
        && initial_controllers.is_none()
        && !no_initial_controllers
    {
        return Err(Error::Usage(
            "Because controller.quorum.voters is not set on this controller, you must specify one of --standalone, --initial-controllers, or --no-initial-controllers.".into(),
        ));
    }

    let directories = log_directories(&props);
    if directories.is_empty() {
        return Err(Error::Config(
            "no log.dirs / metadata.log.dir found in configuration".into(),
        ));
    }
    let metadata_log_dir = props
        .get("metadata.log.dir")
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .or_else(|| directories.first().cloned());

    for directory in &directories {
        let path = Path::new(directory);
        fs::create_dir_all(path)?;
        let meta_path = path.join(META_PROPERTIES);
        if meta_path.exists() {
            if ignore_formatted {
                println!("Skipping already formatted directory {directory}");
                continue;
            }
            return Err(Error::Config(format!(
                "log directory {directory} is already formatted. Use --ignore-formatted to skip."
            )));
        }
        let is_metadata_dir = metadata_log_dir.as_deref() == Some(directory.as_str());
        if is_controller && is_metadata_dir {
            ensure_no_text_reserved_bootstrap(path)?;
        }
        let directory_id = Uuid::new_v4();
        let meta = MetaProperties {
            version: 1,
            cluster_id: Some(cluster_id.to_owned()),
            node_id: Some(node_id),
            directory_id: Some(directory_id.to_string()),
        };
        write_meta_properties(&meta_path, &meta)?;
        println!("Formatting {directory} with metadata:");
        print_meta(&meta);
        if let Some(version) = release_version {
            println!("  release.version={version}");
        }
        for item in feature {
            println!("  feature={item}");
        }

        if is_controller && is_metadata_dir {
            write_controller_bootstrap_artifacts(
                path,
                cluster_id,
                node_id,
                release_version,
                feature,
                standalone,
                no_initial_controllers,
                initial_controllers,
            )?;
            println!(
                "  wrote controller layout: {CLUSTER_METADATA_DIR}/ and residual marker {NATIVE_BOOTSTRAP_RESIDUAL}"
            );
            println!(
                "  residual: did not write Kafka binary {KAFKA_BOOTSTRAP_CHECKPOINT} (BatchFileReader format); full RecordsSnapshotWriter snapshot under {CLUSTER_METADATA_DIR} is not generated — use official kafka-storage for production controller bootstrap"
            );
        }
    }
    Ok(())
}

fn ensure_no_text_reserved_bootstrap(metadata_dir: &Path) -> Result<()> {
    let reserved = metadata_dir.join(KAFKA_BOOTSTRAP_CHECKPOINT);
    if !reserved.exists() {
        return Ok(());
    }
    let head = peek_file_prefix(&reserved, 8)?;
    if looks_like_text_or_json(&head) {
        return Err(Error::Config(format!(
            "refusing to format: {} exists and is not a Kafka binary bootstrap.checkpoint (looks like text/JSON). Remove it or reformat with official kafka-storage after cleanup.",
            reserved.display()
        )));
    }
    Ok(())
}

/// Write the native partial controller bootstrap layout for a metadata log directory.
///
/// Produces:
/// - `__cluster_metadata-0/` — empty metadata partition directory matching `KRaft` layout
/// - `kafka-cli-bootstrap.residual.json` — JSON residual marker (never `bootstrap.checkpoint`)
///
/// Does **not** write Kafka's reserved binary `bootstrap.checkpoint` name, and does **not** emit a
/// full `RecordsSnapshotWriter` snapshot. Those residuals are intentional and covered by tests.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors StorageTool format bootstrap inputs"
)]
fn write_controller_bootstrap_artifacts(
    metadata_dir: &Path,
    cluster_id: &str,
    node_id: i32,
    release_version: Option<&str>,
    feature: &[String],
    standalone: bool,
    no_initial_controllers: bool,
    initial_controllers: Option<&str>,
) -> Result<()> {
    let cluster_meta = metadata_dir.join(CLUSTER_METADATA_DIR);
    fs::create_dir_all(&cluster_meta)?;

    let mut residual = File::create(metadata_dir.join(NATIVE_BOOTSTRAP_RESIDUAL))?;
    writeln!(residual, "{{")?;
    writeln!(residual, "  \"source\": \"kafka-cli storage format\",")?;
    writeln!(residual, "  \"format\": \"partial-native-bootstrap-v1\",")?;
    writeln!(
        residual,
        "  \"kafka.reserved.bootstrap.checkpoint\": \"not written (binary BatchFileReader format only)\","
    )?;
    writeln!(residual, "  \"cluster.id\": \"{cluster_id}\",")?;
    writeln!(residual, "  \"node.id\": {node_id},")?;
    writeln!(
        residual,
        "  \"standalone\": {},",
        if standalone { "true" } else { "false" }
    )?;
    writeln!(
        residual,
        "  \"no.initial.controllers\": {},",
        if no_initial_controllers {
            "true"
        } else {
            "false"
        }
    )?;
    if let Some(version) = release_version {
        writeln!(residual, "  \"release.version\": \"{version}\",")?;
    }
    if let Some(controllers) = initial_controllers {
        writeln!(
            residual,
            "  \"initial.controllers\": \"{}\",",
            controllers.replace('"', "\\\"")
        )?;
    }
    writeln!(residual, "  \"features\": [")?;
    for (index, item) in feature.iter().enumerate() {
        let comma = if index + 1 == feature.len() { "" } else { "," };
        writeln!(residual, "    \"{item}\"{comma}")?;
    }
    writeln!(residual, "  ],")?;
    writeln!(
        residual,
        "  \"residual\": \"full KRaft RecordsSnapshotWriter bootstrap snapshot is not written; Kafka will use default bootstrap metadata when {KAFKA_BOOTSTRAP_CHECKPOINT} is absent\""
    )?;
    writeln!(residual, "}}")?;
    Ok(())
}

fn peek_file_prefix(path: &Path, n: usize) -> Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let mut buf = vec![0_u8; n];
    let read = file.read(&mut buf)?;
    buf.truncate(read);
    Ok(buf)
}

fn looks_like_text_or_json(bytes: &[u8]) -> bool {
    let trimmed: Vec<u8> = bytes
        .iter()
        .copied()
        .skip_while(u8::is_ascii_whitespace)
        .collect();
    matches!(trimmed.first(), Some(b'{' | b'[' | b'"' | b'#' | b'/'))
        || trimmed
            .first()
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MetaProperties {
    version: i32,
    cluster_id: Option<String>,
    node_id: Option<i32>,
    directory_id: Option<String>,
}

fn print_meta(meta: &MetaProperties) {
    println!("  version={}", meta.version);
    if let Some(cluster_id) = &meta.cluster_id {
        println!("  cluster.id={cluster_id}");
    }
    if let Some(node_id) = meta.node_id {
        println!("  node.id={node_id}");
    }
    if let Some(directory_id) = &meta.directory_id {
        println!("  directory.id={directory_id}");
    }
}

fn read_meta_properties(path: &Path) -> Result<MetaProperties> {
    let mut text = String::new();
    File::open(path)?.read_to_string(&mut text)?;
    let mut version = 0;
    let mut cluster_id = None;
    let mut node_id = None;
    let mut directory_id = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "version" => {
                version = value.trim().parse().map_err(|error| {
                    Error::Config(format!("invalid version in meta.properties: {error}"))
                })?;
            }
            "cluster.id" => cluster_id = Some(value.trim().to_owned()),
            "node.id" | "broker.id" => {
                node_id = Some(
                    value
                        .trim()
                        .parse()
                        .map_err(|error| Error::Config(format!("invalid node.id: {error}")))?,
                );
            }
            "directory.id" => directory_id = Some(value.trim().to_owned()),
            _ => {}
        }
    }
    if version >= 1 {
        if cluster_id.is_none() {
            return Err(Error::Config(
                "cluster.id was not found in meta.properties".into(),
            ));
        }
        if node_id.is_none() {
            return Err(Error::Config(
                "node.id was not found in meta.properties".into(),
            ));
        }
    }
    Ok(MetaProperties {
        version,
        cluster_id,
        node_id,
        directory_id,
    })
}

fn write_meta_properties(path: &Path, meta: &MetaProperties) -> Result<()> {
    let mut file = File::create(path)?;
    writeln!(file, "#")?;
    writeln!(file, "# generated by kafka-cli storage format")?;
    writeln!(file, "version={}", meta.version)?;
    if let Some(cluster_id) = &meta.cluster_id {
        writeln!(file, "cluster.id={cluster_id}")?;
    }
    if let Some(node_id) = meta.node_id {
        writeln!(file, "node.id={node_id}")?;
    }
    if let Some(directory_id) = &meta.directory_id {
        writeln!(file, "directory.id={directory_id}")?;
    }
    Ok(())
}

fn load_server_config(path: &Path) -> Result<HashMap<String, String>> {
    crate::config::load_properties(path)
}

fn process_roles(props: &HashMap<String, String>) -> Vec<String> {
    props
        .get("process.roles")
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|role| !role.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn log_directories(props: &HashMap<String, String>) -> Vec<String> {
    let mut directories = Vec::new();
    if let Some(log_dirs) = props.get("log.dirs").or_else(|| props.get("log.dir")) {
        for dir in log_dirs.split(',') {
            let dir = dir.trim();
            if !dir.is_empty() {
                directories.push(dir.to_owned());
            }
        }
    }
    if let Some(metadata) = props.get("metadata.log.dir") {
        let metadata = metadata.trim();
        if !metadata.is_empty() && !directories.iter().any(|dir| dir == metadata) {
            directories.push(metadata.to_owned());
        }
    }
    directories.sort();
    directories.dedup();
    directories
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn random_uuid_action_should_succeed() {
        storage(StorageAction::RandomUuid).expect("uuid");
    }

    #[test]
    fn format_and_info_should_round_trip_meta_properties() {
        let dir = TempDir::new().expect("temp");
        let log_dir = dir.path().join("kafka-logs");
        fs::create_dir_all(&log_dir).expect("mkdir");
        let config_path = dir.path().join("server.properties");
        let mut config = File::create(&config_path).expect("config");
        writeln!(config, "process.roles=broker,controller").unwrap();
        writeln!(config, "node.id=1").unwrap();
        writeln!(config, "log.dirs={}", log_dir.display()).unwrap();
        writeln!(config, "metadata.log.dir={}", log_dir.display()).unwrap();
        writeln!(config, "controller.quorum.voters=1@localhost:9093").unwrap();
        drop(config);

        storage(StorageAction::Format {
            config: config_path.clone(),
            cluster_id: "test-cluster".into(),
            ignore_formatted: false,
            release_version: Some("3.7-IV0".into()),
            feature: vec![],
            standalone: false,
            no_initial_controllers: false,
            initial_controllers: None,
            add_scram: vec![],
        })
        .expect("format");

        let meta = read_meta_properties(&log_dir.join(META_PROPERTIES)).expect("read meta");
        assert_eq!(meta.version, 1);
        assert_eq!(meta.cluster_id.as_deref(), Some("test-cluster"));
        assert_eq!(meta.node_id, Some(1));
        assert!(meta.directory_id.is_some());

        // Controller metadata dir gets layout + residual marker — never Kafka's reserved binary name.
        assert!(
            !log_dir.join(KAFKA_BOOTSTRAP_CHECKPOINT).exists(),
            "must not write reserved binary name bootstrap.checkpoint as JSON"
        );
        let residual = log_dir.join(NATIVE_BOOTSTRAP_RESIDUAL);
        assert!(
            residual.is_file(),
            "native residual marker must be written for controller metadata dir"
        );
        let body = fs::read_to_string(&residual).expect("read residual");
        assert!(body.contains("partial-native-bootstrap-v1"));
        assert!(body.contains("test-cluster"));
        assert!(body.contains("not written"));
        assert!(body.contains("RecordsSnapshotWriter"));
        assert!(
            log_dir.join(CLUSTER_METADATA_DIR).is_dir(),
            "__cluster_metadata-0 directory must exist"
        );

        storage(StorageAction::Info {
            config: config_path,
        })
        .expect("info");
    }

    #[test]
    fn broker_only_format_should_not_write_controller_bootstrap_artifacts() {
        let dir = TempDir::new().expect("temp");
        let log_dir = dir.path().join("data");
        fs::create_dir_all(&log_dir).expect("mkdir");
        let config_path = dir.path().join("server.properties");
        let mut config = File::create(&config_path).expect("config");
        writeln!(config, "process.roles=broker").unwrap();
        writeln!(config, "node.id=2").unwrap();
        writeln!(config, "log.dirs={}", log_dir.display()).unwrap();
        drop(config);

        storage(StorageAction::Format {
            config: config_path,
            cluster_id: "broker-cluster".into(),
            ignore_formatted: false,
            release_version: None,
            feature: vec![],
            standalone: false,
            no_initial_controllers: false,
            initial_controllers: None,
            add_scram: vec![],
        })
        .expect("format broker");

        assert!(log_dir.join(META_PROPERTIES).is_file());
        assert!(!log_dir.join(KAFKA_BOOTSTRAP_CHECKPOINT).exists());
        assert!(!log_dir.join(NATIVE_BOOTSTRAP_RESIDUAL).exists());
        assert!(!log_dir.join(CLUSTER_METADATA_DIR).exists());
    }

    #[test]
    fn format_should_refuse_existing_text_bootstrap_checkpoint() {
        let dir = TempDir::new().expect("temp");
        let log_dir = dir.path().join("kafka-logs");
        fs::create_dir_all(&log_dir).expect("mkdir");
        // Simulate a bad prior run that wrote JSON under the reserved name.
        fs::write(
            log_dir.join(KAFKA_BOOTSTRAP_CHECKPOINT),
            "{\n  \"bad\": true\n}\n",
        )
        .expect("write bad checkpoint");
        let config_path = dir.path().join("server.properties");
        let mut config = File::create(&config_path).expect("config");
        writeln!(config, "process.roles=controller").unwrap();
        writeln!(config, "node.id=1").unwrap();
        writeln!(config, "log.dirs={}", log_dir.display()).unwrap();
        writeln!(config, "metadata.log.dir={}", log_dir.display()).unwrap();
        writeln!(config, "controller.quorum.voters=1@localhost:9093").unwrap();
        drop(config);

        let error = storage(StorageAction::Format {
            config: config_path,
            cluster_id: "refuse-cluster".into(),
            ignore_formatted: false,
            release_version: None,
            feature: vec![],
            standalone: false,
            no_initial_controllers: false,
            initial_controllers: None,
            add_scram: vec![],
        })
        .expect_err("must refuse text bootstrap.checkpoint");
        assert!(
            error.to_string().contains("bootstrap.checkpoint"),
            "error: {error}"
        );
    }
}
