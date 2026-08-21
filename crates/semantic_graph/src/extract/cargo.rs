use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use gpui::SharedString;
use util::rel_path::RelPath;
use worktree::WorktreeId;

use crate::extract::traits::{FsExtractCtx, SemanticExtractor};
use crate::{
    Edge, EdgeKind, EntryKind, GraphPatch, ModuleKind, ModulePayload, ModuleRef, Node, NodeFlags,
    NodeId, NodeKey, NodeKeyRef, SourceLocation, SymbolKey, SymbolKind,
};

/// Cargo workspace / package graph extractor (pure filesystem).
pub struct CargoWorkspaceExtractor;

impl SemanticExtractor for CargoWorkspaceExtractor {
    fn id(&self) -> &'static str {
        "cargo_workspace"
    }

    fn priority(&self) -> i32 {
        100
    }

    fn extract_sync(&self, ctx: &FsExtractCtx) -> Result<GraphPatch> {
        extract_cargo_workspace(&ctx.root, ctx.worktree_id)
    }
}

/// Sync helper for tests and early indexing. Async wrapper comes later.
pub fn extract_cargo_workspace(root: &Path, worktree_id: WorktreeId) -> Result<GraphPatch> {
    let root_manifest = root.join("Cargo.toml");
    let root_toml = read_toml(&root_manifest)?;

    let member_dirs = workspace_member_dirs(root, &root_toml)?;
    let workspace_dep_paths = workspace_dependency_paths(&root_toml);
    let packages = load_packages(root, &member_dirs, &workspace_dep_paths)?;

    let project_key = NodeKey::Project { worktree_id };
    let project_id = NodeId::from_key(&project_key);
    let project_name = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project");
    let cargo_toml =
        RelPath::from_unix_str("Cargo.toml").context("Cargo.toml is not a valid relative path")?;
    let project_node =
        Node::project(project_id, project_key, project_name).with_location(Some(SourceLocation {
            worktree_id,
            path: Arc::from(cargo_toml),
            range: None,
            symbol: None,
        }));

    let mut upsert_nodes = vec![project_node];
    let mut upsert_edges = Vec::new();
    let mut package_ids: BTreeMap<String, NodeId> = BTreeMap::new();
    let mut package_by_manifest_dir: BTreeMap<PathBuf, String> = BTreeMap::new();

    for package in &packages {
        let key = std::fs::canonicalize(&package.manifest_dir)
            .unwrap_or_else(|_| normalize_path(&package.manifest_dir));
        package_by_manifest_dir.insert(key, package.name.clone());
    }

    for package in &packages {
        let manifest_rel = rel_path_from_root(root, &package.manifest_dir)?;
        let module_key = NodeKey::Module {
            worktree_id,
            module_ref: ModuleRef::CargoPackage {
                package_name: SharedString::from(package.name.as_str()),
                manifest_dir: Arc::clone(&manifest_rel),
            },
        };
        let module_id = NodeId::from_key(&module_key);
        package_ids.insert(package.name.clone(), module_id);

        let location_path = if let Some(source) = &package.primary_source {
            rel_path_from_root(root, source)?
        } else {
            let cargo_toml = RelPath::from_unix_str("Cargo.toml")
                .context("Cargo.toml is not a valid relative path")?;
            Arc::from(manifest_rel.join(cargo_toml))
        };

        let module_kind = if package.has_lib {
            ModuleKind::CrateLib
        } else if package.has_bin {
            ModuleKind::CrateBin
        } else {
            ModuleKind::Package
        };

        let module_node = Node::module(
            module_id,
            module_key.clone(),
            package.name.as_str(),
            Some(SourceLocation {
                worktree_id,
                path: location_path,
                range: None,
                symbol: None,
            }),
            ModulePayload {
                language: Some(SharedString::from("rust")),
                module_kind,
                public_exports: Vec::new(),
                deps_out_count: package.path_deps.len() as u32,
                deps_in_count: 0,
                loc_estimate: None,
            },
            NodeFlags {
                is_entry_point: package.has_bin && !package.has_lib,
                is_test: package_is_test(&package.name, &package.manifest_dir),
                ..NodeFlags::default()
            },
        );
        upsert_nodes.push(module_node);
        upsert_edges.push(Edge::contains(project_id, module_id));

        let module_key_ref = NodeKeyRef::from(module_key);

        if package.has_lib {
            if let Some(lib_path) = package.lib_path.as_ref() {
                let entry = entry_node(
                    worktree_id,
                    &module_key_ref,
                    root,
                    lib_path,
                    "lib",
                    EntryKind::LibRoot,
                    false,
                )?;
                let entry_id = entry.id;
                upsert_nodes.push(entry);
                upsert_edges.push(Edge::contains(module_id, entry_id));
            }
        }

        for bin in &package.bin_targets {
            let entry = entry_node(
                worktree_id,
                &module_key_ref,
                root,
                &bin.path,
                &bin.name,
                if bin.name == "main" {
                    EntryKind::Main
                } else {
                    EntryKind::BinTarget
                },
                true,
            )?;
            let entry_id = entry.id;
            upsert_nodes.push(entry);
            upsert_edges.push(Edge::contains(module_id, entry_id));
        }
    }

    for package in &packages {
        let Some(&from_id) = package_ids.get(&package.name) else {
            continue;
        };
        for dep in &package.path_deps {
            let base = if dep.relative_to_workspace {
                root
            } else {
                package.manifest_dir.as_path()
            };
            let dep_manifest = canonicalize_join(base, &dep.path)?;
            let target_name = package_by_manifest_dir
                .get(&dep_manifest)
                .cloned()
                .or_else(|| {
                    // Fall back to dependency key when the path resolves to a known package name.
                    package_ids
                        .contains_key(&dep.name)
                        .then(|| dep.name.clone())
                });
            let Some(target_name) = target_name else {
                continue;
            };
            let Some(&to_id) = package_ids.get(&target_name) else {
                continue;
            };
            if from_id != to_id {
                upsert_edges.push(Edge::depends_on(from_id, to_id));
            }
        }
    }

    // Fill inbound dep counts now that DependsOn edges are known.
    let mut deps_in: BTreeMap<NodeId, u32> = BTreeMap::new();
    for edge in &upsert_edges {
        if edge.kind == EdgeKind::DependsOn {
            *deps_in.entry(edge.to).or_default() += 1;
        }
    }
    for node in &mut upsert_nodes {
        if let crate::NodePayload::Module(payload) = &mut node.payload {
            payload.deps_in_count = deps_in.get(&node.id).copied().unwrap_or(0);
        }
    }

    Ok(GraphPatch {
        base: crate::GraphRevision(0),
        removed_nodes: Vec::new(),
        removed_edges: Vec::new(),
        upsert_nodes,
        upsert_edges,
    })
}

#[derive(Debug)]
struct PackageInfo {
    name: String,
    manifest_dir: PathBuf,
    has_lib: bool,
    has_bin: bool,
    lib_path: Option<PathBuf>,
    primary_source: Option<PathBuf>,
    bin_targets: Vec<BinTarget>,
    path_deps: Vec<PathDep>,
}

#[derive(Debug)]
struct BinTarget {
    name: String,
    path: PathBuf,
}

#[derive(Debug)]
struct PathDep {
    name: String,
    path: PathBuf,
    relative_to_workspace: bool,
}

fn package_is_test(name: &str, manifest_dir: &Path) -> bool {
    if name == "tests" || name.ends_with("_test") || name.ends_with("_tests") {
        return true;
    }
    manifest_dir
        .file_name()
        .and_then(|component| component.to_str())
        == Some("tests")
}

fn entry_node(
    worktree_id: WorktreeId,
    module_key: &NodeKeyRef,
    root: &Path,
    absolute_path: &Path,
    display_name: &str,
    entry_kind: EntryKind,
    is_entry_point: bool,
) -> Result<Node> {
    let path = rel_path_from_root(root, absolute_path)?;
    let symbol_key = SymbolKey {
        qualified_name: SharedString::from(display_name),
        kind: SymbolKind::Module,
    };
    let key = NodeKey::Entry {
        module: module_key.clone(),
        symbol_key: symbol_key.clone(),
    };
    let id = NodeId::from_key(&key);
    Ok(Node::entry(
        id,
        key,
        display_name,
        Some(SourceLocation {
            worktree_id,
            path,
            range: None,
            symbol: Some(symbol_key),
        }),
        entry_kind,
        NodeFlags {
            is_entry_point,
            ..NodeFlags::default()
        },
    ))
}

fn read_toml(path: &Path) -> Result<toml::Value> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))
}

fn workspace_member_dirs(root: &Path, root_toml: &toml::Value) -> Result<Vec<PathBuf>> {
    if let Some(members) = root_toml
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(|members| members.as_array())
    {
        let mut dirs = Vec::new();
        let mut seen = BTreeSet::new();
        for member in members {
            let Some(pattern) = member.as_str() else {
                continue;
            };
            for dir in expand_member_pattern(root, pattern)? {
                let key = canonicalize_or_normalize(&dir);
                if seen.insert(key) {
                    dirs.push(dir);
                }
            }
        }
        return Ok(dirs);
    }

    // Single-package crate (no [workspace] table).
    if root_toml.get("package").is_some() {
        return Ok(vec![root.to_path_buf()]);
    }

    bail!(
        "no workspace members or package found in {}",
        root.join("Cargo.toml").display()
    )
}

fn expand_member_pattern(root: &Path, pattern: &str) -> Result<Vec<PathBuf>> {
    if !is_glob_pattern(pattern) {
        let dir = root.join(pattern);
        if dir.join("Cargo.toml").is_file() {
            return Ok(vec![dir]);
        }
        return Ok(Vec::new());
    }

    let glob = globset::Glob::new(pattern)
        .with_context(|| format!("invalid workspace member glob `{pattern}`"))?;
    let matcher = glob.compile_matcher();
    let mut matches = Vec::new();
    collect_glob_member_dirs(root, root, &matcher, 0, &mut matches)?;
    matches.sort();
    Ok(matches)
}

fn is_glob_pattern(pattern: &str) -> bool {
    pattern.contains('*') || pattern.contains('?') || pattern.contains('[')
}

fn collect_glob_member_dirs(
    workspace_root: &Path,
    dir: &Path,
    matcher: &globset::GlobMatcher,
    depth: u32,
    out: &mut Vec<PathBuf>,
) -> Result<()> {
    // Bound walk depth; Cargo member globs are typically shallow (`crates/*`).
    if depth > 8 {
        return Ok(());
    }
    let entries =
        std::fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))?;
    for entry in entries {
        let entry =
            entry.with_context(|| format!("failed to read entry under {}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to stat {}", path.display()))?;
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == ".git" || name == "target" || name == "node_modules" {
            continue;
        }
        let Ok(relative) = path.strip_prefix(workspace_root) else {
            continue;
        };
        let relative_unix = relative.to_string_lossy().replace('\\', "/");
        if matcher.is_match(&relative_unix) && path.join("Cargo.toml").is_file() {
            out.push(path.clone());
            // Matched package roots are leaves for member expansion.
            continue;
        }
        collect_glob_member_dirs(workspace_root, &path, matcher, depth + 1, out)?;
    }
    Ok(())
}

fn canonicalize_or_normalize(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| normalize_path(path))
}

fn load_packages(
    root: &Path,
    member_dirs: &[PathBuf],
    workspace_dep_paths: &BTreeMap<String, PathBuf>,
) -> Result<Vec<PackageInfo>> {
    let mut packages = Vec::new();
    for dir in member_dirs {
        packages.push(load_package(root, dir, workspace_dep_paths)?);
    }
    Ok(packages)
}

fn load_package(
    _root: &Path,
    manifest_dir: &Path,
    workspace_dep_paths: &BTreeMap<String, PathBuf>,
) -> Result<PackageInfo> {
    let manifest_path = manifest_dir.join("Cargo.toml");
    let value = read_toml(&manifest_path)?;
    let package_table = value
        .get("package")
        .context("missing [package] in member Cargo.toml")?;
    let name = package_table
        .get("name")
        .and_then(|name| name.as_str())
        .context("missing package.name")?
        .to_string();

    let lib_path = resolve_lib_path(manifest_dir, &value);
    let bin_targets = resolve_bin_targets(manifest_dir, &value, &name);
    let has_lib = lib_path.is_some();
    let has_bin = !bin_targets.is_empty();
    let primary_source = lib_path
        .clone()
        .or_else(|| bin_targets.first().map(|bin| bin.path.clone()));

    let path_deps = path_dependencies(&value, workspace_dep_paths);

    Ok(PackageInfo {
        name,
        manifest_dir: manifest_dir.to_path_buf(),
        has_lib,
        has_bin,
        lib_path,
        primary_source,
        bin_targets,
        path_deps,
    })
}

fn resolve_lib_path(manifest_dir: &Path, value: &toml::Value) -> Option<PathBuf> {
    if let Some(path) = value
        .get("lib")
        .and_then(|lib| lib.get("path"))
        .and_then(|path| path.as_str())
    {
        return Some(manifest_dir.join(path));
    }
    let default = manifest_dir.join("src/lib.rs");
    default.is_file().then_some(default)
}

fn resolve_bin_targets(
    manifest_dir: &Path,
    value: &toml::Value,
    package_name: &str,
) -> Vec<BinTarget> {
    let mut bins = Vec::new();

    if let Some(bin_array) = value.get("bin").and_then(|bin| bin.as_array()) {
        for bin in bin_array {
            let name = bin
                .get("name")
                .and_then(|name| name.as_str())
                .unwrap_or(package_name)
                .to_string();
            let path = bin
                .get("path")
                .and_then(|path| path.as_str())
                .map(|path| manifest_dir.join(path))
                .unwrap_or_else(|| manifest_dir.join("src/main.rs"));
            if path.is_file() {
                bins.push(BinTarget { name, path });
            }
        }
    }

    if bins.is_empty() {
        let main = manifest_dir.join("src/main.rs");
        if main.is_file() {
            bins.push(BinTarget {
                name: "main".to_string(),
                path: main,
            });
        }
        let bin_dir = manifest_dir.join("src/bin");
        if bin_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&bin_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                        let name = path
                            .file_stem()
                            .and_then(|stem| stem.to_str())
                            .unwrap_or("bin")
                            .to_string();
                        bins.push(BinTarget { name, path });
                    }
                }
            }
        }
    }

    bins
}

fn workspace_dependency_paths(root_toml: &toml::Value) -> BTreeMap<String, PathBuf> {
    let mut paths = BTreeMap::new();
    let Some(table) = root_toml
        .get("workspace")
        .and_then(|workspace| workspace.get("dependencies"))
        .and_then(|dependencies| dependencies.as_table())
    else {
        return paths;
    };
    for (name, spec) in table {
        let Some(path) = spec
            .as_table()
            .and_then(|table| table.get("path"))
            .and_then(|path| path.as_str())
        else {
            continue;
        };
        paths.insert(name.clone(), PathBuf::from(path));
    }
    paths
}

fn spec_is_workspace_dep(spec: &toml::Value) -> bool {
    spec.as_table()
        .and_then(|table| table.get("workspace"))
        .and_then(|value| value.as_bool())
        == Some(true)
}

fn path_dependencies(
    value: &toml::Value,
    workspace_paths: &BTreeMap<String, PathBuf>,
) -> Vec<PathDep> {
    let mut deps = Vec::new();
    let Some(table) = value.get("dependencies").and_then(|deps| deps.as_table()) else {
        return deps;
    };
    for (name, spec) in table {
        if let Some(path) = spec
            .as_table()
            .and_then(|table| table.get("path"))
            .and_then(|path| path.as_str())
        {
            deps.push(PathDep {
                name: name.clone(),
                path: PathBuf::from(path),
                relative_to_workspace: false,
            });
            continue;
        }
        if spec_is_workspace_dep(spec)
            && let Some(path) = workspace_paths.get(name)
        {
            deps.push(PathDep {
                name: name.clone(),
                path: path.clone(),
                relative_to_workspace: true,
            });
        }
    }
    deps
}

fn canonicalize_join(base: &Path, relative: &Path) -> Result<PathBuf> {
    let joined = base.join(relative);
    match joined.canonicalize() {
        Ok(path) => Ok(path),
        Err(_) => Ok(normalize_path(&joined)),
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn rel_path_from_root(root: &Path, absolute: &Path) -> Result<Arc<RelPath>> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let absolute = absolute
        .canonicalize()
        .unwrap_or_else(|_| normalize_path(absolute));
    let relative = absolute.strip_prefix(&root).with_context(|| {
        format!(
            "path {} is not under workspace root {}",
            absolute.display(),
            root.display()
        )
    })?;
    let unix = relative
        .to_str()
        .with_context(|| format!("non-utf8 relative path {}", relative.display()))?
        .replace('\\', "/");
    let rel =
        RelPath::from_unix_str(&unix).with_context(|| format!("invalid relative path {unix}"))?;
    Ok(rel.into())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use worktree::WorktreeId;

    use super::{extract_cargo_workspace, package_is_test};
    use crate::{EdgeKind, NodeKind, SemanticGraph};

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
            graph.edges.values().any(|e| e.kind == EdgeKind::DependsOn),
            "expected DependsOn edge from app to core_lib"
        );
    }

    #[test]
    fn cargo_extractor_expands_glob_members() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::write(
            root.join("Cargo.toml"),
            r#"
[workspace]
members = ["crates/*"]
"#,
        )
        .unwrap();
        for name in ["alpha", "beta"] {
            let crate_dir = root.join("crates").join(name);
            std::fs::create_dir_all(crate_dir.join("src")).unwrap();
            std::fs::write(
                crate_dir.join("Cargo.toml"),
                format!(
                    r#"
[package]
name = "{name}"
version = "0.1.0"
edition = "2021"
"#
                ),
            )
            .unwrap();
            std::fs::write(crate_dir.join("src/lib.rs"), "// lib\n").unwrap();
        }
        // Non-matching sibling should be ignored.
        let other = root.join("apps").join("gamma");
        std::fs::create_dir_all(other.join("src")).unwrap();
        std::fs::write(
            other.join("Cargo.toml"),
            r#"
[package]
name = "gamma"
version = "0.1.0"
edition = "2021"
"#,
        )
        .unwrap();
        std::fs::write(other.join("src/lib.rs"), "// lib\n").unwrap();

        let patch = extract_cargo_workspace(root, WorktreeId::from_usize(1)).unwrap();
        let mut graph = SemanticGraph::default();
        graph.apply_patch(patch).unwrap();
        let names: BTreeSet<_> = graph
            .nodes
            .values()
            .filter(|n| n.kind == NodeKind::Module)
            .map(|n| n.display_name.to_string())
            .collect();
        assert_eq!(
            names,
            BTreeSet::from(["alpha".to_string(), "beta".to_string()])
        );
    }

    #[test]
    fn cargo_project_location_is_root_cargo_toml() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_data/simple_workspace");
        let patch = extract_cargo_workspace(&root, WorktreeId::from_usize(1)).unwrap();
        let project = patch
            .upsert_nodes
            .iter()
            .find(|node| node.kind == NodeKind::Project)
            .expect("project node");
        let path = project
            .location
            .as_ref()
            .expect("project location")
            .path
            .as_unix_str();
        assert!(
            path.ends_with("Cargo.toml"),
            "expected Project.location to end with Cargo.toml, got {path}"
        );
    }

    #[test]
    fn package_is_test_matches_name_and_manifest_dir() {
        use std::path::Path;

        assert!(package_is_test("foo_tests", Path::new("crates/foo_tests")));
        assert!(package_is_test("foo_test", Path::new("crates/foo_test")));
        assert!(package_is_test("tests", Path::new("crates/integration")));
        assert!(package_is_test("helpers", Path::new("crates/tests")));
        assert!(!package_is_test(
            "editor_benchmarks",
            Path::new("crates/editor_benchmarks")
        ));
        assert!(!package_is_test("core_lib", Path::new("crates/core_lib")));
    }

    #[test]
    fn cargo_extractor_resolves_workspace_true_path_deps() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::write(
            root.join("Cargo.toml"),
            r#"
[workspace]
members = ["crates/*"]

[workspace.dependencies]
core_lib = { path = "crates/core_lib" }
"#,
        )
        .unwrap();
        for (name, extra) in [
            ("core_lib", ""),
            ("app", "\n[dependencies]\ncore_lib.workspace = true\n"),
        ] {
            let crate_dir = root.join("crates").join(name);
            std::fs::create_dir_all(crate_dir.join("src")).unwrap();
            std::fs::write(
                crate_dir.join("Cargo.toml"),
                format!(
                    r#"
[package]
name = "{name}"
version = "0.1.0"
edition = "2021"
{extra}
"#
                ),
            )
            .unwrap();
            std::fs::write(crate_dir.join("src/lib.rs"), "// lib\n").unwrap();
        }

        let patch = extract_cargo_workspace(root, WorktreeId::from_usize(1)).unwrap();
        let mut graph = SemanticGraph::default();
        graph.apply_patch(patch).unwrap();

        let id = |name: &str| {
            graph
                .nodes
                .values()
                .find(|node| node.kind == NodeKind::Module && node.display_name.as_ref() == name)
                .map(|node| node.id)
                .unwrap_or_else(|| panic!("missing module {name}"))
        };
        let app_id = id("app");
        let core_lib_id = id("core_lib");
        assert!(
            graph.edges.values().any(|edge| {
                edge.kind == EdgeKind::DependsOn && edge.from == app_id && edge.to == core_lib_id
            }),
            "workspace = true should resolve to DependsOn(app → core_lib)"
        );
    }
}
