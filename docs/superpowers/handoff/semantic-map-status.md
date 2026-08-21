# Semantic Map — agent handoff (Phase A shipped, C then B next)

**Branch:** `cursor/semantic-map-design-281a`  
**Design spec:** `docs/superpowers/specs/2026-08-08-semantic-map-design.md`  
**Phase A plan:** `docs/superpowers/plans/2026-08-08-semantic-map-phase-a.md` (checkboxes stale; do not re-run)  
**Phase A closeout:** `docs/superpowers/handoff/semantic-map-phase-a-closeout.md`  
**User doc:** `docs/src/semantic-map.md`

Phase lettering is **A → C → B**. Do not start Phase B until C is underway
unless a human explicitly reorders. Do not rewrite the design spec; implement
against §10 (C) and later §11 (B).

---

## 1. What Phase A is (shipped on this PR)

Experimental, **default off**. Offline-first Rust/Cargo orientation: open a
repo, see subsystems/modules + short static “what this does”, jump to source.

### Crates

| Crate | Role |
|-------|------|
| `semantic_graph` | IR, extractors, intents, layout, `SemanticGraphStore`. **Must not** depend on `project`. |
| `semantic_map_ui` | Settings, dock panel, canvas pane item, view models, actions. |
| `project` | Owns `Entity<SemanticGraphStore>`; exposes `semantic_graph()` and `reindex_semantic_graph`. |
| `settings_content` | `SemanticMapSettingsContent` schema (`semantic_map.*`). |
| `zed` | `semantic_map_ui::init(cx)`; registers `SemanticMapPanel` in `initialize_panels`; View menu item. |

### Key types (`semantic_graph`)

- **IR** (`ir.rs`): `Node`, `NodeKind` (`Project`, `Subsystem`, `Module`, `Entry`, `Type`, `External`), `NodeKey` / `ModuleRef`, `Edge` / `EdgeKind` (`Contains`, `DependsOn`, `Exposes`, `Implements`, `Calls`, `References`, `DesignLinks`), `SemanticGraph`, `GraphPatch`, `GraphRevision`, `Lens`, `Intent`, `Evidence`, `SourceLocation`.
- **IDs** (`ids.rs`): `NodeId` / `EdgeId` from `FxHasher` over keys. Process-stable, not a cross-language digest.
- **Store** (`store.rs`): `SemanticGraphStore`, `SemanticGraphSnapshot`, `GraphStatus` (`Idle`, `Indexing`, `Partial`, `Error`), `SemanticGraphEvent::Updated`, `reindex`, `apply_patch`, `nodes_for_path` (path → node ids; Phase C hook).
- **Build** (`invalidation.rs`): `BuildGraphOptions`, `build_initial_graph` (Cargo or generic → rust modules → **cluster then trim** → static intents → LLM stub). `invalidation_scope_for_path` classifies dirty paths; live debounce lives in `semantic_map_ui` (`should_reindex_path`).
- **Extractors** (`extract/`): Cargo workspace, rust module FS walk, generic thin folders, subsystem cluster + `semantic_map.toml` pins.
- **Intent** (`intent/`): `StaticIntentProvider` (crate description / README / pin `summary`); `LlmIntentProvider` **stub** (always empty).
- **Layout** (`layout/`): hierarchy grid + `CanvasPins` (workspace KVP, not repo toml).

`semantic_graph` uses its own `ProjectPath { worktree_id, path }` so it stays
free of a `project` crate dependency.

### User flows (when `semantic_map.enabled` is true)

1. Open Semantic Map panel → first time indexes visible worktree; list of nodes + intent chips; status Ready / Indexing / Partial / Error.
2. Click row to select; `OpenSelectedSource` or double-click a panel row opens source when `SourceLocation` exists. Double-click a canvas card (below pin-drag threshold) also opens source.
3. `OpenCanvas` (or panel **Open Canvas**) opens a card canvas in the active pane; selection syncs with the panel. Pan by dragging the background. Scroll-wheel zooms (clamped ~0.4–2.5) around the pointer.
4. Drag a card past ~3px to pin layout (workspace KVP). Small moves do not pin.
5. Optional `semantic_map.toml` at repo root pins subsystem membership + optional `summary` intent.
6. **Reindex** rebuilds the full graph (no incremental apply yet). Auto-reindex (500ms debounce, while enabled) runs only for `Cargo.toml`, `Cargo.lock`, `semantic_map.toml`, crate-root `README.md`/`README`, and when a worktree is added. Editing `src/**/*.rs` does **not** auto-reindex. Use **Reindex** to force a rebuild. Live path is still a **full** rebuild, not incremental `GraphPatch` apply. While `Indexing`, the last-good snapshot stays visible; a generation counter ignores stale builds. `reindex_semantic_graph` returns `bool` so the panel does not latch `has_reindexed` when no visible worktree exists.

Disabled: panel shows “Enable semantic_map in settings…”. Actions that mutate
the map no-op when disabled.

### Settings (defaults)

All under `semantic_map`. Master gate **`enabled: false`**.

| Key | Default |
|-----|---------|
| `enabled` | `false` |
| `default_skin` | `vibe` (only skin implemented) |
| `hide_external` / `hide_tests` | `true` |
| `auto_open_canvas_on_project_open` | `false` |
| `module_depth` | `3` |
| `max_auto_nodes` | `500` (overflow → `GraphStatus::Partial`) |
| `cluster.min_subsystems` / `max_subsystems` | `3` / `16` (`min_subsystems` is **parsed but unused** by the clusterer; only `max_subsystems` is applied) |
| `intent.llm` | `false` |
| `intent.llm_on_visible_only` | `true` (**parsed but unused**; LLM provider is a no-op stub) |

Repo pins: `semantic_map.toml` `[subsystems.<slug>] members = [...]` and
optional `summary`. This Zed checkout **has a root `semantic_map.toml`**
(product areas: `gpui-ui`, `editing`, `project-services`, `agent`, `collab`,
`git`, `app`, `debugger`, plus `other` for leftovers). Copy that pattern for
other large workspaces.

---

## 2. File map

### `crates/semantic_graph`

| Path | Responsibility |
|------|----------------|
| `src/semantic_graph.rs` | Lib root (`[lib] path`); re-exports. No `mod.rs`. |
| `src/ir.rs` | Graph IR, patches, lens, intents. |
| `src/ids.rs` | `NodeId` / `EdgeId` hashing. |
| `src/store.rs` | GPUI entity, snapshot, async full reindex, `nodes_for_path`. Last-good graph stays visible while `Indexing`. |
| `src/usability.rs` | Fixture + Zed-checkout dogfood tests (`max_auto_nodes` 500, depth 2). |
| `src/invalidation.rs` | `build_initial_graph`, `BuildGraphOptions`, `GraphIndexer` test helper, path-scope enum. Live debounce is in `semantic_map_ui` (`should_reindex_path`). |
| `src/extract.rs` | Extractor module root + re-exports. |
| `src/extract/traits.rs` | `SemanticExtractor`, `FsExtractCtx`, `ExtractBudget`. |
| `src/extract/cargo.rs` | Parse workspace/package; expand `workspace.members` globs (`crates/*`); resolve `{ path }` **and** `{ workspace = true }` via `[workspace.dependencies]` paths. **No `exclude` list.** |
| `src/extract/rust_modules.rs` | Depth-limited `src/*.rs` / `src/*/` walk. **Does not parse `mod`.** |
| `src/extract/generic.rs` | Non-Cargo thin folder map + README evidence. |
| `src/extract/cluster.rs` | Prefix/affinity clusterer + `semantic_map.toml` pin parse/override. |
| `src/intent.rs` | `IntentProvider` trait. |
| `src/intent/static_provider.rs` | Offline summaries. |
| `src/intent/llm_provider.rs` | Settings-gated stub; returns `[]` even when enabled. |
| `src/layout.rs` | Layout module root. |
| `src/layout/hierarchy.rs` | Auto card positions. |
| `src/layout/pins.rs` | `CanvasPins` + `canvas_pins_kvp_key`. |
| `test_data/simple_workspace/` | Fixture workspace (`app` → `core_lib`) + sample `semantic_map.toml`. |

Not present (spec layout leftovers): `persist.rs`, `lens.rs`, `intent/cache.rs`,
`extract/lsp_enrich.rs`. Lens/intent types live in `ir.rs`.

### `crates/semantic_map_ui`

| Path | Responsibility |
|------|----------------|
| `src/semantic_map_ui.rs` | `init`: register settings + panel actions. |
| `src/settings.rs` | `SemanticMapSettings` from `settings_content`. |
| `src/panel.rs` | `actions!(semantic_map, [ToggleFocus, OpenSelectedSource, OpenCanvas, Reindex])`, `SemanticMapPanel`, first-index + Reindex, open source (node location or Contains-child fallback), **debounced auto-reindex**. Default lens is Project/Subsystem/Module (`orientation_lens`). View → Semantic Map always toggles the panel; when the feature is off the panel shows the enable hint instead of a silent no-op. |
| `src/selection.rs` | Window-local `SemanticMapSelection` shared by panel + canvas. |
| `src/view_model.rs` | `PanelViewModel` / `CanvasViewModel` / `SceneNode` / `SceneEdge` from snapshot + `Lens`. |
| `src/canvas.rs` | Canvas module root. |
| `src/canvas/item.rs` | `SemanticMapItem` pane item: pan, wheel zoom, drag-to-pin, double-click open source, selection, KVP pin load/save. |
| `src/canvas/element.rs` | Custom GPUI element for edges. |
| `src/canvas/skins.rs` | Skin module root. |
| `src/canvas/skins/vibe.rs` | Default card chrome. No `c4.rs` / `uml.rs`. |

### Integration (do not invent a second store)

- `crates/project/src/project.rs`: field `semantic_graph`; created in local/remote/ssh constructors; `reindex_semantic_graph` uses **first visible worktree** only.
- `crates/zed/src/zed.rs`: `SemanticMapPanel::load` in `initialize_panels`.
- `crates/zed/src/main.rs` (+ visual test inits): `semantic_map_ui::init`.
- `crates/zed/src/zed/app_menus.rs`: View → Semantic Map (`ToggleFocus`).

---

## 3. How to dogfood

1. Settings Editor: search `semantic_map`, set `enabled` to `true`. Or:

```json
{
  "semantic_map": {
    "enabled": true
  }
}
```

2. Command palette / View menu:
   - `semantic_map::ToggleFocus` — dock panel (indexes on first focus/open when enabled).
   - `semantic_map::OpenCanvas` — pane canvas.
   - `semantic_map::OpenSelectedSource` — jump to selected node’s file. Canvas card double-click also opens source.
   - `semantic_map::Reindex` — full rebuild. Also a **Reindex** button in the panel header. Structural file changes auto-reindex (debounced).

3. Fixture: open `crates/semantic_graph/test_data/simple_workspace`. Expect `app` / `core_lib`, DependsOn, crate-description intents.

4. Optional: Zed self-repo. Partial/truncated at `max_auto_nodes` is OK. No crash.

5. Leave `intent.llm` **false**. Enabling it still does nothing (stub).

6. After editing `Cargo.toml` / `Cargo.lock` / `semantic_map.toml` / a crate-root README, the panel debounces and auto-reindexes. Editing files under `src/` does **not**. Use **Reindex** to force a rebuild. Live path is still a **full** rebuild, not incremental `GraphPatch` apply.

---

## 4. Known gaps / not done in Phase A

Be honest. If a gap looks freshly fixed, **verify in code** before re-implementing.

| Gap | Status at handoff time |
|-----|------------------------|
| **Auto-reindex** on worktree/`UpdatedEntries` / `Cargo.toml` edits | **Wired in the panel** (`should_reindex_path` + 500ms GPUI timer debounce on `WorktreeUpdatedEntries`). Triggers: `Cargo.toml`, `Cargo.lock`, `semantic_map.toml`, crate-root README, and `WorktreeAdded`. **Not** `src/**/*.rs`. Still a full `reindex`, not incremental patches. Last-good snapshot stays visible while `Indexing`. `invalidation_scope_for_path` in `semantic_graph` is unused by the live scheduler — do not add a second scheduler in `Project` without deleting the panel one. |
| **`hide_tests`** | View/layout filter exists. Cargo extractor sets `NodeFlags.is_test` for `*_test` / `*_tests` / `tests` package names. Most workspace crates are not flagged. |
| **LLM intent** | Stub in `llm_provider.rs`. No `language_model` routing, no cache file. |
| **Remote / SSH extraction** | Store is created on remote projects, but extract runs **client-side on the local worktree root** via `reindex_semantic_graph`. No proto (`GetSemanticGraphSnapshot`, etc.). Spec §12 / A.5. |
| **Full `mod` parse** | `rust_modules.rs` is FS walk only (`src/*.rs`, `src/*/`). Inline `mod foo;` / `#[path]` not modeled. Tree-sitter follow-up. |
| **Cargo `exclude` / full glob parity** | `crates/*`-style members expand; exclude lists and Cargo glob quirks **not guaranteed**. |
| **Incremental patches** | `apply_patch` exists; live path is wipe + full rebuild. |
| **Multi-worktree** | Reindex uses first visible worktree only. |
| **UML / C4 skins** | Not implemented. `default_skin` is stored; only `vibe` paints. |
| **Canvas zoom / double-click open** | **Done.** Wheel zoom in `canvas/item.rs` (`clamp_zoom`); double-click below pin threshold calls `open_node_source`. |
| **Call graphs, Type/Calls drill-down** | Not extracted / not shown. |
| **Collab-shared pins** | Canvas pins are workspace KVP only. |
| **Agent overlays / tools** | No `agent` / `agent_ui` / `action_log` references. Phase C. |
| **Diagram edits / design nodes** | `EdgeKind::DesignLinks` exists on the enum only. Phase B. |
| **Spec leftovers** | No `persist.rs`, extension/WASM extractors, LSP enrich. |

---

## 5. Phase C plan (next agent) — do this next

**Spec:** design §10 (Agent co-view). Milestone M6.

Goal: during an agent turn, the map answers “which subsystems were touched”
without opening the diff first.

### Event wiring (reuse existing Zed types)

Prefer `project::Event::BufferEdited { source }` (`crates/project/src/project.rs`,
event ~428, emit ~4016) over per-buffer subscribe. `Project` already fans
`language::BufferEvent::Edited { source: BufferEditSource::Agent }`.

Also available (later slices, not C0):

- `action_log::ActionLog` — `changed_buffers`, `buffer_read` (`crates/action_log/src/action_log.rs`). Thread already owns `action_log`.
- `Project::set_agent_location` / `Event::AgentLocationChanged` (`project.rs`).

Map buffer paths with `semantic_graph::ProjectPath { worktree_id, path: Arc<RelPath> }`
→ `SemanticGraphStore::nodes_for_path` (`store.rs` ~189). Convert from
`project::ProjectPath`; do not pass the project crate type into `semantic_graph`.
Walk to parent `Subsystem` via `Contains` / `graph.children`.

Keep heat state in `semantic_map_ui` (new overlay entity, or extend
`selection.rs`). Both canvas and Agent Panel must observe it. Do **not** put
agent heat on `SemanticGraph` IR.

### UI

- **Heat:** nodes recently touched by agent fade **30s** (pin this duration).
  Tests: `cx.background_executor().timer`, not `smol::Timer`.
- **Pulse:** current `agent_location` target (C3).
- **Trail (optional, later):** edges between consecutively touched modules in one turn.
- **Agent Panel card (C2):** “Affected subsystems” + jump-to-node.
  Layout lives in `crates/agent_ui/src/conversation_view.rs` (ActionLog ~1254).
  Use `Thread::action_log()`. Read that file before adding a dock widget.

`SceneNode` (`view_model.rs` ~64) has `id, kind, rect, title, subtitle` only.
Add heat on the **view model**, not the IR.

### Read tools

Copy `crates/agent/src/tools/read_file_tool.rs`. Register in
`Thread::add_default_tools` next to `ReadFileTool` (`crates/agent/src/thread.rs`
~2140). The `AgentTool` trait is `thread.rs` ~5067.

```text
semantic_map_overview(lens?) -> { subsystems: [{name, summary, modules[]}] }
semantic_map_node(node_id|path) -> { node, intent, neighbors }
```

Read-only. Gate on `semantic_map.enabled`. Empty/disabled → clear error, no
panic. Encourage overview before large refactors (tool description).

### Phase C non-goals

- Agent rewriting `semantic_map.toml` pins without confirmation.
- Replacing diff review.
- Enabling LLM intents.

### Suggested first PR slice

**C0 — heat from agent buffer edits only** (one PR):

1. Overlay entity in `semantic_map_ui`: `touched: HashMap<NodeId, Instant>`
   updated on `project::Event::BufferEdited { source: BufferEditSource::Agent }`.
2. Resolve path → `semantic_graph::ProjectPath` → `nodes_for_path`.
3. Tint `SceneNode` / panel row while heat > 0; decay with
   `cx.background_executor().timer` (30s).
4. Tests in `semantic_map_ui` GPUI tests: Agent edit → node id in overlay;
   User edit → no heat.
5. Do **not** in C0: Agent Panel card, trail, LLM, tools (C1),
   `semantic_map_overview` (C1).

C1: two read tools + register in `Thread`.  
C2: Agent Panel “Affected subsystems” in `conversation_view.rs`.  
C3: pulse + optional trail.

---

## 6. Phase B plan (later agent) — do not implement now

**Spec:** design §11. Milestones M8–M9. After C0 at least.

**B0 first PR (do not start until C0 is underway unless a human reorders):**
add `NodeKind::Design` to `ir.rs`, persist stubs in `.zed/semantic_design.json`,
canvas stub cards. No codegen. `EdgeKind::DesignLinks` already exists;
**there is no `DesignContains`** — add it when implementing if needed.

### B1 — Safe structural edits

Map action → existing Zed machinery (preview diff, permissions):

| Map action | Code effect |
|------------|-------------|
| Rename module/type | LSP rename / file rename already in Zed (`rename_tool`, editor rename). |
| Create module/crate | Scaffold files + manifest via transactional project ops. |
| Move file between modules | Existing refactor paths or explicit unsupported. |
| Mark expected dependency | Tracking task / stub `use` only with confirmation. |

Tag edits so they are auditable (`BufferEditSource` or analytics label).

### B2 — Design nodes

New IR usage (enums already reserved):

- `NodeKind` needs `Design` (add when implementing; **not** on the enum today — verify `ir.rs`).
- `EdgeKind::DesignLinks` exists. Add `DesignContains` only if the B2 model needs it.

Persist in **`.zed/semantic_design.json`** (git-reviewable). Workflow: draw
Design → link to Module or leave unimplemented → “Implement selection” hands
IR JSON + intents to Agent → link flips implemented; drift warning later.

UML/C4 remain **renderers**, never the IR.

---

## 7. Tests to run

```sh
cargo test -p semantic_graph --lib
cargo test -p semantic_map_ui --lib
cargo check -p zed
```

Re-run these; do not trust stale pass counts. Last closeout was ~38 `semantic_graph` + ~19 `semantic_map_ui` before the workspace-dep / orientation-lens / location work.

Useful fixtures/tests:

- `semantic_graph`: cargo glob members, `{ workspace = true }` DependsOn, cluster pins, `nodes_for_path`, reindex Indexing→Idle / Partial / last-good snapshot, static intents, rust module walk.
- `semantic_map_ui`: settings defaults (`enabled == false`, `intent.llm == false`), `orientation_lens` hides Entry, panel GPUI tests, canvas pin-threshold / zoom / double-click classification, `should_reindex_path`, source-location fallback.
- Dogfood: `dogfood_zed_workspace_indexes_well_known_crates` — product-area pins, Contains membership (`editing`→`editor`, …), `DependsOn(editor → gpui)`, README Project intent, no `subsystem-N`.

GPUI tests: use `cx.background_executor().timer(...)`, not `smol::Timer`.

Clippy: `./script/clippy` (not raw `cargo clippy`).

---

## 8. Do not

- **Do not treat UML as IR.** Vibe/C4/UML are skins. IR is `SemanticGraph`.
- **Do not enable LLM by default.** `intent.llm` stays `false`. Stub must not
  require a model service on the offline path.
- **Do not depend `semantic_graph` on `project`.** Path types stay
  `worktree::WorktreeId` + `RelPath`. UI/project adapt.
- **Do not** implement Phase B in the Phase C PR.
- **Do not** add `mod.rs` roots; keep `[lib] path = "src/<crate>.rs"`.
- **Do not** silently drop reindex/pin I/O errors (`let _ =`); use `?` or
  `.log_err()`.
- **Do not** turn `semantic_map.enabled` default to `true` without product sign-off.
- **Do not** rewrite `docs/superpowers/specs/2026-08-08-semantic-map-design.md`
  wholesale; point here for implementation status.
