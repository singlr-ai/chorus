use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use remote::SshConnectionOptions;
use serde::Deserialize;
use util::paths::home_dir;

use crate::error::SingBridgeError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SingClientConfig {
    pub host: String,
}

#[derive(Debug, Deserialize)]
struct RawSingClientConfig {
    host: Option<String>,
}

impl SingClientConfig {
    pub fn default_path() -> PathBuf {
        home_dir().join(".sing").join("config.yaml")
    }

    pub fn load() -> Result<Self, SingBridgeError> {
        Self::load_from(Self::default_path())
    }

    pub fn load_from(path: impl AsRef<Path>) -> Result<Self, SingBridgeError> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path).map_err(|error| match error.kind() {
            ErrorKind::NotFound => SingBridgeError::ConfigNotFound {
                path: path.to_path_buf(),
            },
            _ => SingBridgeError::ConfigRead {
                path: path.to_path_buf(),
                message: error.to_string(),
            },
        })?;

        let raw: RawSingClientConfig =
            serde_yaml::from_str(&content).map_err(|error| SingBridgeError::InvalidConfig {
                path: path.to_path_buf(),
                message: error.to_string(),
            })?;

        let host = raw.host.unwrap_or_default().trim().to_string();
        if host.is_empty() {
            return Err(SingBridgeError::InvalidConfig {
                path: path.to_path_buf(),
                message: "host must not be blank".to_string(),
            });
        }

        Ok(Self { host })
    }

    pub(crate) fn parse_ssh_options(&self) -> Result<SshConnectionOptions, String> {
        if self.host.trim_start().starts_with("ssh ") {
            return Err("host must be an ssh destination, not a full ssh command".to_string());
        }

        let mut options = SshConnectionOptions::parse_command_line(&self.host)
            .map_err(|error| error.to_string())?;

        if options.args.as_ref().is_some_and(|args| !args.is_empty())
            || options.port_forwards.is_some()
        {
            return Err(
                "host must be a hostname, IP, ssh alias, or user@host[:port] without extra ssh flags"
                    .to_string(),
            );
        }

        options.nickname = None;
        options.password = None;
        options.upload_binary_over_ssh = false;
        options.connection_timeout = None;
        Ok(options)
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn load_from_reads_host() {
        let tempdir = tempdir().unwrap();
        let path = tempdir.path().join("config.yaml");
        std::fs::write(&path, "host: sing-host\n").unwrap();

        let config = SingClientConfig::load_from(&path).unwrap();

        assert_eq!(config.host, "sing-host");
    }

    #[test]
    fn load_from_requires_host() {
        let tempdir = tempdir().unwrap();
        let path = tempdir.path().join("config.yaml");
        std::fs::write(&path, "host: \"\"\n").unwrap();

        let error = SingClientConfig::load_from(&path).unwrap_err();

        assert!(matches!(error, SingBridgeError::InvalidConfig { .. }));
    }

    #[test]
    fn parse_ssh_options_rejects_full_command() {
        let config = SingClientConfig {
            host: "ssh -J proxy host".to_string(),
        };

        let error = config.parse_ssh_options().unwrap_err();

        assert_eq!(
            error,
            "host must be an ssh destination, not a full ssh command"
        );
    }

    #[test]
    fn parse_ssh_options_supports_user_and_port() {
        let config = SingClientConfig {
            host: "dev@example.com:2222".to_string(),
        };

        let options = config.parse_ssh_options().unwrap();

        assert_eq!(options.connection_string(), "dev@example.com:2222");
    }
}
