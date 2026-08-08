use std::ops::Range;
use std::sync::Arc;

use anyhow::{Result, bail};
use collections::HashMap;
use gpui::SharedString;
use serde::Serialize;
use strum::{EnumIter, IntoEnumIterator};
use util::rel_path::RelPath;
use worktree::WorktreeId;

use crate::{EdgeId, NodeId};

/// Stable key used to create [`NodeId`]. Stored for debugging and remapping.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub enum NodeKey {
    Project {
        worktree_id: WorktreeId,
    },
    Subsystem {
        project: NodeKeyRef,
        slug: SharedString,
    },
    Module {
        worktree_id: WorktreeId,
        module_ref: ModuleRef,
    },
    Entry {
        module: NodeKeyRef,
        symbol_key: SymbolKey,
    },
    Type {
        module: NodeKeyRef,
        symbol_key: SymbolKey,
    },
    External {
        ecosystem: EcosystemKind,
        name: SharedString,
        version_req: Option<SharedString>,
    },
}

/// Reference to another [`NodeKey`] without making `NodeKey` infinitely sized.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct NodeKeyRef(pub Arc<NodeKey>);

impl From<NodeKey> for NodeKeyRef {
    fn from(key: NodeKey) -> Self {
        Self(Arc::new(key))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub enum ModuleRef {
    CargoPackage {
        package_name: SharedString,
        manifest_dir: Arc<RelPath>,
    },
    PathModule {
        path: Arc<RelPath>,
    },
    LanguagePackage {
        system: SharedString,
        name: SharedString,
        root: Arc<RelPath>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct SymbolKey {
    pub qualified_name: SharedString,
    pub kind: SymbolKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, EnumIter)]
pub enum SymbolKind {
    Function,
    Method,
    Struct,
    Enum,
    Trait,
    Interface,
    Class,
    TypeAlias,
    Module,
    Constant,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, EnumIter)]
pub enum EcosystemKind {
    Cargo,
    Npm,
    PyPI,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Node {
    pub id: NodeId,
    pub key: NodeKey,
    pub kind: NodeKind,
    pub display_name: SharedString,
    pub abbrev: Option<SharedString>,
    pub location: Option<SourceLocation>,
    pub payload: NodePayload,
    pub flags: NodeFlags,
}

impl Node {
    pub fn project(id: NodeId, key: NodeKey, display_name: impl Into<SharedString>) -> Self {
        Self {
            id,
            key,
            kind: NodeKind::Project,
            display_name: display_name.into(),
            abbrev: None,
            location: None,
            payload: NodePayload::Project(ProjectPayload {}),
            flags: NodeFlags::default(),
        }
    }

    pub fn module(
        id: NodeId,
        key: NodeKey,
        display_name: impl Into<SharedString>,
        location: Option<SourceLocation>,
        payload: ModulePayload,
        flags: NodeFlags,
    ) -> Self {
        Self {
            id,
            key,
            kind: NodeKind::Module,
            display_name: display_name.into(),
            abbrev: None,
            location,
            payload: NodePayload::Module(payload),
            flags,
        }
    }

    pub fn subsystem(
        id: NodeId,
        key: NodeKey,
        display_name: impl Into<SharedString>,
        payload: SubsystemPayload,
        flags: NodeFlags,
    ) -> Self {
        Self {
            id,
            key,
            kind: NodeKind::Subsystem,
            display_name: display_name.into(),
            abbrev: None,
            location: None,
            payload: NodePayload::Subsystem(payload),
            flags,
        }
    }

    pub fn entry(
        id: NodeId,
        key: NodeKey,
        display_name: impl Into<SharedString>,
        location: Option<SourceLocation>,
        entry_kind: EntryKind,
        flags: NodeFlags,
    ) -> Self {
        Self {
            id,
            key,
            kind: NodeKind::Entry,
            display_name: display_name.into(),
            abbrev: None,
            location,
            payload: NodePayload::Entry(EntryPayload { entry_kind }),
            flags,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, EnumIter)]
pub enum NodeKind {
    Project,
    Subsystem,
    Module,
    Entry,
    Type,
    External,
}

/// Path + optional range evidence for a node or edge.
///
/// Uses worktree id + relative path rather than `project::ProjectPath` to keep
/// this crate free of a `project` dependency.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct SourceLocation {
    pub worktree_id: WorktreeId,
    pub path: Arc<RelPath>,
    pub range: Option<Range<TextPoint>>,
    pub symbol: Option<SymbolKey>,
}

/// UTF-16-oriented buffer point (row, column), matching editor conventions loosely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct TextPoint {
    pub row: u32,
    pub column: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct NodeFlags {
    pub is_entry_point: bool,
    pub is_generated: bool,
    pub is_test: bool,
    pub is_external: bool,
    pub user_pinned_subsystem: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum NodePayload {
    Project(ProjectPayload),
    Subsystem(SubsystemPayload),
    Module(ModulePayload),
    Entry(EntryPayload),
    Type(TypePayload),
    External(ExternalPayload),
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct ProjectPayload {}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SubsystemPayload {
    pub member_count: u32,
    pub cluster_score: f32,
    pub pinned: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModulePayload {
    pub language: Option<SharedString>,
    pub module_kind: ModuleKind,
    pub public_exports: Vec<SymbolKey>,
    pub deps_out_count: u32,
    pub deps_in_count: u32,
    pub loc_estimate: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, EnumIter)]
pub enum ModuleKind {
    CrateLib,
    CrateBin,
    FileModule,
    Package,
    Folder,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EntryPayload {
    pub entry_kind: EntryKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, EnumIter)]
pub enum EntryKind {
    Main,
    LibRoot,
    BinTarget,
    PublicApi,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TypePayload {
    pub type_kind: TypeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, EnumIter)]
pub enum TypeKind {
    Struct,
    Enum,
    Trait,
    Interface,
    Class,
    TypeAlias,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExternalPayload {
    pub ecosystem: EcosystemKind,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Edge {
    pub id: EdgeId,
    pub kind: EdgeKind,
    pub from: NodeId,
    pub to: NodeId,
    pub weight: f32,
    pub location: Option<SourceLocation>,
    pub payload: EdgePayload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, EnumIter)]
pub enum EdgeKind {
    Contains,
    DependsOn,
    Exposes,
    Implements,
    Calls,
    References,
    DesignLinks,
}

impl EdgeKind {
    pub fn discriminant(self) -> u8 {
        self as u8
    }
}

impl Edge {
    pub fn new(kind: EdgeKind, from: NodeId, to: NodeId) -> Self {
        Self {
            id: EdgeId::from_endpoints(kind.discriminant(), from, to),
            kind,
            from,
            to,
            weight: 1.0,
            location: None,
            payload: EdgePayload::default(),
        }
    }

    pub fn contains(from: NodeId, to: NodeId) -> Self {
        Self::new(EdgeKind::Contains, from, to)
    }

    pub fn depends_on(from: NodeId, to: NodeId) -> Self {
        Self::new(EdgeKind::DependsOn, from, to)
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct EdgePayload {}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Intent {
    pub subject: NodeId,
    pub summary: SharedString,
    pub bullets: Vec<SharedString>,
    pub confidence: Confidence,
    pub source: IntentSource,
    pub evidence: Vec<Evidence>,
    pub updated_at: Timestamp,
    pub content_hash: ContentHash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, EnumIter)]
pub enum Confidence {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum IntentSource {
    Static,
    Llm {
        provider: SharedString,
        model: SharedString,
        prompt_version: u32,
    },
    Mixed,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Evidence {
    pub kind: EvidenceKind,
    pub location: Option<SourceLocation>,
    pub excerpt: SharedString,
    pub weight: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, EnumIter)]
pub enum EvidenceKind {
    ReadmeSection,
    CrateDescription,
    InnerDocComment,
    ModuleDocComment,
    SymbolNameHeuristic,
    ExportList,
    ManifestMetadata,
    LlmCitation,
}

/// Unix millis since epoch (or any monotonic store clock).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct Timestamp(pub u64);

/// Hash of evidence + prompt version for intent cache invalidation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct ContentHash(pub u64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Lens {
    pub root: Option<NodeId>,
    pub allowed_kinds: Vec<NodeKind>,
    pub max_depth: Option<u32>,
    pub hide_external: bool,
    pub hide_tests: bool,
    pub focus: Option<FocusQuery>,
    pub edge_kinds: Vec<EdgeKind>,
}

impl Default for Lens {
    fn default() -> Self {
        Self {
            root: None,
            allowed_kinds: NodeKind::iter().collect(),
            max_depth: None,
            hide_external: false,
            hide_tests: false,
            focus: None,
            edge_kinds: EdgeKind::iter().collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FocusQuery {
    pub text: Option<SharedString>,
    pub node_ids: Vec<NodeId>,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct GraphRevision(pub u64);

impl GraphRevision {
    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct GraphPatch {
    pub base: GraphRevision,
    pub removed_nodes: Vec<NodeId>,
    pub removed_edges: Vec<EdgeId>,
    pub upsert_nodes: Vec<Node>,
    pub upsert_edges: Vec<Edge>,
}

#[derive(Debug, Clone, Default)]
pub struct SemanticGraph {
    revision: GraphRevision,
    pub nodes: HashMap<NodeId, Node>,
    pub edges: HashMap<EdgeId, Edge>,
    pub by_key: HashMap<NodeKey, NodeId>,
    pub children: HashMap<NodeId, Vec<NodeId>>,
    pub dependents: HashMap<NodeId, Vec<NodeId>>,
    pub dependencies: HashMap<NodeId, Vec<NodeId>>,
}

impl SemanticGraph {
    pub fn revision(&self) -> GraphRevision {
        self.revision
    }

    pub fn apply_patch(&mut self, patch: GraphPatch) -> Result<()> {
        if patch.base != self.revision {
            bail!(
                "graph patch base revision {:?} does not match current {:?}",
                patch.base,
                self.revision
            );
        }

        for edge_id in patch.removed_edges {
            self.edges.remove(&edge_id);
        }

        for node_id in patch.removed_nodes {
            if let Some(node) = self.nodes.remove(&node_id) {
                self.by_key.remove(&node.key);
            }
        }

        for node in patch.upsert_nodes {
            if let Some(previous) = self.nodes.insert(node.id, node.clone()) {
                if previous.key != node.key {
                    self.by_key.remove(&previous.key);
                }
            }
            self.by_key.insert(node.key.clone(), node.id);
        }

        for edge in patch.upsert_edges {
            self.edges.insert(edge.id, edge);
        }

        self.rebuild_indexes();
        self.revision = self.revision.next();
        Ok(())
    }

    fn rebuild_indexes(&mut self) {
        self.children.clear();
        self.dependencies.clear();
        self.dependents.clear();

        for edge in self.edges.values() {
            match edge.kind {
                EdgeKind::Contains => {
                    self.children.entry(edge.from).or_default().push(edge.to);
                }
                EdgeKind::DependsOn => {
                    self.dependencies
                        .entry(edge.from)
                        .or_default()
                        .push(edge.to);
                    self.dependents.entry(edge.to).or_default().push(edge.from);
                }
                EdgeKind::Exposes
                | EdgeKind::Implements
                | EdgeKind::Calls
                | EdgeKind::References
                | EdgeKind::DesignLinks => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use worktree::WorktreeId;

    use super::*;

    #[test]
    fn graph_patch_upsert_and_remove() {
        let mut graph = SemanticGraph::default();
        let key = NodeKey::Project {
            worktree_id: WorktreeId::from_usize(1),
        };
        let id = NodeId::from_key(&key);
        let node = Node::project(id, key, "demo");
        graph
            .apply_patch(GraphPatch {
                base: graph.revision(),
                removed_nodes: vec![],
                removed_edges: vec![],
                upsert_nodes: vec![node],
                upsert_edges: vec![],
            })
            .unwrap();
        assert_eq!(graph.nodes.len(), 1);
        graph
            .apply_patch(GraphPatch {
                base: graph.revision(),
                removed_nodes: vec![id],
                removed_edges: vec![],
                upsert_nodes: vec![],
                upsert_edges: vec![],
            })
            .unwrap();
        assert!(graph.nodes.is_empty());
    }
}
