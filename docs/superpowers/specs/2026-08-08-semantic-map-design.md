# Semantic Map Design

**Date:** 2026-08-08  
**Status:** Phase A implementation planned  
**Working title:** Semantic Map — project-structure visualization for vibe coding on Zed  
**Primary crates (proposed):** `semantic_graph`, `semantic_map_ui`

---

## 1. Problem and product intent

### 1.1 The user question

When a developer (or an agent-assisted “vibe coder”) opens an unfamiliar repository, the question that matters first is not “which function calls which”, and not “draw me a complete UML class diagram”. It is:

> **What project structure does this code describe, and what is it doing?**

Secondary questions follow only after that mental model exists:

- Where should I change behavior X?
- What subsystem did the agent just touch?
- What design am I trying to grow toward?

### 1.2 Product thesis

Ship a **first-class semantic map** inside Zed:

1. A language-independent **SemanticGraph IR** is the source of truth for structure.
2. **Views** (panel tree, spatial canvas, UML/C4 skins) are projections of that IR.
3. **Intent** (“what this part does”) is a layered annotation: reliable static evidence first, optional LLM enrichment second.
4. Programming-by-diagram is a **late phase**. Early phases must already make large codebases legible.

### 1.3 Phased outcomes

| Phase | Name | User-visible outcome |
|-------|------|----------------------|
| **A** | Read & orient | Open a repo → see subsystems/modules, short “what it does”, jump to code |
| **C** | Agent co-view | While agent edits, the map highlights which semantic nodes are changing |
| **B** | Diagram-driven design | Edit structure / design intent on the map; drive code generation & refactors |

Phase lettering follows the brainstorming decision order: **A → C → B** (understand, then co-observe agents, then author via diagrams).

### 1.4 Non-goals (global)

- Replacing the text editor as the precision surface for all edits.
- Guaranteeing a single “correct” subsystem clustering for every repo.
- Building a general-purpose Visio/draw.io clone.
- Requiring network/LLM for the map to function.
- Treating UML (or any single standard) as the IR.

### 1.5 Success metrics

**Phase A (must pass):**

- On a cold open of a mid-size Rust workspace (~50–200 crates, e.g. Zed itself), a new contributor can answer in ~5 minutes: main subsystems, each one’s job, primary entry points — without reading source linearly.
- Map remains usable offline with LLM intent disabled.
- Clicking a node opens the correct file/symbol with existing Zed navigation latency expectations.

**Phase C:**

- During an agent turn that edits N files, the map visually answers “which subsystems were affected” without opening the diff first.

**Phase B:**

- A structural change initiated from the map (rename module, scaffold crate, link design node) lands as real buffer/FS operations with clear undo/audit.

---

## 2. Constraints from Zed architecture

These are load-bearing; the feature must fit them rather than invent a parallel app.

### 2.1 UI and state

- UI is **GPUI**: entities, `Render` / custom `Element`, actions, docks/panes.
- Domain state lives under **`Project`** (`Entity<Project>`), not only in window-local UI.
- Text positions that must survive edits should use **`text::Anchor`** / multibuffer anchors, not raw offsets alone.

### 2.2 Existing building blocks to reuse

| Building block | Reuse |
|----------------|-------|
| `Worktree` / `WorktreeStore` | File graph, ignore rules, change events |
| `language::Buffer` / `BufferStore` | Opened file text, parse trees |
| `language::Outline` / outline queries | Symbol outlines from Tree-sitter |
| `LspStore` | `documentSymbol`, `workspace/symbol`, call/type hierarchy when available |
| `ProjectPanel` / `OutlinePanel` | UX patterns for docks; **not** the semantic truth |
| `Workspace` pane items + docks | Host `SemanticMapItem` + `SemanticMapPanel` |
| `agent` / `action_log` / `BufferEditSource::Agent` | Phase C overlays |
| `ReplicaId::AGENT` | Agent cursor visualization already exists; map can mirror “agent focus” |
| Extension host (`ExtensionHostProxy`) | Optional language extractors / renderers later |

### 2.3 Delivery shape

Agreed hybrid delivery:

- **Core IR + store + canvas/panel**: in-tree crates (or a fork that keeps PR-able boundaries).
- **Language adapters, intent strategies, diagram skins**: trait-based; some in-tree, more via extensions over time.
- WASM extensions alone are **insufficient** for a native GPUI canvas of this ambition; do not plan MVP as pure extension.

### 2.4 Local / remote / collab

| Mode | Phase A expectation |
|------|---------------------|
| Local project | Extract on UI machine |
| Remote (`RemoteClient` + `remote_server`) | Prefer extraction near FS/LSP on headless host; UI consumes snapshots/patches over proto |
| Collab guest | Read-only map from host snapshot initially; editing layout pins can stay local |

Exact remote proto messages are specified in §11.

---

## 3. High-level architecture

```text
Workspace
├── SemanticMapPanel  (Dock Panel)
└── SemanticMapItem   (Center Pane Item)
         │
         ▼
Project
└── Entity<SemanticGraphStore>
         ├── SemanticGraph (IR snapshot)
         ├── IntentStore (cached intents)
         ├── ExtractorRegistry
         ├── LayoutStore (pinned positions, camera)
         └── InvalidationEngine
                │
                ├── Worktree events
                ├── Buffer events
                ├── LspStore capabilities / symbols
                └── (Phase C) Agent / ActionLog events
```

### 3.1 Crate split

#### `crates/semantic_graph`

Pure-ish domain crate (minimal GPUI surface: `Entity` store is OK; no rendering).

Responsibilities:

- IR types
- Graph apply/patch/snapshot
- Extractor traits + Rust deep extractor + generic thin extractor
- Intent providers (static + optional LLM bridge via injected service)
- Index persistence
- Tests for merge/invalidation/clustering heuristics

#### `crates/semantic_map_ui`

GPUI UI crate.

Responsibilities:

- `SemanticMapPanel`
- `SemanticMapItem` + canvas element
- Layout algorithms invocation (may call into `semantic_graph::layout`)
- Actions, settings UI hooks
- Wiring to `Workspace` / `Project`

#### Integration sites (existing crates)

| Crate | Change |
|-------|--------|
| `project` | Own `Entity<SemanticGraphStore>` or weak handle; init/teardown; remote facade |
| `workspace` | Register panel + item serializers |
| `zed` | `init` registration like other panels |
| `proto` / `remote_server` | Phase A.5+ remote extraction messages |
| `agent` / `agent_ui` | Phase C tool + overlays |
| `language_extension` / `extension` | Optional extractor/renderer proxies |

### 3.2 Dependency direction

```text
semantic_map_ui → semantic_graph → project APIs (worktree, lsp, buffers)
                               → language / text / gpui (store only)
zed / workspace → semantic_map_ui
agent (Phase C) → semantic_graph (query API)
```

Avoid `semantic_graph` depending on `agent_ui` or `editor` UI types. Jump-to-code goes through `Project` path APIs + workspace navigation callbacks.

---

## 4. SemanticGraph IR (detailed)

### 4.1 Design principles

1. **Language-independent kinds** with language-specific payload extensions.
2. **Stable IDs** across mild refactors (file rename may update payload but can keep id if we track moves; v1 may recreate ids on path change and accept churn).
3. **Incremental patches**, not full rebuilds on every keystroke.
4. **Evidence-backed intent**, never free-floating marketing text without sources when static.
5. **Lens-friendly**: full graph may be large; views query a `Lens` (depth, kinds, focus root).

### 4.2 Identifiers

```rust
// Conceptual types — names illustrative

struct NodeId(u64); // or packed newtype over stable key hash
struct EdgeId(u64);

/// Stable key used to create NodeId. Stored for debugging & remapping.
enum NodeKey {
    Project { worktree_root_key: WorktreeId },
    Subsystem { project: NodeKeyRef, slug: SharedString },
    Module {
        worktree_id: WorktreeId,
        /// Canonical path relative to worktree, or Cargo package id.
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
        ecosystem: EcosystemKind, // Cargo, Npm, ...
        name: SharedString,
        version_req: Option<SharedString>,
    },
}

enum ModuleRef {
    CargoPackage { package_name: SharedString, manifest_dir: RelPath },
    PathModule { path: RelPath },          // file or directory module root
    LanguagePackage { system: SharedString, name: SharedString, root: RelPath },
}

struct SymbolKey {
    /// Language-appropriate qualified name if available, else outline name path.
    qualified_name: SharedString,
    kind: SymbolKind,
}
```

**ID stability policy (Phase A):**

- `Module` for Cargo packages: key = package name + manifest dir relative path.
- Path modules: key = normalized relative path.
- `Subsystem` slug: stable string chosen by clusterer (`ui`, `editor`, `agent`, …); pinned overrides win.
- On package rename: treat as delete+add unless explicit rename event exists (v1 OK).

### 4.3 Node

```rust
struct Node {
    id: NodeId,
    key: NodeKey,
    kind: NodeKind,
    display_name: SharedString,
    /// Short label for canvas (may equal display_name).
    abbrev: Option<SharedString>,
    location: Option<SourceLocation>,
    payload: NodePayload,
    flags: NodeFlags,
}

enum NodeKind {
    Project,
    Subsystem,
    Module,
    Entry,
    Type,
    External,
}

struct SourceLocation {
    project_path: ProjectPath,
    /// Prefer anchors when buffer open; otherwise UTF-16/point range snapshot.
    range: Option<Range<Point>>,
    symbol: Option<SymbolKey>,
}

struct NodeFlags {
    is_entry_point: bool,
    is_generated: bool,
    is_test: bool,
    is_external: bool,
    user_pinned_subsystem: bool,
}

enum NodePayload {
    Project(ProjectPayload),
    Subsystem(SubsystemPayload),
    Module(ModulePayload),
    Entry(EntryPayload),
    Type(TypePayload),
    External(ExternalPayload),
}

struct ModulePayload {
    language: Option<LanguageName>,
    module_kind: ModuleKind, // CrateLib, CrateBin, FileModule, Package, Folder
    public_exports: Vec<SymbolKey>, // capped
    deps_out_count: u32,
    deps_in_count: u32,
    loc_estimate: Option<u32>,
}

struct SubsystemPayload {
    /// Modules contained (also implied by Contains edges; denormalized for UI).
    member_count: u32,
    cluster_score: f32,
    pinned: bool,
}

struct EntryPayload {
    entry_kind: EntryKind, // Main, LibRoot, BinTarget, PublicApi, Custom
}

struct TypePayload {
    type_kind: TypeKind, // Struct, Enum, Trait, Interface, Class, TypeAlias, ...
}

struct ExternalPayload {
    ecosystem: EcosystemKind,
}
```

### 4.4 Edge

```rust
struct Edge {
    id: EdgeId,
    kind: EdgeKind,
    from: NodeId,
    to: NodeId,
    weight: f32,          // e.g. import strength / call count estimate
    location: Option<SourceLocation>, // evidence of the relation
    payload: EdgePayload,
}

enum EdgeKind {
    Contains,
    DependsOn,
    Exposes,
    Implements,   // Phase A optional via LSP
    Calls,        // lazy / Phase A drill-down
    References,   // future
    DesignLinks,  // Phase B
}
```

**Cardinality notes:**

- `Contains` forms a forest under `Project` (subsystems may overlap? **v1: no overlap** — each Module has one Subsystem parent; unclustered modules hang under a synthetic `uncategorized` subsystem).
- `DependsOn` is directed, may have cycles; UI must tolerate cycles.
- `Exposes` links Module → Entry (and later Type).

### 4.5 Intent

```rust
struct Intent {
    subject: NodeId,
    summary: SharedString,          // 1–2 sentences, plain language
    bullets: SmallVec<[SharedString; 3]>, // optional key responsibilities
    confidence: Confidence,         // Low | Medium | High
    source: IntentSource,
    evidence: Vec<Evidence>,
    updated_at: Timestamp,
    content_hash: ContentHash,      // hash of evidence + prompt version
}

enum IntentSource {
    Static,
    Llm { provider: SharedString, model: SharedString, prompt_version: u32 },
    Mixed,
}

struct Evidence {
    kind: EvidenceKind,
    location: Option<SourceLocation>,
    excerpt: SharedString, // capped length
    weight: f32,
}

enum EvidenceKind {
    ReadmeSection,
    CrateDescription,
    InnerDocComment,
    ModuleDocComment,
    SymbolNameHeuristic,
    ExportList,
    ManifestMetadata,
    LlmCitation, // model pointed at a span
}
```

**Rules:**

- Static provider must fill `evidence` whenever it fills `summary`.
- LLM provider may only run if enabled in settings and subject is in the current dirty/enrichment queue.
- UI must show evidence on demand (“Why this summary?”).

### 4.6 Graph container & snapshots

```rust
struct SemanticGraph {
    revision: GraphRevision,
    nodes: HashMap<NodeId, Node>,
    edges: HashMap<EdgeId, Edge>,
    /// Indexes
    by_key: HashMap<NodeKey, NodeId>,
    children: HashMap<NodeId, Vec<NodeId>>, // Contains
    dependents: HashMap<NodeId, Vec<NodeId>>,
    dependencies: HashMap<NodeId, Vec<NodeId>>,
}

struct GraphRevision(u64);

struct GraphPatch {
    base: GraphRevision,
    removed_nodes: Vec<NodeId>,
    removed_edges: Vec<EdgeId>,
    upsert_nodes: Vec<Node>,
    upsert_edges: Vec<Edge>,
}

struct SemanticGraphSnapshot {
    revision: GraphRevision,
    graph: Arc<SemanticGraph>,
    intents: Arc<IntentIndex>,
}
```

`SemanticGraphStore` holds the mutable graph, applies patches serially on the model owner, publishes immutable `Arc` snapshots to UI via `cx.notify()` / events.

### 4.7 Lens (view query)

```rust
struct Lens {
    root: Option<NodeId>,          // None = project root
    allowed_kinds: EnumSet<NodeKind>,
    max_depth: Option<u32>,
    hide_external: bool,
    hide_tests: bool,
    focus: Option<FocusQuery>,     // search string / node ids
    edge_kinds: EnumSet<EdgeKind>,
}

struct FocusQuery {
    text: Option<SharedString>,
    node_ids: Vec<NodeId>,
}
```

Renderers and panel both request `store.view(lens) -> ViewModel`.

---

## 5. Extraction pipeline

### 5.1 Pipeline stages

```text
[Change events]
    Worktree entry changes
    Buffer edits (debounced)
    Cargo/manifest changes
    Language server ready / restart
         │
         ▼
InvalidationEngine
    map paths → affected NodeKeys / Extractor jobs
         │
         ▼
Job scheduler (background)
    coalesce jobs per extractor
         │
         ▼
Extractor::extract(ExtractCtx) -> GraphPatch
         │
         ▼
SemanticGraphStore::apply(patch)
         │
         ▼
IntentLayer::refresh(affected NodeIds)
         │
         ▼
Emit SemanticGraphEvent::Updated { revision, dirty_regions }
         │
         ▼
UI recompute ViewModel / layout as needed
```

### 5.2 Extractor trait

```rust
trait SemanticExtractor: Send + Sync {
    fn id(&self) -> &'static str;
    fn priority(&self) -> i32; // higher runs first for conflicts
    fn interested_in(&self, event: &InvalidationEvent) -> bool;
    fn extract(&self, ctx: ExtractCtx) -> Task<Result<GraphPatch>>;
}

struct ExtractCtx {
    project: Entity<Project>, // or narrower ports for testability
    worktree_snapshot: WorktreeSnapshot,
    dirty_paths: Vec<RelPath>,
    language_registry: Arc<LanguageRegistry>,
    lsp: LspQueryPort,       // async queries, may no-op
    previous: Arc<SemanticGraph>,
    budget: ExtractBudget,   // time/node caps
}
```

**Conflict policy:** If two extractors upsert the same `NodeKey`, higher `priority` wins field-wise; edges unioned with dedupe by `(kind, from, to)`.

### 5.3 Built-in extractors (Phase A)

#### 5.3.1 `cargo_workspace_extractor` (Rust deep)

Inputs:

- `Cargo.toml` workspace members / excludes
- Per-package manifests (name, targets, dependencies, description)
- `Cargo.lock` optional for external versions (not required for MVP structure)

Emits:

- `Module` nodes for packages (`ModuleKind::CrateLib` / `CrateBin` / etc.)
- `DependsOn` between workspace packages
- `External` nodes for third-party deps (aggregated; default collapsed)
- `Entry` for bin targets and `lib.rs` roots
- Static intent seeds from package `description`

#### 5.3.2 `rust_module_tree_extractor`

Inputs:

- Package source dirs
- Tree-sitter / rust parse for `mod`, `pub mod`, path attributes
- File outline queries where available

Emits:

- Nested `Module` nodes under package
- `Contains` edges
- Doc comment evidence for intents

**Depth control:** Default index only down to a configurable module depth (e.g. 2–3 under crate root). Deeper modules load on drill-down.

#### 5.3.3 `subsystem_clusterer`

Not a language parser — a graph pass:

Inputs: Module dependency graph + path prefixes + optional pin file  
Outputs: `Subsystem` nodes + `Contains` Module membership

**v1 algorithm (documented, replaceable):**

1. Start with directory prefixes under repo root (`crates/`, `apps/`, …) OR Cargo package name prefixes.
2. Build undirected affinity from `DependsOn` within workspace.
3. Greedy agglomeration until subsystem count ∈ `[min, max]` (settings).
4. Name from common path segment or package prefix; fallback `subsystem-N`.
5. Apply user pins from `semantic_map.toml` / settings (absolute membership overrides).

#### 5.3.4 `generic_thin_extractor` (multi-language baseline)

Inputs:

- Worktree directories (respect ignore)
- Language detection via `LanguageRegistry`
- README.md / docs near roots
- Heuristic entry files (`main.ts`, `index.ts`, `cmd/`, `src/main.py`, etc.)

Emits:

- Folder `Module`s for top N directory levels
- Entries when heuristics hit
- Static intents from README first heading + first paragraph

This guarantees non-Rust repos still get a map.

#### 5.3.5 `lsp_symbol_enricher` (optional enhancement)

Inputs: LSP document/workspace symbols when available  
Emits: `Type` / `Entry` nodes, `Implements` edges when hierarchy APIs exist  

Must tolerate missing server, partial capability, timeouts. Never block static graph.

### 5.4 Invalidation rules (Phase A)

| Event | Invalidate |
|-------|------------|
| `Cargo.toml` change | Workspace packages + deps edges |
| File add/remove under package | Module tree for that package |
| Buffer edit in `.rs` | Debounce 200–500ms; re-extract that file’s module docs/outline; delay full package recluster |
| rust-analyzer restart | Enrichment edges only |
| Settings pin change | Subsystem membership only |

Keystroke path must **not** rebuild whole workspace graph.

### 5.5 Budgets and progressive disclosure

```rust
struct ExtractBudget {
    max_nodes: usize,           // hard cap for auto graph
    max_external_nodes: usize,
    max_wall_time: Duration,
    max_file_bytes_scanned: usize,
}
```

When capped: set `GraphStatus::Partial { reason }` and let UI show “Load more / Focus this subsystem”.

---

## 6. Intent layer

### 6.1 Static provider algorithm

For a `Module` / `Subsystem`:

1. Collect evidence candidates (ordered):
   - package description / README under module root
   - `//!` or `/**` module docs (first paragraph)
   - bin target names
   - top exported types/functions (names only)
2. Compose summary with templates, e.g.:
   - `"{name}: {description}"`
   - or `"{name} — {crate_role} used by {dependents}. Exports {top_exports}."`
3. Confidence: High if description/README; Medium if only docs; Low if only names.

For `Subsystem`: summarize from member module static intents (concatenate / TF heuristics); LLM optional later.

### 6.2 LLM provider

**Trigger:**

- User enabled `semantic_map.intent.llm = true`
- Node visible in current lens OR explicitly requested
- `content_hash` miss

**Prompt inputs (bounded):**

- Node name/kind
- Static evidence excerpts (token capped)
- Dependency neighbor names (not full source)
- Optional: top outline symbols

**Outputs (JSON schema):**

```json
{
  "summary": "…",
  "bullets": ["…", "…"],
  "confidence": "medium"
}
```

**Safety / privacy:**

- Default off in Phase A unless product decides opt-in aligned with Zed agent settings.
- Respect project trust / disable for untrusted worktrees.
- Prefer existing Zed model routing (`language_model` stack) rather than new HTTP stack.
- Cache on disk under project cache dir; no requirement to upload whole repo.

### 6.3 IntentStore persistence

```text
~/.cache/zed/semantic_map/<project_hash>/intents.jsonl
```

Or workspace DB table via `db`/`sqlez` if we want sync with workspace sessions. Phase A: file cache is enough.

---

## 7. UI design

### 7.1 Information architecture

Two coordinated surfaces:

1. **Panel** — hierarchical literacy (fast scan, search, summaries).
2. **Canvas** — spatial literacy (boundaries, dependency direction, focus).

Selection is shared via `SemanticMapUiState` entity (window-local), referencing `NodeId`s + graph revision.

### 7.2 SemanticMapPanel

**Dock:** default left or right (near Project Panel); user-movable via `Panel` trait.

**Contents:**

- Header: project name, status chip (`Ready` / `Indexing…` / `Partial` / `LLM intents on`)
- Search box (name + intent text)
- Tree:
  - Project
    - Subsystem
      - Module
        - Entry (optional expansion)
- Row: icon by kind, name, one-line intent truncated
- Context menu: Open source, Focus on canvas, Copy node id, Pin to subsystem…, Refresh intent

**Interactions:**

| Input | Result |
|-------|--------|
| Click | Select + reveal on canvas if open |
| Double-click / Enter | Open primary `SourceLocation` in editor |
| Alt-click | Open README/docs evidence |
| Filter | Hide non-matching subtrees |

### 7.3 SemanticMapItem (canvas)

**Item traits:** `Item + EventEmitter + Focusable + SerializableItem` (serialize camera + lens + pins).

**Scene graph (render model, not IR):**

```rust
struct DiagramScene {
    nodes: Vec<SceneNode>,
    edges: Vec<SceneEdge>,
    camera: Camera,
}

struct SceneNode {
    id: NodeId,
    kind: NodeKind,
    rect: Rect<Pixels>,
    title: SharedString,
    subtitle: Option<SharedString>, // intent
    selected: bool,
    dimmed: bool,
    agent_heat: f32, // Phase C
}

struct SceneEdge {
    id: EdgeId,
    from: NodeId,
    to: NodeId,
    kind: EdgeKind,
    routed_path: Vec<Point<Pixels>>,
}
```

**Renderer skins (`DiagramRenderer`):**

| Skin | Default lens | Notes |
|------|--------------|-------|
| `vibe` | Subsystem+Module, DependsOn+Contains | Default; readable cards |
| `c4_container` | Subsystem as containers | Labels follow C4 vocabulary |
| `uml_package` | Modules as packages | Phase A optional |
| `uml_class` | Types under focused module | Drill-down only |

Skins must not invent nodes absent from IR (except synthetic layout helpers).

**Canvas interactions (Phase A):**

- Pan (middle mouse / trackpad), zoom
- Click select, shift multi-select
- Drag node → write pin to `LayoutStore`
- Double-click Module → open source or expand children lens
- Hover edge → tooltip with evidence path
- Toolbar: skin select, hide externals, re-run layout, open panel focus

**Layout:**

- Auto: layered layout for DependsOn (Sugiyama-style or simpler hierarchical + crossing reduction); fallback force-directed for cycles.
- Pinned positions override auto for those nodes.
- Persist pins in workspace DB (`SemanticMapLayout` table) keyed by workspace id + node key.

### 7.4 Empty / loading / error states

- No project: prompt to open folder
- Indexing: skeleton cards + progress
- Extractor error: banner with retry; keep last good snapshot
- Partial budget: explicit “graph truncated” affordance

### 7.5 Actions (initial set)

```text
semantic_map::TogglePanel
semantic_map::OpenCanvas
semantic_map::FocusSelection
semantic_map::OpenSource
semantic_map::Reindex
semantic_map::ToggleExternalDeps
semantic_map::CycleSkin
semantic_map::RefreshIntent
semantic_map::PinSelectionToSubsystem  // settings/toml edit helper
```

---

## 8. Settings and repo config

### 8.1 User settings (`settings.json`)

```json
{
  "semantic_map": {
    "enabled": true,
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
}
```

### 8.2 Repo config (`semantic_map.toml` at root, optional)

```toml
[subsystems.ui]
members = ["crates/gpui", "crates/ui", "crates/theme"]
summary = "GPU UI framework and shared widgets"

[subsystems.editor]
members = ["crates/editor", "crates/multi_buffer", "crates/language"]

[pins.modules."crates/sandbox"]
subsystem = "agent"
```

Repo config overrides heuristics; committed to git for team-shared maps.

---

## 9. Phase A — detailed scope

### 9.1 In scope

- IR + store + snapshots + patches
- Cargo + rust module extractors + subsystem clusterer
- Generic thin extractor
- Static intents + optional LLM intents
- Panel + canvas (`vibe` skin; `c4_container` if cheap)
- Jump to source
- Layout pins + persistence
- Settings + optional `semantic_map.toml`
- Unit tests for IR/extractors; GPUI tests for panel selection basics
- Feature flag / settings gate

### 9.2 Explicitly out of scope for A

- Diagram editing that mutates code
- Full UML class diagrams for whole repo
- Call graph visualization by default
- Collab-shared pins
- Perfect clustering
- Non-Rust deep extractors (thin only)

### 9.3 Acceptance scenarios

1. **Zed dogfood:** Open Zed checkout → see subsystems roughly corresponding to UI / editor / project / agent / collab → each has a readable summary → click `editor` opens `crates/editor`.
2. **LLM off:** Same as above with coherent static summaries.
3. **Edit Cargo.toml member:** Graph updates without restart.
4. **Non-Rust folder:** Thin map shows top folders + README intents.
5. **Remote SSH project (stretch A.5):** Map still opens; may index slower; no crash if enricher missing.

---

## 10. Phase C — Agent co-view (detailed)

### 10.1 Goals

Make agent work **structurally visible**.

### 10.2 Event wiring

Subscribe to:

- `BufferEvent::Edited` with `BufferEditSource::Agent`
- `action_log` buffer edit/read records
- `Project::set_agent_location` / agent selections

Map buffer paths → `NodeId` via `SemanticGraphStore::nodes_for_path`.

### 10.3 UI overlays

- **Heat:** nodes recently touched by agent fade over ~30–60s
- **Pulse:** currently active edit target
- **Trail:** optional edges between consecutively touched modules in one turn
- **Side card** in Agent Panel: “Affected subsystems” list with jump-to-node

### 10.4 Agent tooling

Add read tool (or extend existing) roughly:

```text
semantic_map_overview(lens?) -> { subsystems: [{name, summary, modules[]}] }
semantic_map_node(node_id|path) -> { node, intent, neighbors }
```

Encourage agents to call overview before large refactors.

### 10.5 Non-goals for C

- Letting the agent freely rewrite subsystem pins without user confirmation
- Replacing diff review

---

## 11. Phase B — Diagram-driven design (detailed)

Phase B is where “program by looking at diagrams” starts. Split deliberately.

### 11.1 B1 — Safe structural edits

Operations:

| Map action | Code effect |
|------------|-------------|
| Rename module/type | LSP rename / file rename flows already in Zed |
| Create module/crate | Scaffold files + manifest edits via transactional project ops |
| Move file between modules | Use existing refactor paths where available; else explicit unsupported |
| Mark expected dependency | Create tracking task / stub `use` only with confirmation |

All B1 ops:

- Preview diff before apply
- Go through permissions if agent-assisted
- Tag edits with a clear source (`BufferEditSource` extension or analytics-only label)

### 11.2 B2 — Design nodes

Introduce IR kinds:

```text
NodeKind::Design
EdgeKind::DesignLinks   // Design ↔ Module/Type (implements / planned)
EdgeKind::DesignContains
```

Design nodes live in `.zed/semantic_design.json` (or similar) so they are reviewable in git.

Workflow:

1. User draws Design subsystem + intended modules on map.
2. User links Design → existing Module or leaves “unimplemented”.
3. “Implement selection” hands subgraph to Agent with IR JSON + intents.
4. As code appears, link status flips to implemented; drift detection warns when code diverges from design summary.

### 11.3 Standards story

| Standard | Role |
|----------|------|
| LSP | Enrichment + rename/navigation |
| UML | Optional renderer for types/packages |
| C4 | Optional renderer for containers/context |
| Custom Vibe skin | Default cognitive UI |

No standard owns the IR.

### 11.4 Honest product boundary

Zed will not make code disappear. The north star is:

> **Spend most orientation and planning time on the map; drop to code for precision, conflicts, and proofs.**

---

## 12. Remote protocol (Phase A.5+)

Add proto messages (names indicative):

```text
GetSemanticGraphSnapshot { project_id, lens }
UpdateSemanticGraph { project_id, revision, patch }  // server push
ReindexSemanticGraph { project_id, mode }
GetSemanticIntents { project_id, node_ids }
```

Host runs extractors (`HeadlessProject` holds `SemanticGraphStore`).  
Client UI applies snapshots like other remote stores.

Until proto exists, remote mode can run **client-side thin extraction** on mirrored worktree entries only (degraded).

---

## 13. Performance plan

| Topic | Strategy |
|-------|----------|
| Large workspaces | Cap auto nodes; subsystem-first; lazy module depth |
| UI jank | Background extract; immutable snapshots; canvas virtualization |
| LSP cost | Enrich async; cache symbol responses; never on critical path |
| LLM cost | Visible-only, debounce, strong caching, batch by subsystem |
| Memory | Store arcs; purge Type/Calls subgraphs outside lens |

Bench targets (aspirational for A):

- Initial Cargo graph for Zed-sized workspace < 3s warm FS cache on reference hardware
- Panel filter keystroke < 16ms for 1k nodes
- Canvas pan at 60fps with ≤300 visible nodes

---

## 14. Testing strategy

### 14.1 `semantic_graph` unit/integration

- Fixture workspaces under `crates/semantic_graph/test_data/…`
- Cargo extractor golden graphs (stabilize names, ignore volatile ids via key asserts)
- Patch apply / revision monotonicity
- Cluster pins override
- Intent static composition snapshots
- Invalidation coalescing under burst file events

### 14.2 `semantic_map_ui` GPUI tests

- Panel renders modules from a stub store
- Selection sync between panel and canvas state entity
- OpenSource dispatches correct path (mock workspace)
- Use GPUI executor timers per Zed test guidelines

### 14.3 Manual dogfood checklist

- Open Zed on itself
- Open a small TS repo (thin extractor)
- Toggle LLM intents
- Remote SSH smoke (when enabled)

---

## 15. Security, trust, privacy

- Respect worktree trust: untrusted projects → no LLM intents, no auto external network for extractors.
- Capability-gate any extension extractors (process exec, download).
- Do not send whole file bodies to LLM by default; evidence excerpts only.
- Design files and pins are user data — include in privacy docs when shipping.

---

## 16. Suggested file layout (new crates)

```text
crates/semantic_graph/
  Cargo.toml
  src/
    semantic_graph.rs          # lib root
    ir.rs                      # Node/Edge/Graph/Patch
    ids.rs
    store.rs                   # SemanticGraphStore entity
    invalidation.rs
    lens.rs
    intent/
      mod.rs
      static_provider.rs
      llm_provider.rs
      cache.rs
    extract/
      mod.rs
      traits.rs
      cargo.rs
      rust_modules.rs
      generic.rs
      lsp_enrich.rs
      cluster.rs
    layout/
      mod.rs
      hierarchy.rs
      pins.rs
    persist.rs

crates/semantic_map_ui/
  Cargo.toml
  src/
    semantic_map_ui.rs
    panel.rs
    canvas/
      mod.rs
      item.rs
      element.rs
      interaction.rs
      skins/
        vibe.rs
        c4.rs
        uml.rs
    view_model.rs
    actions.rs
    settings.rs
```

Follow Zed rule: **no `mod.rs` preference for new modules** — use `src/semantic_graph.rs` as lib path in `Cargo.toml`.

---

## 17. Implementation milestones (engineering)

### M0 — Skeleton

- Crates exist, wired into workspace `Cargo.toml`
- `SemanticGraphStore` empty snapshot on `Project::local`
- Feature setting default false

### M1 — Rust graph without UI

- Cargo + module extractors
- CLI/debug command or log dump of graph
- Tests on fixtures

### M2 — Panel MVP

- Tree + static intents + open source
- Setting enabled for dogfooders

### M3 — Canvas MVP

- Vibe skin, auto layout, select/focus, pins
- Shared selection with panel

### M4 — Clustering + repo pins + polish

- Subsystem clusterer + `semantic_map.toml`
- Empty/loading/partial states
- Performance passes

### M5 — Optional LLM intents

- Provider integration + cache + evidence UI

### M6 — Phase C overlays + agent tool

### M7 — Remote snapshot protocol

### M8 — Phase B1 structural ops

### M9 — Phase B2 design nodes

Milestones M0–M5 = Phase A shippable dogfood. M6 = Phase C. M8–M9 = Phase B.

---

## 18. Open questions (resolved vs deferred)

### Resolved in brainstorming

- Phase order: A → C → B
- Primary user question: structure + purpose
- Intent: hybrid static + optional LLM
- Languages: Rust deep first, multi-lang IR
- UI: panel + canvas
- Delivery: core in-tree/fork, adapters extensible
- Architecture style: SemanticGraph platform (not Outline++ or LSP-only)

### Deferred decisions (do not block M0–M3)

1. Exact clustering algorithm v2 (embeddings? import NLP?)
2. Whether intents live in SQLite vs file cache long-term
3. Upstream merge strategy vs long-lived fork branding
4. Which UML subset is worth a skin beyond packages/classes
5. Whether Design nodes use CRDT collaboration in collab sessions
6. Formal schema versioning for IR export (`semantic_graph.json` for external tools)

---

## 19. Documentation plan (product docs, later)

When implementing, add user docs under `docs/src/` (not part of this spec’s approval):

- `docs/src/semantic-map.md` — user guide
- Settings reference entries
- Short “how intent works” privacy note

Dev glossary additions: `SemanticGraph`, `SemanticMapPanel`, `DiagramRenderer`.

---

## 20. Summary

Semantic Map adds a **Project-owned semantic IR** and two UI surfaces so users can orient by structure and purpose. LSP and UML are adapters/renderers, not the core. Phase A makes repositories legible; Phase C mirrors agent impact onto that map; Phase B turns the map into a constrained design surface that writes back through Zed’s existing buffer/LSP/project machinery.

This is intentionally a **platform wedge** inside Zed, not a one-off visualization panel.

---

## Appendix A — Example IR sketch (Zed-like)

```text
Project: zed
├── Subsystem: gpui-ui
│   ├── Module: gpui
│   ├── Module: ui
│   └── Module: theme
├── Subsystem: editing
│   ├── Module: editor
│   ├── Module: multi_buffer
│   ├── Module: language
│   └── Module: text
├── Subsystem: project-services
│   ├── Module: project
│   ├── Module: lsp
│   └── Module: worktree
└── Subsystem: agent
    ├── Module: agent
    └── Module: agent_ui

DependsOn: editor → multi_buffer → language → text
DependsOn: editor → project
DependsOn: agent_ui → agent → project
Intent(editor): "Text editing surface: selections, display map, LSP UX."
Intent(project): "Domain hub for worktrees, buffers, LSP, git, tasks."
```

## Appendix B — Glossary

| Term | Meaning |
|------|---------|
| IR | SemanticGraph intermediate representation |
| Intent | Short description of what a node does |
| Lens | Filter/projection over the graph for a view |
| Skin / Renderer | Visual projection (Vibe/C4/UML) |
| Pin | User-fixed canvas position or subsystem membership |
| Thin extractor | Generic low-depth structure extractor |
| Deep extractor | Language-aware high-quality extractor (Rust first) |

## Appendix C — Decision log

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Truth source | SemanticGraph IR | Avoid coupling to UML/LSP quirks |
| Intent | Static + optional LLM | Offline-first, purpose-aware |
| Default visualization | Subsystem/Module map | Answers “what is this project” |
| UI | Panel + Canvas | Hierarchy + spatial cognition |
| Phase order | A → C → B | Value before diagram authoring |
| Extensibility | Traits + later WASM | Core must be native GPUI |
