---
title: Semantic Map - Zed
description: Orient in a project with Zed's Semantic Map panel and canvas. Enable the feature, open the map, pin layout, and keep LLM intent off by default.
---

# Semantic Map

The Semantic Map shows how your project is organized: subsystems, modules, short
"what this does" summaries, and links into source. Use it to orient in an
unfamiliar repository without reading the tree file by file.

Phase A focuses on Rust / Cargo projects. The map works offline. LLM intent
enrichment is off by default and optional.

## Getting Started {#getting-started}

Semantic Map is disabled by default. Enable it in settings, then open the panel.

1. Open the Settings Editor (`Cmd+,` on macOS, `Ctrl+,` on Linux/Windows) and
   search for `semantic_map`.
2. Set `enabled` to `true`.

Or add this to your `settings.json`:

```json [settings]
{
  "semantic_map": {
    "enabled": true
  }
}
```

## Panel {#panel}

Toggle the Semantic Map panel with {#action semantic_map::ToggleFocus} from the
command palette.

When the panel is focused, Zed indexes the project and lists nodes (subsystems
and modules) with short intent summaries. Click a row to select it. Use
{#action semantic_map::OpenSelectedSource} or double-click a row to jump to
the node's source.

Status chips in the panel header show whether the graph is indexing, ready,
partial (truncated by budget), or in error. Use **Reindex**
({#action semantic_map::Reindex}) to rebuild after large project changes.

> **Note:** If the feature is disabled, the panel prompts you to enable
> `semantic_map` in settings.

## Canvas {#canvas}

Open the spatial canvas with {#action semantic_map::OpenCanvas}, or click
**Open Canvas** in the panel.

The canvas lays out nodes as cards with dependency edges. Pan by dragging
the background. Scroll to zoom around the pointer, or around the canvas
center when the pointer is outside the viewport. Double-click a card to
open its source when a location is available. Select a card to sync
selection with the panel. You can also use
{#action semantic_map::OpenSelectedSource} after selecting a card.

To open the canvas automatically when a project opens:

```json [settings]
{
  "semantic_map": {
    "enabled": true,
    "auto_open_canvas_on_project_open": true
  }
}
```

## Pins {#pins}

Two kinds of pins shape the map:

### Canvas layout pins {#canvas-layout-pins}

Drag a card on the canvas past a short threshold to pin its position. Pinned
positions override automatic layout for those nodes and persist for the
workspace. Small moves without a real drag keep the selection and do not pin.

### Repo subsystem pins {#repo-subsystem-pins}

Add an optional `semantic_map.toml` at the project root to override how crates
group into subsystems. Committed pins stay shared with the team.

```toml
[subsystems.ui]
members = ["crates/gpui", "crates/ui", "crates/theme"]
summary = "GPU UI framework and shared widgets"

[subsystems.editor]
members = ["crates/editor", "crates/multi_buffer", "crates/language"]
```

Repo config overrides clustering heuristics on the next index. When a
subsystem pin includes a `summary`, that text becomes the subsystem's
static intent on the map.

## Phase A limitations {#phase-a-limitations}

- Cargo `workspace.members` globs such as `crates/*` are expanded; exclude
  lists and full Cargo glob parity are not guaranteed.
- The map does not rebuild when you edit files. Use
  {#action semantic_map::Reindex} after large project or
  `semantic_map.toml` changes.

## Status {#status}

Semantic Map is experimental Phase A. It is off by default. You enable it to
browse subsystems and jump to source. Agent highlighting and diagram edits are
not available.

## What's next {#whats-next}

Later work:

- Agent co-view (Phase C): highlight map nodes the agent is editing.
- Diagram-driven design (Phase B): structural edits and design nodes from the
  map.

Those features are not in this release.

## Privacy and intent {#privacy}

Intent text ("what this part does") comes from static sources first: package
descriptions, module structure, and similar local evidence. The map does not
need a network connection for that path.

LLM enrichment is **off by default** (`semantic_map.intent.llm` is `false`).
Leave it off for dogfood and offline use. When you opt in later, Zed only
enriches intents if you enable the setting; it does not upload the whole
repository by default.

```json [settings]
{
  "semantic_map": {
    "enabled": true,
    "intent": {
      "llm": false,
      "llm_on_visible_only": true
    }
  }
}
```

> **Note:** With `intent.llm` enabled today, enrichment is still a no-op stub
> until model routing lands. Static intents keep working either way.

## Settings reference {#settings-reference}

| Setting                                         | Default | Purpose                                      |
| ----------------------------------------------- | ------- | -------------------------------------------- |
| `semantic_map.enabled`                          | `false` | Master gate for panel and canvas             |
| `semantic_map.auto_open_canvas_on_project_open` | `false` | Open canvas when a project opens             |
| `semantic_map.hide_external`                    | `true`  | Hide external dependency nodes               |
| `semantic_map.hide_tests`                       | `true`  | Hide test-only modules and crates            |
| `semantic_map.module_depth`                     | `3`     | How deep to expand modules automatically     |
| `semantic_map.max_auto_nodes`                   | `500`   | Soft cap before the graph is marked partial  |
| `semantic_map.cluster.min_subsystems`           | `3`     | Soft minimum subsystem count when clustering |
| `semantic_map.cluster.max_subsystems`           | `16`    | Soft maximum subsystem count when clustering |
| `semantic_map.intent.llm`                       | `false` | Opt into LLM intent enrichment               |
| `semantic_map.intent.llm_on_visible_only`       | `true`  | Limit LLM requests to visible nodes          |

## See Also {#see-also}

- [Outline Panel](./outline-panel.md)
- [Project Panel](./project-panel.md)
- [AI Privacy](./ai/privacy-and-security.md)
