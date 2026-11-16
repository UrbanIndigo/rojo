//! Looks up instances in a project through `rojo sourcemap`, which already
//! knows which files every instance comes from.

use std::{
    cmp::Reverse,
    fmt::Write,
    path::{Path, PathBuf},
};

use serde::Deserialize;

use super::serve::rojo_command;

/// A node of a sourcemap, as written by `rojo sourcemap`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub name: String,
    pub class_name: String,
    #[serde(default)]
    pub file_paths: Vec<PathBuf>,
    #[serde(default)]
    pub children: Vec<Node>,
}

/// Instances listed under one node before the rest is summarized, so a folder
/// like `node_modules` does not flood the agent's context.
const MAX_LISTED_CHILDREN: usize = 50;

/// Builds the instance tree of a project, including non-script instances.
///
/// This runs Rojo as a child process, like the other tools do, so that a
/// project that fails to load cannot take the MCP server down with it.
pub fn load(project: &Path) -> Result<Node, String> {
    let output = rojo_command()
        .arg("sourcemap")
        .arg(project)
        .args([
            "--include-non-scripts",
            "--include-directories",
            "--absolute",
            "--color",
            "never",
        ])
        .output()
        .map_err(|err| format!("Could not run rojo sourcemap: {}", err))?;

    if !output.status.success() {
        return Err(format!(
            "Could not load the project at {}:\n{}",
            project.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    serde_json::from_slice(&output.stdout)
        .map_err(|err| format!("rojo sourcemap returned an unreadable sourcemap: {}", err))
}

/// Finds the instance at `path`, returning the chain of nodes from the root's
/// child down to it. An empty chain is the root itself.
///
/// Paths are instance names joined with `.`, optionally starting with `game.`.
/// Names can contain dots themselves, so every child whose name the path
/// starts with is tried, longest name first. Names can also be joined with `/`
/// instead, which is never ambiguous for names without slashes.
pub fn resolve<'a>(root: &'a Node, path: &str) -> Option<Vec<&'a Node>> {
    let path = path.trim();
    let path = if root.class_name == "DataModel" && !has_child(root, "game") {
        match path {
            "game" => "",
            _ => path.strip_prefix("game.").unwrap_or(path),
        }
    } else {
        path
    };

    resolve_dotted(root, path).or_else(|| {
        if !path.contains('/') {
            return None;
        }

        let mut chain = Vec::new();
        let mut node = root;
        for name in path.split('/').filter(|name| !name.is_empty()) {
            node = node.children.iter().find(|child| child.name == name)?;
            chain.push(node);
        }
        Some(chain)
    })
}

fn has_child(node: &Node, name: &str) -> bool {
    node.children.iter().any(|child| child.name == name)
}

fn resolve_dotted<'a>(node: &'a Node, path: &str) -> Option<Vec<&'a Node>> {
    if path.is_empty() {
        return Some(Vec::new());
    }

    let mut candidates: Vec<&Node> = node
        .children
        .iter()
        .filter(|child| {
            path == child.name
                || (path.starts_with(child.name.as_str())
                    && path[child.name.len()..].starts_with('.'))
        })
        .collect();
    candidates.sort_by_key(|child| Reverse(child.name.len()));

    for child in candidates {
        let rest = path.get(child.name.len() + 1..).unwrap_or("");
        if let Some(mut chain) = resolve_dotted(child, rest) {
            chain.insert(0, child);
            return Some(chain);
        }
    }

    None
}

/// The path of an instance as `resolve` accepts it.
pub fn full_name(chain: &[&Node]) -> String {
    chain
        .iter()
        .map(|node| node.name.as_str())
        .collect::<Vec<_>>()
        .join(".")
}

/// Every instance whose name contains `query` (or whose full path does, when
/// `query` contains a dot) and whose class is `class_name`, in tree order.
pub fn find<'a>(
    root: &'a Node,
    query: Option<&str>,
    class_name: Option<&str>,
    limit: usize,
) -> (Vec<Vec<&'a Node>>, usize) {
    let query = query.map(str::to_lowercase);
    let mut found = Vec::new();
    let mut total = 0;
    let mut chain = Vec::new();

    find_in(
        root,
        query.as_deref(),
        class_name,
        limit,
        &mut chain,
        &mut found,
        &mut total,
    );

    (found, total)
}

fn find_in<'a>(
    node: &'a Node,
    query: Option<&str>,
    class_name: Option<&str>,
    limit: usize,
    chain: &mut Vec<&'a Node>,
    found: &mut Vec<Vec<&'a Node>>,
    total: &mut usize,
) {
    for child in &node.children {
        chain.push(child);

        let class_matches =
            class_name.is_none_or(|class_name| child.class_name.eq_ignore_ascii_case(class_name));
        let query_matches = query.is_none_or(|query| {
            if query.contains('.') {
                full_name(chain).to_lowercase().contains(query)
            } else {
                child.name.to_lowercase().contains(query)
            }
        });

        if class_matches && query_matches {
            *total += 1;
            if found.len() < limit {
                found.push(chain.clone());
            }
        }

        find_in(child, query, class_name, limit, chain, found, total);
        chain.pop();
    }
}

/// The chains of every instance made from `file`.
///
/// `file` should be canonical. Sourcemap paths usually are too, but when an
/// exact match fails, paths with the same file name are canonicalized before
/// comparing them, which catches differences such as letter case on Windows.
pub fn find_by_file<'a>(root: &'a Node, file: &Path) -> Vec<Vec<&'a Node>> {
    let mut exact = Vec::new();
    let mut chain = Vec::new();
    collect_by_file(root, &mut chain, &mut exact, &|path| path == file);

    if !exact.is_empty() {
        return exact;
    }

    let mut fuzzy = Vec::new();
    collect_by_file(root, &mut chain, &mut fuzzy, &|path| {
        path.file_name() == file.file_name()
            && dunce::canonicalize(path).is_ok_and(|path| path == file)
    });
    fuzzy
}

fn collect_by_file<'a>(
    node: &'a Node,
    chain: &mut Vec<&'a Node>,
    found: &mut Vec<Vec<&'a Node>>,
    matches: &dyn Fn(&Path) -> bool,
) {
    for child in &node.children {
        chain.push(child);
        if child.file_paths.iter().any(|path| matches(path)) {
            found.push(chain.clone());
        }
        collect_by_file(child, chain, found, matches);
        chain.pop();
    }
}

/// Writes an indented outline of `node` and its descendants up to `depth`
/// levels down.
pub fn outline(node: &Node, label: &str, depth: usize, project_dir: &Path) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{}", describe(node, label, project_dir));
    outline_children(node, depth, 1, project_dir, &mut out);
    out
}

fn outline_children(node: &Node, depth: usize, level: usize, project_dir: &Path, out: &mut String) {
    if level > depth {
        if !node.children.is_empty() {
            let _ = writeln!(
                out,
                "{}... {} children not shown",
                "  ".repeat(level),
                node.children.len()
            );
        }
        return;
    }

    for child in node.children.iter().take(MAX_LISTED_CHILDREN) {
        let _ = writeln!(
            out,
            "{}{}",
            "  ".repeat(level),
            describe(child, &child.name, project_dir)
        );
        outline_children(child, depth, level + 1, project_dir, out);
    }

    if node.children.len() > MAX_LISTED_CHILDREN {
        let _ = writeln!(
            out,
            "{}... {} more children",
            "  ".repeat(level),
            node.children.len() - MAX_LISTED_CHILDREN
        );
    }
}

/// `label (ClassName)` followed by the files the instance comes from.
pub fn describe(node: &Node, label: &str, project_dir: &Path) -> String {
    let mut line = format!("{} ({})", label, node.class_name);
    if !node.file_paths.is_empty() {
        line.push_str(" <- ");
        line.push_str(&display_paths(&node.file_paths, project_dir));
    }
    line
}

/// Paths relative to the project directory where possible, to keep output
/// short.
pub fn display_paths(paths: &[PathBuf], project_dir: &Path) -> String {
    paths
        .iter()
        .map(|path| {
            path.strip_prefix(project_dir)
                .unwrap_or(path)
                .display()
                .to_string()
                .replace('\\', "/")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Lists the children of a node, summarizing beyond a limit.
pub fn list_children(node: &Node, limit: usize) -> String {
    let mut out = String::new();
    for child in node.children.iter().take(limit) {
        let _ = writeln!(out, "  {} ({})", child.name, child.class_name);
    }
    if node.children.len() > limit {
        let _ = writeln!(out, "  ... {} more", node.children.len() - limit);
    }
    out
}

#[cfg(test)]
mod test {
    use super::*;

    fn node(name: &str, class_name: &str, children: Vec<Node>) -> Node {
        Node {
            name: name.to_owned(),
            class_name: class_name.to_owned(),
            file_paths: Vec::new(),
            children,
        }
    }

    fn file(name: &str, class_name: &str, path: &str) -> Node {
        Node {
            file_paths: vec![PathBuf::from(path)],
            ..node(name, class_name, Vec::new())
        }
    }

    fn place() -> Node {
        node(
            "Game",
            "DataModel",
            vec![node(
                "ReplicatedStorage",
                "ReplicatedStorage",
                vec![
                    node(
                        "Shared",
                        "Folder",
                        vec![
                            file("Util", "ModuleScript", "/p/src/shared/Util.luau"),
                            file("Util.spec", "ModuleScript", "/p/src/shared/Util.spec.luau"),
                        ],
                    ),
                    node("Shared.Old", "Folder", vec![]),
                ],
            )],
        )
    }

    fn names(chain: &[&Node]) -> Vec<String> {
        chain.iter().map(|node| node.name.clone()).collect()
    }

    #[test]
    fn resolves_dotted_paths_with_dots_in_names() {
        let root = place();

        let chain = resolve(&root, "ReplicatedStorage.Shared.Util.spec").unwrap();
        assert_eq!(names(&chain), ["ReplicatedStorage", "Shared", "Util.spec"]);

        let chain = resolve(&root, "game.ReplicatedStorage.Shared.Old").unwrap();
        assert_eq!(names(&chain), ["ReplicatedStorage", "Shared.Old"]);

        let chain = resolve(&root, "ReplicatedStorage/Shared/Util").unwrap();
        assert_eq!(names(&chain), ["ReplicatedStorage", "Shared", "Util"]);

        assert!(resolve(&root, "").unwrap().is_empty());
        assert!(resolve(&root, "game").unwrap().is_empty());
        assert!(resolve(&root, "ReplicatedStorage.Missing").is_none());
    }

    #[test]
    fn finds_by_name_class_and_full_path() {
        let root = place();

        let (found, total) = find(&root, Some("util"), None, 10);
        assert_eq!(total, 2);
        assert_eq!(full_name(&found[0]), "ReplicatedStorage.Shared.Util");

        let (found, _) = find(&root, None, Some("folder"), 10);
        assert_eq!(found.len(), 2);

        let (found, _) = find(&root, Some("shared.util.spec"), None, 10);
        assert_eq!(found.len(), 1);

        let (found, total) = find(&root, Some("util"), None, 1);
        assert_eq!((found.len(), total), (1, 2));
    }

    #[test]
    fn finds_instances_by_file() {
        let root = place();

        let found = find_by_file(&root, Path::new("/p/src/shared/Util.luau"));
        assert_eq!(found.len(), 1);
        assert_eq!(full_name(&found[0]), "ReplicatedStorage.Shared.Util");

        assert!(find_by_file(&root, Path::new("/p/src/missing.luau")).is_empty());
    }

    #[test]
    fn outlines_are_depth_limited_and_use_relative_paths() {
        let root = place();
        let text = outline(&root, "Game", 2, Path::new("/p"));

        assert!(text.contains("Game (DataModel)"));
        assert!(text.contains("  ReplicatedStorage (ReplicatedStorage)"));
        assert!(text.contains("    Shared (Folder)"));
        assert!(text.contains("... 2 children not shown"));
        assert!(!text.contains("Util.spec"));

        let deeper = outline(&root, "Game", 3, Path::new("/p"));
        assert!(deeper.contains("Util (ModuleScript) <- src/shared/Util.luau"));
    }
}
