use db::kvp::KeyValueStore;
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Point, Render,
    SharedString, Styled, Subscription, Task, Window, div, point, px,
};
use project::Project;
use semantic_graph::{
    CanvasPins, EdgeKind, Lens, NodeId, SemanticGraphEvent, canvas_pins_kvp_key,
};
use settings::{Settings, SettingsStore};
use ui::{Color, Label, LabelSize, prelude::*};
use util::{ResultExt as _, TryFutureExt as _};
use workspace::{
    Workspace,
    item::{Item, ItemEvent},
};

/// Screen-space threshold: movement below this is treated as a click (select only, no pin).
const PIN_DRAG_THRESHOLD_PX: f32 = 3.0;

use super::element::SemanticMapCanvasElement;
use super::skins::vibe::VibeSkin;
use crate::{
    CanvasViewModel, SceneNode, SemanticMapSelection, SemanticMapSettings,
};

struct NodeDrag {
    node_id: NodeId,
    start_screen: Point<f32>,
    origin: Point<f32>,
}

pub struct SemanticMapItem {
    project: Entity<Project>,
    selection: Entity<SemanticMapSelection>,
    focus_handle: FocusHandle,
    view_model: CanvasViewModel,
    pins: CanvasPins,
    pins_key: Option<String>,
    pan: Point<f32>,
    zoom: f32,
    panning: Option<Point<f32>>,
    pan_anchor: Point<f32>,
    node_drag: Option<NodeDrag>,
    _pending_pin_save: Task<Option<()>>,
    _subscriptions: Vec<Subscription>,
}

impl SemanticMapItem {
    pub fn new(
        project: Entity<Project>,
        selection: Entity<SemanticMapSelection>,
        pins_key: Option<String>,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let focus_handle = cx.focus_handle();
            let semantic_graph = project.read(cx).semantic_graph().clone();
            let pins = load_pins_from_kvp(pins_key.as_deref(), &project, cx);

            let mut subscriptions = Vec::new();
            subscriptions.push(cx.subscribe(
                &semantic_graph,
                |this: &mut Self, _, event, cx| {
                    if matches!(event, SemanticGraphEvent::Updated { .. }) {
                        this.refresh_view_model(cx);
                    }
                },
            ));
            subscriptions.push(cx.observe(&selection, |_, _, cx| {
                cx.notify();
            }));
            subscriptions.push(cx.observe_global::<SettingsStore>(|this, cx| {
                this.refresh_view_model(cx);
                cx.notify();
            }));

            let mut this = Self {
                project: project.clone(),
                selection,
                focus_handle,
                view_model: CanvasViewModel::default(),
                pins,
                pins_key,
                pan: point(24.0, 24.0),
                zoom: 1.0,
                panning: None,
                pan_anchor: point(0.0, 0.0),
                node_drag: None,
                _pending_pin_save: Task::ready(None),
                _subscriptions: subscriptions,
            };
            this.refresh_view_model(cx);
            this
        })
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
        self.view_model =
            CanvasViewModel::from_snapshot_with_pins(&snapshot, &lens, Some(&self.pins));
        cx.notify();
    }

    fn select_node(&mut self, node_id: NodeId, cx: &mut Context<Self>) {
        self.selection.update(cx, |selection, cx| {
            selection.select([node_id], cx);
        });
        cx.notify();
    }

    fn start_pan(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        if self.node_drag.is_some() {
            return;
        }
        if event.button != MouseButton::Middle && event.button != MouseButton::Left {
            return;
        }
        self.panning = Some(point(event.position.x.into(), event.position.y.into()));
        self.pan_anchor = self.pan;
        cx.notify();
    }

    fn update_pan(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if self.node_drag.is_some() {
            return;
        }
        let Some(start) = self.panning else {
            return;
        };
        let current = point::<f32>(event.position.x.into(), event.position.y.into());
        self.pan = point(
            self.pan_anchor.x + (current.x - start.x),
            self.pan_anchor.y + (current.y - start.y),
        );
        cx.notify();
    }

    fn end_pan(&mut self, cx: &mut Context<Self>) {
        if self.panning.take().is_some() {
            cx.notify();
        }
    }

    fn start_node_drag(&mut self, node_id: NodeId, event: &MouseDownEvent, cx: &mut Context<Self>) {
        let Some(node) = self.view_model.nodes.iter().find(|node| node.id == node_id) else {
            return;
        };
        self.panning = None;
        self.node_drag = Some(NodeDrag {
            node_id,
            start_screen: point(event.position.x.into(), event.position.y.into()),
            origin: point(node.rect.origin.x, node.rect.origin.y),
        });
        self.select_node(node_id, cx);
        cx.notify();
    }

    fn update_node_drag(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(drag) = &self.node_drag else {
            return;
        };
        let current = point::<f32>(event.position.x.into(), event.position.y.into());
        let zoom = if self.zoom.abs() < f32::EPSILON {
            1.0
        } else {
            self.zoom
        };
        let world = point(
            drag.origin.x + (current.x - drag.start_screen.x) / zoom,
            drag.origin.y + (current.y - drag.start_screen.y) / zoom,
        );
        let node_id = drag.node_id;
        if let Some(node) = self
            .view_model
            .nodes
            .iter_mut()
            .find(|node| node.id == node_id)
        {
            node.rect.origin = world;
        }
        // Keep DependsOn polylines roughly aligned while dragging.
        for edge in &mut self.view_model.edges {
            if edge.from != node_id && edge.to != node_id {
                continue;
            }
            let Some(from_node) = self.view_model.nodes.iter().find(|n| n.id == edge.from) else {
                continue;
            };
            let Some(to_node) = self.view_model.nodes.iter().find(|n| n.id == edge.to) else {
                continue;
            };
            edge.routed_path = vec![
                point(
                    from_node.rect.origin.x + from_node.rect.size.width / 2.0,
                    from_node.rect.origin.y + from_node.rect.size.height / 2.0,
                ),
                point(
                    to_node.rect.origin.x + to_node.rect.size.width / 2.0,
                    to_node.rect.origin.y + to_node.rect.size.height / 2.0,
                ),
            ];
        }
        cx.notify();
    }

    fn end_node_drag(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.node_drag.take() else {
            return;
        };
        let Some(node) = self
            .view_model
            .nodes
            .iter()
            .find(|node| node.id == drag.node_id)
        else {
            return;
        };
        let position = (node.rect.origin.x, node.rect.origin.y);
        let zoom = if self.zoom.abs() < f32::EPSILON {
            1.0
        } else {
            self.zoom
        };
        let screen_dx = (position.0 - drag.origin.x) * zoom;
        let screen_dy = (position.1 - drag.origin.y) * zoom;
        // Click (no meaningful move): selection already applied on mouse-down; do not pin.
        if screen_dx * screen_dx + screen_dy * screen_dy
            <= PIN_DRAG_THRESHOLD_PX * PIN_DRAG_THRESHOLD_PX
        {
            self.refresh_view_model(cx);
            return;
        }
        let snapshot = self.project.read(cx).semantic_graph().read(cx).snapshot();
        let Some(key) = snapshot
            .graph
            .nodes
            .get(&drag.node_id)
            .map(|node| node.key.clone())
        else {
            return;
        };
        self.pins.pin(key, position);
        self.persist_pins(cx);
        self.refresh_view_model(cx);
    }

    fn persist_pins(&mut self, cx: &mut Context<Self>) {
        let Some(serialization_key) = self.pins_key.clone() else {
            return;
        };
        let Some(json) = self.pins.to_json().log_err() else {
            return;
        };
        let kvp = KeyValueStore::global(cx);
        self._pending_pin_save = cx.background_spawn(
            async move {
                kvp.write_kvp(serialization_key, json).await?;
                anyhow::Ok(())
            }
            .log_err(),
        );
    }

    fn dependency_names_for_selected(&self, cx: &App) -> Vec<SharedString> {
        let Some(selected) = self.selection.read(cx).selected.first().copied() else {
            return Vec::new();
        };
        let snapshot = self.project.read(cx).semantic_graph().read(cx).snapshot();
        let mut names = Vec::new();
        for edge in snapshot.graph.edges.values() {
            if edge.kind != EdgeKind::DependsOn || edge.from != selected {
                continue;
            }
            if let Some(target) = snapshot.graph.nodes.get(&edge.to) {
                names.push(target.display_name.clone());
            }
        }
        names.sort_by(|a, b| a.as_ref().cmp(b.as_ref()));
        names
    }

    fn render_card(
        &self,
        node: &SceneNode,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let node_id = node.id;
        let title = node.title.clone();
        let subtitle = node.subtitle.clone();
        let left = px(node.rect.origin.x * self.zoom + self.pan.x);
        let top = px(node.rect.origin.y * self.zoom + self.pan.y);
        let width = px(node.rect.size.width * self.zoom);
        let height = px(node.rect.size.height * self.zoom);

        div()
            .id(ElementId::Name(
                format!("semantic-map-card-{node_id:?}").into(),
            ))
            .absolute()
            .left(left)
            .top(top)
            .w(width)
            .h(height)
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(VibeSkin::card_border(selected, cx))
            .bg(VibeSkin::card_background(node.kind, cx))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.start_node_drag(node_id, event, cx);
                }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .size_full()
                    .child(Label::new(title).size(LabelSize::Small))
                    .when_some(subtitle, |this, subtitle| {
                        this.child(
                            Label::new(subtitle)
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                        )
                    }),
            )
    }

    pub fn open_in_workspace(
        workspace: &mut Workspace,
        project: Entity<Project>,
        selection: Entity<SemanticMapSelection>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let pane = workspace.active_pane().clone();
        let existing = pane.read(cx).items_of_type::<SemanticMapItem>().next();
        if let Some(existing) = existing
            && let Some(index) = pane.read(cx).index_for_item(&existing)
        {
            pane.update(cx, |pane, cx| {
                pane.activate_item(index, true, true, window, cx);
            });
            return;
        }

        let pins_key = workspace
            .database_id()
            .map(|id| i64::from(id).to_string())
            .or_else(|| workspace.session_id())
            .map(canvas_pins_kvp_key);
        let item = SemanticMapItem::new(project, selection, pins_key, cx);
        pane.update(cx, |pane, cx| {
            pane.add_item(Box::new(item), true, true, None, window, cx);
        });
    }
}

fn load_pins_from_kvp(
    pins_key: Option<&str>,
    project: &Entity<Project>,
    cx: &App,
) -> CanvasPins {
    let Some(key) = pins_key else {
        return CanvasPins::default();
    };
    let Some(json) = KeyValueStore::global(cx)
        .read_kvp(key)
        .log_err()
        .flatten()
    else {
        return CanvasPins::default();
    };
    let snapshot = project.read(cx).semantic_graph().read(cx).snapshot();
    CanvasPins::from_json(&json, &snapshot.graph)
        .log_err()
        .unwrap_or_default()
}

impl EventEmitter<ItemEvent> for SemanticMapItem {}

impl Focusable for SemanticMapItem {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SemanticMapItem {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.selection.read(cx).selected.clone();
        let dependency_names = self.dependency_names_for_selected(cx);
        let selected_name = selected.first().and_then(|id| {
            self.view_model
                .nodes
                .iter()
                .find(|node| node.id == *id)
                .map(|node| node.title.clone())
        });

        h_flex()
            .id("semantic-map-canvas")
            .key_context("SemanticMapCanvas")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().editor_background)
            .child(
                div()
                    .id("semantic-map-canvas-viewport")
                    .relative()
                    .flex_1()
                    .size_full()
                    .overflow_hidden()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.start_pan(event, cx);
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Middle,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.start_pan(event, cx);
                        }),
                    )
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                        if this.node_drag.is_some() {
                            this.update_node_drag(event, cx);
                        } else {
                            this.update_pan(event, cx);
                        }
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseUpEvent, _, cx| {
                            if this.node_drag.is_some() {
                                this.end_node_drag(cx);
                            } else {
                                this.end_pan(cx);
                            }
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Middle,
                        cx.listener(|this, _: &MouseUpEvent, _, cx| {
                            this.end_pan(cx);
                        }),
                    )
                    .child(SemanticMapCanvasElement::new(
                        self.view_model.edges.clone(),
                        self.pan,
                        self.zoom,
                    ))
                    .children(self.view_model.nodes.iter().map(|node| {
                        let is_selected = selected.contains(&node.id);
                        self.render_card(node, is_selected, cx).into_any_element()
                    })),
            )
            .child(
                v_flex()
                    .id("semantic-map-detail")
                    .w(px(220.))
                    .h_full()
                    .p_3()
                    .gap_2()
                    .border_l_1()
                    .border_color(cx.theme().colors().border)
                    .bg(cx.theme().colors().panel_background)
                    .child(Label::new("Details").size(LabelSize::Small))
                    .map(|this| match selected_name {
                        Some(name) => this.child(Label::new(name).size(LabelSize::Default)),
                        None => this.child(
                            Label::new("Select a node")
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        ),
                    })
                    .child(
                        Label::new("Depends on")
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    )
                    .when(dependency_names.is_empty(), |this| {
                        this.child(
                            Label::new("(none)")
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        )
                    })
                    .children(
                        dependency_names
                            .into_iter()
                            .map(|name| Label::new(name).size(LabelSize::Small).into_any_element()),
                    ),
            )
    }
}

impl Item for SemanticMapItem {
    type Event = ItemEvent;

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        "Semantic Map".into()
    }

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::FileTree))
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("Semantic Map Canvas Opened")
    }

    fn show_toolbar(&self) -> bool {
        false
    }

    fn to_item_events(event: &Self::Event, f: &mut dyn FnMut(ItemEvent)) {
        f(*event)
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;
    use project::FakeFs;
    use serde_json::json;
    use settings::SettingsStore;
    use workspace::MultiWorkspace;

    use super::*;
    use crate::SemanticMapSelection;

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings = SettingsStore::test(cx);
            cx.set_global(settings);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            crate::init(cx);
        });
    }

    #[gpui::test]
    async fn canvas_item_registers_as_workspace_item(cx: &mut TestAppContext) {
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/root", json!({ "src": { "lib.rs": "" } }))
            .await;

        let project = Project::test(fs, ["/root".as_ref()], cx).await;
        cx.update(|cx| {
            cx.update_global::<SettingsStore, _>(|store, cx| {
                store.update_user_settings(cx, |settings| {
                    settings
                        .semantic_map
                        .get_or_insert_default()
                        .enabled = Some(true);
                });
            });
        });

        let window = cx.add_window(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace = window
            .read_with(cx, |mw, _| mw.workspace().clone())
            .unwrap();

        let selection = cx.new(|_cx| SemanticMapSelection::new());
        window
            .update(cx, |_, window, cx| {
                workspace.update(cx, |workspace, cx| {
                    SemanticMapItem::open_in_workspace(
                        workspace,
                        project.clone(),
                        selection,
                        window,
                        cx,
                    );
                })
            })
            .unwrap();

        workspace.read_with(cx, |workspace, cx| {
            let pane = workspace.active_pane().read(cx);
            let count = pane.items_of_type::<SemanticMapItem>().count();
            assert_eq!(count, 1);
            assert!(
                pane.active_item()
                    .and_then(|item| item.downcast::<SemanticMapItem>())
                    .is_some()
            );
        });
    }
}
