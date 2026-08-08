use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use util::rel_path::RelPath;
use worktree::WorktreeId;

use crate::{GraphPatch, SemanticGraph};

/// Caps for a single extraction pass.
#[derive(Debug, Clone, Default)]
pub struct ExtractBudget {
    pub max_nodes: Option<usize>,
    pub max_duration: Option<Duration>,
}

/// Filesystem-oriented extract context (no `Entity<Project>`).
///
/// Async / project-backed extract context comes later; this keeps unit tests
/// lightweight and pure-FS.
#[derive(Debug, Clone)]
pub struct FsExtractCtx {
    pub root: PathBuf,
    pub worktree_id: WorktreeId,
    pub dirty_paths: Vec<Arc<RelPath>>,
    pub previous: Option<Arc<SemanticGraph>>,
    pub budget: ExtractBudget,
}

/// Pluggable graph extractor.
pub trait SemanticExtractor: Send + Sync {
    fn id(&self) -> &'static str;
    fn priority(&self) -> i32;
    fn extract_sync(&self, ctx: &FsExtractCtx) -> Result<GraphPatch>;
}
