//! Repository registry: discover the git repositories the user can open from the
//! repo chooser.
//!
//! Discovery scans a single root directory (`$HERDR_REPO_SCAN_ROOT`, else
//! `~/workspaces`) for git repositories, deduped by git-common-dir so a repo's
//! linked worktrees collapse into the one repository they belong to.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::discovery::git_space_metadata;

/// A git repository surfaced in the repo chooser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    /// Stable identity: canonical git-common-dir, shared across linked worktrees.
    pub key: String,
    /// Filesystem root of the primary (non-linked) checkout.
    pub root: PathBuf,
    /// Human label (the repository directory name).
    pub label: String,
}

/// The directory scanned for repositories. `$HERDR_REPO_SCAN_ROOT` overrides the
/// default of `~/workspaces`.
pub fn default_scan_root() -> Option<PathBuf> {
    if let Some(override_dir) = std::env::var_os("HERDR_REPO_SCAN_ROOT") {
        return Some(PathBuf::from(override_dir));
    }
    std::env::var_os("HOME").map(|home| Path::new(&home).join("workspaces"))
}

/// Discover repositories under `scan_root` (immediate subdirectories only).
///
/// Each subdirectory is probed with [`git_space_metadata`]; results are deduped
/// by git-common-dir key, preferring the non-linked (primary) checkout as the
/// root so a repo's many linked worktrees collapse into one entry. Returns an
/// empty list if `scan_root` can't be read.
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
            // Keep the primary checkout's root over a linked worktree's.
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

#[cfg(test)]
mod tests {
    use super::super::test_support::{run_git, temp_test_dir};
    use super::*;

    fn init_repo(root: &Path) {
        std::fs::create_dir_all(root).unwrap();
        run_git(root, &["init", "-q", "-b", "main"]);
        run_git(root, &["config", "user.email", "test@example.com"]);
        run_git(root, &["config", "user.name", "test"]);
        std::fs::write(root.join("README.md"), "hi\n").unwrap();
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-q", "-m", "init"]);
    }

    #[test]
    fn default_scan_root_uses_env_override() {
        // SAFETY: no other test reads or writes HERDR_REPO_SCAN_ROOT.
        std::env::set_var("HERDR_REPO_SCAN_ROOT", "/tmp/herdr-scan-override");
        let root = default_scan_root();
        std::env::remove_var("HERDR_REPO_SCAN_ROOT");
        assert_eq!(root, Some(PathBuf::from("/tmp/herdr-scan-override")));
    }

    #[test]
    fn scan_missing_dir_is_empty() {
        let missing = temp_test_dir("scan-missing").join("does-not-exist");
        assert!(scan_repositories(&missing).is_empty());
    }

    #[test]
    fn scan_ignores_non_git_subdirs() {
        let scan_root = temp_test_dir("scan-non-git");
        std::fs::create_dir_all(scan_root.join("just-a-folder")).unwrap();
        std::fs::write(scan_root.join("a-file"), "x").unwrap();
        assert!(scan_repositories(&scan_root).is_empty());
    }

    #[test]
    fn scan_finds_repos_sorted_by_label() {
        let scan_root = temp_test_dir("scan-sorted");
        init_repo(&scan_root.join("zebra"));
        init_repo(&scan_root.join("alpha"));
        let repos = scan_repositories(&scan_root);
        let labels: Vec<&str> = repos.iter().map(|repo| repo.label.as_str()).collect();
        assert_eq!(labels, vec!["alpha", "zebra"]);
    }

    #[test]
    fn scan_dedupes_linked_worktree_into_primary() {
        let scan_root = temp_test_dir("scan-dedupe");
        let primary = scan_root.join("project");
        init_repo(&primary);
        // A linked worktree of the same repo, also sitting under the scan root.
        let worktree = scan_root.join("project-feature");
        run_git(
            &primary,
            &[
                "worktree",
                "add",
                "-q",
                worktree.to_str().unwrap(),
                "-b",
                "feature",
            ],
        );

        let repos = scan_repositories(&scan_root);
        assert_eq!(
            repos.len(),
            1,
            "linked worktree should collapse into its repo"
        );
        let repo = &repos[0];
        // The surviving entry points at the primary checkout, not the worktree.
        assert_eq!(
            std::fs::canonicalize(&repo.root).unwrap(),
            std::fs::canonicalize(&primary).unwrap()
        );
    }
}
