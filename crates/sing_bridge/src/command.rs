use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::{FutureExt as _, pin_mut, select_biased};
use remote::SshConnectionOptions;
use util::{
    command::{Stdio, new_command},
    shell::ShellKind,
};

use crate::error::{RemoteFailure, RemoteFailureKind, SingCommandError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRequest {
    pub display_name: String,
    pub args: Vec<String>,
    pub timeout: Duration,
}

impl CommandRequest {
    pub fn new(display_name: impl Into<String>, args: Vec<String>, timeout: Duration) -> Self {
        Self {
            display_name: display_name.into(),
            args,
            timeout,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub exit_status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[async_trait]
pub trait SingCommandRunner: Send + Sync {
    async fn run(&self, request: CommandRequest) -> Result<CommandOutput, SingCommandError>;
}

#[derive(Debug, Clone)]
pub struct SshSingCommandRunner {
    host: SshConnectionOptions,
    remote_program: String,
    subprocess: SubprocessRunner,
}

impl SshSingCommandRunner {
    pub fn new(host: SshConnectionOptions) -> Self {
        Self {
            host,
            remote_program: "sing".to_string(),
            subprocess: SubprocessRunner,
        }
    }

    fn build_ssh_args(&self, request: &CommandRequest) -> Result<Vec<String>, SingCommandError> {
        let mut args = self.host.additional_args();
        args.push("-T".to_string());
        args.push("-o".to_string());
        args.push("BatchMode=yes".to_string());
        args.push("-o".to_string());
        args.push(format!(
            "ConnectTimeout={}",
            connect_timeout_seconds(request.timeout)
        ));
        args.push(self.host.ssh_destination());
        args.push(build_remote_command(
            &self.remote_program,
            &request.display_name,
            &request.args,
        )?);
        Ok(args)
    }

    fn classify_process_failure(
        &self,
        request: &CommandRequest,
        output: CommandOutput,
    ) -> SingCommandError {
        let normalized = output.stderr.to_ascii_lowercase();
        if output.exit_status == Some(255) && is_auth_failure(&normalized) {
            return SingCommandError::AuthenticationFailed {
                host: self.host.connection_string(),
                stderr: output.stderr,
            };
        }

        if output.exit_status == Some(255) && is_connectivity_failure(&normalized) {
            return SingCommandError::ConnectionFailed {
                host: self.host.connection_string(),
                stderr: output.stderr,
            };
        }

        SingCommandError::RemoteFailure(RemoteFailure {
            command: request.display_name.clone(),
            kind: classify_remote_failure_kind(&output.stdout, &output.stderr),
            exit_status: output.exit_status,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

#[async_trait]
impl SingCommandRunner for SshSingCommandRunner {
    async fn run(&self, request: CommandRequest) -> Result<CommandOutput, SingCommandError> {
        let args = self.build_ssh_args(&request)?;
        log::debug!(
            "running sing command {} via {}",
            request.display_name,
            self.host.connection_string()
        );

        match self
            .subprocess
            .run("ssh", &args, &request.display_name, request.timeout)
            .await
        {
            Ok(output) => Ok(output),
            Err(ProcessError::SpawnFailed { message, .. }) => {
                if message.contains("No such file") || message.contains("not found") {
                    Err(SingCommandError::SshUnavailable)
                } else {
                    Err(SingCommandError::SpawnFailed {
                        command: request.display_name,
                        message,
                    })
                }
            }
            Err(ProcessError::Timeout { timeout, .. }) => Err(SingCommandError::Timeout {
                command: request.display_name,
                timeout,
            }),
            Err(ProcessError::Exited { output, .. }) => {
                Err(self.classify_process_failure(&request, output))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct SubprocessRunner;

#[derive(Debug)]
enum ProcessError {
    SpawnFailed { message: String },
    Timeout { timeout: Duration },
    Exited { output: CommandOutput },
}

impl SubprocessRunner {
    async fn run(
        &self,
        program: &str,
        args: &[String],
        display_name: &str,
        timeout: Duration,
    ) -> Result<CommandOutput, ProcessError> {
        let mut command = new_command(program);
        command
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .args(args);

        let child = command.spawn().map_err(|error| ProcessError::SpawnFailed {
            message: error.to_string(),
        })?;

        let output = child.output().fuse();
        let timer = async_io::Timer::at(Instant::now() + timeout).fuse();
        pin_mut!(output, timer);

        let output = select_biased! {
            result = output => {
                result.map_err(|error| ProcessError::SpawnFailed {
                    message: error.to_string(),
                })?
            }
            _ = timer => {
                log::warn!("sing command {} timed out after {:?}", display_name, timeout);
                return Err(ProcessError::Timeout { timeout });
            }
        };

        let output = CommandOutput {
            exit_status: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        };

        if output.exit_status == Some(0) {
            Ok(output)
        } else {
            Err(ProcessError::Exited { output })
        }
    }
}

fn connect_timeout_seconds(timeout: Duration) -> u64 {
    timeout.as_secs().clamp(1, 30)
}

fn build_remote_command(
    remote_program: &str,
    display_name: &str,
    args: &[String],
) -> Result<String, SingCommandError> {
    let program = ShellKind::Posix.try_quote(remote_program).ok_or_else(|| {
        SingCommandError::InvalidCommand {
            command: display_name.to_string(),
            message: "remote program could not be shell-quoted".to_string(),
        }
    })?;

    let mut command = format!("exec {program}");
    for arg in args {
        let quoted =
            ShellKind::Posix
                .try_quote(arg)
                .ok_or_else(|| SingCommandError::InvalidCommand {
                    command: display_name.to_string(),
                    message: "command argument could not be shell-quoted".to_string(),
                })?;
        command.push(' ');
        command.push_str(&quoted);
    }
    Ok(command)
}

fn is_auth_failure(stderr: &str) -> bool {
    stderr.contains("permission denied")
        || stderr.contains("publickey")
        || stderr.contains("authentication failed")
        || stderr.contains("sign_and_send_pubkey")
        || stderr.contains("no supported authentication methods available")
}

fn is_connectivity_failure(stderr: &str) -> bool {
    stderr.contains("host key verification failed")
        || stderr.contains("could not resolve hostname")
        || stderr.contains("connection timed out")
        || stderr.contains("operation timed out")
        || stderr.contains("no route to host")
        || stderr.contains("connection refused")
        || stderr.contains("connection closed by remote host")
        || stderr.contains("kex_exchange_identification")
        || stderr.contains("network is unreachable")
        || stderr.contains("connection reset by peer")
}

fn classify_remote_failure_kind(stdout: &str, stderr: &str) -> RemoteFailureKind {
    let combined = format!("{stdout}\n{stderr}").to_ascii_lowercase();

    if combined.contains("root privileges required") {
        return RemoteFailureKind::PermissionDenied;
    }

    if combined.contains("is stopped. start it with: sing up") {
        return RemoteFailureKind::ProjectStopped;
    }

    if combined.contains("client config not found")
        || combined.contains("project descriptor not found")
        || combined.contains("no specs_dir configured")
        || combined.contains("server not initialized")
    {
        return RemoteFailureKind::MissingConfig;
    }

    if combined.contains("does not exist")
        || combined.contains("not found in index.yaml")
        || combined.contains("no agent log found")
    {
        return RemoteFailureKind::NotFound;
    }

    if combined.contains("failed to parse") || combined.contains("malformed") {
        return RemoteFailureKind::MalformedData;
    }

    if combined.contains("command not found") {
        return RemoteFailureKind::Unavailable;
    }

    if combined.contains("invalid ")
        || combined.contains(" is required")
        || combined.contains("must match ")
        || combined.contains("must be one of")
    {
        return RemoteFailureKind::Validation;
    }

    if combined.contains("permission denied") {
        return RemoteFailureKind::PermissionDenied;
    }

    RemoteFailureKind::Unknown
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pretty_assertions::assert_eq;
    use smol::block_on;

    use super::*;

    #[test]
    fn build_remote_command_quotes_arguments() {
        let args = vec![
            "spec".to_string(),
            "create".to_string(),
            "demo".to_string(),
            "--title".to_string(),
            "Fix \"quoted\" title".to_string(),
        ];

        let command = build_remote_command("sing", "spec create", &args).unwrap();

        assert_eq!(
            command,
            "exec sing spec create demo --title 'Fix \"quoted\" title'"
        );
    }

    #[test]
    fn classifies_remote_failures() {
        assert_eq!(
            classify_remote_failure_kind(
                "",
                "Project 'demo' is stopped. Start it with: sing up demo"
            ),
            RemoteFailureKind::ProjectStopped
        );
        assert_eq!(
            classify_remote_failure_kind(
                "",
                "Root privileges required. Run with: sudo sing host status"
            ),
            RemoteFailureKind::PermissionDenied
        );
        assert_eq!(
            classify_remote_failure_kind("", "Spec 'demo' not found in index.yaml"),
            RemoteFailureKind::NotFound
        );
    }

    #[test]
    fn subprocess_runner_collects_output() {
        block_on(async {
            let runner = SubprocessRunner;
            let output = runner
                .run(
                    "sh",
                    &[
                        "-c".to_string(),
                        "printf 'hello'; printf 'oops' 1>&2".to_string(),
                    ],
                    "test",
                    Duration::from_secs(2),
                )
                .await
                .unwrap();

            assert_eq!(output.stdout, "hello");
            assert_eq!(output.stderr, "oops");
            assert_eq!(output.exit_status, Some(0));
        });
    }

    #[test]
    fn subprocess_runner_times_out() {
        block_on(async {
            let runner = SubprocessRunner;
            let error = runner
                .run(
                    "sh",
                    &["-c".to_string(), "sleep 5".to_string()],
                    "test",
                    Duration::from_millis(50),
                )
                .await
                .unwrap_err();

            assert!(matches!(error, ProcessError::Timeout { .. }));
        });
    }
}
