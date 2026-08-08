# Semantic Map Phase A Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a Zed-native Semantic Map (IR store + Rust/generic extractors + static intents + side panel + central canvas) so opening a project answers “what is the structure and what does it do?” without reading code linearly.

**Architecture:** Add `semantic_graph` (Project-owned IR, extractors, intents) and `semantic_map_ui` (dock panel + pane canvas item). Extractors produce `GraphPatch`es applied to `SemanticGraphStore`; UI reads immutable snapshots through a shared selection state. UML/C4/LLM/agent overlays are out of this plan except optional LLM intent hooks stubbed behind settings.

**Tech Stack:** Rust, GPUI, existing `project` / `worktree` / `language` / `workspace` / `settings` / `ui` crates; Cargo.toml parsing via `toml` + filesystem; Tree-sitter outlines where already available through `language`.

**Spec:** `docs/superpowers/specs/2026-08-08-semantic-map-design.md` (Phase A / milestones M0–M5 only).

**Out of this plan (separate later plans):** Phase C agent co-view, Phase B diagram edits, remote proto extraction, full UML class skin, WASM extractors.

## Global Constraints

- Prefer implementing in existing files unless creating the new logical crates above; no `mod.rs` paths — use `src/<crate_name>.rs` as `[lib] path`.
- No `unwrap()` in production paths; propagate with `?` or log with visibility.
- Never silently discard fallible work with `let _ =` on errors.
- GPUI tests use `cx.background_executor().timer(...)`, not `smol::Timer`, when pumping.
- Feature gated: `semantic_map.enabled` defaults to `false` until dogfood-ready; code may still compile and unit-test.
- Dogfood language priority: Rust/Cargo deep; all other languages get generic thin extractor only.
- Offline-first: map works with `semantic_map.intent.llm = false`.
- Follow Zed workspace patterns for crate registration in root `Cargo.toml` (`members` + `[workspace.dependencies]` path entry).
- License headers / `publish.workspace = true` / `edition.workspace = true` like sibling crates (e.g. `outline_panel`).

---

## File structure (create / modify)

### Create

| Path | Responsibility |
|------|----------------|
| `crates/semantic_graph/Cargo.toml` | Domain crate manifest |
| `crates/semantic_graph/src/semantic_graph.rs` | Lib root, module exports, `init` helpers |
| `crates/semantic_graph/src/ir.rs` | Node/Edge/Graph/Patch/Lens types |
| `crates/semantic_graph/src/ids.rs` | `NodeId`/`EdgeId`/`NodeKey` hashing |
| `crates/semantic_graph/src/store.rs` | `SemanticGraphStore` entity |
| `crates/semantic_graph/src/invalidation.rs` | Dirty path → jobs |
| `crates/semantic_graph/src/extract/traits.rs` | `SemanticExtractor`, `ExtractCtx` |
| `crates/semantic_graph/src/extract/cargo.rs` | Cargo workspace extractor |
| `crates/semantic_graph/src/extract/rust_modules.rs` | Rust module tree (depth-limited) |
| `crates/semantic_graph/src/extract/generic.rs` | Thin multi-language folder extractor |
| `crates/semantic_graph/src/extract/cluster.rs` | Subsystem clustering |
| `crates/semantic_graph/src/extract/extract.rs` | Registry + orchestration |
| `crates/semantic_graph/src/intent/static_provider.rs` | Static intents |
| `crates/semantic_graph/src/intent/intent.rs` | Intent types + `IntentStore` |
| `crates/semantic_graph/src/layout/pins.rs` | Pinned positions model |
| `crates/semantic_graph/src/layout/hierarchy.rs` | Auto layout → scene coords (logical) |
| `crates/semantic_graph/test_data/simple_workspace/**` | Fixture Cargo workspace |
| `crates/semantic_map_ui/Cargo.toml` | UI crate manifest |
| `crates/semantic_map_ui/src/semantic_map_ui.rs` | Lib root, `init`, actions |
| `crates/semantic_map_ui/src/settings.rs` | `SemanticMapSettings` |
| `crates/semantic_map_ui/src/view_model.rs` | Lens → panel/canvas view models |
| `crates/semantic_map_ui/src/selection.rs` | Window-local shared selection |
| `crates/semantic_map_ui/src/panel.rs` | `SemanticMapPanel` |
| `crates/semantic_map_ui/src/canvas/item.rs` | `SemanticMapItem` pane item |
| `crates/semantic_map_ui/src/canvas/element.rs` | Custom GPUI element paint/hit-test |
| `crates/semantic_map_ui/src/canvas/skins/vibe.rs` | Default skin |

### Modify

| Path | Change |
|------|--------|
| `Cargo.toml` | `members` + workspace dep entries for both crates |
| `crates/project/Cargo.toml` | Depend on `semantic_graph` |
| `crates/project/src/project.rs` | Own/create `SemanticGraphStore`; expose accessor |
| `crates/settings_content/src/settings_content.rs` | Add `semantic_map` settings content |
| `crates/zed/Cargo.toml` | Depend on `semantic_map_ui` |
| `crates/zed/src/main.rs` | `semantic_map_ui::init(cx)` |
| `crates/zed/src/zed.rs` | Register panel on workspace like outline/project panel; menu item optional |
| `crates/zed_actions/src/zed_actions.rs` (or local actions in UI crate) | Prefer actions in `semantic_map_ui` via `actions!` like `outline_panel` |

---

### Task 1: Scaffold `semantic_graph` crate + IR types

**Files:**
- Create: `crates/semantic_graph/Cargo.toml`
- Create: `crates/semantic_graph/src/semantic_graph.rs`
- Create: `crates/semantic_graph/src/ids.rs`
- Create: `crates/semantic_graph/src/ir.rs`
- Modify: `/workspace/Cargo.toml` (members + workspace.dependencies)
- Test: unit tests inside `ir.rs` / `ids.rs`

**Interfaces:**
- Consumes: workspace crate conventions only
- Produces: `NodeId`, `EdgeId`, `NodeKey`, `Node`, `Edge`, `NodeKind`, `EdgeKind`, `SemanticGraph`, `GraphPatch`, `GraphRevision`, `Lens`, `Intent`, `Evidence`, `SourceLocation`

- [ ] **Step 1: Add crate manifest**

Create `crates/semantic_graph/Cargo.toml`:

```toml
[package]
name = "semantic_graph"
version = "0.1.0"
edition.workspace = true
publish.workspace = true
license = "GPL-3.0-or-later"

[lints]
workspace = true

[lib]
path = "src/semantic_graph.rs"
doctest = false

[dependencies]
anyhow.workspace = true
collections.workspace = true
gpui.workspace = true
parking_lot.workspace = true
serde.workspace = true
serde_json.workspace = true
strum.workspace = true
util.workspace = true
worktree.workspace = true

[dev-dependencies]
gpui = { workspace = true, features = ["test-support"] }
pretty_assertions.workspace = true
```

If `strum` / `parking_lot` are not workspace deps, use already-present equivalents (`collections`, `std::sync::Arc`, manual enums) — prefer existing workspace crates over adding new ones.

- [ ] **Step 2: Register in root workspace**

In `/workspace/Cargo.toml`:
1. Add `"crates/semantic_graph"` to `members` in alphabetical-ish position near other `s*` crates.
2. Add under `[workspace.dependencies]`:

```toml
semantic_graph = { path = "crates/semantic_graph" }
```

- [ ] **Step 3: Write failing ID/IR tests**

In `ids.rs` / `ir.rs` include:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn node_key_hash_is_stable_for_cargo_package() {
        let key = NodeKey::Module {
            worktree_id: WorktreeId::from_usize(1),
            module_ref: ModuleRef::CargoPackage {
                package_name: "editor".into(),
                manifest_dir: RelPath::unix("crates/editor").unwrap(),
            },
        };
        let a = NodeId::from_key(&key);
        let b = NodeId::from_key(&key);
        assert_eq!(a, b);
    }

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
```

Adapt `WorktreeId` / `RelPath` constructors to match current `worktree` / `util::rel_path` APIs in this repo (grep `WorktreeId::` and `RelPath::unix`).

- [ ] **Step 4: Run tests (expect fail / compile fail)**

```bash
cargo test -p semantic_graph --lib
```

Expected: compile errors because types missing.

- [ ] **Step 5: Implement IDs + IR minimally**

`ids.rs`: `NodeId(u64)`, `EdgeId(u64)`, `NodeId::from_key` using a stable hash (e.g. `seahash` if present, else `DefaultHasher` with documented caveat, preferably an existing workspace hash helper).

`ir.rs`: enums/structs from spec §4 (`NodeKind`, `EdgeKind`, `Node`, `Edge`, `SemanticGraph`, `GraphPatch`, `GraphRevision`, `Lens`, `Intent`, `Evidence`, `SourceLocation`, payloads as needed for Phase A). Implement `SemanticGraph::apply_patch` that:
1. Rejects mismatched `base` revision (`anyhow::bail!`)
2. Removes edges/nodes
3. Upserts nodes/edges
4. Rebuilds `by_key`, `children`, `dependencies`, `dependents` indexes for `Contains` / `DependsOn`
5. Bumps revision

`semantic_graph.rs`:

```rust
mod ids;
mod ir;
mod store;
// later modules added in subsequent tasks

pub use ids::*;
pub use ir::*;
```

- [ ] **Step 6: Run tests**

```bash
cargo test -p semantic_graph --lib
```

Expected: PASS for Task 1 tests.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml crates/semantic_graph
git commit -m "semantic_graph: add IR types and patch apply"
```

---

### Task 2: `SemanticGraphStore` entity + snapshot API

**Files:**
- Create: `crates/semantic_graph/src/store.rs`
- Modify: `crates/semantic_graph/src/semantic_graph.rs`
- Test: `store.rs` tests with `gpui::TestAppContext`

**Interfaces:**
- Consumes: `SemanticGraph`, `GraphPatch`, `Intent` map
- Produces:
  - `SemanticGraphStore::new(cx) -> Self`
  - `fn snapshot(&self) -> SemanticGraphSnapshot`
  - `fn apply_patch(&mut self, patch: GraphPatch, cx: &mut Context<Self>) -> Result<()>`
  - `fn set_intents(&mut self, intents: Vec<Intent>, cx: &mut Context<Self>)`
  - `fn nodes_for_path(&self, path: &ProjectPath) -> Vec<NodeId>` (path matching via `SourceLocation` / module roots)
  - Event: `SemanticGraphEvent::Updated { revision: GraphRevision }`

- [ ] **Step 1: Write failing store test**

```rust
#[gpui::test]
async fn store_applies_patch_and_notifies(cx: &mut TestAppContext) {
    let store = cx.new(|cx| SemanticGraphStore::new(cx));
    let mut notified = false;
    // observe Updated via cx or subscription pattern used in sibling stores
    store.update(cx, |store, cx| {
        let key = NodeKey::Project {
            worktree_id: WorktreeId::from_usize(1),
        };
        let id = NodeId::from_key(&key);
        store
            .apply_patch(
                GraphPatch {
                    base: store.snapshot().revision,
                    removed_nodes: vec![],
                    removed_edges: vec![],
                    upsert_nodes: vec![Node::project(id, key, "p")],
                    upsert_edges: vec![],
                },
                cx,
            )
            .unwrap();
    });
    let snap = store.read_with(cx, |s, _| s.snapshot());
    assert_eq!(snap.graph.nodes.len(), 1);
    let _ = notified;
}
```

Mirror subscription style from `crates/project/src/buffer_store.rs` / worktree store tests (`cx.subscriber` / `EventEmitter`).

- [ ] **Step 2: Run test — fail**

```bash
cargo test -p semantic_graph --lib store
```

- [ ] **Step 3: Implement `SemanticGraphStore`**

```rust
pub struct SemanticGraphStore {
    graph: SemanticGraph,
    intents: IntentIndex,
    status: GraphStatus,
}

pub struct SemanticGraphSnapshot {
    pub revision: GraphRevision,
    pub graph: Arc<SemanticGraph>,
    pub intents: Arc<IntentIndex>,
    pub status: GraphStatus,
}

pub enum GraphStatus {
    Idle,
    Indexing,
    Partial { reason: SharedString },
    Error { message: SharedString },
}

pub enum SemanticGraphEvent {
    Updated { revision: GraphRevision },
}

impl EventEmitter<SemanticGraphEvent> for SemanticGraphStore {}
```

`apply_patch` updates graph, clones into `Arc` snapshot fields, `cx.emit(Updated…)`, `cx.notify()`.

- [ ] **Step 4: Tests pass + commit**

```bash
cargo test -p semantic_graph --lib
git add crates/semantic_graph
git commit -m "semantic_graph: add SemanticGraphStore snapshots"
```

---

### Task 3: Wire store into `Project`

**Files:**
- Modify: `crates/project/Cargo.toml`
- Modify: `crates/project/src/project.rs` (struct fields + `Project::local` / `Project::remote` / accessors)
- Modify: `crates/project/src/project.rs` tests or add focused test if pattern exists
- Test: `cargo test -p project --lib` (subset if full suite too heavy)

**Interfaces:**
- Consumes: `SemanticGraphStore::new`
- Produces:
  - `Project::semantic_graph(&self) -> &Entity<SemanticGraphStore>`
  - Store created in `Project::local` and `Project::remote` (remote may start empty until later plan)

- [ ] **Step 1: Add dependency**

`crates/project/Cargo.toml`:

```toml
semantic_graph.workspace = true
```

- [ ] **Step 2: Add field + constructor wiring**

In `Project` struct add:

```rust
semantic_graph: Entity<SemanticGraphStore>,
```

In `Project::local` (near other store creations ~line 1188+):

```rust
let semantic_graph = cx.new(|cx| SemanticGraphStore::new(cx));
```

Pass into the `Project { ... }` literal. Same for `Project::remote` with an empty store.

Add accessor:

```rust
pub fn semantic_graph(&self) -> &Entity<SemanticGraphStore> {
    &self.semantic_graph
}
```

- [ ] **Step 3: Compile project crate**

```bash
cargo check -p project
```

Expected: SUCCESS (fix any missing struct literal sites — search `Project {` in `project.rs` and test-support).

- [ ] **Step 4: Commit**

```bash
git add crates/project/Cargo.toml crates/project/src/project.rs Cargo.toml
git commit -m "project: own SemanticGraphStore"
```

---

### Task 4: Extractor traits + Cargo workspace extractor

**Files:**
- Create: `crates/semantic_graph/src/extract/traits.rs`
- Create: `crates/semantic_graph/src/extract/cargo.rs`
- Create: `crates/semantic_graph/src/extract/extract.rs`
- Create: `crates/semantic_graph/test_data/simple_workspace/Cargo.toml`
- Create: `crates/semantic_graph/test_data/simple_workspace/crates/app/Cargo.toml`
- Create: `crates/semantic_graph/test_data/simple_workspace/crates/app/src/main.rs`
- Create: `crates/semantic_graph/test_data/simple_workspace/crates/core_lib/Cargo.toml`
- Create: `crates/semantic_graph/test_data/simple_workspace/crates/core_lib/src/lib.rs`
- Modify: `crates/semantic_graph/Cargo.toml` (add `toml`, `fs`/`smol`/`async-std` as used by siblings — prefer `fs` crate patterns)
- Test: cargo extractor golden test

**Interfaces:**
- Consumes: filesystem paths to a Cargo workspace root
- Produces:
  - `trait SemanticExtractor`
  - `CargoWorkspaceExtractor`
  - `fn extract_cargo_workspace(root: &Path) -> Result<GraphPatch>` (sync helper for tests; async wrapper later)

- [ ] **Step 1: Create fixture**

`test_data/simple_workspace/Cargo.toml`:

```toml
[workspace]
members = ["crates/app", "crates/core_lib"]
resolver = "2"
```

`crates/core_lib/Cargo.toml`:

```toml
[package]
name = "core_lib"
version = "0.1.0"
edition = "2021"
description = "Core domain logic for the demo app"
```

`crates/core_lib/src/lib.rs`:

```rust
//! Core domain logic.

pub fn answer() -> i32 {
    42
}
```

`crates/app/Cargo.toml`:

```toml
[package]
name = "app"
version = "0.1.0"
edition = "2021"
description = "CLI entrypoint"

[dependencies]
core_lib = { path = "../core_lib" }
```

`crates/app/src/main.rs`:

```rust
fn main() {
    println!("{}", core_lib::answer());
}
```

- [ ] **Step 2: Write failing extractor test**

```rust
#[test]
fn cargo_extractor_emits_packages_and_depends_on() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_data/simple_workspace");
    let patch = extract_cargo_workspace(&root, WorktreeId::from_usize(1)).unwrap();
    let mut graph = SemanticGraph::default();
    graph.apply_patch(patch).unwrap();

    let names: BTreeSet<_> = graph
        .nodes
        .values()
        .filter(|n| n.kind == NodeKind::Module)
        .map(|n| n.display_name.to_string())
        .collect();
    assert!(names.contains("app"));
    assert!(names.contains("core_lib"));

    assert!(
        graph
            .edges
            .values()
            .any(|e| e.kind == EdgeKind::DependsOn),
        "expected DependsOn edge from app to core_lib"
    );
}
```

- [ ] **Step 3: Run — fail**

```bash
cargo test -p semantic_graph --lib cargo_extractor
```

- [ ] **Step 4: Implement Cargo extractor**

Parse root `Cargo.toml` with `toml::Value`:
- Read `workspace.members` globs/paths (start with literal paths; add glob via `util`/`globset` if workspace already depends on one — grep `globset` in repo).
- For each member manifest: package name, description, deps with `path =`, bin/lib targets.
- Emit:
  - `NodeKind::Project`
  - `Module` per package with `SourceLocation` to manifest or `src/lib.rs`/`src/main.rs`
  - `Entry` for bin/lib roots
  - `DependsOn` for path deps inside workspace
  - `External` for non-path deps (optional; can skip in first PR if noisy)
  - `Contains` Project→Module (subsystem clustering comes later)

Keep extraction **pure FS** in this task (no `Entity<Project>` yet) so unit tests stay lightweight.

- [ ] **Step 5: Tests pass + commit**

```bash
cargo test -p semantic_graph --lib
git add crates/semantic_graph
git commit -m "semantic_graph: Cargo workspace extractor"
```

---

### Task 5: Generic thin extractor + static intents + orchestration

**Files:**
- Create: `crates/semantic_graph/src/extract/generic.rs`
- Create: `crates/semantic_graph/src/intent/intent.rs`
- Create: `crates/semantic_graph/src/intent/static_provider.rs`
- Create: `crates/semantic_graph/src/invalidation.rs`
- Modify: `crates/semantic_graph/src/extract/extract.rs`
- Test: generic + intent tests

**Interfaces:**
- Produces:
  - `GenericThinExtractor::extract_root(root, worktree_id, max_depth) -> GraphPatch`
  - `StaticIntentProvider::intents_for_graph(graph, fs_reader) -> Vec<Intent>`
  - `GraphIndexer::reindex_cargo_or_generic(root) -> SemanticGraphSnapshot` (sync test helper)

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn generic_extractor_uses_readme_for_top_folder() {
    // create temp dir with README.md "# Widgets\n\nUI widgets." and src/
}

#[test]
fn static_intent_uses_crate_description() {
    // build graph from simple_workspace fixture; assert core_lib intent contains "domain"
}
```

- [ ] **Step 2: Implement generic extractor**

Walk top `max_depth` directories (skip `.git`, `target`, `node_modules`); create `Module` nodes; attach README first heading/paragraph as evidence on intents via static provider (not on node payload).

- [ ] **Step 3: Implement static intent provider**

For each `Module` with Cargo description or README evidence, create `Intent { source: Static, confidence: High|Medium, summary, evidence }`.

- [ ] **Step 4: Orchestration helper**

```rust
pub fn build_initial_graph(root: &Path, worktree_id: WorktreeId) -> Result<(SemanticGraph, IntentIndex)> {
    let mut graph = SemanticGraph::default();
    if root.join("Cargo.toml").is_file() {
        graph.apply_patch(extract_cargo_workspace(root, worktree_id)?)?;
    } else {
        graph.apply_patch(generic_extract(root, worktree_id, 2)?)?;
    }
    let intents = StaticIntentProvider::enrich(&graph, root)?;
    Ok((graph, intents))
}
```

- [ ] **Step 5: Test + commit**

```bash
cargo test -p semantic_graph --lib
git add crates/semantic_graph
git commit -m "semantic_graph: generic extractor and static intents"
```

---

### Task 6: Rust module depth extractor + subsystem clusterer

**Files:**
- Create: `crates/semantic_graph/src/extract/rust_modules.rs`
- Create: `crates/semantic_graph/src/extract/cluster.rs`
- Create: `crates/semantic_graph/test_data/simple_workspace/semantic_map.toml` (optional pins)
- Test: clustering membership + pin override

**Interfaces:**
- Produces:
  - `extract_rust_modules(package_root, package_node_id, depth) -> GraphPatch` (file modules under crate)
  - `cluster_subsystems(graph, ClusterConfig, pins) -> GraphPatch` adding `Subsystem` nodes + rewriting `Contains`

- [ ] **Step 1: Failing cluster test**

```rust
#[test]
fn clusterer_respects_pins() {
    // graph with modules a,b,c
    // pins say a,b in "ui"
    // assert Contains edges from subsystem:ui
}
```

- [ ] **Step 2: Implement rust_modules (minimal)**

Phase A minimal acceptable behavior: for each Cargo package, if `src/lib.rs` or `src/main.rs` exists, add child `Module` nodes for `src/*.rs` and `src/*/` directories up to `module_depth` (default 2). Do not fully parse `mod` trees yet if time-boxed; follow-up can use Tree-sitter. Document limitation in code comment only if non-obvious.

- [ ] **Step 3: Implement clusterer**

Algorithm from spec §5.3.3 (prefix + greedy affinity). Pins from `semantic_map.toml`:

```toml
[subsystems.ui]
members = ["crates/app"]
```

Parser: `toml` → `PinConfig`.

- [ ] **Step 4: Integrate into `build_initial_graph`**

Order: cargo → rust_modules → cluster → static intents.

- [ ] **Step 5: Test + commit**

```bash
cargo test -p semantic_graph --lib
git commit -am "semantic_graph: module depth and subsystem clustering"
```

---

### Task 7: Project-facing reindex task (async)

**Files:**
- Modify: `crates/semantic_graph/src/store.rs`
- Modify: `crates/semantic_graph/src/invalidation.rs`
- Modify: `crates/project/src/project.rs` (start initial index on local project when enabled — gate via settings in Task 8; for now always schedule but no-op if UI not present)
- Test: store status transitions Indexing → Idle

**Interfaces:**
- Produces:
  - `SemanticGraphStore::reindex(&mut self, root: Arc<Path>, worktree_id, cx)`
  - Uses `cx.background_spawn` for `build_initial_graph`, then foreground apply

- [ ] **Step 1: Implement async reindex**

```rust
pub fn reindex(
    &mut self,
    root: std::path::PathBuf,
    worktree_id: WorktreeId,
    cx: &mut Context<Self>,
) {
    self.status = GraphStatus::Indexing;
    cx.notify();
    let task = cx.background_spawn(async move { build_initial_graph(&root, worktree_id) });
    cx.spawn(async move |this, cx| {
        let result = task.await;
        this.update(cx, |this, cx| match result {
            Ok((graph, intents)) => {
                this.replace_graph(graph, intents, cx);
            }
            Err(error) => {
                this.status = GraphStatus::Error {
                    message: SharedString::from(format!("{error:#}")),
                };
                cx.notify();
            }
        })
        .ok();
    })
    .detach();
}
```

Follow exact `cx.spawn` / `WeakEntity` patterns from nearby Project stores (copy structure from `LspStore` or git store refresh).

- [ ] **Step 2: Trigger from `Project::local` after worktrees settle OR explicit `Project::reindex_semantic_graph(cx)` called by UI init**

Prefer explicit method on `Project` called by UI when settings enabled (Task 10), to avoid work when feature off:

```rust
pub fn reindex_semantic_graph(&self, cx: &mut App) {
    // resolve first worktree abs path + id; call store.reindex
}
```

- [ ] **Step 3: Unit/integration test with temp fixture dir + `TestAppContext`**

- [ ] **Step 4: Commit**

```bash
git commit -am "semantic_graph: async reindex into store"
```

---

### Task 8: Settings (`semantic_map`)

**Files:**
- Create: `crates/semantic_map_ui/Cargo.toml` (minimal, settings module first)
- Create: `crates/semantic_map_ui/src/semantic_map_ui.rs`
- Create: `crates/semantic_map_ui/src/settings.rs`
- Modify: `crates/settings_content/src/settings_content.rs`
- Modify: `/workspace/Cargo.toml` members + deps
- Test: settings default parse

**Interfaces:**
- Produces: `SemanticMapSettings` implementing `settings::Settings` with fields from spec §8.1 (`enabled` default `false`, `default_skin`, `hide_external`, `module_depth`, `max_auto_nodes`, `intent.llm` default `false`, cluster min/max)

- [ ] **Step 1: Scaffold UI crate with settings only**

Mirror `outline_panel` settings pattern (`outline_panel_settings.rs` + `settings_content` struct). Grep `OutlinePanelSettingsContent` and clone the registration pattern for `SemanticMapSettingsContent`.

- [ ] **Step 2: Defaults**

```rust
enabled: false,
default_skin: Vibe,
hide_external: true,
hide_tests: true,
module_depth: 3,
max_auto_nodes: 500,
intent_llm: false,
cluster_min: 3,
cluster_max: 16,
```

- [ ] **Step 3: `cargo check -p semantic_map_ui -p settings_content`**

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml crates/semantic_map_ui crates/settings_content
git commit -m "semantic_map_ui: add SemanticMap settings"
```

---

### Task 9: View model + shared selection

**Files:**
- Create: `crates/semantic_map_ui/src/view_model.rs`
- Create: `crates/semantic_map_ui/src/selection.rs`
- Test: lens filtering unit tests (no window)

**Interfaces:**
- Consumes: `SemanticGraphSnapshot`, `Lens`
- Produces:
  - `PanelViewModel { rows: Vec<PanelRow> }`
  - `CanvasViewModel { nodes: Vec<SceneNode>, edges: Vec<SceneEdge> }`
  - `SemanticMapSelection` entity: `selected: Vec<NodeId>`, methods `select`, `clear`

- [ ] **Step 1: Failing test — lens hides externals**

```rust
#[test]
fn panel_view_hides_external_when_requested() { /* ... */ }
```

- [ ] **Step 2: Implement DFS for panel rows from `Contains` starting at Project**

Row fields: `node_id`, `depth`, `name`, `intent_summary: Option<SharedString>`, `kind`.

- [ ] **Step 3: Canvas view model uses hierarchy layout placeholder**

For each visible Module/Subsystem assign grid positions (`x = column * 240`, `y = row * 120`) until Task 12 adds real layout. Store in `SceneNode.rect` using `f32` space; UI maps to pixels.

- [ ] **Step 4: Commit**

```bash
git commit -am "semantic_map_ui: view models and selection state"
```

---

### Task 10: `SemanticMapPanel` dock panel

**Files:**
- Create: `crates/semantic_map_ui/src/panel.rs`
- Modify: `crates/semantic_map_ui/src/semantic_map_ui.rs` (`init`, `actions!`)
- Modify: `crates/zed/Cargo.toml`, `crates/zed/src/main.rs`, `crates/zed/src/zed.rs`
- Test: GPUI test that panel renders rows from stub store OR project fixture

**Interfaces:**
- Consumes: `Project::semantic_graph`, settings, selection
- Produces: `SemanticMapPanel` implementing `Panel + Focusable + Render`
- Actions: `semantic_map::ToggleFocus`, `semantic_map::OpenSelectedSource`, `semantic_map::Reindex`

- [ ] **Step 1: Copy structural patterns from `outline_panel` / `project_panel` for `Panel` impl**

Essential pieces:
- `fn position` / `set_position` from settings dock side
- `fn persistent_name() -> "Semantic Map"`
- `fn icon` / `icon_tooltip`
- `fn toggle_action`
- Focus handle
- `workspace.add_panel` registration in `semantic_map_ui::init`

- [ ] **Step 2: Render `uniform_list` of panel rows**

Show name + truncated intent. Click updates `SemanticMapSelection` and calls open path when double-clicked via `workspace.open_path`.

- [ ] **Step 3: Wire `init`**

```rust
pub fn init(cx: &mut App) {
    SemanticMapSettings::register(cx);
    cx.observe_new(|workspace: &mut Workspace, _, cx| {
        let panel = cx.new(|cx| SemanticMapPanel::new(workspace, cx));
        workspace.add_panel(panel, cx);
    })
    .detach();
}
```

Match exact `observe_new` / `add_panel` signatures used by `project_panel::init`.

- [ ] **Step 4: Call `semantic_map_ui::init(cx)` from `crates/zed/src/main.rs` next to `outline_panel::init`**

Also add to test harnesses in `zed.rs` / `visual_tests.rs` if required for compile.

- [ ] **Step 5: When settings.enabled and panel first focused, call `project.reindex_semantic_graph(cx)`**

- [ ] **Step 6: Manual check / GPUI test**

```bash
cargo test -p semantic_map_ui --lib
cargo check -p zed
```

- [ ] **Step 7: Commit**

```bash
git commit -am "semantic_map_ui: add Semantic Map panel"
```

---

### Task 11: Open source navigation from panel

**Files:**
- Modify: `crates/semantic_map_ui/src/panel.rs`
- Test: unit test mapping `SourceLocation` → `ProjectPath`

**Interfaces:**
- Produces: `fn open_node_source(workspace, project, node, cx)` opens file at `node.location`

- [ ] **Step 1: Implement open helper using `Workspace::open_path` / existing project path open APIs** (grep `open_path` in `workspace.rs` for exact signature).

- [ ] **Step 2: Bind double-click / `OpenSelectedSource` action**

- [ ] **Step 3: Commit**

```bash
git commit -am "semantic_map_ui: open source from selected node"
```

---

### Task 12: Canvas item + vibe skin element

**Files:**
- Create: `crates/semantic_map_ui/src/canvas/item.rs`
- Create: `crates/semantic_map_ui/src/canvas/element.rs`
- Create: `crates/semantic_map_ui/src/canvas/skins/vibe.rs`
- Create: `crates/semantic_graph/src/layout/hierarchy.rs`
- Modify: panel context menu / action `semantic_map::OpenCanvas`
- Test: layout positions deterministic for fixture graph; item registers as workspace item

**Interfaces:**
- Produces:
  - `SemanticMapItem` implementing `Item + Focusable + Render`
  - `SemanticMapCanvasElement` painting cards + lines
  - `hierarchy::layout(graph, lens) -> HashMap<NodeId, (f32, f32)>`

- [ ] **Step 1: Hierarchy layout function + test**

Deterministic order by `display_name`; subsystems on row 0, modules under their subsystem on subsequent rows.

- [ ] **Step 2: `SemanticMapItem::new(project, selection, cx)`**

Render child canvas element with camera (pan/zoom state on item).

- [ ] **Step 3: Element paint**

For each `SceneNode`, draw a rounded rect via GPUI `paint_quad` / div-based approximate layout first. Acceptable Phase A approach: **implement canvas as positioned `div` children inside a pannable container** (faster than full custom element). If using divs, skip custom `Element` and document that hit-testing uses GPUI layout.

Recommended Phase A pragmatism: **div-based canvas** in `item.rs` using absolute offsets from layout map; defer custom `Element` to polish task if needed.

- [ ] **Step 4: Edges as SVG-like lines**

If div-only is too weak for edges, draw edges with `canvas` / `window.paint_path` in a thin custom element wrapper. Minimum: show dependency list in a detail pane when a node is selected (must have **some** DependsOn visibility). Prefer visible lines.

- [ ] **Step 5: Action opens item in active pane**

```rust
workspace.active_pane().update(cx, |pane, cx| {
    pane.add_item(Box::new(item), true, true, None, cx);
});
```

Match `Pane::add_item` signature from `workspace/src/pane.rs`.

- [ ] **Step 6: `cargo check -p semantic_map_ui -p zed` + tests**

- [ ] **Step 7: Commit**

```bash
git commit -am "semantic_map_ui: central Semantic Map canvas"
```

---

### Task 13: Layout pins + `semantic_map.toml` load on index

**Files:**
- Create: `crates/semantic_graph/src/layout/pins.rs`
- Modify: cluster + indexer to load repo `semantic_map.toml`
- Modify: canvas drag handler to update pins in window state; persist via `db::kvp` or workspace DB key `semantic-map-pins:{workspace_id}`
- Test: toml pin parse; pin overrides cluster

- [ ] **Step 1: Parse pins (already partially in Task 6) — persist canvas positions separately from subsystem pins**

```rust
pub struct CanvasPins {
    pub positions: HashMap<NodeKey, (f32, f32)>,
}
```

- [ ] **Step 2: On node drag end, save pin; layout uses pin if present**

- [ ] **Step 3: Commit**

```bash
git commit -am "semantic_map: canvas pins and repo subsystem pins"
```

---

### Task 14: Status UX, truncation budgets, reindex action polish

**Files:**
- Modify: `store.rs`, panel header, settings `max_auto_nodes`
- Test: when modules > max, status becomes `Partial`

- [ ] **Step 1: Enforce `max_auto_nodes` during patch build**

- [ ] **Step 2: Panel header chips: Indexing / Ready / Partial / Error**

- [ ] **Step 3: `semantic_map::Reindex` clears and rebuilds**

- [ ] **Step 4: Commit**

```bash
git commit -am "semantic_map: indexing status and node budgets"
```

---

### Task 15: Optional LLM intent provider stub (settings-gated)

**Files:**
- Create: `crates/semantic_graph/src/intent/llm_provider.rs`
- Modify: orchestration after static intents
- Test: with `intent_llm=false`, provider not called; with true and missing model service, leave static intents intact (no error banner required)

**Interfaces:**
- Produces: `LlmIntentProvider` that is a **stub** returning `Ok(Vec::new())` until wired to `language_model` in a follow-up PR. Must not block Phase A dogfood.

- [ ] **Step 1: Implement stub + hook point**

```rust
pub struct LlmIntentProvider;

impl LlmIntentProvider {
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
```

- [ ] **Step 2: Commit**

```bash
git commit -am "semantic_map: stub LLM intent provider behind settings"
```

---

### Task 16: Dogfood docs + enablement notes

**Files:**
- Create: `docs/src/semantic-map.md` (user-facing, concise)
- Modify: `docs/src/SUMMARY.md` to link it
- Modify: design spec status line to `Phase A implementation planned`

- [ ] **Step 1: Write user doc covering enable setting, panel toggle, canvas open, pins, privacy (LLM off by default)**

- [ ] **Step 2: Prettier check if required by docs AGENTS**

```bash
cd docs && npx prettier --write src/semantic-map.md src/SUMMARY.md
```

- [ ] **Step 3: Commit**

```bash
git add docs
git commit -m "docs: add Semantic Map user guide for Phase A"
```

---

### Task 17: End-to-end verification checklist (manual + automated)

**Files:** none new (checklist execution)

- [ ] **Step 1: Automated**

```bash
cargo test -p semantic_graph --lib
cargo test -p semantic_map_ui --lib
cargo check -p zed
```

Expected: all PASS / SUCCESS.

- [ ] **Step 2: Manual dogfood (local build)**

1. Set `"semantic_map": { "enabled": true }` in settings.
2. Open `test_data/simple_workspace` via Zed.
3. Open Semantic Map panel → see `app`, `core_lib`, intents from descriptions.
4. Open canvas → see DependsOn relationship.
5. Double-click `core_lib` → opens lib source.
6. Open Zed self-repo (optional) → graph Partial or truncated OK; no crash.

- [ ] **Step 3: Fix blockers found in dogfood; commit**

```bash
git commit -am "semantic_map: dogfood fixes after Phase A checklist"
```

---

## Spec coverage mapping (Phase A)

| Spec section | Tasks |
|--------------|-------|
| §4 IR | Task 1–2 |
| §5 Cargo/generic/cluster/rust modules | Tasks 4–6 |
| §5 invalidation/reindex | Task 7, 14 |
| §6 Static intent | Task 5 |
| §6 LLM | Task 15 (stub only) |
| §7 Panel | Tasks 9–11 |
| §7 Canvas + vibe skin | Task 12–13 |
| §8 Settings + toml pins | Tasks 8, 13 |
| §9 Phase A acceptance | Task 17 |
| §11 Remote proto | Deferred (later plan) |
| Phase C/B | Deferred (later plans) |

## Follow-up plans (do not implement here)

1. `2026-08-XX-semantic-map-phase-c.md` — agent heat/trail + agent tools  
2. `2026-08-XX-semantic-map-phase-b.md` — structural edits + design nodes  
3. `2026-08-XX-semantic-map-remote.md` — host-side extraction proto  

---

## Plan self-review notes

- No Phase B/C implementation tasks included (scoped).  
- LLM is explicitly a stub so Phase A is offline-complete.  
- Canvas prefers div-based first implementation to reduce GPUI risk.  
- Types introduced in Task 1 are the names used throughout later tasks.  
