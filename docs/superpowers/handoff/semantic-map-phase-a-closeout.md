# Semantic Map Phase A — closeout (PR #1 usable)

**Branch:** `cursor/semantic-map-design-281a`  
**PR:** https://github.com/RickyQzh/zed/pull/1  
**Canonical handoff for Phase C then B:** `docs/superpowers/handoff/semantic-map-status.md`

This note is a snapshot of “is Phase A usable?” for the next agent. If it
disagrees with the handoff or the code, **trust the code**.

## Usable means

With `semantic_map.enabled: true` on this Zed checkout:

1. **View → Semantic Map** opens the panel (even if the setting was still off:
   the panel shows an enable hint instead of a silent no-op).
2. The panel lists **product-area subsystems** (`gpui-ui`, `editing`,
   `project-services`, `agent`, …) plus crate modules, not a `subsystem-N`
   mega-blob. Leftovers go to `other`. Pins live in repo-root `semantic_map.toml`.
3. Short intents exist on the Project (README), pinned subsystems (`summary`),
   and crates that have a Cargo `description`.
4. Double-click / `OpenSelectedSource` jumps to a file. Project/Subsystem fall
   back to a Contains-child location.
5. **Open Canvas** shows cards + `DependsOn` edges, including
   `{ workspace = true }` deps (e.g. `editor` → `gpui`). Pan, wheel-zoom
   (0.4–2.5), drag-to-pin. Default lens hides Entry/Type/External rows.
6. Reindex keeps the last-good graph while `Indexing`. Auto-reindex is
   **structural files only** (`Cargo.toml` / `Cargo.lock` / `semantic_map.toml` /
   crate-root README), 500ms debounce in the **panel** (not Project).

Default remains **off**. LLM intent is a no-op stub. Leave `intent.llm` false.

## Tests that encode this

```sh
cargo test -p semantic_graph --lib
cargo test -p semantic_map_ui --lib
cargo check -p zed
```

Highest-signal: `dogfood_zed_workspace_indexes_well_known_crates`,
`user_can_understand_simple_workspace_from_map`,
`cargo_extractor_resolves_workspace_true_path_deps`,
`orientation_lens_hides_entry_rows`,
`reindex_keeps_last_snapshot_until_build_applies`,
`should_reindex_path_classifies_structural_changes`.

Headless environment cannot run the full Zed GUI. Canvas pan/zoom/drag and
View-menu discoverability are unit/code-review, not a GUI loop.

## Known leftovers (not Phase A blockers)

- `cluster.min_subsystems` and `intent.llm_on_visible_only` are stored, unused.
- `apply_patch` does not cascade-remove edges; live path is full rebuild.
- Canvas pin KVP stores `NodeId` hashes; reload after first graph `Updated` if
  in-memory pins are empty. Still fragile if the user pins before Ready.
- Remote/SSH extract is client-local FS, first visible worktree only.
- No `mod` parse, no Cargo `exclude`, no UML/C4 skins.

## Next agent

**Phase C0**, not B. Heat overlay from `project::Event::BufferEdited` with
`BufferEditSource::Agent`. Details in the handoff §5.

Do not rewrite the design spec. Do not enable `semantic_map.enabled` by default.
Do not depend `semantic_graph` on `project`.
