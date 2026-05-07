use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use sing_bridge::{
    AgentLog, AgentReport, ProjectAgentStatus, ProjectConfig, ProjectRemoteTarget,
    ProjectStartResult, ProjectStopResult, ProjectSummary, SingBridge, SpecBoard,
};

#[async_trait]
pub trait SingProjectClient: Send + Sync {
    async fn list_projects(&self) -> Result<Vec<ProjectSummary>>;
    async fn project_config(&self, project: &str) -> Result<ProjectConfig>;
    async fn project_remote_target(&self, project: &str) -> Result<ProjectRemoteTarget>;
    async fn start_project(&self, project: &str) -> Result<ProjectStartResult>;
    async fn stop_project(&self, project: &str) -> Result<ProjectStopResult>;
    async fn list_specs(&self, _project: &str) -> Result<SpecBoard> {
        anyhow::bail!("spec board is unavailable from this project client")
    }
    async fn agent_status(&self, _project: &str) -> Result<ProjectAgentStatus> {
        anyhow::bail!("agent status is unavailable from this project client")
    }
    async fn agent_log(&self, _project: &str, _tail: u32) -> Result<AgentLog> {
        anyhow::bail!("agent log is unavailable from this project client")
    }
    async fn agent_report(&self, _project: &str) -> Result<AgentReport> {
        anyhow::bail!("agent report is unavailable from this project client")
    }
}

#[async_trait]
impl SingProjectClient for SingBridge {
    async fn list_projects(&self) -> Result<Vec<ProjectSummary>> {
        Ok(SingBridge::list_projects(self).await?)
    }

    async fn project_config(&self, project: &str) -> Result<ProjectConfig> {
        Ok(SingBridge::project_config(self, project).await?)
    }

    async fn project_remote_target(&self, project: &str) -> Result<ProjectRemoteTarget> {
        Ok(SingBridge::project_remote_target(self, project).await?)
    }

    async fn start_project(&self, project: &str) -> Result<ProjectStartResult> {
        Ok(SingBridge::start_project(self, project).await?)
    }

    async fn stop_project(&self, project: &str) -> Result<ProjectStopResult> {
        Ok(SingBridge::stop_project(self, project).await?)
    }

    async fn list_specs(&self, project: &str) -> Result<SpecBoard> {
        Ok(SingBridge::list_specs(self, project).await?)
    }

    async fn agent_status(&self, project: &str) -> Result<ProjectAgentStatus> {
        Ok(SingBridge::project_agent_status(self, project).await?)
    }

    async fn agent_log(&self, project: &str, tail: u32) -> Result<AgentLog> {
        Ok(SingBridge::project_agent_log(self, project, tail).await?)
    }

    async fn agent_report(&self, project: &str) -> Result<AgentReport> {
        Ok(SingBridge::project_agent_report(self, project).await?)
    }
}

pub trait SingProjectClientFactory: Send + Sync {
    fn create(&self) -> Result<Arc<dyn SingProjectClient>>;
}

#[derive(Default)]
pub struct DefaultSingProjectClientFactory;

impl SingProjectClientFactory for DefaultSingProjectClientFactory {
    fn create(&self) -> Result<Arc<dyn SingProjectClient>> {
        Ok(Arc::new(SingBridge::load()?))
    }
}
