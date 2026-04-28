use std::{
    net::TcpListener,
    sync::Arc,
    time::{Duration, Instant},
};

use futures::AsyncReadExt as _;
use http_client::{AsyncBody, HttpClient, Json, Method, Request, StatusCode};
use remote::SshConnectionOptions;
use reqwest_client::ReqwestClient;
use serde::{Serialize, de::DeserializeOwned};
use util::{
    command::{Child, Stdio, new_command},
    shell::ShellKind,
};

use crate::{
    error::{SingBridgeError, SingCommandError},
    models::{
        AgentLog, AgentReport, DispatchRequest, DispatchResult, ProjectAgentStatus, StopAgentResult,
    },
};

const HOST_API_PORT: u16 = 7070;
const TUNNEL_TIMEOUT: Duration = Duration::from_secs(10);
const API_START_TIMEOUT: Duration = Duration::from_secs(10);
const API_TOKEN_PATH: &str = "~/.sing/api-token";

#[derive(Clone)]
pub(crate) struct SingApiClient {
    host: SshConnectionOptions,
    http: Arc<dyn HttpClient>,
}

impl SingApiClient {
    pub(crate) fn new(host: SshConnectionOptions) -> Self {
        Self {
            host,
            http: Arc::new(ReqwestClient::new()),
        }
    }

    pub(crate) async fn dispatch(
        &self,
        project: &str,
        request: DispatchRequest,
    ) -> Result<DispatchResult, SingBridgeError> {
        self.post(
            &format!("/v1/projects/{project}/dispatch"),
            &ApiDispatchRequest::from(request),
        )
        .await
    }

    pub(crate) async fn agent_status(
        &self,
        project: &str,
    ) -> Result<ProjectAgentStatus, SingBridgeError> {
        self.get(&format!("/v1/projects/{project}/agent")).await
    }

    pub(crate) async fn agent_log(
        &self,
        project: &str,
        tail: u32,
    ) -> Result<AgentLog, SingBridgeError> {
        self.get(&format!("/v1/projects/{project}/agent/log?tail={tail}"))
            .await
    }

    pub(crate) async fn stop_agent(
        &self,
        project: &str,
    ) -> Result<StopAgentResult, SingBridgeError> {
        self.post_empty(&format!("/v1/projects/{project}/agent/stop"))
            .await
    }

    pub(crate) async fn agent_report(&self, project: &str) -> Result<AgentReport, SingBridgeError> {
        self.post_empty(&format!("/v1/projects/{project}/agent/report"))
            .await
    }

    async fn get<T>(&self, path: &str) -> Result<T, SingBridgeError>
    where
        T: DeserializeOwned,
    {
        self.send(Method::GET, path, AsyncBody::empty()).await
    }

    async fn post<T, B>(&self, path: &str, body: &B) -> Result<T, SingBridgeError>
    where
        T: DeserializeOwned,
        B: Serialize,
    {
        self.send(Method::POST, path, Json(body).into()).await
    }

    async fn post_empty<T>(&self, path: &str) -> Result<T, SingBridgeError>
    where
        T: DeserializeOwned,
    {
        self.post(path, &serde_json::json!({})).await
    }

    async fn send<T>(
        &self,
        method: Method,
        path: &str,
        body: AsyncBody,
    ) -> Result<T, SingBridgeError>
    where
        T: DeserializeOwned,
    {
        ensure_api_server(&self.host).await?;
        let token = read_api_token(&self.host).await?;
        let tunnel = SshApiTunnel::open(&self.host).await?;
        let uri = format!("http://127.0.0.1:{}{path}", tunnel.local_port);
        let request = Request::builder()
            .method(method)
            .uri(&uri)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .body(body)
            .map_err(|error| SingBridgeError::api_request(path, error.to_string()))?;
        let response = self
            .http
            .send(request)
            .await
            .map_err(|error| SingBridgeError::api_request(path, error.to_string()))?;
        let status = response.status();
        let text = read_body(response.into_body())
            .await
            .map_err(|error| SingBridgeError::api_request(path, error.to_string()))?;
        if !status.is_success() {
            return Err(api_failure(path, status, &text));
        }
        serde_json::from_str(&text).map_err(|source| SingBridgeError::InvalidResponse {
            command: format!("sing api {path}"),
            output: text,
            source,
        })
    }
}

struct SshApiTunnel {
    local_port: u16,
    child: Child,
}

impl SshApiTunnel {
    async fn open(host: &SshConnectionOptions) -> Result<Self, SingBridgeError> {
        let local_port = available_local_port()?;
        let mut args = host.additional_args();
        args.extend([
            "-N".to_string(),
            "-L".to_string(),
            format!("127.0.0.1:{local_port}:127.0.0.1:{HOST_API_PORT}"),
            "-o".to_string(),
            "ExitOnForwardFailure=yes".to_string(),
            "-o".to_string(),
            "BatchMode=yes".to_string(),
            host.ssh_destination(),
        ]);
        let mut command = new_command("ssh");
        command
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .args(args);
        let child = command.spawn().map_err(|error| SingBridgeError::Command {
            command: "ssh sing api tunnel".to_string(),
            source: SingCommandError::SpawnFailed {
                command: "ssh sing api tunnel".to_string(),
                message: error.to_string(),
            },
        })?;
        let tunnel = Self { local_port, child };
        tunnel.wait_until_ready().await?;
        Ok(tunnel)
    }

    async fn wait_until_ready(&self) -> Result<(), SingBridgeError> {
        let started_at = Instant::now();
        while started_at.elapsed() < TUNNEL_TIMEOUT {
            if std::net::TcpStream::connect(("127.0.0.1", self.local_port)).is_ok() {
                return Ok(());
            }
            async_io::Timer::at(Instant::now() + Duration::from_millis(25)).await;
        }
        Err(SingBridgeError::ApiUnavailable {
            message: "timed out creating SSH tunnel to sing API".to_string(),
        })
    }
}

impl Drop for SshApiTunnel {
    fn drop(&mut self) {
        if let Err(error) = self.child.kill() {
            log::warn!("failed to stop sing API SSH tunnel: {error}");
        }
    }
}

#[derive(Serialize)]
struct ApiDispatchRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    spec_id: Option<String>,
    mode: &'static str,
    dry_run: bool,
}

impl From<DispatchRequest> for ApiDispatchRequest {
    fn from(request: DispatchRequest) -> Self {
        Self {
            spec_id: request.spec_id,
            mode: if request.background {
                "background"
            } else {
                "foreground"
            },
            dry_run: request.dry_run,
        }
    }
}

async fn ensure_api_server(host: &SshConnectionOptions) -> Result<(), SingBridgeError> {
    let command = format!(
        "mkdir -p ~/.sing && if ! bash -lc 'cat </dev/null >/dev/tcp/127.0.0.1/{HOST_API_PORT}' >/dev/null 2>&1; then nohup sing api --host 127.0.0.1 --port {HOST_API_PORT} > ~/.sing/api.log 2>&1 </dev/null & fi"
    );
    run_host_shell(host, &command, "sing api start").await?;
    Ok(())
}

async fn read_api_token(host: &SshConnectionOptions) -> Result<String, SingBridgeError> {
    let command = format!("cat {API_TOKEN_PATH}");
    let started_at = Instant::now();
    let mut last_error = None;

    while started_at.elapsed() < API_START_TIMEOUT {
        match run_host_shell(host, &command, "sing api token").await {
            Ok(output) => {
                let token = output.trim().to_string();
                if !token.is_empty() {
                    return Ok(token);
                }
                last_error = Some("token file was empty".to_string());
            }
            Err(error) => {
                last_error = Some(error.to_string());
            }
        }
        async_io::Timer::at(Instant::now() + Duration::from_millis(100)).await;
    }

    Err(SingBridgeError::ApiUnavailable {
        message: format!(
            "sing API token was not available on host: {}",
            last_error.unwrap_or_else(|| "timed out waiting for token".to_string())
        ),
    })
}

async fn run_host_shell(
    host: &SshConnectionOptions,
    script: &str,
    display_name: &str,
) -> Result<String, SingBridgeError> {
    let quoted = ShellKind::Posix.try_quote(script).ok_or_else(|| {
        SingBridgeError::api_request(display_name, "failed to quote remote shell command")
    })?;
    let mut args = host.additional_args();
    args.extend([
        "-T".to_string(),
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        host.ssh_destination(),
        format!("sh -lc {quoted}"),
    ]);
    let mut command = new_command("ssh");
    command
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .args(args);
    let output = command
        .output()
        .await
        .map_err(|error| SingBridgeError::Command {
            command: display_name.to_string(),
            source: SingCommandError::SpawnFailed {
                command: display_name.to_string(),
                message: error.to_string(),
            },
        })?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    Err(SingBridgeError::Command {
        command: display_name.to_string(),
        source: SingCommandError::RemoteFailure(crate::RemoteFailure {
            command: display_name.to_string(),
            kind: crate::RemoteFailureKind::Unavailable,
            exit_status: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }),
    })
}

async fn read_body(mut body: AsyncBody) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    body.read_to_end(&mut bytes).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn api_failure(path: &str, status: StatusCode, text: &str) -> SingBridgeError {
    match serde_json::from_str::<ApiErrorEnvelope>(text) {
        Ok(envelope) => SingBridgeError::ApiFailure {
            path: path.to_string(),
            status: status.as_u16(),
            code: envelope.error.code,
            message: envelope.error.message,
            action: envelope.error.action,
        },
        Err(_) => SingBridgeError::ApiFailure {
            path: path.to_string(),
            status: status.as_u16(),
            code: "http_error".to_string(),
            message: text.to_string(),
            action: None,
        },
    }
}

fn available_local_port() -> Result<u16, SingBridgeError> {
    TcpListener::bind(("127.0.0.1", 0))
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .map_err(|error| SingBridgeError::ApiUnavailable {
            message: format!("failed to allocate a local port for sing API tunnel: {error}"),
        })
}

#[derive(serde::Deserialize)]
struct ApiErrorEnvelope {
    error: ApiErrorBody,
}

#[derive(serde::Deserialize)]
struct ApiErrorBody {
    code: String,
    message: String,
    #[serde(default)]
    action: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_background_dispatch_request() -> Result<(), serde_json::Error> {
        let request = ApiDispatchRequest::from(DispatchRequest {
            spec_id: Some("chorus-dispatch-integration".to_string()),
            background: true,
            dry_run: true,
        });

        let value = serde_json::to_value(request)?;

        assert_eq!(
            value,
            serde_json::json!({
                "spec_id": "chorus-dispatch-integration",
                "mode": "background",
                "dry_run": true
            })
        );
        Ok(())
    }

    #[test]
    fn serializes_next_spec_dispatch_request() -> Result<(), serde_json::Error> {
        let request = ApiDispatchRequest::from(DispatchRequest {
            spec_id: None,
            background: false,
            dry_run: false,
        });

        let value = serde_json::to_value(request)?;

        assert_eq!(
            value,
            serde_json::json!({
                "mode": "foreground",
                "dry_run": false
            })
        );
        Ok(())
    }

    #[test]
    fn parses_structured_api_failure() {
        let error = api_failure(
            "/v1/projects/demo/dispatch",
            StatusCode::FORBIDDEN,
            r#"{"error":{"code":"forbidden","message":"denied","action":"check access"}}"#,
        );

        assert!(matches!(
            error,
            SingBridgeError::ApiFailure {
                status: 403,
                ref code,
                ref message,
                action: Some(ref action),
                ..
            } if code == "forbidden" && message == "denied" && action == "check access"
        ));
    }

    #[test]
    fn falls_back_for_unstructured_api_failure() {
        let error = api_failure(
            "/v1/projects/demo/dispatch",
            StatusCode::INTERNAL_SERVER_ERROR,
            "upstream crashed",
        );

        assert!(matches!(
            error,
            SingBridgeError::ApiFailure {
                status: 500,
                ref code,
                ref message,
                action: None,
                ..
            } if code == "http_error" && message == "upstream crashed"
        ));
    }

    #[test]
    fn allocates_local_port() -> Result<(), SingBridgeError> {
        let port = available_local_port()?;

        assert!(port > 0);
        Ok(())
    }
}
