mod static_provider;

pub use static_provider::StaticIntentProvider;

use anyhow::Result;
use std::path::Path;

use crate::{Intent, SemanticGraph};

/// Pluggable intent enrichment over an existing graph.
pub trait IntentProvider {
    fn intents_for_graph(&self, graph: &SemanticGraph, root: &Path) -> Result<Vec<Intent>>;
}
