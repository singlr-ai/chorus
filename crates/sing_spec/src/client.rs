use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use sing_bridge::{
    AgentLog, AgentReport, CreateSpecRequest, CreateSpecResult, DispatchRequest, DispatchResult,
    ProjectAgentStatus, ProjectRemoteTarget, ProjectSummary, SingBridge, SpecDocument, SpecStatus,
    StopAgentResult, UpdateSpecStatusResult,
};

#[async_trait]
pub trait SingSpecClient: Send + Sync {
    async fn list_projects(&self) -> Result<Vec<ProjectSummary>>;
    async fn project_remote_target(&self, project: &str) -> Result<ProjectRemoteTarget>;
    async fn show_spec(&self, project: &str, spec_id: &str) -> Result<SpecDocument>;
    async fn create_spec(
        &self,
        project: &str,
        request: CreateSpecRequest,
    ) -> Result<CreateSpecResult>;
    async fn update_spec_status(
        &self,
        project: &str,
        spec_id: &str,
        status: SpecStatus,
    ) -> Result<UpdateSpecStatusResult>;
    async fn dispatch(&self, project: &str, request: DispatchRequest) -> Result<DispatchResult>;
    async fn agent_status(&self, project: &str) -> Result<ProjectAgentStatus>;
    async fn agent_log(&self, project: &str, tail: u32) -> Result<AgentLog>;
    async fn stop_agent(&self, project: &str) -> Result<StopAgentResult>;
    async fn agent_report(&self, project: &str) -> Result<AgentReport>;
}

#[async_trait]
impl SingSpecClient for SingBridge {
    async fn list_projects(&self) -> Result<Vec<ProjectSummary>> {
        Ok(SingBridge::list_projects(self).await?)
    }

    async fn project_remote_target(&self, project: &str) -> Result<ProjectRemoteTarget> {
        Ok(SingBridge::project_remote_target(self, project).await?)
    }

    async fn show_spec(&self, project: &str, spec_id: &str) -> Result<SpecDocument> {
        Ok(SingBridge::show_spec(self, project, spec_id).await?)
    }

    async fn create_spec(
        &self,
        project: &str,
        request: CreateSpecRequest,
    ) -> Result<CreateSpecResult> {
        Ok(SingBridge::create_spec(self, project, request).await?)
    }

    async fn update_spec_status(
        &self,
        project: &str,
        spec_id: &str,
        status: SpecStatus,
    ) -> Result<UpdateSpecStatusResult> {
        Ok(SingBridge::update_spec_status(self, project, spec_id, status).await?)
    }

    async fn dispatch(&self, project: &str, request: DispatchRequest) -> Result<DispatchResult> {
        Ok(SingBridge::dispatch(self, project, request).await?)
    }

    async fn agent_status(&self, project: &str) -> Result<ProjectAgentStatus> {
        Ok(SingBridge::project_agent_status(self, project).await?)
    }

    async fn agent_log(&self, project: &str, tail: u32) -> Result<AgentLog> {
        Ok(SingBridge::project_agent_log(self, project, tail).await?)
    }

    async fn stop_agent(&self, project: &str) -> Result<StopAgentResult> {
        Ok(SingBridge::stop_project_agent(self, project).await?)
    }

    async fn agent_report(&self, project: &str) -> Result<AgentReport> {
        Ok(SingBridge::project_agent_report(self, project).await?)
    }
}

pub trait SingSpecClientFactory: Send + Sync {
    fn create(&self) -> Result<Arc<dyn SingSpecClient>>;
}

#[derive(Default)]
pub struct DefaultSingSpecClientFactory;

impl SingSpecClientFactory for DefaultSingSpecClientFactory {
    fn create(&self) -> Result<Arc<dyn SingSpecClient>> {
        Ok(Arc::new(SingBridge::load()?))
    }
}
