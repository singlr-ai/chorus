use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use sing_bridge::{
    ProjectConfig, ProjectRemoteTarget, ProjectStartResult, ProjectStopResult, ProjectSummary,
    SingBridge,
};

#[async_trait]
pub trait SingProjectClient: Send + Sync {
    async fn list_projects(&self) -> Result<Vec<ProjectSummary>>;
    async fn project_config(&self, project: &str) -> Result<ProjectConfig>;
    async fn project_remote_target(&self, project: &str) -> Result<ProjectRemoteTarget>;
    async fn start_project(&self, project: &str) -> Result<ProjectStartResult>;
    async fn stop_project(&self, project: &str) -> Result<ProjectStopResult>;
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
