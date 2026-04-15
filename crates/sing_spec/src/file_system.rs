use std::{
    io::Write,
    path::{Path, PathBuf},
    process,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use futures::{FutureExt as _, pin_mut, select_biased};
use remote::{RemoteConnectionOptions, SshConnectionOptions};
use sing_bridge::ProjectRemoteTarget;
use tempfile::NamedTempFile;
use util::{
    command::{Stdio, new_command},
    shell::ShellKind,
};

const REMOTE_FILE_TIMEOUT: Duration = Duration::from_secs(30);

#[async_trait]
pub trait SpecFileSystem: Send + Sync {
    async fn read_text(&self, target: &ProjectRemoteTarget, path: &Path) -> Result<Option<String>>;
    async fn write_text_atomic(
        &self,
        target: &ProjectRemoteTarget,
        path: &Path,
        content: &str,
    ) -> Result<()>;
}

#[derive(Debug, Clone)]
pub struct SshSpecFileSystem {
    timeout: Duration,
}

impl Default for SshSpecFileSystem {
    fn default() -> Self {
        Self::new(REMOTE_FILE_TIMEOUT)
    }
}

impl SshSpecFileSystem {
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }

    async fn run_ssh(
        &self,
        options: &SshConnectionOptions,
        program: &str,
        command_args: &[String],
    ) -> Result<ProcessOutput, ProcessFailure> {
        let remote_command = build_remote_command(program, command_args)
            .map_err(|error| ProcessFailure::Spawn(error.to_string()))?;
        let mut args = options.additional_args();
        args.push("-T".to_string());
        args.push("-o".to_string());
        args.push("BatchMode=yes".to_string());
        args.push("-o".to_string());
        args.push(format!(
            "ConnectTimeout={}",
            connect_timeout_seconds(self.timeout)
        ));
        args.push(options.ssh_destination());
        args.push(remote_command);
        run_process("ssh", &args, self.timeout).await
    }

    async fn run_scp(
        &self,
        options: &SshConnectionOptions,
        local_path: &Path,
        remote_path: &Path,
    ) -> Result<(), ProcessFailure> {
        let mut args = options.additional_args_for_scp();
        args.push("-o".to_string());
        args.push("BatchMode=yes".to_string());
        args.push("-o".to_string());
        args.push(format!(
            "ConnectTimeout={}",
            connect_timeout_seconds(self.timeout)
        ));
        if let Some(port) = options.port {
            args.push("-P".to_string());
            args.push(port.to_string());
        }
        args.push(local_path.display().to_string());
        args.push(format!(
            "{}:{}",
            scp_destination(options),
            quote_remote_path(remote_path)
                .map_err(|error| ProcessFailure::Spawn(error.to_string()))?
        ));

        run_process("scp", &args, self.timeout).await.map(|_| ())
    }

    async fn cleanup_remote_temp(
        &self,
        options: &SshConnectionOptions,
        temp_path: &Path,
    ) -> Result<()> {
        let args = vec!["-f".to_string(), "--".to_string(), path_string(temp_path)];
        let _ = self.run_ssh(options, "rm", &args).await;
        Ok(())
    }
}

#[async_trait]
impl SpecFileSystem for SshSpecFileSystem {
    async fn read_text(&self, target: &ProjectRemoteTarget, path: &Path) -> Result<Option<String>> {
        let options = ssh_options(target)?;
        let args = vec!["--".to_string(), path_string(path)];
        match self.run_ssh(&options, "cat", &args).await {
            Ok(output) => Ok(Some(output.stdout)),
            Err(ProcessFailure::Exited(output)) if is_missing_file(&output) => Ok(None),
            Err(error) => {
                Err(error).with_context(|| format!("failed to read remote file {}", path.display()))
            }
        }
    }

    async fn write_text_atomic(
        &self,
        target: &ProjectRemoteTarget,
        path: &Path,
        content: &str,
    ) -> Result<()> {
        let options = ssh_options(target)?;
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("remote path {} has no parent", path.display()))?;
        let temp_path = temporary_remote_path(path)?;

        let mkdir_args = vec!["-p".to_string(), "--".to_string(), path_string(parent)];
        self.run_ssh(&options, "mkdir", &mkdir_args)
            .await
            .with_context(|| format!("failed to create remote directory {}", parent.display()))?;

        let mut temp_file = NamedTempFile::new().context("failed to create local temp file")?;
        temp_file
            .write_all(content.as_bytes())
            .context("failed to write local temp file")?;
        temp_file
            .flush()
            .context("failed to flush local temp file")?;

        if let Err(error) = self.run_scp(&options, temp_file.path(), &temp_path).await {
            return Err(error).with_context(|| {
                format!(
                    "failed to upload temporary remote file {}",
                    temp_path.display()
                )
            });
        }

        let move_args = vec!["--".to_string(), path_string(&temp_path), path_string(path)];
        if let Err(error) = self.run_ssh(&options, "mv", &move_args).await {
            let _ = self.cleanup_remote_temp(&options, &temp_path).await;
            return Err(error).with_context(|| {
                format!(
                    "failed to atomically replace remote file {}",
                    path.display()
                )
            });
        }

        Ok(())
    }
}

fn ssh_options(target: &ProjectRemoteTarget) -> Result<SshConnectionOptions> {
    match &target.connection_options {
        RemoteConnectionOptions::Ssh(options) => Ok(options.clone()),
        connection => bail!(
            "spec file access requires ssh transport, found {}",
            connection.display_name()
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcessOutput {
    exit_status: Option<i32>,
    stdout: String,
    stderr: String,
}

#[derive(Debug)]
enum ProcessFailure {
    Spawn(String),
    Timeout(Duration),
    Exited(ProcessOutput),
}

impl std::fmt::Display for ProcessFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(message) => write!(f, "{message}"),
            Self::Timeout(timeout) => write!(f, "timed out after {timeout:?}"),
            Self::Exited(output) => {
                let stderr = output.stderr.trim();
                let stdout = output.stdout.trim();
                if !stderr.is_empty() {
                    write!(f, "remote command failed: {stderr}")
                } else if !stdout.is_empty() {
                    write!(f, "remote command failed: {stdout}")
                } else {
                    write!(
                        f,
                        "remote command failed with exit status {:?}",
                        output.exit_status
                    )
                }
            }
        }
    }
}

impl std::error::Error for ProcessFailure {}

async fn run_process(
    program: &str,
    args: &[String],
    timeout: Duration,
) -> Result<ProcessOutput, ProcessFailure> {
    let mut command = new_command(program);
    command
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .args(args);

    let child = command
        .spawn()
        .map_err(|error| ProcessFailure::Spawn(error.to_string()))?;

    let output = child.output().fuse();
    let timer = smol::Timer::after(timeout).fuse();
    pin_mut!(output, timer);

    let output = select_biased! {
        result = output => {
            result.map_err(|error| ProcessFailure::Spawn(error.to_string()))?
        }
        _ = timer => {
            return Err(ProcessFailure::Timeout(timeout));
        }
    };

    let output = ProcessOutput {
        exit_status: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    };

    if output.exit_status == Some(0) {
        Ok(output)
    } else {
        Err(ProcessFailure::Exited(output))
    }
}

fn build_remote_command(program: &str, args: &[String]) -> Result<String> {
    let program = ShellKind::Posix
        .try_quote(program)
        .ok_or_else(|| anyhow!("remote program could not be shell-quoted"))?;
    let mut command = format!("exec {program}");
    for arg in args {
        let arg = ShellKind::Posix
            .try_quote(arg)
            .ok_or_else(|| anyhow!("remote command argument could not be shell-quoted"))?;
        command.push(' ');
        command.push_str(&arg);
    }
    Ok(command)
}

fn scp_destination(options: &SshConnectionOptions) -> String {
    if let Some(username) = &options.username {
        format!("{}@{}", username, options.host.to_bracketed_string())
    } else {
        options.host.to_bracketed_string()
    }
}

fn quote_remote_path(path: &Path) -> Result<String> {
    ShellKind::Posix
        .try_quote(&path_string(path))
        .map(|path| path.into_owned())
        .ok_or_else(|| anyhow!("remote path could not be shell-quoted"))
}

fn temporary_remote_path(path: &Path) -> Result<PathBuf> {
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow!("remote path {} has no file name", path.display()))?
        .to_string_lossy();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before unix epoch")?
        .as_nanos();
    Ok(path.with_file_name(format!(".{file_name}.chorus-tmp-{}-{nonce}", process::id())))
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn connect_timeout_seconds(timeout: Duration) -> u64 {
    timeout.as_secs().clamp(1, 30)
}

fn is_missing_file(output: &ProcessOutput) -> bool {
    let combined = format!("{}\n{}", output.stdout, output.stderr).to_ascii_lowercase();
    combined.contains("no such file or directory")
        || combined.contains("cannot open")
        || combined.contains("can't open")
        || combined.contains("cannot stat")
}
