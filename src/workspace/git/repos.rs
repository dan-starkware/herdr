//! Repository registry: discover the repositories the user can act on from the
//! control panel, list their branches, and resolve review bases.
//!
//! Discovery scans a configured root directory (default `~/workspace`) for git
//! repositories, deduped by git-common-dir so linked worktrees collapse into the
//! repository they belong to.

// Phase 0 backend: consumed by the control-half UI in Phase 1. Until then these
// public entry points have no in-crate caller outside tests.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::discovery::{git_space_metadata, git_trimmed_stdout};

/// A git repository surfaced in the control panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    /// Stable identity: canonical git-common-dir, shared across linked worktrees.
    pub key: String,
    /// Filesystem root of the primary (non-linked) checkout.
    pub root: PathBuf,
    /// Human label (repository directory name).
    pub label: String,
}

/// A branch candidate offered in the review picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    /// Display/checkout name (`feature`, or `origin/feature` for remote-only refs).
    pub name: String,
    /// Whether this is the repository's currently checked-out branch.
    pub is_current: bool,
    /// Whether this came from `refs/remotes/*` with no matching local head.
    pub is_remote: bool,
}

/// The default directory scanned for repositories when none is configured.
pub fn default_scan_root() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| Path::new(&home).join("workspace"))
}

/// Discover repositories under `scan_root` (immediate subdirectories only).
///
/// Each subdirectory is probed with [`git_space_metadata`]; results are deduped by
/// git-common-dir key, preferring the non-linked (primary) checkout as the root.
pub fn scan_repositories(scan_root: &Path) -> Vec<Repository> {
    let Ok(entries) = std::fs::read_dir(scan_root) else {
        return Vec::new();
    };

    // key -> (repo, came_from_primary_checkout)
    let mut by_key: HashMap<String, (Repository, bool)> = HashMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(meta) = git_space_metadata(&path) else {
            continue;
        };
        let is_primary = !meta.is_linked_worktree;
        let repo = Repository {
            key: meta.key.clone(),
            root: meta.repo_root,
            label: meta.label,
        };
        match by_key.get(&meta.key) {
            // Prefer the primary checkout's root over a linked worktree's.
            Some((_, existing_primary)) if *existing_primary && !is_primary => {}
            _ => {
                by_key.insert(meta.key, (repo, is_primary));
            }
        }
    }

    let mut repos: Vec<Repository> = by_key.into_values().map(|(repo, _)| repo).collect();
    repos.sort_by(|a, b| {
        a.label
            .to_lowercase()
            .cmp(&b.label.to_lowercase())
            .then_with(|| a.root.cmp(&b.root))
    });
    repos
}

/// List branches for `repo_root`: local heads first (current branch first), then
/// remote-only branches whose short name has no local counterpart.
pub fn list_branches(repo_root: &Path) -> Vec<Branch> {
    let raw = match git_command_stdout(
        repo_root,
        &[
            "for-each-ref",
            "--format=%(HEAD)%00%(refname)%00%(refname:short)",
            "refs/heads",
            "refs/remotes",
        ],
    ) {
        Some(raw) => raw,
        None => return Vec::new(),
    };

    let mut locals: Vec<Branch> = Vec::new();
    let mut local_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut remotes: Vec<Branch> = Vec::new();

    for line in raw.lines() {
        let mut fields = line.split('\0');
        let head = fields.next().unwrap_or("");
        let full = fields.next().unwrap_or("");
        let short = fields.next().unwrap_or("");
        if short.is_empty() {
            continue;
        }
        if full.starts_with("refs/remotes/") {
            // Skip symbolic `origin/HEAD` pointers.
            if short.ends_with("/HEAD") {
                continue;
            }
            remotes.push(Branch {
                name: short.to_string(),
                is_current: false,
                is_remote: true,
            });
        } else {
            local_names.insert(short.to_string());
            locals.push(Branch {
                name: short.to_string(),
                is_current: head == "*",
                is_remote: false,
            });
        }
    }

    locals.sort_by(|a, b| {
        b.is_current
            .cmp(&a.is_current)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    // Only surface remote branches that aren't already tracked locally.
    remotes.retain(|remote| {
        let bare = remote
            .name
            .split_once('/')
            .map(|(_, rest)| rest)
            .unwrap_or(remote.name.as_str());
        !local_names.contains(bare)
    });
    remotes.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    locals.extend(remotes);
    locals
}

/// Resolve the Graphite parent branch of `branch`, if Graphite tracks it.
///
/// Graphite (`gt`) stores per-branch metadata as a git ref
/// `refs/branch-metadata/<branch>` whose blob is JSON containing
/// `parentBranchName`. Reading the ref directly avoids invoking `gt` and works
/// regardless of which branch is currently checked out.
pub fn graphite_parent(repo_root: &Path, branch: &str) -> Option<String> {
    let blob = git_trimmed_stdout(
        repo_root,
        &["cat-file", "-p", &format!("refs/branch-metadata/{branch}")],
    )?;
    let value: serde_json::Value = serde_json::from_str(&blob).ok()?;
    let parent = value.get("parentBranchName")?.as_str()?;
    (!parent.is_empty()).then(|| parent.to_string())
}

/// The repository's default branch (`origin/HEAD` target), falling back to
/// `main`/`master` when present.
pub fn default_branch(repo_root: &Path) -> Option<String> {
    if let Some(symbolic) =
        git_trimmed_stdout(repo_root, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
    {
        if let Some((_, branch)) = symbolic.split_once('/') {
            if !branch.is_empty() {
                return Some(branch.to_string());
            }
        }
    }

    for candidate in ["main", "master"] {
        if git_trimmed_stdout(
            repo_root,
            &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{candidate}")],
        )
        .is_some()
        {
            return Some(candidate.to_string());
        }
    }
    None
}

/// The base ref to diff `branch` against in review: the Graphite parent, or the
/// repository default branch, or `HEAD` as a last resort.
pub fn review_base(repo_root: &Path, branch: &str) -> String {
    graphite_parent(repo_root, branch)
        .or_else(|| default_branch(repo_root))
        .unwrap_or_else(|| "HEAD".to_string())
}

fn git_command_stdout(repo_root: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::git::test_support::{run_git, temp_test_dir};

    fn init_repo(root: &Path) {
        run_git(root, &["init", "-b", "main", "."]);
        run_git(root, &["config", "user.email", "herdr@example.invalid"]);
        run_git(root, &["config", "user.name", "Herdr Test"]);
        run_git(root, &["commit", "--allow-empty", "-m", "initial"]);
    }

    #[test]
    fn scan_repositories_lists_repos_and_skips_non_repos() {
        let scan = temp_test_dir("scan-root");
        let repo_a = scan.join("alpha");
        let repo_b = scan.join("beta");
        std::fs::create_dir_all(&repo_a).unwrap();
        std::fs::create_dir_all(&repo_b).unwrap();
        std::fs::create_dir_all(scan.join("not-a-repo")).unwrap();
        init_repo(&repo_a);
        init_repo(&repo_b);

        let repos = scan_repositories(&scan);
        let labels: Vec<&str> = repos.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, vec!["alpha", "beta"]);

        std::fs::remove_dir_all(scan).unwrap();
    }

    #[test]
    fn scan_repositories_collapses_linked_worktrees_into_primary() {
        let scan = temp_test_dir("scan-worktrees");
        let repo = scan.join("proj");
        std::fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        let linked = scan.join("proj-feature");
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "feature",
                linked.to_str().unwrap(),
            ],
        );

        let repos = scan_repositories(&scan);
        assert_eq!(repos.len(), 1, "linked worktree should collapse into one repo");
        assert_eq!(repos[0].label, "proj");
        assert_eq!(
            super::super::discovery::canonicalize_best_effort_path(&repos[0].root),
            super::super::discovery::canonicalize_best_effort_path(&repo),
            "primary checkout should win as the repo root"
        );

        std::fs::remove_dir_all(scan).unwrap();
    }

    #[test]
    fn list_branches_orders_current_first_then_alphabetical() {
        let root = temp_test_dir("list-branches");
        init_repo(&root);
        run_git(&root, &["branch", "zebra"]);
        run_git(&root, &["branch", "apple"]);

        let branches = list_branches(&root);
        let names: Vec<&str> = branches.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, vec!["main", "apple", "zebra"]);
        assert!(branches[0].is_current);
        assert!(!branches[1].is_current);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn default_branch_prefers_main_when_no_remote() {
        let root = temp_test_dir("default-branch");
        init_repo(&root);

        assert_eq!(default_branch(&root).as_deref(), Some("main"));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn graphite_parent_reads_branch_metadata_ref() {
        let root = temp_test_dir("graphite-parent");
        init_repo(&root);
        run_git(&root, &["checkout", "-b", "child"]);
        run_git(&root, &["commit", "--allow-empty", "-m", "child work"]);

        // Simulate Graphite metadata: a ref pointing at a JSON blob.
        let json = r#"{"parentBranchName":"main","parentBranchRevision":"abc"}"#;
        let hash_out = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["hash-object", "-w", "--stdin"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(json.as_bytes())
                    .unwrap();
                child.wait_with_output()
            })
            .unwrap();
        let oid = String::from_utf8(hash_out.stdout).unwrap().trim().to_string();
        run_git(&root, &["update-ref", "refs/branch-metadata/child", &oid]);

        assert_eq!(graphite_parent(&root, "child").as_deref(), Some("main"));
        assert_eq!(graphite_parent(&root, "main"), None);
        assert_eq!(review_base(&root, "child"), "main");

        std::fs::remove_dir_all(root).unwrap();
    }
}
