use std::path::{Path, PathBuf};

use super::{terminal_targets::TerminalTargetError, App, Mode};
use crate::api::schema::{AgentStartParams, SplitDirection};

/// Command launched for a new agent created from the control panel.
/// TODO: make the agent selectable in the create form.
const CREATE_AGENT_DEFAULT_ARGV: &[&str] = &["claude"];

impl App {
    pub(super) fn collect_agent_infos(&self) -> Vec<crate::api::schema::AgentInfo> {
        self.state
            .workspaces
            .iter()
            .enumerate()
            .flat_map(|(ws_idx, ws)| {
                ws.tabs.iter().flat_map(move |tab| {
                    tab.layout
                        .pane_ids()
                        .into_iter()
                        .filter_map(move |pane_id| self.agent_info(ws_idx, pane_id))
                })
            })
            .collect()
    }

    pub(super) fn agent_info_for_target(
        &self,
        target: &str,
    ) -> Result<crate::api::schema::AgentInfo, TerminalTargetError> {
        let resolved = self.resolve_terminal_target(target)?;
        self.agent_info(resolved.ws_idx, resolved.pane_id)
            .ok_or_else(|| TerminalTargetError::NotFound {
                target: target.to_string(),
            })
    }

    pub(super) fn focus_agent_target(
        &mut self,
        target: &str,
    ) -> Result<crate::api::schema::AgentInfo, TerminalTargetError> {
        let resolved = self.resolve_terminal_target(target)?;
        self.state
            .focus_pane_in_workspace(resolved.ws_idx, resolved.pane_id);
        self.state.mode = Mode::Home;
        self.agent_info(resolved.ws_idx, resolved.pane_id)
            .ok_or_else(|| TerminalTargetError::NotFound {
                target: target.to_string(),
            })
    }

    pub(super) fn rename_agent_target(
        &mut self,
        target: &str,
        name: Option<String>,
    ) -> Result<crate::api::schema::AgentInfo, AgentRenameError> {
        let resolved = self
            .resolve_terminal_target(target)
            .map_err(AgentRenameError::Target)?;
        let normalized_name = name.and_then(|name| {
            let trimmed = name.trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        });

        if let Some(name) = normalized_name.as_deref() {
            let conflicts = self.agent_name_conflicts(name, &resolved.terminal_id);
            if !conflicts.is_empty() {
                return Err(AgentRenameError::DuplicateName {
                    name: name.to_string(),
                    candidates: conflicts,
                });
            }
        }

        let Some(terminal) = self
            .state
            .terminals
            .values_mut()
            .find(|terminal| terminal.id.to_string() == resolved.terminal_id)
        else {
            return Err(AgentRenameError::Target(TerminalTargetError::NotFound {
                target: target.to_string(),
            }));
        };
        match normalized_name {
            Some(name) => {
                terminal.set_agent_name(name.clone());
                terminal.set_manual_label(name);
            }
            None => terminal.clear_agent_name(),
        }
        self.state.mark_session_dirty();
        self.agent_info(resolved.ws_idx, resolved.pane_id)
            .ok_or_else(|| {
                AgentRenameError::Target(TerminalTargetError::NotFound {
                    target: target.to_string(),
                })
            })
    }

    pub(super) fn start_agent(
        &mut self,
        params: AgentStartParams,
    ) -> Result<(crate::api::schema::AgentInfo, Vec<String>), AgentStartError> {
        let name = params.name.trim().to_string();
        if name.is_empty() {
            return Err(AgentStartError::InvalidName);
        }
        if params.argv.is_empty() {
            return Err(AgentStartError::EmptyArgv);
        }
        let conflicts = self.agent_name_conflicts(&name, "");
        if !conflicts.is_empty() {
            return Err(AgentStartError::DuplicateName {
                name,
                candidates: conflicts,
            });
        }

        let cwd = params
            .cwd
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"));
        let argv = params.argv;
        let focus = params.focus;
        let (rows, cols) = self.state.estimate_pane_size();

        let (ws_idx, tab_idx, pane_id) = if let Some(tab_id) = params.tab_id {
            let (ws_idx, tab_idx) =
                self.parse_tab_id(&tab_id)
                    .ok_or_else(|| AgentStartError::TargetNotFound {
                        target: tab_id.clone(),
                    })?;
            if let Some(workspace_id) = params.workspace_id.as_deref() {
                let requested_ws_idx = self.parse_workspace_id(workspace_id).ok_or_else(|| {
                    AgentStartError::TargetNotFound {
                        target: workspace_id.to_string(),
                    }
                })?;
                if requested_ws_idx != ws_idx {
                    return Err(AgentStartError::PlacementConflict);
                }
            }
            let target_pane = self.state.workspaces[ws_idx].tabs[tab_idx].layout.focused();
            self.spawn_agent_split(
                ws_idx,
                target_pane,
                params.split.unwrap_or(SplitDirection::Right),
                cwd,
                &argv,
                focus,
            )?
        } else if let Some(workspace_id) = params.workspace_id {
            let ws_idx = self.parse_workspace_id(&workspace_id).ok_or_else(|| {
                AgentStartError::TargetNotFound {
                    target: workspace_id.clone(),
                }
            })?;
            let tab_idx = self.state.workspaces[ws_idx].active_tab;
            let target_pane = self.state.workspaces[ws_idx].tabs[tab_idx].layout.focused();
            self.spawn_agent_split(
                ws_idx,
                target_pane,
                params.split.unwrap_or(SplitDirection::Right),
                cwd,
                &argv,
                focus,
            )?
        } else if self.state.workspaces.is_empty() {
            self.spawn_agent_workspace(cwd, rows, cols, &argv, focus)?
        } else {
            let ws_idx = self.state.active.unwrap_or(0);
            let tab_idx = self.state.workspaces[ws_idx].active_tab;
            let target_pane = self.state.workspaces[ws_idx].tabs[tab_idx].layout.focused();
            self.spawn_agent_split(
                ws_idx,
                target_pane,
                params.split.unwrap_or(SplitDirection::Right),
                cwd,
                &argv,
                focus,
            )?
        };

        let terminal_id = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.terminal_id(pane_id))
            .cloned()
            .ok_or_else(|| AgentStartError::SpawnFailed("terminal disappeared".into()))?;
        let Some(terminal) = self.state.terminals.get_mut(&terminal_id) else {
            return Err(AgentStartError::SpawnFailed("terminal disappeared".into()));
        };
        terminal.set_agent_name(name.clone());
        terminal.set_manual_label(name);
        self.state.mark_session_dirty();

        let agent = self
            .agent_info(ws_idx, pane_id)
            .ok_or_else(|| AgentStartError::SpawnFailed("agent disappeared".into()))?;
        debug_assert_eq!(agent.tab_id, self.public_tab_id(ws_idx, tab_idx).unwrap());
        Ok((agent, argv))
    }

    pub(super) fn agent_start_error_body(
        &self,
        err: AgentStartError,
    ) -> crate::api::schema::ErrorBody {
        match err {
            AgentStartError::InvalidName => crate::api::schema::ErrorBody {
                code: "invalid_agent_name".into(),
                message: "agent name must not be empty".into(),
            },
            AgentStartError::EmptyArgv => crate::api::schema::ErrorBody {
                code: "invalid_agent_argv".into(),
                message: "agent start argv must not be empty".into(),
            },
            AgentStartError::TargetNotFound { target } => crate::api::schema::ErrorBody {
                code: "agent_placement_not_found".into(),
                message: format!("agent placement target {target} not found"),
            },
            AgentStartError::PlacementConflict => crate::api::schema::ErrorBody {
                code: "agent_placement_conflict".into(),
                message: "--tab must belong to --workspace".into(),
            },
            AgentStartError::SpawnFailed(message) => crate::api::schema::ErrorBody {
                code: "agent_start_failed".into(),
                message,
            },
            AgentStartError::DuplicateName { name, candidates } => crate::api::schema::ErrorBody {
                code: "agent_name_taken".into(),
                message: format!(
                    "agent name {name} is already used; candidates: {}",
                    candidates
                        .into_iter()
                        .map(|candidate| format!(
                            "terminal_id={} pane_id={} workspace_id={} tab_id={} cwd={} status={:?}",
                            candidate.terminal_id,
                            candidate.pane_id,
                            candidate.workspace_id,
                            candidate.tab_id,
                            candidate.cwd.unwrap_or_else(|| "unknown".into()),
                            candidate.agent_status,
                        ))
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            },
        }
    }

    pub(super) fn agent_target_error_body(
        &self,
        err: TerminalTargetError,
    ) -> crate::api::schema::ErrorBody {
        match err {
            TerminalTargetError::NotFound { target } => crate::api::schema::ErrorBody {
                code: "agent_not_found".into(),
                message: format!("agent target {target} not found"),
            },
            TerminalTargetError::Ambiguous { target, candidates } => {
                crate::api::schema::ErrorBody {
                    code: "agent_target_ambiguous".into(),
                    message: format!(
                        "agent target {target} is ambiguous; candidates: {}",
                        candidates
                            .into_iter()
                            .map(|candidate| format!(
                                "terminal_id={} pane_id={} workspace_id={} tab_id={} cwd={} status={:?}",
                                candidate.terminal_id,
                                candidate.pane_id,
                                candidate.workspace_id,
                                candidate.tab_id,
                                candidate.cwd.unwrap_or_else(|| "unknown".into()),
                                candidate.agent_status,
                            ))
                            .collect::<Vec<_>>()
                            .join("; ")
                    ),
                }
            }
        }
    }

    pub(super) fn agent_rename_error_body(
        &self,
        err: AgentRenameError,
    ) -> crate::api::schema::ErrorBody {
        match err {
            AgentRenameError::Target(err) => self.agent_target_error_body(err),
            AgentRenameError::DuplicateName { name, candidates } => crate::api::schema::ErrorBody {
                code: "agent_name_taken".into(),
                message: format!(
                    "agent name {name} is already used; candidates: {}",
                    candidates
                        .into_iter()
                        .map(|candidate| format!(
                            "terminal_id={} pane_id={} workspace_id={} tab_id={} cwd={} status={:?}",
                            candidate.terminal_id,
                            candidate.pane_id,
                            candidate.workspace_id,
                            candidate.tab_id,
                            candidate.cwd.unwrap_or_else(|| "unknown".into()),
                            candidate.agent_status,
                        ))
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            },
        }
    }

    fn spawn_agent_workspace(
        &mut self,
        cwd: PathBuf,
        rows: u16,
        cols: u16,
        argv: &[String],
        focus: bool,
    ) -> Result<(usize, usize, crate::layout::PaneId), AgentStartError> {
        let (ws, terminal, runtime) = crate::workspace::Workspace::new_argv_command(
            cwd,
            rows,
            cols,
            argv,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
            self.event_tx.clone(),
            self.render_notify.clone(),
            self.render_dirty.clone(),
        )
        .map_err(|err| AgentStartError::SpawnFailed(err.to_string()))?;
        self.terminal_runtimes.insert(terminal.id.clone(), runtime);
        self.state.terminals.insert(terminal.id.clone(), terminal);
        self.state.workspaces.push(ws);
        let ws_idx = self.state.workspaces.len() - 1;
        self.state
            .remove_alias_shadowed_by_new_pane(self.state.workspaces[ws_idx].tabs[0].root_pane);
        if focus || self.state.active.is_none() {
            self.state.switch_workspace(ws_idx);
            self.state.mode = Mode::Home;
        }
        self.schedule_session_save();
        let pane_id = self.state.workspaces[ws_idx].tabs[0].root_pane;
        Ok((ws_idx, 0, pane_id))
    }

    fn spawn_agent_split(
        &mut self,
        ws_idx: usize,
        target_pane: crate::layout::PaneId,
        split: SplitDirection,
        cwd: PathBuf,
        argv: &[String],
        focus: bool,
    ) -> Result<(usize, usize, crate::layout::PaneId), AgentStartError> {
        let (rows, cols) = self.state.estimate_pane_size();
        let previous_focus = self.state.current_pane_focus_target();
        let direction = match split {
            SplitDirection::Right => ratatui::layout::Direction::Horizontal,
            SplitDirection::Down => ratatui::layout::Direction::Vertical,
        };
        let result = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| {
                ws.split_pane_argv_command(
                    target_pane,
                    direction,
                    rows,
                    cols,
                    Some(cwd),
                    argv,
                    self.state.pane_scrollback_limit_bytes,
                    self.state.host_terminal_theme,
                    focus,
                )
            })
            .ok_or_else(|| AgentStartError::TargetNotFound {
                target: target_pane.raw().to_string(),
            })?
            .map_err(|err| AgentStartError::SpawnFailed(err.to_string()))?;
        self.terminal_runtimes
            .insert(result.1.terminal.id.clone(), result.1.runtime);
        self.state
            .remove_alias_shadowed_by_new_pane(result.1.pane_id);
        self.state
            .terminals
            .insert(result.1.terminal.id.clone(), result.1.terminal);
        if focus {
            self.state.switch_workspace_tab(ws_idx, result.0);
            self.state
                .record_pane_focus_change(previous_focus, ws_idx, result.1.pane_id);
            self.state.mode = Mode::Home;
        }
        self.schedule_session_save();
        Ok((ws_idx, result.0, result.1.pane_id))
    }

    /// Handle a key while in [`Mode::CreateAgent`] (the new-agent form). The form
    /// has several rows (name, base branch, new-branch toggle, new-branch name);
    /// up/down move the active row and the active row is the one being edited.
    /// The name row is active first, so typing a name and pressing enter still
    /// works with no extra keystrokes.
    pub(super) fn handle_create_agent_key(&mut self, key: crossterm::event::KeyEvent) {
        use crate::app::state::CreateFormRow;
        use crossterm::event::{KeyCode, KeyModifiers};
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL);
        let row = self.state.control.create_form_row;
        match key.code {
            KeyCode::Esc => {
                self.state.mode = Mode::Home;
                self.state.name_input.clear();
                self.state.control.reset_create_form();
            }
            KeyCode::Enter => self.submit_create_agent(),
            KeyCode::Up => self.state.control.move_create_form_row(-1),
            KeyCode::Down => self.state.control.move_create_form_row(1),
            KeyCode::Backspace => match row {
                CreateFormRow::Name => {
                    self.state.name_input.pop();
                }
                CreateFormRow::Base => {
                    if let Some(base) = self.state.control.create_base_branch.as_mut() {
                        base.pop();
                    }
                }
                CreateFormRow::NewBranchName => {
                    self.state.control.create_branch_name.pop();
                }
                CreateFormRow::NewBranchToggle => {}
            },
            // Space flips the checkbox on the toggle row and is ignored on text
            // rows (agent/branch names don't contain spaces), so it never lands
            // in a name field.
            KeyCode::Char(' ') if plain => {
                if row == CreateFormRow::NewBranchToggle {
                    self.state.control.create_new_branch = !self.state.control.create_new_branch;
                }
            }
            KeyCode::Char(c) if plain => match row {
                CreateFormRow::Name => self.state.name_input.push(c),
                CreateFormRow::Base => self
                    .state
                    .control
                    .create_base_branch
                    .get_or_insert_with(String::new)
                    .push(c),
                CreateFormRow::NewBranchName => self.state.control.create_branch_name.push(c),
                CreateFormRow::NewBranchToggle => {}
            },
            _ => {}
        }
    }

    /// Pick a default agent name `<repo>-<n>` using the first integer `n` (from 1)
    /// that is free — no existing checkout directory and no agent already using it.
    fn default_agent_name(&self, repo: &crate::workspace::Repository) -> String {
        let mut i = 1usize;
        loop {
            let candidate = format!("{}-{i}", repo.label);
            let path = crate::worktree::default_checkout_path(
                &self.state.worktree_directory,
                &repo.label,
                &candidate,
            );
            let name_taken = !self.agent_name_conflicts(&candidate, "").is_empty();
            if !path.exists() && !name_taken {
                return candidate;
            }
            i += 1;
        }
    }

    /// Create a worktree for the selected repository and launch an agent in it.
    /// The worktree name doubles as the workspace title.
    fn submit_create_agent(&mut self) {
        let Some(repo) = self.state.control.selected_repository().cloned() else {
            self.state.mode = Mode::Home;
            return;
        };
        let mut name = self.state.name_input.trim().to_string();
        if name.is_empty() {
            // No name given — default to "<repo>-<n>" with the first free integer.
            name = self.default_agent_name(&repo);
        }

        // The worktree directory name comes from the agent NAME, not the branch.
        let checkout_path = crate::worktree::default_checkout_path(
            &self.state.worktree_directory,
            &repo.label,
            &name,
        );

        // The base branch is the (possibly edited) picker selection. A blank
        // base falls back to a fresh branch from HEAD (the old behavior).
        let trimmed_base = self
            .state
            .control
            .create_base_branch
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let base = trimmed_base.clone().unwrap_or_else(|| "HEAD".to_string());
        let new_branch = self.state.control.create_new_branch || trimmed_base.is_none();

        if new_branch {
            // Branch name: the dedicated field, or the agent name when left blank.
            let branch_name = {
                let custom = self.state.control.create_branch_name.trim();
                if custom.is_empty() {
                    name.clone()
                } else {
                    custom.to_string()
                }
            };
            let command = crate::worktree::build_worktree_add_new_branch_command(
                &repo.root,
                &checkout_path,
                &branch_name,
                &base,
            );
            if let Err(err) = crate::worktree::run_worktree_command(&command) {
                tracing::warn!(error = %err, "create-agent worktree add (new branch) failed");
                self.state.set_home_toast("create agent failed", err);
                self.state.mode = Mode::Home;
                self.state.name_input.clear();
                self.state.control.reset_create_form();
                return;
            }
            // Best-effort: stack the new branch onto its base with Graphite so it
            // shows up in the stack. gt has no `-C`; run it in the new worktree.
            // Failures are intentionally ignored — the git branch is correct
            // regardless of whether Graphite tracking succeeds.
            self.graphite_track(&checkout_path, &base);
        } else {
            // Check out the existing base branch directly (no `-b`).
            let command = crate::worktree::build_worktree_add_existing_branch_command(
                &repo.root,
                &checkout_path,
                &base,
            );
            if let Err(err) = crate::worktree::run_worktree_command(&command) {
                if crate::worktree::is_branch_already_checked_out_error(&err) {
                    // The branch is live in another worktree; stash that worktree's
                    // path so the prompt can offer to branch off it, or detach it.
                    self.state.control.create_conflict_worktree =
                        crate::worktree::worktree_path_for_branch(&repo.root, &base);
                    self.state.mode = Mode::ConfirmCreateBranch;
                    return;
                }
                tracing::warn!(error = %err, "create-agent worktree add (existing branch) failed");
                self.state.set_home_toast("create agent failed", err);
                self.state.mode = Mode::Home;
                self.state.name_input.clear();
                self.state.control.reset_create_form();
                return;
            }
        }

        self.finish_create_agent(&repo, &checkout_path, name);
    }

    /// Best-effort `gt track --parent <base>` inside `checkout_path` so a newly
    /// created branch stacks on top of its base. `gt` has no `-C`, so we set the
    /// working directory; we also pass `--quiet --no-interactive` so it never
    /// blocks on a prompt. Any failure is logged and ignored.
    fn graphite_track(&self, checkout_path: &std::path::Path, base: &str) {
        let result = std::process::Command::new("gt")
            .current_dir(checkout_path)
            .args([
                "track",
                "--parent",
                base,
                "--quiet",
                "--no-interactive",
            ])
            .output();
        match result {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::warn!(stderr = %stderr.trim(), base, "gt track failed (ignored)");
            }
            Err(err) => {
                tracing::warn!(error = %err, "gt track could not be launched (ignored)");
            }
        }
    }

    /// Spawn the agent in the freshly-created worktree and wire up its workspace
    /// metadata, then return to the home surface with the new agent in Main.
    fn finish_create_agent(
        &mut self,
        repo: &crate::workspace::Repository,
        checkout_path: &std::path::Path,
        name: String,
    ) {
        let argv: Vec<String> = CREATE_AGENT_DEFAULT_ARGV
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (rows, cols) = self.state.estimate_pane_size();
        match self.spawn_agent_workspace(checkout_path.to_path_buf(), rows, cols, &argv, true) {
            Ok((ws_idx, _tab, pane_id)) => {
                if let Some(ws) = self.state.workspaces.get_mut(ws_idx) {
                    ws.set_custom_name(name.clone());
                    if let Some(meta) = crate::workspace::git_space_metadata(checkout_path) {
                        ws.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
                            key: meta.key,
                            label: repo.label.clone(),
                            repo_root: repo.root.clone(),
                            checkout_path: checkout_path.to_path_buf(),
                            is_linked_worktree: true,
                        });
                    }
                }
                if let Some(terminal_id) = self
                    .state
                    .workspaces
                    .get(ws_idx)
                    .and_then(|ws| ws.terminal_id(pane_id))
                    .cloned()
                {
                    if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
                        terminal.set_agent_name(name.clone());
                        terminal.set_manual_label(name);
                    }
                }
                // Surface the new agent in Main while staying in the home shell
                // (with the picker open, the close below keeps focus on it).
                self.state.active = Some(ws_idx);
                self.state.selected = ws_idx;
                self.state.control.focus = crate::app::state::FocusPane::Main;
            }
            Err(err) => {
                let body = self.agent_start_error_body(err);
                tracing::warn!(error = %body.message, "create-agent spawn failed");
                self.state.set_home_toast("create agent failed", body.message);
            }
        }
        self.close_create_form_after_agent();
    }

    /// Close the create-agent form after the agent was (attempted to be)
    /// opened. When the flow was launched from the branch picker, return to
    /// the picker — re-listing the branches so a just-created branch shows up,
    /// with the selection kept on the branch it was on — so more agents can be
    /// opened without reopening it; Esc in the picker still closes it. Without
    /// a picker, land back on the home surface.
    pub(super) fn close_create_form_after_agent(&mut self) {
        self.state.control.reset_create_form();
        self.state.name_input.clear();
        if let Some(review) = self.state.control.review.as_mut() {
            review.refresh_branches();
            self.state.mode = Mode::Review;
            self.state.control.focus = crate::app::state::FocusPane::Control;
        } else {
            self.state.mode = Mode::Home;
        }
    }

    /// Handle a key while confirming what to do because the chosen base branch is
    /// already checked out in another worktree. Three choices: branch off it
    /// (`y`), detach that other worktree to free the branch (`d`), or cancel (`n`).
    pub(super) fn handle_confirm_create_branch_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                // Retry the create flow on the new-branch path.
                self.state.control.create_new_branch = true;
                self.state.control.create_conflict_worktree = None;
                self.submit_create_agent();
            }
            KeyCode::Char('d') | KeyCode::Char('D') => {
                self.detach_conflicting_worktree_and_retry();
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.state.control.reset_create_form();
                self.state.name_input.clear();
                self.state.mode = Mode::Home;
            }
            _ => {}
        }
    }

    /// Detach the worktree currently holding the base branch (checking it out at
    /// the same commit in detached HEAD), then retry the existing-branch checkout
    /// now that the branch is free. Keeps the prompt open on failure.
    fn detach_conflicting_worktree_and_retry(&mut self) {
        let Some(path) = self.state.control.create_conflict_worktree.clone() else {
            // Nothing to detach; treat as cancel.
            self.state.control.reset_create_form();
            self.state.name_input.clear();
            self.state.mode = Mode::Home;
            return;
        };
        let command = crate::worktree::build_worktree_detach_command(&path);
        if let Err(err) = crate::worktree::run_worktree_command(&command) {
            tracing::warn!(error = %err, "detach conflicting worktree failed");
            self.state.set_home_toast("detach failed", err);
            return;
        }
        self.state.control.create_conflict_worktree = None;
        // The branch is free now; retry the existing-branch checkout.
        self.submit_create_agent();
    }

    /// Open a plain interactive shell in the selected repository's root and
    /// surface it in Main. Unlike a new agent, this creates no worktree — it
    /// just drops a terminal into the repo so you can run ad-hoc commands.
    pub(super) fn open_terminal_in_selected_repo(&mut self) {
        let Some(repo) = self.state.control.selected_repository().cloned() else {
            return;
        };
        let shell = crate::pane::pane_shell(&self.state.default_shell);
        let argv = vec![shell];
        let (rows, cols) = self.state.estimate_pane_size();
        match self.spawn_agent_workspace(repo.root.clone(), rows, cols, &argv, true) {
            Ok((ws_idx, _tab, _pane)) => {
                if let Some(ws) = self.state.workspaces.get_mut(ws_idx) {
                    ws.set_custom_name(format!("terminal: {}", repo.label));
                }
                self.state.active = Some(ws_idx);
                self.state.selected = ws_idx;
                self.state.control.focus = crate::app::state::FocusPane::Main;
            }
            Err(err) => {
                let body = self.agent_start_error_body(err);
                tracing::warn!(error = %body.message, "open-terminal spawn failed");
                self.state.set_home_toast("open terminal failed", body.message);
            }
        }
        self.state.mode = Mode::Home;
    }

    /// Handle a key while in [`Mode::RenameAgent`] (the rename form).
    pub(super) fn handle_rename_agent_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};
        match key.code {
            KeyCode::Esc => {
                self.state.mode = Mode::Home;
                self.state.name_input.clear();
            }
            KeyCode::Enter => self.submit_rename_agent(),
            KeyCode::Backspace => {
                if self.state.name_input_replace_on_type {
                    self.state.name_input.clear();
                    self.state.name_input_replace_on_type = false;
                } else {
                    self.state.name_input.pop();
                }
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL) =>
            {
                if self.state.name_input_replace_on_type {
                    self.state.name_input.clear();
                    self.state.name_input_replace_on_type = false;
                }
                self.state.name_input.push(c);
            }
            _ => {}
        }
    }

    /// Apply the rename: relabel the selected agent and, when it owns a managed
    /// worktree, move the worktree directory so its name tracks the agent's.
    fn submit_rename_agent(&mut self) {
        let name = self.state.name_input.trim().to_string();
        if name.is_empty() {
            // Keep the form open until a name is provided.
            return;
        }

        let entries = crate::ui::agent_panel_entries_all(&self.state);
        let Some(ws_idx) = entries
            .get(self.state.control.selected_agent)
            .map(|entry| entry.ws_idx)
        else {
            self.state.mode = Mode::Home;
            self.state.name_input.clear();
            return;
        };

        // Move the worktree directory to match the new name, if this agent owns a
        // managed (linked) worktree. The directory name is derived the same way
        // as at creation: <worktree_dir>/<repo>/<name-slug>.
        let move_plan = self.state.workspaces.get(ws_idx).and_then(|ws| {
            ws.worktree_space().map(|space| {
                let new_path = crate::worktree::default_checkout_path(
                    &self.state.worktree_directory,
                    &space.label,
                    &name,
                );
                (space.repo_root.clone(), space.checkout_path.clone(), new_path)
            })
        });

        let mut moved_to: Option<PathBuf> = None;
        if let Some((repo_root, old_path, new_path)) = move_plan {
            if new_path != old_path {
                let command =
                    crate::worktree::build_worktree_move_command(&repo_root, &old_path, &new_path);
                if let Err(err) = crate::worktree::run_worktree_command(&command) {
                    tracing::warn!(error = %err, "rename-agent worktree move failed");
                    self.state.set_home_toast("rename failed", err);
                    self.state.mode = Mode::Home;
                    self.state.name_input.clear();
                    return;
                }
                moved_to = Some(new_path);
            }
        }

        if let Some(ws) = self.state.workspaces.get_mut(ws_idx) {
            ws.set_custom_name(name.clone());
            if let Some(new_path) = moved_to.clone() {
                ws.identity_cwd = new_path.clone();
                if let Some(space) = ws.worktree_space.as_mut() {
                    space.checkout_path = new_path;
                }
            }
        }

        // Relabel the agent terminal and repoint its recorded cwd at the moved
        // worktree so later actions (PR, review) resolve the right path.
        if let Some(terminal_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.focused_pane_id())
            .and_then(|pane_id| self.state.workspaces[ws_idx].terminal_id(pane_id))
            .cloned()
        {
            if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
                terminal.set_agent_name(name.clone());
                terminal.set_manual_label(name);
                if let Some(new_path) = moved_to {
                    terminal.cwd = new_path;
                }
            }
        }

        self.state.mark_session_dirty();
        self.state.mode = Mode::Home;
        self.state.name_input.clear();
    }

    /// Handle a key while in [`Mode::Review`] (the branch picker).
    pub(super) fn handle_review_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};
        // Plain letters (no modifier) act as commands in the picker; an alt/ctrl
        // modifier shouldn't trigger navigation.
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL);
        // While `O`'s PR-number input is collecting digits, it owns the keys:
        // digits append, backspace edits, Enter opens that PR, Esc returns to
        // the list (without closing the picker). Everything else is inert.
        if self
            .state
            .control
            .review
            .as_ref()
            .is_some_and(|review| review.pr_number_input.is_some())
        {
            match key.code {
                KeyCode::Esc => {
                    if let Some(review) = self.state.control.review.as_mut() {
                        review.pr_number_input = None;
                    }
                }
                KeyCode::Backspace => {
                    if let Some(input) = self
                        .state
                        .control
                        .review
                        .as_mut()
                        .and_then(|review| review.pr_number_input.as_mut())
                    {
                        input.pop();
                    }
                }
                KeyCode::Char(c) if c.is_ascii_digit() => {
                    if let Some(input) = self
                        .state
                        .control
                        .review
                        .as_mut()
                        .and_then(|review| review.pr_number_input.as_mut())
                    {
                        input.push(c);
                    }
                }
                KeyCode::Enter => self.open_typed_pr_for_review(),
                _ => {}
            }
            return;
        }
        // Which list the picker is showing; several commands dispatch on it.
        let prs_shown = self
            .state
            .control
            .review
            .as_ref()
            .is_some_and(|review| review.source == crate::app::state::PickerSource::ReviewRequests);
        match key.code {
            KeyCode::Esc => {
                self.state.mode = Mode::Home;
                self.state.control.review = None;
            }
            KeyCode::Up => self.state.review_move_selection(-1),
            KeyCode::Down => self.state.review_move_selection(1),
            // Vim-style navigation: j/k mirror the arrow keys; h/l are inert,
            // matching the home Control/Agents lists.
            KeyCode::Char('k') if plain => self.state.review_move_selection(-1),
            KeyCode::Char('j') if plain => self.state.review_move_selection(1),
            KeyCode::Char('h') | KeyCode::Char('l') if plain => {}
            // Plain `o` toggles between the repo's branches and the open PRs
            // awaiting the user's review.
            KeyCode::Char('o') if plain => self.toggle_review_picker_source(),
            // `O` (shift+o) opens a PR by typed number — any PR in the repo,
            // not just the ones awaiting the user's review.
            KeyCode::Char('O') if plain => {
                if let Some(review) = self.state.control.review.as_mut() {
                    review.pr_number_input = Some(String::new());
                }
            }
            // The picker is not a text input, so plain `p` (or alt+p) submits a
            // PR. Only meaningful on the branch list — the PR list IS PRs.
            KeyCode::Char('p') if !prs_shown => self.submit_pr_for_review(),
            // Plain `c` checks the selected branch (or PR head) out into the
            // Main pane's worktree, when Main is a worktree of the repo browsed.
            KeyCode::Char('c') if plain => {
                if prs_shown {
                    self.checkout_selected_pr_into_main();
                } else {
                    self.checkout_selected_branch_into_main();
                }
            }
            // Enter (or Space): on the branch list, pick the base branch and
            // move to the name form (alt pre-checks "create a new branch"); on
            // the PR list, open the PR for review in its own workspace. Neither
            // depends on what the Main pane currently holds — only `c` does.
            // Either way the picker stays open once the agent/workspace opens,
            // so more branches/PRs can be opened without reopening it.
            KeyCode::Enter | KeyCode::Char(' ') => {
                if prs_shown {
                    self.open_selected_pr_for_review();
                } else {
                    let new_branch = key.modifiers.contains(KeyModifiers::ALT);
                    self.pick_branch_for_create(new_branch);
                }
            }
            _ => {}
        }
    }

    /// `o` in the branch picker: toggle between the repo's branch list and the
    /// open PRs awaiting the user's review. The PR list is re-fetched (via
    /// `gh`) on every toggle to it — review requests come and go while the
    /// picker is open, so a one-shot cache goes stale. If the fetch fails, the
    /// previous list (when there is one) is shown with a toast; with nothing
    /// to fall back on, the branch list stays shown and the toast explains.
    fn toggle_review_picker_source(&mut self) {
        use crate::app::state::PickerSource;
        let Some(review) = self.state.control.review.as_ref() else {
            return;
        };
        if review.source == PickerSource::Branches {
            let repo_root = review.repo.root.clone();
            let had_prs = review.prs.is_some();
            match crate::workspace::list_prs_for_my_review(&repo_root) {
                Ok(prs) => {
                    if let Some(review) = self.state.control.review.as_mut() {
                        review.prs = Some(prs);
                    }
                }
                Err(err) if had_prs => {
                    self.state.set_home_toast("PR list refresh failed", err);
                }
                Err(err) => {
                    self.state.set_home_toast("PR list failed", err);
                    return;
                }
            }
        }
        if let Some(review) = self.state.control.review.as_mut() {
            review.source = match review.source {
                PickerSource::Branches => PickerSource::ReviewRequests,
                PickerSource::ReviewRequests => PickerSource::Branches,
            };
            review.selected = 0;
            review.scroll = 0;
        }
    }

    /// Stash the branch selected in the picker as the create-agent base and open
    /// the name form. `new_branch` pre-fills the "create a new branch?" checkbox.
    fn pick_branch_for_create(&mut self, new_branch: bool) {
        let Some(review) = self.state.control.review.as_ref() else {
            return;
        };
        let Some(branch) = review.branches.get(review.selected) else {
            return;
        };
        // The base must be a local branch name; strip a remote prefix if present.
        let base = branch
            .name
            .rsplit_once('/')
            .filter(|_| branch.is_remote)
            .map(|(_, name)| name.to_string())
            .unwrap_or_else(|| branch.name.clone());
        self.state.control.create_base_branch = Some(base);
        self.state.control.create_new_branch = new_branch;
        self.state.control.create_branch_name.clear();
        self.state.control.create_form_row = crate::app::state::CreateFormRow::Name;
        self.state.control.create_conflict_worktree = None;
        self.state.name_input.clear();
        self.state.name_input_replace_on_type = false;
        self.state.mode = Mode::CreateAgent;
    }

    /// `c` in the branch picker: check the selected branch out into the Main
    /// pane's worktree, in place, and refresh the review row to the new branch.
    ///
    /// Only acts when the active (Main) workspace is a worktree of the same repo
    /// whose branches are being browsed; otherwise it explains why via a toast.
    /// The picker stays open on the same repo either way, so you can keep
    /// checking branches out; on success it refreshes the branch list.
    fn checkout_selected_branch_into_main(&mut self) {
        let Some(review) = self.state.control.review.as_ref() else {
            return;
        };
        let Some(branch) = review.branches.get(review.selected) else {
            return;
        };
        // The checkout target must be a local branch name; strip a remote prefix
        // (matching `pick_branch_for_create`).
        let branch_name = branch
            .name
            .rsplit_once('/')
            .filter(|_| branch.is_remote)
            .map(|(_, name)| name.to_string())
            .unwrap_or_else(|| branch.name.clone());
        let picker_repo_root = review.repo.root.clone();

        let Some(ws_idx) = self.state.active else {
            self.state
                .set_home_toast("checkout skipped", "no active workspace in the Main pane");
            return;
        };
        let Some(space) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.worktree_space().cloned())
        else {
            self.state
                .set_home_toast("checkout skipped", "the Main pane is not a repo worktree");
            return;
        };
        if crate::worktree::canonical_or_original(&space.repo_root)
            != crate::worktree::canonical_or_original(&picker_repo_root)
        {
            self.state.set_home_toast(
                "checkout skipped",
                "the Main pane is a different repo than the one selected",
            );
            return;
        }

        let command =
            crate::worktree::build_checkout_branch_command(&space.checkout_path, &branch_name);
        match crate::worktree::run_worktree_command(&command) {
            Ok(()) => self.apply_checkout_into_main(ws_idx, branch_name),
            // The branch is live in another worktree (git refuses to check the
            // same branch out twice). Offer to detach that worktree and retry,
            // mirroring the create-agent flow's conflict prompt.
            Err(err) if crate::worktree::is_branch_already_checked_out_error(&err) => {
                match crate::worktree::worktree_path_for_branch(&picker_repo_root, &branch_name) {
                    Some(worktree) => {
                        self.state.control.checkout_conflict =
                            Some(crate::app::state::CheckoutConflict {
                                branch: branch_name,
                                worktree,
                            });
                        self.state.mode = Mode::ConfirmCheckoutDetach;
                    }
                    // Couldn't locate the holding worktree; nothing to offer.
                    None => self.state.set_home_toast("checkout failed", err),
                }
            }
            Err(err) => self.state.set_home_toast("checkout failed", err),
        }
    }

    /// Apply a successful in-place checkout of `branch_name` into the Main pane's
    /// worktree (workspace `ws_idx`): refresh the cached branch, respawn the
    /// review row against the new base, and keep the picker open with an
    /// up-to-date branch list so you can keep checking branches out.
    fn apply_checkout_into_main(&mut self, ws_idx: usize, branch_name: String) {
        // Reflect the new branch immediately so the review row respawns against
        // it; the periodic git poll keeps `cached_git_branch` accurate after.
        if let Some(ws) = self.state.workspaces.get_mut(ws_idx) {
            ws.cached_git_branch = Some(branch_name.clone());
        }
        self.respawn_review_row_after_checkout(ws_idx);
        self.state.mark_session_dirty();
        self.state.set_home_toast("checked out", branch_name);
        // Stay in the picker on the same repo so you can keep checking branches
        // out; refresh the branch list (keeping the selection in place) so the
        // current-branch marker reflects the checkout just performed.
        if let Some(review) = self.state.control.review.as_mut() {
            review.refresh_branches();
        }
    }

    /// Handle a key while confirming whether to detach the worktree holding the
    /// branch the picker's `c` wants to claim. `d` detaches it and retries the
    /// checkout into Main; `n`/Esc cancels back to the picker.
    pub(super) fn handle_confirm_checkout_detach_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Char('d') | KeyCode::Char('D') => {
                self.detach_conflicting_worktree_and_checkout();
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.state.control.checkout_conflict = None;
                self.state.mode = Mode::Review;
            }
            _ => {}
        }
    }

    /// Detach the worktree currently holding the conflicting branch (checking it
    /// out at the same commit in detached HEAD), then retry the picker checkout
    /// now that the branch is free. Keeps the prompt open on detach failure.
    fn detach_conflicting_worktree_and_checkout(&mut self) {
        let Some(conflict) = self.state.control.checkout_conflict.take() else {
            self.state.mode = Mode::Review;
            return;
        };
        let command = crate::worktree::build_worktree_detach_command(&conflict.worktree);
        if let Err(err) = crate::worktree::run_worktree_command(&command) {
            tracing::warn!(error = %err, "detach conflicting worktree (picker checkout) failed");
            // Restore the conflict so the prompt stays actionable.
            self.state.control.checkout_conflict = Some(conflict);
            self.state.set_home_toast("detach failed", err);
            return;
        }
        // The branch is free now; return to the picker and retry the checkout
        // (the selection is unchanged, so it lands on the same branch).
        self.state.mode = Mode::Review;
        self.checkout_selected_branch_into_main();
    }

    /// `c` on the review-requests list: check the selected PR out into the Main
    /// pane's worktree (via `gh pr checkout`, which also handles fork PRs) and
    /// tag the workspace with the PR so the review row diffs against the PR
    /// base and `alt+g` switches to drafting review replies. This is the one
    /// PR-list action that requires the Main pane to be a worktree of the
    /// picker's repo; the picker stays open, like the branch-list checkout.
    fn checkout_selected_pr_into_main(&mut self) {
        let Some(pr) = self
            .state
            .control
            .review
            .as_ref()
            .and_then(|review| review.selected_pr())
            .cloned()
        else {
            return;
        };
        let Some(picker_repo_root) = self
            .state
            .control
            .review
            .as_ref()
            .map(|review| review.repo.root.clone())
        else {
            return;
        };

        let Some(ws_idx) = self.state.active else {
            self.state
                .set_home_toast("checkout skipped", "no active workspace in the Main pane");
            return;
        };
        let Some(space) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.worktree_space().cloned())
        else {
            self.state
                .set_home_toast("checkout skipped", "the Main pane is not a repo worktree");
            return;
        };
        if crate::worktree::canonical_or_original(&space.repo_root)
            != crate::worktree::canonical_or_original(&picker_repo_root)
        {
            self.state.set_home_toast(
                "checkout skipped",
                "the Main pane is a different repo than the one selected",
            );
            return;
        }

        let output = std::process::Command::new("gh")
            .current_dir(&space.checkout_path)
            .args(["pr", "checkout", &pr.number.to_string()])
            .output();
        match output {
            Ok(out) if out.status.success() => {}
            Ok(out) => {
                let message = String::from_utf8_lossy(&out.stderr).trim().to_string();
                tracing::warn!(error = %message, "gh pr checkout failed");
                self.state.set_home_toast("checkout failed", message);
                return;
            }
            Err(err) => {
                self.state
                    .set_home_toast("checkout failed", format!("gh not available: {err}"));
                return;
            }
        }

        // Reflect the new branch and the PR-under-review immediately; the
        // periodic git poll keeps `cached_git_branch` accurate after.
        if let Some(ws) = self.state.workspaces.get_mut(ws_idx) {
            ws.cached_git_branch = Some(pr.head_branch.clone());
            ws.reviewing_pr = Some(pr.clone());
        }
        self.respawn_review_row_after_checkout(ws_idx);
        self.state.mark_session_dirty();
        self.state
            .set_home_toast("reviewing", format!("PR #{} · {}", pr.number, pr.head_branch));

        // Keep the picker's branch list (shown when toggling back) in sync
        // with the checkout just performed.
        if let Some(review) = self.state.control.review.as_mut() {
            review.refresh_branches();
        }
    }

    /// Space/Enter on the review-requests list: open the selected PR for
    /// review, independent of what the Main pane holds (mirroring the branch
    /// list, where space opens the create-agent form; only `c` targets Main).
    ///
    /// Reuses a workspace already checked out on the PR's head branch when one
    /// exists; otherwise creates a fresh worktree (detached `worktree add`,
    /// then `gh pr checkout`, which fetches the head and works for fork PRs)
    /// and spawns an agent workspace in it via the create-agent machinery.
    /// Either way the workspace is tagged with the PR — making the review row
    /// diff against the PR base and `alt+g` draft replies — and the review row
    /// is opened.
    fn open_selected_pr_for_review(&mut self) {
        let Some(pr) = self
            .state
            .control
            .review
            .as_ref()
            .and_then(|review| review.selected_pr())
            .cloned()
        else {
            return;
        };
        self.open_pr_for_review(pr);
    }

    /// Enter on `O`'s PR-number input: look the typed number up via `gh`
    /// (any PR in the repo, not just review requests) and open it for review
    /// like a row in the PR list. The input stays up on a failed lookup so
    /// the number can be corrected; toasts explain what went wrong.
    fn open_typed_pr_for_review(&mut self) {
        let Some(review) = self.state.control.review.as_ref() else {
            return;
        };
        let Some(number) = review
            .pr_number_input
            .as_ref()
            .and_then(|input| input.parse::<u64>().ok())
        else {
            return; // empty (or absurdly long) input: keep collecting digits
        };
        let repo_root = review.repo.root.clone();
        match crate::workspace::pr_by_number(&repo_root, number) {
            Ok(pr) => {
                if let Some(review) = self.state.control.review.as_mut() {
                    review.pr_number_input = None;
                }
                self.open_pr_for_review(pr);
            }
            Err(err) => self
                .state
                .set_home_toast(format!("PR #{number} lookup failed"), err),
        }
    }

    /// Open `pr` for review, independent of what the Main pane holds (shared
    /// by the PR list's Enter/Space and `O`'s typed PR number).
    fn open_pr_for_review(&mut self, pr: crate::workspace::ReviewPr) {
        let Some(repo) = self
            .state
            .control
            .review
            .as_ref()
            .map(|review| review.repo.clone())
        else {
            return;
        };

        // Reuse a workspace already on this PR's branch.
        let repo_key = crate::worktree::canonical_or_original(&repo.root);
        let existing = self.state.workspaces.iter().position(|ws| {
            ws.worktree_space().is_some_and(|space| {
                crate::worktree::canonical_or_original(&space.repo_root) == repo_key
            }) && ws.branch().as_deref() == Some(pr.head_branch.as_str())
        });
        if let Some(ws_idx) = existing {
            self.state.switch_workspace(ws_idx);
            self.finish_open_pr_review(ws_idx, pr);
            return;
        }

        // Fresh worktree: add it detached, then let `gh pr checkout` fetch the
        // PR head and switch to it in place.
        let name = format!("pr-{}", pr.number);
        let checkout_path = crate::worktree::default_checkout_path(
            &self.state.worktree_directory,
            &repo.label,
            &name,
        );
        let add =
            crate::worktree::build_worktree_add_detached_command(&repo.root, &checkout_path);
        if let Err(err) = crate::worktree::run_worktree_command(&add) {
            self.state.set_home_toast("open PR failed", err);
            return;
        }
        let checkout_err = match std::process::Command::new("gh")
            .current_dir(&checkout_path)
            .args(["pr", "checkout", &pr.number.to_string()])
            .output()
        {
            Ok(out) if out.status.success() => None,
            Ok(out) => Some(String::from_utf8_lossy(&out.stderr).trim().to_string()),
            Err(err) => Some(format!("gh not available: {err}")),
        };
        if let Some(err) = checkout_err {
            tracing::warn!(error = %err, "gh pr checkout into fresh worktree failed");
            // Don't leave the dangling detached worktree behind.
            let remove =
                crate::worktree::build_worktree_remove_command(&repo.root, &checkout_path, true);
            let _ = crate::worktree::run_worktree_command(&remove);
            self.state.set_home_toast("open PR failed", err);
            return;
        }

        self.finish_create_agent(&repo, &checkout_path, name);
        // finish_create_agent toasts on spawn failure and has already put the
        // picker back up; in that case there is no new workspace to tag, so stop.
        let Some(ws_idx) = self.state.workspaces.iter().position(|ws| {
            ws.worktree_space()
                .is_some_and(|space| space.checkout_path == checkout_path)
        }) else {
            return;
        };
        self.finish_open_pr_review(ws_idx, pr);
    }

    /// Shared tail of opening a PR for review in workspace `ws_idx` (which is
    /// already active): tag it with the PR, open the review row against the PR
    /// base (respawning a stale one), and keep the picker open and focused so
    /// more PRs can be opened straight away.
    fn finish_open_pr_review(&mut self, ws_idx: usize, pr: crate::workspace::ReviewPr) {
        let toast = format!("PR #{} · {}", pr.number, pr.head_branch);
        if let Some(ws) = self.state.workspaces.get_mut(ws_idx) {
            ws.cached_git_branch = Some(pr.head_branch.clone());
            ws.reviewing_pr = Some(pr);
        }
        let review_open = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.pane_with_role(crate::pane::PaneRole::Review))
            .is_some();
        if review_open {
            // Retarget an already-open review row at the PR's base.
            self.respawn_review_row_after_checkout(ws_idx);
        } else {
            self.toggle_review_row();
        }
        self.state.mark_session_dirty();
        // Keep the picker open and focused — selection still on the PR just
        // opened — so more PRs can be opened without reopening it; the new
        // workspace is surfaced in Main behind it. Esc still closes the picker.
        self.state.mode = Mode::Review;
        self.state.control.focus = crate::app::state::FocusPane::Control;
        self.state.set_home_toast("reviewing", toast);
    }

    /// If the active workspace's REVIEW row is open, replace it with a freshly
    /// spawned one so it reflects the now-current branch. The old review pane's
    /// `vimrev` is bound to the previous branch's base, so reattaching it would
    /// show a stale diff — kill it and let [`Self::toggle_review_row`] spawn a
    /// fresh row (which reads the updated `cached_git_branch`).
    fn respawn_review_row_after_checkout(&mut self, ws_idx: usize) {
        use crate::pane::PaneRole;
        let review_pane = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.pane_with_role(PaneRole::Review));
        let Some(review_pane) = review_pane else {
            return; // review row not open; nothing to refresh
        };
        let terminal_id = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.terminal_id(review_pane).cloned());
        if let Some(ws) = self.state.workspaces.get_mut(ws_idx) {
            ws.remove_pane(review_pane);
            // Drop the stash so `toggle_review_row` spawns fresh instead of
            // reattaching the now-killed terminal.
            ws.detached_review = None;
            // Keep focus off the gone pane.
            if let Some(agent) = ws.agent_pane() {
                if let Some(tab_idx) = ws.find_tab_index_for_pane(agent) {
                    ws.tabs[tab_idx].layout.focus_pane(agent);
                    ws.tabs[tab_idx].layout.equalize_vertical();
                }
            }
        }
        if let Some(terminal_id) = terminal_id {
            self.state.remove_unattached_terminal_ids([terminal_id]);
        }
        self.toggle_review_row();
    }

    /// Submit a PR for the branch selected in the review picker.
    fn submit_pr_for_review(&mut self) {
        let Some(review) = self.state.control.review.as_ref() else {
            return;
        };
        let Some(branch) = review.branches.get(review.selected) else {
            return;
        };
        let repo_root = review.repo.root.clone();
        // PR head must be a local branch name; strip a remote prefix if present.
        let head = branch
            .name
            .rsplit_once('/')
            .filter(|_| branch.is_remote)
            .map(|(_, name)| name.to_string())
            .unwrap_or_else(|| branch.name.clone());
        let base = crate::workspace::review_base(&repo_root, &head);
        self.submit_pr(&repo_root, &head, &base);
    }

    /// Submit a PR for the branch of the agent selected in the agents half.
    pub(super) fn submit_pr_for_selected_agent(&mut self) {
        let entries = crate::ui::agent_panel_entries_all(&self.state);
        let Some(ws_idx) = entries
            .get(self.state.control.selected_agent)
            .map(|entry| entry.ws_idx)
        else {
            return;
        };
        let Some(ws) = self.state.workspaces.get(ws_idx) else {
            return;
        };
        let Some(branch) = ws.branch() else {
            self.state
                .set_home_toast("PR failed", "agent has no branch");
            return;
        };
        let repo_root = ws
            .worktree_space()
            .map(|space| space.repo_root.clone())
            .unwrap_or_else(|| ws.identity_cwd.clone());
        let base = crate::workspace::review_base(&repo_root, &branch);
        self.submit_pr(&repo_root, &branch, &base);
    }

    /// Run `gh pr create --fill` for `head` against `base`, reporting via a toast.
    fn submit_pr(&mut self, repo_root: &Path, head: &str, base: &str) {
        let output = std::process::Command::new("gh")
            .current_dir(repo_root)
            .args(["pr", "create", "--fill", "--head", head, "--base", base])
            .output();
        match output {
            Ok(out) if out.status.success() => {
                let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
                self.state.set_home_toast("PR created", url);
            }
            Ok(out) => {
                let message = String::from_utf8_lossy(&out.stderr).trim().to_string();
                tracing::warn!(error = %message, "gh pr create failed");
                self.state.set_home_toast("PR failed", message);
            }
            Err(err) => {
                self.state
                    .set_home_toast("PR failed", format!("gh not available: {err}"));
            }
        }
    }

    /// Toggle the in-worktree REVIEW row of the active workspace. See
    /// [`Self::toggle_row`].
    pub(crate) fn toggle_review_row(&mut self) {
        self.toggle_row(crate::pane::PaneRole::Review);
    }

    /// Toggle the in-worktree TERMINAL row of the active workspace. See
    /// [`Self::toggle_row`].
    pub(crate) fn toggle_terminal_row(&mut self) {
        self.toggle_row(crate::pane::PaneRole::Terminal);
    }

    /// Toggle a stacked review/terminal row inside the active workspace (the
    /// agent's own worktree).
    ///
    /// - If the row is currently attached: DETACH the pane (remove from layout,
    ///   keep its terminal alive in the registries) and stash its terminal id so
    ///   a later re-open re-attaches the same terminal. Never kills it.
    /// - Else open it: re-attach a previously-detached terminal when present,
    ///   otherwise spawn a fresh one (review command / plain shell) in the
    ///   agent's worktree. New rows land on TOP of their split target.
    fn toggle_row(&mut self, role: crate::pane::PaneRole) {
        use crate::pane::PaneRole;

        let Some(ws_idx) = self.state.active else {
            return;
        };

        // Currently attached? -> detach (keep the terminal alive).
        let attached = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.pane_with_role(role));
        if let Some(pane_id) = attached {
            let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
                return;
            };
            let terminal_id = ws.terminal_id(pane_id).cloned();
            ws.remove_pane(pane_id);
            match role {
                PaneRole::Review => ws.detached_review = terminal_id,
                PaneRole::Terminal => ws.detached_terminal = terminal_id,
                PaneRole::Agent => {}
            }
            // Refocus the agent (root) pane so focus never dangles on a gone pane.
            if let Some(agent) = ws.agent_pane() {
                if let Some(tab_idx) = ws.find_tab_index_for_pane(agent) {
                    ws.tabs[tab_idx].layout.focus_pane(agent);
                    ws.tabs[tab_idx].layout.equalize_vertical();
                }
            }
            self.state.mark_session_dirty();
            return;
        }

        // Opening: pick the split target (so order stays review/terminal/agent).
        let target = {
            let Some(ws) = self.state.workspaces.get(ws_idx) else {
                return;
            };
            match role {
                // The terminal row splits the agent (root) pane.
                PaneRole::Terminal => ws.agent_pane(),
                // The review row lands above the terminal row when present, else
                // above the agent — so review is always the topmost row.
                PaneRole::Review => ws
                    .pane_with_role(PaneRole::Terminal)
                    .or_else(|| ws.agent_pane()),
                PaneRole::Agent => None,
            }
        };
        let Some(target) = target else {
            return;
        };

        // Re-attach a kept-alive terminal, if we have one.
        let detached = self.state.workspaces.get(ws_idx).and_then(|ws| match role {
            PaneRole::Review => ws.detached_review.clone(),
            PaneRole::Terminal => ws.detached_terminal.clone(),
            PaneRole::Agent => None,
        });
        if let Some(terminal_id) = detached {
            // Only re-attach if the terminal still exists; otherwise fall through
            // to a fresh spawn.
            if self.state.terminals.contains_key(&terminal_id) {
                let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
                    return;
                };
                if let Some(new) = ws.reattach_row(target, terminal_id, role) {
                    if let Some(tab_idx) = ws.find_tab_index_for_pane(new) {
                        ws.tabs[tab_idx].layout.focus_pane(new);
                        ws.tabs[tab_idx].layout.equalize_vertical();
                    }
                    match role {
                        PaneRole::Review => ws.detached_review = None,
                        PaneRole::Terminal => ws.detached_terminal = None,
                        PaneRole::Agent => {}
                    }
                    self.state.mark_session_dirty();
                    return;
                }
            }
            // Stale handle: drop it and spawn fresh.
            if let Some(ws) = self.state.workspaces.get_mut(ws_idx) {
                match role {
                    PaneRole::Review => ws.detached_review = None,
                    PaneRole::Terminal => ws.detached_terminal = None,
                    PaneRole::Agent => {}
                }
            }
        }

        // First open: while reviewing someone else's PR, freshen the diff
        // target `origin/<base>` in the background first — the row then spawns
        // from the fetch handler. (Re-attaches above keep their already-
        // rendered diff, so they don't fetch.)
        if role == PaneRole::Review && self.start_review_base_fetch(ws_idx) {
            return;
        }
        self.spawn_fresh_row(ws_idx, role);
    }

    /// Spawn a fresh review/terminal row in workspace `ws_idx`. Recomputes the
    /// split target itself so it can run deferred — from the review-base fetch
    /// handler — as well as straight from [`Self::toggle_row`].
    fn spawn_fresh_row(&mut self, ws_idx: usize, role: crate::pane::PaneRole) {
        use crate::pane::PaneRole;
        let target = {
            let Some(ws) = self.state.workspaces.get(ws_idx) else {
                return;
            };
            match role {
                PaneRole::Terminal => ws.agent_pane(),
                PaneRole::Review => ws
                    .pane_with_role(PaneRole::Terminal)
                    .or_else(|| ws.agent_pane()),
                PaneRole::Agent => None,
            }
        };
        let Some(target) = target else {
            return;
        };

        // Build argv + cwd and spawn the row.
        let (argv, cwd) = match self.row_spawn_spec(ws_idx, role) {
            Some(spec) => spec,
            None => return,
        };
        let (rows, cols) = self.state.estimate_pane_size();
        let result = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| {
                ws.split_pane_argv_command(
                    target,
                    ratatui::layout::Direction::Vertical,
                    rows,
                    cols,
                    Some(cwd),
                    &argv,
                    self.state.pane_scrollback_limit_bytes,
                    self.state.host_terminal_theme,
                    true,
                )
            });
        let (tab_idx, new) = match result {
            Some(Ok(pair)) => pair,
            Some(Err(err)) => {
                tracing::warn!(error = %err, "row spawn failed");
                self.state.set_home_toast("open row failed", err.to_string());
                return;
            }
            None => return,
        };
        let new_pane_id = new.pane_id;
        self.terminal_runtimes
            .insert(new.terminal.id.clone(), new.runtime);
        self.state
            .remove_alias_shadowed_by_new_pane(new_pane_id);
        self.state
            .terminals
            .insert(new.terminal.id.clone(), new.terminal);
        if let Some(ws) = self.state.workspaces.get_mut(ws_idx) {
            // The fresh pane spawned BELOW target; swap it to the top row.
            ws.tabs[tab_idx].layout.swap_panes(target, new_pane_id);
            if let Some(pane) = ws.tabs[tab_idx].panes.get_mut(&new_pane_id) {
                pane.role = role;
            }
            ws.tabs[tab_idx].layout.focus_pane(new_pane_id);
            ws.tabs[tab_idx].layout.equalize_vertical();
        }
        self.state.mark_session_dirty();
    }

    /// While workspace `ws_idx` is reviewing someone else's PR, start a
    /// background `git fetch origin <base> <head>` so the review row's
    /// `origin/<base>`..`origin/<head>` diff reflects the PR as GitHub sees it
    /// (remote-tracking refs are only as fresh as the last fetch). Returns true
    /// when the caller must NOT spawn the review row yet —
    /// [`Self::handle_review_base_fetch_finished`] spawns it when the fetch
    /// lands. A loading box renders in the toast slot meanwhile.
    fn start_review_base_fetch(&mut self, ws_idx: usize) -> bool {
        let Some(ws) = self.state.workspaces.get(ws_idx) else {
            return false;
        };
        let Some(pr) = ws.reviewing_pr_active() else {
            return false; // not a PR review: spawn against the local base as usual
        };
        if let Some(fetch) = &self.state.control.review_base_fetch {
            // One fetch at a time. Same workspace: its handler will spawn the
            // row, nothing more to do. Another workspace's fetch holds the
            // slot: fall through to an unfetched spawn rather than dropping
            // the row on the floor.
            return fetch.workspace_id == ws.id;
        }
        let workspace_id = ws.id.clone();
        let base_branch = pr.base_branch.clone();
        let head_branch = pr.head_branch.clone();
        let repo_root = ws
            .worktree_space()
            .map(|space| space.repo_root.clone())
            .unwrap_or_else(|| ws.identity_cwd.clone());
        self.state.control.review_base_fetch =
            Some(crate::app::state::ReviewBaseFetchState {
                workspace_id: workspace_id.clone(),
                pr_number: pr.number,
                base_branch: base_branch.clone(),
            });
        tracing::info!(pr = pr.number, base = %base_branch, head = %head_branch, "starting review-refs fetch");
        let event_tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let result = match std::process::Command::new("git")
                .current_dir(&repo_root)
                .args(["fetch", "origin", &base_branch, &head_branch])
                .output()
            {
                Ok(out) if out.status.success() => Ok(()),
                Ok(out) => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
                Err(err) => Err(format!("git not available: {err}")),
            };
            let _ = event_tx.blocking_send(crate::events::AppEvent::ReviewBaseFetchFinished(
                crate::events::ReviewBaseFetchResult {
                    workspace_id,
                    result,
                },
            ));
        });
        true
    }

    /// Background review-base fetch completed: clear the loading state and
    /// spawn the review row that was waiting on it. A failed fetch still opens
    /// the row — a possibly-stale `origin/<base>` beats no review — but says
    /// so in a toast.
    pub(crate) fn handle_review_base_fetch_finished(
        &mut self,
        result: crate::events::ReviewBaseFetchResult,
    ) {
        let matches = self
            .state
            .control
            .review_base_fetch
            .as_ref()
            .is_some_and(|fetch| fetch.workspace_id == result.workspace_id);
        if !matches {
            return; // stale result; some newer fetch owns the slot
        }
        let fetch = self
            .state
            .control
            .review_base_fetch
            .take()
            .expect("checked above");
        if let Err(err) = result.result {
            tracing::warn!(base = %fetch.base_branch, error = %err, "review-refs fetch failed");
            self.state.set_home_toast(
                "fetch failed",
                format!(
                    "origin/{} and the PR head may be stale: {err}",
                    fetch.base_branch
                ),
            );
        }
        let ws_idx = self
            .state
            .workspaces
            .iter()
            .position(|ws| ws.id == result.workspace_id);
        if let Some(ws_idx) = ws_idx {
            // Skip when a review row appeared meanwhile (e.g. re-attached);
            // spawning another would stack a duplicate row.
            let already_open = self
                .state
                .workspaces
                .get(ws_idx)
                .and_then(|ws| ws.pane_with_role(crate::pane::PaneRole::Review))
                .is_some();
            if !already_open {
                self.spawn_fresh_row(ws_idx, crate::pane::PaneRole::Review);
            }
        }
        self.render_dirty
            .store(true, std::sync::atomic::Ordering::Release);
        self.render_notify.notify_one();
    }

    /// Whether the active workspace's REVIEW row is the currently focused pane.
    /// Gates `alt+g` so the fix command only fires while reviewing.
    pub(crate) fn review_pane_focused(&self) -> bool {
        let Some(ws_idx) = self.state.active else {
            return false;
        };
        let Some(ws) = self.state.workspaces.get(ws_idx) else {
            return false;
        };
        let focused = ws.focused_pane_id();
        focused.is_some() && focused == ws.pane_with_role(crate::pane::PaneRole::Review)
    }

    /// alt+g: hand the review-row context to the active workspace's agent.
    /// Writes a prompt into the agent (root) pane and submits it with Enter.
    ///
    /// On the user's own branch, the prompt asks the agent to fix every
    /// `CLAUDE:` comment in the branch diff. While the workspace is reviewing
    /// someone else's PR (see [`crate::workspace::Workspace::reviewing_pr_active`]),
    /// it instead asks the agent to turn those `CLAUDE:` notes into PR review
    /// comments — drafted together with the user, then submitted via `gh`.
    ///
    /// Refuses (with a toast) when the agent already has a prompt typed in, since
    /// our text would otherwise be concatenated onto it and the merged line
    /// submitted. See [`Self::agent_prompt_busy`].
    pub(crate) fn send_claude_fix_command(&mut self) {
        let Some(ws_idx) = self.state.active else {
            return;
        };
        // Pull everything we need out of the workspace first, then drop the
        // borrow so we can call the runtime + toast.
        let Some(ws) = self.state.workspaces.get(ws_idx) else {
            return;
        };
        let repo_root = ws
            .worktree_space()
            .map(|s| s.repo_root.clone())
            .unwrap_or_else(|| ws.identity_cwd.clone());
        let Some(branch) = ws.branch() else {
            self.state.set_home_toast("fix failed", "agent has no branch");
            return;
        };
        let Some(agent_pane) = ws.agent_pane() else {
            self.state.set_home_toast("fix failed", "no agent pane");
            return;
        };
        let reviewing = ws.reviewing_pr_active().cloned();
        // Match the review row's diff base (see `row_spawn_spec`).
        let base = match &reviewing {
            Some(pr) => format!("origin/{}", pr.base_branch),
            None => crate::workspace::review_base(&repo_root, &branch),
        };

        // Guard: don't clobber a half-typed prompt.
        if self.agent_prompt_busy(ws_idx, agent_pane) {
            self.state.set_home_toast(
                "fix skipped",
                "agent has a prompt typed — clear it first",
            );
            return;
        }

        let prompt = match &reviewing {
            Some(pr) => claude_reply_prompt(&base, pr),
            None => claude_fix_prompt(&base),
        };
        let send_result: Result<(), String> = match self.lookup_runtime_sender(ws_idx, agent_pane)
        {
            None => Err("agent not running".to_string()),
            Some(runtime) => {
                let text_bytes = super::api_helpers::encode_api_text(runtime, &prompt);
                runtime
                    .try_send_bytes(bytes::Bytes::from(text_bytes))
                    .map_err(|err| err.to_string())
                    .and_then(|()| {
                        // Submit as a separate Enter event, mirroring the
                        // `pane.send_input` API path.
                        let enter = runtime.encode_terminal_key(
                            crossterm::event::KeyEvent::new(
                                crossterm::event::KeyCode::Enter,
                                crossterm::event::KeyModifiers::empty(),
                            )
                            .into(),
                        );
                        runtime
                            .try_send_bytes(bytes::Bytes::from(enter))
                            .map_err(|err| err.to_string())
                    })
            }
        };
        match send_result {
            Ok(()) => match &reviewing {
                Some(pr) => self.state.set_home_toast(
                    "replies sent",
                    format!("asked agent to draft replies for PR #{}", pr.number),
                ),
                None => self
                    .state
                    .set_home_toast("fix sent", "asked agent to fix CLAUDE: comments"),
            },
            Err(err) => self.state.set_home_toast("fix failed", err),
        }
    }

    /// Best-effort: does the agent (root) pane look like it already has a prompt
    /// typed in? Reads the cursor's row from the rendered screen and checks for
    /// non-whitespace text between the prompt marker and the cursor. See
    /// [`prompt_has_typed_text`] for the heuristic and its limits.
    fn agent_prompt_busy(&self, ws_idx: usize, agent_pane: crate::layout::PaneId) -> bool {
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, agent_pane) else {
            return false;
        };
        // A 0,0-origin area the size of u16::MAX leaves the returned cursor in
        // raw viewport coordinates (no offset, never clamped out).
        let area = ratatui::layout::Rect {
            x: 0,
            y: 0,
            width: u16::MAX,
            height: u16::MAX,
        };
        let Some(cursor) = runtime.cursor_state(area, true) else {
            return false;
        };
        let text = runtime.visible_text();
        let Some(row) = text.split('\n').nth(cursor.y as usize) else {
            return false;
        };
        prompt_has_typed_text(row, cursor.x)
    }

    /// Build the (argv, cwd) for a freshly-spawned review/terminal row in the
    /// active workspace's worktree. Returns `None` (with a toast for review when
    /// the agent has no branch) when the row cannot be spawned.
    fn row_spawn_spec(
        &mut self,
        ws_idx: usize,
        role: crate::pane::PaneRole,
    ) -> Option<(Vec<String>, PathBuf)> {
        use crate::pane::PaneRole;
        let ws = self.state.workspaces.get(ws_idx)?;
        let repo_root = ws
            .worktree_space()
            .map(|s| s.repo_root.clone())
            .unwrap_or_else(|| ws.identity_cwd.clone());
        let cwd = ws
            .worktree_space()
            .map(|s| s.checkout_path.clone())
            .unwrap_or_else(|| ws.identity_cwd.clone());
        let default_shell = self.state.default_shell.clone();
        match role {
            PaneRole::Terminal => {
                let shell = crate::pane::pane_shell(&default_shell);
                Some((vec![shell], cwd))
            }
            PaneRole::Review => {
                let Some(agent_branch) = ws.branch() else {
                    self.state
                        .set_home_toast("review failed", "agent has no branch");
                    return None;
                };
                let review_cmd = std::env::var("HERDR_REVIEW_CMD")
                    .unwrap_or_else(|_| "vimrev".to_string());
                // While reviewing someone else's PR, diff the PR exactly as
                // GitHub shows it — `origin/<head>` against `origin/<base>` —
                // via the review command's two-ref form, so a stale local
                // checkout can't skew the review. Otherwise diff the worktree
                // against the usual graphite-parent/default-branch base.
                let command_line = match ws.reviewing_pr_active() {
                    Some(pr) => format!(
                        "{review_cmd} {} {}",
                        shell_single_quote(&format!("origin/{}", pr.base_branch)),
                        shell_single_quote(&format!("origin/{}", pr.head_branch)),
                    ),
                    None => {
                        let base = crate::workspace::review_base(&repo_root, &agent_branch);
                        format!("{review_cmd} {}", shell_single_quote(&base))
                    }
                };
                let shell = crate::pane::pane_shell(&default_shell);
                Some((vec![shell, "-ic".to_string(), command_line], cwd))
            }
            PaneRole::Agent => None,
        }
    }

    fn agent_info(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<crate::api::schema::AgentInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane_state = ws.pane_state(pane_id)?;
        let terminal = self.state.terminals.get(&pane_state.attached_terminal_id)?;
        if !terminal.is_agent_terminal() {
            return None;
        }
        let pane = self.pane_info(ws_idx, pane_id)?;
        Some(crate::api::schema::AgentInfo {
            terminal_id: pane.terminal_id,
            name: terminal.agent_name.clone(),
            agent: pane.agent,
            title: pane.title,
            display_agent: pane.display_agent,
            agent_status: pane.agent_status,
            custom_status: pane.custom_status,
            working_seconds: pane.working_seconds,
            state_labels: pane.state_labels,
            agent_session: pane.agent_session,
            workspace_id: pane.workspace_id,
            tab_id: pane.tab_id,
            pane_id: pane.pane_id,
            focused: pane.focused,
            cwd: pane.cwd,
            foreground_cwd: pane.foreground_cwd,
            revision: pane.revision,
        })
    }

    fn agent_name_conflicts(
        &self,
        name: &str,
        except_terminal_id: &str,
    ) -> Vec<crate::api::schema::AgentInfo> {
        self.collect_agent_infos()
            .into_iter()
            .filter(|agent| {
                agent.name.as_deref() == Some(name) && agent.terminal_id != except_terminal_id
            })
            .collect()
    }
}

/// Wrap a value in single quotes for safe interpolation into a shell command.
fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Prompt-line markers an agent renders before its input (`>` for Claude Code,
/// `❯` for shells/other TUIs). Used to locate the typed region of the input.
const PROMPT_MARKERS: [char; 2] = ['>', '❯'];

/// The prompt sent to the agent by `alt+g`. `base` is the diff base the review
/// row uses, embedded so the agent targets exactly what's under review.
fn claude_fix_prompt(base: &str) -> String {
    format!(
        "Review the diff in this branch against its base (`git diff {base}...HEAD`). \
Find every comment that starts with `CLAUDE:` and fix the code according to that \
comment. After applying each fix, remove the `CLAUDE:` comment."
    )
}

/// The prompt sent by `alt+g` while the workspace is reviewing someone else's
/// PR: instead of fixing the code, turn the user's `CLAUDE:` review notes into
/// PR review comments — drafted together with the user, then submitted via
/// `gh` — and clean the notes out of the worktree afterwards.
fn claude_reply_prompt(base: &str, pr: &crate::workspace::ReviewPr) -> String {
    format!(
        "I'm reviewing PR #{number} ({url}) by {author}. This branch is theirs — do NOT \
change their code. My review notes are comments starting with `CLAUDE:` that I added to \
the diff against the PR base (`git diff {base}...HEAD`).\n\
1. Collect every `CLAUDE:` note, with its file and line in the PR's diff.\n\
2. Fetch the PR's existing review threads: \
`gh api repos/{{owner}}/{{repo}}/pulls/{number}/comments` (resolve {{owner}}/{{repo}} from \
`gh repo view --json owner,name`).\n\
3. For each note, draft a review comment — or a reply when it belongs on an existing \
thread — and show me all drafts. Iterate on them with me; do NOT submit anything until I \
explicitly approve.\n\
4. Once I approve, submit the inline comments as a single review: \
`gh api -X POST repos/{{owner}}/{{repo}}/pulls/{number}/reviews` with `\"event\": \"COMMENT\"` \
and a `comments` array of `{{path, line, side, body}}` entries; post thread replies via \
`gh api -X POST repos/{{owner}}/{{repo}}/pulls/{number}/comments/<comment_id>/replies`.\n\
5. After submitting, remove my `CLAUDE:` notes from the worktree (e.g. `git restore` the \
touched files or delete just those comment lines) so the checkout is clean.",
        number = pr.number,
        url = pr.url,
        author = pr.author,
    )
}

/// Best-effort heuristic: does the cursor's row (`row`) already contain
/// user-typed text to the LEFT of the cursor (`cursor_col`)?
///
/// We look at the slice between the last prompt marker (`>` / `❯`) before the
/// cursor and the cursor column. Placeholder hints render to the RIGHT of the
/// cursor, so they're naturally excluded. Returns `false` when there's no marker
/// before the cursor (agent mid-generation, or an input layout we don't
/// recognise) — we'd rather occasionally proceed than wrongly refuse on an empty
/// prompt. Tuned to Claude Code's input box; multi-line input and other agents'
/// layouts may not be detected.
fn prompt_has_typed_text(row: &str, cursor_col: u16) -> bool {
    let before: Vec<char> = row.chars().take(cursor_col as usize).collect();
    let Some(marker_idx) = before.iter().rposition(|c| PROMPT_MARKERS.contains(c)) else {
        return false;
    };
    before[marker_idx + 1..].iter().any(|c| !c.is_whitespace())
}

#[cfg(test)]
mod claude_fix_tests {
    use super::{claude_fix_prompt, claude_reply_prompt, prompt_has_typed_text};

    #[test]
    fn prompt_embeds_base_and_instructions() {
        let p = claude_fix_prompt("origin/master");
        assert!(p.contains("git diff origin/master...HEAD"));
        assert!(p.contains("CLAUDE:"));
        assert!(p.contains("remove"));
    }

    #[test]
    fn reply_prompt_embeds_pr_and_collaboration_contract() {
        let pr = crate::workspace::ReviewPr {
            number: 412,
            title: "Fix parser".to_string(),
            author: "alice".to_string(),
            head_branch: "alice/fix-parser".to_string(),
            base_branch: "master".to_string(),
            url: "https://github.com/acme/proj/pull/412".to_string(),
            graph_prefix: String::new(),
        };
        let p = claude_reply_prompt("origin/master", &pr);
        // Targets the PR, diffs the same base as the review row.
        assert!(p.contains("PR #412"));
        assert!(p.contains("https://github.com/acme/proj/pull/412"));
        assert!(p.contains("git diff origin/master...HEAD"));
        // It's someone else's branch: no code changes, drafts need approval.
        assert!(p.contains("do NOT"));
        assert!(p.contains("approve"));
        // Submission goes through gh, against the PR's review endpoints.
        assert!(p.contains("gh api"));
        assert!(p.contains("pulls/412/reviews"));
        assert!(p.contains("CLAUDE:"));
    }

    #[test]
    fn empty_prompt_is_not_busy() {
        // Cursor parked just after "> " in Claude Code's input box.
        let row = "│ > ";
        // cursor at column 4 (one past the space following the marker).
        assert!(!prompt_has_typed_text(row, 4));
    }

    #[test]
    fn typed_text_is_busy() {
        let row = "│ > fix the widget";
        let col = row.chars().count() as u16;
        assert!(prompt_has_typed_text(row, col));
    }

    #[test]
    fn placeholder_to_right_of_cursor_is_not_busy() {
        // Cursor sits right after "> "; the dim placeholder is to its right.
        let row = "│ > Try \"edit a file\"";
        assert!(!prompt_has_typed_text(row, 4));
    }

    #[test]
    fn no_marker_defaults_to_not_busy() {
        // A row with no prompt marker (e.g. agent mid-generation output).
        assert!(!prompt_has_typed_text("compiling crate foo...", 10));
    }

    #[test]
    fn shell_prompt_marker_detected() {
        let row = "❯ ls -la";
        let col = row.chars().count() as u16;
        assert!(prompt_has_typed_text(row, col));
    }
}

#[cfg(test)]
mod review_base_fetch_tests {
    use super::App;

    fn app() -> App {
        App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        )
    }

    fn pr() -> crate::workspace::ReviewPr {
        crate::workspace::ReviewPr {
            number: 412,
            title: "Fix parser".to_string(),
            author: "alice".to_string(),
            head_branch: "alice/fix-parser".to_string(),
            base_branch: "master".to_string(),
            url: "https://github.com/acme/proj/pull/412".to_string(),
            graph_prefix: String::new(),
        }
    }

    #[test]
    fn toggling_the_review_row_defers_the_spawn_to_the_base_fetch() {
        let mut app = app();
        let mut ws = crate::workspace::Workspace::test_new("main");
        // Point the fetch at a directory that is not a git repo so the
        // background `git fetch` fails fast instead of touching the network.
        ws.identity_cwd = std::env::temp_dir();
        ws.cached_git_branch = Some(pr().head_branch);
        ws.reviewing_pr = Some(pr());
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);

        app.toggle_review_row();

        let fetch = app
            .state
            .control
            .review_base_fetch
            .as_ref()
            .expect("fetch must be in flight");
        assert_eq!(fetch.pr_number, 412);
        assert_eq!(fetch.base_branch, "master");
        // The row spawn waits for the fetch handler.
        assert!(app.state.workspaces[0]
            .pane_with_role(crate::pane::PaneRole::Review)
            .is_none());

        // A second toggle while the fetch is in flight must not spawn either.
        app.toggle_review_row();
        assert!(app.state.workspaces[0]
            .pane_with_role(crate::pane::PaneRole::Review)
            .is_none());
    }

    #[test]
    fn review_row_diffs_the_remote_pr_refs_while_reviewing() {
        let mut app = app();
        let mut ws = crate::workspace::Workspace::test_new("main");
        ws.cached_git_branch = Some(pr().head_branch);
        ws.reviewing_pr = Some(pr());
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);

        let (argv, _cwd) = app
            .row_spawn_spec(0, crate::pane::PaneRole::Review)
            .expect("review row must spawn");
        let command_line = argv.last().expect("argv has a command line");
        // origin/<base> then origin/<head> — the review command's
        // two-ref (parent, tip) form, independent of the local checkout.
        assert!(
            command_line.ends_with("'origin/master' 'origin/alice/fix-parser'"),
            "unexpected review command: {command_line}"
        );
    }

    #[test]
    fn failed_fetch_clears_the_loading_state_and_toasts() {
        let mut app = app();
        app.state.control.review_base_fetch =
            Some(crate::app::state::ReviewBaseFetchState {
                workspace_id: "gone".into(),
                pr_number: 7,
                base_branch: "main".into(),
            });
        app.handle_review_base_fetch_finished(crate::events::ReviewBaseFetchResult {
            workspace_id: "gone".into(),
            result: Err("no network".into()),
        });
        assert!(app.state.control.review_base_fetch.is_none());
        let toast = app.state.toast.expect("failure must surface in a toast");
        assert_eq!(toast.title, "fetch failed");
        assert!(toast.context.contains("origin/main"));
    }

    #[test]
    fn fetch_result_for_another_workspace_is_ignored() {
        let mut app = app();
        app.state.control.review_base_fetch =
            Some(crate::app::state::ReviewBaseFetchState {
                workspace_id: "current".into(),
                pr_number: 7,
                base_branch: "main".into(),
            });
        app.handle_review_base_fetch_finished(crate::events::ReviewBaseFetchResult {
            workspace_id: "stale".into(),
            result: Ok(()),
        });
        // The in-flight fetch still owns the slot; nothing toasted.
        assert!(app.state.control.review_base_fetch.is_some());
        assert!(app.state.toast.is_none());
    }
}

pub(super) enum AgentStartError {
    InvalidName,
    EmptyArgv,
    TargetNotFound {
        target: String,
    },
    PlacementConflict,
    SpawnFailed(String),
    DuplicateName {
        name: String,
        candidates: Vec<crate::api::schema::AgentInfo>,
    },
}

pub(super) enum AgentRenameError {
    Target(TerminalTargetError),
    DuplicateName {
        name: String,
        candidates: Vec<crate::api::schema::AgentInfo>,
    },
}
