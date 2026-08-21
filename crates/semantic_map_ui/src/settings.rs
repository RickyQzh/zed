use settings::{RegisterSetting, Settings};

pub use settings::SemanticMapSkin;

#[derive(Debug, Clone, Copy, PartialEq, RegisterSetting)]
pub struct SemanticMapSettings {
    pub enabled: bool,
    pub default_skin: SemanticMapSkin,
    pub hide_external: bool,
    pub hide_tests: bool,
    pub auto_open_canvas_on_project_open: bool,
    pub module_depth: usize,
    pub max_auto_nodes: usize,
    pub intent: SemanticMapIntentSettings,
    pub cluster: SemanticMapClusterSettings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticMapIntentSettings {
    pub llm: bool,
    pub llm_on_visible_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticMapClusterSettings {
    pub min_subsystems: usize,
    pub max_subsystems: usize,
}

impl Settings for SemanticMapSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let map = content.semantic_map.as_ref().unwrap();
        let intent = map.intent.as_ref().unwrap();
        let cluster = map.cluster.as_ref().unwrap();
        Self {
            enabled: map.enabled.unwrap(),
            default_skin: map.default_skin.unwrap(),
            hide_external: map.hide_external.unwrap(),
            hide_tests: map.hide_tests.unwrap(),
            auto_open_canvas_on_project_open: map.auto_open_canvas_on_project_open.unwrap(),
            module_depth: map.module_depth.unwrap(),
            max_auto_nodes: map.max_auto_nodes.unwrap(),
            intent: SemanticMapIntentSettings {
                llm: intent.llm.unwrap(),
                llm_on_visible_only: intent.llm_on_visible_only.unwrap(),
            },
            cluster: SemanticMapClusterSettings {
                min_subsystems: cluster.min_subsystems.unwrap(),
                max_subsystems: cluster.max_subsystems.unwrap(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use settings::{RootUserSettings, SemanticMapSkin, Settings, SettingsContent};

    use super::SemanticMapSettings;

    #[test]
    fn semantic_map_settings_defaults_parse() {
        let content = SettingsContent::parse_json_with_comments(
            r#"{
                "semantic_map": {
                    "enabled": false,
                    "default_skin": "vibe",
                    "hide_external": true,
                    "hide_tests": true,
                    "auto_open_canvas_on_project_open": false,
                    "module_depth": 3,
                    "max_auto_nodes": 500,
                    "intent": {
                        "llm": false,
                        "llm_on_visible_only": true
                    },
                    "cluster": {
                        "min_subsystems": 3,
                        "max_subsystems": 16
                    }
                }
            }"#,
        )
        .expect("semantic_map defaults should parse");

        let settings = SemanticMapSettings::from_settings(&content);
        assert!(!settings.enabled);
        assert_eq!(settings.default_skin, SemanticMapSkin::Vibe);
        assert!(settings.hide_external);
        assert!(settings.hide_tests);
        assert!(!settings.auto_open_canvas_on_project_open);
        assert_eq!(settings.module_depth, 3);
        assert_eq!(settings.max_auto_nodes, 500);
        assert!(!settings.intent.llm);
        assert!(settings.intent.llm_on_visible_only);
        assert_eq!(settings.cluster.min_subsystems, 3);
        assert_eq!(settings.cluster.max_subsystems, 16);
    }
}
