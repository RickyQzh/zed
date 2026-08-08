use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Point, Render,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Window, div, point, px,
};
use project::Project;
use semantic_graph::{EdgeKind, Lens, NodeId, SemanticGraphEvent};
use settings::{Settings, SettingsStore};
use ui::{Color, Label, LabelSize, prelude::*};
use workspace::{
    Workspace,
    item::{Item, ItemEvent},
};

use super::element::SemanticMapCanvasElement;
use super::skins::vibe::VibeSkin;
use crate::{
    CanvasViewModel, SceneNode, SemanticMapSelection, SemanticMapSettings,
};

pub struct SemanticMapItem {
    project: Entity<Project>,
    selection: Entity<SemanticMapSelection>,
    focus_handle: FocusHandle,
    view_model: CanvasViewModel,
    pan: Point<f32>,
    zoom: f32,
    panning: Option<Point<f32>>,
    pan_anchor: Point<f32>,
    _subscriptions: Vec<Subscription>,
}

impl SemanticMapItem {
    pub fn new(
        project: Entity<Project>,
        selection: Entity<SemanticMapSelection>,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let focus_handle = cx.focus_handle();
            let semantic_graph = project.read(cx).semantic_graph().clone();

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
                pan: point(24.0, 24.0),
                zoom: 1.0,
                panning: None,
                pan_anchor: point(0.0, 0.0),
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
        self.view_model = CanvasViewModel::from_snapshot(&snapshot, &lens);
        cx.notify();
    }

    fn select_node(&mut self, node_id: NodeId, cx: &mut Context<Self>) {
        self.selection.update(cx, |selection, cx| {
            selection.select([node_id], cx);
        });
        cx.notify();
    }

    fn start_pan(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        if event.button != MouseButton::Middle && event.button != MouseButton::Left {
            return;
        }
        self.panning = Some(point(event.position.x.into(), event.position.y.into()));
        self.pan_anchor = self.pan;
        cx.notify();
    }

    fn update_pan(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
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
            .on_click(cx.listener(move |this, _, _, cx| {
                this.select_node(node_id, cx);
            }))
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

        let item = SemanticMapItem::new(project, selection, cx);
        pane.update(cx, |pane, cx| {
            pane.add_item(Box::new(item), true, true, None, window, cx);
        });
    }
}

impl EventEmitter<ItemEvent> for SemanticMapItem {
}

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
                        this.update_pan(event, cx);
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseUpEvent, _, cx| {
                            this.end_pan(cx);
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
