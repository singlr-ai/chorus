use std::{any::Any, rc::Rc, sync::Arc, sync::LazyLock};

use acp_thread::AgentConnection;
use agent::{
    AgentTool, AnyAgentTool, NativeAgentToolProvider, ThreadStore, ToolCallEventStream, ToolInput,
};
use agent_client_protocol::schema as acp;
use agent_servers::{AgentServer, AgentServerDelegate};
use anyhow::Result;
use fs::Fs;
use futures::FutureExt as _;
use gpui::{App, AppContext, Entity, SharedString, Task};
use language_model::LanguageModelToolResultContent;
use project::{AgentId, Project};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sing_bridge::{DispatchRequest, SingBridge};

pub static SING_ORCHESTRATOR_AGENT_ID: LazyLock<AgentId> =
    LazyLock::new(|| AgentId::new("Sing Orchestrator"));

#[derive(Clone)]
pub struct SingOrchestratorServer {
    inner: agent::NativeAgentServer,
}

impl SingOrchestratorServer {
    pub fn new(fs: Arc<dyn Fs>, thread_store: Entity<ThreadStore>) -> Self {
        Self {
            inner: agent::NativeAgentServer::new_with_tools(
                fs,
                thread_store,
                SING_ORCHESTRATOR_AGENT_ID.clone(),
                "sing-orchestrator".into(),
                ui::IconName::Sparkle,
                Some(Arc::new(SingToolProvider)),
            ),
        }
    }
}

impl AgentServer for SingOrchestratorServer {
    fn logo(&self) -> ui::IconName {
        self.inner.logo()
    }

    fn agent_id(&self) -> AgentId {
        self.inner.agent_id()
    }

    fn connect(
        &self,
        delegate: AgentServerDelegate,
        project: Entity<Project>,
        cx: &mut App,
    ) -> Task<Result<Rc<dyn AgentConnection>>> {
        self.inner.connect(delegate, project, cx)
    }

    fn into_any(self: Rc<Self>) -> Rc<dyn Any> {
        self
    }

    fn favorite_model_ids(&self, cx: &mut App) -> collections::HashSet<acp::ModelId> {
        self.inner.favorite_model_ids(cx)
    }

    fn toggle_favorite_model(
        &self,
        model_id: acp::ModelId,
        should_be_favorite: bool,
        fs: Arc<dyn Fs>,
        cx: &App,
    ) {
        self.inner
            .toggle_favorite_model(model_id, should_be_favorite, fs, cx);
    }
}

struct SingToolProvider;

impl NativeAgentToolProvider for SingToolProvider {
    fn tools(&self, _project: Entity<Project>, _cx: &mut App) -> Vec<Arc<dyn AnyAgentTool>> {
        vec![
            SingListProjectsTool.erase(),
            SingListSpecsTool.erase(),
            SingShowSpecTool.erase(),
            SingAgentStatusTool.erase(),
            SingDispatchSpecTool.erase(),
        ]
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum SingToolOutput {
    Success { result: serde_json::Value },
    Error { error: String },
}

impl SingToolOutput {
    fn success(value: impl Serialize) -> Self {
        match serde_json::to_value(value) {
            Ok(result) => Self::Success { result },
            Err(error) => Self::Error {
                error: format!("failed to serialize sing tool result: {error}"),
            },
        }
    }

    fn error(error: impl ToString) -> Self {
        Self::Error {
            error: error.to_string(),
        }
    }
}

impl From<SingToolOutput> for LanguageModelToolResultContent {
    fn from(value: SingToolOutput) -> Self {
        match value {
            SingToolOutput::Success { result } => serde_json::to_string_pretty(&result)
                .unwrap_or_else(|error| format!("failed to render sing tool result: {error}"))
                .into(),
            SingToolOutput::Error { error } => error.into(),
        }
    }
}

/// Lists sing projects configured on the engineer's sing host.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct SingListProjectsInput {}

struct SingListProjectsTool;

impl AgentTool for SingListProjectsTool {
    type Input = SingListProjectsInput;
    type Output = SingToolOutput;

    const NAME: &'static str = "sing_list_projects";

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Read
    }

    fn initial_title(
        &self,
        _input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        "List sing projects".into()
    }

    fn run(
        self: Arc<Self>,
        input: ToolInput<Self::Input>,
        _event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output, Self::Output>> {
        cx.spawn(async move |cx| {
            input.recv().await.map_err(SingToolOutput::error)?;
            let task = cx.background_spawn(async move {
                let bridge = SingBridge::load().map_err(SingToolOutput::error)?;
                bridge
                    .list_projects()
                    .await
                    .map(SingToolOutput::success)
                    .map_err(SingToolOutput::error)
            });
            task.await
        })
    }
}

/// Lists the spec board for a sing project.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct SingProjectInput {
    /// The sing project name.
    project: String,
}

struct SingListSpecsTool;

impl AgentTool for SingListSpecsTool {
    type Input = SingProjectInput;
    type Output = SingToolOutput;

    const NAME: &'static str = "sing_list_specs";

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Read
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        match input {
            Ok(input) => format!("List specs for {}", input.project).into(),
            Err(_) => "List sing specs".into(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: ToolInput<Self::Input>,
        _event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output, Self::Output>> {
        cx.spawn(async move |cx| {
            let input = input.recv().await.map_err(SingToolOutput::error)?;
            let task = cx.background_spawn(async move {
                let bridge = SingBridge::load().map_err(SingToolOutput::error)?;
                bridge
                    .list_specs(&input.project)
                    .await
                    .map(SingToolOutput::success)
                    .map_err(SingToolOutput::error)
            });
            task.await
        })
    }
}

/// Shows the full markdown content and metadata for one sing spec.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct SingShowSpecInput {
    /// The sing project name.
    project: String,
    /// The spec id to read.
    spec_id: String,
}

struct SingShowSpecTool;

impl AgentTool for SingShowSpecTool {
    type Input = SingShowSpecInput;
    type Output = SingToolOutput;

    const NAME: &'static str = "sing_show_spec";

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Read
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        match input {
            Ok(input) => format!("Show spec {}", input.spec_id).into(),
            Err(_) => "Show sing spec".into(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: ToolInput<Self::Input>,
        _event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output, Self::Output>> {
        cx.spawn(async move |cx| {
            let input = input.recv().await.map_err(SingToolOutput::error)?;
            let task = cx.background_spawn(async move {
                let bridge = SingBridge::load().map_err(SingToolOutput::error)?;
                bridge
                    .show_spec(&input.project, &input.spec_id)
                    .await
                    .map(SingToolOutput::success)
                    .map_err(SingToolOutput::error)
            });
            task.await
        })
    }
}

/// Reads current agent session status for a sing project.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct SingAgentStatusInput {
    /// The sing project name.
    project: String,
}

struct SingAgentStatusTool;

impl AgentTool for SingAgentStatusTool {
    type Input = SingAgentStatusInput;
    type Output = SingToolOutput;

    const NAME: &'static str = "sing_agent_status";

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Read
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        match input {
            Ok(input) => format!("Check agent status for {}", input.project).into(),
            Err(_) => "Check sing agent status".into(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: ToolInput<Self::Input>,
        _event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output, Self::Output>> {
        cx.spawn(async move |cx| {
            let input = input.recv().await.map_err(SingToolOutput::error)?;
            let task = cx.background_spawn(async move {
                let bridge = SingBridge::load().map_err(SingToolOutput::error)?;
                bridge
                    .project_agent_status(&input.project)
                    .await
                    .map(SingToolOutput::success)
                    .map_err(SingToolOutput::error)
            });
            task.await
        })
    }
}

/// Dispatches the next ready spec, or a specific spec, to a sing-managed agent session.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct SingDispatchSpecInput {
    /// The sing project name.
    project: String,
    /// Optional specific spec id. Omit this to dispatch the next ready spec.
    spec_id: Option<String>,
    /// If true, validate and preview dispatch without mutating spec state or launching an agent.
    #[serde(default)]
    dry_run: bool,
}

struct SingDispatchSpecTool;

impl AgentTool for SingDispatchSpecTool {
    type Input = SingDispatchSpecInput;
    type Output = SingToolOutput;

    const NAME: &'static str = "sing_dispatch_spec";

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Execute
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        match input {
            Ok(input) if input.dry_run => format!("Preview dispatch for {}", input.project).into(),
            Ok(input) => format!("Dispatch spec for {}", input.project).into(),
            Err(_) => "Dispatch sing spec".into(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: ToolInput<Self::Input>,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output, Self::Output>> {
        cx.spawn(async move |cx| {
            let input = input.recv().await.map_err(SingToolOutput::error)?;
            let authorize = cx.update(|cx| {
                let spec = input.spec_id.clone().unwrap_or_else(|| "next ready spec".to_string());
                let context = agent::ToolPermissionContext::new(
                    Self::NAME,
                    vec![input.project.clone(), spec.clone()],
                );
                event_stream.authorize(format!("Dispatch {spec} for {}", input.project), context, cx)
            });
            let task = cx.background_spawn(async move {
                authorize.await.map_err(SingToolOutput::error)?;
                let bridge = SingBridge::load().map_err(SingToolOutput::error)?;
                bridge
                    .dispatch(
                        &input.project,
                        DispatchRequest {
                            spec_id: input.spec_id,
                            background: true,
                            dry_run: input.dry_run,
                        },
                    )
                    .await
                    .map(SingToolOutput::success)
                    .map_err(SingToolOutput::error)
            });
            futures::select! {
                result = task.fuse() => result,
                _ = event_stream.cancelled_by_user().fuse() => Err(SingToolOutput::error("sing dispatch cancelled by user")),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_stable_agent_id() {
        assert_eq!(SING_ORCHESTRATOR_AGENT_ID.as_ref(), "Sing Orchestrator");
    }

    #[test]
    fn renders_success_output_as_pretty_json() {
        let output = SingToolOutput::success(serde_json::json!({"project":"demo"}));
        let content = LanguageModelToolResultContent::from(output);
        assert!(
            matches!(content, LanguageModelToolResultContent::Text(text) if text.contains("demo"))
        );
    }
}
