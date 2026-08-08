use gpui::Context;
use semantic_graph::NodeId;

/// Shared selection for the semantic map panel and canvas.
pub struct SemanticMapSelection {
    pub selected: Vec<NodeId>,
}

impl SemanticMapSelection {
    pub fn new() -> Self {
        Self {
            selected: Vec::new(),
        }
    }

    pub fn select(
        &mut self,
        node_ids: impl IntoIterator<Item = NodeId>,
        cx: &mut Context<Self>,
    ) {
        self.selected = node_ids.into_iter().collect();
        cx.notify();
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.selected.clear();
        cx.notify();
    }
}

impl Default for SemanticMapSelection {
    fn default() -> Self {
        Self::new()
    }
}
