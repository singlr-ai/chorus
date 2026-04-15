use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use db::kvp::KeyValueStore;
use gpui::{
    Action, AnyElement, App, AsyncWindowContext, Context, Entity, EventEmitter, FocusHandle,
    Focusable, ParentElement, Pixels, Render, StatefulInteractiveElement, Styled, Task, WeakEntity,
    Window, actions, px,
};
use recent_projects::open_remote_project;
use serde::{Deserialize, Serialize};
use sing_bridge::ProjectStatus;
use ui::{
    Button, Color, Icon, IconName, IconSize, Indicator, Label, LabelSize, ListItem,
    ListItemSpacing, Tooltip, prelude::*,
};
use util::{ResultExt, TryFutureExt};
use workspace::{
    MultiWorkspace, OpenOptions, Toast, Workspace,
    dock::{DockPosition, Panel, PanelEvent},
    notifications::NotificationId,
};

use crate::client::{DefaultSingProjectClientFactory, SingProjectClient, SingProjectClientFactory};
use crate::state::{ProjectActionKind, ProjectRow, load_project_rows, next_selection};

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
    focus_handle: FocusHandle,
    client_factory: Arc<dyn SingProjectClientFactory>,
    client: Option<Arc<dyn SingProjectClient>>,
    position: DockPosition,
    active: bool,
    loading: bool,
    last_error: Option<String>,
    projects: Vec<ProjectRow>,
    selected_project: Option<String>,
    pending_actions: HashMap<String, ProjectActionKind>,
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
                    .context("loading sing project panel")
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
            let panel = Self::new(workspace, serialized.as_ref(), client_factory, cx);
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
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let workspace = workspace.weak_handle();
        let position = serialized
            .and_then(|panel| panel.position)
            .map(SerializedDockPosition::to_dock_position)
            .unwrap_or(DockPosition::Left);

        cx.new(|cx| Self {
            workspace,
            focus_handle: cx.focus_handle(),
            client_factory,
            client: None,
            position,
            active: serialized.and_then(|panel| panel.active).unwrap_or(false),
            loading: false,
            last_error: None,
            projects: Vec::new(),
            selected_project: serialized.and_then(|panel| panel.selected_project.clone()),
            pending_actions: HashMap::default(),
            current_request_id: 0,
            pending_serialization: Task::ready(None),
            polling_task: Task::ready(()),
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
        let Some(serialization_key) = self
            .workspace
            .read_with(cx, |workspace, _| Self::serialization_key(workspace))
            .ok()
            .flatten()
        else {
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
                self.projects = projects;
                self.selected_project =
                    next_selection(self.selected_project.as_deref(), &self.projects);
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

    fn select_project(&mut self, project: &str, cx: &mut Context<Self>) {
        if self.selected_project.as_deref() == Some(project) {
            return;
        }

        self.selected_project = Some(project.to_string());
        self.serialize(cx);
        cx.notify();
    }

    fn open_project(&mut self, project: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending_actions.contains_key(&project) {
            return;
        }

        self.pending_actions
            .insert(project.clone(), ProjectActionKind::Open);
        cx.notify();

        let workspace = self.workspace.clone();
        cx.spawn_in(window, async move |panel, cx| {
            let result = async {
                let client = panel.update_in(cx, |panel, _, _| panel.ensure_client())??;
                let target = client.project_remote_target(&project).await?;
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
                    vec![target.workspace_root],
                    app_state,
                    open_options,
                    cx,
                )
                .await?;
                Ok(String::new())
            }
            .await;

            panel
                .update_in(cx, |panel, window, cx| {
                    panel.finish_action(&project, ProjectActionKind::Open, result, window, cx);
                })
                .ok();
        })
        .detach();
    }

    fn start_project(&mut self, project: String, window: &mut Window, cx: &mut Context<Self>) {
        self.run_project_action(project, ProjectActionKind::Start, window, cx);
    }

    fn stop_project(&mut self, project: String, window: &mut Window, cx: &mut Context<Self>) {
        self.run_project_action(project, ProjectActionKind::Stop, window, cx);
    }

    fn run_project_action(
        &mut self,
        project: String,
        action: ProjectActionKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending_actions.contains_key(&project) {
            return;
        }

        self.pending_actions.insert(project.clone(), action);
        cx.notify();

        cx.spawn_in(window, async move |panel, cx| {
            let result = async {
                let client = panel.update_in(cx, |panel, _, _| panel.ensure_client())??;
                match action {
                    ProjectActionKind::Start => {
                        let result = client.start_project(&project).await?;
                        Ok(format!("Started {}", result.name))
                    }
                    ProjectActionKind::Stop => {
                        client.stop_project(&project).await?;
                        Ok(format!("Stopped {project}"))
                    }
                    ProjectActionKind::Open => Ok(String::new()),
                }
            }
            .await;

            panel
                .update_in(cx, |panel, window, cx| {
                    panel.finish_action(&project, action, result, window, cx);
                })
                .ok();
        })
        .detach();
    }

    fn finish_action(
        &mut self,
        project: &str,
        action: ProjectActionKind,
        result: Result<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending_actions.remove(project);

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
                self.show_action_error(error.to_string(), cx);
            }
        }
    }

    fn show_agent_status(&mut self, project: &str, cx: &mut Context<Self>) {
        let Some(project) = self.projects.iter().find(|row| row.name == project) else {
            return;
        };

        self.show_toast(format!("{} | {}", project.name, project.agent_detail()), cx);
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
                        NotificationId::composite::<SingProjectPanel>("sing-project-panel"),
                        message.clone(),
                    ),
                    cx,
                );
            });
        }
    }

    fn selected_row(&self) -> Option<&ProjectRow> {
        let selected_project = self.selected_project.as_deref()?;
        self.projects
            .iter()
            .find(|project| project.name == selected_project)
    }

    fn is_action_pending(&self, project: &str, action: ProjectActionKind) -> bool {
        self.pending_actions.get(project).copied() == Some(action)
    }

    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .gap_2()
            .p_2()
            .border_b_1()
            .border_color(theme.colors().border_variant)
            .bg(theme.colors().editor_background)
            .child(
                v_flex()
                    .gap_0p5()
                    .child(Label::new("Chorus Projects"))
                    .child(
                        Label::new(if self.loading {
                            "Refreshing sing project state"
                        } else {
                            "Project lifecycle and remote open"
                        })
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                    ),
            )
            .child(
                Button::new("sing-project-refresh", "Refresh")
                    .label_size(LabelSize::Small)
                    .loading(self.loading)
                    .tooltip(Tooltip::text("Refresh sing project state"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.refresh(window, cx);
                    })),
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
                .child(Icon::new(IconName::RotateCw).color(Color::Muted))
                .child(
                    Label::new("Loading sing projects")
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
                    Label::new("No sing projects found")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .into_any_element();
        }

        let selected_project = self.selected_project.as_deref();
        let items = self
            .projects
            .iter()
            .map(|project| {
                let project_name = project.name.clone();
                let status_color = status_color(project.status);
                ListItem::new(format!("sing-project-row-{project_name}"))
                    .inset(true)
                    .spacing(ListItemSpacing::Sparse)
                    .toggle_state(selected_project == Some(project_name.as_str()))
                    .start_slot(Indicator::dot().color(status_color))
                    .child(
                        v_flex()
                            .w_full()
                            .gap_1()
                            .child(
                                h_flex()
                                    .w_full()
                                    .justify_between()
                                    .gap_2()
                                    .child(Label::new(project.name.clone()).truncate())
                                    .child(
                                        Label::new(project.status_label())
                                            .size(LabelSize::XSmall)
                                            .color(status_color),
                                    ),
                            )
                            .child(
                                Label::new(project.agent_summary())
                                    .size(LabelSize::Small)
                                    .color(Color::Muted)
                                    .truncate(),
                            )
                            .child(
                                Label::new(project.spec_summary())
                                    .size(LabelSize::Small)
                                    .color(Color::Muted)
                                    .truncate(),
                            ),
                    )
                    .tooltip(Tooltip::text(project.name.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_project(&project_name, cx);
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

    fn render_selected_project(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let project = self.selected_row()?;
        let theme = cx.theme();
        let open_pending = self.is_action_pending(&project.name, ProjectActionKind::Open);
        let start_pending = self.is_action_pending(&project.name, ProjectActionKind::Start);
        let stop_pending = self.is_action_pending(&project.name, ProjectActionKind::Stop);
        let runtime_summary = project
            .runtime_summary()
            .unwrap_or_else(|| "Runtime metadata unavailable".to_string());
        let detail_error = project.detail_error.clone();
        let project_name = project.name.clone();
        let agent_project_name = project.name.clone();
        let can_open = project.can_open();
        let can_start = project.can_start();
        let can_stop = project.can_stop();

        Some(
            v_flex()
                .w_full()
                .gap_2()
                .p_2()
                .border_t_1()
                .border_color(theme.colors().border_variant)
                .bg(theme.colors().editor_background)
                .child(Label::new(project.name.clone()))
                .when_some(project.description.as_ref(), |element, description| {
                    element.child(
                        Label::new(description.clone())
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                })
                .child(
                    Label::new(runtime_summary)
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .truncate(),
                )
                .child(
                    Label::new(project.agent_detail())
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .truncate(),
                )
                .child(
                    Label::new(project.spec_detail())
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .truncate(),
                )
                .when_some(project.ip.as_ref(), |element, ip| {
                    element.child(
                        Label::new(format!("Container IP {ip}"))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                })
                .when_some(detail_error.as_ref(), |element, error| {
                    element.child(
                        Label::new(error.clone())
                            .size(LabelSize::Small)
                            .color(Color::Warning)
                            .truncate(),
                    )
                })
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .child(
                            Button::new(
                                format!("sing-project-open-{}", project.name),
                                "Open remote",
                            )
                            .label_size(LabelSize::Small)
                            .loading(open_pending)
                            .disabled(!can_open || start_pending || stop_pending)
                            .tooltip(Tooltip::text("Open this project in a remote workspace"))
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.open_project(project_name.clone(), window, cx);
                                },
                            )),
                        )
                        .when(can_start, |element| {
                            let project_name = project.name.clone();
                            element.child(
                                Button::new(
                                    format!("sing-project-start-{}", project.name),
                                    "Start",
                                )
                                .label_size(LabelSize::Small)
                                .loading(start_pending)
                                .disabled(open_pending || stop_pending)
                                .tooltip(Tooltip::text("Run sing up for this project"))
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.start_project(project_name.clone(), window, cx);
                                    },
                                )),
                            )
                        })
                        .when(can_stop, |element| {
                            let project_name = project.name.clone();
                            element.child(
                                Button::new(format!("sing-project-stop-{}", project.name), "Stop")
                                    .label_size(LabelSize::Small)
                                    .loading(stop_pending)
                                    .disabled(open_pending || start_pending)
                                    .tooltip(Tooltip::text("Run sing down for this project"))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.stop_project(project_name.clone(), window, cx);
                                    })),
                            )
                        })
                        .child(
                            Button::new(
                                format!("sing-project-agent-{}", project.name),
                                "Agent status",
                            )
                            .label_size(LabelSize::Small)
                            .tooltip(Tooltip::text("Show current agent status"))
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.show_agent_status(&agent_project_name, cx);
                                },
                            )),
                        ),
                )
                .into_any_element(),
        )
    }
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
            .when_some(self.render_selected_project(cx), |element, details| {
                element.child(details)
            })
    }
}

impl Panel for SingProjectPanel {
    fn persistent_name() -> &'static str {
        "Chorus Projects"
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
        Some("Chorus Projects")
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
