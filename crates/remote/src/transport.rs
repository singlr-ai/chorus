use std::{
    io::Write,
    path::{Path, PathBuf},
};

use crate::{
    RemoteArch, RemoteOs, RemotePlatform,
    json_log::LogRecord,
    protocol::{MESSAGE_LEN_SIZE, message_len_from_buffer, read_message_with_len, write_message},
};
use anyhow::{Context as _, Result};
use futures::{
    AsyncReadExt as _, FutureExt as _, StreamExt as _,
    channel::mpsc::{Sender, UnboundedReceiver, UnboundedSender},
};
use gpui::{AppContext as _, AsyncApp, Task};
use release_channel::ReleaseChannel;
use rpc::proto::Envelope;
use semver::Version;
use util::command::Child;

pub mod docker;
#[cfg(any(test, feature = "test-support"))]
pub mod mock;
pub mod ssh;
pub mod wsl;

/// Parses the output of `uname -sm` to determine the remote platform.
/// Takes the last line to skip possible shell initialization output.
fn parse_platform(output: &str) -> Result<RemotePlatform> {
    let output = output.trim();
    let uname = output.rsplit_once('\n').map_or(output, |(_, last)| last);
    let Some((os, arch)) = uname.split_once(" ") else {
        anyhow::bail!("unknown uname: {uname:?}")
    };

    let os = match os {
        "Darwin" => RemoteOs::MacOs,
        "Linux" => RemoteOs::Linux,
        _ => anyhow::bail!(
            "Prebuilt remote servers are not yet available for {os:?}. See https://zed.dev/docs/remote-development"
        ),
    };

    // exclude armv5,6,7 as they are 32-bit.
    let arch = if arch.starts_with("armv8")
        || arch.starts_with("armv9")
        || arch.starts_with("arm64")
        || arch.starts_with("aarch64")
    {
        RemoteArch::Aarch64
    } else if arch.starts_with("x86") {
        RemoteArch::X86_64
    } else {
        anyhow::bail!(
            "Prebuilt remote servers are not yet available for {arch:?}. See https://zed.dev/docs/remote-development"
        )
    };

    Ok(RemotePlatform { os, arch })
}

/// Parses the output of `echo $SHELL` to determine the remote shell.
/// Takes the last line to skip possible shell initialization output.
fn parse_shell(output: &str, fallback_shell: &str) -> String {
    let output = output.trim();
    let shell = output.rsplit_once('\n').map_or(output, |(_, last)| last);
    if shell.is_empty() {
        log::error!("$SHELL is not set, falling back to {fallback_shell}");
        fallback_shell.to_owned()
    } else {
        shell.to_owned()
    }
}

fn handle_rpc_messages_over_child_process_stdio(
    mut remote_proxy_process: Child,
    incoming_tx: UnboundedSender<Envelope>,
    mut outgoing_rx: UnboundedReceiver<Envelope>,
    mut connection_activity_tx: Sender<()>,
    cx: &AsyncApp,
) -> Task<Result<i32>> {
    let mut child_stderr = remote_proxy_process.stderr.take().unwrap();
    let mut child_stdout = remote_proxy_process.stdout.take().unwrap();
    let mut child_stdin = remote_proxy_process.stdin.take().unwrap();

    let mut stdin_buffer = Vec::new();
    let mut stdout_buffer = Vec::new();
    let mut stderr_buffer = Vec::new();
    let mut stderr_offset = 0;

    let stdin_task = cx.background_spawn(async move {
        while let Some(outgoing) = outgoing_rx.next().await {
            write_message(&mut child_stdin, &mut stdin_buffer, outgoing).await?;
        }
        anyhow::Ok(())
    });

    let stdout_task = cx.background_spawn({
        let mut connection_activity_tx = connection_activity_tx.clone();
        async move {
            loop {
                stdout_buffer.resize(MESSAGE_LEN_SIZE, 0);
                let len = child_stdout.read(&mut stdout_buffer).await?;

                if len == 0 {
                    return anyhow::Ok(());
                }

                if len < MESSAGE_LEN_SIZE {
                    child_stdout.read_exact(&mut stdout_buffer[len..]).await?;
                }

                let message_len = message_len_from_buffer(&stdout_buffer);
                let envelope =
                    read_message_with_len(&mut child_stdout, &mut stdout_buffer, message_len)
                        .await?;
                connection_activity_tx.try_send(()).ok();
                incoming_tx.unbounded_send(envelope).ok();
            }
        }
    });

    let stderr_task: Task<anyhow::Result<()>> = cx.background_spawn(async move {
        loop {
            stderr_buffer.resize(stderr_offset + 1024, 0);

            let len = child_stderr
                .read(&mut stderr_buffer[stderr_offset..])
                .await?;
            if len == 0 {
                return anyhow::Ok(());
            }

            stderr_offset += len;
            let mut start_ix = 0;
            while let Some(ix) = stderr_buffer[start_ix..stderr_offset]
                .iter()
                .position(|b| b == &b'\n')
            {
                let line_ix = start_ix + ix;
                let content = &stderr_buffer[start_ix..line_ix];
                start_ix = line_ix + 1;
                if let Ok(record) = serde_json::from_slice::<LogRecord>(content) {
                    record.log(log::logger())
                } else {
                    std::io::stderr()
                        .write_fmt(format_args!(
                            "(remote) {}\n",
                            String::from_utf8_lossy(content)
                        ))
                        .ok();
                }
            }
            stderr_buffer.drain(0..start_ix);
            stderr_offset -= start_ix;

            connection_activity_tx.try_send(()).ok();
        }
    });

    cx.background_spawn(async move {
        let result = futures::select! {
            result = stdin_task.fuse() => {
                result.context("stdin")
            }
            result = stdout_task.fuse() => {
                result.context("stdout")
            }
            result = stderr_task.fuse() => {
                result.context("stderr")
            }
        };
        let exit_status = remote_proxy_process.status().await?;
        let status = exit_status.code().unwrap_or_else(|| {
            #[cfg(unix)]
            let status = std::os::unix::process::ExitStatusExt::signal(&exit_status).unwrap_or(1);
            #[cfg(not(unix))]
            let status = 1;
            status
        });
        match result {
            Ok(_) => Ok(status),
            Err(error) => Err(error),
        }
    })
}

pub(crate) fn remote_server_binary_name(
    release_channel: ReleaseChannel,
    version: &Version,
) -> String {
    let version_str = match release_channel {
        ReleaseChannel::Dev => "build".to_string(),
        _ => version.to_string(),
    };
    format!(
        "chorus-remote-server-{}-{version_str}",
        release_channel.dev_name()
    )
}

pub(crate) fn legacy_remote_server_binary_name(
    release_channel: ReleaseChannel,
    version: &Version,
) -> String {
    let version_str = match release_channel {
        ReleaseChannel::Dev => "build".to_string(),
        _ => version.to_string(),
    };
    format!(
        "zed-remote-server-{}-{version_str}",
        release_channel.dev_name()
    )
}

pub(crate) fn copy_remote_server_override() -> Option<PathBuf> {
    std::env::var("CHORUS_COPY_REMOTE_SERVER")
        .or_else(|_| std::env::var("ZED_COPY_REMOTE_SERVER"))
        .ok()
        .map(PathBuf::from)
}

pub(crate) fn build_remote_server_mode() -> String {
    std::env::var("CHORUS_BUILD_REMOTE_SERVER")
        .or_else(|_| std::env::var("ZED_BUILD_REMOTE_SERVER"))
        .unwrap_or_else(|_| "nocompress".into())
}

fn zstd_musl_lib_dir() -> Option<String> {
    std::env::var("CHORUS_ZSTD_MUSL_LIB")
        .or_else(|_| std::env::var("ZED_ZSTD_MUSL_LIB"))
        .ok()
}

pub(crate) fn packaged_remote_server_path(platform: RemotePlatform) -> Option<PathBuf> {
    let current_exe = std::env::current_exe().ok()?;
    let current_dir = current_exe.parent()?;
    let archive_name = bundled_remote_server_archive_name(platform);
    packaged_remote_server_locations(current_dir, &archive_name)
        .into_iter()
        .find(|candidate| candidate.exists())
}

fn bundled_remote_server_archive_name(platform: RemotePlatform) -> String {
    let extension = if platform.os.is_windows() {
        "zip"
    } else {
        "gz"
    };
    format!(
        "chorus-remote-server-{}-{}.{}",
        platform.os.as_str(),
        platform.arch.as_str(),
        extension
    )
}

fn packaged_remote_server_locations(current_dir: &Path, archive_name: &str) -> Vec<PathBuf> {
    let mut locations = Vec::new();

    if cfg!(target_os = "macos") {
        locations.push(
            current_dir
                .join("../Resources/remote-servers")
                .join(archive_name),
        );
        locations.push(current_dir.join("remote-servers").join(archive_name));
    } else if cfg!(any(target_os = "linux", target_os = "freebsd")) {
        locations.push(
            current_dir
                .join("../libexec/remote-servers")
                .join(archive_name),
        );
        locations.push(current_dir.join("remote-servers").join(archive_name));
    } else if cfg!(target_os = "windows") {
        locations.push(current_dir.join("remote-servers").join(archive_name));
    }

    locations.push(current_dir.join(archive_name));
    locations
}

#[cfg(any(debug_assertions, feature = "build-remote-server-binary"))]
async fn build_remote_server_from_source(
    platform: &crate::RemotePlatform,
    delegate: &dyn crate::RemoteClientDelegate,
    binary_exists_on_server: bool,
    cx: &mut AsyncApp,
) -> Result<Option<std::path::PathBuf>> {
    use std::env::VarError;
    use std::path::Path;
    use util::command::{Command, Stdio, new_command};

    if let Some(path) = copy_remote_server_override() {
        if path.exists() {
            return Ok(Some(path));
        } else {
            log::warn!(
                "CHORUS_COPY_REMOTE_SERVER path does not exist, falling back to CHORUS_BUILD_REMOTE_SERVER: {}",
                path.display()
            );
        }
    }

    // By default, we make building remote server from source opt-out and we do not force artifact compression
    // for quicker builds.
    let build_remote_server = build_remote_server_mode();

    if let "never" = &*build_remote_server {
        return Ok(None);
    } else if let "false" | "no" | "off" | "0" = &*build_remote_server {
        if binary_exists_on_server {
            return Ok(None);
        }
        log::warn!(
            "CHORUS_BUILD_REMOTE_SERVER is disabled, but no server binary exists on the server"
        )
    }

    async fn run_cmd(command: &mut Command) -> Result<()> {
        let output = command
            .kill_on_drop(true)
            .stdout(Stdio::inherit())
            .output()
            .await?;
        anyhow::ensure!(
            output.status.success(),
            "Failed to run command: {command:?}: output: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }

    let use_musl = !build_remote_server.contains("nomusl");
    let triple = format!(
        "{}-{}",
        platform.arch,
        match platform.os {
            RemoteOs::Linux =>
                if use_musl {
                    "unknown-linux-musl"
                } else {
                    "unknown-linux-gnu"
                },
            RemoteOs::MacOs => "apple-darwin",
            RemoteOs::Windows if cfg!(windows) => "pc-windows-msvc",
            RemoteOs::Windows => "pc-windows-gnu",
        }
    );
    let mut rust_flags = match std::env::var("RUSTFLAGS") {
        Ok(val) => val,
        Err(VarError::NotPresent) => String::new(),
        Err(e) => {
            log::error!("Failed to get env var `RUSTFLAGS` value: {e}");
            String::new()
        }
    };
    if platform.os == RemoteOs::Linux && use_musl {
        rust_flags.push_str(" -C target-feature=+crt-static");

        if let Some(path) = zstd_musl_lib_dir() {
            rust_flags.push_str(&format!(" -C link-arg=-L{path}"));
        }
    }
    if platform.arch.as_str() == std::env::consts::ARCH
        && platform.os.as_str() == std::env::consts::OS
    {
        delegate.set_status(Some("Building remote server binary from source"), cx);
        log::info!("building remote server binary from source");
        run_cmd(
            new_command("cargo")
                .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
                .args([
                    "build",
                    "--package",
                    "remote_server",
                    "--features",
                    "debug-embed",
                    "--target-dir",
                    "target/remote_server",
                    "--target",
                    &triple,
                ])
                .env("RUSTFLAGS", &rust_flags),
        )
        .await?;
    } else {
        if which("zig", cx).await?.is_none() {
            anyhow::bail!(if cfg!(not(windows)) {
                "zig not found on $PATH, install zig (see https://ziglang.org/learn/getting-started or use zigup)"
            } else {
                "zig not found on $PATH, install zig (use `winget install -e --id zig.zig` or see https://ziglang.org/learn/getting-started or use zigup)"
            });
        }

        let rustup = which("rustup", cx)
            .await?
            .context("rustup not found on $PATH, install rustup (see https://rustup.rs/)")?;
        delegate.set_status(Some("Adding rustup target for cross-compilation"), cx);
        log::info!("adding rustup target");
        run_cmd(new_command(rustup).args(["target", "add"]).arg(&triple)).await?;

        if which("cargo-zigbuild", cx).await?.is_none() {
            delegate.set_status(Some("Installing cargo-zigbuild for cross-compilation"), cx);
            log::info!("installing cargo-zigbuild");
            run_cmd(new_command("cargo").args(["install", "--locked", "cargo-zigbuild"])).await?;
        }

        delegate.set_status(
            Some(&format!(
                "Building remote binary from source for {triple} with Zig"
            )),
            cx,
        );
        log::info!("building remote binary from source for {triple} with Zig");
        run_cmd(
            new_command("cargo")
                .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
                .args([
                    "zigbuild",
                    "--package",
                    "remote_server",
                    "--features",
                    "debug-embed",
                    "--target-dir",
                    "target/remote_server",
                    "--target",
                    &triple,
                ])
                .env("RUSTFLAGS", &rust_flags),
        )
        .await?;
    };
    let bin_path = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .join("target")
        .join("remote_server")
        .join(&triple)
        .join("debug")
        .join("remote_server")
        .with_extension(if platform.os.is_windows() { "exe" } else { "" });

    let path = if !build_remote_server.contains("nocompress") {
        delegate.set_status(Some("Compressing binary"), cx);

        #[cfg(not(target_os = "windows"))]
        let archive_path = {
            run_cmd(new_command("gzip").arg("-f").arg(&bin_path)).await?;
            bin_path.with_extension("gz")
        };

        #[cfg(target_os = "windows")]
        let archive_path = {
            let zip_path = bin_path.with_extension("zip");
            if smol::fs::metadata(&zip_path).await.is_ok() {
                smol::fs::remove_file(&zip_path).await?;
            }
            let compress_command = format!(
                "Compress-Archive -Path '{}' -DestinationPath '{}' -Force",
                bin_path.display(),
                zip_path.display(),
            );
            run_cmd(new_command("powershell.exe").args([
                "-NoProfile",
                "-Command",
                &compress_command,
            ]))
            .await?;
            zip_path
        };

        std::env::current_dir()?.join(archive_path)
    } else {
        bin_path
    };

    Ok(Some(path))
}

#[cfg(any(debug_assertions, feature = "build-remote-server-binary"))]
async fn which(
    binary_name: impl AsRef<str>,
    cx: &mut AsyncApp,
) -> Result<Option<std::path::PathBuf>> {
    let binary_name = binary_name.as_ref().to_string();
    let binary_name_cloned = binary_name.clone();
    let res = cx
        .background_spawn(async move { which::which(binary_name_cloned) })
        .await;
    match res {
        Ok(path) => Ok(Some(path)),
        Err(which::Error::CannotFindBinaryPath) => Ok(None),
        Err(err) => Err(anyhow::anyhow!(
            "Failed to run 'which' to find the binary '{binary_name}': {err}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn test_parse_platform() {
        let result = parse_platform("Linux x86_64\n").unwrap();
        assert_eq!(result.os, RemoteOs::Linux);
        assert_eq!(result.arch, RemoteArch::X86_64);

        let result = parse_platform("Darwin arm64\n").unwrap();
        assert_eq!(result.os, RemoteOs::MacOs);
        assert_eq!(result.arch, RemoteArch::Aarch64);

        let result = parse_platform("Linux x86_64").unwrap();
        assert_eq!(result.os, RemoteOs::Linux);
        assert_eq!(result.arch, RemoteArch::X86_64);

        let result = parse_platform("some shell init output\nLinux aarch64\n").unwrap();
        assert_eq!(result.os, RemoteOs::Linux);
        assert_eq!(result.arch, RemoteArch::Aarch64);

        let result = parse_platform("some shell init output\nLinux aarch64").unwrap();
        assert_eq!(result.os, RemoteOs::Linux);
        assert_eq!(result.arch, RemoteArch::Aarch64);

        assert_eq!(
            parse_platform("Linux armv8l\n").unwrap().arch,
            RemoteArch::Aarch64
        );
        assert_eq!(
            parse_platform("Linux aarch64\n").unwrap().arch,
            RemoteArch::Aarch64
        );
        assert_eq!(
            parse_platform("Linux x86_64\n").unwrap().arch,
            RemoteArch::X86_64
        );

        let result = parse_platform(
            r#"Linux x86_64 - What you're referring to as Linux, is in fact, GNU/Linux...\n"#,
        )
        .unwrap();
        assert_eq!(result.os, RemoteOs::Linux);
        assert_eq!(result.arch, RemoteArch::X86_64);

        assert!(parse_platform("Windows x86_64\n").is_err());
        assert!(parse_platform("Linux armv7l\n").is_err());
    }

    #[test]
    fn test_parse_shell() {
        assert_eq!(parse_shell("/bin/bash\n", "sh"), "/bin/bash");
        assert_eq!(parse_shell("/bin/zsh\n", "sh"), "/bin/zsh");

        assert_eq!(parse_shell("/bin/bash", "sh"), "/bin/bash");
        assert_eq!(
            parse_shell("some shell init output\n/bin/bash\n", "sh"),
            "/bin/bash"
        );
        assert_eq!(
            parse_shell("some shell init output\n/bin/bash", "sh"),
            "/bin/bash"
        );
        assert_eq!(parse_shell("", "sh"), "sh");
        assert_eq!(parse_shell("\n", "sh"), "sh");
    }

    #[test]
    fn test_remote_server_binary_names() {
        let version = Version::new(0, 233, 0);

        assert_eq!(
            remote_server_binary_name(ReleaseChannel::Dev, &version),
            "chorus-remote-server-dev-build"
        );
        assert_eq!(
            remote_server_binary_name(ReleaseChannel::Stable, &version),
            "chorus-remote-server-stable-0.233.0"
        );
        assert_eq!(
            legacy_remote_server_binary_name(ReleaseChannel::Dev, &version),
            "zed-remote-server-dev-build"
        );
    }

    #[test]
    fn test_bundled_remote_server_archive_names() {
        assert_eq!(
            bundled_remote_server_archive_name(RemotePlatform {
                os: RemoteOs::Linux,
                arch: RemoteArch::X86_64,
            }),
            "chorus-remote-server-linux-x86_64.gz"
        );
        assert_eq!(
            bundled_remote_server_archive_name(RemotePlatform {
                os: RemoteOs::Windows,
                arch: RemoteArch::Aarch64,
            }),
            "chorus-remote-server-windows-aarch64.zip"
        );
    }

    #[test]
    fn test_packaged_remote_server_locations_include_bundle_path() {
        let current_dir = Path::new("/tmp/chorus");
        let archive_name = "chorus-remote-server-linux-x86_64.gz";
        let locations = packaged_remote_server_locations(current_dir, archive_name);

        #[cfg(target_os = "macos")]
        {
            assert_eq!(
                locations[0],
                PathBuf::from(
                    "/tmp/chorus/../Resources/remote-servers/chorus-remote-server-linux-x86_64.gz"
                )
            );
        }

        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        {
            assert_eq!(
                locations[0],
                PathBuf::from(
                    "/tmp/chorus/../libexec/remote-servers/chorus-remote-server-linux-x86_64.gz"
                )
            );
        }

        #[cfg(target_os = "windows")]
        {
            assert_eq!(
                locations[0],
                PathBuf::from("/tmp/chorus/remote-servers/chorus-remote-server-linux-x86_64.gz")
            );
        }

        assert_eq!(
            locations.last().unwrap(),
            &PathBuf::from(format!("/tmp/chorus/{archive_name}"))
        );
    }
}
