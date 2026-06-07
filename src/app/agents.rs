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
        self.state.mode = Mode::Terminal;
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
            self.state.mode = Mode::Terminal;
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
            self.state.mode = Mode::Terminal;
        }
        self.schedule_session_save();
        Ok((ws_idx, result.0, result.1.pane_id))
    }

    /// Handle a key while in [`Mode::CreateAgent`] (the new-agent name form).
    pub(super) fn handle_create_agent_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};
        match key.code {
            KeyCode::Esc => {
                self.state.mode = Mode::Home;
                self.state.name_input.clear();
            }
            KeyCode::Enter => self.submit_create_agent(),
            KeyCode::Backspace => {
                self.state.name_input.pop();
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

    /// Create a worktree for the selected repository and launch an agent in it.
    /// The worktree name doubles as the workspace title.
    fn submit_create_agent(&mut self) {
        let Some(repo) = self.state.control.selected_repository().cloned() else {
            self.state.mode = Mode::Home;
            return;
        };
        let name = self.state.name_input.trim().to_string();
        if name.is_empty() {
            // Keep the form open until a name is provided.
            return;
        }

        let branch = format!("worktree/{}", crate::worktree::branch_to_path_slug(&name));
        let checkout_path = crate::worktree::default_checkout_path(
            &self.state.worktree_directory,
            &repo.label,
            &branch,
        );
        let command = crate::worktree::build_worktree_add_new_branch_command(
            &repo.root,
            &checkout_path,
            &branch,
            "HEAD",
        );
        if let Err(err) = crate::worktree::run_worktree_command(&command) {
            tracing::warn!(error = %err, "create-agent worktree add failed");
            self.state.set_home_toast("create agent failed", err);
            self.state.mode = Mode::Home;
            return;
        }

        let argv: Vec<String> = CREATE_AGENT_DEFAULT_ARGV
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (rows, cols) = self.state.estimate_pane_size();
        match self.spawn_agent_workspace(checkout_path.clone(), rows, cols, &argv, true) {
            Ok((ws_idx, _tab, pane_id)) => {
                if let Some(ws) = self.state.workspaces.get_mut(ws_idx) {
                    ws.set_custom_name(name.clone());
                    if let Some(meta) = crate::workspace::git_space_metadata(&checkout_path) {
                        ws.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
                            key: meta.key,
                            label: repo.label.clone(),
                            repo_root: repo.root.clone(),
                            checkout_path: checkout_path.clone(),
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
        self.state.mode = Mode::Home;
        self.state.name_input.clear();
    }

    /// Handle a key while in [`Mode::Review`] (the branch picker).
    pub(super) fn handle_review_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Esc => {
                self.state.mode = Mode::Home;
                self.state.control.review = None;
            }
            KeyCode::Up => self.state.review_move_selection(-1),
            KeyCode::Down => self.state.review_move_selection(1),
            // The picker is not a text input, so plain `p` (or alt+p) submits a PR.
            KeyCode::Char('p') => self.submit_pr_for_review(),
            KeyCode::Enter => self.open_review_branch(),
            _ => {}
        }
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

    /// Open `vimrev` on the selected branch in a per-repo detached review
    /// worktree, diffing against the branch's Graphite parent. The dedicated
    /// review worktree is the only checkout that ever moves, so live agent
    /// worktrees are never disturbed.
    fn open_review_branch(&mut self) {
        let Some(review) = self.state.control.review.as_ref() else {
            self.state.mode = Mode::Home;
            return;
        };
        let Some(branch) = review.branches.get(review.selected) else {
            return;
        };
        let repo = review.repo.clone();
        let branch_name = branch.name.clone();
        let review_path = self
            .state
            .worktree_directory
            .join(format!("{}-review", repo.label));
        let base = crate::workspace::review_base(&repo.root, &branch_name);

        let command = if review_path.exists() {
            crate::worktree::build_checkout_detached_command(&review_path, &branch_name)
        } else {
            crate::worktree::build_worktree_add_detached_command(
                &repo.root,
                &review_path,
                &branch_name,
            )
        };
        if let Err(err) = crate::worktree::run_worktree_command(&command) {
            tracing::warn!(error = %err, "review worktree setup failed");
            self.state.set_home_toast("review failed", err);
            self.state.mode = Mode::Home;
            self.state.control.review = None;
            return;
        }

        let argv = vec!["vimrev".to_string(), base];
        let (rows, cols) = self.state.estimate_pane_size();
        match self.spawn_agent_workspace(review_path, rows, cols, &argv, true) {
            Ok((ws_idx, _tab, _pane)) => {
                if let Some(ws) = self.state.workspaces.get_mut(ws_idx) {
                    ws.set_custom_name(format!("review: {branch_name}"));
                }
                self.state.active = Some(ws_idx);
                self.state.selected = ws_idx;
                self.state.control.focus = crate::app::state::FocusPane::Main;
            }
            Err(err) => {
                let body = self.agent_start_error_body(err);
                tracing::warn!(error = %body.message, "review spawn failed");
                self.state.set_home_toast("review failed", body.message);
            }
        }
        self.state.mode = Mode::Home;
        self.state.control.review = None;
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
