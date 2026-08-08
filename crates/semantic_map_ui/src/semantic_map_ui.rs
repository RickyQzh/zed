mod settings;

pub use settings::{
    SemanticMapClusterSettings, SemanticMapIntentSettings, SemanticMapSettings, SemanticMapSkin,
};

pub fn init(cx: &mut gpui::App) {
    use ::settings::Settings as _;
    SemanticMapSettings::register(cx);
}
