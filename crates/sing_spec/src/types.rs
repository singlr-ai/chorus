use std::path::PathBuf;

use remote::RemoteConnectionOptions;
use sing_bridge::{BoardSpecRecord, DispatchResult, SpecBoardSummary, SpecStatus};

pub type SpecEntry = BoardSpecRecord;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptionalValue<T> {
    Set(T),
    Clear,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SpecMetadataPatch {
    pub title: Option<String>,
    pub status: Option<SpecStatus>,
    pub assignee: Option<OptionalValue<String>>,
    pub depends_on: Option<Vec<String>>,
    pub branch: Option<OptionalValue<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecBoardState {
    pub project: String,
    pub index_path: PathBuf,
    pub specs: Vec<SpecEntry>,
    pub summary: SpecBoardSummary,
}

impl SpecBoardState {
    pub fn find_spec(&self, spec_id: &str) -> Option<&SpecEntry> {
        self.specs.iter().find(|spec| spec.spec.id == spec_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecOpenTarget {
    pub project: String,
    pub workspace_root: PathBuf,
    pub spec_path: PathBuf,
    pub relative_path: PathBuf,
    pub connection_options: RemoteConnectionOptions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecDocumentState {
    pub project: String,
    pub spec: SpecEntry,
    pub spec_path: PathBuf,
    pub content_available: bool,
    pub content: Option<String>,
    pub open_target: SpecOpenTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecMutationResult {
    pub board: SpecBoardState,
    pub spec: SpecEntry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecDispatchResult {
    pub board: SpecBoardState,
    pub dispatch: DispatchResult,
    pub selected_spec: Option<SpecEntry>,
}
