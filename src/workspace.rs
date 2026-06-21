use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::layout::Direction;
use tokio::sync::{mpsc, Notify};

use crate::events::AppEvent;
use crate::layout::PaneId;
#[cfg(test)]
use crate::layout::TileLayout;
use crate::pane::{PaneRole, PaneState};
use crate::terminal::{TerminalId, TerminalRuntime, TerminalRuntimeRegistry, TerminalState};

mod aggregate;
mod git;
mod tab;

#[cfg(test)]
use self::git::git_ahead_behind;
pub use self::{
    git::{
        default_scan_root, derive_label_from_cwd, fetch_pr_status_snapshot, git_branch,
        git_space_metadata, git_status_cache_key, github_owner_name, list_prs_for_my_review,
        list_review_branches, pr_by_number, pr_number_for_ref, review_base, scan_repositories,
        Branch, CiState, FetchedPr, GitSpaceMetadata, GitStatusCacheEntry, PersonPr, PersonPrs,
        PrBucket, PrKey, PrStatusSnapshot, Repository, ReviewPr, StackGraph, StackRow,
    },
    tab::Tab,
};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorktreeSpaceMembership {
    pub key: String,
    pub label: String,
    pub repo_root: PathBuf,
    pub checkout_path: PathBuf,
    pub is_linked_worktree: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceGitStatus {
    pub workspace_id: String,
    pub resolved_identity_cwd: PathBuf,
    pub branch: Option<String>,
    pub ahead_behind: Option<(usize, usize)>,
    pub space: Option<GitSpaceMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceGitStatusSnapshot {
    pub branch: Option<String>,
    pub ahead_behind: Option<(usize, usize)>,
    pub space: Option<GitSpaceMetadata>,
}

impl WorkspaceGitStatusSnapshot {
    pub fn into_workspace_status(
        self,
        workspace_id: String,
        resolved_identity_cwd: PathBuf,
    ) -> WorkspaceGitStatus {
        WorkspaceGitStatus {
            workspace_id,
            resolved_identity_cwd,
            branch: self.branch,
            ahead_behind: self.ahead_behind,
            space: self.space,
        }
    }
}

static NEXT_WORKSPACE_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) fn generate_workspace_id() -> String {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_micros())
        .unwrap_or(0);
    let counter = NEXT_WORKSPACE_ID.fetch_add(1, Ordering::Relaxed);
    format!("w{micros:x}{counter:x}")
}

/// A named workspace containing tabs.
pub struct Workspace {
    /// Stable public workspace identity, independent of display order.
    pub id: String,
    /// User-provided override. If set, auto-derived identity stops updating.
    pub custom_name: Option<String>,
    /// Fallback workspace identity source for tests, old snapshots, or missing runtimes.
    pub identity_cwd: PathBuf,
    /// Cached current git branch for the workspace repo.
    pub(crate) cached_git_branch: Option<String>,
    /// Cached ahead/behind counts for the workspace repo's current branch upstream.
    pub(crate) cached_git_ahead_behind: Option<(usize, usize)>,
    /// Cached derived Git repo metadata for worktree actions and status display.
    pub(crate) cached_git_space: Option<GitSpaceMetadata>,
    /// Explicit Herdr-managed worktree grouping provenance.
    pub worktree_space: Option<WorktreeSpaceMembership>,
    /// The PR opened for review in this workspace (from the branch picker's
    /// review-requests list). Only honoured while the checked-out branch still
    /// matches the PR head; see [`Self::reviewing_pr_active`].
    pub reviewing_pr: Option<ReviewPr>,
    /// Stable-ish public pane numbers within this workspace.
    /// New panes append at the end; closing a pane compacts higher numbers down.
    pub public_pane_numbers: HashMap<PaneId, usize>,
    pub(crate) next_public_pane_number: usize,
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    /// Transient: the kept-alive review-row terminal when that row is detached
    /// (toggled closed). Re-attaching reuses this terminal. Not serialized.
    pub(crate) detached_review: Option<TerminalId>,
    /// Transient: the kept-alive terminal-row terminal when that row is detached
    /// (toggled closed). Re-attaching reuses this terminal. Not serialized.
    pub(crate) detached_terminal: Option<TerminalId>,
    #[cfg(test)]
    pub(crate) test_runtimes: HashMap<PaneId, TerminalRuntime>,
}

impl Deref for Workspace {
    type Target = Tab;

    fn deref(&self) -> &Self::Target {
        self.active_tab()
            .expect("workspace must always have at least one active tab")
    }
}

impl DerefMut for Workspace {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.active_tab_mut()
            .expect("workspace must always have at least one active tab")
    }
}

impl Workspace {
    pub fn new(
        initial_cwd: PathBuf,
        rows: u16,
        cols: u16,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        shell_config: crate::pane::PaneShellConfig<'_>,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<AtomicBool>,
    ) -> std::io::Result<(Self, TerminalState, TerminalRuntime)> {
        Self::new_with_tab(
            initial_cwd,
            rows,
            cols,
            scrollback_limit_bytes,
            host_terminal_theme,
            shell_config,
            events,
            render_notify,
            render_dirty,
            None,
        )
    }

    pub fn new_argv_command(
        initial_cwd: PathBuf,
        rows: u16,
        cols: u16,
        argv: &[String],
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<AtomicBool>,
    ) -> std::io::Result<(Self, TerminalState, TerminalRuntime)> {
        Self::new_with_tab(
            initial_cwd,
            rows,
            cols,
            scrollback_limit_bytes,
            host_terminal_theme,
            crate::pane::PaneShellConfig::new("", crate::config::ShellModeConfig::NonLogin),
            events,
            render_notify,
            render_dirty,
            Some(argv),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_with_tab(
        initial_cwd: PathBuf,
        rows: u16,
        cols: u16,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        shell_config: crate::pane::PaneShellConfig<'_>,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<AtomicBool>,
        argv: Option<&[String]>,
    ) -> std::io::Result<(Self, TerminalState, TerminalRuntime)> {
        let (tab, terminal, runtime) = if let Some(argv) = argv {
            Tab::new_argv_command(
                1,
                initial_cwd.clone(),
                rows,
                cols,
                argv,
                scrollback_limit_bytes,
                host_terminal_theme,
                events,
                render_notify,
                render_dirty,
            )?
        } else {
            Tab::new(
                1,
                initial_cwd.clone(),
                rows,
                cols,
                scrollback_limit_bytes,
                host_terminal_theme,
                shell_config,
                events,
                render_notify,
                render_dirty,
            )?
        };
        let mut public_pane_numbers = HashMap::new();
        public_pane_numbers.insert(tab.root_pane, 1);
        Ok((
            Self {
                id: generate_workspace_id(),
                custom_name: None,
                identity_cwd: initial_cwd.clone(),
                cached_git_branch: git_branch(&initial_cwd),
                cached_git_ahead_behind: None,
                cached_git_space: None,
                worktree_space: None,
                reviewing_pr: None,
                public_pane_numbers,
                next_public_pane_number: 2,
                tabs: vec![tab],
                active_tab: 0,
                detached_review: None,
                detached_terminal: None,
                #[cfg(test)]
                test_runtimes: HashMap::new(),
            },
            terminal,
            runtime,
        ))
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active_tab)
    }

    pub fn active_tab_index(&self) -> usize {
        self.active_tab
    }

    pub fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active_tab)
    }

    pub fn active_tab_display_name(&self) -> Option<String> {
        self.active_tab().map(Tab::display_name)
    }

    pub fn switch_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.active_tab = idx;
            if let Some(tab) = self.tabs.get_mut(idx) {
                for pane in tab.panes.values_mut() {
                    pane.seen = true;
                }
            }
        }
    }

    pub fn create_tab(
        &mut self,
        rows: u16,
        cols: u16,
        cwd: PathBuf,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        shell_config: crate::pane::PaneShellConfig<'_>,
    ) -> std::io::Result<(usize, TerminalState, TerminalRuntime)> {
        self.create_tab_with_runtime(
            rows,
            cols,
            cwd,
            scrollback_limit_bytes,
            host_terminal_theme,
            shell_config,
            None,
        )
    }

    fn create_tab_with_runtime(
        &mut self,
        rows: u16,
        cols: u16,
        cwd: PathBuf,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        shell_config: crate::pane::PaneShellConfig<'_>,
        argv: Option<&[String]>,
    ) -> std::io::Result<(usize, TerminalState, TerminalRuntime)> {
        let number = self.tabs.len() + 1;
        let events = self
            .active_tab()
            .map(|tab| tab.events.clone())
            .expect("workspace must always have at least one tab");
        let render_notify = self
            .active_tab()
            .map(|tab| tab.render_notify.clone())
            .expect("workspace must always have at least one tab");
        let render_dirty = self
            .active_tab()
            .map(|tab| tab.render_dirty.clone())
            .expect("workspace must always have at least one tab");

        let (tab, terminal, runtime) = if let Some(argv) = argv {
            Tab::new_argv_command(
                number,
                cwd,
                rows,
                cols,
                argv,
                scrollback_limit_bytes,
                host_terminal_theme,
                events,
                render_notify,
                render_dirty,
            )?
        } else {
            Tab::new(
                number,
                cwd,
                rows,
                cols,
                scrollback_limit_bytes,
                host_terminal_theme,
                shell_config,
                events,
                render_notify,
                render_dirty,
            )?
        };
        self.register_new_pane(tab.root_pane);
        self.tabs.push(tab);
        Ok((self.tabs.len() - 1, terminal, runtime))
    }

    pub fn close_tab(&mut self, idx: usize) -> bool {
        if self.tabs.len() <= 1 || idx >= self.tabs.len() {
            return false;
        }
        let tab = self.tabs.remove(idx);
        for pane_id in tab.panes.keys() {
            self.unregister_pane(*pane_id);
        }
        self.renumber_tabs();
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        } else if idx <= self.active_tab && self.active_tab > 0 {
            self.active_tab -= 1;
        }
        true
    }

    pub fn move_tab(&mut self, source_idx: usize, insert_idx: usize) -> bool {
        if source_idx >= self.tabs.len() || insert_idx > self.tabs.len() {
            return false;
        }

        let target_idx = if source_idx < insert_idx {
            insert_idx.saturating_sub(1)
        } else {
            insert_idx
        }
        .min(self.tabs.len().saturating_sub(1));

        if source_idx == target_idx {
            return false;
        }

        let active_root_pane = self.tabs.get(self.active_tab).map(|tab| tab.root_pane);
        let tab = self.tabs.remove(source_idx);
        self.tabs.insert(target_idx, tab);
        self.renumber_tabs();
        self.active_tab = active_root_pane
            .and_then(|root_pane| self.tabs.iter().position(|tab| tab.root_pane == root_pane))
            .unwrap_or(target_idx);
        true
    }

    pub fn close_active_tab(&mut self) -> bool {
        self.close_tab(self.active_tab)
    }

    pub fn split_focused(
        &mut self,
        direction: Direction,
        rows: u16,
        cols: u16,
        cwd: Option<PathBuf>,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        shell_config: crate::pane::PaneShellConfig<'_>,
    ) -> std::io::Result<crate::workspace::tab::NewPane> {
        let new_pane = self
            .active_tab_mut()
            .expect("workspace must always have at least one tab")
            .split_focused(
                direction,
                rows,
                cols,
                cwd,
                scrollback_limit_bytes,
                host_terminal_theme,
                shell_config,
            )?;
        self.register_new_pane(new_pane.pane_id);
        Ok(new_pane)
    }

    pub fn split_pane(
        &mut self,
        pane_id: PaneId,
        direction: Direction,
        rows: u16,
        cols: u16,
        cwd: Option<PathBuf>,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        shell_config: crate::pane::PaneShellConfig<'_>,
        focus_new_pane: bool,
    ) -> Option<std::io::Result<(usize, crate::workspace::tab::NewPane)>> {
        self.split_pane_with_runtime(
            pane_id,
            direction,
            None,
            rows,
            cols,
            cwd,
            scrollback_limit_bytes,
            host_terminal_theme,
            shell_config,
            focus_new_pane,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn split_pane_with_ratio(
        &mut self,
        pane_id: PaneId,
        direction: Direction,
        ratio: f32,
        rows: u16,
        cols: u16,
        cwd: Option<PathBuf>,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        shell_config: crate::pane::PaneShellConfig<'_>,
        focus_new_pane: bool,
    ) -> Option<std::io::Result<(usize, crate::workspace::tab::NewPane)>> {
        self.split_pane_with_runtime(
            pane_id,
            direction,
            Some(ratio),
            rows,
            cols,
            cwd,
            scrollback_limit_bytes,
            host_terminal_theme,
            shell_config,
            focus_new_pane,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn split_pane_argv_command(
        &mut self,
        pane_id: PaneId,
        direction: Direction,
        rows: u16,
        cols: u16,
        cwd: Option<PathBuf>,
        argv: &[String],
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        focus_new_pane: bool,
    ) -> Option<std::io::Result<(usize, crate::workspace::tab::NewPane)>> {
        self.split_pane_with_runtime(
            pane_id,
            direction,
            None,
            rows,
            cols,
            cwd,
            scrollback_limit_bytes,
            host_terminal_theme,
            crate::pane::PaneShellConfig::new("", crate::config::ShellModeConfig::NonLogin),
            focus_new_pane,
            Some(argv),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn split_pane_with_runtime(
        &mut self,
        pane_id: PaneId,
        direction: Direction,
        ratio: Option<f32>,
        rows: u16,
        cols: u16,
        cwd: Option<PathBuf>,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        shell_config: crate::pane::PaneShellConfig<'_>,
        focus_new_pane: bool,
        argv: Option<&[String]>,
    ) -> Option<std::io::Result<(usize, crate::workspace::tab::NewPane)>> {
        let tab_idx = self.find_tab_index_for_pane(pane_id)?;
        let tab = &mut self.tabs[tab_idx];
        let previous_focus = tab.layout.focused();
        tab.layout.focus_pane(pane_id);
        let new_pane = match if let Some(argv) = argv {
            tab.split_focused_argv_command(
                direction,
                rows,
                cols,
                cwd,
                argv,
                scrollback_limit_bytes,
                host_terminal_theme,
            )
        } else {
            match ratio {
                Some(ratio) => tab.split_focused_with_ratio(
                    direction,
                    ratio,
                    rows,
                    cols,
                    cwd,
                    scrollback_limit_bytes,
                    host_terminal_theme,
                    shell_config,
                ),
                None => tab.split_focused(
                    direction,
                    rows,
                    cols,
                    cwd,
                    scrollback_limit_bytes,
                    host_terminal_theme,
                    shell_config,
                ),
            }
        } {
            Ok(new_pane) => new_pane,
            Err(err) => {
                tab.layout.focus_pane(previous_focus);
                return Some(Err(err));
            }
        };
        if !focus_new_pane {
            tab.layout.focus_pane(previous_focus);
        }
        self.register_new_pane(new_pane.pane_id);
        Some(Ok((tab_idx, new_pane)))
    }

    /// Close the focused pane. Returns true if the workspace should close.
    pub fn close_focused(&mut self) -> bool {
        let pane_count = self
            .active_tab()
            .map(|tab| tab.layout.pane_count())
            .unwrap_or(0);
        let tab_count = self.tabs.len();
        if pane_count <= 1 {
            return tab_count <= 1 || self.close_active_tab_and_report();
        }

        if let Some((removed, _terminal_id)) = self.active_tab_mut().and_then(Tab::close_focused) {
            self.unregister_pane(removed);
        }
        false
    }

    /// Remove a specific pane from this workspace without terminating its runtime.
    /// Returns true if the workspace should close.
    pub fn remove_pane(&mut self, pane_id: PaneId) -> bool {
        let Some(tab_idx) = self.find_tab_index_for_pane(pane_id) else {
            return false;
        };
        let pane_count = self.tabs[tab_idx].layout.pane_count();
        let tab_count = self.tabs.len();
        if pane_count <= 1 {
            if tab_count <= 1 {
                return true;
            }
            self.tabs.remove(tab_idx);
            self.unregister_pane(pane_id);
            self.renumber_tabs();
            if self.active_tab >= self.tabs.len() {
                self.active_tab = self.tabs.len() - 1;
            } else if tab_idx <= self.active_tab && self.active_tab > 0 {
                self.active_tab -= 1;
            }
            return false;
        }

        if let Some((removed, _terminal_id)) = self.tabs[tab_idx].remove_pane(pane_id) {
            self.unregister_pane(removed);
        }
        false
    }

    pub fn public_pane_number(&self, pane_id: PaneId) -> Option<usize> {
        self.public_pane_numbers.get(&pane_id).copied()
    }

    pub fn set_custom_name(&mut self, name: String) {
        self.custom_name = Some(name);
    }

    pub fn resolved_identity_cwd(&self) -> Option<PathBuf> {
        Some(self.identity_cwd.clone())
    }

    pub fn resolved_identity_cwd_from(
        &self,
        terminals: &HashMap<TerminalId, TerminalState>,
        terminal_runtimes: &TerminalRuntimeRegistry,
    ) -> Option<PathBuf> {
        self.tabs
            .first()
            .and_then(|tab| tab.cwd_for_pane(tab.root_pane, terminals, terminal_runtimes))
            .or_else(|| Some(self.identity_cwd.clone()))
    }

    pub fn display_name(&self) -> String {
        if let Some(name) = &self.custom_name {
            return name.clone();
        }

        self.resolved_identity_cwd()
            .map(|cwd| derive_label_from_cwd(&cwd))
            .unwrap_or_else(|| "workspace".into())
    }

    pub fn display_name_from(
        &self,
        terminals: &HashMap<TerminalId, TerminalState>,
        terminal_runtimes: &TerminalRuntimeRegistry,
    ) -> String {
        if let Some(name) = &self.custom_name {
            return name.clone();
        }

        self.resolved_identity_cwd_from(terminals, terminal_runtimes)
            .map(|cwd| derive_label_from_cwd(&cwd))
            .unwrap_or_else(|| "workspace".into())
    }

    pub fn branch(&self) -> Option<String> {
        self.cached_git_branch.clone()
    }

    /// The PR under review in this workspace, while the checked-out branch
    /// still matches the PR's head. Switching to another branch makes the
    /// stored PR dormant (and active again if the head is checked back out).
    pub fn reviewing_pr_active(&self) -> Option<&ReviewPr> {
        let pr = self.reviewing_pr.as_ref()?;
        (self.cached_git_branch.as_deref() == Some(pr.head_branch.as_str())).then_some(pr)
    }

    pub fn git_ahead_behind(&self) -> Option<(usize, usize)> {
        self.cached_git_ahead_behind
    }

    pub fn git_space(&self) -> Option<&GitSpaceMetadata> {
        self.cached_git_space.as_ref()
    }

    pub fn worktree_space(&self) -> Option<&WorktreeSpaceMembership> {
        self.worktree_space.as_ref()
    }

    #[cfg(test)]
    pub fn refresh_git_ahead_behind(&mut self) {
        let cwd = self.resolved_identity_cwd();
        self.cached_git_branch = cwd.as_deref().and_then(git_branch);
        self.cached_git_ahead_behind = cwd.as_deref().and_then(git_ahead_behind);
        self.cached_git_space = cwd.as_deref().and_then(git_space_metadata);
    }

    pub fn git_status_snapshot_for_cwd_with_cache(
        resolved_identity_cwd: &std::path::Path,
        cached: Option<&GitStatusCacheEntry>,
    ) -> (WorkspaceGitStatusSnapshot, Option<GitStatusCacheEntry>) {
        self::git::git_status_snapshot_for_cwd(resolved_identity_cwd, cached)
    }

    pub fn find_tab_index_for_pane(&self, pane_id: PaneId) -> Option<usize> {
        self.tabs
            .iter()
            .position(|tab| tab.panes.contains_key(&pane_id))
    }

    pub fn pane_state(&self, pane_id: PaneId) -> Option<&PaneState> {
        self.tabs.iter().find_map(|tab| tab.panes.get(&pane_id))
    }

    pub fn terminal_id(&self, pane_id: PaneId) -> Option<&TerminalId> {
        self.tabs.iter().find_map(|tab| tab.terminal_id(pane_id))
    }

    pub fn focused_pane_id(&self) -> Option<PaneId> {
        self.active_tab().map(|tab| tab.layout.focused())
    }

    /// The pane in the active tab currently carrying `role`, if any.
    pub fn pane_with_role(&self, role: PaneRole) -> Option<PaneId> {
        let tab = self.active_tab()?;
        tab.layout
            .pane_ids()
            .into_iter()
            .find(|id| tab.panes.get(id).map(|p| p.role) == Some(role))
    }

    /// The agent (root) pane of the active tab — always the bottom row.
    pub fn agent_pane(&self) -> Option<PaneId> {
        self.active_tab().map(|tab| tab.root_pane)
    }

    /// The pane (in any tab) attached to `tid`, if any.
    pub fn pane_for_terminal(&self, tid: &TerminalId) -> Option<PaneId> {
        self.tabs.iter().find_map(|tab| {
            tab.panes
                .iter()
                .find(|(_, pane)| &pane.attached_terminal_id == tid)
                .map(|(id, _)| *id)
        })
    }

    /// Re-attach an existing (kept-alive) terminal as a new row stacked on top
    /// of `target` within `target`'s tab, tagged with `role`. Returns the new
    /// pane id, or `None` if `target` is not found.
    pub fn reattach_row(
        &mut self,
        target: PaneId,
        terminal_id: TerminalId,
        role: PaneRole,
    ) -> Option<PaneId> {
        let tab_idx = self.find_tab_index_for_pane(target)?;
        let new = self.tabs[tab_idx].split_attach_above(target, terminal_id, role);
        self.register_new_pane(new);
        Some(new)
    }

    pub fn close_pane(&mut self, pane_id: PaneId) -> bool {
        let tab_idx = match self.find_tab_index_for_pane(pane_id) {
            Some(idx) => idx,
            None => return false,
        };
        let pane_count = self.tabs[tab_idx].layout.pane_count();
        let tab_count = self.tabs.len();
        if pane_count <= 1 {
            if tab_count <= 1 {
                return true;
            }
            self.tabs.remove(tab_idx);
            self.unregister_pane(pane_id);
            self.renumber_tabs();
            if self.active_tab >= self.tabs.len() {
                self.active_tab = self.tabs.len() - 1;
            } else if tab_idx <= self.active_tab && self.active_tab > 0 {
                self.active_tab -= 1;
            }
            return false;
        }

        if let Some((removed, _terminal_id)) = self.tabs[tab_idx].close_pane(pane_id) {
            self.unregister_pane(removed);
        }
        false
    }

    fn register_new_pane(&mut self, pane_id: PaneId) {
        self.public_pane_numbers
            .insert(pane_id, self.next_public_pane_number);
        self.next_public_pane_number += 1;
    }

    fn unregister_pane(&mut self, pane_id: PaneId) {
        if let Some(removed_number) = self.public_pane_numbers.remove(&pane_id) {
            for number in self.public_pane_numbers.values_mut() {
                if *number > removed_number {
                    *number -= 1;
                }
            }
            self.next_public_pane_number = self.public_pane_numbers.len() + 1;
        }
    }

    fn renumber_tabs(&mut self) {
        for (idx, tab) in self.tabs.iter_mut().enumerate() {
            tab.number = idx + 1;
        }
    }

    fn close_active_tab_and_report(&mut self) -> bool {
        if self.tabs.len() <= 1 {
            return true;
        }
        self.close_active_tab();
        false
    }
}

#[cfg(test)]
impl Workspace {
    pub(crate) fn test_new(name: &str) -> Self {
        let (events, _) = mpsc::channel(64);
        let render_notify = Arc::new(Notify::new());
        let render_dirty = Arc::new(AtomicBool::new(false));
        let identity_cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
        let (layout, root_id) = TileLayout::new();
        let terminal_id = TerminalId::alloc();
        let mut panes = HashMap::new();
        panes.insert(root_id, PaneState::new(terminal_id));
        let tab = Tab {
            custom_name: None,
            number: 1,
            root_pane: root_id,
            layout,
            panes,
            runtimes: HashMap::new(),
            zoomed: false,
            events,
            render_notify,
            render_dirty,
        };
        let mut public_pane_numbers = HashMap::new();
        public_pane_numbers.insert(tab.root_pane, 1);
        Self {
            id: generate_workspace_id(),
            custom_name: Some(name.to_string()),
            identity_cwd: identity_cwd.clone(),
            cached_git_branch: git_branch(&identity_cwd),
            cached_git_ahead_behind: None,
            cached_git_space: None,
            worktree_space: None,
            reviewing_pr: None,
            public_pane_numbers,
            next_public_pane_number: 2,
            tabs: vec![tab],
            active_tab: 0,
            detached_review: None,
            detached_terminal: None,
            test_runtimes: HashMap::new(),
        }
    }

    pub(crate) fn insert_test_runtime(&mut self, pane_id: PaneId, runtime: TerminalRuntime) {
        self.test_runtimes.insert(pane_id, runtime);
    }

    /// Mark this test workspace as backed by an open (linked) worktree, so it
    /// shows up as an agent. The paths point at a non-existent temp directory so
    /// any `git worktree` call made against them fails harmlessly in tests.
    pub(crate) fn attach_test_worktree(&mut self) {
        let path = std::env::temp_dir().join(format!("herdr-test-worktree-{}", self.id));
        self.worktree_space = Some(WorktreeSpaceMembership {
            key: format!("test-worktree:{}", self.id),
            label: self.display_name(),
            repo_root: path.clone(),
            checkout_path: path,
            is_linked_worktree: true,
        });
    }

    pub(crate) fn test_split(&mut self, direction: Direction) -> PaneId {
        let tab = self.active_tab_mut().expect("workspace must have tab");
        let new_id = tab.layout.split_focused(direction);
        tab.panes
            .insert(new_id, PaneState::new(TerminalId::alloc()));
        self.register_new_pane(new_id);
        new_id
    }

    pub(crate) fn test_add_tab(&mut self, name: Option<&str>) -> usize {
        let (events, _) = mpsc::channel(64);
        let render_notify = Arc::new(Notify::new());
        let render_dirty = Arc::new(AtomicBool::new(false));
        let (layout, root_id) = TileLayout::new();
        let mut panes = HashMap::new();
        panes.insert(root_id, PaneState::new(TerminalId::alloc()));
        let tab = Tab {
            custom_name: name.map(str::to_string),
            number: self.tabs.len() + 1,
            root_pane: root_id,
            layout,
            panes,
            runtimes: HashMap::new(),
            zoomed: false,
            events,
            render_notify,
            render_dirty,
        };
        self.register_new_pane(root_id);
        self.tabs.push(tab);
        self.tabs.len() - 1
    }
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;

    use super::*;

    /// Top→bottom pane ids of the active tab, ordered by rendered y.
    fn rows_top_to_bottom(ws: &Workspace) -> Vec<(PaneId, PaneRole)> {
        let tab = ws.active_tab().unwrap();
        let mut infos = tab.layout.panes(Rect::new(0, 0, 80, 30));
        infos.sort_by_key(|info| info.rect.y);
        infos
            .into_iter()
            .map(|info| (info.id, tab.panes.get(&info.id).unwrap().role))
            .collect()
    }

    #[test]
    fn reattach_review_lands_on_top_of_agent() {
        let mut ws = Workspace::test_new("test");
        let agent = ws.agent_pane().unwrap();
        let review_tid = TerminalId::alloc();

        let review = ws
            .reattach_row(agent, review_tid, PaneRole::Review)
            .expect("review pane");

        let rows = rows_top_to_bottom(&ws);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], (review, PaneRole::Review), "review on top");
        assert_eq!(rows[1], (agent, PaneRole::Agent), "agent at bottom");
        // Focus lands on the freshly attached row.
        assert_eq!(ws.focused_pane_id(), Some(review));
        assert_eq!(ws.pane_with_role(PaneRole::Review), Some(review));
    }

    #[test]
    fn reattach_terminal_then_review_orders_review_terminal_agent() {
        let mut ws = Workspace::test_new("test");
        let agent = ws.agent_pane().unwrap();

        // Terminal splits the agent (root) pane and sits above it.
        let terminal = ws
            .reattach_row(agent, TerminalId::alloc(), PaneRole::Terminal)
            .expect("terminal pane");
        // Review lands above the terminal row.
        let review_target = ws.pane_with_role(PaneRole::Terminal).unwrap();
        let review = ws
            .reattach_row(review_target, TerminalId::alloc(), PaneRole::Review)
            .expect("review pane");

        let rows = rows_top_to_bottom(&ws);
        assert_eq!(
            rows,
            vec![
                (review, PaneRole::Review),
                (terminal, PaneRole::Terminal),
                (agent, PaneRole::Agent),
            ]
        );
    }

    #[test]
    fn detached_review_keeps_terminal_for_reattach() {
        let mut ws = Workspace::test_new("test");
        let agent = ws.agent_pane().unwrap();
        let review_tid = TerminalId::alloc();
        let review = ws
            .reattach_row(agent, review_tid.clone(), PaneRole::Review)
            .unwrap();
        assert_eq!(ws.terminal_id(review).cloned(), Some(review_tid.clone()));

        // Detaching keeps the terminal id available; the pane is gone.
        let detached = ws.terminal_id(review).cloned();
        ws.remove_pane(review);
        assert!(ws.pane_with_role(PaneRole::Review).is_none());
        assert_eq!(detached, Some(review_tid.clone()));

        // Re-attaching the same terminal id restores the row on top.
        let reattached = ws
            .reattach_row(agent, review_tid.clone(), PaneRole::Review)
            .unwrap();
        assert_eq!(ws.terminal_id(reattached).cloned(), Some(review_tid));
        let rows = rows_top_to_bottom(&ws);
        assert_eq!(rows[0].1, PaneRole::Review);
        assert_eq!(rows[1].0, agent);
    }

    #[test]
    fn workspace_identity_follows_first_tab_root_pane_cwd() {
        let mut ws = Workspace::test_new("ignored");
        ws.custom_name = None;
        let root_pane = ws.tabs[0].root_pane;
        let terminal_id = ws.tabs[0].terminal_id(root_pane).unwrap().clone();
        let mut terminals = HashMap::new();
        terminals.insert(
            terminal_id.clone(),
            TerminalState::new(terminal_id, PathBuf::from("/herdr-test/pion")),
        );
        let terminal_runtimes = TerminalRuntimeRegistry::new();

        assert_eq!(ws.display_name_from(&terminals, &terminal_runtimes), "pion");
        assert_eq!(
            ws.resolved_identity_cwd_from(&terminals, &terminal_runtimes),
            Some(PathBuf::from("/herdr-test/pion"))
        );
    }

    #[test]
    fn moving_tab_keeps_active_identity_and_renumbers_auto_tabs() {
        let mut ws = Workspace::test_new("test");
        let moved_root = ws.tabs[0].root_pane;
        ws.test_add_tab(Some("foo"));
        let final_auto_idx = ws.test_add_tab(None);
        let active_root = ws.tabs[final_auto_idx].root_pane;
        ws.switch_tab(final_auto_idx);

        assert!(ws.move_tab(0, ws.tabs.len()));

        let labels: Vec<_> = ws.tabs.iter().map(|tab| tab.display_name()).collect();
        assert_eq!(labels, vec!["foo", "2", "3"]);
        assert_eq!(ws.tabs[0].custom_name.as_deref(), Some("foo"));
        assert!(ws.tabs[1].custom_name.is_none());
        assert!(ws.tabs[2].custom_name.is_none());
        assert_eq!(ws.tabs[2].root_pane, moved_root);
        assert_eq!(ws.tabs[ws.active_tab].root_pane, active_root);
    }
}
