mod canvas;
mod panel;
mod selection;
mod settings;
mod view_model;

pub use canvas::SemanticMapItem;
pub use panel::{
    OpenCanvas, OpenSelectedSource, Reindex, SemanticMapPanel, ToggleFocus,
};
pub use selection::SemanticMapSelection;
pub use settings::{
    SemanticMapClusterSettings, SemanticMapIntentSettings, SemanticMapSettings, SemanticMapSkin,
};
pub use view_model::{
    CanvasViewModel, PanelRow, PanelStatus, PanelViewModel, SceneEdge, SceneNode,
};

pub fn init(cx: &mut gpui::App) {
    use ::settings::Settings as _;

    SemanticMapSettings::register(cx);
    cx.observe_new(panel::register_panel_actions).detach();
}
