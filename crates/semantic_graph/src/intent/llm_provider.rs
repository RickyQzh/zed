use crate::{Intent, IntentIndex, SemanticGraph};

/// Optional LLM intent enrichment. Stub until wired to `language_model`.
pub struct LlmIntentProvider;

impl LlmIntentProvider {
    /// When `enabled` is false, returns immediately with no intents (offline path).
    /// When enabled, this stub still returns empty — real model routing is a follow-up.
    pub fn enrich_if_enabled(
        &self,
        enabled: bool,
        _graph: &SemanticGraph,
        _static_intents: &IntentIndex,
    ) -> Vec<Intent> {
        if !enabled {
            return Vec::new();
        }
        // Intentionally empty stub — real model routing is a follow-up.
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::LlmIntentProvider;
    use crate::{IntentIndex, SemanticGraph};

    #[test]
    fn disabled_returns_empty_without_model() {
        let graph = SemanticGraph::default();
        let static_intents = IntentIndex::default();
        let extras = LlmIntentProvider.enrich_if_enabled(false, &graph, &static_intents);
        assert!(extras.is_empty());
    }

    #[test]
    fn enabled_stub_returns_empty_without_model_service() {
        let graph = SemanticGraph::default();
        let static_intents = IntentIndex::default();
        let extras = LlmIntentProvider.enrich_if_enabled(true, &graph, &static_intents);
        assert!(extras.is_empty());
    }
}
