use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use collections::FxHasher;
use gpui::SharedString;
use util::rel_path::RelPath;
use worktree::WorktreeId;

use crate::intent::IntentProvider;
use crate::{
    Confidence, ContentHash, Evidence, EvidenceKind, Intent, IntentIndex, IntentSource, ModuleRef,
    Node, NodeKind, SemanticGraph, SourceLocation, Timestamp,
};

/// Offline static intents from Cargo `description` and README heading/paragraph.
pub struct StaticIntentProvider;

impl IntentProvider for StaticIntentProvider {
    fn intents_for_graph(&self, graph: &SemanticGraph, root: &Path) -> Result<Vec<Intent>> {
        Self::intents_for_graph(graph, root)
    }
}

impl StaticIntentProvider {
    pub fn intents_for_graph(graph: &SemanticGraph, root: &Path) -> Result<Vec<Intent>> {
        let mut intents = Vec::new();
        let updated_at = Timestamp(unix_millis_now());

        for node in graph.nodes.values() {
            if node.kind == NodeKind::Project {
                if let Some(intent) = project_readme_intent(root, node, updated_at)? {
                    intents.push(intent);
                }
                continue;
            }
            if node.kind != NodeKind::Module {
                continue;
            }
            let Some(module_root) = module_fs_root(root, node) else {
                continue;
            };

            let mut evidence = Vec::new();

            if let Some((description, location)) =
                cargo_description_evidence(root, node, &module_root)?
            {
                evidence.push(Evidence {
                    kind: EvidenceKind::CrateDescription,
                    location,
                    excerpt: SharedString::from(description),
                    weight: 1.0,
                });
            }

            if let Some((heading, paragraph, location)) =
                readme_evidence(root, node, &module_root)?
            {
                let excerpt = match (heading.as_str(), paragraph.as_str()) {
                    (h, p) if !h.is_empty() && !p.is_empty() => format!("{h}\n\n{p}"),
                    (h, _) if !h.is_empty() => h.to_string(),
                    (_, p) if !p.is_empty() => p.to_string(),
                    _ => String::new(),
                };
                if !excerpt.is_empty() {
                    evidence.push(Evidence {
                        kind: EvidenceKind::ReadmeSection,
                        location,
                        excerpt: SharedString::from(excerpt),
                        weight: 0.9,
                    });
                }
            }

            if evidence.is_empty() {
                continue;
            }

            let confidence = if evidence
                .iter()
                .any(|item| matches!(item.kind, EvidenceKind::CrateDescription | EvidenceKind::ReadmeSection))
            {
                Confidence::High
            } else {
                Confidence::Medium
            };

            let summary = compose_summary(&node.display_name, &evidence);
            let content_hash = hash_evidence(&evidence);

            intents.push(Intent {
                subject: node.id,
                summary: SharedString::from(summary),
                bullets: Vec::new(),
                confidence,
                source: IntentSource::Static,
                evidence,
                updated_at,
                content_hash,
            });
        }

        Ok(intents)
    }

    /// Build an [`IntentIndex`] keyed by subject node id.
    pub fn enrich(graph: &SemanticGraph, root: &Path) -> Result<IntentIndex> {
        let intents = Self::intents_for_graph(graph, root)?;
        let mut index = IntentIndex::default();
        for intent in intents {
            index.insert(intent.subject, intent);
        }
        Ok(index)
    }
}

fn compose_summary(name: &SharedString, evidence: &[Evidence]) -> String {
    let primary = evidence
        .iter()
        .find(|item| item.kind == EvidenceKind::CrateDescription)
        .or_else(|| {
            evidence
                .iter()
                .find(|item| item.kind == EvidenceKind::ReadmeSection)
        });
    match primary {
        Some(item) if item.kind == EvidenceKind::CrateDescription => {
            format!("{name}: {}", item.excerpt)
        }
        Some(item) => {
            // Prefer first paragraph / heading text from README excerpt.
            let text = item.excerpt.as_ref();
            let first_line = text.lines().find(|line| !line.trim().is_empty()).unwrap_or(text);
            let heading = first_line.trim().trim_start_matches('#').trim();
            if heading.is_empty() {
                format!("{name}")
            } else if heading.eq_ignore_ascii_case(name.as_ref()) {
                // Use paragraph when heading matches the folder name.
                let paragraph = text
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty() && !line.starts_with('#'))
                    .next();
                match paragraph {
                    Some(p) => format!("{name}: {p}"),
                    None => format!("{name}: {heading}"),
                }
            } else {
                format!("{name}: {heading}")
            }
        }
        None => name.to_string(),
    }
}

fn hash_evidence(evidence: &[Evidence]) -> ContentHash {
    let mut hasher = FxHasher::default();
    for item in evidence {
        item.kind.hash(&mut hasher);
        item.excerpt.hash(&mut hasher);
    }
    ContentHash(hasher.finish())
}

fn unix_millis_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn module_fs_root(workspace_root: &Path, node: &Node) -> Option<PathBuf> {
    match &node.key {
        crate::NodeKey::Module { module_ref, .. } => match module_ref {
            ModuleRef::CargoPackage { manifest_dir, .. } => {
                Some(join_rel(workspace_root, manifest_dir))
            }
            ModuleRef::PathModule { path } => Some(join_rel(workspace_root, path)),
            ModuleRef::LanguagePackage { root, .. } => Some(join_rel(workspace_root, root)),
        },
        _ => None,
    }
}

fn join_rel(workspace_root: &Path, rel: &RelPath) -> PathBuf {
    if rel.is_empty() {
        workspace_root.to_path_buf()
    } else {
        workspace_root.join(rel.as_std_path())
    }
}

fn cargo_description_evidence(
    workspace_root: &Path,
    node: &Node,
    module_root: &Path,
) -> Result<Option<(String, Option<SourceLocation>)>> {
    let is_cargo = matches!(
        &node.key,
        crate::NodeKey::Module {
            module_ref: ModuleRef::CargoPackage { .. },
            ..
        }
    );
    if !is_cargo {
        // Still allow reading Cargo.toml if present under a PathModule.
        if !module_root.join("Cargo.toml").is_file() {
            return Ok(None);
        }
    }

    let manifest = module_root.join("Cargo.toml");
    if !manifest.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&manifest)
        .with_context(|| format!("failed to read {}", manifest.display()))?;
    let value: toml::Value = toml::from_str(&text)
        .with_context(|| format!("failed to parse {}", manifest.display()))?;
    let description = value
        .get("package")
        .and_then(|package| package.get("description"))
        .and_then(|description| description.as_str())
        .map(str::trim)
        .filter(|description| !description.is_empty())
        .map(str::to_string);

    let Some(description) = description else {
        return Ok(None);
    };

    let worktree_id = worktree_id_from_node(node);
    let rel = rel_path_from_root(workspace_root, &manifest)?;
    Ok(Some((
        description,
        Some(SourceLocation {
            worktree_id,
            path: rel,
            range: None,
            symbol: None,
        }),
    )))
}

fn readme_evidence(
    workspace_root: &Path,
    node: &Node,
    module_root: &Path,
) -> Result<Option<(String, String, Option<SourceLocation>)>> {
    let readme_path = ["README.md", "Readme.md", "readme.md"]
        .iter()
        .map(|name| module_root.join(name))
        .find(|path| path.is_file());
    let Some(readme_path) = readme_path else {
        return Ok(None);
    };
    let text = std::fs::read_to_string(&readme_path)
        .with_context(|| format!("failed to read {}", readme_path.display()))?;
    let (heading, paragraph) = parse_readme_heading_and_paragraph(&text);
    if heading.is_empty() && paragraph.is_empty() {
        return Ok(None);
    }

    let worktree_id = worktree_id_from_node(node);
    let rel = rel_path_from_root(workspace_root, &readme_path)?;
    Ok(Some((
        heading,
        paragraph,
        Some(SourceLocation {
            worktree_id,
            path: rel,
            range: None,
            symbol: None,
        }),
    )))
}

fn parse_readme_heading_and_paragraph(text: &str) -> (String, String) {
    let mut heading = String::new();
    let mut paragraph_lines: Vec<&str> = Vec::new();
    let mut past_heading = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if is_skippable_readme_line(trimmed) {
            if !paragraph_lines.is_empty() {
                break;
            }
            continue;
        }
        if !past_heading {
            if let Some(rest) = trimmed.strip_prefix('#') {
                heading = rest.trim_start_matches('#').trim().to_string();
                past_heading = true;
                continue;
            }
            // No ATX heading — treat first non-empty prose line as paragraph start.
            past_heading = true;
            paragraph_lines.push(trimmed);
            continue;
        }
        if trimmed.starts_with('#') && paragraph_lines.is_empty() {
            continue;
        }
        paragraph_lines.push(trimmed);
    }

    (heading, paragraph_lines.join(" "))
}

fn is_skippable_readme_line(trimmed: &str) -> bool {
    if trimmed.is_empty() {
        return true;
    }
    let without_align = trimmed.trim_start_matches('|').trim();
    if without_align.starts_with("![") || without_align.starts_with("[![") {
        return true;
    }
    let break_chars: Vec<char> = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
    !break_chars.is_empty()
        && break_chars.len() >= 3
        && break_chars
            .iter()
            .all(|c| matches!(c, '-' | '*' | '_' | '='))
}

fn worktree_id_from_node(node: &Node) -> WorktreeId {
    match &node.key {
        crate::NodeKey::Project { worktree_id } | crate::NodeKey::Module { worktree_id, .. } => {
            *worktree_id
        }
        _ => WorktreeId::from_usize(0),
    }
}

fn project_readme_intent(root: &Path, node: &Node, updated_at: Timestamp) -> Result<Option<Intent>> {
    let Some((heading, paragraph, location)) = readme_evidence(root, node, root)? else {
        return Ok(None);
    };
    let summary = if !paragraph.is_empty() {
        paragraph.clone()
    } else if !heading.is_empty() {
        heading.clone()
    } else {
        return Ok(None);
    };
    let excerpt = match (heading.as_str(), paragraph.as_str()) {
        (h, p) if !h.is_empty() && !p.is_empty() => format!("{h}\n\n{p}"),
        (h, _) if !h.is_empty() => h.to_string(),
        (_, p) => p.to_string(),
    };
    let evidence = vec![Evidence {
        kind: EvidenceKind::ReadmeSection,
        location,
        excerpt: SharedString::from(excerpt),
        weight: 1.0,
    }];
    let content_hash = hash_evidence(&evidence);
    Ok(Some(Intent {
        subject: node.id,
        summary: SharedString::from(summary),
        bullets: Vec::new(),
        confidence: Confidence::High,
        source: IntentSource::Static,
        evidence,
        updated_at,
        content_hash,
    }))
}

fn rel_path_from_root(root: &Path, absolute: &Path) -> Result<Arc<RelPath>> {
    let root_canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let absolute_canon = absolute
        .canonicalize()
        .unwrap_or_else(|_| absolute.to_path_buf());
    let relative = absolute_canon.strip_prefix(&root_canon).with_context(|| {
        format!(
            "path {} is not under workspace root {}",
            absolute_canon.display(),
            root_canon.display()
        )
    })?;
    if relative.as_os_str().is_empty() {
        return Ok(RelPath::empty_arc());
    }
    let unix = relative
        .to_str()
        .with_context(|| format!("non-utf8 relative path {}", relative.display()))?
        .replace('\\', "/");
    let rel = RelPath::from_unix_str(&unix)
        .with_context(|| format!("invalid relative path {unix}"))?;
    Ok(rel.into())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use worktree::WorktreeId;

    use super::{parse_readme_heading_and_paragraph, StaticIntentProvider};
    use crate::extract::extract_cargo_workspace;
    use crate::{GraphPatch, Node, NodeId, NodeKey, NodeKind, SemanticGraph};

    #[test]
    fn static_intent_uses_crate_description() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_data/simple_workspace");
        let worktree_id = WorktreeId::from_usize(1);
        let patch = extract_cargo_workspace(&root, worktree_id).unwrap();
        let mut graph = SemanticGraph::default();
        graph.apply_patch(patch).unwrap();

        let intents = StaticIntentProvider::enrich(&graph, &root).unwrap();

        let core_lib_id = graph
            .nodes
            .values()
            .find(|node| node.kind == NodeKind::Module && node.display_name.as_ref() == "core_lib")
            .map(|node| node.id)
            .expect("core_lib module");

        let intent = intents.get(&core_lib_id).expect("intent for core_lib");
        assert!(
            intent.summary.to_lowercase().contains("domain")
                || intent
                    .evidence
                    .iter()
                    .any(|evidence| evidence.excerpt.to_lowercase().contains("domain")),
            "expected core_lib intent to mention domain, got summary={:?} evidence={:?}",
            intent.summary,
            intent
                .evidence
                .iter()
                .map(|evidence| evidence.excerpt.to_string())
                .collect::<Vec<_>>()
        );
        assert_eq!(intent.source, crate::IntentSource::Static);
    }

    #[test]
    fn parse_readme_skips_badges_and_rules_for_first_paragraph() {
        let text = "# Zed\n\n[![CI](https://example.com/badge.svg)](https://example.com)\n\n---\n\nWelcome to Zed, a high-performance, multiplayer code editor.\n";
        let (heading, paragraph) = parse_readme_heading_and_paragraph(text);
        assert_eq!(heading, "Zed");
        assert!(
            paragraph.to_lowercase().contains("high-performance"),
            "expected prose paragraph, got {paragraph:?}"
        );
        assert!(
            !paragraph.contains("badge"),
            "badge markup should not become the first paragraph, got {paragraph:?}"
        );
    }

    #[test]
    fn static_intent_lifts_root_readme_onto_project() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("README.md"),
            "# Zed\n\n[![CI](https://example.com/badge.svg)](https://example.com)\n\nWelcome to Zed, a high-performance, multiplayer code editor.\n",
        )
        .unwrap();

        let worktree_id = WorktreeId::from_usize(1);
        let key = NodeKey::Project { worktree_id };
        let id = NodeId::from_key(&key);
        let mut graph = SemanticGraph::default();
        graph
            .apply_patch(GraphPatch {
                base: graph.revision(),
                removed_nodes: vec![],
                removed_edges: vec![],
                upsert_nodes: vec![Node::project(id, key, "workspace")],
                upsert_edges: vec![],
            })
            .unwrap();

        let intents = StaticIntentProvider::enrich(&graph, root).unwrap();
        let intent = intents.get(&id).expect("project node should have a README intent");
        assert!(
            intent.summary.to_lowercase().contains("high-performance")
                && intent.summary.to_lowercase().contains("multiplayer"),
            "expected root README paragraph on the Project node, got {:?}",
            intent.summary
        );
        assert_eq!(intent.source, crate::IntentSource::Static);
    }
}
