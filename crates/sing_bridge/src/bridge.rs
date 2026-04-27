use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use remote::{RemoteConnectionOptions, SshConnectionOptions};
use serde::de::DeserializeOwned;

use crate::api::SingApiClient;
use crate::client_config::SingClientConfig;
use crate::command::{CommandRequest, SingCommandRunner, SshSingCommandRunner};
use crate::error::SingBridgeError;
use crate::models::{
    AgentLog, AgentReport, AgentSummaryList, CreateSpecRequest, CreateSpecResult, DispatchRequest,
    DispatchResult, HostStatus, ProjectAgentStatus, ProjectConfig, ProjectRemoteTarget,
    ProjectServiceList, ProjectServiceLogs, ProjectStartResult, ProjectStatus, ProjectStopResult,
    ProjectSummary, SpecBoard, SpecDocument, SpecStatus, StopAgentResult, UpdateSpecStatusResult,
};
use crate::validation;

const QUERY_TIMEOUT: Duration = Duration::from_secs(20);
const LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct SingBridge {
    config: SingClientConfig,
    host_options: SshConnectionOptions,
    runner: Arc<dyn SingCommandRunner>,
}

impl SingBridge {
    pub fn load() -> Result<Self, SingBridgeError> {
        Self::load_from(SingClientConfig::default_path())
    }

    pub fn load_from(path: impl AsRef<Path>) -> Result<Self, SingBridgeError> {
        let path = path.as_ref();
        let config = SingClientConfig::load_from(path)?;
        let host_options =
            config
                .parse_ssh_options()
                .map_err(|message| SingBridgeError::InvalidConfig {
                    path: path.to_path_buf(),
                    message,
                })?;
        let runner = Arc::new(SshSingCommandRunner::new(host_options.clone()));
        Ok(Self {
            config,
            host_options,
            runner,
        })
    }

    pub fn with_runner(
        config: SingClientConfig,
        runner: Arc<dyn SingCommandRunner>,
    ) -> Result<Self, SingBridgeError> {
        let host_options = config
            .parse_ssh_options()
            .map_err(|message| SingBridgeError::invalid_input("config.host", message))?;
        Ok(Self {
            config,
            host_options,
            runner,
        })
    }

    pub fn config(&self) -> &SingClientConfig {
        &self.config
    }

    pub fn host_connection_options(&self) -> RemoteConnectionOptions {
        RemoteConnectionOptions::Ssh(self.host_options.clone())
    }

    pub async fn list_projects(&self) -> Result<Vec<ProjectSummary>, SingBridgeError> {
        self.run_json("project list", vec!["project", "list"], QUERY_TIMEOUT)
            .await
    }

    pub async fn project_config(&self, project: &str) -> Result<ProjectConfig, SingBridgeError> {
        validation::project_name(project)?;
        self.run_json(
            "project config",
            vec!["project", "config", project],
            QUERY_TIMEOUT,
        )
        .await
    }

    pub async fn project_remote_target(
        &self,
        project: &str,
    ) -> Result<ProjectRemoteTarget, SingBridgeError> {
        let project_config = self.project_config(project).await?;
        if project_config.container_status != ProjectStatus::Running {
            return Err(SingBridgeError::ProjectNotRunning {
                project: project.to_string(),
                status: project_config.container_status.to_string(),
            });
        }

        let container_ip = project_config.container_ip.clone().ok_or_else(|| {
            SingBridgeError::MissingContainerAddress {
                project: project.to_string(),
            }
        })?;
        let ssh_user = project_config
            .ssh_user
            .clone()
            .unwrap_or_else(|| "dev".to_string());
        let workspace_root = format!("/home/{ssh_user}/workspace").into();
        let connection_options = RemoteConnectionOptions::Ssh(SshConnectionOptions {
            host: container_ip.clone().into(),
            username: Some(ssh_user.clone()),
            args: Some(vec!["-J".to_string(), self.jump_target()]),
            nickname: Some(project_config.name.clone()),
            upload_binary_over_ssh: false,
            ..Default::default()
        });

        Ok(ProjectRemoteTarget {
            project: project_config.name,
            ssh_user,
            container_ip,
            workspace_root,
            connection_options,
        })
    }

    pub async fn start_project(
        &self,
        project: &str,
    ) -> Result<ProjectStartResult, SingBridgeError> {
        validation::project_name(project)?;
        self.run_json("up", vec!["up", project], LIFECYCLE_TIMEOUT)
            .await
    }

    pub async fn stop_project(&self, project: &str) -> Result<ProjectStopResult, SingBridgeError> {
        validation::project_name(project)?;
        self.run_json("down", vec!["down", project], LIFECYCLE_TIMEOUT)
            .await
    }

    pub async fn list_project_services(
        &self,
        project: &str,
    ) -> Result<ProjectServiceList, SingBridgeError> {
        validation::project_name(project)?;
        self.run_json("logs list", vec!["logs", project], QUERY_TIMEOUT)
            .await
    }

    pub async fn project_service_logs(
        &self,
        project: &str,
        service: &str,
        tail: u32,
    ) -> Result<ProjectServiceLogs, SingBridgeError> {
        validation::project_name(project)?;
        validation::service_name(service)?;
        self.run_json(
            "logs",
            vec!["logs", project, service, "--tail", &tail.to_string()],
            QUERY_TIMEOUT,
        )
        .await
    }

    pub async fn host_status(&self) -> Result<HostStatus, SingBridgeError> {
        self.run_json("host status", vec!["host", "status"], QUERY_TIMEOUT)
            .await
    }

    pub async fn list_specs(&self, project: &str) -> Result<SpecBoard, SingBridgeError> {
        validation::project_name(project)?;
        self.run_json("spec list", vec!["spec", "list", project], QUERY_TIMEOUT)
            .await
    }

    pub async fn show_spec(
        &self,
        project: &str,
        spec_id: &str,
    ) -> Result<SpecDocument, SingBridgeError> {
        validation::project_name(project)?;
        validation::spec_id(spec_id)?;
        self.run_json(
            "spec show",
            vec!["spec", "show", project, spec_id],
            QUERY_TIMEOUT,
        )
        .await
    }

    pub async fn create_spec(
        &self,
        project: &str,
        request: CreateSpecRequest,
    ) -> Result<CreateSpecResult, SingBridgeError> {
        validation::project_name(project)?;
        let title = validation::title(&request.title)?;
        if let Some(spec_id) = &request.id {
            validation::spec_id(spec_id)?;
        }
        if let Some(branch) = &request.branch {
            validation::git_ref(branch)?;
        }
        for dependency in &request.depends_on {
            validation::spec_id(dependency)?;
        }

        let mut args = vec![
            "spec".to_string(),
            "create".to_string(),
            project.to_string(),
            "--title".to_string(),
            title,
            "--status".to_string(),
            request.status.as_cli_arg().to_string(),
        ];
        if let Some(spec_id) = request.id {
            args.push("--id".to_string());
            args.push(spec_id);
        }
        if let Some(assignee) = request.assignee {
            args.push("--assignee".to_string());
            args.push(assignee);
        }
        if let Some(branch) = request.branch {
            args.push("--branch".to_string());
            args.push(branch);
        }
        if !request.depends_on.is_empty() {
            args.push("--depends-on".to_string());
            args.push(request.depends_on.join(","));
        }

        self.run_json(
            "spec create",
            args.iter().map(String::as_str).collect(),
            QUERY_TIMEOUT,
        )
        .await
    }

    pub async fn update_spec_status(
        &self,
        project: &str,
        spec_id: &str,
        status: SpecStatus,
    ) -> Result<UpdateSpecStatusResult, SingBridgeError> {
        validation::project_name(project)?;
        validation::spec_id(spec_id)?;
        self.run_json(
            "spec status",
            vec!["spec", "status", project, spec_id, status.as_cli_arg()],
            QUERY_TIMEOUT,
        )
        .await
    }

    pub async fn dispatch(
        &self,
        project: &str,
        request: DispatchRequest,
    ) -> Result<DispatchResult, SingBridgeError> {
        validation::project_name(project)?;
        if let Some(spec_id) = &request.spec_id {
            validation::spec_id(spec_id)?;
        }

        SingApiClient::new(self.host_options.clone())
            .dispatch(project, request)
            .await
    }

    pub async fn list_agents(&self) -> Result<AgentSummaryList, SingBridgeError> {
        self.run_json("agent status", vec!["agent", "status"], QUERY_TIMEOUT)
            .await
    }

    pub async fn project_agent_status(
        &self,
        project: &str,
    ) -> Result<ProjectAgentStatus, SingBridgeError> {
        validation::project_name(project)?;
        SingApiClient::new(self.host_options.clone())
            .agent_status(project)
            .await
    }

    pub async fn project_agent_log(
        &self,
        project: &str,
        tail: u32,
    ) -> Result<AgentLog, SingBridgeError> {
        validation::project_name(project)?;
        SingApiClient::new(self.host_options.clone())
            .agent_log(project, tail)
            .await
    }

    pub async fn stop_project_agent(
        &self,
        project: &str,
    ) -> Result<StopAgentResult, SingBridgeError> {
        validation::project_name(project)?;
        SingApiClient::new(self.host_options.clone())
            .stop_agent(project)
            .await
    }

    pub async fn project_agent_report(
        &self,
        project: &str,
    ) -> Result<AgentReport, SingBridgeError> {
        validation::project_name(project)?;
        SingApiClient::new(self.host_options.clone())
            .agent_report(project)
            .await
    }

    fn jump_target(&self) -> String {
        self.host_options.connection_string()
    }

    async fn run_json<T>(
        &self,
        display_name: &str,
        args: Vec<&str>,
        timeout: Duration,
    ) -> Result<T, SingBridgeError>
    where
        T: DeserializeOwned,
    {
        let mut args = args.into_iter().map(str::to_string).collect::<Vec<_>>();
        args.push("--json".to_string());
        self.run_json_args(display_name, args, timeout).await
    }

    async fn run_json_args<T>(
        &self,
        display_name: &str,
        args: Vec<String>,
        timeout: Duration,
    ) -> Result<T, SingBridgeError>
    where
        T: DeserializeOwned,
    {
        let output = self
            .runner
            .run(CommandRequest::new(display_name, args, timeout))
            .await
            .map_err(|source| SingBridgeError::command(display_name, source))?;
        serde_json::from_str(output.stdout.trim()).map_err(|source| {
            SingBridgeError::InvalidResponse {
                command: display_name.to_string(),
                output: output.stdout,
                source,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use pretty_assertions::assert_eq;
    use remote::RemoteConnectionOptions;
    use smol::block_on;

    use crate::command::CommandOutput;
    use crate::error::{RemoteFailure, RemoteFailureKind, SingCommandError};
    use crate::models::{CreateSpecRequest, ProjectStatus, SpecStatus};

    use super::*;

    #[derive(Default)]
    struct FakeRunner {
        requests: Mutex<Vec<CommandRequest>>,
        responses: Mutex<VecDeque<Result<CommandOutput, SingCommandError>>>,
    }

    impl FakeRunner {
        fn with_responses(responses: Vec<Result<CommandOutput, SingCommandError>>) -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                responses: Mutex::new(responses.into()),
            }
        }

        fn requests(&self) -> Vec<CommandRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl SingCommandRunner for FakeRunner {
        async fn run(&self, request: CommandRequest) -> Result<CommandOutput, SingCommandError> {
            self.requests.lock().unwrap().push(request);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| panic!("missing fake response"))
        }
    }

    fn bridge_with_responses(
        responses: Vec<Result<CommandOutput, SingCommandError>>,
    ) -> (SingBridge, Arc<FakeRunner>) {
        let runner = Arc::new(FakeRunner::with_responses(responses));
        let bridge = SingBridge::with_runner(
            SingClientConfig {
                host: "sing-host".to_string(),
            },
            runner.clone(),
        )
        .unwrap();
        (bridge, runner)
    }

    #[test]
    fn project_remote_target_uses_proxy_jump() {
        block_on(async {
            let response = Ok(CommandOutput {
                exit_status: Some(0),
                stdout: r#"{
                    "name":"demo",
                    "container_status":"running",
                    "container_ip":"10.1.2.3",
                    "agent_session":{"available":true,"running":false},
                    "specs":{"available":false,"reason":"project_stopped"},
                    "ssh_user":"dev"
                }"#
                .to_string(),
                stderr: String::new(),
            });
            let (bridge, _runner) = bridge_with_responses(vec![response]);

            let target = bridge.project_remote_target("demo").await.unwrap();

            assert_eq!(target.project, "demo");
            assert_eq!(target.ssh_user, "dev");
            assert_eq!(
                target.workspace_root.to_string_lossy(),
                "/home/dev/workspace"
            );
            match target.connection_options {
                RemoteConnectionOptions::Ssh(options) => {
                    assert_eq!(options.username.as_deref(), Some("dev"));
                    assert_eq!(options.host.to_string(), "10.1.2.3");
                    assert_eq!(options.nickname.as_deref(), Some("demo"));
                    assert_eq!(
                        options.args,
                        Some(vec!["-J".to_string(), "sing-host".to_string()])
                    );
                }
                _ => panic!("expected ssh connection options"),
            }
        });
    }

    #[test]
    fn list_projects_decodes_json() {
        block_on(async {
            let response = Ok(CommandOutput {
                exit_status: Some(0),
                stdout: r#"[{"name":"demo","status":"running","ip":"10.0.0.3"}]"#.to_string(),
                stderr: String::new(),
            });
            let (bridge, runner) = bridge_with_responses(vec![response]);

            let projects = bridge.list_projects().await.unwrap();

            assert_eq!(projects.len(), 1);
            assert_eq!(projects[0].name, "demo");
            assert_eq!(projects[0].status, ProjectStatus::Running);
            let requests = runner.requests();
            assert_eq!(requests[0].display_name, "project list");
            assert_eq!(
                requests[0].args,
                vec!["project", "list", "--json"]
                    .into_iter()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            );
        });
    }

    #[test]
    fn create_spec_builds_request() {
        block_on(async {
            let response = Ok(CommandOutput {
                exit_status: Some(0),
                stdout: r#"{
                    "name":"demo",
                    "created":true,
                    "spec":{"id":"auth-fix","title":"Auth Fix","status":"pending"},
                    "spec_path":"/home/dev/workspace/specs/auth-fix/spec.md"
                }"#
                .to_string(),
                stderr: String::new(),
            });
            let (bridge, runner) = bridge_with_responses(vec![response]);

            let request = CreateSpecRequest {
                title: " Auth Fix ".to_string(),
                id: Some("auth-fix".to_string()),
                status: SpecStatus::Pending,
                assignee: Some("claude-code".to_string()),
                branch: Some("feat/auth-fix".to_string()),
                depends_on: vec!["foundation".to_string()],
            };
            let result = bridge.create_spec("demo", request).await.unwrap();

            assert_eq!(result.spec.id, "auth-fix");
            let requests = runner.requests();
            assert_eq!(requests[0].display_name, "spec create");
            assert_eq!(
                requests[0].args,
                vec![
                    "spec",
                    "create",
                    "demo",
                    "--title",
                    "Auth Fix",
                    "--status",
                    "pending",
                    "--id",
                    "auth-fix",
                    "--assignee",
                    "claude-code",
                    "--branch",
                    "feat/auth-fix",
                    "--depends-on",
                    "foundation",
                    "--json"
                ]
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>()
            );
        });
    }

    #[test]
    fn run_json_reports_invalid_response() {
        block_on(async {
            let response = Ok(CommandOutput {
                exit_status: Some(0),
                stdout: "{invalid".to_string(),
                stderr: String::new(),
            });
            let (bridge, _runner) = bridge_with_responses(vec![response]);

            let error = bridge.list_projects().await.unwrap_err();

            assert!(matches!(error, SingBridgeError::InvalidResponse { .. }));
        });
    }

    #[test]
    fn run_json_propagates_remote_failure() {
        block_on(async {
            let response = Err(SingCommandError::RemoteFailure(RemoteFailure {
                command: "spec show".to_string(),
                kind: RemoteFailureKind::NotFound,
                exit_status: Some(1),
                stdout: String::new(),
                stderr: "Spec 'missing' not found in index.yaml".to_string(),
            }));
            let (bridge, _runner) = bridge_with_responses(vec![response]);

            let error = bridge.show_spec("demo", "missing").await.unwrap_err();

            assert!(matches!(error, SingBridgeError::Command { .. }));
        });
    }
}
