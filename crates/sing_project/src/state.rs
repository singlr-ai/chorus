use std::sync::Arc;

use anyhow::Result;
use futures::future::join_all;
use sing_bridge::{
    AgentSessionInfo, ProjectConfig, ProjectRuntimes, ProjectSpecAvailability, ProjectStatus,
    ProjectSummary,
};

use crate::client::SingProjectClient;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectActionKind {
    Open,
    Start,
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRow {
    pub name: String,
    pub status: ProjectStatus,
    pub ip: Option<String>,
    pub description: Option<String>,
    pub runtimes: Option<ProjectRuntimes>,
    pub agent_session: AgentSessionInfo,
    pub specs: ProjectSpecAvailability,
    pub detail_error: Option<String>,
}

impl ProjectRow {
    fn from_summary(summary: ProjectSummary, config: Result<ProjectConfig>) -> Self {
        match config {
            Ok(config) => Self {
                name: config.name,
                status: config.container_status,
                ip: config.container_ip.or(summary.ip),
                description: config.description,
                runtimes: config.runtimes,
                agent_session: config.agent_session,
                specs: config.specs,
                detail_error: None,
            },
            Err(error) => {
                let error = error.to_string();
                Self {
                    name: summary.name,
                    status: summary.status,
                    ip: summary.ip,
                    description: None,
                    runtimes: None,
                    agent_session: AgentSessionInfo {
                        available: false,
                        reason: Some(error.clone()),
                        ..Default::default()
                    },
                    specs: ProjectSpecAvailability {
                        available: false,
                        reason: Some(error.clone()),
                        ..Default::default()
                    },
                    detail_error: Some(error),
                }
            }
        }
    }

    pub fn status_label(&self) -> &'static str {
        match self.status {
            ProjectStatus::Running => "Running",
            ProjectStatus::Stopped => "Stopped",
            ProjectStatus::NotCreated => "Not created",
            ProjectStatus::Error => "Error",
        }
    }

    pub fn can_open(&self) -> bool {
        self.status == ProjectStatus::Running
    }

    pub fn can_start(&self) -> bool {
        matches!(
            self.status,
            ProjectStatus::Stopped | ProjectStatus::NotCreated
        )
    }

    pub fn can_stop(&self) -> bool {
        self.status == ProjectStatus::Running
    }

    pub fn agent_summary(&self) -> String {
        if self.agent_session.running {
            if let Some(task) = self.agent_session.task.as_deref() {
                format!("Agent running | {task}")
            } else {
                "Agent running".to_string()
            }
        } else if self.agent_session.available {
            "Agent idle".to_string()
        } else if let Some(reason) = self.agent_session.reason.as_deref() {
            format!("Agent unavailable | {}", humanize_reason(reason))
        } else {
            "Agent unavailable".to_string()
        }
    }

    pub fn agent_badge(&self) -> &'static str {
        if self.agent_session.running {
            "Agent active"
        } else if self.agent_session.available {
            "Agent idle"
        } else if matches!(
            self.status,
            ProjectStatus::Stopped | ProjectStatus::NotCreated
        ) {
            "Agent paused"
        } else {
            "Agent unavailable"
        }
    }

    pub fn agent_detail(&self) -> String {
        let mut parts = Vec::new();

        parts.push(if self.agent_session.running {
            "Agent running".to_string()
        } else if self.agent_session.available {
            "Agent idle".to_string()
        } else {
            "Agent unavailable".to_string()
        });

        if let Some(task) = self.agent_session.task.as_deref() {
            parts.push(format!("task {task}"));
        }
        if let Some(branch) = self.agent_session.branch.as_deref() {
            parts.push(format!("branch {branch}"));
        }
        if let Some(pid) = self.agent_session.pid {
            parts.push(format!("pid {pid}"));
        }
        if let Some(reason) = self.agent_session.reason.as_deref() {
            parts.push(humanize_reason(reason));
        }

        parts.join(" | ")
    }

    pub fn spec_summary(&self) -> String {
        if self.specs.available {
            let ready = self.specs.ready_count.unwrap_or_default();
            let blocked = self.specs.blocked_count.unwrap_or_default();
            format!("Specs ready {ready} | blocked {blocked}")
        } else if let Some(reason) = self.specs.reason.as_deref() {
            format!("Specs unavailable | {}", humanize_reason(reason))
        } else {
            "Specs unavailable".to_string()
        }
    }

    pub fn spec_detail(&self) -> String {
        if self.specs.available {
            let ready = self.specs.ready_count.unwrap_or_default();
            let blocked = self.specs.blocked_count.unwrap_or_default();
            let next = self
                .specs
                .next_ready_id
                .as_deref()
                .map(|next| format!("next {next}"));

            let mut parts = vec![format!("Ready {ready}"), format!("Blocked {blocked}")];
            if let Some(next) = next {
                parts.push(next);
            }
            parts.join(" | ")
        } else if let Some(reason) = self.specs.reason.as_deref() {
            humanize_reason(reason)
        } else {
            "Spec data unavailable".to_string()
        }
    }

    pub fn ready_count(&self) -> Option<u32> {
        self.specs
            .available
            .then_some(self.specs.ready_count.unwrap_or_default())
    }

    pub fn blocked_count(&self) -> Option<u32> {
        self.specs
            .available
            .then_some(self.specs.blocked_count.unwrap_or_default())
    }

    pub fn next_ready_id(&self) -> Option<&str> {
        self.specs
            .available
            .then_some(self.specs.next_ready_id.as_deref())
            .flatten()
    }

    pub fn current_task(&self) -> Option<&str> {
        self.agent_session.task.as_deref()
    }

    pub fn branch(&self) -> Option<&str> {
        self.agent_session.branch.as_deref()
    }

    pub fn runtime_summary(&self) -> Option<String> {
        let runtimes = self.runtimes.as_ref()?;
        let mut parts = Vec::new();

        if let Some(jdk) = runtimes.jdk {
            parts.push(format!("JDK {jdk}"));
        }
        if let Some(node) = runtimes.node.as_deref() {
            parts.push(format!("Node {node}"));
        }
        if let Some(maven) = runtimes.maven.as_deref() {
            parts.push(format!("Maven {maven}"));
        }

        (!parts.is_empty()).then(|| parts.join(" | "))
    }
}

pub async fn load_project_rows(client: Arc<dyn SingProjectClient>) -> Result<Vec<ProjectRow>> {
    let projects = client.list_projects().await?;
    let rows = join_all(projects.into_iter().map(|summary| {
        let client = client.clone();
        async move {
            let config = client.project_config(&summary.name).await;
            ProjectRow::from_summary(summary, config)
        }
    }))
    .await;

    let mut rows = rows;
    rows.sort_by(|left, right| {
        project_status_rank(left.status)
            .cmp(&project_status_rank(right.status))
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(rows)
}

pub fn next_selection(previous: Option<&str>, projects: &[ProjectRow]) -> Option<String> {
    previous.and_then(|selected| {
        projects
            .iter()
            .find(|project| project.name == selected)
            .map(|project| project.name.clone())
    })
}

fn humanize_reason(reason: &str) -> String {
    if !reason.contains('_') {
        return reason.to_string();
    }

    reason
        .split('_')
        .filter(|part| !part.is_empty())
        .enumerate()
        .map(|(index, part)| {
            if index == 0 {
                capitalize_word(part)
            } else {
                part.to_ascii_lowercase()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn project_status_rank(status: ProjectStatus) -> u8 {
    match status {
        ProjectStatus::Running => 0,
        ProjectStatus::Stopped => 1,
        ProjectStatus::NotCreated => 2,
        ProjectStatus::Error => 3,
    }
}

fn capitalize_word(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => {
            let mut capitalized = first.to_uppercase().collect::<String>();
            capitalized.push_str(&chars.as_str().to_ascii_lowercase());
            capitalized
        }
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use anyhow::{Result, anyhow};
    use async_trait::async_trait;
    use futures::executor::block_on;
    use sing_bridge::{
        AgentSessionInfo, ProjectConfig, ProjectRemoteTarget, ProjectRuntimes,
        ProjectSpecAvailability, ProjectStartResult, ProjectStartStatus, ProjectStatus,
        ProjectStopResult, ProjectSummary,
    };

    use super::{ProjectRow, load_project_rows, next_selection};
    use crate::client::SingProjectClient;

    #[derive(Clone)]
    struct FakeClient {
        projects: std::result::Result<Vec<ProjectSummary>, String>,
        configs: HashMap<String, std::result::Result<ProjectConfig, String>>,
    }

    impl Default for FakeClient {
        fn default() -> Self {
            Self {
                projects: Ok(Vec::new()),
                configs: HashMap::default(),
            }
        }
    }

    #[async_trait]
    impl SingProjectClient for FakeClient {
        async fn list_projects(&self) -> Result<Vec<ProjectSummary>> {
            self.projects.clone().map_err(|error| anyhow!(error))
        }

        async fn project_config(&self, project: &str) -> Result<ProjectConfig> {
            self.configs
                .get(project)
                .cloned()
                .unwrap_or_else(|| Err(format!("missing config for {project}")))
                .map_err(|error| anyhow!(error))
        }

        async fn project_remote_target(&self, _project: &str) -> Result<ProjectRemoteTarget> {
            Err(anyhow!("unused"))
        }

        async fn start_project(&self, project: &str) -> Result<ProjectStartResult> {
            Ok(ProjectStartResult {
                name: project.to_string(),
                status: ProjectStartStatus::Started,
                ip: None,
            })
        }

        async fn stop_project(&self, project: &str) -> Result<ProjectStopResult> {
            Ok(ProjectStopResult {
                stopped: vec![project.to_string()],
            })
        }
    }

    #[test]
    fn load_project_rows_keeps_partial_results() {
        let client = FakeClient {
            projects: Ok(vec![
                ProjectSummary {
                    name: "beta".to_string(),
                    status: ProjectStatus::Stopped,
                    ip: None,
                },
                ProjectSummary {
                    name: "alpha".to_string(),
                    status: ProjectStatus::Running,
                    ip: Some("10.0.0.10".to_string()),
                },
            ]),
            configs: HashMap::from([
                (
                    "alpha".to_string(),
                    Ok(project_config("alpha", ProjectStatus::Running)),
                ),
                ("beta".to_string(), Err("ssh command failed".to_string())),
            ]),
        };

        let rows = block_on(load_project_rows(Arc::new(client))).expect("rows should load");

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "alpha");
        assert_eq!(rows[0].status, ProjectStatus::Running);
        assert_eq!(
            rows[0].runtime_summary().as_deref(),
            Some("JDK 26 | Node 24.14.1")
        );
        assert_eq!(rows[0].agent_summary(), "Agent running | auth-fix");
        assert_eq!(rows[0].spec_summary(), "Specs ready 2 | blocked 1");

        assert_eq!(rows[1].name, "beta");
        assert_eq!(rows[1].status, ProjectStatus::Stopped);
        assert_eq!(rows[1].detail_error.as_deref(), Some("ssh command failed"));
        assert_eq!(
            rows[1].agent_summary(),
            "Agent unavailable | ssh command failed"
        );
        assert_eq!(
            rows[1].spec_summary(),
            "Specs unavailable | ssh command failed"
        );
    }

    #[test]
    fn load_project_rows_sorts_running_projects_first() {
        let client = FakeClient {
            projects: Ok(vec![
                ProjectSummary {
                    name: "zeta".to_string(),
                    status: ProjectStatus::Stopped,
                    ip: None,
                },
                ProjectSummary {
                    name: "alpha".to_string(),
                    status: ProjectStatus::Running,
                    ip: None,
                },
                ProjectSummary {
                    name: "beta".to_string(),
                    status: ProjectStatus::Running,
                    ip: None,
                },
            ]),
            configs: HashMap::from([
                (
                    "zeta".to_string(),
                    Ok(project_config("zeta", ProjectStatus::Stopped)),
                ),
                (
                    "alpha".to_string(),
                    Ok(project_config("alpha", ProjectStatus::Running)),
                ),
                (
                    "beta".to_string(),
                    Ok(project_config("beta", ProjectStatus::Running)),
                ),
            ]),
        };

        let rows = block_on(load_project_rows(Arc::new(client))).expect("rows should load");

        assert_eq!(
            rows.iter()
                .map(|project| project.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "beta", "zeta"]
        );
    }

    #[test]
    fn next_selection_prefers_existing_project() {
        let projects = vec![
            ProjectRow {
                name: "alpha".to_string(),
                status: ProjectStatus::Running,
                ip: None,
                description: None,
                runtimes: None,
                agent_session: AgentSessionInfo::default(),
                specs: ProjectSpecAvailability::default(),
                detail_error: None,
            },
            ProjectRow {
                name: "beta".to_string(),
                status: ProjectStatus::Stopped,
                ip: None,
                description: None,
                runtimes: None,
                agent_session: AgentSessionInfo::default(),
                specs: ProjectSpecAvailability::default(),
                detail_error: None,
            },
        ];

        assert_eq!(
            next_selection(Some("beta"), &projects).as_deref(),
            Some("beta")
        );
        assert_eq!(next_selection(Some("missing"), &projects).as_deref(), None);
        assert_eq!(next_selection(None, &projects).as_deref(), None);
    }

    #[test]
    fn project_row_humanizes_backend_reason_codes() {
        let row = ProjectRow {
            name: "sing".to_string(),
            status: ProjectStatus::Stopped,
            ip: None,
            description: None,
            runtimes: None,
            agent_session: AgentSessionInfo {
                available: false,
                running: false,
                reason: Some("project_stopped".to_string()),
                pid: None,
                task: None,
                started_at: None,
                branch: None,
                log_path: None,
            },
            specs: ProjectSpecAvailability {
                available: false,
                reason: Some("project_stopped".to_string()),
                counts: None,
                ready_count: None,
                blocked_count: None,
                next_ready_id: None,
            },
            detail_error: None,
        };

        assert_eq!(row.agent_summary(), "Agent unavailable | Project stopped");
        assert_eq!(row.agent_detail(), "Agent unavailable | Project stopped");
        assert_eq!(row.spec_summary(), "Specs unavailable | Project stopped");
        assert_eq!(row.spec_detail(), "Project stopped");
    }

    fn project_config(name: &str, status: ProjectStatus) -> ProjectConfig {
        ProjectConfig {
            name: name.to_string(),
            description: Some(format!("{name} project")),
            image: None,
            resources: None,
            container_status: status,
            container_ip: Some("10.0.0.10".to_string()),
            container_limits: None,
            runtimes: Some(ProjectRuntimes {
                jdk: Some(26),
                node: Some("24.14.1".to_string()),
                maven: None,
            }),
            services: Default::default(),
            agent: None,
            agent_session: AgentSessionInfo {
                available: true,
                running: true,
                reason: None,
                pid: Some(42),
                task: Some("auth-fix".to_string()),
                started_at: None,
                branch: Some("feat/auth-fix".to_string()),
                log_path: None,
            },
            specs: ProjectSpecAvailability {
                available: true,
                reason: None,
                counts: None,
                ready_count: Some(2),
                blocked_count: Some(1),
                next_ready_id: Some("auth-session".to_string()),
            },
            ssh_user: Some("dev".to_string()),
        }
    }
}
