use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use sing_bridge::{
    CreateSpecRequest, CreateSpecResult, ProjectRemoteTarget, ProjectSummary, SingBridge,
    SpecDocument, SpecStatus, UpdateSpecStatusResult,
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
