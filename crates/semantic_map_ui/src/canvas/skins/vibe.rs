use gpui::{App, Hsla};
use semantic_graph::NodeKind;
use ui::prelude::*;

/// Default Phase A card styling for the semantic map canvas.
pub struct VibeSkin;

impl VibeSkin {
    pub fn card_background(kind: NodeKind, cx: &App) -> Hsla {
        let colors = cx.theme().colors();
        match kind {
            NodeKind::Subsystem => colors.elevated_surface_background,
            NodeKind::Module => colors.surface_background,
            _ => colors.background,
        }
    }

    pub fn card_border(selected: bool, cx: &App) -> Hsla {
        let colors = cx.theme().colors();
        if selected {
            colors.border_selected
        } else {
            colors.border_variant
        }
    }

    pub fn edge_color(cx: &App) -> Hsla {
        cx.theme().colors().border
    }
}
