use std::ops::Range;
use std::path::Path;
use std::time::Duration;

use gpui::{
    Action, App, AsyncWindowContext, ClickEvent, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement, IntoElement, ParentElement, Pixels, Render, SharedString,
    Styled, Subscription, Task, UniformListScrollHandle, WeakEntity, Window, actions, div, px,
    uniform_list,
};
use project::{PathChange, Project, ProjectPath, worktree_store::WorktreeStoreEvent};
use semantic_graph::{Lens, NodeId, SemanticGraphEvent, SourceLocation};
use settings::{Settings, SettingsStore};
use ui::{Chip, Color, Label, LabelSize, ListItem, Tooltip, prelude::*};
use util::ResultExt as _;
use workspace::{
    Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};

use crate::{PanelRow, PanelStatus, PanelViewModel, SemanticMapSelection, SemanticMapSettings};

actions!(
    semantic_map,
    [
        /// Toggles focus on the Semantic Map panel.
        ToggleFocus,
        /// Opens the source location for the selected semantic map node.
        OpenSelectedSource,
        /// Opens the Semantic Map canvas in the active pane.
        OpenCanvas,
        /// Rebuilds the semantic graph for the current project.
        Reindex,
    ]
);

const PANEL_KEY: &str = "SemanticMapPanel";
const DEFAULT_WIDTH: Pixels = px(260.);
const INTENT_TRUNCATE_CHARS: usize = 72;
const REINDEX_DEBOUNCE: Duration = Duration::from_millis(500);
const IGNORED_PATH_COMPONENTS: &[&str] = &["target", ".git", "node_modules"];

pub struct SemanticMapPanel {
    project: Entity<Project>,
    workspace: WeakEntity<Workspace>,
    selection: Entity<SemanticMapSelection>,
    focus_handle: FocusHandle,
    view_model: PanelViewModel,
    position: DockPosition,
    has_reindexed: bool,
    enabled: bool,
    reindex_debounce_task: Option<Task<()>>,
    scroll_handle: UniformListScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl SemanticMapPanel {
    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> anyhow::Result<Entity<Self>> {
        workspace.update_in(&mut cx, |workspace, window, cx| {
            Self::new(workspace, window, cx)
        })
    }

    pub fn new(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let project = workspace.project().clone();
        let workspace_handle = workspace.weak_handle();

        let panel = cx.new(|cx| {
            let selection = cx.new(|_cx| SemanticMapSelection::new());
            let focus_handle = cx.focus_handle();
            let semantic_graph = project.read(cx).semantic_graph().clone();

            let mut subscriptions = Vec::new();
            subscriptions.push(
                cx.subscribe(&semantic_graph, |this: &mut Self, _, event, cx| {
                    if matches!(event, SemanticGraphEvent::Updated { .. }) {
                        this.refresh_view_model(cx);
                    }
                }),
            );
            // Status moves to Indexing/Error via notify without always emitting Updated.
            subscriptions.push(cx.observe(&semantic_graph, |this, _, cx| {
                this.refresh_view_model(cx);
            }));
            subscriptions.push(cx.observe_global::<SettingsStore>(|this, cx| {
                this.on_settings_changed(cx);
            }));
            let worktree_store = project.read(cx).worktree_store();
            subscriptions.push(cx.subscribe(&worktree_store, |this, _, event, cx| {
                this.on_worktree_store_event(event, cx);
            }));

            let enabled = SemanticMapSettings::get_global(cx).enabled;
            let mut this = Self {
                project: project.clone(),
                workspace: workspace_handle,
                selection,
                focus_handle,
                view_model: PanelViewModel::default(),
                position: DockPosition::Left,
                has_reindexed: false,
                enabled,
                reindex_debounce_task: None,
                scroll_handle: UniformListScrollHandle::new(),
                _subscriptions: subscriptions,
            };

            this.refresh_view_model(cx);
            if enabled {
                this.ensure_indexed(cx);
            }

            this
        });

        let settings = SemanticMapSettings::get_global(cx);
        if settings.enabled && settings.auto_open_canvas_on_project_open {
            panel.update(cx, |panel, cx| {
                panel.open_canvas(&OpenCanvas, window, cx);
            });
        }

        panel
    }

    pub fn selection(&self) -> &Entity<SemanticMapSelection> {
        &self.selection
    }

    pub fn project(&self) -> &Entity<Project> {
        &self.project
    }

    pub fn view_model(&self) -> &PanelViewModel {
        &self.view_model
    }

    fn open_canvas(&mut self, _: &OpenCanvas, window: &mut Window, cx: &mut Context<Self>) {
        if !SemanticMapSettings::get_global(cx).enabled {
            return;
        }
        let project = self.project.clone();
        let selection = self.selection.clone();
        self.workspace
            .update(cx, |workspace, cx| {
                crate::SemanticMapItem::open_in_workspace(
                    workspace, project, selection, window, cx,
                );
            })
            .log_err();
    }

    fn lens_from_settings(settings: &SemanticMapSettings) -> Lens {
        Lens {
            hide_external: settings.hide_external,
            hide_tests: settings.hide_tests,
            max_depth: Some(settings.module_depth as u32),
            ..Lens::default()
        }
    }

    fn refresh_view_model(&mut self, cx: &mut Context<Self>) {
        let settings = SemanticMapSettings::get_global(cx);
        let lens = Self::lens_from_settings(settings);
        let snapshot = self.project.read(cx).semantic_graph().read(cx).snapshot();
        self.view_model = PanelViewModel::from_snapshot(&snapshot, &lens);
        cx.notify();
    }

    fn ensure_indexed(&mut self, cx: &mut Context<Self>) {
        if self.has_reindexed {
            return;
        }
        self.start_reindex(cx);
    }

    fn on_settings_changed(&mut self, cx: &mut Context<Self>) {
        let enabled = SemanticMapSettings::get_global(cx).enabled;
        let was_enabled = self.enabled;
        self.enabled = enabled;
        self.refresh_view_model(cx);
        if !enabled {
            self.reindex_debounce_task.take();
        } else if !was_enabled {
            self.start_reindex(cx);
        }
        cx.notify();
    }

    fn on_worktree_store_event(&mut self, event: &WorktreeStoreEvent, cx: &mut Context<Self>) {
        if !SemanticMapSettings::get_global(cx).enabled {
            return;
        }
        let should_reindex = match event {
            WorktreeStoreEvent::WorktreeUpdatedEntries(_, changes) => {
                changes.iter().any(|(path, _, change)| {
                    should_reindex_worktree_change(path.as_std_path(), *change)
                })
            }
            WorktreeStoreEvent::WorktreeAdded(_) => true,
            _ => false,
        };
        if should_reindex {
            self.schedule_reindex(cx);
        }
    }

    fn schedule_reindex(&mut self, cx: &mut Context<Self>) {
        if !SemanticMapSettings::get_global(cx).enabled {
            return;
        }
        self.reindex_debounce_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(REINDEX_DEBOUNCE).await;
            this.update(cx, |this, cx| {
                this.start_reindex(cx);
            })
            .log_err();
        }));
    }

    fn start_reindex(&mut self, cx: &mut Context<Self>) {
        if !SemanticMapSettings::get_global(cx).enabled {
            return;
        }
        self.reindex_debounce_task.take();
        let options = Self::build_options_from_settings(SemanticMapSettings::get_global(cx));
        self.project.update(cx, |project, cx| {
            project.reindex_semantic_graph(options, cx);
        });
        self.has_reindexed = true;
    }

    fn build_options_from_settings(
        settings: &SemanticMapSettings,
    ) -> semantic_graph::BuildGraphOptions {
        semantic_graph::BuildGraphOptions {
            max_auto_nodes: settings.max_auto_nodes,
            module_depth: settings.module_depth as u32,
            cluster: semantic_graph::ClusterConfig {
                min_subsystems: settings.cluster.min_subsystems,
                max_subsystems: settings.cluster.max_subsystems,
            },
            intent_llm: settings.intent.llm,
        }
    }

    fn select_node(&mut self, node_id: NodeId, cx: &mut Context<Self>) {
        self.selection.update(cx, |selection, cx| {
            selection.select([node_id], cx);
        });
        cx.notify();
    }

    fn open_node_source(&self, node_id: NodeId, window: &mut Window, cx: &mut Context<Self>) {
        open_node_source(&self.workspace, &self.project, node_id, window, cx);
    }

    fn open_selected_source(
        &mut self,
        _: &OpenSelectedSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(node_id) = self.selection.read(cx).selected.first().copied() else {
            return;
        };
        open_node_source(&self.workspace, &self.project, node_id, window, cx);
    }

    fn reindex(&mut self, _: &Reindex, _window: &mut Window, cx: &mut Context<Self>) {
        self.start_reindex(cx);
    }

    fn render_row(
        &self,
        row: &PanelRow,
        selected: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let node_id = row.node_id;
        let name = row.name.clone();
        let intent = row
            .intent_summary
            .as_ref()
            .map(|summary| truncate_intent(summary));

        ListItem::new(ElementId::Name(
            format!("semantic-map-row-{node_id:?}").into(),
        ))
        .selectable(true)
        .toggle_state(selected)
        .indent_level(row.depth as usize)
        .indent_step_size(px(12.))
        .on_click({
            let panel = cx.weak_entity();
            move |event: &ClickEvent, window, cx| {
                panel
                    .update(cx, |this, cx| {
                        this.select_node(node_id, cx);
                        if event.click_count() > 1 {
                            this.open_node_source(node_id, window, cx);
                        }
                    })
                    .log_err();
            }
        })
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .min_w_0()
                .child(Label::new(name).size(LabelSize::Small))
                .when_some(intent, |this, intent| {
                    this.child(
                        Label::new(intent)
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    )
                }),
        )
    }
}

fn truncate_intent(summary: &SharedString) -> SharedString {
    let truncated: String = summary.chars().take(INTENT_TRUNCATE_CHARS).collect();
    if summary.chars().count() > INTENT_TRUNCATE_CHARS {
        format!("{truncated}…").into()
    } else {
        truncated.into()
    }
}

fn status_chip(status: &PanelStatus) -> Chip {
    let label = status.chip_label();
    let label_color = match status {
        PanelStatus::Ready => Color::Created,
        PanelStatus::Indexing => Color::Accent,
        PanelStatus::Partial { .. } => Color::Warning,
        PanelStatus::Error { .. } => Color::Error,
    };
    let tooltip_text = match status {
        PanelStatus::Partial { reason } => Some(reason.clone()),
        PanelStatus::Error { message } => Some(message.clone()),
        PanelStatus::Ready | PanelStatus::Indexing => None,
    };
    let chip = Chip::new(label).label_color(label_color);
    match tooltip_text {
        Some(text) => chip.tooltip(Tooltip::text(text)),
        None => chip,
    }
}

/// Empty-state copy is only for a Ready panel with no rows — not Indexing/Partial/Error.
fn shows_empty_nodes_copy(enabled: bool, row_count: usize, status: &PanelStatus) -> bool {
    enabled && row_count == 0 && matches!(status, PanelStatus::Ready)
}

fn path_has_ignored_component(path: &Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|name| IGNORED_PATH_COMPONENTS.contains(&name))
    })
}

fn is_rust_under_src(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("rs")
        && path
            .components()
            .any(|component| component.as_os_str() == "src")
}

fn is_crate_root_readme(path: &Path) -> bool {
    let is_readme = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("README.md"));
    if !is_readme {
        return false;
    }
    match path.parent() {
        None => true,
        Some(parent) if parent.as_os_str().is_empty() => true,
        Some(parent) => !parent.components().any(|component| {
            matches!(
                component.as_os_str().to_str(),
                Some("src" | "target" | "tests" | ".git")
            )
        }),
    }
}

/// Returns whether a changed path should trigger a semantic-graph rebuild.
fn should_reindex_path(path: &Path) -> bool {
    if path_has_ignored_component(path) {
        return false;
    }

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if file_name.eq_ignore_ascii_case("Cargo.toml")
        || file_name.eq_ignore_ascii_case("semantic_map.toml")
        || file_name.eq_ignore_ascii_case("build.rs")
    {
        return true;
    }
    if is_crate_root_readme(path) || is_rust_under_src(path) {
        return true;
    }

    // Added/removed directories typically have no extension.
    !file_name.is_empty() && path.extension().is_none()
}

fn should_reindex_worktree_change(path: &Path, change: PathChange) -> bool {
    if matches!(change, PathChange::Loaded) {
        return false;
    }
    should_reindex_path(path)
}

fn project_path_from_source_location(location: &SourceLocation) -> ProjectPath {
    ProjectPath {
        worktree_id: location.worktree_id,
        path: location.path.clone(),
    }
}

fn open_node_source(
    workspace: &WeakEntity<Workspace>,
    project: &Entity<Project>,
    node_id: NodeId,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(location) = project
        .read(cx)
        .semantic_graph()
        .read(cx)
        .snapshot()
        .graph
        .nodes
        .get(&node_id)
        .and_then(|node| node.location.clone())
    else {
        return;
    };

    let project_path = project_path_from_source_location(&location);
    workspace
        .update(cx, |workspace, cx| {
            workspace
                .open_path(project_path, None, true, window, cx)
                .detach_and_log_err(cx);
        })
        .log_err();
}

impl EventEmitter<PanelEvent> for SemanticMapPanel {}

impl Focusable for SemanticMapPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for SemanticMapPanel {
    fn persistent_name() -> &'static str {
        "Semantic Map"
    }

    fn panel_key() -> &'static str {
        PANEL_KEY
    }

    fn position(&self, _window: &Window, _cx: &App) -> DockPosition {
        self.position
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
    }

    fn set_position(
        &mut self,
        position: DockPosition,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.position = position;
        cx.notify();
    }

    fn default_size(&self, _window: &Window, _cx: &App) -> Pixels {
        DEFAULT_WIDTH
    }

    fn icon(&self, _window: &Window, cx: &App) -> Option<IconName> {
        SemanticMapSettings::get_global(cx)
            .enabled
            .then_some(IconName::FileTree)
    }

    fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
        Some("Semantic Map")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(ToggleFocus)
    }

    fn set_active(&mut self, active: bool, _window: &mut Window, cx: &mut Context<Self>) {
        if active {
            self.ensure_indexed(cx);
        }
    }

    fn activation_priority(&self) -> u32 {
        8
    }

    fn enabled(&self, cx: &App) -> bool {
        SemanticMapSettings::get_global(cx).enabled
    }
}

impl Render for SemanticMapPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let row_count = self.view_model.rows.len();
        let enabled = SemanticMapSettings::get_global(cx).enabled;

        v_flex()
            .id("semantic-map-panel")
            .key_context("SemanticMapPanel")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().panel_background)
            .on_action(cx.listener(Self::open_selected_source))
            .on_action(cx.listener(Self::open_canvas))
            .on_action(cx.listener(Self::reindex))
            .when(!enabled, |this| {
                this.child(
                    div().p_3().child(
                        Label::new("Enable semantic_map in settings to use this panel.")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
                )
            })
            .when(enabled, |this| {
                let status = &self.view_model.status;
                let chip = status_chip(status);
                this.child(
                    h_flex()
                        .w_full()
                        .px_2()
                        .py_1()
                        .gap_2()
                        .justify_between()
                        .child(chip)
                        .child(
                            h_flex()
                                .gap_1()
                                .child(
                                    Button::new("semantic-map-reindex", "Reindex")
                                        .label_size(LabelSize::Small)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.reindex(&Reindex, window, cx);
                                        })),
                                )
                                .child(
                                    Button::new("open-semantic-map-canvas", "Open Canvas")
                                        .label_size(LabelSize::Small)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.open_canvas(&OpenCanvas, window, cx);
                                        })),
                                ),
                        ),
                )
            })
            .when(
                shows_empty_nodes_copy(enabled, row_count, &self.view_model.status),
                |this| {
                    this.child(
                        div().p_3().child(
                            Label::new("No semantic map nodes yet. Focus the panel to index.")
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        ),
                    )
                },
            )
            .when(enabled, |this| {
                this.child(
                    uniform_list(
                        "semantic-map-rows",
                        row_count,
                        cx.processor(move |this, range: Range<usize>, window, cx| {
                            let selected = this.selection.read(cx).selected.clone();
                            range
                                .filter_map(|index| {
                                    let row = this.view_model.rows.get(index)?;
                                    let is_selected = selected.contains(&row.node_id);
                                    Some(
                                        this.render_row(row, is_selected, window, cx)
                                            .into_any_element(),
                                    )
                                })
                                .collect()
                        }),
                    )
                    .size_full()
                    .track_scroll(&self.scroll_handle),
                )
            })
    }
}

pub fn register_panel_actions(
    workspace: &mut Workspace,
    _: Option<&mut Window>,
    _: &mut Context<Workspace>,
) {
    workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
        if !SemanticMapSettings::get_global(cx).enabled {
            return;
        }
        workspace.toggle_panel_focus::<SemanticMapPanel>(window, cx);
    });
    workspace.register_action(|workspace, _: &OpenSelectedSource, window, cx| {
        let Some(panel) = workspace.panel::<SemanticMapPanel>(cx) else {
            return;
        };
        panel.update(cx, |panel, cx| {
            panel.open_selected_source(&OpenSelectedSource, window, cx);
        });
    });
    workspace.register_action(|workspace, _: &OpenCanvas, window, cx| {
        let Some(panel) = workspace.panel::<SemanticMapPanel>(cx) else {
            return;
        };
        panel.update(cx, |panel, cx| {
            panel.open_canvas(&OpenCanvas, window, cx);
        });
    });
    workspace.register_action(|workspace, _: &Reindex, window, cx| {
        if let Some(panel) = workspace.panel::<SemanticMapPanel>(cx) {
            panel.update(cx, |panel, cx| {
                panel.reindex(&Reindex, window, cx);
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::TestAppContext;
    use pretty_assertions::assert_eq;
    use project::FakeFs;
    use semantic_graph::{
        GraphPatch, GraphRevision, GraphStatus, IntentIndex, Node, NodeFlags, NodeId, NodeKey,
        NodeKind, SemanticGraph, SemanticGraphSnapshot, SubsystemPayload,
    };
    use serde_json::json;
    use settings::SettingsStore;
    use workspace::MultiWorkspace;
    use worktree::WorktreeId;

    use super::*;

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings = SettingsStore::test(cx);
            cx.set_global(settings);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            crate::init(cx);
        });
    }

    fn enable_semantic_map(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.update_global::<SettingsStore, _>(|store, cx| {
                store.update_user_settings(cx, |settings| {
                    settings.semantic_map.get_or_insert_default().enabled = Some(true);
                });
            });
        });
    }

    fn graph_has_node(project: &Entity<Project>, node_id: NodeId, cx: &TestAppContext) -> bool {
        project.read_with(cx, |project, cx| {
            project
                .semantic_graph()
                .read(cx)
                .snapshot()
                .graph
                .nodes
                .contains_key(&node_id)
        })
    }

    fn apply_stub_graph(
        project: &Entity<Project>,
        snapshot: &SemanticGraphSnapshot,
        cx: &mut TestAppContext,
    ) {
        project.update(cx, |project, cx| {
            project.semantic_graph().update(cx, |store, cx| {
                store.replace_graph((*snapshot.graph).clone(), (*snapshot.intents).clone(), cx);
            });
        });
    }

    async fn test_project_and_panel(
        cx: &mut TestAppContext,
    ) -> (Entity<Project>, Entity<SemanticMapPanel>) {
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/root", json!({ "src": { "lib.rs": "" } }))
            .await;
        let project = Project::test(fs, ["/root".as_ref()], cx).await;
        let window =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace = window
            .read_with(cx, |mw, _| mw.workspace().clone())
            .unwrap();
        let panel = window
            .update(cx, |_, window, cx| {
                workspace.update(cx, |workspace, cx| {
                    SemanticMapPanel::new(workspace, window, cx)
                })
            })
            .unwrap();
        (project, panel)
    }

    fn stub_snapshot() -> (SemanticGraphSnapshot, NodeId, NodeId) {
        let worktree_id = WorktreeId::from_usize(1);
        let project_key = NodeKey::Project { worktree_id };
        let project_id = NodeId::from_key(&project_key);
        let project = Node::project(project_id, project_key.clone(), "demo");

        let subsystem_key = NodeKey::Subsystem {
            project: project_key.into(),
            slug: "core".into(),
        };
        let subsystem_id = NodeId::from_key(&subsystem_key);
        let subsystem = Node::subsystem(
            subsystem_id,
            subsystem_key,
            "core",
            SubsystemPayload {
                member_count: 0,
                cluster_score: 1.0,
                pinned: false,
            },
            NodeFlags::default(),
        );

        let mut graph = SemanticGraph::default();
        graph
            .apply_patch(GraphPatch {
                base: GraphRevision(0),
                removed_nodes: Vec::new(),
                removed_edges: Vec::new(),
                upsert_nodes: vec![project, subsystem],
                upsert_edges: vec![semantic_graph::Edge::contains(project_id, subsystem_id)],
            })
            .expect("patch applies");

        let snapshot = SemanticGraphSnapshot {
            revision: graph.revision(),
            graph: Arc::new(graph),
            intents: Arc::new(IntentIndex::default()),
            status: GraphStatus::Idle,
        };
        (snapshot, project_id, subsystem_id)
    }

    #[gpui::test]
    async fn panel_builds_rows_from_stub_store(cx: &mut TestAppContext) {
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/root", json!({ "src": { "lib.rs": "" } }))
            .await;

        let project = Project::test(fs, ["/root".as_ref()], cx).await;
        let (snapshot, project_id, subsystem_id) = stub_snapshot();
        project.update(cx, |project, cx| {
            project.semantic_graph().update(cx, |store, cx| {
                store.replace_graph((*snapshot.graph).clone(), (*snapshot.intents).clone(), cx);
            });
        });

        let window =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace = window
            .read_with(cx, |mw, _| mw.workspace().clone())
            .unwrap();

        // Create while disabled so ensure_indexed does not clear the stub via reindex.
        let panel = window
            .update(cx, |_, window, cx| {
                workspace.update(cx, |workspace, cx| {
                    SemanticMapPanel::new(workspace, window, cx)
                })
            })
            .unwrap();

        enable_semantic_map(cx);
        apply_stub_graph(&project, &snapshot, cx);

        panel.read_with(cx, |panel, _| {
            let ids: Vec<_> = panel
                .view_model()
                .rows
                .iter()
                .map(|row| row.node_id)
                .collect();
            assert_eq!(ids, vec![project_id, subsystem_id]);
            assert_eq!(panel.view_model().rows[0].kind, NodeKind::Project);
            assert_eq!(panel.view_model().rows[1].kind, NodeKind::Subsystem);
            assert_eq!(panel.view_model().rows[1].depth, 1);
            assert_eq!(panel.view_model().status, PanelStatus::Ready);
        });
    }

    #[test]
    fn truncates_long_intent_summaries() {
        let long: SharedString = "a".repeat(100).into();
        let truncated = truncate_intent(&long);
        assert!(truncated.ends_with('…'));
        assert_eq!(truncated.chars().count(), INTENT_TRUNCATE_CHARS + 1);
    }

    #[test]
    fn empty_nodes_copy_only_when_ready() {
        assert!(shows_empty_nodes_copy(true, 0, &PanelStatus::Ready));
        assert!(!shows_empty_nodes_copy(true, 0, &PanelStatus::Indexing));
        assert!(!shows_empty_nodes_copy(
            true,
            0,
            &PanelStatus::Partial {
                reason: "truncated".into(),
            },
        ));
        assert!(!shows_empty_nodes_copy(
            true,
            0,
            &PanelStatus::Error {
                message: "boom".into(),
            },
        ));
        assert!(!shows_empty_nodes_copy(true, 1, &PanelStatus::Ready));
        assert!(!shows_empty_nodes_copy(false, 0, &PanelStatus::Ready));
    }

    #[test]
    fn maps_source_location_to_project_path() {
        use util::rel_path::RelPath;

        let worktree_id = WorktreeId::from_usize(7);
        let path = RelPath::from_unix_str("src/lib.rs").expect("valid unix path");
        let location = SourceLocation {
            worktree_id,
            path: Arc::from(path),
            range: None,
            symbol: None,
        };

        let project_path = project_path_from_source_location(&location);
        assert_eq!(project_path.worktree_id, worktree_id);
        assert_eq!(project_path.path.as_ref(), location.path.as_ref());
    }

    #[test]
    fn should_reindex_path_classifies_structural_changes() {
        use std::path::Path;

        assert!(should_reindex_path(Path::new("Cargo.toml")));
        assert!(should_reindex_path(Path::new("crates/foo/Cargo.toml")));
        assert!(should_reindex_path(Path::new("semantic_map.toml")));
        assert!(should_reindex_path(Path::new(
            "crates/foo/semantic_map.toml"
        )));
        assert!(should_reindex_path(Path::new("build.rs")));
        assert!(should_reindex_path(Path::new("src/lib.rs")));
        assert!(should_reindex_path(Path::new("crates/foo/src/main.rs")));
        assert!(should_reindex_path(Path::new("README.md")));
        assert!(should_reindex_path(Path::new("crates/foo/README.md")));
        assert!(should_reindex_path(Path::new("src")));
        assert!(should_reindex_path(Path::new("crates/new_crate")));

        assert!(!should_reindex_path(Path::new("notes.md")));
        assert!(!should_reindex_path(Path::new("src/README.md")));
        assert!(!should_reindex_path(Path::new("docs/guide.md")));
        assert!(!should_reindex_path(Path::new("tests/it.rs")));
        assert!(!should_reindex_path(Path::new("target/debug")));
        assert!(!should_reindex_path(Path::new("target/foo.rs")));
        assert!(!should_reindex_path(Path::new(".git/config")));
        assert!(!should_reindex_path(Path::new(
            "node_modules/pkg/src/lib.rs"
        )));
    }

    #[test]
    fn should_reindex_worktree_change_skips_initial_scan() {
        use std::path::Path;

        assert!(!should_reindex_worktree_change(
            Path::new("Cargo.toml"),
            PathChange::Loaded
        ));
        assert!(should_reindex_worktree_change(
            Path::new("Cargo.toml"),
            PathChange::Updated
        ));
        assert!(should_reindex_worktree_change(
            Path::new("src/lib.rs"),
            PathChange::Added
        ));
        assert!(!should_reindex_worktree_change(
            Path::new("src/lib.rs"),
            PathChange::Loaded
        ));
        assert!(!should_reindex_worktree_change(
            Path::new("notes.md"),
            PathChange::Updated
        ));
    }

    #[gpui::test]
    async fn enabling_settings_triggers_index(cx: &mut TestAppContext) {
        init_test(cx);
        let (_project, panel) = test_project_and_panel(cx).await;

        panel.read_with(cx, |panel, _| {
            assert!(!panel.has_reindexed);
            assert!(!panel.enabled);
        });

        enable_semantic_map(cx);

        panel.read_with(cx, |panel, _| {
            assert!(panel.enabled);
            assert!(panel.has_reindexed);
        });
    }

    #[gpui::test]
    async fn schedule_reindex_is_a_no_op_when_disabled(cx: &mut TestAppContext) {
        init_test(cx);
        let (_project, panel) = test_project_and_panel(cx).await;

        panel.update(cx, |panel, cx| {
            panel.schedule_reindex(cx);
            assert!(panel.reindex_debounce_task.is_none());
            assert!(!panel.has_reindexed);
        });
    }

    #[gpui::test]
    async fn schedule_reindex_debounces_and_cancels_prior_task(cx: &mut TestAppContext) {
        init_test(cx);
        let (project, panel) = test_project_and_panel(cx).await;
        enable_semantic_map(cx);
        cx.run_until_parked();

        let (snapshot, _, subsystem_id) = stub_snapshot();
        apply_stub_graph(&project, &snapshot, cx);

        panel.update(cx, |panel, cx| {
            panel.schedule_reindex(cx);
            assert!(panel.reindex_debounce_task.is_some());
        });

        cx.executor().advance_clock(Duration::from_millis(200));
        assert!(
            graph_has_node(&project, subsystem_id, cx),
            "debounce should not rebuild after 200ms"
        );

        panel.update(cx, |panel, cx| {
            panel.schedule_reindex(cx);
            assert!(panel.reindex_debounce_task.is_some());
        });

        cx.executor().advance_clock(Duration::from_millis(400));
        assert!(
            graph_has_node(&project, subsystem_id, cx),
            "second schedule should reset the debounce window"
        );

        cx.executor().advance_clock(Duration::from_millis(200));
        cx.run_until_parked();

        panel.read_with(cx, |panel, _| {
            assert!(panel.reindex_debounce_task.is_none());
            assert!(panel.has_reindexed);
        });
        assert!(
            !graph_has_node(&project, subsystem_id, cx),
            "debounced reindex should replace the stub graph"
        );
    }
}
