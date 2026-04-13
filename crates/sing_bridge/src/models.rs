use std::path::PathBuf;

use indexmap::IndexMap;
use remote::RemoteConnectionOptions;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProjectStatus {
    Running,
    Stopped,
    NotCreated,
    #[default]
    Error,
}

impl ProjectStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::NotCreated => "not_created",
            Self::Error => "error",
        }
    }
}

impl std::fmt::Display for ProjectStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectStartStatus {
    Running,
    Started,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SpecStatus {
    #[default]
    Pending,
    InProgress,
    Review,
    Done,
}

impl SpecStatus {
    pub fn as_cli_arg(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Review => "review",
            Self::Done => "done",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchMode {
    Background,
    Foreground,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub name: String,
    pub status: ProjectStatus,
    #[serde(default)]
    pub ip: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectResources {
    pub cpu: u32,
    pub memory: String,
    pub disk: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProjectContainerLimits {
    #[serde(default)]
    pub cpu: Option<String>,
    #[serde(default)]
    pub memory: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProjectRuntimes {
    #[serde(default)]
    pub jdk: Option<u32>,
    #[serde(default)]
    pub node: Option<String>,
    #[serde(default)]
    pub maven: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectServiceConfig {
    pub image: String,
    #[serde(default)]
    pub ports: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectAgentConfig {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub auto_snapshot: bool,
    #[serde(default)]
    pub auto_branch: bool,
    #[serde(default)]
    pub specs_dir: Option<String>,
    #[serde(default)]
    pub install: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AgentSessionInfo {
    pub available: bool,
    pub running: bool,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub pid: Option<i32>,
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub log_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SpecCounts {
    #[serde(default)]
    pub pending: u32,
    #[serde(default)]
    pub in_progress: u32,
    #[serde(default)]
    pub review: u32,
    #[serde(default)]
    pub done: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProjectSpecAvailability {
    pub available: bool,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub counts: Option<SpecCounts>,
    #[serde(default)]
    pub ready_count: Option<u32>,
    #[serde(default)]
    pub blocked_count: Option<u32>,
    #[serde(default)]
    pub next_ready_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectConfig {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub resources: Option<ProjectResources>,
    pub container_status: ProjectStatus,
    #[serde(default)]
    pub container_ip: Option<String>,
    #[serde(default)]
    pub container_limits: Option<ProjectContainerLimits>,
    #[serde(default)]
    pub runtimes: Option<ProjectRuntimes>,
    #[serde(default)]
    pub services: IndexMap<String, ProjectServiceConfig>,
    #[serde(default)]
    pub agent: Option<ProjectAgentConfig>,
    pub agent_session: AgentSessionInfo,
    pub specs: ProjectSpecAvailability,
    #[serde(default)]
    pub ssh_user: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectStartResult {
    pub name: String,
    pub status: ProjectStartStatus,
    #[serde(default)]
    pub ip: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProjectStopResult {
    #[serde(default)]
    pub stopped: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectServiceList {
    pub name: String,
    #[serde(default)]
    pub services: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectServiceLogs {
    pub name: String,
    pub service: String,
    #[serde(default)]
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostStatus {
    pub hostname: String,
    pub os: String,
    pub cores: u32,
    pub threads: u32,
    pub memory_mb: u64,
    pub storage_backend: String,
    #[serde(default)]
    pub pool: Option<String>,
    #[serde(default)]
    pub pool_disk: Option<String>,
    #[serde(default)]
    pub pool_size: Option<String>,
    #[serde(default)]
    pub pool_allocated: Option<String>,
    #[serde(default)]
    pub pool_free: Option<String>,
    #[serde(default)]
    pub pool_capacity: Option<String>,
    #[serde(default)]
    pub disk_size: Option<String>,
    #[serde(default)]
    pub disk_used: Option<String>,
    #[serde(default)]
    pub disk_available: Option<String>,
    #[serde(default)]
    pub disk_use_percent: Option<String>,
    pub incus_version: String,
    pub initialized_at: String,
    pub containers_total: u64,
    pub containers_running: u64,
    pub containers_stopped: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecRecord {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub status: SpecStatus,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardSpecRecord {
    #[serde(flatten)]
    pub spec: SpecRecord,
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub blocked: bool,
    #[serde(default)]
    pub unmet_dependencies: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecBoardSummary {
    pub counts: SpecCounts,
    #[serde(default)]
    pub ready_count: u32,
    #[serde(default)]
    pub blocked_count: u32,
    #[serde(default)]
    pub next_ready_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecBoard {
    pub name: String,
    #[serde(default)]
    pub specs: Vec<BoardSpecRecord>,
    pub counts: SpecCounts,
    pub summary: SpecBoardSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecDocument {
    pub name: String,
    pub spec: BoardSpecRecord,
    pub spec_path: String,
    pub content_available: bool,
    #[serde(default)]
    pub content: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateSpecResult {
    pub name: String,
    pub created: bool,
    pub spec: SpecRecord,
    pub spec_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateSpecStatusResult {
    pub name: String,
    pub spec: SpecRecord,
    pub summary: SpecBoardSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchResult {
    pub name: String,
    pub spec_id: String,
    pub spec_title: String,
    pub mode: DispatchMode,
    pub task: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSummary {
    pub name: String,
    pub status: String,
    #[serde(default)]
    pub elapsed: Option<String>,
    #[serde(default)]
    pub commits: Option<u32>,
    #[serde(default)]
    pub task: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AgentSummaryList {
    #[serde(default)]
    pub agents: Vec<AgentSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectAgentStatus {
    pub name: String,
    pub agent_running: bool,
    #[serde(default)]
    pub pid: Option<i32>,
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub log_path: Option<String>,
    #[serde(default)]
    pub commits_since_launch: Option<u32>,
    #[serde(default)]
    pub last_commit_minutes_ago: Option<u64>,
    #[serde(default)]
    pub tasks: Option<SpecCounts>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLog {
    pub name: String,
    #[serde(default)]
    pub lines: Vec<String>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRemoteTarget {
    pub project: String,
    pub ssh_user: String,
    pub container_ip: String,
    pub workspace_root: PathBuf,
    pub connection_options: RemoteConnectionOptions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateSpecRequest {
    pub title: String,
    pub id: Option<String>,
    pub status: SpecStatus,
    pub assignee: Option<String>,
    pub branch: Option<String>,
    pub depends_on: Vec<String>,
}

impl Default for CreateSpecRequest {
    fn default() -> Self {
        Self {
            title: String::new(),
            id: None,
            status: SpecStatus::Pending,
            assignee: None,
            branch: None,
            depends_on: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchRequest {
    pub spec_id: Option<String>,
    pub background: bool,
    pub dry_run: bool,
}

impl Default for DispatchRequest {
    fn default() -> Self {
        Self {
            spec_id: None,
            background: true,
            dry_run: false,
        }
    }
}
