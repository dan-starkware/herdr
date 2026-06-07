mod config;
#[cfg(test)]
mod config_tests;
mod discovery;
mod repos;
mod status;
#[cfg(test)]
mod test_support;

pub use self::{
    discovery::{derive_label_from_cwd, git_branch, git_space_metadata, GitSpaceMetadata},
    repos::{
        default_branch, default_scan_root, graphite_parent, list_branches, review_base,
        scan_repositories, Branch, Repository,
    },
    status::{git_status_cache_key, git_status_snapshot_for_cwd, GitStatusCacheEntry},
};

#[cfg(test)]
pub(super) use self::status::git_ahead_behind;
