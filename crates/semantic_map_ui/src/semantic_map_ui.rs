mod selection;
mod settings;
mod view_model;

pub use selection::SemanticMapSelection;
pub use settings::{
    SemanticMapClusterSettings, SemanticMapIntentSettings, SemanticMapSettings, SemanticMapSkin,
};
pub use view_model::{
    CanvasViewModel, PanelRow, PanelViewModel, SceneEdge, SceneNode,
};

pub fn init(cx: &mut gpui::App) {
    use ::settings::Settings as _;
    SemanticMapSettings::register(cx);
}
