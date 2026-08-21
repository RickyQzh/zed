# Phase A closeout: usability tests

## Status

Complete. Usability-oriented tests encode “a user can understand a project from the map.” No extractor crash or bug was found during dogfood; no production extractors were rewritten.

## Commits

- `b266b9ec8a` — `semantic_graph: add usability and dogfood tests`
- `d42c665461` — `semantic_map_ui: assert module rows, not subsystem slugs`

## Tests added

`crates/semantic_graph/src/usability.rs` (via `build_initial_graph` / `GraphIndexer`):

1. `user_can_understand_simple_workspace_from_map` — indexes `test_data/simple_workspace` and asserts:
   - modules `app` and `core_lib` exist
   - `DependsOn(app → core_lib)`
   - static intent for `core_lib` mentions domain/logic (`Core domain logic for the demo app`)
   - pin summary “Application binary” appears as a subsystem intent
2. `cargo_extractor_lists_zed_workspace_members` — cargo-only parse of this repo’s workspace members
3. `dogfood_zed_workspace_indexes_well_known_crates` — full `build_initial_graph` on the Zed repo (`max_auto_nodes` 500, `module_depth` 2)

`crates/semantic_map_ui/src/view_model.rs`:

4. `panel_and_canvas_show_simple_workspace_modules` — `GraphIndexer` snapshot (no `Project` entity); `PanelViewModel` / `CanvasViewModel` emit Module rows/cards for `app` and `core_lib`, plus pin-summary subsystem intent

Helper: `GraphIndexer::reindex_with_options` so tests can build a `SemanticGraphSnapshot` without a full `Project`.

## Verification

```
cargo test -p semantic_graph --lib
  30 passed; 0 failed; 0 ignored; finished in 1.95s

cargo test -p semantic_map_ui --lib
  18 passed; 0 failed; 0 ignored; finished in 0.15s
```

New tests: 3 in `semantic_graph`, 1 in `semantic_map_ui`.

## Dogfood outcome

- `build_initial_graph(/workspace, max_auto_nodes=500, module_depth=2)` returned `Ok`
- Graph non-empty; well-known crates present as modules: `gpui`, `editor`, `project`
- Isolated runtime **1.94s** (under 5s) — **not** `#[ignore]`
- Fast cargo-member test also kept as a cheap smoke path
- No extractor crash; no production fix required

The UI test originally matched the first `core_lib` panel row, which clustering names as a single-member **subsystem**. The test now requires `NodeKind::Module` so it asserts the crate card/intent, not the slug.
