//! Kafka client property loading and normalization.

use std::{collections::HashMap, fs::File, path::Path};

use krafka::{
    admin::{AdminClient, AdminClientBuilder},
    auth::{AuthConfig, TlsConfig},
};
use rdkafka::ClientConfig;

use crate::error::{Error, Result};

/// Loads a Kafka properties file.
pub fn load_properties(path: &Path) -> Result<HashMap<String, String>> {
    let file = File::open(path)?;
    java_properties::read(file).map_err(|error| Error::Config(error.to_string()))
}

/// Builds an rdkafka configuration from Java-compatible properties.
pub fn client_config(bootstrap: &str, path: Option<&Path>) -> Result<ClientConfig> {
    let mut values = match path {
        Some(path) => load_properties(path)?,
        None => HashMap::new(),
    };
    values.insert("bootstrap.servers".into(), bootstrap.into());

    let mut config = ClientConfig::new();
    for (key, value) in values {
        config.set(normalize_key(&key), value);
    }
    Ok(config)
}

/// Builds the pure-Rust admin client used for APIs absent from librdkafka.
pub async fn protocol_admin(
    bootstrap: &str,
    timeout: std::time::Duration,
    path: Option<&Path>,
) -> Result<AdminClient> {
    let values = match path {
        Some(path) => load_properties(path)?,
        None => HashMap::new(),
    };
    let connect_timeout = timeout.min(std::time::Duration::from_secs(10));
    let mut builder = AdminClientBuilder::default()
        .bootstrap_servers(bootstrap)
        .client_id("kafka-cli-admin")
        .connect_timeout(connect_timeout)
        .request_timeout(timeout);
    let auth = protocol_auth(&values)?;
    if let Some(auth) = auth {
        builder = builder.auth(auth);
    }
    Ok(builder.build().await?)
}

pub(crate) fn protocol_auth(values: &HashMap<String, String>) -> Result<Option<AuthConfig>> {
    let security = values
        .get("security.protocol")
        .map_or("PLAINTEXT", String::as_str)
        .to_ascii_uppercase();
    let tls = || {
        let mut tls = TlsConfig::new().with_native_roots();
        if let Some(ca) = values
            .get("ssl.ca.location")
            .or_else(|| values.get("ssl.truststore.location"))
        {
            tls = tls.with_ca_cert(ca);
        }
        if let (Some(cert), Some(key)) = (
            values.get("ssl.certificate.location"),
            values.get("ssl.key.location"),
        ) {
            tls = tls.with_client_cert(cert, key);
        }
        tls
    };
    let credentials = || -> Result<(&str, &str)> {
        let username = values
            .get("sasl.username")
            .map(String::as_str)
            .ok_or_else(|| Error::Config("sasl.username is required".into()))?;
        let password = values
            .get("sasl.password")
            .map(String::as_str)
            .ok_or_else(|| Error::Config("sasl.password is required".into()))?;
        Ok((username, password))
    };
    let mechanism = values
        .get("sasl.mechanism")
        .map_or("PLAIN", String::as_str)
        .to_ascii_uppercase();
    let auth = match (security.as_str(), mechanism.as_str()) {
        ("PLAINTEXT", _) => None,
        ("SSL", _) => Some(AuthConfig::ssl(tls())),
        ("SASL_PLAINTEXT", "PLAIN") => {
            let (username, password) = credentials()?;
            Some(AuthConfig::sasl_plain(username, password)?)
        }
        ("SASL_PLAINTEXT", "SCRAM-SHA-256") => {
            let (username, password) = credentials()?;
            Some(AuthConfig::sasl_scram_sha256(username, password))
        }
        ("SASL_PLAINTEXT", "SCRAM-SHA-512") => {
            let (username, password) = credentials()?;
            Some(AuthConfig::sasl_scram_sha512(username, password))
        }
        ("SASL_SSL", "PLAIN") => {
            let (username, password) = credentials()?;
            Some(AuthConfig::sasl_plain_ssl(username, password, tls())?)
        }
        ("SASL_SSL", mechanism) => {
            return Err(Error::Config(format!(
                "the pure-Rust admin transport does not yet support {mechanism} over TLS"
            )));
        }
        (protocol, _) => {
            return Err(Error::Config(format!(
                "unsupported security.protocol: {protocol}"
            )));
        }
    };
    Ok(auth)
}

fn normalize_key(key: &str) -> &str {
    match key {
        "ssl.truststore.location" => "ssl.ca.location",
        "ssl.keystore.location" => "ssl.certificate.location",
        "ssl.key.location" => "ssl.key.location",
        "ssl.key.password" => "ssl.key.password",
        _ => key,
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn load_properties_should_parse_java_properties() {
        let mut file = tempfile::NamedTempFile::new().expect("temporary file");
        writeln!(file, "security.protocol=SASL_SSL").expect("write properties");
        let values = load_properties(file.path()).expect("load properties");
        assert_eq!(
            values.get("security.protocol").map(String::as_str),
            Some("SASL_SSL")
        );
    }

    #[test]
    fn normalize_key_should_map_java_truststore() {
        assert_eq!(normalize_key("ssl.truststore.location"), "ssl.ca.location");
    }

    #[test]
    fn protocol_auth_should_build_scram_sha_512() {
        let values = HashMap::from([
            ("security.protocol".into(), "SASL_PLAINTEXT".into()),
            ("sasl.mechanism".into(), "SCRAM-SHA-512".into()),
            ("sasl.username".into(), "alice".into()),
            ("sasl.password".into(), "secret".into()),
        ]);
        assert!(protocol_auth(&values).is_ok());
    }
}
