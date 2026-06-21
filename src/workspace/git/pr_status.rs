//! PR-status data layer backing the keyboard-first PR pane.
//!
//! For every repository discovered under `~/workspace` this fetches — in ONE
//! `gh api graphql` call per repo — the open PRs the user authored
//! (`author:@me`) and the open PRs the user is reviewing
//! (`review-requested:@me` OR `reviewed-by:@me`), together with each PR's review
//! threads (last replier per thread), review submissions (approvals), and the
//! current user's login. Each PR is then classified into one of three buckets
//! (RED waiting-for-me / GREEN lgtm'd / GREY waiting-on-the-other-side) and the
//! PRs are grouped per author for display, with their Graphite stack structure
//! retained so the pane can render parents/children.
//!
//! GitHub code search ANDs its qualifiers (there is no in-query OR), so the
//! "reviewing" set is fetched as two aliased searches (`requested` +
//! `reviewedBy`) and unioned in Rust — collapsing them into one search would
//! silently under-return.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};

use serde::Deserialize;

use super::discovery::git_trimmed_stdout;
use super::{Repository, ReviewPr};

/// Which review state a PR is in, from the current user's point of view.
/// Priority when a PR could fit more than one: RED > GREEN > GREY.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrBucket {
    /// Waiting for me: an unresolved thread where I wasn't the last to reply,
    /// or (for others' PRs) an unreviewed PR with no threads at all.
    Red,
    /// Lgtm'd: approved (others' PRs: by me; my PRs: by any reviewer).
    Green,
    /// Waiting on the other side: every unresolved thread has me as the last
    /// replier, or (for my PRs) it is unreviewed.
    Grey,
}

/// The last comment's author on one GitHub review thread, plus whether the
/// thread is resolved (resolved threads need no reply, so they don't make a PR
/// red).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewThread {
    pub is_resolved: bool,
    /// Login of the author of the thread's most recent comment (`None` only for
    /// the degenerate empty thread).
    pub last_comment_author: Option<String>,
}

/// One submitted review on a PR (`APPROVED` / `CHANGES_REQUESTED` / `COMMENTED`
/// / `DISMISSED` / `PENDING`) and who submitted it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewSubmission {
    pub state: String,
    pub author: String,
}

/// CI status of a PR's latest commit, from its status-check rollup.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CiState {
    /// No checks, or status unknown.
    #[default]
    None,
    /// Checks pending / running.
    Pending,
    /// All checks passed.
    Passing,
    /// At least one check failed or errored.
    Failing,
}

/// A fetched PR with everything classification and stacking need. `is_mine` is
/// not stored — it is derived from `author == viewer` wherever needed, so the
/// viewer login only has to be known by aggregation time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedPr {
    pub number: u64,
    pub title: String,
    pub url: String,
    /// The PR author's login.
    pub author: String,
    pub is_draft: bool,
    /// Base branch (`baseRefName`) — the parent edge in a Graphite stack.
    pub base_ref: String,
    /// Head branch (`headRefName`).
    pub head_ref: String,
    /// Owning repository's stable key ([`Repository::key`]); scopes stack edges.
    pub repo_key: String,
    pub threads: Vec<ReviewThread>,
    pub reviews: Vec<ReviewSubmission>,
    /// CI status of the PR's latest commit (status-check rollup).
    pub ci: CiState,
}

impl FetchedPr {
    /// Adapt to the [`ReviewPr`] the reviewer-mode flow consumes.
    pub fn to_review_pr(&self) -> ReviewPr {
        ReviewPr {
            number: self.number,
            title: self.title.clone(),
            author: self.author.clone(),
            head_branch: self.head_ref.clone(),
            base_branch: self.base_ref.clone(),
            url: self.url.clone(),
            graph_prefix: String::new(),
        }
    }
}

/// Classify one PR for the current user (`me`). Priority RED > GREEN > GREY.
///
/// Resolved threads need no reply, so only UNRESOLVED threads drive the
/// last-replier logic; a PR with no review threads at all is "unreviewed".
pub fn classify_pr(pr: &FetchedPr, me: &str) -> PrBucket {
    let is_mine = pr.author == me;
    let waiting_on_me = pr
        .threads
        .iter()
        .filter(|t| !t.is_resolved)
        .any(|t| t.last_comment_author.as_deref() != Some(me));
    let unreviewed = pr.threads.is_empty();

    // RED: a thread awaits my reply, or someone else's PR is untouched.
    if waiting_on_me {
        return PrBucket::Red;
    }
    if !is_mine && unreviewed {
        return PrBucket::Red;
    }
    // GREEN: approved (mine: by anyone; others': by me).
    if pr_is_approved(pr, me, is_mine) {
        return PrBucket::Green;
    }
    // GREY: all threads me-last, or my own unreviewed PR.
    PrBucket::Grey
}

/// Whether the PR counts as approved. For my PRs, any reviewer's latest
/// meaningful review being `APPROVED` qualifies; for others' PRs, only my own.
fn pr_is_approved(pr: &FetchedPr, me: &str, is_mine: bool) -> bool {
    let latest = latest_state_by_author(pr);
    if is_mine {
        latest.values().any(|state| state == "APPROVED")
    } else {
        latest.get(me).map(|state| state == "APPROVED").unwrap_or(false)
    }
}

/// Each author's most recent *meaningful* review state. `reviews(last:50)`
/// arrives oldest-first, so a later insert wins; `COMMENTED`/`PENDING` are
/// ignored so a stray comment doesn't override a real APPROVED/CHANGES_REQUESTED.
fn latest_state_by_author(pr: &FetchedPr) -> HashMap<String, String> {
    let mut latest = HashMap::new();
    for review in &pr.reviews {
        if review.state == "COMMENTED" || review.state == "PENDING" {
            continue;
        }
        latest.insert(review.author.clone(), review.state.clone());
    }
    latest
}

// ---------------------------------------------------------------------------
// Per-person aggregation + snapshot
// ---------------------------------------------------------------------------

/// One PR under a person, with its computed bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonPr {
    pub pr: FetchedPr,
    pub bucket: PrBucket,
}

/// All of one person's PRs (mine, or one reviewing-author's), with bucket tallies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonPrs {
    pub login: String,
    pub is_me: bool,
    pub prs: Vec<PersonPr>,
    pub red: usize,
    pub green: usize,
    pub grey: usize,
    /// PRs with failing CI — independent of red/green/grey (a PR can be both).
    pub ci: usize,
}

/// The immutable snapshot the PR pane renders. Built off the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrStatusSnapshot {
    pub viewer_login: String,
    /// Me first, then reviewing authors (most-red first).
    pub people: Vec<PersonPrs>,
    /// Full parent/child graph across all fetched PRs, independent of buckets.
    pub stacks: StackGraph,
    pub generated_at: SystemTime,
    /// Per-repo fetch failures (repo label + message); drives a "stale" badge.
    pub errors: Vec<String>,
}

impl PrStatusSnapshot {
    /// Stack-ordered visible PRs for one person, honoring the green/grey toggles
    /// (red is always shown). Returns `(connector_prefix, PersonPr)` rows where
    /// the prefix is the box-drawing stack art (`├ `, `└─┬ `, `│ └ `, …); a
    /// hidden PR's visible descendants re-parent onto the nearest visible
    /// ancestor (req 10), and roots are grouped by repo.
    pub fn visible_person_rows(
        &self,
        login: &str,
        show_green: bool,
        show_grey: bool,
    ) -> Vec<(String, PersonPr)> {
        let Some(person) = self.people.iter().find(|p| p.login == login) else {
            return Vec::new();
        };
        let by_key: HashMap<PrKey, &PersonPr> = person
            .prs
            .iter()
            .map(|pp| ((pp.pr.repo_key.clone(), pp.pr.number), pp))
            .collect();
        let visible = |key: &PrKey| {
            by_key.get(key).is_some_and(|pp| match pp.bucket {
                PrBucket::Red => true,
                PrBucket::Green => show_green,
                PrBucket::Grey => show_grey,
            })
        };
        self.stacks
            .visible_forest(&visible)
            .into_iter()
            .filter_map(|row| by_key.get(&row.key).map(|pp| (row.prefix, (*pp).clone())))
            .collect()
    }
}

/// Group PRs by author (me first, others most-red first), classifying each.
fn aggregate_people(prs: &[FetchedPr], viewer: &str) -> Vec<PersonPrs> {
    let mut by_login: HashMap<String, PersonPrs> = HashMap::new();
    for pr in prs {
        let bucket = classify_pr(pr, viewer);
        let entry = by_login
            .entry(pr.author.clone())
            .or_insert_with(|| PersonPrs {
                login: pr.author.clone(),
                is_me: pr.author == viewer,
                prs: Vec::new(),
                red: 0,
                green: 0,
                grey: 0,
                ci: 0,
            });
        match bucket {
            PrBucket::Red => entry.red += 1,
            PrBucket::Green => entry.green += 1,
            PrBucket::Grey => entry.grey += 1,
        }
        if pr.ci == CiState::Failing {
            entry.ci += 1;
        }
        entry.prs.push(PersonPr { pr: pr.clone(), bucket });
    }

    let mut me: Option<PersonPrs> = None;
    let mut others: Vec<PersonPrs> = Vec::new();
    for person in by_login.into_values() {
        if person.is_me {
            me = Some(person);
        } else {
            others.push(person);
        }
    }
    others.sort_by(|a, b| b.red.cmp(&a.red).then_with(|| a.login.cmp(&b.login)));

    let mut people = Vec::with_capacity(others.len() + 1);
    if let Some(me) = me {
        people.push(me);
    }
    people.extend(others);
    people
}

// ---------------------------------------------------------------------------
// Graphite stack graph
// ---------------------------------------------------------------------------

/// Identity of a PR within a snapshot: `(repo_key, number)`.
pub type PrKey = (String, u64);

#[derive(Debug, Clone, PartialEq, Eq)]
struct StackNode {
    parent: Option<PrKey>,
    children: Vec<PrKey>,
    /// First-appearance index, for deterministic root/child ordering.
    order: usize,
}

/// A row in a rendered stack tree: the PR key plus the box-drawing connector
/// prefix (`├ `, `└─┬ `, `│ └ `, …) that draws the stack shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackRow {
    pub key: PrKey,
    pub prefix: String,
}

/// The parent/child graph across every fetched PR, derived from
/// `base_ref → head_ref` chaining (scoped per repo). Built once and never
/// mutated by the pane's bucket toggles, so descendant links survive when an
/// intermediate PR is filtered out of view.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StackGraph {
    nodes: HashMap<PrKey, StackNode>,
}

impl StackGraph {
    /// Build the graph from all fetched PRs. An edge forms when one PR's
    /// `base_ref` equals another PR's `head_ref` *in the same repo*.
    pub fn build(prs: &[FetchedPr]) -> Self {
        let mut by_head: HashMap<(&str, &str), PrKey> = HashMap::new();
        let mut nodes: HashMap<PrKey, StackNode> = HashMap::new();
        for (order, pr) in prs.iter().enumerate() {
            let key = (pr.repo_key.clone(), pr.number);
            by_head.insert((pr.repo_key.as_str(), pr.head_ref.as_str()), key.clone());
            nodes.insert(
                key,
                StackNode {
                    parent: None,
                    children: Vec::new(),
                    order,
                },
            );
        }
        for pr in prs {
            let key = (pr.repo_key.clone(), pr.number);
            if let Some(parent) = by_head.get(&(pr.repo_key.as_str(), pr.base_ref.as_str())) {
                if *parent != key {
                    let parent = parent.clone();
                    nodes.get_mut(&key).expect("node exists").parent = Some(parent.clone());
                    nodes.get_mut(&parent).expect("parent exists").children.push(key);
                }
            }
        }
        // Order each node's children by first appearance.
        let order_of: HashMap<PrKey, usize> =
            nodes.iter().map(|(k, n)| (k.clone(), n.order)).collect();
        for node in nodes.values_mut() {
            node.children.sort_by_key(|child| order_of.get(child).copied().unwrap_or(usize::MAX));
        }
        StackGraph { nodes }
    }

    /// Render the visible subset as a forest of box-drawing stack trees. A
    /// hidden node (failing `visible`) emits nothing but its visible descendants
    /// re-parent onto the nearest visible ancestor (req 10). Roots are grouped by
    /// repo (so each repo's stacks are contiguous), then by first-appearance.
    /// Each emitted row carries its connector prefix (`├ `, `└─┬ `, `│ └ `, …).
    pub fn visible_forest(&self, visible: &dyn Fn(&PrKey) -> bool) -> Vec<StackRow> {
        // Nearest visible ancestor of `key` (climbing through hidden parents).
        let visible_parent = |key: &PrKey| -> Option<PrKey> {
            let mut cur = self.nodes.get(key).and_then(|node| node.parent.clone());
            while let Some(parent) = cur {
                if visible(&parent) {
                    return Some(parent);
                }
                cur = self.nodes.get(&parent).and_then(|node| node.parent.clone());
            }
            None
        };
        // Build the visible-only parent/child structure.
        let mut vis_children: HashMap<PrKey, Vec<PrKey>> = HashMap::new();
        let mut roots: Vec<PrKey> = Vec::new();
        for key in self.nodes.keys() {
            if !visible(key) {
                continue;
            }
            match visible_parent(key) {
                Some(parent) => vis_children.entry(parent).or_default().push(key.clone()),
                None => roots.push(key.clone()),
            }
        }
        let order = |key: &PrKey| self.nodes.get(key).map(|node| node.order).unwrap_or(usize::MAX);
        // Group roots by repo, then by appearance, so repos render contiguously.
        roots.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| order(a).cmp(&order(b))));
        for children in vis_children.values_mut() {
            children.sort_by_key(|key| order(key));
        }

        let mut out = Vec::new();
        let mut visited = HashSet::new();
        for root in &roots {
            self.walk_tree(root, &[], true, true, &vis_children, &mut out, &mut visited);
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_tree(
        &self,
        key: &PrKey,
        ancestors: &[bool],
        is_last: bool,
        is_root: bool,
        vis_children: &HashMap<PrKey, Vec<PrKey>>,
        out: &mut Vec<StackRow>,
        visited: &mut HashSet<PrKey>,
    ) {
        if !visited.insert(key.clone()) {
            return; // cycle guard
        }
        let children = vis_children.get(key).cloned().unwrap_or_default();
        let mut prefix = String::new();
        if !is_root {
            for &more in ancestors {
                prefix.push_str(if more { "│ " } else { "  " });
            }
            prefix.push(if is_last { '└' } else { '├' });
            if !children.is_empty() {
                prefix.push_str("─┬");
            }
            prefix.push(' ');
        }
        out.push(StackRow { key: key.clone(), prefix });
        for (i, child) in children.iter().enumerate() {
            let child_is_last = i + 1 == children.len();
            // The root sits at column 0 with no spine; deeper levels inherit a
            // `│`/space column per ancestor depending on whether it has more
            // siblings below.
            let child_ancestors: Vec<bool> = if is_root {
                ancestors.to_vec()
            } else {
                let mut a = ancestors.to_vec();
                a.push(!is_last);
                a
            };
            self.walk_tree(child, &child_ancestors, child_is_last, false, vis_children, out, visited);
        }
    }
}

// ---------------------------------------------------------------------------
// Fetching (runs on a worker thread, never the UI thread)
// ---------------------------------------------------------------------------

/// Fetch a fresh snapshot across all `repos`. Per-repo failures are collected
/// into `snapshot.errors` rather than aborting the whole refresh; repos with no
/// GitHub `origin` are skipped silently. Blocking — call off the UI thread.
pub fn fetch_pr_status_snapshot(repos: &[Repository]) -> PrStatusSnapshot {
    // Map each scanned repo's GitHub "owner/name" (lowercased) -> repo key. This
    // is local `git config` only (no API). It lets us run a few GLOBAL searches
    // and filter to ~/workspace client-side: per-repo searches cost 3 requests
    // PER REPO every refresh and blow GitHub's search rate limit, whereas global
    // searches are 3 requests total regardless of repo count.
    let mut repo_by_owner_name: HashMap<String, String> = HashMap::new();
    for repo in repos {
        if let Some(owner_name) = github_owner_name(&repo.root) {
            repo_by_owner_name.insert(owner_name.to_lowercase(), repo.key.clone());
        }
    }
    let generated_at = SystemTime::now();
    if repo_by_owner_name.is_empty() {
        return PrStatusSnapshot {
            viewer_login: viewer_login().unwrap_or_default(),
            people: Vec::new(),
            stacks: StackGraph::default(),
            generated_at,
            errors: Vec::new(),
        };
    }
    let (viewer, prs, errors) = match fetch_global_prs(&repo_by_owner_name) {
        Ok((viewer, prs)) => (viewer, prs, Vec::new()),
        Err(err) => (String::new(), Vec::new(), vec![err]),
    };
    let viewer = if viewer.is_empty() {
        viewer_login().unwrap_or_default()
    } else {
        viewer
    };
    PrStatusSnapshot {
        people: aggregate_people(&prs, &viewer),
        stacks: StackGraph::build(&prs),
        viewer_login: viewer,
        generated_at,
        errors,
    }
}

/// `owner/name` for a repo's GitHub `origin`, or `None` for a non-GitHub remote.
pub fn github_owner_name(repo_root: &Path) -> Option<String> {
    let url = git_trimmed_stdout(repo_root, &["config", "--get", "remote.origin.url"])?;
    parse_github_owner_name(&url)
}

/// Parse `owner/name` out of a GitHub remote URL (ssh or https, with/without
/// `.git`). Returns `None` for non-GitHub hosts.
fn parse_github_owner_name(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("git@github.com:")
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
        .or_else(|| url.strip_prefix("https://github.com/"))
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let rest = rest.strip_suffix(".git").unwrap_or(rest).trim_matches('/');
    let (owner, name) = rest.split_once('/')?;
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    Some(format!("{owner}/{name}"))
}

/// Generous cap on a single `gh` fetch so a slow/hung call can't block the
/// background refresh forever — `gh` exposes no timeout of its own, and a plain
/// `.output()` waits indefinitely. Normal graphql calls finish in a few
/// seconds; this only fires on a genuinely stuck call, after which the refresh
/// self-heals on the next 30s cycle.
const PR_FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// Run a `gh` command with a timeout, draining stdout on a side thread so a
/// large (>64KB) response can't deadlock the child on a full pipe while we poll.
/// Returns stdout on success; on timeout the child is killed and an error
/// returned.
fn run_gh_capped(
    repo_root: Option<&Path>,
    args: &[&str],
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut command = Command::new("gh");
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(root) = repo_root {
        command.current_dir(root);
    }
    let mut child = command
        .spawn()
        .map_err(|err| format!("gh not available: {err}"))?;
    let mut stdout_pipe = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = stdout_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = reader.join();
                    return Err(format!("gh timed out after {}s", timeout.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(err) => {
                let _ = reader.join();
                return Err(format!("gh wait failed: {err}"));
            }
        }
    };
    let stdout = reader.join().unwrap_or_default();
    if status.success() {
        return Ok(stdout);
    }
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    Err(if stderr.trim().is_empty() {
        format!("gh exited unsuccessfully ({status})")
    } else {
        stderr.trim().to_string()
    })
}

/// One global `gh api graphql` call: three searches across all of GitHub (not
/// per repo), filtered to the scanned repos via `repo_by_owner_name`. Returns
/// `(viewer_login, prs)`.
fn fetch_global_prs(
    repo_by_owner_name: &HashMap<String, String>,
) -> Result<(String, Vec<FetchedPr>), String> {
    // Two-phase to stay cheap on GitHub's GraphQL points budget: phase 1 is a
    // cheap global search (ids + scalars only) filtered to ~/workspace; phase 2
    // fetches the expensive detail (reviews/threads/CI) for ONLY those PRs by
    // node id, so cost scales with the local PR count rather than the ~150 PRs
    // the global searches surface.
    let (viewer, refs) = fetch_pr_refs(repo_by_owner_name)?;
    if refs.is_empty() {
        return Ok((viewer, Vec::new()));
    }
    let details = fetch_pr_details(&refs)?;
    let prs = refs
        .into_iter()
        .zip(details)
        .map(|(pr_ref, detail)| FetchedPr {
            number: pr_ref.number,
            title: pr_ref.title,
            url: pr_ref.url,
            author: pr_ref.author,
            is_draft: pr_ref.is_draft,
            base_ref: pr_ref.base_ref,
            head_ref: pr_ref.head_ref,
            repo_key: pr_ref.repo_key,
            threads: detail.threads,
            reviews: detail.reviews,
            ci: detail.ci,
        })
        .collect();
    Ok((viewer, prs))
}

/// Phase 1: three global searches returning only ids + scalar fields (cheap),
/// filtered to the scanned repos. Returns `(viewer_login, refs)`.
fn fetch_pr_refs(
    repo_by_owner_name: &HashMap<String, String>,
) -> Result<(String, Vec<PrRef>), String> {
    let query = format!("query={REF_QUERY}");
    let mine = "mineQuery=is:pr is:open author:@me sort:updated-desc".to_string();
    let requested =
        "requestedQuery=is:pr is:open review-requested:@me sort:updated-desc".to_string();
    let reviewed = "reviewedQuery=is:pr is:open reviewed-by:@me sort:updated-desc".to_string();
    let stdout = run_gh_capped(
        None,
        &["api", "graphql", "-f", &query, "-f", &mine, "-f", &requested, "-f", &reviewed],
        PR_FETCH_TIMEOUT,
    )?;
    let data: RefData = extract_data(&stdout)?;
    let viewer = data.viewer.login.clone();
    Ok((viewer, select_local_refs(data, repo_by_owner_name)))
}

/// Phase 2: fetch reviews/threads/CI for exactly `refs` by node id, in chunks of
/// 100 (`nodes(ids:)` preserves input order). Returns one [`PrDetail`] per ref.
fn fetch_pr_details(refs: &[PrRef]) -> Result<Vec<PrDetail>, String> {
    let mut details: Vec<PrDetail> = Vec::with_capacity(refs.len());
    for chunk in refs.chunks(100) {
        let ids = chunk
            .iter()
            .map(|pr_ref| format!("\"{}\"", pr_ref.id))
            .collect::<Vec<_>>()
            .join(",");
        let mut query = String::from("query=query { nodes(ids: [");
        query.push_str(&ids);
        query.push_str("]) { ... on PullRequest { ");
        query.push_str(DETAIL_FIELDS);
        query.push_str(" } } }");
        let stdout = run_gh_capped(None, &["api", "graphql", "-f", &query], PR_FETCH_TIMEOUT)?;
        let data: DetailData = extract_data(&stdout)?;
        let mut got = 0;
        for node in data.nodes {
            details.push(node.map(detail_from_raw).unwrap_or_default());
            got += 1;
        }
        // Defensive: keep `details` aligned 1:1 with `refs` even if gh returns
        // fewer nodes than ids requested.
        while got < chunk.len() {
            details.push(PrDetail::default());
            got += 1;
        }
    }
    Ok(details)
}

/// Parse a GraphQL envelope, using `data` even when partial `errors` are present
/// (GitHub returns both, e.g. when one field times out); fail only when there is
/// no data at all — discarding good data on a partial error spuriously "fails".
fn extract_data<T: serde::de::DeserializeOwned>(stdout: &[u8]) -> Result<T, String> {
    let envelope: GraphqlEnvelope<T> =
        serde_json::from_slice(stdout).map_err(|err| format!("unexpected gh output: {err}"))?;
    match envelope.data {
        Some(data) => Ok(data),
        None => Err(envelope
            .errors
            .map(|errors| {
                errors
                    .into_iter()
                    .map(|e| e.message)
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| "no data in gh response".to_string())),
    }
}

fn detail_from_raw(node: RawDetailNode) -> PrDetail {
    let threads = node
        .review_threads
        .nodes
        .into_iter()
        .map(|thread| ReviewThread {
            is_resolved: thread.is_resolved,
            last_comment_author: thread
                .comments
                .nodes
                .into_iter()
                .last()
                .and_then(|comment| comment.author)
                .map(|author| author.login),
        })
        .collect();
    let reviews = node
        .reviews
        .nodes
        .into_iter()
        .map(|review| ReviewSubmission {
            state: review.state,
            author: review.author.map(|a| a.login).unwrap_or_default(),
        })
        .collect();
    let ci = node
        .commits
        .nodes
        .into_iter()
        .last()
        .and_then(|n| n.commit.status_check_rollup)
        .map(|rollup| match rollup.state.as_str() {
            "SUCCESS" => CiState::Passing,
            "FAILURE" | "ERROR" => CiState::Failing,
            "PENDING" | "EXPECTED" => CiState::Pending,
            _ => CiState::None,
        })
        .unwrap_or(CiState::None);
    PrDetail {
        threads,
        reviews,
        ci,
    }
}

/// A PR from phase 1: identity + scalar fields, before detail is fetched.
struct PrRef {
    id: String,
    number: u64,
    title: String,
    url: String,
    author: String,
    is_draft: bool,
    base_ref: String,
    head_ref: String,
    repo_key: String,
}

/// Per-PR detail from phase 2.
#[derive(Default)]
struct PrDetail {
    threads: Vec<ReviewThread>,
    reviews: Vec<ReviewSubmission>,
    ci: CiState,
}

/// Look up (and process-cache) the current GitHub user's login. Stable for the
/// process, so a set-once [`OnceLock`] is the right primitive across worker
/// threads.
pub fn viewer_login() -> Option<String> {
    static VIEWER_LOGIN: OnceLock<String> = OnceLock::new();
    if let Some(login) = VIEWER_LOGIN.get() {
        return Some(login.clone());
    }
    let login = run_gh_viewer_login()?;
    Some(VIEWER_LOGIN.get_or_init(|| login).clone())
}

fn run_gh_viewer_login() -> Option<String> {
    let stdout = run_gh_capped(
        None,
        &["api", "graphql", "-f", "query=query{viewer{login}}", "-q", ".data.viewer.login"],
        PR_FETCH_TIMEOUT,
    )
    .ok()?;
    let login = String::from_utf8_lossy(&stdout).trim().to_string();
    (!login.is_empty()).then_some(login)
}

/// Phase-1 query: cheap global searches returning only ids + scalar fields.
const REF_QUERY: &str = "\
query($mineQuery: String!, $requestedQuery: String!, $reviewedQuery: String!) {
  viewer { login }
  mine: search(type: ISSUE, first: 50, query: $mineQuery) { ...prRef }
  requested: search(type: ISSUE, first: 50, query: $requestedQuery) { ...prRef }
  reviewedBy: search(type: ISSUE, first: 50, query: $reviewedQuery) { ...prRef }
}
fragment prRef on SearchResultItemConnection {
  nodes {
    ... on PullRequest {
      id
      number
      title
      url
      isDraft
      baseRefName
      headRefName
      author { login }
      repository { nameWithOwner }
    }
  }
}";

/// Phase-2 detail fields, fetched per PR by node id (the expensive part).
const DETAIL_FIELDS: &str = "reviews(last: 50) { nodes { state author { login } } } \
reviewThreads(first: 50) { nodes { isResolved comments(last: 1) { nodes { author { login } } } } } \
commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }";

/// Turn phase-1 search results into `PrRef`s, keeping only PRs in a scanned
/// ~/workspace repo (case-insensitive owner/name match) and deduping the PRs
/// that appear in more than one search.
fn select_local_refs(data: RefData, repo_by_owner_name: &HashMap<String, String>) -> Vec<PrRef> {
    let mut refs = Vec::new();
    let mut seen: HashSet<PrKey> = HashSet::new();
    for node in data
        .mine
        .nodes
        .into_iter()
        .chain(data.requested.nodes)
        .chain(data.reviewed_by.nodes)
    {
        if node.number == 0 || node.id.is_empty() {
            continue; // empty (non-PR) node
        }
        let Some(repo_key) = repo_by_owner_name
            .get(&node.repository.name_with_owner.to_lowercase())
            .cloned()
        else {
            continue; // not a ~/workspace repo
        };
        if !seen.insert((repo_key.clone(), node.number)) {
            continue; // already taken from another search
        }
        refs.push(PrRef {
            id: node.id,
            number: node.number,
            title: node.title,
            url: node.url,
            author: node.author.map(|a| a.login).unwrap_or_else(|| "ghost".to_string()),
            is_draft: node.is_draft,
            base_ref: node.base_ref_name,
            head_ref: node.head_ref_name,
            repo_key,
        });
    }
    refs
}

// --- Raw GraphQL response shapes -------------------------------------------

#[derive(Deserialize)]
struct GraphqlEnvelope<T> {
    data: Option<T>,
    errors: Option<Vec<GraphqlError>>,
}

#[derive(Deserialize)]
struct GraphqlError {
    message: String,
}

/// Phase-1 response: viewer + the three aliased searches of PR refs.
#[derive(Deserialize)]
struct RefData {
    viewer: RawViewer,
    #[serde(default)]
    mine: RawRefSearch,
    #[serde(default)]
    requested: RawRefSearch,
    #[serde(rename = "reviewedBy", default)]
    reviewed_by: RawRefSearch,
}

#[derive(Deserialize)]
struct RawViewer {
    login: String,
}

#[derive(Deserialize, Default)]
struct RawRefSearch {
    #[serde(default)]
    nodes: Vec<RawRefNode>,
}

/// One phase-1 search node (ids + scalars). `#[serde(default)]` tolerates the
/// empty `{}` a non-PR result yields (filtered out later by `number == 0`).
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct RawRefNode {
    id: String,
    number: u64,
    title: String,
    url: String,
    is_draft: bool,
    base_ref_name: String,
    head_ref_name: String,
    author: Option<RawLogin>,
    repository: RawRepo,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct RawRepo {
    name_with_owner: String,
}

/// Phase-2 response: detail nodes in the same order as the requested ids
/// (`null` for any id that wasn't a PR).
#[derive(Deserialize)]
struct DetailData {
    #[serde(default)]
    nodes: Vec<Option<RawDetailNode>>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct RawDetailNode {
    reviews: RawReviews,
    review_threads: RawThreads,
    commits: RawCommits,
}

#[derive(Deserialize, Default)]
struct RawCommits {
    #[serde(default)]
    nodes: Vec<RawCommitNode>,
}

#[derive(Deserialize, Default)]
struct RawCommitNode {
    #[serde(default)]
    commit: RawCommit,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct RawCommit {
    status_check_rollup: Option<RawRollup>,
}

#[derive(Deserialize, Default)]
struct RawRollup {
    #[serde(default)]
    state: String,
}

#[derive(Deserialize, Default)]
struct RawLogin {
    #[serde(default)]
    login: String,
}

#[derive(Deserialize, Default)]
struct RawReviews {
    #[serde(default)]
    nodes: Vec<RawReview>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct RawReview {
    state: String,
    author: Option<RawLogin>,
}

#[derive(Deserialize, Default)]
struct RawThreads {
    #[serde(default)]
    nodes: Vec<RawThread>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct RawThread {
    is_resolved: bool,
    comments: RawComments,
}

#[derive(Deserialize, Default)]
struct RawComments {
    #[serde(default)]
    nodes: Vec<RawComment>,
}

#[derive(Deserialize, Default)]
struct RawComment {
    #[serde(default)]
    author: Option<RawLogin>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(number: u64, author: &str, base: &str, head: &str) -> FetchedPr {
        FetchedPr {
            number,
            title: format!("pr {number}"),
            url: format!("https://github.com/acme/proj/pull/{number}"),
            author: author.to_string(),
            is_draft: false,
            base_ref: base.to_string(),
            head_ref: head.to_string(),
            repo_key: "acme/proj".to_string(),
            threads: Vec::new(),
            reviews: Vec::new(),
            ci: CiState::None,
        }
    }

    fn thread(resolved: bool, last: Option<&str>) -> ReviewThread {
        ReviewThread {
            is_resolved: resolved,
            last_comment_author: last.map(str::to_string),
        }
    }

    fn review(state: &str, author: &str) -> ReviewSubmission {
        ReviewSubmission {
            state: state.to_string(),
            author: author.to_string(),
        }
    }

    #[test]
    fn red_when_a_thread_awaits_my_reply() {
        let mut p = pr(1, "alice", "main", "alice/x");
        p.threads = vec![thread(false, Some("alice"))]; // they replied last
        assert_eq!(classify_pr(&p, "me"), PrBucket::Red);
    }

    #[test]
    fn others_unreviewed_pr_is_red() {
        let p = pr(1, "alice", "main", "alice/x"); // no threads
        assert_eq!(classify_pr(&p, "me"), PrBucket::Red);
    }

    #[test]
    fn my_unreviewed_pr_is_grey() {
        let p = pr(1, "me", "main", "me/x"); // no threads, not approved
        assert_eq!(classify_pr(&p, "me"), PrBucket::Grey);
    }

    #[test]
    fn grey_when_all_threads_me_last() {
        let mut p = pr(1, "alice", "main", "alice/x");
        p.threads = vec![thread(false, Some("me")), thread(false, Some("me"))];
        assert_eq!(classify_pr(&p, "me"), PrBucket::Grey);
    }

    #[test]
    fn resolved_thread_does_not_make_it_red() {
        let mut p = pr(1, "alice", "main", "alice/x");
        p.threads = vec![thread(true, Some("alice"))]; // resolved -> ignored
        assert_eq!(classify_pr(&p, "me"), PrBucket::Grey);
    }

    #[test]
    fn green_others_pr_only_when_i_approved() {
        let mut p = pr(1, "alice", "main", "alice/x");
        p.threads = vec![thread(false, Some("me"))]; // not red
        p.reviews = vec![review("APPROVED", "bob")];
        assert_eq!(classify_pr(&p, "me"), PrBucket::Grey, "bob's approval isn't mine");
        p.reviews = vec![review("APPROVED", "me")];
        assert_eq!(classify_pr(&p, "me"), PrBucket::Green);
    }

    #[test]
    fn green_my_pr_when_anyone_approved() {
        let mut p = pr(1, "me", "main", "me/x");
        p.threads = vec![thread(false, Some("me"))];
        p.reviews = vec![review("APPROVED", "alice")];
        assert_eq!(classify_pr(&p, "me"), PrBucket::Green);
    }

    #[test]
    fn red_beats_green() {
        let mut p = pr(1, "alice", "main", "alice/x");
        p.threads = vec![thread(false, Some("alice"))]; // awaits me -> red
        p.reviews = vec![review("APPROVED", "me")]; // also approved
        assert_eq!(classify_pr(&p, "me"), PrBucket::Red);
    }

    #[test]
    fn later_changes_requested_unapproves() {
        let mut p = pr(1, "me", "main", "me/x");
        p.threads = vec![thread(false, Some("me"))];
        p.reviews = vec![review("APPROVED", "alice"), review("CHANGES_REQUESTED", "alice")];
        assert_eq!(classify_pr(&p, "me"), PrBucket::Grey);
    }

    #[test]
    fn stack_visible_rows_keep_descendant_under_hidden_parent() {
        // A (red) -> B (green) -> C (red); hide B, C must stay under A.
        let prs = vec![
            pr(1, "alice", "main", "a"),
            pr(2, "alice", "a", "b"),
            pr(3, "alice", "b", "c"),
        ];
        let graph = StackGraph::build(&prs);
        let visible_keys: HashSet<PrKey> =
            [("acme/proj".to_string(), 1), ("acme/proj".to_string(), 3)]
                .into_iter()
                .collect();
        let rows = graph.visible_forest(&|key| visible_keys.contains(key));
        let view: Vec<(u64, &str)> = rows.iter().map(|r| (r.key.1, r.prefix.as_str())).collect();
        // #2 hidden, so #3 attaches directly under #1 (the root).
        assert_eq!(view, vec![(1, ""), (3, "└ ")]);
    }

    #[test]
    fn stack_visible_forest_draws_box_connectors() {
        let prs = vec![
            pr(1, "alice", "main", "a"),
            pr(2, "alice", "a", "b"),
            pr(3, "alice", "b", "c"),
        ];
        let graph = StackGraph::build(&prs);
        let rows = graph.visible_forest(&|_| true);
        let view: Vec<(u64, &str)> = rows.iter().map(|r| (r.key.1, r.prefix.as_str())).collect();
        // Linear A->B->C: root flush, B is A's only child (└─┬), C under B.
        assert_eq!(view, vec![(1, ""), (2, "└─┬ "), (3, "  └ ")]);
    }

    #[test]
    fn aggregate_puts_me_first_then_most_red() {
        let mut mine = pr(1, "me", "main", "me/x");
        mine.threads = vec![thread(false, Some("me"))]; // grey
        let mut alice = pr(2, "alice", "main", "alice/x"); // unreviewed -> red
        let _ = &mut alice;
        let bob1 = pr(3, "bob", "main", "bob/x"); // red
        let bob2 = pr(4, "bob", "main", "bob/y"); // red
        let people = aggregate_people(&[mine, alice, bob1, bob2], "me");
        assert_eq!(people[0].login, "me");
        assert!(people[0].is_me);
        assert_eq!(people[1].login, "bob", "bob has more red than alice");
        assert_eq!(people[1].red, 2);
        assert_eq!(people[2].login, "alice");
    }

    #[test]
    fn parse_github_owner_name_handles_url_shapes() {
        assert_eq!(
            parse_github_owner_name("git@github.com:acme/proj.git").as_deref(),
            Some("acme/proj")
        );
        assert_eq!(
            parse_github_owner_name("https://github.com/acme/proj").as_deref(),
            Some("acme/proj")
        );
        assert_eq!(parse_github_owner_name("git@gitlab.com:acme/proj.git"), None);
    }

    #[test]
    fn select_local_refs_filters_to_local_repos_unions_and_dedups() {
        // Phase 1: PR #1 (acme/proj) is local; #2 appears in two searches; #9 is
        // in a repo NOT in ~/workspace and must be dropped.
        let raw = br#"{"data":{
            "viewer":{"login":"me"},
            "mine":{"nodes":[{"id":"PR_1","number":1,"title":"mine","url":"u","isDraft":false,
                "baseRefName":"main","headRefName":"me/x","author":{"login":"me"},
                "repository":{"nameWithOwner":"acme/proj"}}]},
            "requested":{"nodes":[
                {"id":"PR_2","number":2,"title":"rev","url":"u","isDraft":false,
                 "baseRefName":"main","headRefName":"a/x","author":{"login":"alice"},
                 "repository":{"nameWithOwner":"Acme/Proj"}},
                {"id":"PR_9","number":9,"title":"foreign","url":"u","isDraft":false,
                 "baseRefName":"main","headRefName":"x","author":{"login":"bob"},
                 "repository":{"nameWithOwner":"other/repo"}}]},
            "reviewedBy":{"nodes":[{"id":"PR_2","number":2,"title":"rev","url":"u","isDraft":false,
                "baseRefName":"main","headRefName":"a/x","author":{"login":"alice"},
                "repository":{"nameWithOwner":"acme/proj"}}]}
        }}"#;
        let data: RefData = extract_data(raw).unwrap();
        assert_eq!(data.viewer.login, "me");
        let map: HashMap<String, String> =
            [("acme/proj".to_string(), "acme/proj".to_string())].into_iter().collect();
        let refs = select_local_refs(data, &map);
        // #1 and #2 kept (case-insensitive owner match), #2 deduped, #9 dropped.
        assert_eq!(refs.len(), 2, "local PRs only, deduped; foreign repo dropped");
        let pr2 = refs.iter().find(|r| r.number == 2).unwrap();
        assert_eq!(pr2.repo_key, "acme/proj");
        assert_eq!(pr2.id, "PR_2");
    }

    #[test]
    fn detail_from_raw_extracts_threads_reviews_and_ci() {
        // Phase 2: per-PR detail parsing (last-replier, review state, CI rollup).
        let node: RawDetailNode = serde_json::from_str(
            r#"{
                "reviews":{"nodes":[{"state":"APPROVED","author":{"login":"alice"}}]},
                "reviewThreads":{"nodes":[{"isResolved":false,"comments":{"nodes":[{"author":{"login":"bob"}}]}}]},
                "commits":{"nodes":[{"commit":{"statusCheckRollup":{"state":"FAILURE"}}}]}
            }"#,
        )
        .unwrap();
        let detail = detail_from_raw(node);
        assert_eq!(detail.reviews.len(), 1);
        assert_eq!(detail.threads.len(), 1);
        assert_eq!(detail.threads[0].last_comment_author.as_deref(), Some("bob"));
        assert_eq!(detail.ci, CiState::Failing);
    }
}
