use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use db::kvp::KeyValueStore;
use editor::{Editor, EditorEvent};
use gpui::{
    Action, AnyElement, App, AsyncWindowContext, Context, Entity, EventEmitter, FocusHandle,
    Focusable, ParentElement, Pixels, Render, SharedString, StatefulInteractiveElement, Styled,
    Task, WeakEntity, Window, actions, px,
};
use recent_projects::open_remote_project;
use serde::{Deserialize, Serialize};
use sing_bridge::{
    AgentLog, AgentReport, BoardSpecRecord, ProjectAgentStatus, ProjectStatus, SpecBoard,
    SpecStatus,
};
use ui::{
    Button, Chip, Color, Icon, IconButtonShape, IconName, IconSize, Indicator, Label, LabelSize,
    ListItem, ListItemSpacing, SpinnerLabel, TintColor, Tooltip, prelude::*,
};
use util::{ResultExt, TryFutureExt};
use workspace::{
    Item, MultiWorkspace, OpenOptions, Toast, Workspace,
    dock::{DockPosition, Panel, PanelEvent},
    item::TabContentParams,
    notifications::NotificationId,
};

use crate::client::{DefaultSingProjectClientFactory, SingProjectClient, SingProjectClientFactory};
use crate::state::{
    ProjectActionKind, ProjectRow, agent_activity_events, load_project_rows, next_selection,
};

const SING_PROJECT_PANEL_KEY: &str = "SingProjectPanel";
const REFRESH_INTERVAL: Duration = Duration::from_secs(30);

actions!(sing_project, [Toggle, ToggleFocus]);

#[derive(Debug, Serialize, Deserialize, Default)]
struct SerializedSingProjectPanel {
    active: Option<bool>,
    selected_project: Option<String>,
    position: Option<SerializedDockPosition>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SerializedDockPosition {
    Left,
    Bottom,
    Right,
}

impl SerializedDockPosition {
    fn from_dock_position(position: DockPosition) -> Self {
        match position {
            DockPosition::Left => Self::Left,
            DockPosition::Bottom => Self::Bottom,
            DockPosition::Right => Self::Right,
        }
    }

    fn to_dock_position(self) -> DockPosition {
        match self {
            Self::Left => DockPosition::Left,
            Self::Bottom => DockPosition::Bottom,
            Self::Right => DockPosition::Right,
        }
    }
}

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
            workspace.toggle_panel_focus::<SingProjectPanel>(window, cx);
        });
        workspace.register_action(|workspace, _: &Toggle, window, cx| {
            if !workspace.toggle_panel_focus::<SingProjectPanel>(window, cx) {
                workspace.close_panel::<SingProjectPanel>(window, cx);
            }
        });
    })
    .detach();
}

pub struct SingProjectPanel {
    workspace: WeakEntity<Workspace>,
    serialization_key: Option<String>,
    focus_handle: FocusHandle,
    client_factory: Arc<dyn SingProjectClientFactory>,
    client: Option<Arc<dyn SingProjectClient>>,
    search_bar: Entity<Editor>,
    position: DockPosition,
    active: bool,
    loading: bool,
    last_error: Option<String>,
    last_refreshed_at: Option<Instant>,
    projects: Vec<ProjectRow>,
    selected_project: Option<String>,
    current_request_id: usize,
    pending_serialization: Task<Option<()>>,
    polling_task: Task<()>,
}

impl SingProjectPanel {
    pub async fn load(
        workspace: WeakEntity<Workspace>,
        cx: AsyncWindowContext,
    ) -> anyhow::Result<Entity<Self>> {
        Self::load_with_factory(workspace, Arc::new(DefaultSingProjectClientFactory), cx).await
    }

    async fn load_with_factory(
        workspace: WeakEntity<Workspace>,
        client_factory: Arc<dyn SingProjectClientFactory>,
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
                    .context("loading SAIL project panel")
                    .log_err()
                    .flatten()
                    .map(|panel| serde_json::from_str::<SerializedSingProjectPanel>(&panel))
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
        serialized: Option<&SerializedSingProjectPanel>,
        client_factory: Arc<dyn SingProjectClientFactory>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let serialization_key = Self::serialization_key(workspace);
        let workspace = workspace.weak_handle();
        let position = serialized
            .and_then(|panel| panel.position)
            .map(SerializedDockPosition::to_dock_position)
            .unwrap_or(DockPosition::Left);

        cx.new(|cx| {
            let search_bar = cx.new(|cx| {
                let mut editor = Editor::single_line(window, cx);
                editor.set_placeholder_text("Filter projects...", window, cx);
                editor
            });
            cx.subscribe(&search_bar, |_this, _, event: &EditorEvent, cx| {
                if matches!(
                    event,
                    EditorEvent::BufferEdited | EditorEvent::Edited { .. }
                ) {
                    cx.notify();
                }
            })
            .detach();

            Self {
                workspace,
                serialization_key,
                focus_handle: cx.focus_handle(),
                client_factory,
                client: None,
                search_bar,
                position,
                active: serialized.and_then(|panel| panel.active).unwrap_or(true),
                loading: false,
                last_error: None,
                last_refreshed_at: None,
                projects: Vec::new(),
                selected_project: serialized.and_then(|panel| panel.selected_project.clone()),
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
            .map(|id| format!("{SING_PROJECT_PANEL_KEY}-{id:?}"))
    }

    fn serialize(&mut self, cx: &mut Context<Self>) {
        let Some(serialization_key) = self.serialization_key.clone() else {
            return;
        };

        let serialized = SerializedSingProjectPanel {
            active: self.active.then_some(true),
            selected_project: self.selected_project.clone(),
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

    fn ensure_client(&mut self) -> Result<Arc<dyn SingProjectClient>> {
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }

        let client = self.client_factory.create()?;
        self.client = Some(client.clone());
        Ok(client)
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.loading = true;
        self.last_error = None;
        self.current_request_id += 1;
        let request_id = self.current_request_id;
        cx.notify();

        cx.spawn_in(window, async move |panel, cx| {
            let client = match panel.update_in(cx, |panel, _, _| panel.ensure_client()) {
                Ok(Ok(client)) => client,
                Ok(Err(error)) => {
                    panel
                        .update_in(cx, |panel, _, cx| {
                            panel.finish_refresh(request_id, Err(error), cx);
                        })
                        .ok();
                    return;
                }
                Err(_) => return,
            };

            let result = load_project_rows(client).await;
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
        result: Result<Vec<ProjectRow>>,
        cx: &mut Context<Self>,
    ) {
        if request_id != self.current_request_id {
            return;
        }

        self.loading = false;

        match result {
            Ok(projects) => {
                let events = agent_activity_events(&self.projects, &projects);
                self.last_refreshed_at = Some(Instant::now());
                self.projects = projects;
                self.selected_project =
                    next_selection(self.selected_project.as_deref(), &self.projects);
                self.sync_open_project_homes(cx);
                for event in events {
                    self.show_agent_activity_event(
                        event.project,
                        event.message,
                        event.needs_attention,
                        cx,
                    );
                }
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
                if self.projects.is_empty() {
                    self.selected_project = None;
                }
            }
        }

        self.serialize(cx);
        cx.notify();
    }

    fn sync_open_project_homes(&self, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };

        workspace.update(cx, |workspace, cx| {
            let homes = workspace
                .items_of_type::<ProjectHomeItem>(cx)
                .collect::<Vec<_>>();
            for home in homes {
                let project_name = home.read(cx).project_name().to_string();
                if let Some(project) = self
                    .projects
                    .iter()
                    .find(|project| project.name == project_name)
                    .cloned()
                {
                    home.update(cx, |home, cx| home.set_project(project, cx));
                } else {
                    home.update(cx, |home, cx| home.mark_missing(cx));
                }
            }
        });
    }

    fn select_project(&mut self, project: &str, cx: &mut Context<Self>) {
        if self.selected_project.as_deref() != Some(project) {
            self.selected_project = Some(project.to_string());
            self.serialize(cx);
            cx.notify();
        }
    }

    fn open_project_home(
        &mut self,
        project_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_project(&project_name, cx);

        let Some(project) = self
            .projects
            .iter()
            .find(|project| project.name == project_name)
            .cloned()
        else {
            return;
        };

        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };

        let client_factory = self.client_factory.clone();
        workspace.update(cx, |workspace, cx| {
            let existing = workspace
                .items_of_type::<ProjectHomeItem>(cx)
                .find(|item| item.read(cx).project_name() == project_name);

            if let Some(item) = existing {
                item.update(cx, |item, cx| {
                    item.set_project(project.clone(), cx);
                });
                workspace.activate_item(&item, true, true, window, cx);
                return;
            }

            let item = cx.new(|cx| {
                ProjectHomeItem::new(
                    project.clone(),
                    workspace.weak_handle(),
                    client_factory.clone(),
                    cx,
                )
            });
            let added_to_center = workspace.add_item_to_center(Box::new(item.clone()), window, cx);
            if !added_to_center {
                workspace.add_item_to_active_pane(Box::new(item.clone()), None, true, window, cx);
            }
            item.update(cx, |item, cx| item.refresh(window, cx));
        });
    }

    fn show_agent_activity_event(
        &mut self,
        project: String,
        message: String,
        needs_attention: bool,
        cx: &mut Context<Self>,
    ) {
        let prefix = if needs_attention {
            "Agent needs attention"
        } else {
            "Agent update"
        };
        self.show_toast(format!("{prefix}: {project} · {message}"), cx);
    }

    fn show_toast(&mut self, message: String, cx: &mut Context<Self>) {
        if let Some(workspace) = self.workspace.upgrade() {
            workspace.update(cx, |workspace, cx| {
                workspace.show_toast(
                    Toast::new(
                        NotificationId::composite::<SingProjectPanel>("sing-project-panel"),
                        message.clone(),
                    ),
                    cx,
                );
            });
        }
    }

    fn search_query(&self, cx: &App) -> String {
        self.search_bar
            .read(cx)
            .text(cx)
            .trim()
            .to_ascii_lowercase()
    }

    fn filtered_projects<'a>(&'a self, cx: &App) -> Vec<&'a ProjectRow> {
        let query = self.search_query(cx);
        self.projects
            .iter()
            .filter(|project| Self::matches_search_query(project, &query))
            .collect()
    }

    fn matches_search_query(project: &ProjectRow, query: &str) -> bool {
        query.is_empty()
            || contains_query(&project.name, query)
            || project
                .description
                .as_deref()
                .is_some_and(|value| contains_query(value, query))
            || contains_query(project.status_label(), query)
            || contains_query(&project.agent_summary(), query)
            || contains_query(&project.spec_summary(), query)
            || contains_query(&project.agent_detail(), query)
            || contains_query(&project.spec_detail(), query)
            || project
                .runtime_summary()
                .as_deref()
                .is_some_and(|value| contains_query(value, query))
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

    fn project_status_chip(&self, project: &ProjectRow) -> Chip {
        Self::badge(project.status_label(), status_color(project.status))
    }

    fn badge(label: impl Into<String>, color: Color) -> Chip {
        Chip::new(label.into()).label_color(color)
    }

    fn project_secondary_line(&self, project: &ProjectRow) -> Option<String> {
        Some(project.list_summary())
    }

    fn render_refresh_control(&self, cx: &mut Context<Self>) -> AnyElement {
        let tooltip = match self.refresh_status_label() {
            Some(refreshed) => format!("Refresh project state ({refreshed})"),
            None => "Refresh project state".to_string(),
        };

        if self.loading {
            return h_flex()
                .id("sing-project-refresh")
                .size(px(28.))
                .items_center()
                .justify_center()
                .rounded_sm()
                .tooltip(Tooltip::text(tooltip))
                .child(SpinnerLabel::dots_variant().size(LabelSize::Small))
                .into_any_element();
        }

        IconButton::new("sing-project-refresh", IconName::RotateCw)
            .shape(IconButtonShape::Square)
            .icon_size(IconSize::Small)
            .style(ButtonStyle::Subtle)
            .tooltip(Tooltip::text(tooltip))
            .on_click(cx.listener(|this, _, window, cx| {
                this.refresh(window, cx);
            }))
            .into_any_element()
    }

    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let border = cx.theme().colors().border_variant;
        let editor_background = cx.theme().colors().editor_background;
        let panel_background = cx.theme().colors().panel_background;
        v_flex()
            .w_full()
            .gap_1p5()
            .p_2()
            .border_b_1()
            .border_color(border)
            .bg(editor_background)
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(Label::new("Projects"))
                    .child(self.render_refresh_control(cx)),
            )
            .child(
                h_flex()
                    .py_1()
                    .px_1p5()
                    .gap_1p5()
                    .rounded_sm()
                    .bg(panel_background)
                    .border_1()
                    .border_color(border)
                    .child(Icon::new(IconName::MagnifyingGlass).color(Color::Muted))
                    .child(self.search_bar.clone()),
            )
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

    fn render_projects(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.loading && self.projects.is_empty() {
            return v_flex()
                .size_full()
                .justify_center()
                .items_center()
                .gap_2()
                .child(SpinnerLabel::dots_variant().size(LabelSize::Large))
                .child(
                    Label::new("Loading projects")
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
                .child(Icon::new(IconName::Server).color(Color::Muted))
                .child(
                    Label::new("No projects found")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .into_any_element();
        }

        let filtered_projects = self.filtered_projects(cx);
        if filtered_projects.is_empty() {
            return v_flex()
                .size_full()
                .justify_center()
                .items_center()
                .gap_2()
                .child(Icon::new(IconName::MagnifyingGlass).color(Color::Muted))
                .child(
                    Label::new("No projects match this filter")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .into_any_element();
        }

        let selected_project = self.selected_project.as_deref();
        let items = filtered_projects
            .into_iter()
            .map(|project| {
                let project_name = project.name.clone();
                let status_color = status_color(project.status);
                let secondary_line = self.project_secondary_line(project);
                ListItem::new(format!("sing-project-row-{project_name}"))
                    .inset(true)
                    .spacing(ListItemSpacing::Sparse)
                    .toggle_state(selected_project == Some(project_name.as_str()))
                    .start_slot(Indicator::dot().color(status_color))
                    .end_slot(self.project_status_chip(project))
                    .child(
                        v_flex()
                            .w_full()
                            .gap_1()
                            .child(
                                Label::new(project.name.clone())
                                    .size(LabelSize::Small)
                                    .truncate(),
                            )
                            .when_some(secondary_line, |element, line| {
                                element.child(
                                    Label::new(line)
                                        .size(LabelSize::Small)
                                        .color(Color::Muted)
                                        .truncate(),
                                )
                            }),
                    )
                    .tooltip(Tooltip::text(project.name.clone()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_project_home(project_name.clone(), window, cx);
                    }))
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        div()
            .id("sing-project-list")
            .flex_1()
            .overflow_y_scroll()
            .child(v_flex().w_full().gap_1().p_2().children(items))
            .into_any_element()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectHomeMode {
    Summary,
    Specs,
    ArchivedSpecs,
    Kanban,
    Agents,
    Activity,
    Settings,
}

impl ProjectHomeMode {
    const ALL: [Self; 7] = [
        Self::Summary,
        Self::Specs,
        Self::ArchivedSpecs,
        Self::Kanban,
        Self::Agents,
        Self::Activity,
        Self::Settings,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Summary => "Summary",
            Self::Specs => "Specs",
            Self::ArchivedSpecs => "Archived",
            Self::Kanban => "Kanban",
            Self::Agents => "Agents",
            Self::Activity => "Activity",
            Self::Settings => "Settings",
        }
    }
}

struct ProjectHomeRefresh {
    project: ProjectRow,
    spec_board: Option<Result<SpecBoard, String>>,
    agent_status: Result<ProjectAgentStatus, String>,
    agent_log: Result<AgentLog, String>,
    agent_report: Result<AgentReport, String>,
}

pub struct ProjectHomeItem {
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    client_factory: Arc<dyn SingProjectClientFactory>,
    client: Option<Arc<dyn SingProjectClient>>,
    project_name: String,
    project: Option<ProjectRow>,
    selected_mode: ProjectHomeMode,
    loading: bool,
    pending_action: Option<ProjectActionKind>,
    last_error: Option<String>,
    last_refreshed_at: Option<Instant>,
    spec_board: Option<SpecBoard>,
    spec_error: Option<String>,
    agent_status: Option<ProjectAgentStatus>,
    agent_error: Option<String>,
    agent_log: Option<AgentLog>,
    agent_log_error: Option<String>,
    agent_report: Option<AgentReport>,
    agent_report_error: Option<String>,
}

impl ProjectHomeItem {
    fn new(
        project: ProjectRow,
        workspace: WeakEntity<Workspace>,
        client_factory: Arc<dyn SingProjectClientFactory>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            workspace,
            focus_handle: cx.focus_handle(),
            client_factory,
            client: None,
            project_name: project.name.clone(),
            project: Some(project),
            selected_mode: ProjectHomeMode::Summary,
            loading: false,
            pending_action: None,
            last_error: None,
            last_refreshed_at: None,
            spec_board: None,
            spec_error: None,
            agent_status: None,
            agent_error: None,
            agent_log: None,
            agent_log_error: None,
            agent_report: None,
            agent_report_error: None,
        }
    }

    fn project_name(&self) -> &str {
        &self.project_name
    }

    fn set_project(&mut self, project: ProjectRow, cx: &mut Context<Self>) {
        self.project_name = project.name.clone();
        self.project = Some(project);
        self.last_error = None;
        cx.notify();
    }

    fn mark_missing(&mut self, cx: &mut Context<Self>) {
        self.project = None;
        self.last_error = Some("Project is no longer present after refresh".to_string());
        cx.notify();
    }

    fn ensure_client(&mut self) -> Result<Arc<dyn SingProjectClient>> {
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }

        let client = self.client_factory.create()?;
        self.client = Some(client.clone());
        Ok(client)
    }

    fn select_mode(&mut self, mode: ProjectHomeMode, cx: &mut Context<Self>) {
        if self.selected_mode != mode {
            self.selected_mode = mode;
            cx.notify();
        }
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.loading = true;
        self.last_error = None;
        cx.notify();

        let project_name = self.project_name.clone();
        cx.spawn_in(window, async move |item, cx| {
            let result = async {
                let client = item.update_in(cx, |item, _, _| item.ensure_client())??;
                let projects = load_project_rows(client.clone()).await?;
                let project = projects
                    .into_iter()
                    .find(|project| project.name == project_name)
                    .ok_or_else(|| anyhow!("project `{project_name}` was not found"))?;

                let spec_board = project.specs.available.then(|| {
                    let client = client.clone();
                    let project_name = project_name.clone();
                    async move {
                        client
                            .list_specs(&project_name)
                            .await
                            .map_err(|error| error.to_string())
                    }
                });

                let spec_board = match spec_board {
                    Some(task) => Some(task.await),
                    None => None,
                };

                let agent_status = client
                    .agent_status(&project_name)
                    .await
                    .map_err(|error| error.to_string());
                let agent_log = client
                    .agent_log(&project_name, 80)
                    .await
                    .map_err(|error| error.to_string());
                let agent_report = client
                    .agent_report(&project_name)
                    .await
                    .map_err(|error| error.to_string());

                anyhow::Ok(ProjectHomeRefresh {
                    project,
                    spec_board,
                    agent_status,
                    agent_log,
                    agent_report,
                })
            }
            .await;

            item.update_in(cx, |item, _, cx| item.finish_refresh(result, cx))
                .ok();
        })
        .detach();
    }

    fn finish_refresh(&mut self, result: Result<ProjectHomeRefresh>, cx: &mut Context<Self>) {
        self.loading = false;

        match result {
            Ok(refresh) => {
                self.project_name = refresh.project.name.clone();
                self.project = Some(refresh.project);
                self.last_error = None;
                self.last_refreshed_at = Some(Instant::now());

                if let Some(spec_board) = refresh.spec_board {
                    match spec_board {
                        Ok(board) => {
                            self.spec_board = Some(board);
                            self.spec_error = None;
                        }
                        Err(error) => {
                            self.spec_error = Some(error);
                        }
                    }
                } else {
                    self.spec_board = None;
                    self.spec_error = None;
                }

                match refresh.agent_status {
                    Ok(status) => {
                        self.agent_status = Some(status);
                        self.agent_error = None;
                    }
                    Err(error) => self.agent_error = Some(error),
                }
                match refresh.agent_log {
                    Ok(log) => {
                        self.agent_log = Some(log);
                        self.agent_log_error = None;
                    }
                    Err(error) => self.agent_log_error = Some(error),
                }
                match refresh.agent_report {
                    Ok(report) => {
                        self.agent_report = Some(report);
                        self.agent_report_error = None;
                    }
                    Err(error) => self.agent_report_error = Some(error),
                }
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
            }
        }

        cx.notify();
    }

    fn open_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.run_project_action(ProjectActionKind::Open, window, cx);
    }

    fn start_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.run_project_action(ProjectActionKind::Start, window, cx);
    }

    fn stop_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.run_project_action(ProjectActionKind::Stop, window, cx);
    }

    fn run_project_action(
        &mut self,
        action: ProjectActionKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending_action.is_some() {
            return;
        }

        self.pending_action = Some(action);
        cx.notify();

        let project = self.project_name.clone();
        let workspace = self.workspace.clone();
        cx.spawn_in(window, async move |item, cx| {
            let result = async {
                let client = item.update_in(cx, |item, _, _| item.ensure_client())??;
                match action {
                    ProjectActionKind::Start => {
                        let result = client.start_project(&project).await?;
                        Ok(format!("Started {}", result.name))
                    }
                    ProjectActionKind::Stop => {
                        client.stop_project(&project).await?;
                        Ok(format!("Stopped {project}"))
                    }
                    ProjectActionKind::Open => {
                        let target = client.project_remote_target(&project).await?;
                        let (app_state, open_options) =
                            workspace.update_in(cx, |workspace, window, _| {
                                let requesting_window =
                                    window.window_handle().downcast::<MultiWorkspace>();
                                let open_options = OpenOptions {
                                    requesting_window,
                                    ..Default::default()
                                };
                                (workspace.app_state().clone(), open_options)
                            })?;
                        open_remote_project(
                            target.connection_options,
                            vec![target.workspace_root],
                            app_state,
                            open_options,
                            cx,
                        )
                        .await?;
                        Ok(String::new())
                    }
                }
            }
            .await;

            item.update_in(cx, |item, window, cx| {
                item.finish_action(action, result, window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn finish_action(
        &mut self,
        action: ProjectActionKind,
        result: Result<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending_action = None;

        match result {
            Ok(message) => {
                self.last_error = None;
                if !message.is_empty() {
                    self.show_toast(message, cx);
                }
                if matches!(action, ProjectActionKind::Start | ProjectActionKind::Stop) {
                    self.refresh(window, cx);
                } else {
                    cx.notify();
                }
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
                self.show_error(error.to_string(), cx);
                cx.notify();
            }
        }
    }

    fn show_toast(&self, message: String, cx: &mut Context<Self>) {
        if let Some(workspace) = self.workspace.upgrade() {
            workspace.update(cx, |workspace, cx| {
                workspace.show_toast(
                    Toast::new(
                        NotificationId::composite::<ProjectHomeItem>("sing-project-home"),
                        message.clone(),
                    ),
                    cx,
                );
            });
        }
    }

    fn show_error(&self, error: String, cx: &mut Context<Self>) {
        if let Some(workspace) = self.workspace.upgrade() {
            workspace.update(cx, |workspace, cx| {
                workspace.show_error(&anyhow!(error.clone()), cx);
            });
        }
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

    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let project = self.project.as_ref();
        let open_pending = self.pending_action == Some(ProjectActionKind::Open);
        let start_pending = self.pending_action == Some(ProjectActionKind::Start);
        let stop_pending = self.pending_action == Some(ProjectActionKind::Stop);

        v_flex()
            .w_full()
            .gap_2()
            .p_3()
            .border_b_1()
            .border_color(theme.colors().border_variant)
            .bg(theme.colors().editor_background)
            .child(
                h_flex()
                    .w_full()
                    .items_start()
                    .justify_between()
                    .gap_3()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(Icon::new(IconName::Server).color(Color::Muted))
                                    .child(
                                        Label::new(self.project_name.clone())
                                            .size(LabelSize::Large),
                                    ),
                            )
                            .when_some(
                                project.and_then(|project| project.description.as_ref()),
                                |element, description| {
                                    element
                                        .child(Label::new(description.clone()).color(Color::Muted))
                                },
                            )
                            .when_some(self.last_error.as_ref(), |element, error| {
                                element.child(Label::new(error.clone()).color(Color::Warning))
                            }),
                    )
                    .when_some(project, |element, project| {
                        element.child(project_status_chip(project))
                    }),
            )
            .when_some(project, |element, project| {
                element.child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .flex_wrap()
                        .child(
                            Button::new(
                                format!("sing-project-home-open-{}", self.project_name),
                                "Open remote",
                            )
                            .style(ButtonStyle::Filled)
                            .label_size(LabelSize::Small)
                            .loading(open_pending)
                            .disabled(!project.can_open() || start_pending || stop_pending)
                            .tooltip(Tooltip::text("Open this project in a remote workspace"))
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    this.open_project(window, cx);
                                },
                            )),
                        )
                        .when(project.can_start(), |element| {
                            element.child(
                                Button::new(
                                    format!("sing-project-home-start-{}", self.project_name),
                                    "Start",
                                )
                                .style(ButtonStyle::Tinted(TintColor::Success))
                                .label_size(LabelSize::Small)
                                .loading(start_pending)
                                .disabled(open_pending || stop_pending)
                                .tooltip(Tooltip::text("Run sail up for this project"))
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        this.start_project(window, cx);
                                    },
                                )),
                            )
                        })
                        .when(project.can_stop(), |element| {
                            element.child(
                                Button::new(
                                    format!("sing-project-home-stop-{}", self.project_name),
                                    "Stop",
                                )
                                .style(ButtonStyle::Tinted(TintColor::Warning))
                                .label_size(LabelSize::Small)
                                .loading(stop_pending)
                                .disabled(open_pending || start_pending)
                                .tooltip(Tooltip::text("Run sail down for this project"))
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        this.stop_project(window, cx);
                                    },
                                )),
                            )
                        })
                        .child(
                            Button::new(
                                format!("sing-project-home-refresh-{}", self.project_name),
                                "Refresh",
                            )
                            .style(ButtonStyle::Outlined)
                            .label_size(LabelSize::Small)
                            .loading(self.loading)
                            .disabled(self.pending_action.is_some())
                            .tooltip(Tooltip::text(
                                self.refresh_status_label()
                                    .unwrap_or_else(|| "Refresh project home".to_string()),
                            ))
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    this.refresh(window, cx);
                                },
                            )),
                        ),
                )
            })
            .into_any_element()
    }

    fn render_mode_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        h_flex()
            .w_full()
            .gap_1()
            .p_2()
            .border_b_1()
            .border_color(cx.theme().colors().border_variant)
            .bg(cx.theme().colors().panel_background)
            .children(ProjectHomeMode::ALL.into_iter().map(|mode| {
                Button::new(
                    format!("sing-project-home-mode-{}-{mode:?}", self.project_name),
                    mode.label(),
                )
                .style(if self.selected_mode == mode {
                    ButtonStyle::Filled
                } else {
                    ButtonStyle::Subtle
                })
                .label_size(LabelSize::Small)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select_mode(mode, cx);
                }))
                .into_any_element()
            }))
            .into_any_element()
    }

    fn render_content(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(project) = self.project.as_ref() else {
            return v_flex()
                .size_full()
                .justify_center()
                .items_center()
                .gap_2()
                .child(Icon::new(IconName::Warning).color(Color::Warning))
                .child(Label::new("Project unavailable").color(Color::Warning))
                .into_any_element();
        };

        div()
            .id("sing-project-home-content")
            .flex_1()
            .overflow_y_scroll()
            .child(match self.selected_mode {
                ProjectHomeMode::Summary => self.render_summary(project, cx),
                ProjectHomeMode::Specs => self.render_specs(project, cx),
                ProjectHomeMode::ArchivedSpecs => self.render_archived_specs(cx),
                ProjectHomeMode::Kanban => self.render_kanban(cx),
                ProjectHomeMode::Agents => self.render_agents(project, cx),
                ProjectHomeMode::Activity => self.render_activity(cx),
                ProjectHomeMode::Settings => self.render_settings(project, cx),
            })
            .into_any_element()
    }

    fn render_summary(&self, project: &ProjectRow, cx: &mut Context<Self>) -> AnyElement {
        let activity = project.agent_activity();
        let runtime_summary = project
            .runtime_summary()
            .unwrap_or_else(|| "Runtime metadata unavailable".to_string());
        let branch = project.branch().map(str::to_string);
        let ip = project.ip.clone();

        v_flex()
            .w_full()
            .gap_3()
            .p_3()
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .flex_wrap()
                    .children(project_home_badges(project)),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .flex_wrap()
                    .child(metric_tile(
                        "Status",
                        project.status_label(),
                        status_color(project.status),
                        cx,
                    ))
                    .child(metric_tile(
                        "Ready",
                        project.ready_count().unwrap_or_default().to_string(),
                        Color::Success,
                        cx,
                    ))
                    .child(metric_tile(
                        "Blocked",
                        project.blocked_count().unwrap_or_default().to_string(),
                        Color::Warning,
                        cx,
                    ))
                    .child(metric_tile(
                        "Agent",
                        project.agent_badge(),
                        Color::Accent,
                        cx,
                    )),
            )
            .child(section_panel(
                "Project details",
                v_flex()
                    .w_full()
                    .gap_1p5()
                    .child(home_detail_line("Runtime", runtime_summary, Color::Default))
                    .child(home_detail_line(
                        "Activity",
                        activity.headline,
                        Color::Default,
                    ))
                    .when_some(activity.detail, |element, detail| {
                        element.child(home_detail_line("Context", detail, Color::Muted))
                    })
                    .when_some(activity.timestamp, |element, timestamp| {
                        element.child(home_detail_line("Started", timestamp, Color::Muted))
                    })
                    .child(home_detail_line(
                        "Specs",
                        project.spec_detail(),
                        Color::Default,
                    ))
                    .when_some(branch, |element, branch| {
                        element.child(home_detail_line("Branch", branch, Color::Accent))
                    })
                    .when_some(ip, |element, ip| {
                        element.child(home_detail_line(
                            "Network",
                            format!("IP {ip}"),
                            Color::Muted,
                        ))
                    })
                    .when_some(project.detail_error.as_ref(), |element, error| {
                        element.child(home_detail_line("Details", error.clone(), Color::Warning))
                    })
                    .into_any_element(),
                cx,
            ))
            .into_any_element()
    }

    fn render_specs(&self, project: &ProjectRow, cx: &mut Context<Self>) -> AnyElement {
        let Some(board) = self.spec_board.as_ref() else {
            return self.render_spec_unavailable(project, cx);
        };

        let active_specs = board
            .specs
            .iter()
            .filter(|spec| spec.spec.status != SpecStatus::Done)
            .collect::<Vec<_>>();

        v_flex()
            .w_full()
            .gap_3()
            .p_3()
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .flex_wrap()
                    .child(metric_tile(
                        "Pending",
                        board.counts.pending.to_string(),
                        Color::Muted,
                        cx,
                    ))
                    .child(metric_tile(
                        "In progress",
                        board.counts.in_progress.to_string(),
                        Color::Accent,
                        cx,
                    ))
                    .child(metric_tile(
                        "Review",
                        board.counts.review.to_string(),
                        Color::Warning,
                        cx,
                    ))
                    .child(metric_tile(
                        "Ready",
                        board.summary.ready_count.to_string(),
                        Color::Success,
                        cx,
                    )),
            )
            .child(section_panel(
                "Active specs",
                specs_list(active_specs, "No active specs", cx),
                cx,
            ))
            .into_any_element()
    }

    fn render_spec_unavailable(&self, project: &ProjectRow, cx: &mut Context<Self>) -> AnyElement {
        let message = self
            .spec_error
            .clone()
            .unwrap_or_else(|| project.spec_detail());

        v_flex()
            .w_full()
            .gap_3()
            .p_3()
            .child(section_panel(
                "Specs",
                v_flex()
                    .w_full()
                    .gap_2()
                    .child(Label::new(message).color(Color::Muted))
                    .into_any_element(),
                cx,
            ))
            .into_any_element()
    }

    fn render_archived_specs(&self, cx: &mut Context<Self>) -> AnyElement {
        let archived = self
            .spec_board
            .as_ref()
            .map(|board| {
                board
                    .specs
                    .iter()
                    .filter(|spec| spec.spec.status == SpecStatus::Done)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        v_flex()
            .w_full()
            .gap_3()
            .p_3()
            .child(section_panel(
                "Archived specs",
                specs_list(archived, "No archived specs", cx),
                cx,
            ))
            .into_any_element()
    }

    fn render_kanban(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(board) = self.spec_board.as_ref() else {
            return v_flex()
                .w_full()
                .gap_3()
                .p_3()
                .child(section_panel(
                    "Kanban",
                    Label::new(self.spec_error.clone().unwrap_or_else(|| {
                        "Spec board is not loaded for this project".to_string()
                    }))
                    .color(Color::Muted)
                    .into_any_element(),
                    cx,
                ))
                .into_any_element();
        };

        h_flex()
            .w_full()
            .items_start()
            .gap_2()
            .flex_wrap()
            .p_3()
            .children([
                kanban_column("Pending", board, SpecStatus::Pending, cx),
                kanban_column("In progress", board, SpecStatus::InProgress, cx),
                kanban_column("Review", board, SpecStatus::Review, cx),
                kanban_column("Done", board, SpecStatus::Done, cx),
            ])
            .into_any_element()
    }

    fn render_agents(&self, project: &ProjectRow, cx: &mut Context<Self>) -> AnyElement {
        let status = self
            .agent_status
            .as_ref()
            .map(format_agent_status)
            .or_else(|| self.agent_error.clone())
            .unwrap_or_else(|| project.agent_detail());
        let report = self
            .agent_report
            .as_ref()
            .map(format_agent_report)
            .or_else(|| self.agent_report_error.clone())
            .unwrap_or_else(|| "Agent report has not been loaded yet".to_string());

        v_flex()
            .w_full()
            .gap_3()
            .p_3()
            .child(section_panel(
                "Agent status",
                v_flex()
                    .w_full()
                    .gap_1p5()
                    .child(home_detail_line("State", status, Color::Default))
                    .child(home_detail_line(
                        "Session",
                        project.agent_detail(),
                        Color::Muted,
                    ))
                    .into_any_element(),
                cx,
            ))
            .child(section_panel(
                "Agent report",
                Label::new(report).color(Color::Muted).into_any_element(),
                cx,
            ))
            .into_any_element()
    }

    fn render_activity(&self, cx: &mut Context<Self>) -> AnyElement {
        let log = self
            .agent_log
            .as_ref()
            .map(|log| log.lines.clone())
            .unwrap_or_default();
        let error = self.agent_log_error.clone().or_else(|| {
            self.agent_log
                .as_ref()
                .and_then(|log| log.error.as_ref().cloned())
        });

        v_flex()
            .w_full()
            .gap_3()
            .p_3()
            .child(section_panel(
                "Activity log",
                if let Some(error) = error {
                    Label::new(error).color(Color::Warning).into_any_element()
                } else if log.is_empty() {
                    Label::new("No recent agent log lines")
                        .color(Color::Muted)
                        .into_any_element()
                } else {
                    v_flex()
                        .w_full()
                        .gap_1()
                        .children(log.into_iter().map(|line| {
                            Label::new(line)
                                .size(LabelSize::Small)
                                .color(Color::Muted)
                                .buffer_font(cx)
                                .into_any_element()
                        }))
                        .into_any_element()
                },
                cx,
            ))
            .into_any_element()
    }

    fn render_settings(&self, project: &ProjectRow, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .w_full()
            .gap_3()
            .p_3()
            .child(section_panel(
                "Project settings",
                v_flex()
                    .w_full()
                    .gap_1p5()
                    .child(home_detail_line(
                        "Name",
                        project.name.clone(),
                        Color::Default,
                    ))
                    .child(home_detail_line(
                        "Status",
                        project.status_label(),
                        status_color(project.status),
                    ))
                    .child(home_detail_line(
                        "Specs",
                        project.spec_detail(),
                        Color::Default,
                    ))
                    .when_some(project.description.as_ref(), |element, description| {
                        element.child(home_detail_line(
                            "Description",
                            description.clone(),
                            Color::Muted,
                        ))
                    })
                    .when_some(project.runtime_summary(), |element, runtimes| {
                        element.child(home_detail_line("Runtimes", runtimes, Color::Muted))
                    })
                    .when_some(project.ip.as_ref(), |element, ip| {
                        element.child(home_detail_line("Container IP", ip.clone(), Color::Muted))
                    })
                    .into_any_element(),
                cx,
            ))
            .into_any_element()
    }
}

impl EventEmitter<()> for ProjectHomeItem {}

impl Focusable for ProjectHomeItem {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ProjectHomeItem {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("sing-project-home")
            .track_focus(&self.focus_handle)
            .size_full()
            .overflow_hidden()
            .bg(cx.theme().colors().background)
            .child(self.render_header(cx))
            .child(self.render_mode_picker(cx))
            .child(self.render_content(cx))
    }
}

impl Item for ProjectHomeItem {
    type Event = ();

    fn tab_content(&self, params: TabContentParams, _window: &Window, cx: &App) -> AnyElement {
        Label::new(self.tab_content_text(params.detail.unwrap_or_default(), cx))
            .color(params.text_color())
            .truncate()
            .into_any_element()
    }

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        format!("{} Home", self.project_name).into()
    }

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::Server))
    }

    fn tab_tooltip_text(&self, _: &App) -> Option<SharedString> {
        Some(format!("Project home: {}", self.project_name).into())
    }
}

fn project_status_chip(project: &ProjectRow) -> Chip {
    Chip::new(project.status_label()).label_color(status_color(project.status))
}

fn project_home_badges(project: &ProjectRow) -> Vec<AnyElement> {
    let mut badges = vec![
        Chip::new(project.agent_badge())
            .label_color(match project.status {
                ProjectStatus::Running if project.agent_session.running => Color::Accent,
                ProjectStatus::Running => Color::Muted,
                ProjectStatus::Error => Color::Error,
                ProjectStatus::Stopped | ProjectStatus::NotCreated => Color::Warning,
            })
            .into_any_element(),
    ];

    if let Some(ready) = project.ready_count().filter(|ready| *ready > 0) {
        badges.push(
            Chip::new(format!("Ready {ready}"))
                .label_color(Color::Success)
                .into_any_element(),
        );
    }

    if let Some(blocked) = project.blocked_count().filter(|blocked| *blocked > 0) {
        badges.push(
            Chip::new(format!("Blocked {blocked}"))
                .label_color(Color::Warning)
                .into_any_element(),
        );
    }

    if project.status == ProjectStatus::Running && !project.specs.available {
        badges.push(
            Chip::new("Specs need setup")
                .label_color(Color::Warning)
                .into_any_element(),
        );
    }

    badges
}

fn metric_tile(
    label: &'static str,
    value: impl Into<String>,
    color: Color,
    cx: &mut App,
) -> AnyElement {
    let theme = cx.theme();
    v_flex()
        .min_w(px(150.))
        .gap_1()
        .p_2()
        .rounded_sm()
        .border_1()
        .border_color(theme.colors().border_variant)
        .bg(theme.colors().editor_background)
        .child(Label::new(label).size(LabelSize::Small).color(Color::Muted))
        .child(Label::new(value.into()).color(color))
        .into_any_element()
}

fn section_panel(title: &'static str, content: AnyElement, cx: &mut App) -> AnyElement {
    let theme = cx.theme();
    v_flex()
        .w_full()
        .gap_2()
        .p_3()
        .rounded_sm()
        .border_1()
        .border_color(theme.colors().border_variant)
        .bg(theme.colors().editor_background)
        .child(Label::new(title))
        .child(content)
        .into_any_element()
}

fn home_detail_line(label: &'static str, value: impl Into<String>, color: Color) -> AnyElement {
    h_flex()
        .w_full()
        .items_start()
        .gap_3()
        .child(
            div()
                .w(px(96.))
                .child(Label::new(label).size(LabelSize::Small).color(Color::Muted)),
        )
        .child(Label::new(value.into()).size(LabelSize::Small).color(color))
        .into_any_element()
}

fn specs_list(specs: Vec<&BoardSpecRecord>, empty: &'static str, cx: &mut App) -> AnyElement {
    if specs.is_empty() {
        return Label::new(empty).color(Color::Muted).into_any_element();
    }

    v_flex()
        .w_full()
        .gap_1()
        .children(specs.into_iter().map(|spec| spec_row(spec, cx)))
        .into_any_element()
}

fn spec_row(spec: &BoardSpecRecord, cx: &mut App) -> AnyElement {
    let theme = cx.theme();
    h_flex()
        .id(format!("sing-project-home-spec-row-{}", spec.spec.id))
        .w_full()
        .items_start()
        .justify_between()
        .gap_3()
        .p_2()
        .rounded_sm()
        .border_1()
        .border_color(theme.colors().border_variant)
        .bg(theme.colors().panel_background)
        .child(
            v_flex()
                .min_w(px(0.))
                .gap_0p5()
                .child(
                    h_flex()
                        .gap_1p5()
                        .items_center()
                        .child(Indicator::dot().color(spec_status_color(spec.spec.status)))
                        .child(Label::new(spec.spec.id.clone()).size(LabelSize::Small)),
                )
                .child(Label::new(spec_title(spec)).color(Color::Default))
                .when(!spec.spec.depends_on.is_empty(), |element| {
                    element.child(
                        Label::new(format!("Depends on {}", spec.spec.depends_on.join(", ")))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                })
                .when(spec.blocked, |element| {
                    element.child(
                        Label::new(format!("Blocked by {}", spec.unmet_dependencies.join(", ")))
                            .size(LabelSize::Small)
                            .color(Color::Warning),
                    )
                }),
        )
        .child(
            Chip::new(spec_status_label(spec.spec.status))
                .label_color(spec_status_color(spec.spec.status)),
        )
        .tooltip(Tooltip::text(format!(
            "{}: {}",
            spec.spec.id,
            spec_title(spec)
        )))
        .into_any_element()
}

fn kanban_column(
    title: &'static str,
    board: &SpecBoard,
    status: SpecStatus,
    cx: &mut App,
) -> AnyElement {
    let theme = cx.theme();
    let specs = board
        .specs
        .iter()
        .filter(|spec| spec.spec.status == status)
        .collect::<Vec<_>>();

    v_flex()
        .w(px(280.))
        .min_w(px(260.))
        .gap_2()
        .p_2()
        .rounded_sm()
        .border_1()
        .border_color(theme.colors().border_variant)
        .bg(theme.colors().panel_background)
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .gap_2()
                .child(Label::new(title))
                .child(Chip::new(specs.len().to_string()).label_color(spec_status_color(status))),
        )
        .when(specs.is_empty(), |element| {
            element.child(
                Label::new("No specs")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
        })
        .children(specs.into_iter().map(|spec| kanban_card(spec, cx)))
        .into_any_element()
}

fn kanban_card(spec: &BoardSpecRecord, cx: &mut App) -> AnyElement {
    let theme = cx.theme();
    v_flex()
        .id(format!("sing-project-home-kanban-card-{}", spec.spec.id))
        .w_full()
        .gap_1()
        .p_2()
        .rounded_sm()
        .border_1()
        .border_color(theme.colors().border_variant)
        .bg(theme.colors().editor_background)
        .child(
            Label::new(spec.spec.id.clone())
                .size(LabelSize::XSmall)
                .color(Color::Muted),
        )
        .child(Label::new(spec_title(spec)).size(LabelSize::Small))
        .when(spec.ready, |element| {
            element.child(Chip::new("Ready").label_color(Color::Success))
        })
        .when(spec.blocked, |element| {
            element.child(Chip::new("Blocked").label_color(Color::Warning))
        })
        .tooltip(Tooltip::text(format!(
            "{}: {}",
            spec.spec.id,
            spec_title(spec)
        )))
        .into_any_element()
}

fn spec_title(spec: &BoardSpecRecord) -> String {
    if spec.spec.title.is_empty() {
        spec.spec.id.clone()
    } else {
        spec.spec.title.clone()
    }
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
    if let Some(commits) = status.commits_since_launch {
        parts.push(format!("{commits} commits"));
    }
    parts.join(" | ")
}

fn format_agent_report(report: &AgentReport) -> String {
    let mut parts = vec![format!("Session {}", report.session_status)];
    if let Some(duration) = report.duration.as_deref() {
        parts.push(format!("duration {duration}"));
    }
    if let Some(branch) = report.branch.as_deref() {
        parts.push(format!("branch {branch}"));
    }
    parts.push(format!("{} commits", report.commits_since_launch));
    if report.guardrail_triggered {
        parts.push(
            report
                .guardrail_reason
                .as_ref()
                .map(|reason| format!("guardrail {reason}"))
                .unwrap_or_else(|| "guardrail triggered".to_string()),
        );
    }
    parts.join(" | ")
}

impl EventEmitter<PanelEvent> for SingProjectPanel {}

impl Focusable for SingProjectPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SingProjectPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        v_flex()
            .id("sing-project-panel")
            .track_focus(&self.focus_handle)
            .overflow_hidden()
            .size_full()
            .bg(theme.colors().panel_background)
            .child(self.render_header(cx))
            .when_some(self.render_error_banner(cx), |element, banner| {
                element.child(banner)
            })
            .child(self.render_projects(cx))
    }
}

impl Panel for SingProjectPanel {
    fn persistent_name() -> &'static str {
        "Projects"
    }

    fn panel_key() -> &'static str {
        SING_PROJECT_PANEL_KEY
    }

    fn position(&self, _: &Window, _: &App) -> DockPosition {
        self.position
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(
            position,
            DockPosition::Left | DockPosition::Bottom | DockPosition::Right
        )
    }

    fn set_position(&mut self, position: DockPosition, _: &mut Window, cx: &mut Context<Self>) {
        self.position = position;
        self.serialize(cx);
        cx.notify();
    }

    fn default_size(&self, _: &Window, _: &App) -> Pixels {
        px(320.)
    }

    fn min_size(&self, _: &Window, _: &App) -> Option<Pixels> {
        Some(px(240.))
    }

    fn icon(&self, _: &Window, _: &App) -> Option<IconName> {
        Some(IconName::Server)
    }

    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("Projects")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(ToggleFocus)
    }

    fn icon_label(&self, _: &Window, _: &App) -> Option<String> {
        let running_count = self
            .projects
            .iter()
            .filter(|project| project.can_open())
            .count();
        (running_count > 0).then(|| running_count.to_string())
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
        2
    }
}

fn status_color(status: ProjectStatus) -> Color {
    match status {
        ProjectStatus::Running => Color::Success,
        ProjectStatus::Stopped => Color::Warning,
        ProjectStatus::NotCreated => Color::Muted,
        ProjectStatus::Error => Color::Error,
    }
}

fn contains_query(value: &str, query: &str) -> bool {
    value.to_ascii_lowercase().contains(query)
}
