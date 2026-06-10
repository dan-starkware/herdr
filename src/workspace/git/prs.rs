//! GitHub pull-request queries via the `gh` CLI: list the open PRs awaiting
//! the user's review, for the branch picker's "reviewing" list.

use std::path::Path;

/// An open pull request awaiting the user's review.
///
/// Also stored on a [`crate::workspace::Workspace`] (as `reviewing_pr`) once the
/// PR is opened for review, so it serializes with the session snapshot.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReviewPr {
    pub number: u64,
    pub title: String,
    /// The PR author's login.
    pub author: String,
    /// The PR's head branch (what `gh pr checkout` checks out).
    pub head_branch: String,
    /// The PR's base branch (what the review diff is taken against).
    pub base_branch: String,
    pub url: String,
    /// Stack art rendered before the row: `◯ ` for the top of a stack, `│ `
    /// for the PRs under it, empty for a standalone PR. Filled in by
    /// [`partition_into_stacks`]; purely cosmetic, so it defaults empty when
    /// deserializing older session snapshots.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub graph_prefix: String,
}

/// List the open PRs in `repo_root`'s repository where the user's review is
/// requested, via `gh pr list --search "review-requested:@me"`.
///
/// Errors carry a user-facing message (gh missing, not authenticated, …).
pub fn list_prs_for_my_review(repo_root: &Path) -> Result<Vec<ReviewPr>, String> {
    let output = std::process::Command::new("gh")
        .current_dir(repo_root)
        .args([
            "pr",
            "list",
            "--search",
            "review-requested:@me",
            "--json",
            "number,title,author,headRefName,baseRefName,url",
        ])
        .output()
        .map_err(|err| format!("gh not available: {err}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    parse_pr_list(&String::from_utf8_lossy(&output.stdout))
}

/// Parse `gh pr list --json number,title,author,headRefName,baseRefName,url`
/// output into [`ReviewPr`]s.
fn parse_pr_list(raw: &str) -> Result<Vec<ReviewPr>, String> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct RawPr {
        number: u64,
        title: String,
        author: RawAuthor,
        head_ref_name: String,
        base_ref_name: String,
        url: String,
    }
    #[derive(serde::Deserialize)]
    struct RawAuthor {
        login: String,
    }

    let prs: Vec<RawPr> =
        serde_json::from_str(raw).map_err(|err| format!("unexpected gh output: {err}"))?;
    Ok(partition_into_stacks(
        prs.into_iter()
            .map(|pr| ReviewPr {
                number: pr.number,
                title: pr.title,
                author: pr.author.login,
                head_branch: pr.head_ref_name,
                base_branch: pr.base_ref_name,
                url: pr.url,
                graph_prefix: String::new(),
            })
            .collect(),
    ))
}

/// Group the PRs into their Graphite stacks and order each stack top-first.
///
/// Others' PR branches aren't in the local Graphite metadata, but Graphite
/// encodes the stack in the PRs themselves: a stacked PR's base branch IS its
/// parent's head branch. Chaining `base_branch -> head_branch` therefore
/// reconstructs Graphite's graph (and works for manually-stacked PRs too).
///
/// Output order: stacks keep gh's ordering of their roots; within a stack the
/// top-most PR comes first (like `gt log`), parents after their children. Rows
/// get a `graph_prefix`: `◯ ` on stack tops (leaves), `│ ` on the PRs they sit
/// on, nothing on standalone PRs.
fn partition_into_stacks(prs: Vec<ReviewPr>) -> Vec<ReviewPr> {
    use std::collections::HashMap;

    let by_head: HashMap<&str, usize> = prs
        .iter()
        .enumerate()
        .map(|(idx, pr)| (pr.head_branch.as_str(), idx))
        .collect();
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); prs.len()];
    let mut parent: Vec<Option<usize>> = vec![None; prs.len()];
    for (idx, pr) in prs.iter().enumerate() {
        if let Some(&parent_idx) = by_head.get(pr.base_branch.as_str()) {
            if parent_idx != idx {
                children[parent_idx].push(idx);
                parent[idx] = Some(parent_idx);
            }
        }
    }

    // A stack takes the list position of its first member in gh's (newest
    // first) ordering, so a stack with a fresh PR on top stays near the top.
    let mut root_order: Vec<usize> = Vec::new();
    let mut root_seen = vec![false; prs.len()];
    for idx in 0..prs.len() {
        let mut root = idx;
        let mut steps = 0;
        while let Some(up) = parent[root] {
            root = up;
            steps += 1;
            if steps > prs.len() {
                break; // base->head cycle: no root; the fallback below keeps it
            }
        }
        if steps <= prs.len() && !root_seen[root] {
            root_seen[root] = true;
            root_order.push(root);
        }
    }

    // Emit each tree post-order (children above their parent), iteratively to
    // sidestep recursion limits on degenerate inputs.
    let mut order: Vec<usize> = Vec::with_capacity(prs.len());
    let mut visited = vec![false; prs.len()];
    for root in root_order {
        // (node, children-emitted?) work stack.
        let mut work = vec![(root, false)];
        while let Some((node, expanded)) = work.pop() {
            if expanded {
                order.push(node);
                continue;
            }
            if visited[node] {
                continue;
            }
            visited[node] = true;
            work.push((node, true));
            // Reverse keeps the children in their original (gh) order while
            // emitting each child's subtree before the parent.
            for &child in children[node].iter().rev() {
                work.push((child, false));
            }
        }
    }
    // A base->head cycle (malformed data) leaves every participant parented;
    // surface them flat rather than dropping them.
    for idx in 0..prs.len() {
        if !visited[idx] {
            order.push(idx);
            children[idx].clear();
            parent[idx] = None;
        }
    }

    let mut out: Vec<ReviewPr> = Vec::with_capacity(prs.len());
    let mut slots: Vec<Option<ReviewPr>> = prs.into_iter().map(Some).collect();
    for idx in order {
        let mut pr = slots[idx].take().expect("each PR is emitted exactly once");
        let in_stack = parent[idx].is_some() || !children[idx].is_empty();
        pr.graph_prefix = if !in_stack {
            String::new()
        } else if children[idx].is_empty() {
            "◯ ".to_string()
        } else {
            "│ ".to_string()
        };
        out.push(pr);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gh_pr_list_json() {
        let raw = r#"[
            {
                "author": {"id": "u1", "is_bot": false, "login": "alice", "name": "Alice"},
                "baseRefName": "master",
                "headRefName": "alice/fix-parser",
                "number": 412,
                "title": "Fix parser panic on empty input",
                "url": "https://github.com/acme/proj/pull/412"
            },
            {
                "author": {"login": "bob"},
                "baseRefName": "main",
                "headRefName": "bob/feature",
                "number": 7,
                "title": "Add feature",
                "url": "https://github.com/acme/proj/pull/7"
            }
        ]"#;
        let prs = parse_pr_list(raw).unwrap();
        assert_eq!(prs.len(), 2);
        assert_eq!(
            prs[0],
            ReviewPr {
                number: 412,
                title: "Fix parser panic on empty input".to_string(),
                author: "alice".to_string(),
                head_branch: "alice/fix-parser".to_string(),
                base_branch: "master".to_string(),
                url: "https://github.com/acme/proj/pull/412".to_string(),
                graph_prefix: String::new(),
            }
        );
        assert_eq!(prs[1].author, "bob");
    }

    fn pr(number: u64, head: &str, base: &str) -> ReviewPr {
        ReviewPr {
            number,
            title: format!("pr {number}"),
            author: "alice".to_string(),
            head_branch: head.to_string(),
            base_branch: base.to_string(),
            url: format!("https://github.com/acme/proj/pull/{number}"),
            graph_prefix: String::new(),
        }
    }

    #[test]
    fn stacks_chain_base_to_head_top_first() {
        // gh order is newest-first; the chain is 3 (top) -> 2 -> 1 (on master).
        let prs = partition_into_stacks(vec![
            pr(2, "feat/two", "feat/one"),
            pr(99, "lone/fix", "master"),
            pr(3, "feat/three", "feat/two"),
            pr(1, "feat/one", "master"),
        ]);
        let view: Vec<(u64, &str)> = prs
            .iter()
            .map(|p| (p.number, p.graph_prefix.as_str()))
            .collect();
        assert_eq!(
            view,
            vec![
                (3, "◯ "),
                (2, "│ "),
                (1, "│ "),
                (99, ""), // standalone PR: no stack art
            ]
        );
    }

    #[test]
    fn stacks_with_two_children_keep_subtrees_contiguous() {
        // 12 and 13 both sit on 11: both are stack tops, the parent below.
        let prs = partition_into_stacks(vec![
            pr(11, "a/base", "master"),
            pr(12, "a/left", "a/base"),
            pr(13, "a/right", "a/base"),
        ]);
        let view: Vec<(u64, &str)> = prs
            .iter()
            .map(|p| (p.number, p.graph_prefix.as_str()))
            .collect();
        assert_eq!(view, vec![(12, "◯ "), (13, "◯ "), (11, "│ ")]);
    }

    #[test]
    fn base_head_cycle_falls_back_to_flat_rows() {
        let prs = partition_into_stacks(vec![
            pr(1, "a", "b"),
            pr(2, "b", "a"),
        ]);
        assert_eq!(prs.len(), 2, "cycle members must not be dropped");
        assert!(prs.iter().all(|p| p.graph_prefix.is_empty()));
    }

    #[test]
    fn empty_list_parses() {
        assert_eq!(parse_pr_list("[]").unwrap(), Vec::new());
    }

    #[test]
    fn malformed_output_is_an_error() {
        assert!(parse_pr_list("not json").is_err());
    }
}
