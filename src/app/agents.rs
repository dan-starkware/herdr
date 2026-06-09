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
                // Surface the new agent in Main while staying in the home shell.
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
        self.state.control.reset_create_form();
        self.state.mode = Mode::Home;
        self.state.name_input.clear();
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
            // The picker is not a text input, so plain `p` (or alt+p) submits a PR.
            KeyCode::Char('p') => self.submit_pr_for_review(),
            // Enter picks the base branch and moves to the name form. alt+Enter
            // does the same but pre-checks "create a new branch".
            KeyCode::Enter => {
                let new_branch = key.modifiers.contains(KeyModifiers::ALT);
                self.pick_branch_for_create(new_branch);
            }
            _ => {}
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

        // First open: build argv + cwd and spawn a fresh row.
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

    /// alt+g: ask the active workspace's agent to fix every `CLAUDE:` comment in
    /// the branch diff. Writes a prompt into the agent (root) pane and submits
    /// it with Enter.
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
        let base = crate::workspace::review_base(&repo_root, &branch);

        // Guard: don't clobber a half-typed prompt.
        if self.agent_prompt_busy(ws_idx, agent_pane) {
            self.state.set_home_toast(
                "fix skipped",
                "agent has a prompt typed — clear it first",
            );
            return;
        }

        let prompt = claude_fix_prompt(&base);
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
            Ok(()) => self
                .state
                .set_home_toast("fix sent", "asked agent to fix CLAUDE: comments"),
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
                let base = crate::workspace::review_base(&repo_root, &agent_branch);
                let review_cmd = std::env::var("HERDR_REVIEW_CMD")
                    .unwrap_or_else(|_| "vimrev".to_string());
                let command_line = format!("{review_cmd} {}", shell_single_quote(&base));
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
    use super::{claude_fix_prompt, prompt_has_typed_text};

    #[test]
    fn prompt_embeds_base_and_instructions() {
        let p = claude_fix_prompt("origin/master");
        assert!(p.contains("git diff origin/master...HEAD"));
        assert!(p.contains("CLAUDE:"));
        assert!(p.contains("remove"));
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
