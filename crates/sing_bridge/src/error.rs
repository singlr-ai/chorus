use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteFailureKind {
    MissingConfig,
    NotFound,
    PermissionDenied,
    ProjectStopped,
    Validation,
    MalformedData,
    Unavailable,
    Unknown,
}

impl RemoteFailureKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MissingConfig => "missing_config",
            Self::NotFound => "not_found",
            Self::PermissionDenied => "permission_denied",
            Self::ProjectStopped => "project_stopped",
            Self::Validation => "validation",
            Self::MalformedData => "malformed_data",
            Self::Unavailable => "unavailable",
            Self::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for RemoteFailureKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("remote command {command} failed ({kind})")]
pub struct RemoteFailure {
    pub command: String,
    pub kind: RemoteFailureKind,
    pub exit_status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SingCommandError {
    #[error("ssh is not installed or not on PATH")]
    SshUnavailable,
    #[error("failed to start ssh for {command}: {message}")]
    SpawnFailed { command: String, message: String },
    #[error("invalid command arguments for {command}: {message}")]
    InvalidCommand { command: String, message: String },
    #[error("sing command timed out after {timeout:?}: {command}")]
    Timeout { command: String, timeout: Duration },
    #[error("ssh authentication failed for {host}")]
    AuthenticationFailed { host: String, stderr: String },
    #[error("unable to reach sing host {host}")]
    ConnectionFailed { host: String, stderr: String },
    #[error(transparent)]
    RemoteFailure(#[from] RemoteFailure),
}

#[derive(Debug, Error)]
pub enum SingBridgeError {
    #[error("sing client config not found at {path}")]
    ConfigNotFound { path: PathBuf },
    #[error("failed to read sing client config at {path}: {message}")]
    ConfigRead { path: PathBuf, message: String },
    #[error("invalid sing client config at {path}: {message}")]
    InvalidConfig { path: PathBuf, message: String },
    #[error("invalid {field}: {message}")]
    InvalidInput {
        field: &'static str,
        message: String,
    },
    #[error("project {project} is not running ({status})")]
    ProjectNotRunning { project: String, status: String },
    #[error("project {project} has no container IP address")]
    MissingContainerAddress { project: String },
    #[error("failed to run {command}")]
    Command {
        command: String,
        #[source]
        source: SingCommandError,
    },
    #[error("failed to decode {command} response: {source}")]
    InvalidResponse {
        command: String,
        output: String,
        #[source]
        source: serde_json::Error,
    },
}

impl SingBridgeError {
    pub(crate) fn invalid_input(
        field: &'static str,
        message: impl Into<String>,
    ) -> SingBridgeError {
        Self::InvalidInput {
            field,
            message: message.into(),
        }
    }

    pub(crate) fn command(command: impl Into<String>, source: SingCommandError) -> SingBridgeError {
        Self::Command {
            command: command.into(),
            source,
        }
    }
}
