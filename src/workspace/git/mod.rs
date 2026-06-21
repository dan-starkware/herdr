mod config;
#[cfg(test)]
mod config_tests;
mod discovery;
mod pr_status;
mod prs;
mod repos;
mod status;
#[cfg(test)]
mod test_support;

pub use self::{
    discovery::{derive_label_from_cwd, git_branch, git_space_metadata, GitSpaceMetadata},
    pr_status::{
        fetch_pr_status_snapshot, github_owner_name, CiState, FetchedPr, PersonPr, PersonPrs,
        PrBucket, PrKey, PrStatusSnapshot, StackGraph, StackRow,
    },
    prs::{list_prs_for_my_review, pr_by_number, pr_number_for_ref, ReviewPr},
    repos::{
        default_scan_root, list_review_branches, review_base, scan_repositories, Branch,
        Repository,
    },
    status::{git_status_cache_key, git_status_snapshot_for_cwd, GitStatusCacheEntry},
};

#[cfg(test)]
pub(super) use self::status::git_ahead_behind;
