use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use db::kvp::KeyValueStore;
use editor::Editor;
use gpui::{
    Action, AnyElement, App, AsyncWindowContext, Context, Entity, EventEmitter, FocusHandle,
    Focusable, ParentElement, Pixels, Render, StatefulInteractiveElement, Styled, Task, WeakEntity,
    Window, actions, px,
};
use recent_projects::open_remote_project;
use serde::{Deserialize, Serialize};
use sing_bridge::{
    AgentLog, AgentReport, CreateSpecRequest, ProjectAgentStatus, ProjectStatus, ProjectSummary,
    SpecStatus, StopAgentResult,
};
use ui::{
    Button, ButtonStyle, Chip, Color, Icon, IconButtonShape, IconName, IconSize, Indicator, Label,
    LabelSize, TintColor, Tooltip, prelude::*,
};
use util::{ResultExt, TryFutureExt};
use workspace::{
    MultiWorkspace, OpenOptions, Toast, Workspace,
    dock::{DockPosition, Panel, PanelEvent},
    notifications::NotificationId,
};

use crate::{
    DefaultSingSpecClientFactory, RemoteSpecStore, SingSpecClient, SingSpecClientFactory,
    SpecBoardState, SpecEntry, SshSpecFileSystem,
};

const SING_SPEC_BOARD_PANEL_KEY: &str = "SingSpecBoardPanel";
const REFRESH_INTERVAL: Duration = Duration::from_secs(30);

actions!(sing_spec, [Toggle, ToggleFocus]);

#[derive(Debug, Serialize, Deserialize, Default)]
struct SerializedSingSpecBoardPanel {
    active: Option<bool>,
    selected_project: Option<String>,
    selected_spec: Option<String>,
    position: Option<SerializedDockPosition>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SerializedDockPosition {
    Bottom,
    Right,
}

impl SerializedDockPosition {
    fn from_dock_position(position: DockPosition) -> Self {
        match position {
            DockPosition::Bottom => Self::Bottom,
            DockPosition::Right => Self::Right,
            DockPosition::Left => Self::Right,
        }
    }

    fn to_dock_position(self) -> DockPosition {
        match self {
            Self::Bottom => DockPosition::Bottom,
            Self::Right => DockPosition::Right,
        }
    }
}

#[derive(Debug, Clone)]
struct LoadedBoardState {
    projects: Vec<ProjectSummary>,
    selected_project: Option<String>,
    board: Option<SpecBoardState>,
    selected_spec: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PendingSpecAction {
    Open(String),
    Move(String, SpecStatus),
    Dispatch(Option<String>),
    AgentStatus,
    AgentLog,
    StopAgent,
    AgentReport,
    Create,
}

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
            workspace.toggle_panel_focus::<SingSpecBoardPanel>(window, cx);
        });
        workspace.register_action(|workspace, _: &Toggle, window, cx| {
            if !workspace.toggle_panel_focus::<SingSpecBoardPanel>(window, cx) {
                workspace.close_panel::<SingSpecBoardPanel>(window, cx);
            }
        });
    })
    .detach();
}

pub struct SingSpecBoardPanel {
    workspace: WeakEntity<Workspace>,
    serialization_key: Option<String>,
    focus_handle: FocusHandle,
    client_factory: Arc<dyn SingSpecClientFactory>,
    client: Option<Arc<dyn SingSpecClient>>,
    position: DockPosition,
    active: bool,
    loading: bool,
    last_error: Option<String>,
    last_refreshed_at: Option<Instant>,
    projects: Vec<ProjectSummary>,
    selected_project: Option<String>,
    board: Option<SpecBoardState>,
    selected_spec: Option<String>,
    new_spec_title: Entity<Editor>,
    show_new_spec_form: bool,
    pending_action: Option<PendingSpecAction>,
    current_request_id: usize,
    pending_serialization: Task<Option<()>>,
    polling_task: Task<()>,
}

impl SingSpecBoardPanel {
    pub async fn load(
        workspace: WeakEntity<Workspace>,
        cx: AsyncWindowContext,
    ) -> anyhow::Result<Entity<Self>> {
        Self::load_with_factory(workspace, Arc::new(DefaultSingSpecClientFactory), cx).await
    }

    async fn load_with_factory(
        workspace: WeakEntity<Workspace>,
        client_factory: Arc<dyn SingSpecClientFactory>,
        mut cx: AsyncWindowContext,
    ) -> anyhow::Result<Entity<Self>> {
        let serialized = match workspace
            .read_with(&cx, |workspace, _| Self::serialization_key(workspace))
            .ok()
            .flatten()
        {
            Some(serialization_key) => {
                let kvp = cx.update(|_, cx| KeyValueStore::global(cx))?;
                cx.background_spawn(async move { kvp.read_kvp(&serialization_key) })
                    .await
                    .context("loading sing spec board panel")
                    .log_err()
                    .flatten()
                    .map(|panel| serde_json::from_str::<SerializedSingSpecBoardPanel>(&panel))
                    .transpose()
                    .log_err()
                    .flatten()
            }
            None => None,
        };

        workspace.update_in(&mut cx, |workspace, window, cx| {
            let panel = Self::new(workspace, serialized.as_ref(), client_factory, window, cx);
            panel.update(cx, |panel, cx| {
                panel.refresh(window, cx);
                panel.start_polling(window, cx);
            });
            panel
        })
    }

    fn new(
        workspace: &mut Workspace,
        serialized: Option<&SerializedSingSpecBoardPanel>,
        client_factory: Arc<dyn SingSpecClientFactory>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let serialization_key = Self::serialization_key(workspace);
        let workspace = workspace.weak_handle();
        let position = serialized
            .and_then(|panel| panel.position)
            .map(SerializedDockPosition::to_dock_position)
            .unwrap_or(DockPosition::Bottom);

        cx.new(|cx| {
            let new_spec_title = cx.new(|cx| {
                let mut editor = Editor::single_line(window, cx);
                editor.set_placeholder_text("Describe the spec...", window, cx);
                editor
            });

            Self {
                workspace,
                serialization_key,
                focus_handle: cx.focus_handle(),
                client_factory,
                client: None,
                position,
                active: serialized.and_then(|panel| panel.active).unwrap_or(false),
                loading: false,
                last_error: None,
                last_refreshed_at: None,
                projects: Vec::new(),
                selected_project: serialized.and_then(|panel| panel.selected_project.clone()),
                board: None,
                selected_spec: serialized.and_then(|panel| panel.selected_spec.clone()),
                new_spec_title,
                show_new_spec_form: false,
                pending_action: None,
                current_request_id: 0,
                pending_serialization: Task::ready(None),
                polling_task: Task::ready(()),
            }
        })
    }

    fn serialization_key(workspace: &Workspace) -> Option<String> {
        workspace
            .database_id()
            .map(|id| i64::from(id).to_string())
            .or(workspace.session_id())
            .map(|id| format!("{SING_SPEC_BOARD_PANEL_KEY}-{id:?}"))
    }

    fn serialize(&mut self, cx: &mut Context<Self>) {
        let Some(serialization_key) = self.serialization_key.clone() else {
            return;
        };

        let serialized = SerializedSingSpecBoardPanel {
            active: self.active.then_some(true),
            selected_project: self.selected_project.clone(),
            selected_spec: self.selected_spec.clone(),
            position: Some(SerializedDockPosition::from_dock_position(self.position)),
        };

        let kvp = KeyValueStore::global(cx);
        self.pending_serialization = cx.background_spawn(
            async move {
                kvp.write_kvp(serialization_key, serde_json::to_string(&serialized)?)
                    .await?;
                anyhow::Ok(())
            }
            .log_err(),
        );
    }

    fn start_polling(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.polling_task = cx.spawn_in(window, async move |panel, cx| {
            loop {
                cx.background_executor().timer(REFRESH_INTERVAL).await;

                let should_refresh = panel
                    .read_with(cx, |panel, _| panel.active)
                    .ok()
                    .unwrap_or(false);

                if !should_refresh {
                    continue;
                }

                panel
                    .update_in(cx, |panel, window, cx| panel.refresh(window, cx))
                    .ok();
            }
        });
    }

    fn ensure_client(&mut self) -> Result<Arc<dyn SingSpecClient>> {
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }

        let client = self.client_factory.create()?;
        self.client = Some(client.clone());
        Ok(client)
    }

    fn store(&mut self) -> Result<RemoteSpecStore> {
        let client = self.ensure_client()?;
        Ok(RemoteSpecStore::new(
            client,
            Arc::new(SshSpecFileSystem::default()),
        ))
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.loading = true;
        self.last_error = None;
        self.current_request_id += 1;
        let request_id = self.current_request_id;
        let preferred_project = self.selected_project.clone();
        let preferred_spec = self.selected_spec.clone();
        cx.notify();

        cx.spawn_in(window, async move |panel, cx| {
            let result = async {
                let client = panel.update_in(cx, |panel, _, _| panel.ensure_client())??;
                let projects = running_projects(client.list_projects().await?);
                let selected_project =
                    next_project_selection(preferred_project.as_deref(), &projects);
                let board = if let Some(project) = &selected_project {
                    Some(
                        RemoteSpecStore::new(
                            client.clone(),
                            Arc::new(SshSpecFileSystem::default()),
                        )
                        .load_board(project)
                        .await?,
                    )
                } else {
                    None
                };
                let selected_spec = next_spec_selection(preferred_spec.as_deref(), board.as_ref());

                Ok(LoadedBoardState {
                    projects,
                    selected_project,
                    board,
                    selected_spec,
                })
            }
            .await;

            panel
                .update_in(cx, |panel, _, cx| {
                    panel.finish_refresh(request_id, result, cx);
                })
                .ok();
        })
        .detach();
    }

    fn finish_refresh(
        &mut self,
        request_id: usize,
        result: Result<LoadedBoardState>,
        cx: &mut Context<Self>,
    ) {
        if request_id != self.current_request_id {
            return;
        }

        self.loading = false;

        match result {
            Ok(state) => {
                self.last_refreshed_at = Some(Instant::now());
                self.projects = state.projects;
                self.selected_project = state.selected_project;
                self.board = state.board;
                self.selected_spec = state.selected_spec;
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
                if self.board.is_none() {
                    self.selected_spec = None;
                }
            }
        }

        self.serialize(cx);
        cx.notify();
    }

    fn select_project(&mut self, project: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_project.as_deref() == Some(project) {
            return;
        }

        self.selected_project = Some(project.to_string());
        self.selected_spec = None;
        self.serialize(cx);
        self.refresh(window, cx);
    }

    fn open_spec(
        &mut self,
        project: String,
        spec_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending_action.is_some() {
            return;
        }

        self.selected_spec = Some(spec_id.clone());
        self.pending_action = Some(PendingSpecAction::Open(spec_id.clone()));
        self.serialize(cx);
        cx.notify();

        let workspace = self.workspace.clone();
        cx.spawn_in(window, async move |panel, cx| {
            let result = async {
                let store = panel.update_in(cx, |panel, _, _| panel.store())??;
                let target = store.open_target(&project, &spec_id).await?;
                let (app_state, open_options) =
                    workspace.update_in(cx, |workspace, window, _| {
                        let requesting_window = window.window_handle().downcast::<MultiWorkspace>();
                        let open_options = OpenOptions {
                            requesting_window,
                            ..Default::default()
                        };
                        (workspace.app_state().clone(), open_options)
                    })?;

                open_remote_project(
                    target.connection_options,
                    vec![target.spec_path],
                    app_state,
                    open_options,
                    cx,
                )
                .await?;
                Ok(())
            }
            .await;

            panel
                .update_in(cx, |panel, _, cx| {
                    panel.finish_open_spec(result, cx);
                })
                .ok();
        })
        .detach();
    }

    fn finish_open_spec(&mut self, result: Result<()>, cx: &mut Context<Self>) {
        self.pending_action = None;
        match result {
            Ok(()) => {
                self.last_error = None;
                cx.notify();
            }
            Err(error) => self.show_action_error(error.to_string(), cx),
        }
    }

    fn toggle_new_spec_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_new_spec_form = !self.show_new_spec_form;
        if !self.show_new_spec_form {
            self.new_spec_title
                .update(cx, |editor, cx| editor.clear(window, cx));
        } else {
            window.focus(&self.new_spec_title.focus_handle(cx), cx);
        }
        cx.notify();
    }

    fn create_spec(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = self.selected_project.clone() else {
            return;
        };
        if self.pending_action.is_some() {
            return;
        }

        let title = self.new_spec_title.read(cx).text(cx).trim().to_string();
        if title.is_empty() {
            self.show_action_error("Spec title cannot be blank".to_string(), cx);
            return;
        }

        self.pending_action = Some(PendingSpecAction::Create);
        cx.notify();

        cx.spawn_in(window, async move |panel, cx| {
            let result = async {
                let store = panel.update_in(cx, |panel, _, _| panel.store())??;
                store
                    .create_spec(
                        &project,
                        CreateSpecRequest {
                            title,
                            ..Default::default()
                        },
                    )
                    .await
            }
            .await;

            panel
                .update_in(cx, |panel, window, cx| {
                    panel.finish_create_spec(project, result, window, cx);
                })
                .ok();
        })
        .detach();
    }

    fn finish_create_spec(
        &mut self,
        project: String,
        result: Result<crate::SpecMutationResult>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending_action = None;

        match result {
            Ok(result) => {
                let spec_id = result.spec.spec.id.clone();
                self.last_error = None;
                self.selected_project = Some(project.clone());
                self.board = Some(result.board);
                self.selected_spec = Some(spec_id.clone());
                self.show_new_spec_form = false;
                self.last_refreshed_at = Some(Instant::now());
                self.new_spec_title
                    .update(cx, |editor, cx| editor.clear(window, cx));
                self.serialize(cx);
                self.show_toast(format!("Created spec {spec_id}"), cx);
                cx.notify();
            }
            Err(error) => self.show_action_error(error.to_string(), cx),
        }
    }

    fn dispatch_next_ready(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dispatch_spec(None, window, cx);
    }

    fn dispatch_selected_spec(
        &mut self,
        spec_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dispatch_spec(Some(spec_id), window, cx);
    }

    fn dispatch_spec(
        &mut self,
        spec_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.selected_project.clone() else {
            return;
        };
        if self.pending_action.is_some() {
            return;
        }

        self.pending_action = Some(PendingSpecAction::Dispatch(spec_id.clone()));
        cx.notify();

        cx.spawn_in(window, async move |panel, cx| {
            let result = async {
                let store = panel.update_in(cx, |panel, _, _| panel.store())??;
                store.dispatch_spec(&project, spec_id).await
            }
            .await;

            panel
                .update_in(cx, |panel, _, cx| {
                    panel.finish_dispatch_spec(project, result, cx);
                })
                .ok();
        })
        .detach();
    }

    fn finish_dispatch_spec(
        &mut self,
        project: String,
        result: Result<crate::SpecDispatchResult>,
        cx: &mut Context<Self>,
    ) {
        self.pending_action = None;

        match result {
            Ok(result) => {
                let selected_spec = result.selected_spec.map(|spec| spec.spec.id);
                let message = if let Some(spec_id) = selected_spec.as_ref() {
                    format!("Dispatched spec {spec_id}")
                } else if let Some(reason) = result.dispatch.reason.as_ref() {
                    format!("No spec dispatched: {reason}")
                } else {
                    "No spec dispatched".to_string()
                };
                self.last_error = None;
                self.selected_project = Some(project);
                self.board = Some(result.board);
                self.selected_spec = selected_spec;
                self.last_refreshed_at = Some(Instant::now());
                self.serialize(cx);
                self.show_toast(message, cx);
                cx.notify();
            }
            Err(error) => self.show_action_error(error.to_string(), cx),
        }
    }

    fn show_agent_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.run_agent_action(PendingSpecAction::AgentStatus, window, cx);
    }

    fn show_agent_log(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.run_agent_action(PendingSpecAction::AgentLog, window, cx);
    }

    fn stop_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.run_agent_action(PendingSpecAction::StopAgent, window, cx);
    }

    fn show_agent_report(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.run_agent_action(PendingSpecAction::AgentReport, window, cx);
    }

    fn run_agent_action(
        &mut self,
        action: PendingSpecAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.selected_project.clone() else {
            return;
        };
        if self.pending_action.is_some() {
            return;
        }

        self.pending_action = Some(action.clone());
        cx.notify();

        cx.spawn_in(window, async move |panel, cx| {
            let result = async {
                let store = panel.update_in(cx, |panel, _, _| panel.store())??;
                match action {
                    PendingSpecAction::AgentStatus => {
                        let status = store.agent_status(&project).await?;
                        Ok(format_agent_status(&status))
                    }
                    PendingSpecAction::AgentLog => {
                        let log = store.agent_log(&project, 200).await?;
                        Ok(format_agent_log(&log))
                    }
                    PendingSpecAction::StopAgent => {
                        let stopped = store.stop_agent(&project).await?;
                        Ok(format_stop_agent(&stopped))
                    }
                    PendingSpecAction::AgentReport => {
                        let report = store.agent_report(&project).await?;
                        Ok(format_agent_report(&report))
                    }
                    _ => Err(anyhow!("agent action runner received a non-agent action")),
                }
            }
            .await;

            panel
                .update_in(cx, |panel, _, cx| panel.finish_agent_action(result, cx))
                .ok();
        })
        .detach();
    }

    fn finish_agent_action(&mut self, result: Result<String>, cx: &mut Context<Self>) {
        self.pending_action = None;

        match result {
            Ok(message) => {
                self.last_error = None;
                self.show_toast(message, cx);
                cx.notify();
            }
            Err(error) => self.show_action_error(error.to_string(), cx),
        }
    }

    fn move_selected_spec(
        &mut self,
        target_status: SpecStatus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.selected_project.clone() else {
            return;
        };
        let Some(spec) = self.selected_spec_entry() else {
            return;
        };
        if self.pending_action.is_some() {
            return;
        }

        if let Err(message) = can_move_to(spec, target_status) {
            self.show_action_error(message.to_string(), cx);
            return;
        }

        let spec_id = spec.spec.id.clone();
        self.pending_action = Some(PendingSpecAction::Move(spec_id.clone(), target_status));
        cx.notify();

        cx.spawn_in(window, async move |panel, cx| {
            let result = async {
                let store = panel.update_in(cx, |panel, _, _| panel.store())??;
                store.update_status(&project, &spec_id, target_status).await
            }
            .await;

            panel
                .update_in(cx, |panel, _, cx| {
                    panel.finish_move_spec(result, cx);
                })
                .ok();
        })
        .detach();
    }

    fn finish_move_spec(
        &mut self,
        result: Result<crate::SpecMutationResult>,
        cx: &mut Context<Self>,
    ) {
        self.pending_action = None;

        match result {
            Ok(result) => {
                let spec_id = result.spec.spec.id.clone();
                let status = result.spec.spec.status;
                self.last_error = None;
                self.board = Some(result.board);
                self.selected_spec = Some(spec_id.clone());
                self.last_refreshed_at = Some(Instant::now());
                self.serialize(cx);
                self.show_toast(
                    format!("Moved {spec_id} to {}", spec_status_label(status)),
                    cx,
                );
                cx.notify();
            }
            Err(error) => self.show_action_error(error.to_string(), cx),
        }
    }

    fn selected_spec_entry(&self) -> Option<&SpecEntry> {
        let selected_spec = self.selected_spec.as_deref()?;
        self.board
            .as_ref()?
            .specs
            .iter()
            .find(|spec| spec.spec.id == selected_spec)
    }

    fn is_opening(&self, spec_id: &str) -> bool {
        matches!(
            self.pending_action,
            Some(PendingSpecAction::Open(ref pending_spec)) if pending_spec == spec_id
        )
    }

    fn is_moving(&self, spec_id: &str, status: SpecStatus) -> bool {
        matches!(
            self.pending_action,
            Some(PendingSpecAction::Move(ref pending_spec, pending_status))
                if pending_spec == spec_id && pending_status == status
        )
    }

    fn is_dispatching(&self, spec_id: Option<&str>) -> bool {
        match (&self.pending_action, spec_id) {
            (Some(PendingSpecAction::Dispatch(Some(pending_spec))), Some(spec_id)) => {
                pending_spec == spec_id
            }
            (Some(PendingSpecAction::Dispatch(None)), None) => true,
            _ => false,
        }
    }

    fn is_creating(&self) -> bool {
        self.pending_action == Some(PendingSpecAction::Create)
    }

    fn refresh_status_label(&self) -> Option<String> {
        let refreshed_at = self.last_refreshed_at?;
        let elapsed = refreshed_at.elapsed().as_secs();

        Some(if elapsed < 5 {
            "Updated just now".to_string()
        } else if elapsed < 60 {
            format!("Updated {elapsed}s ago")
        } else if elapsed < 3600 {
            format!("Updated {}m ago", elapsed / 60)
        } else {
            format!("Updated {}h ago", elapsed / 3600)
        })
    }

    fn show_action_error(&mut self, error: String, cx: &mut Context<Self>) {
        self.last_error = Some(error.clone());
        if let Some(workspace) = self.workspace.upgrade() {
            workspace.update(cx, |workspace, cx| {
                workspace.show_error(&anyhow!(error.clone()), cx);
            });
        }
        cx.notify();
    }

    fn show_toast(&mut self, message: String, cx: &mut Context<Self>) {
        if let Some(workspace) = self.workspace.upgrade() {
            workspace.update(cx, |workspace, cx| {
                workspace.show_toast(
                    Toast::new(
                        NotificationId::composite::<SingSpecBoardPanel>("sing-spec-board-panel"),
                        message.clone(),
                    ),
                    cx,
                );
            });
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();

        v_flex()
            .w_full()
            .gap_1p5()
            .p_2()
            .border_b_1()
            .border_color(theme.colors().border_variant)
            .bg(theme.colors().editor_background)
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(Label::new("Specs"))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("sing-spec-dispatch-next", "Dispatch next")
                                    .style(ButtonStyle::Filled)
                                    .label_size(LabelSize::Small)
                                    .loading(self.is_dispatching(None))
                                    .disabled(
                                        self.selected_project.is_none()
                                            || self.pending_action.is_some()
                                                && !self.is_dispatching(None)
                                            || self.board.as_ref().map_or(true, |board| {
                                                board.summary.ready_count == 0
                                            }),
                                    )
                                    .tooltip(Tooltip::text("Dispatch the next ready spec"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.dispatch_next_ready(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("sing-spec-agent-status", "Agent status")
                                    .style(ButtonStyle::Outlined)
                                    .label_size(LabelSize::Small)
                                    .loading(matches!(
                                        self.pending_action,
                                        Some(PendingSpecAction::AgentStatus)
                                    ))
                                    .disabled(
                                        self.selected_project.is_none()
                                            || self.pending_action.is_some()
                                                && !matches!(
                                                    self.pending_action,
                                                    Some(PendingSpecAction::AgentStatus)
                                                ),
                                    )
                                    .tooltip(Tooltip::text("Show active agent status"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.show_agent_status(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("sing-spec-agent-log", "Logs")
                                    .style(ButtonStyle::Outlined)
                                    .label_size(LabelSize::Small)
                                    .loading(matches!(
                                        self.pending_action,
                                        Some(PendingSpecAction::AgentLog)
                                    ))
                                    .disabled(
                                        self.selected_project.is_none()
                                            || self.pending_action.is_some()
                                                && !matches!(
                                                    self.pending_action,
                                                    Some(PendingSpecAction::AgentLog)
                                                ),
                                    )
                                    .tooltip(Tooltip::text("Show recent agent log lines"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.show_agent_log(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("sing-spec-agent-report", "Report")
                                    .style(ButtonStyle::Outlined)
                                    .label_size(LabelSize::Small)
                                    .loading(matches!(
                                        self.pending_action,
                                        Some(PendingSpecAction::AgentReport)
                                    ))
                                    .disabled(
                                        self.selected_project.is_none()
                                            || self.pending_action.is_some()
                                                && !matches!(
                                                    self.pending_action,
                                                    Some(PendingSpecAction::AgentReport)
                                                ),
                                    )
                                    .tooltip(Tooltip::text("Show current agent report"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.show_agent_report(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("sing-spec-agent-stop", "Stop agent")
                                    .style(ButtonStyle::Tinted(TintColor::Warning))
                                    .label_size(LabelSize::Small)
                                    .loading(matches!(
                                        self.pending_action,
                                        Some(PendingSpecAction::StopAgent)
                                    ))
                                    .disabled(
                                        self.selected_project.is_none()
                                            || self.pending_action.is_some()
                                                && !matches!(
                                                    self.pending_action,
                                                    Some(PendingSpecAction::StopAgent)
                                                ),
                                    )
                                    .tooltip(Tooltip::text("Stop the active agent"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.stop_agent(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("sing-spec-new", "New spec")
                                    .style(ButtonStyle::Filled)
                                    .label_size(LabelSize::Small)
                                    .disabled(self.selected_project.is_none() || self.is_creating())
                                    .tooltip(Tooltip::text(
                                        "Create a new spec in the selected project",
                                    ))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.toggle_new_spec_form(window, cx);
                                    })),
                            )
                            .child(
                                IconButton::new("sing-spec-refresh", IconName::RotateCw)
                                    .shape(IconButtonShape::Square)
                                    .icon_size(IconSize::Small)
                                    .disabled(self.loading)
                                    .style(ButtonStyle::Subtle)
                                    .tooltip(Tooltip::text("Refresh spec board"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.refresh(window, cx);
                                    })),
                            ),
                    ),
            )
            .when(!self.projects.is_empty(), |element| {
                element.child(self.render_project_picker(cx))
            })
            .when(
                self.show_new_spec_form && self.selected_project.is_some(),
                |element| element.child(self.render_new_spec_form(cx)),
            )
            .child(self.render_summary(cx))
            .into_any_element()
    }

    fn render_project_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let buttons = self.projects.iter().map(|project| {
            let project_name = project.name.clone();
            Button::new(
                format!("sing-spec-project-{}", project.name),
                project.name.clone(),
            )
            .style(
                if self.selected_project.as_deref() == Some(project.name.as_str()) {
                    ButtonStyle::Filled
                } else {
                    ButtonStyle::Subtle
                },
            )
            .label_size(LabelSize::Small)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_project(&project_name, window, cx);
            }))
            .into_any_element()
        });

        h_flex()
            .w_full()
            .gap_1()
            .flex_wrap()
            .children(buttons)
            .into_any_element()
    }

    fn render_new_spec_form(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();

        h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .child(
                h_flex()
                    .flex_1()
                    .py_1()
                    .px_1p5()
                    .gap_1p5()
                    .rounded_sm()
                    .bg(theme.colors().panel_background)
                    .border_1()
                    .border_color(theme.colors().border_variant)
                    .child(Icon::new(IconName::Pencil).color(Color::Muted))
                    .child(self.new_spec_title.clone()),
            )
            .child(
                Button::new("sing-spec-create", "Create")
                    .style(ButtonStyle::Filled)
                    .label_size(LabelSize::Small)
                    .loading(self.is_creating())
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.create_spec(window, cx);
                    })),
            )
            .child(
                Button::new("sing-spec-cancel-create", "Cancel")
                    .style(ButtonStyle::Outlined)
                    .label_size(LabelSize::Small)
                    .disabled(self.is_creating())
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_new_spec_form(window, cx);
                    })),
            )
            .into_any_element()
    }

    fn render_summary(&self, _cx: &mut Context<Self>) -> AnyElement {
        let summary = self.board.as_ref().map(|board| &board.summary);

        h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .gap_2()
            .child(h_flex().gap_1().flex_wrap().children(vec![
                        summary_badge(
                            format!(
                                "Ready {}",
                                summary.map_or(0, |summary| summary.ready_count)
                            ),
                            Color::Success,
                        )
                        .into_any_element(),
                        summary_badge(
                            format!(
                                "Blocked {}",
                                summary.map_or(0, |summary| summary.blocked_count)
                            ),
                            Color::Warning,
                        )
                        .into_any_element(),
                        summary_badge(
                            format!(
                                "Review {}",
                                summary.map_or(0, |summary| summary.counts.review)
                            ),
                            Color::Accent,
                        )
                        .into_any_element(),
                    ]))
            .child(
                Label::new(if self.loading {
                    "Refreshing board"
                } else if self.projects.is_empty() {
                    "Start a project to browse specs"
                } else {
                    "Kanban view for the selected project"
                })
                .size(LabelSize::Small)
                .color(Color::Muted),
            )
            .when_some(self.refresh_status_label(), |element, refreshed| {
                element.child(
                    Label::new(refreshed)
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
            })
            .into_any_element()
    }

    fn render_error_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme();
        self.last_error.as_ref().map(|error| {
            h_flex()
                .w_full()
                .items_center()
                .gap_2()
                .p_2()
                .border_b_1()
                .border_color(theme.colors().border_variant)
                .bg(theme.colors().editor_background)
                .child(
                    Icon::new(IconName::Warning)
                        .size(IconSize::Small)
                        .color(Color::Warning),
                )
                .child(
                    Label::new(error.clone())
                        .size(LabelSize::Small)
                        .color(Color::Warning)
                        .truncate(),
                )
                .into_any_element()
        })
    }

    fn render_board(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.loading && self.board.is_none() {
            return v_flex()
                .size_full()
                .justify_center()
                .items_center()
                .gap_2()
                .child(Icon::new(IconName::RotateCw).color(Color::Muted))
                .child(
                    Label::new("Loading spec board")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .into_any_element();
        }

        if self.projects.is_empty() {
            return v_flex()
                .size_full()
                .justify_center()
                .items_center()
                .gap_2()
                .child(Icon::new(IconName::ListTodo).color(Color::Muted))
                .child(
                    Label::new("No running projects with accessible specs")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .into_any_element();
        }

        let Some(board) = self.board.as_ref() else {
            return v_flex()
                .size_full()
                .justify_center()
                .items_center()
                .gap_2()
                .child(Icon::new(IconName::ListTodo).color(Color::Muted))
                .child(
                    Label::new("Select a project to load its board")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .into_any_element();
        };

        div()
            .id("sing-spec-board-columns")
            .flex_1()
            .child(h_flex().w_full().gap_2().flex_wrap().p_2().children([
                self.render_column(board, SpecStatus::Pending, cx),
                self.render_column(board, SpecStatus::InProgress, cx),
                self.render_column(board, SpecStatus::Review, cx),
                self.render_column(board, SpecStatus::Done, cx),
            ]))
            .into_any_element()
    }

    fn render_column(
        &self,
        board: &SpecBoardState,
        status: SpecStatus,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let specs = specs_for_status(board, status);
        let selected_spec = self.selected_spec.as_deref();
        let project_name = board.project.clone();
        let selected_bg = theme.colors().element_selected.opacity(0.4);
        let hover_bg = theme.colors().element_hover;
        let border = theme.colors().border_variant;
        let focused_border = theme.colors().border_focused;

        let cards = specs.into_iter().map(|spec| {
            let card_project = project_name.clone();
            let spec_id = spec.spec.id.clone();
            let opening = self.is_opening(&spec_id);
            let selected = selected_spec == Some(spec_id.as_str());
            let title = if spec.spec.title.is_empty() {
                spec.spec.id.clone()
            } else {
                spec.spec.title.clone()
            };

            v_flex()
                .id(format!("sing-spec-card-{spec_id}"))
                .w_full()
                .gap_1p5()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(if selected { focused_border } else { border })
                .bg(if selected {
                    selected_bg
                } else {
                    theme.colors().editor_background
                })
                .cursor_pointer()
                .hover(
                    move |style| {
                        if !selected { style.bg(hover_bg) } else { style }
                    },
                )
                .tooltip(Tooltip::text("Open spec.md"))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_spec(card_project.clone(), spec_id.clone(), window, cx);
                }))
                .child(
                    h_flex()
                        .w_full()
                        .justify_between()
                        .gap_2()
                        .child(
                            h_flex()
                                .gap_1()
                                .items_center()
                                .child(Indicator::dot().color(spec_status_color(status)))
                                .child(
                                    Label::new(spec.spec.id.clone())
                                        .size(LabelSize::XSmall)
                                        .color(Color::Muted)
                                        .truncate(),
                                ),
                        )
                        .when(opening, |element| {
                            element.child(
                                Chip::new("Opening")
                                    .label_size(LabelSize::XSmall)
                                    .label_color(Color::Accent),
                            )
                        }),
                )
                .child(Label::new(title).size(LabelSize::Small))
                .child(h_flex().gap_1().flex_wrap().children(card_badges(spec)))
                .when(spec.blocked, |element| {
                    element.child(
                        Label::new(format!("Waiting on {}", spec.unmet_dependencies.join(", ")))
                            .size(LabelSize::Small)
                            .color(Color::Warning)
                            .truncate(),
                    )
                })
                .into_any_element()
        });

        v_flex()
            .w(px(280.))
            .min_w(px(280.))
            .h_full()
            .gap_2()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(theme.colors().border_variant)
            .bg(theme.colors().panel_background)
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .gap_2()
                    .child(Label::new(spec_status_label(status)))
                    .child(summary_badge(
                        specs_for_status(board, status).len().to_string(),
                        spec_status_color(status),
                    )),
            )
            .when(specs_for_status(board, status).is_empty(), |element| {
                element.child(
                    v_flex()
                        .w_full()
                        .flex_1()
                        .justify_center()
                        .items_center()
                        .child(
                            Label::new(format!(
                                "No {}",
                                spec_status_label(status).to_ascii_lowercase()
                            ))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                        ),
                )
            })
            .children(cards)
            .into_any_element()
    }

    fn render_selected_spec(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let spec = self.selected_spec_entry()?;
        let theme = cx.theme();
        let project = self.selected_project.clone()?;
        let title = if spec.spec.title.is_empty() {
            spec.spec.id.clone()
        } else {
            spec.spec.title.clone()
        };
        let spec_id = spec.spec.id.clone();

        Some(
            v_flex()
                .w_full()
                .gap_2()
                .p_2()
                .border_t_1()
                .border_color(theme.colors().border_variant)
                .bg(theme.colors().editor_background)
                .child(
                    h_flex()
                        .w_full()
                        .justify_between()
                        .gap_2()
                        .child(
                            v_flex().gap_0p5().child(Label::new(title)).child(
                                Label::new(spec.spec.id.clone())
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            ),
                        )
                        .child(summary_badge(
                            spec_status_label(spec.spec.status),
                            spec_status_color(spec.spec.status),
                        )),
                )
                .child(
                    h_flex()
                        .w_full()
                        .gap_1()
                        .flex_wrap()
                        .children(card_badges(spec)),
                )
                .when_some(spec.spec.assignee.as_ref(), |element, assignee| {
                    element.child(
                        Label::new(format!("Assignee {assignee}"))
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .truncate(),
                    )
                })
                .when_some(spec.spec.branch.as_ref(), |element, branch| {
                    element.child(
                        Label::new(format!("Branch {branch}"))
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .truncate(),
                    )
                })
                .when(!spec.spec.depends_on.is_empty(), |element| {
                    element.child(
                        Label::new(format!("Depends on {}", spec.spec.depends_on.join(", ")))
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .truncate(),
                    )
                })
                .when(spec.blocked, |element| {
                    element.child(
                        Label::new(format!("Blocked by {}", spec.unmet_dependencies.join(", ")))
                            .size(LabelSize::Small)
                            .color(Color::Warning)
                            .truncate(),
                    )
                })
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .flex_wrap()
                        .child(
                            Button::new("sing-spec-open-selected", "Open spec")
                                .style(ButtonStyle::Filled)
                                .label_size(LabelSize::Small)
                                .loading(self.is_opening(&spec_id))
                                .disabled(
                                    self.pending_action.is_some() && !self.is_opening(&spec_id),
                                )
                                .on_click({
                                    let project = project.clone();
                                    let spec_id = spec_id.clone();
                                    cx.listener(move |this, _, window, cx| {
                                        this.open_spec(
                                            project.clone(),
                                            spec_id.clone(),
                                            window,
                                            cx,
                                        );
                                    })
                                }),
                        )
                        .child({
                            let spec_id = spec_id.clone();
                            let blocked = spec.blocked;
                            Button::new("sing-spec-dispatch-selected", "Dispatch")
                                .style(ButtonStyle::Filled)
                                .label_size(LabelSize::Small)
                                .loading(self.is_dispatching(Some(&spec_id)))
                                .disabled(
                                    blocked
                                        || self.pending_action.is_some()
                                            && !self.is_dispatching(Some(&spec_id)),
                                )
                                .tooltip(Tooltip::text(if blocked {
                                    "Spec is blocked by unmet dependencies"
                                } else {
                                    "Dispatch this spec to the configured agent"
                                }))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.dispatch_selected_spec(spec_id.clone(), window, cx);
                                }))
                        })
                        .children(self.render_move_buttons(spec, cx)),
                )
                .into_any_element(),
        )
    }

    fn render_move_buttons(&self, spec: &SpecEntry, cx: &mut Context<Self>) -> Vec<AnyElement> {
        [
            SpecStatus::Pending,
            SpecStatus::InProgress,
            SpecStatus::Review,
            SpecStatus::Done,
        ]
        .into_iter()
        .filter(|status| *status != spec.spec.status)
        .map(|status| {
            let spec_id = spec.spec.id.clone();
            let validation = can_move_to(spec, status).err();
            Button::new(
                format!("sing-spec-move-{spec_id}-{}", status.as_cli_arg()),
                format!("Move to {}", spec_status_label(status)),
            )
            .style(spec_status_button_style(status))
            .label_size(LabelSize::Small)
            .loading(self.is_moving(&spec_id, status))
            .disabled(
                validation.is_some()
                    || self.pending_action.is_some() && !self.is_moving(&spec_id, status),
            )
            .tooltip(Tooltip::text(
                validation.unwrap_or_else(|| "Update spec status"),
            ))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.move_selected_spec(status, window, cx);
            }))
            .into_any_element()
        })
        .collect()
    }
}

impl EventEmitter<PanelEvent> for SingSpecBoardPanel {}

impl Focusable for SingSpecBoardPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SingSpecBoardPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        v_flex()
            .id("sing-spec-board-panel")
            .track_focus(&self.focus_handle)
            .overflow_hidden()
            .size_full()
            .bg(theme.colors().panel_background)
            .child(self.render_header(cx))
            .when_some(self.render_error_banner(cx), |element, banner| {
                element.child(banner)
            })
            .child(self.render_board(cx))
            .when_some(self.render_selected_spec(cx), |element, details| {
                element.child(details)
            })
    }
}

impl Panel for SingSpecBoardPanel {
    fn persistent_name() -> &'static str {
        "Specs"
    }

    fn panel_key() -> &'static str {
        SING_SPEC_BOARD_PANEL_KEY
    }

    fn position(&self, _: &Window, _: &App) -> DockPosition {
        self.position
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Bottom | DockPosition::Right)
    }

    fn set_position(&mut self, position: DockPosition, _: &mut Window, cx: &mut Context<Self>) {
        self.position = position;
        self.serialize(cx);
        cx.notify();
    }

    fn default_size(&self, _: &Window, _: &App) -> Pixels {
        px(420.)
    }

    fn min_size(&self, _: &Window, _: &App) -> Option<Pixels> {
        Some(px(320.))
    }

    fn icon(&self, _: &Window, _: &App) -> Option<IconName> {
        Some(IconName::ListTodo)
    }

    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("Specs")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(ToggleFocus)
    }

    fn icon_label(&self, _: &Window, _: &App) -> Option<String> {
        self.board.as_ref().and_then(|board| {
            let ready = board.summary.ready_count;
            (ready > 0).then(|| ready.to_string())
        })
    }

    fn starts_open(&self, _: &Window, _: &App) -> bool {
        self.active
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.active == active {
            return;
        }

        self.active = active;
        self.serialize(cx);
        if active {
            self.refresh(window, cx);
        } else {
            cx.notify();
        }
    }

    fn activation_priority(&self) -> u32 {
        3
    }
}

fn running_projects(mut projects: Vec<ProjectSummary>) -> Vec<ProjectSummary> {
    projects.retain(|project| project.status == ProjectStatus::Running);
    projects.sort_by(|left, right| left.name.cmp(&right.name));
    projects
}

fn next_project_selection(previous: Option<&str>, projects: &[ProjectSummary]) -> Option<String> {
    previous
        .and_then(|selected| {
            projects
                .iter()
                .find(|project| project.name == selected)
                .map(|project| project.name.clone())
        })
        .or_else(|| projects.first().map(|project| project.name.clone()))
}

fn next_spec_selection(previous: Option<&str>, board: Option<&SpecBoardState>) -> Option<String> {
    let board = board?;
    previous
        .and_then(|selected| {
            board
                .specs
                .iter()
                .find(|spec| spec.spec.id == selected)
                .map(|spec| spec.spec.id.clone())
        })
        .or_else(|| board.specs.first().map(|spec| spec.spec.id.clone()))
}

fn specs_for_status(board: &SpecBoardState, status: SpecStatus) -> Vec<&SpecEntry> {
    board
        .specs
        .iter()
        .filter(|spec| spec.spec.status == status)
        .collect()
}

fn card_badges(spec: &SpecEntry) -> Vec<AnyElement> {
    let mut badges = Vec::new();

    if spec.ready {
        badges.push(summary_badge("Ready", Color::Success).into_any_element());
    }
    if spec.blocked {
        badges.push(summary_badge("Blocked", Color::Warning).into_any_element());
    }
    if let Some(assignee) = spec.spec.assignee.as_ref() {
        badges.push(
            Chip::new(assignee.clone())
                .label_size(LabelSize::XSmall)
                .label_color(Color::Muted)
                .into_any_element(),
        );
    }
    if let Some(branch) = spec.spec.branch.as_ref() {
        badges.push(
            Chip::new(branch.clone())
                .label_size(LabelSize::XSmall)
                .label_color(Color::Accent)
                .into_any_element(),
        );
    }
    if !spec.spec.depends_on.is_empty() {
        badges.push(
            Chip::new(format!("Deps {}", spec.spec.depends_on.len()))
                .label_size(LabelSize::XSmall)
                .label_color(Color::Muted)
                .into_any_element(),
        );
    }

    badges
}

fn summary_badge(label: impl Into<String>, color: Color) -> Chip {
    Chip::new(label.into())
        .label_size(LabelSize::XSmall)
        .label_color(color)
}

fn format_agent_status(status: &ProjectAgentStatus) -> String {
    let state = if status.agent_running {
        "Agent running"
    } else {
        "Agent idle"
    };
    let mut parts = vec![state.to_string()];
    if let Some(task) = status.task.as_deref() {
        parts.push(format!("task {task}"));
    }
    if let Some(branch) = status.branch.as_deref() {
        parts.push(format!("branch {branch}"));
    }
    if let Some(pid) = status.pid {
        parts.push(format!("pid {pid}"));
    }
    parts.join(" | ")
}

fn format_agent_log(log: &AgentLog) -> String {
    if let Some(error) = log.error.as_deref() {
        return format!("Agent log unavailable: {error}");
    }
    if log.lines.is_empty() {
        return "Agent log is empty".to_string();
    }
    let mut lines = log.lines.iter().rev().take(3).cloned().collect::<Vec<_>>();
    lines.reverse();
    format!("Agent log | {}", lines.join(" | "))
}

fn format_stop_agent(result: &StopAgentResult) -> String {
    if result.stopped {
        if let Some(pid) = result.pid {
            return format!("Stopped agent pid {pid}");
        }
        return "Stopped agent".to_string();
    }
    result
        .reason
        .as_ref()
        .map(|reason| format!("No agent stopped: {reason}"))
        .unwrap_or_else(|| "No agent stopped".to_string())
}

fn format_agent_report(report: &AgentReport) -> String {
    let mut parts = vec![format!("Agent {}", report.session_status)];
    if let Some(duration) = report.duration.as_deref() {
        parts.push(format!("duration {duration}"));
    }
    if let Some(branch) = report.branch.as_deref() {
        parts.push(format!("branch {branch}"));
    }
    parts.push(format!("commits {}", report.commits_since_launch));
    if report.guardrail_triggered {
        parts.push("guardrail triggered".to_string());
    }
    parts.join(" | ")
}

fn spec_status_label(status: SpecStatus) -> &'static str {
    match status {
        SpecStatus::Pending => "Pending",
        SpecStatus::InProgress => "In progress",
        SpecStatus::Review => "Review",
        SpecStatus::Done => "Done",
    }
}

fn spec_status_color(status: SpecStatus) -> Color {
    match status {
        SpecStatus::Pending => Color::Muted,
        SpecStatus::InProgress => Color::Accent,
        SpecStatus::Review => Color::Warning,
        SpecStatus::Done => Color::Success,
    }
}

fn spec_status_button_style(status: SpecStatus) -> ButtonStyle {
    match status {
        SpecStatus::Pending => ButtonStyle::Outlined,
        SpecStatus::InProgress => ButtonStyle::Tinted(TintColor::Accent),
        SpecStatus::Review => ButtonStyle::Tinted(TintColor::Warning),
        SpecStatus::Done => ButtonStyle::Tinted(TintColor::Success),
    }
}

fn can_move_to(
    spec: &SpecEntry,
    target_status: SpecStatus,
) -> std::result::Result<(), &'static str> {
    if spec.spec.status == target_status {
        return Err("Spec is already in that column");
    }
    if target_status == SpecStatus::InProgress && spec.blocked {
        return Err("Resolve dependencies before moving this spec to in progress");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use sing_bridge::{BoardSpecRecord, ProjectSummary, SpecBoardSummary, SpecCounts, SpecRecord};

    use super::{
        can_move_to, next_project_selection, next_spec_selection, running_projects,
        specs_for_status,
    };
    use crate::SpecBoardState;
    use sing_bridge::{ProjectStatus, SpecStatus};

    #[test]
    fn running_projects_filters_and_sorts() {
        let projects = running_projects(vec![
            ProjectSummary {
                name: "beta".to_string(),
                status: ProjectStatus::Stopped,
                ip: None,
            },
            ProjectSummary {
                name: "gamma".to_string(),
                status: ProjectStatus::Running,
                ip: None,
            },
            ProjectSummary {
                name: "alpha".to_string(),
                status: ProjectStatus::Running,
                ip: None,
            },
        ]);

        assert_eq!(
            projects
                .iter()
                .map(|project| project.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "gamma"]
        );
    }

    #[test]
    fn next_project_selection_falls_back_to_first_running_project() {
        let projects = vec![
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
        ];

        assert_eq!(
            next_project_selection(Some("beta"), &projects).as_deref(),
            Some("beta")
        );
        assert_eq!(
            next_project_selection(Some("missing"), &projects).as_deref(),
            Some("alpha")
        );
    }

    #[test]
    fn next_spec_selection_prefers_existing_selection() {
        let board = fixture_board();

        assert_eq!(
            next_spec_selection(Some("spec-b"), Some(&board)).as_deref(),
            Some("spec-b")
        );
        assert_eq!(
            next_spec_selection(Some("missing"), Some(&board)).as_deref(),
            Some("spec-a")
        );
    }

    #[test]
    fn can_move_to_blocks_in_progress_when_dependencies_are_unmet() {
        let blocked = &fixture_board().specs[1];

        assert!(can_move_to(blocked, SpecStatus::Review).is_ok());
        assert!(can_move_to(blocked, SpecStatus::InProgress).is_err());
    }

    #[test]
    fn specs_for_status_groups_board_cards() {
        let board = fixture_board();

        assert_eq!(specs_for_status(&board, SpecStatus::Pending).len(), 2);
        assert_eq!(specs_for_status(&board, SpecStatus::Review).len(), 1);
    }

    fn fixture_board() -> SpecBoardState {
        SpecBoardState {
            project: "sing".to_string(),
            index_path: PathBuf::from("/home/dev/workspace/specs/index.yaml"),
            specs: vec![
                BoardSpecRecord {
                    spec: SpecRecord {
                        id: "spec-a".to_string(),
                        title: "Spec A".to_string(),
                        status: SpecStatus::Pending,
                        assignee: None,
                        depends_on: Vec::new(),
                        branch: None,
                    },
                    ready: true,
                    blocked: false,
                    unmet_dependencies: Vec::new(),
                },
                BoardSpecRecord {
                    spec: SpecRecord {
                        id: "spec-b".to_string(),
                        title: "Spec B".to_string(),
                        status: SpecStatus::Pending,
                        assignee: Some("codex".to_string()),
                        depends_on: vec!["spec-a".to_string()],
                        branch: Some("feat/spec-b".to_string()),
                    },
                    ready: false,
                    blocked: true,
                    unmet_dependencies: vec!["spec-a".to_string()],
                },
                BoardSpecRecord {
                    spec: SpecRecord {
                        id: "spec-c".to_string(),
                        title: "Spec C".to_string(),
                        status: SpecStatus::Review,
                        assignee: None,
                        depends_on: Vec::new(),
                        branch: None,
                    },
                    ready: false,
                    blocked: false,
                    unmet_dependencies: Vec::new(),
                },
            ],
            summary: SpecBoardSummary {
                counts: SpecCounts {
                    pending: 2,
                    in_progress: 0,
                    review: 1,
                    done: 0,
                },
                ready_count: 1,
                blocked_count: 1,
                next_ready_id: Some("spec-a".to_string()),
            },
        }
    }
}
