//! Personal pull-request inbox: PRs the user is involved in, fetched from `gh`.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestSummary {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: String,
    pub is_draft: bool,
    pub repo: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PullRequestInboxStatus {
    Ok,
    Loading,
    GhNotInstalled,
    GhNotAuthed,
    Error { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestInbox {
    pub prs: Vec<PullRequestSummary>,
    #[serde(flatten)]
    pub status: PullRequestInboxStatus,
}

impl Default for PullRequestInbox {
    fn default() -> Self {
        Self {
            prs: Vec::new(),
            status: PullRequestInboxStatus::Loading,
        }
    }
}

#[derive(Deserialize)]
struct RawRepo {
    #[serde(rename = "nameWithOwner")]
    name_with_owner: Option<String>,
    name: Option<String>,
}

#[derive(Deserialize)]
struct RawPr {
    number: u64,
    title: String,
    url: String,
    state: String,
    #[serde(rename = "isDraft")]
    is_draft: bool,
    repository: RawRepo,
    #[serde(rename = "updatedAt")]
    updated_at: String,
}

/// Parse stdout of `gh search prs --json number,title,url,state,isDraft,repository,updatedAt`.
pub fn parse_gh_search_prs(stdout: &str) -> Result<Vec<PullRequestSummary>, serde_json::Error> {
    let raw: Vec<RawPr> = serde_json::from_str(stdout)?;
    Ok(raw
        .into_iter()
        .map(|r| PullRequestSummary {
            number: r.number,
            title: r.title,
            url: r.url,
            state: r.state,
            is_draft: r.is_draft,
            repo: r
                .repository
                .name_with_owner
                .or(r.repository.name)
                .unwrap_or_default(),
            updated_at: r.updated_at,
        })
        .collect())
}

/// Run `gh` to fetch the open PRs the user is involved in. Maps every failure
/// mode onto a status the UI can render; never panics, never returns an error.
/// Classify a failed `gh` invocation's stderr into an inbox status.
///
/// Auth-related failures map to `GhNotAuthed`; anything else is a generic
/// `Error` carrying the trimmed stderr.
fn classify_gh_failure(stderr: &str) -> PullRequestInboxStatus {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("auth") || lower.contains("logged in") || lower.contains("gh auth login") {
        PullRequestInboxStatus::GhNotAuthed
    } else {
        PullRequestInboxStatus::Error {
            message: stderr.trim().to_string(),
        }
    }
}

pub fn fetch_pr_inbox() -> PullRequestInbox {
    use std::process::Command;
    let output = match Command::new("gh")
        .args([
            "search",
            "prs",
            "--involves=@me",
            "--state=open",
            "--limit=30",
            "--json",
            "number,title,url,state,isDraft,repository,updatedAt",
        ])
        .output()
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return PullRequestInbox {
                prs: Vec::new(),
                status: PullRequestInboxStatus::GhNotInstalled,
            }
        }
        Err(err) => {
            tracing::warn!(error = %err, "failed to spawn gh for pr inbox");
            return PullRequestInbox {
                prs: Vec::new(),
                status: PullRequestInboxStatus::Error {
                    message: err.to_string(),
                },
            };
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return PullRequestInbox {
            prs: Vec::new(),
            status: classify_gh_failure(&stderr),
        };
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    match parse_gh_search_prs(&stdout) {
        Ok(prs) => PullRequestInbox {
            prs,
            status: PullRequestInboxStatus::Ok,
        },
        Err(err) => {
            tracing::warn!(error = %err, "failed to parse gh pr inbox output");
            PullRequestInbox {
                prs: Vec::new(),
                status: PullRequestInboxStatus::Error {
                    message: err.to_string(),
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_JSON: &str = r#"[{"isDraft":false,"number":14655,"repository":{"name":"sequencer","nameWithOwner":"starkware-libs/sequencer"},"state":"open","title":"add native override layer","updatedAt":"2026-06-28T14:06:20Z","url":"https://github.com/starkware-libs/sequencer/pull/14655"},{"isDraft":true,"number":14649,"repository":{"name":"sequencer","nameWithOwner":"starkware-libs/sequencer"},"state":"open","title":"delete base app_configs","updatedAt":"2026-06-29T11:26:10Z","url":"https://github.com/starkware-libs/sequencer/pull/14649"}]"#;

    #[test]
    fn parses_gh_search_prs() {
        let prs = parse_gh_search_prs(SAMPLE_JSON).expect("should parse");
        assert_eq!(prs.len(), 2);
        assert_eq!(prs[0].number, 14655);
        assert_eq!(prs[0].repo, "starkware-libs/sequencer");
        assert!(!prs[0].is_draft);
        assert!(prs[1].is_draft);
        assert_eq!(prs[0].title, "add native override layer");
    }

    #[test]
    fn parses_empty_list() {
        let prs = parse_gh_search_prs("[]").expect("should parse empty");
        assert!(prs.is_empty());
    }

    #[test]
    fn classify_gh_failure_detects_auth() {
        assert_eq!(
            classify_gh_failure("To get started with GitHub CLI, please run: gh auth login"),
            PullRequestInboxStatus::GhNotAuthed
        );
        assert_eq!(
            classify_gh_failure("You are not logged in to any GitHub hosts."),
            PullRequestInboxStatus::GhNotAuthed
        );
    }

    #[test]
    fn classify_gh_failure_generic_error() {
        assert_eq!(
            classify_gh_failure("  some unexpected failure\n"),
            PullRequestInboxStatus::Error {
                message: "some unexpected failure".to_string(),
            }
        );
    }
}
