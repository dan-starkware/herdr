use std::path::PathBuf;

use super::{terminal_targets::TerminalTargetError, App, Mode};
use crate::api::schema::{AgentStartParams, SplitDirection};

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
        extra_env: Vec<(String, String)>,
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
                extra_env,
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
                extra_env,
                focus,
            )?
        } else if self.state.workspaces.is_empty() {
            self.spawn_agent_workspace(cwd, rows, cols, &argv, extra_env, focus)?
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
                extra_env,
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
        extra_env: Vec<(String, String)>,
        focus: bool,
    ) -> Result<(usize, usize, crate::layout::PaneId), AgentStartError> {
        let (ws, terminal, runtime) = crate::workspace::Workspace::new_argv_command_with_extra_env(
            cwd,
            rows,
            cols,
            argv,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
            self.event_tx.clone(),
            self.render_notify.clone(),
            self.render_dirty.clone(),
            extra_env,
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
        extra_env: Vec<(String, String)>,
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
                    extra_env,
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
            screen_detection_skipped: terminal.full_lifecycle_hook_authority_active(),
            custom_status: pane.custom_status,
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

    /// Pick a default agent name `<repo>-<n>`, scanning `n` upward from one for
    /// the first that is free — no existing checkout directory and no live agent
    /// of that name.
    fn default_agent_worktree_name(&self, repo_label: &str) -> String {
        let mut index = 1usize;
        loop {
            let candidate = format!("{repo_label}-{index}");
            let path = crate::worktree::default_checkout_path(
                &self.state.worktree_directory,
                repo_label,
                &candidate,
            );
            let name_taken = !self.agent_name_conflicts(&candidate, "").is_empty();
            if !path.exists() && !name_taken {
                return candidate;
            }
            index += 1;
        }
    }

    /// Create a fresh git worktree for the repository backing workspace `ws_idx`
    /// and launch an agent in it as its own workspace. The worktree is a clean
    /// checkout on a new branch off `HEAD`, isolating the agent's edits; the
    /// configured gitignored paths are symlinked in so the agent inherits local
    /// setup git does not track. Diagnostics surface through `config_diagnostic`.
    pub(crate) fn create_agent_in_worktree(&mut self, ws_idx: usize) {
        let space = match self.worktree_source_metadata(ws_idx) {
            Ok((_, space, _, _)) => space,
            Err(err) => {
                self.set_transient_diagnostic(err);
                return;
            }
        };
        let repo = crate::workspace::Repository {
            key: space.key.clone(),
            root: space.repo_root.clone(),
            label: space.label.clone(),
        };
        // Quick path (context menu): a new branch named after the agent, off
        // HEAD, running the configured default agent.
        let argv = self.state.agent_worktree_command.clone();
        self.create_agent_in_worktree_for(
            &repo,
            AgentBranchSpec::NewFromAgentName {
                base: "HEAD".into(),
            },
            argv,
        );
    }

    /// Create a worktree for `repo` per `branch`, launch `argv` in it as its own
    /// workspace, and tag it for kill-time cleanup. Diagnostics surface through
    /// `config_diagnostic`.
    pub(crate) fn create_agent_in_worktree_for(
        &mut self,
        repo: &crate::workspace::Repository,
        branch: AgentBranchSpec,
        argv: Vec<String>,
    ) {
        let name = self.default_agent_worktree_name(&repo.label);
        let checkout_path = crate::worktree::default_checkout_path(
            &self.state.worktree_directory,
            &repo.label,
            &name,
        );

        if let Some(parent) = checkout_path.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                self.set_transient_diagnostic(format!(
                    "create agent: could not create worktree dir: {err}"
                ));
                return;
            }
        }

        let command = match &branch {
            AgentBranchSpec::Existing(branch_name) => {
                // A branch can only be checked out by one worktree at a time. The
                // default branch is almost always already checked out by the
                // primary worktree, so base a fresh agent branch on it instead of
                // failing the worktree add.
                if matches!(
                    crate::worktree::branch_checked_out_anywhere(&repo.root, branch_name),
                    Ok(true)
                ) {
                    crate::worktree::build_worktree_add_new_branch_command(
                        &repo.root,
                        &checkout_path,
                        &name,
                        branch_name,
                    )
                } else {
                    crate::worktree::build_worktree_add_existing_branch_command(
                        &repo.root,
                        &checkout_path,
                        branch_name,
                    )
                }
            }
            AgentBranchSpec::New {
                name: branch_name,
                base,
            } => crate::worktree::build_worktree_add_new_branch_command(
                &repo.root,
                &checkout_path,
                branch_name,
                base,
            ),
            AgentBranchSpec::NewFromAgentName { base } => {
                crate::worktree::build_worktree_add_new_branch_command(
                    &repo.root,
                    &checkout_path,
                    &name,
                    base,
                )
            }
        };
        if let Err(err) = crate::worktree::run_worktree_command(&command) {
            tracing::warn!(error = %err, "create-agent worktree add failed");
            self.set_transient_diagnostic(format!("create agent failed: {err}"));
            return;
        }

        // Inherit local gitignored setup (build config, env files, ...).
        let failures = crate::worktree::symlink_agent_paths(
            &repo.root,
            &checkout_path,
            &self.state.agent_worktree_symlink_paths,
        );
        for (path, err) in &failures {
            tracing::warn!(path, error = %err, "agent worktree symlink failed");
        }

        let space = crate::workspace::GitSpaceMetadata {
            key: repo.key.clone(),
            checkout_key: repo.root.display().to_string(),
            label: repo.label.clone(),
            repo_root: repo.root.clone(),
            is_linked_worktree: false,
        };
        self.finish_create_agent_in_worktree(&space, &checkout_path, name, argv);
    }

    /// Spawn `argv` in the freshly created worktree as its own workspace and tag
    /// it with worktree membership so kill-time cleanup can find it.
    fn finish_create_agent_in_worktree(
        &mut self,
        space: &crate::workspace::GitSpaceMetadata,
        checkout_path: &std::path::Path,
        name: String,
        argv: Vec<String>,
    ) {
        if argv.is_empty() {
            self.set_transient_diagnostic("create agent: no agent command selected".into());
            return;
        }
        let (rows, cols) = self.state.estimate_pane_size();
        match self.spawn_agent_workspace(
            checkout_path.to_path_buf(),
            rows,
            cols,
            &argv,
            Vec::new(),
            true,
        ) {
            Ok((spawned_idx, _tab, pane_id)) => {
                if let Some(ws) = self.state.workspaces.get_mut(spawned_idx) {
                    ws.set_custom_name(name.clone());
                    ws.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
                        key: space.key.clone(),
                        label: space.label.clone(),
                        repo_root: space.repo_root.clone(),
                        checkout_path: checkout_path.to_path_buf(),
                        is_linked_worktree: true,
                    });
                }
                if let Some(workspace_id) = self
                    .state
                    .workspaces
                    .get(spawned_idx)
                    .map(|ws| ws.id.clone())
                {
                    // Bind the worktree's lifecycle to this agent so closing it
                    // prompts to remove the checkout.
                    self.state.agent_worktree_workspace_ids.insert(workspace_id);
                }
                if let Some(terminal_id) = self
                    .state
                    .workspaces
                    .get(spawned_idx)
                    .and_then(|ws| ws.terminal_id(pane_id))
                    .cloned()
                {
                    if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
                        // The worktree slug labels the pane border via the
                        // manual label, but it is deliberately not claimed as
                        // the agent name: doing so shadows the detected agent
                        // type ("claude", "codex", ...) in the agents panel,
                        // where the workspace name already carries the slug.
                        terminal.set_manual_label(name);
                    }
                }
                self.state.mark_session_dirty();
            }
            Err(err) => {
                let body = self.agent_start_error_body(err);
                tracing::warn!(error = %body.message, "create-agent spawn failed");
                self.set_transient_diagnostic(format!("create agent failed: {}", body.message));
            }
        }
    }
}

/// Which branch a worktree agent should be created on.
pub(crate) enum AgentBranchSpec {
    /// Check out an existing branch in the worktree.
    Existing(String),
    /// Create a new branch `name` off `base`.
    New { name: String, base: String },
    /// Create a new branch named after the generated agent name, off `base`.
    NewFromAgentName { base: String },
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
