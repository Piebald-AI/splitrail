//! Groups the working directories that analyzers record into projects.
//!
//! One repository shows up under many paths: subdirectories, linked worktrees
//! (inside `<repo>/.worktrees/`, in a sibling `<repo>-worktrees/` folder, or
//! anywhere else), clones that moved, and remotes that were renamed. Paths
//! join one project when the filesystem or an analyzer's project hash links
//! them; links are transitive.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

/// Folders inside a repository root that hold its linked worktrees.
const IN_REPO_WORKTREES: [&str; 2] = [".worktrees", ".worktree"];

pub(super) struct Projects {
    by_path: HashMap<String, String>,
    by_hash: HashMap<String, String>,
    /// Recorded and inferred paths merged into each project, keyed by project id.
    pub(super) paths: BTreeMap<String, BTreeSet<String>>,
}

impl Projects {
    /// Groups every message's path, joined by the `(project hash, path)` pair
    /// where each session started. Later paths of a session are not linked:
    /// a session that changes directory into another repository would
    /// otherwise merge the two. A project's id is its most useful path: one
    /// that still exists, then the one with the most messages.
    pub(super) fn group<'a>(
        session_starts: impl IntoIterator<Item = (&'a str, &'a str)>,
        message_paths: impl IntoIterator<Item = &'a str>,
        home: Option<&Path>,
    ) -> Self {
        let mut counts: HashMap<&str, u64> = HashMap::new();
        for path in message_paths {
            *counts.entry(path).or_default() += 1;
        }
        let known: HashSet<&str> = counts.keys().copied().collect();

        let mut sets = Sets::default();
        let mut weight: HashMap<usize, u64> = HashMap::new();
        for (&path, &count) in &counts {
            let path_node = sets.node(Node::Path(path.to_string()));
            let anchor = match repository_root(Path::new(path), home, &known) {
                Some(root) => {
                    let root_node = sets.node(Node::Path(root.to_string_lossy().into_owned()));
                    sets.union(path_node, root_node);
                    root_node
                }
                None => path_node,
            };
            *weight.entry(anchor).or_default() += count;
        }
        for (hash, path) in session_starts {
            if !hash.is_empty() {
                let hash_node = sets.node(Node::Hash(hash.to_string()));
                let path_node = sets.node(Node::Path(path.to_string()));
                sets.union(path_node, hash_node);
            }
        }

        let groups: Vec<usize> = (0..sets.nodes.len()).map(|node| sets.find(node)).collect();
        let mut best: HashMap<usize, (bool, u64, &str)> = HashMap::new();
        for (node, group) in groups.iter().enumerate() {
            let Node::Path(path) = &sets.nodes[node] else {
                continue;
            };
            let candidate = (
                Path::new(path).exists(),
                weight.get(&node).copied().unwrap_or(0),
                path.as_str(),
            );
            let current = best.entry(*group).or_insert(candidate);
            // Prefer existing paths, then busier ones; ties keep the shorter-sorting path.
            if (candidate.0, candidate.1) > (current.0, current.1)
                || ((candidate.0, candidate.1) == (current.0, current.1) && candidate.2 < current.2)
            {
                *current = candidate;
            }
        }

        let mut projects = Projects {
            by_path: HashMap::new(),
            by_hash: HashMap::new(),
            paths: BTreeMap::new(),
        };
        for (node, group) in groups.iter().enumerate() {
            match &sets.nodes[node] {
                Node::Path(path) => {
                    let id = best[group].2.to_string();
                    projects
                        .paths
                        .entry(id.clone())
                        .or_default()
                        .insert(path.clone());
                    projects.by_path.insert(path.clone(), id);
                }
                Node::Hash(hash) => {
                    // A hash never seen with a path is its own project.
                    let id = best.get(group).map_or(hash.as_str(), |best| best.2);
                    projects.by_hash.insert(hash.clone(), id.to_string());
                }
            }
        }
        projects
    }

    /// The project id for a message; messages without a known path or hash
    /// keep their raw hash, as before grouping existed.
    pub(super) fn id<'a>(&'a self, hash: &'a str, path: Option<&str>) -> &'a str {
        path.and_then(|path| self.by_path.get(path))
            .or_else(|| self.by_hash.get(hash))
            .map_or(hash, String::as_str)
    }
}

/// The repository a recorded path belongs to, when the path itself, the
/// filesystem, or a sibling worktree folder tells. `known` holds every
/// recorded path, so worktrees of a repository that has since moved still
/// find its old location.
fn repository_root(path: &Path, home: Option<&Path>, known: &HashSet<&str>) -> Option<PathBuf> {
    // `<repo>/.worktrees/<name>` stays attributable after the worktree is removed.
    let components: Vec<Component> = path.components().collect();
    if let Some(index) = components.iter().position(|component| {
        matches!(component, Component::Normal(name)
            if name.to_str().is_some_and(|name| IN_REPO_WORKTREES.contains(&name)))
    }) && index > 0
    {
        return Some(components[..index].iter().collect());
    }

    // The nearest enclosing checkout; a linked worktree's `.git` file names the
    // main repository. The walk stops below the home directory so a repository
    // there cannot claim every project.
    for dir in path
        .ancestors()
        .take_while(|dir| Some(*dir) != home && dir.parent().is_some())
    {
        let dot_git = dir.join(".git");
        if dot_git.is_dir() {
            return Some(dir.to_path_buf());
        }
        if dot_git.is_file() {
            return Some(main_worktree(&dot_git).unwrap_or_else(|| dir.to_path_buf()));
        }
    }

    // A removed worktree from a `<repo>-worktrees/` folder whose main checkout
    // sits inside that folder or next to it.
    path.ancestors().skip(1).find_map(|dir| {
        let repo = dir
            .file_name()?
            .to_str()?
            .strip_suffix("-worktrees")
            .filter(|repo| !repo.is_empty())?;
        [dir.join(repo), dir.with_file_name(repo)]
            .into_iter()
            .filter(|checkout| checkout != path)
            .find(|checkout| {
                checkout.join(".git").exists()
                    || checkout
                        .to_str()
                        .is_some_and(|checkout| known.contains(checkout))
            })
    })
}

/// The main working tree of a linked worktree, from its `.git` file
/// (`gitdir: <common dir>/worktrees/<name>`) and that directory's `commondir`.
/// Submodules have no `commondir` and are their own project.
fn main_worktree(dot_git: &Path) -> Option<PathBuf> {
    let link = std::fs::read_to_string(dot_git).ok()?;
    let gitdir = dot_git
        .parent()?
        .join(link.trim().strip_prefix("gitdir:")?.trim());
    let common = std::fs::read_to_string(gitdir.join("commondir")).ok()?;
    let common = normalize(&gitdir.join(common.trim()));
    // A bare repository has no main checkout, so the repository names the project.
    if common.file_name()? == ".git" {
        common.parent().map(Path::to_path_buf)
    } else {
        Some(common)
    }
}

/// Resolves `.` and `..` lexically, keeping symlinked paths as git recorded them.
fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other),
        }
    }
    normalized
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum Node {
    Path(String),
    Hash(String),
}

/// Union-find over paths and analyzer project hashes.
#[derive(Default)]
struct Sets {
    nodes: Vec<Node>,
    index: HashMap<Node, usize>,
    parent: Vec<usize>,
}

impl Sets {
    fn node(&mut self, node: Node) -> usize {
        if let Some(&index) = self.index.get(&node) {
            return index;
        }
        let index = self.nodes.len();
        self.nodes.push(node.clone());
        self.index.insert(node, index);
        self.parent.push(index);
        index
    }

    fn find(&mut self, mut node: usize) -> usize {
        while self.parent[node] != node {
            self.parent[node] = self.parent[self.parent[node]];
            node = self.parent[node];
        }
        node
    }

    fn union(&mut self, left: usize, right: usize) {
        let (left, right) = (self.find(left), self.find(right));
        if left != right {
            self.parent[right] = left;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn text(path: &Path) -> String {
        path.components()
            .collect::<PathBuf>()
            .to_string_lossy()
            .into_owned()
    }

    /// Lays out a linked worktree the way `git worktree add` does.
    fn link_worktree(common_dir: &Path, worktree: &Path) {
        let gitdir = common_dir.join("worktrees/linked");
        fs::create_dir_all(&gitdir).unwrap();
        fs::write(gitdir.join("commondir"), "../..\n").unwrap();
        fs::create_dir_all(worktree).unwrap();
        fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();
    }

    #[test]
    fn merges_worktrees_moved_clones_and_subdirectories() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let main = home.join("src/example-worktrees/example");
        fs::create_dir_all(main.join("cmd/tool")).unwrap();
        link_worktree(&main.join(".git"), &home.join("elsewhere/example-feature"));
        let kubevirt = home.join("src/kubevirt");
        fs::create_dir_all(kubevirt.join(".git")).unwrap();

        let id = text(&main);
        let merged = [
            id.clone(),
            text(&main.join("cmd/tool")),
            text(&home.join("elsewhere/example-feature")),
            text(&main.join(".worktrees/removed/src")),
            text(&main.join(".worktree/removed-too")),
            text(&home.join("src/example-worktrees/example-issue-1")),
            // The repository moved; worktrees left beside its old location still find it.
            text(Path::new("/old/example-worktrees/example")),
            text(Path::new("/old/example-worktrees/example-issue-2")),
            text(Path::new("/older/example")),
        ];
        let other = text(&home.join("src/example-other"));
        let kubevirt = text(&kubevirt);
        let starts = [
            ("remote-a", merged[0].as_str()),
            ("claude-feature", merged[2].as_str()),
            // A moved clone shares the remote's hash; a renamed remote was seen at the same path.
            ("remote-a", merged[6].as_str()),
            ("remote-b", merged[6].as_str()),
            ("remote-b", merged[8].as_str()),
            ("other", other.as_str()),
        ];
        // A session that started in `main` later ran in `kubevirt`; only its start links a hash.
        let message_paths = merged
            .iter()
            .map(String::as_str)
            .chain([other.as_str(), kubevirt.as_str()]);
        let projects = Projects::group(starts, message_paths, Some(home));

        for path in &merged {
            assert_eq!(projects.id("", Some(path.as_str())), id, "{path}");
        }
        assert_eq!(projects.paths[&id].len(), merged.len());
        assert_eq!(projects.id("remote-b", None), id);
        assert_eq!(projects.id("remote-a", Some(&kubevirt)), kubevirt);
        // Similar names stay apart, and hashes without any link stay as they are.
        assert_eq!(projects.id("other", Some(&other)), other);
        assert_eq!(projects.id("orphan", None), "orphan");
        assert_eq!(projects.id("", None), "");
    }

    #[test]
    fn home_checkout_claims_nothing_and_bare_repositories_name_their_worktrees() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        fs::create_dir_all(home.join(".git")).unwrap();
        fs::create_dir_all(home.join("notes")).unwrap();
        let bare = home.join("src/tool.git");
        link_worktree(&bare, &home.join("src/tool/main"));

        let notes = text(&home.join("notes"));
        let checkout = text(&home.join("src/tool/main"));
        let projects = Projects::group(
            [("n", notes.as_str()), ("t", checkout.as_str())],
            [notes.as_str(), checkout.as_str()],
            Some(home),
        );
        assert_eq!(projects.id("n", Some(&notes)), notes);
        assert_eq!(projects.id("t", Some(&checkout)), text(&bare));
    }
}
