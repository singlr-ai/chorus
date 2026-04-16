use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, anyhow};
use sing_bridge::{CreateSpecRequest, ProjectRemoteTarget, SingBridge, SpecDocument, SpecStatus};

use crate::{
    client::SingSpecClient,
    file_system::{SpecFileSystem, SshSpecFileSystem},
    index::{SpecIndexDocument, compute_board},
    types::{
        SpecBoardState, SpecDocumentState, SpecMetadataPatch, SpecMutationResult, SpecOpenTarget,
    },
};

#[derive(Clone)]
pub struct RemoteSpecStore {
    client: Arc<dyn SingSpecClient>,
    file_system: Arc<dyn SpecFileSystem>,
}

impl RemoteSpecStore {
    pub fn new(client: Arc<dyn SingSpecClient>, file_system: Arc<dyn SpecFileSystem>) -> Self {
        Self {
            client,
            file_system,
        }
    }

    pub fn load() -> Result<Self> {
        Ok(Self::new(
            Arc::new(SingBridge::load()?),
            Arc::new(SshSpecFileSystem::default()),
        ))
    }

    pub async fn load_board(&self, project: &str) -> Result<SpecBoardState> {
        let target = self.client.project_remote_target(project).await?;
        let index_path = spec_index_path(&target);
        let document = self.read_index_document(&target, &index_path).await?;
        build_board_state(project.to_string(), index_path, document)
    }

    pub async fn load_spec(&self, project: &str, spec_id: &str) -> Result<SpecDocumentState> {
        let target = self.client.project_remote_target(project).await?;
        let document = self.client.show_spec(project, spec_id).await?;
        build_spec_document_state(project, &target, document)
    }

    pub async fn open_target(&self, project: &str, spec_id: &str) -> Result<SpecOpenTarget> {
        Ok(self.load_spec(project, spec_id).await?.open_target)
    }

    pub async fn create_spec(
        &self,
        project: &str,
        request: CreateSpecRequest,
    ) -> Result<SpecMutationResult> {
        let created = self.client.create_spec(project, request).await?;
        let board = self.load_board(project).await?;
        let spec = board.find_spec(&created.spec.id).cloned().ok_or_else(|| {
            anyhow!(
                "created spec `{}` was not found after refresh",
                created.spec.id
            )
        })?;
        Ok(SpecMutationResult { board, spec })
    }

    pub async fn update_metadata(
        &self,
        project: &str,
        spec_id: &str,
        patch: SpecMetadataPatch,
    ) -> Result<SpecMutationResult> {
        let target = self.client.project_remote_target(project).await?;
        let index_path = spec_index_path(&target);
        let mut document = self.read_index_document(&target, &index_path).await?;
        document.update_spec(spec_id, &patch)?;
        let content = document.render()?;
        self.file_system
            .write_text_atomic(&target, &index_path, &content)
            .await?;

        let board = self.load_board(project).await?;
        let spec = board
            .find_spec(spec_id)
            .cloned()
            .ok_or_else(|| anyhow!("updated spec `{spec_id}` was not found after refresh"))?;
        Ok(SpecMutationResult { board, spec })
    }

    pub async fn update_status(
        &self,
        project: &str,
        spec_id: &str,
        status: SpecStatus,
    ) -> Result<SpecMutationResult> {
        let updated = self
            .client
            .update_spec_status(project, spec_id, status)
            .await?;
        let board = self.load_board(project).await?;
        let spec = board.find_spec(&updated.spec.id).cloned().ok_or_else(|| {
            anyhow!(
                "updated spec `{}` was not found after refresh",
                updated.spec.id
            )
        })?;
        Ok(SpecMutationResult { board, spec })
    }

    async fn read_index_document(
        &self,
        target: &ProjectRemoteTarget,
        index_path: &Path,
    ) -> Result<SpecIndexDocument> {
        let content = self.file_system.read_text(target, index_path).await?;
        SpecIndexDocument::parse(content.as_deref())
    }
}

fn build_board_state(
    project: String,
    index_path: PathBuf,
    document: SpecIndexDocument,
) -> Result<SpecBoardState> {
    let specs = document.spec_records()?;
    let (specs, summary) = compute_board(&specs);
    Ok(SpecBoardState {
        project,
        index_path,
        specs,
        summary,
    })
}

fn build_spec_document_state(
    project: &str,
    target: &ProjectRemoteTarget,
    document: SpecDocument,
) -> Result<SpecDocumentState> {
    let spec_path = PathBuf::from(&document.spec_path);
    let open_target = build_open_target(project, target, &spec_path)?;
    Ok(SpecDocumentState {
        project: project.to_string(),
        spec: document.spec,
        spec_path,
        content_available: document.content_available,
        content: document.content,
        open_target,
    })
}

fn build_open_target(
    project: &str,
    target: &ProjectRemoteTarget,
    spec_path: &Path,
) -> Result<SpecOpenTarget> {
    let relative_path = spec_path
        .strip_prefix(&target.workspace_root)
        .with_context(|| {
            format!(
                "spec path {} is not inside workspace root {}",
                spec_path.display(),
                target.workspace_root.display()
            )
        })?
        .to_path_buf();

    Ok(SpecOpenTarget {
        project: project.to_string(),
        workspace_root: target.workspace_root.clone(),
        spec_path: spec_path.to_path_buf(),
        relative_path,
        connection_options: target.connection_options.clone(),
    })
}

fn spec_index_path(target: &ProjectRemoteTarget) -> PathBuf {
    target.workspace_root.join("specs").join("index.yaml")
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        path::{Path, PathBuf},
        sync::{Arc, Mutex},
    };

    use anyhow::{Result, anyhow};
    use async_trait::async_trait;
    use futures::executor::block_on;
    use pretty_assertions::assert_eq;
    use remote::{RemoteConnectionOptions, SshConnectionOptions};
    use sing_bridge::{
        CreateSpecRequest, CreateSpecResult, ProjectRemoteTarget, ProjectStatus, ProjectSummary,
        SpecDocument, SpecRecord, SpecStatus, UpdateSpecStatusResult,
    };

    use super::RemoteSpecStore;
    use crate::{
        client::SingSpecClient,
        file_system::SpecFileSystem,
        index::SpecIndexDocument,
        types::{OptionalValue, SpecMetadataPatch},
    };

    #[derive(Default)]
    struct FixtureState {
        files: HashMap<PathBuf, String>,
    }

    #[derive(Clone)]
    struct MemorySpecFileSystem {
        state: Arc<Mutex<FixtureState>>,
    }

    #[async_trait]
    impl SpecFileSystem for MemorySpecFileSystem {
        async fn read_text(
            &self,
            _target: &ProjectRemoteTarget,
            path: &Path,
        ) -> Result<Option<String>> {
            Ok(self.state.lock().unwrap().files.get(path).cloned())
        }

        async fn write_text_atomic(
            &self,
            _target: &ProjectRemoteTarget,
            path: &Path,
            content: &str,
        ) -> Result<()> {
            self.state
                .lock()
                .unwrap()
                .files
                .insert(path.to_path_buf(), content.to_string());
            Ok(())
        }
    }

    #[derive(Clone)]
    struct FakeClient {
        state: Arc<Mutex<FixtureState>>,
        target: ProjectRemoteTarget,
    }

    #[async_trait]
    impl SingSpecClient for FakeClient {
        async fn list_projects(&self) -> Result<Vec<ProjectSummary>> {
            Ok(vec![ProjectSummary {
                name: self.target.project.clone(),
                status: ProjectStatus::Running,
                ip: Some(self.target.container_ip.clone()),
            }])
        }

        async fn project_remote_target(&self, _project: &str) -> Result<ProjectRemoteTarget> {
            Ok(self.target.clone())
        }

        async fn show_spec(&self, project: &str, spec_id: &str) -> Result<SpecDocument> {
            let spec_path = self
                .target
                .workspace_root
                .join("specs")
                .join(spec_id)
                .join("spec.md");
            let content = self.state.lock().unwrap().files.get(&spec_path).cloned();
            Ok(SpecDocument {
                name: project.to_string(),
                spec: {
                    let board = RemoteSpecStore::new(
                        Arc::new(self.clone()),
                        Arc::new(MemorySpecFileSystem {
                            state: self.state.clone(),
                        }),
                    )
                    .load_board(project)
                    .await?;
                    board
                        .find_spec(spec_id)
                        .cloned()
                        .ok_or_else(|| anyhow!("missing spec `{spec_id}`"))?
                },
                spec_path: spec_path.display().to_string(),
                content_available: content.is_some(),
                content,
            })
        }

        async fn create_spec(
            &self,
            _project: &str,
            request: CreateSpecRequest,
        ) -> Result<CreateSpecResult> {
            let id = request.id.clone().unwrap_or_else(|| "new-spec".to_string());
            let index_path = self.target.workspace_root.join("specs").join("index.yaml");
            let spec_path = self
                .target
                .workspace_root
                .join("specs")
                .join(&id)
                .join("spec.md");

            let state = &mut *self.state.lock().unwrap();
            let existing = state.files.get(&index_path).cloned();
            let mut document = crate::index::SpecIndexDocument::parse(existing.as_deref())?;
            document
                .update_spec(
                    &id,
                    &SpecMetadataPatch {
                        title: Some(request.title.clone()),
                        status: Some(request.status),
                        assignee: request.assignee.clone().map(OptionalValue::Set),
                        branch: request.branch.clone().map(OptionalValue::Set),
                        depends_on: Some(request.depends_on.clone()),
                    },
                )
                .ok();

            if existing.is_none() {
                state
                    .files
                    .insert(index_path.clone(), "specs: []\n".to_string());
            }
            let mut value =
                serde_yaml::from_str::<serde_yaml::Value>(state.files.get(&index_path).unwrap())?;
            let specs = value
                .get_mut("specs")
                .and_then(serde_yaml::Value::as_sequence_mut)
                .unwrap();
            specs.push(serde_yaml::Value::Mapping(serde_yaml::Mapping::from_iter(
                [
                    (
                        serde_yaml::Value::String("id".to_string()),
                        serde_yaml::Value::String(id.clone()),
                    ),
                    (
                        serde_yaml::Value::String("title".to_string()),
                        serde_yaml::Value::String(request.title.clone()),
                    ),
                    (
                        serde_yaml::Value::String("status".to_string()),
                        serde_yaml::Value::String(request.status.as_cli_arg().to_string()),
                    ),
                ],
            )));
            state.files.insert(
                index_path,
                serde_yaml::to_string(&value)?
                    .trim_start_matches("---\n")
                    .to_string(),
            );
            state
                .files
                .insert(spec_path.clone(), format!("# {}\n", request.title));

            Ok(CreateSpecResult {
                name: self.target.project.clone(),
                created: true,
                spec: SpecRecord {
                    id,
                    title: request.title,
                    status: request.status,
                    assignee: request.assignee,
                    depends_on: request.depends_on,
                    branch: request.branch,
                },
                spec_path: spec_path.display().to_string(),
            })
        }

        async fn update_spec_status(
            &self,
            project: &str,
            spec_id: &str,
            status: SpecStatus,
        ) -> Result<UpdateSpecStatusResult> {
            let index_path = self.target.workspace_root.join("specs").join("index.yaml");

            {
                let state = &mut *self.state.lock().unwrap();
                let existing = state.files.get(&index_path).cloned();
                let mut document = SpecIndexDocument::parse(existing.as_deref())?;
                document.update_spec(
                    spec_id,
                    &SpecMetadataPatch {
                        status: Some(status),
                        ..Default::default()
                    },
                )?;
                state.files.insert(index_path.clone(), document.render()?);
            }

            let board = RemoteSpecStore::new(
                Arc::new(self.clone()),
                Arc::new(MemorySpecFileSystem {
                    state: self.state.clone(),
                }),
            )
            .load_board(project)
            .await?;
            let spec = board
                .find_spec(spec_id)
                .cloned()
                .ok_or_else(|| anyhow!("missing spec `{spec_id}` after status update"))?;

            Ok(UpdateSpecStatusResult {
                name: self.target.project.clone(),
                spec: spec.spec,
                summary: board.summary,
            })
        }
    }

    fn fixture_store(index_yaml: &str) -> RemoteSpecStore {
        let workspace_root = PathBuf::from("/home/dev/workspace");
        let index_path = workspace_root.join("specs").join("index.yaml");
        let state = Arc::new(Mutex::new(FixtureState {
            files: HashMap::from([(index_path, index_yaml.to_string())]),
        }));
        let target = ProjectRemoteTarget {
            project: "demo".to_string(),
            ssh_user: "dev".to_string(),
            container_ip: "10.0.0.2".to_string(),
            workspace_root,
            connection_options: RemoteConnectionOptions::Ssh(SshConnectionOptions {
                host: "10.0.0.2".into(),
                username: Some("dev".to_string()),
                ..Default::default()
            }),
        };

        RemoteSpecStore::new(
            Arc::new(FakeClient {
                state: state.clone(),
                target,
            }),
            Arc::new(MemorySpecFileSystem { state }),
        )
    }

    #[test]
    fn load_board_defaults_to_empty_when_index_is_missing() {
        let workspace_root = PathBuf::from("/home/dev/workspace");
        let state = Arc::new(Mutex::new(FixtureState::default()));
        let target = ProjectRemoteTarget {
            project: "demo".to_string(),
            ssh_user: "dev".to_string(),
            container_ip: "10.0.0.2".to_string(),
            workspace_root,
            connection_options: RemoteConnectionOptions::Ssh(SshConnectionOptions {
                host: "10.0.0.2".into(),
                username: Some("dev".to_string()),
                ..Default::default()
            }),
        };
        let store = RemoteSpecStore::new(
            Arc::new(FakeClient {
                state: state.clone(),
                target,
            }),
            Arc::new(MemorySpecFileSystem { state }),
        );

        let board = block_on(store.load_board("demo")).unwrap();
        assert!(board.specs.is_empty());
        assert_eq!(board.summary.ready_count, 0);
    }

    #[test]
    fn update_metadata_refreshes_board_and_preserves_unknown_fields() {
        let store = fixture_store(
            r#"
owner: chorus
specs:
  - id: alpha
    title: Alpha
    status: pending
    priority: high
"#,
        );

        let result = block_on(store.update_metadata(
            "demo",
            "alpha",
            SpecMetadataPatch {
                title: Some("Alpha Updated".to_string()),
                status: Some(SpecStatus::Done),
                branch: Some(OptionalValue::Set("feat/alpha".to_string())),
                ..Default::default()
            },
        ))
        .unwrap();

        assert_eq!(result.spec.spec.title, "Alpha Updated");
        assert_eq!(result.spec.spec.status, SpecStatus::Done);
        assert_eq!(result.board.summary.counts.done, 1);
    }

    #[test]
    fn create_spec_returns_refreshed_board_and_openable_document() {
        let store = fixture_store("specs: []\n");

        let result = block_on(store.create_spec(
            "demo",
            CreateSpecRequest {
                id: Some("new-spec".to_string()),
                title: "New Spec".to_string(),
                status: SpecStatus::Pending,
                assignee: None,
                branch: None,
                depends_on: Vec::new(),
            },
        ))
        .unwrap();

        assert_eq!(result.board.specs.len(), 1);
        assert_eq!(result.spec.spec.id, "new-spec");

        let document = block_on(store.load_spec("demo", "new-spec")).unwrap();
        assert_eq!(
            document.open_target.relative_path,
            PathBuf::from("specs/new-spec/spec.md")
        );
        assert_eq!(document.content.as_deref(), Some("# New Spec\n"));
    }

    #[test]
    fn update_status_refreshes_summary_and_unblocks_dependency() {
        let store = fixture_store(
            r#"
specs:
  - id: alpha
    title: Alpha
    status: pending
  - id: beta
    title: Beta
    status: pending
    depends_on:
      - alpha
"#,
        );

        let result = block_on(store.update_status("demo", "alpha", SpecStatus::Done)).unwrap();

        assert_eq!(result.spec.spec.status, SpecStatus::Done);
        assert_eq!(result.board.summary.ready_count, 1);
        assert_eq!(result.board.summary.blocked_count, 0);
        assert_eq!(result.board.summary.next_ready_id.as_deref(), Some("beta"));
    }
}
