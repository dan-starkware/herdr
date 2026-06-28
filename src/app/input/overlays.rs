use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    layout::Rect,
    widgets::{Block, Borders},
};
use tracing::error;

use crate::app::{
    state::{
        AgentChooserState, AppState, BranchChooserState, DragState, DragTarget, Mode,
        NavigatorTarget, NewAgentFlow, RepoChooserIntent, RepoChooserState,
    },
    App,
};

use super::{
    modal::{leave_modal, modal_action_from_buttons, ModalAction},
    ScrollbarClickTarget,
};

fn rect_contains(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height
}

impl App {
    pub(super) fn handle_overlay_mouse(&mut self, mouse: MouseEvent) -> bool {
        if self.state.mode == Mode::ReleaseNotes {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left)
                    if self
                        .state
                        .release_notes_close_button_at(mouse.column, mouse.row) =>
                {
                    self.dismiss_release_notes();
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(target) = self
                        .state
                        .release_notes_scrollbar_target_at(mouse.column, mouse.row)
                    {
                        match target {
                            ScrollbarClickTarget::Thumb { grab_row_offset } => {
                                self.state.drag = Some(DragState {
                                    target: DragTarget::ReleaseNotesScrollbar { grab_row_offset },
                                });
                            }
                            ScrollbarClickTarget::Track { offset_from_bottom } => {
                                self.state
                                    .set_release_notes_offset_from_bottom(offset_from_bottom);
                            }
                        }
                    }
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let Some(DragState {
                        target: DragTarget::ReleaseNotesScrollbar { grab_row_offset },
                    }) = &self.state.drag
                    {
                        if let Some(offset_from_bottom) = self
                            .state
                            .release_notes_offset_for_drag_row(mouse.row, *grab_row_offset)
                        {
                            self.state
                                .set_release_notes_offset_from_bottom(offset_from_bottom);
                        }
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    self.state.drag = None;
                }
                MouseEventKind::ScrollUp => self.scroll_release_notes(-3),
                MouseEventKind::ScrollDown => self.scroll_release_notes(3),
                _ => {}
            }
            return true;
        }

        if self.state.mode == Mode::ProductAnnouncement {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left)
                    if self
                        .state
                        .product_announcement_close_button_at(mouse.column, mouse.row) =>
                {
                    self.dismiss_product_announcement();
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(target) = self
                        .state
                        .product_announcement_scrollbar_target_at(mouse.column, mouse.row)
                    {
                        match target {
                            ScrollbarClickTarget::Thumb { grab_row_offset } => {
                                self.state.drag = Some(DragState {
                                    target: DragTarget::ProductAnnouncementScrollbar {
                                        grab_row_offset,
                                    },
                                });
                            }
                            ScrollbarClickTarget::Track { offset_from_bottom } => self
                                .state
                                .set_product_announcement_offset_from_bottom(offset_from_bottom),
                        }
                    }
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let Some(DragState {
                        target: DragTarget::ProductAnnouncementScrollbar { grab_row_offset },
                    }) = &self.state.drag
                    {
                        if let Some(offset_from_bottom) = self
                            .state
                            .product_announcement_offset_for_drag_row(mouse.row, *grab_row_offset)
                        {
                            self.state
                                .set_product_announcement_offset_from_bottom(offset_from_bottom);
                        }
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    self.state.drag = None;
                }
                MouseEventKind::ScrollUp => self.scroll_product_announcement(-3),
                MouseEventKind::ScrollDown => self.scroll_product_announcement(3),
                _ => {}
            }
            return true;
        }

        if self.state.mode == Mode::Navigator {
            match mouse.kind {
                MouseEventKind::Moved => {
                    if let Some(idx) = self.state.navigator_row_index_at_from(
                        &self.terminal_runtimes,
                        mouse.column,
                        mouse.row,
                    ) {
                        self.state.navigator.selected = idx;
                        self.state
                            .ensure_navigator_selection_visible_from(&self.terminal_runtimes);
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if self
                        .state
                        .navigator_search_contains(mouse.column, mouse.row)
                    {
                        self.state.navigator.search_focused = true;
                    } else if let Some(idx) = self.state.navigator_row_index_at_from(
                        &self.terminal_runtimes,
                        mouse.column,
                        mouse.row,
                    ) {
                        self.state.navigator.selected = idx;
                        let target = self
                            .state
                            .navigator_rows_from(&self.terminal_runtimes)
                            .get(idx)
                            .map(|row| (row.target.clone(), row.is_workspace));
                        if let Some((NavigatorTarget::Workspace { .. }, true)) = target {
                            if self.state.navigator_row_caret_at(mouse.column) {
                                self.state.toggle_selected_navigator_workspace_from(
                                    &self.terminal_runtimes,
                                );
                            } else {
                                self.state
                                    .accept_navigator_selection_from(&self.terminal_runtimes);
                            }
                        } else {
                            self.state
                                .accept_navigator_selection_from(&self.terminal_runtimes);
                        }
                    } else if !self.state.navigator_popup_contains(mouse.column, mouse.row) {
                        leave_modal(&mut self.state);
                    }
                }
                MouseEventKind::ScrollUp => {
                    self.state.navigator.scroll = self.state.navigator.scroll.saturating_sub(3);
                    self.state.navigator.selected = self.state.navigator.scroll;
                    self.state
                        .clamp_navigator_selection_from(&self.terminal_runtimes);
                }
                MouseEventKind::ScrollDown => {
                    let viewport = self.state.navigator_body_rect().height as usize;
                    let max = self
                        .state
                        .navigator_max_scroll_from(&self.terminal_runtimes, viewport);
                    self.state.navigator.scroll =
                        self.state.navigator.scroll.saturating_add(3).min(max);
                    self.state.navigator.selected = self.state.navigator.scroll;
                    self.state
                        .clamp_navigator_selection_from(&self.terminal_runtimes);
                }
                _ => {}
            }
            return true;
        }

        if self.state.mode == Mode::RepoChooser {
            match mouse.kind {
                MouseEventKind::Moved => {
                    if let Some(idx) = self
                        .state
                        .repo_chooser_row_index_at(mouse.column, mouse.row)
                    {
                        self.state.repo_chooser.selected = idx;
                        self.state.ensure_repo_chooser_selection_visible();
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(idx) = self
                        .state
                        .repo_chooser_row_index_at(mouse.column, mouse.row)
                    {
                        self.state.repo_chooser.selected = idx;
                        self.accept_repo_chooser();
                    } else if !self
                        .state
                        .repo_chooser_popup_contains(mouse.column, mouse.row)
                    {
                        leave_modal(&mut self.state);
                    }
                }
                MouseEventKind::ScrollUp => {
                    self.state.repo_chooser.scroll =
                        self.state.repo_chooser.scroll.saturating_sub(3);
                    self.state.repo_chooser.selected = self.state.repo_chooser.scroll;
                    self.state.clamp_repo_chooser_selection();
                }
                MouseEventKind::ScrollDown => {
                    let viewport = self.state.repo_chooser_body_rect().height as usize;
                    let max = self
                        .state
                        .repo_chooser_filtered_indices()
                        .len()
                        .saturating_sub(viewport);
                    self.state.repo_chooser.scroll =
                        self.state.repo_chooser.scroll.saturating_add(3).min(max);
                    self.state.repo_chooser.selected = self.state.repo_chooser.scroll;
                    self.state.clamp_repo_chooser_selection();
                }
                _ => {}
            }
            return true;
        }

        if self.state.mode == Mode::BranchChooser {
            match mouse.kind {
                MouseEventKind::Moved => {
                    if let Some(idx) = self
                        .state
                        .branch_chooser_row_index_at(mouse.column, mouse.row)
                    {
                        self.state.branch_chooser.selected = idx;
                        self.state.ensure_branch_chooser_selection_visible();
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(idx) = self
                        .state
                        .branch_chooser_row_index_at(mouse.column, mouse.row)
                    {
                        self.state.branch_chooser.selected = idx;
                        // Clear any typed query so the highlighted row is chosen.
                        self.state.branch_chooser.query.clear();
                        self.accept_branch_chooser();
                    } else if !self
                        .state
                        .branch_chooser_popup_contains(mouse.column, mouse.row)
                    {
                        self.state.open_new_agent_flow();
                    }
                }
                MouseEventKind::ScrollUp => {
                    self.state.branch_chooser.scroll =
                        self.state.branch_chooser.scroll.saturating_sub(3);
                    self.state.branch_chooser.selected = self.state.branch_chooser.scroll;
                    self.state.clamp_branch_chooser_selection();
                }
                MouseEventKind::ScrollDown => {
                    let viewport = self.state.branch_chooser_body_rect().height as usize;
                    let max = self
                        .state
                        .branch_chooser_filtered_indices()
                        .len()
                        .saturating_sub(viewport);
                    self.state.branch_chooser.scroll =
                        self.state.branch_chooser.scroll.saturating_add(3).min(max);
                    self.state.branch_chooser.selected = self.state.branch_chooser.scroll;
                    self.state.clamp_branch_chooser_selection();
                }
                _ => {}
            }
            return true;
        }

        if self.state.mode == Mode::AgentChooser {
            match mouse.kind {
                MouseEventKind::Moved => {
                    if let Some(idx) = self
                        .state
                        .agent_chooser_row_index_at(mouse.column, mouse.row)
                    {
                        self.state.agent_chooser.selected = idx;
                        self.state.ensure_agent_chooser_selection_visible();
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(idx) = self
                        .state
                        .agent_chooser_row_index_at(mouse.column, mouse.row)
                    {
                        self.state.agent_chooser.selected = idx;
                        self.state.agent_chooser.query.clear();
                        self.accept_agent_chooser();
                    } else if !self
                        .state
                        .agent_chooser_popup_contains(mouse.column, mouse.row)
                    {
                        self.state.open_new_agent_flow();
                    }
                }
                MouseEventKind::ScrollUp => {
                    self.state.agent_chooser.scroll =
                        self.state.agent_chooser.scroll.saturating_sub(3);
                    self.state.agent_chooser.selected = self.state.agent_chooser.scroll;
                    self.state.clamp_agent_chooser_selection();
                }
                MouseEventKind::ScrollDown => {
                    let viewport = self.state.agent_chooser_body_rect().height as usize;
                    let max = self
                        .state
                        .agent_chooser_filtered_indices()
                        .len()
                        .saturating_sub(viewport);
                    self.state.agent_chooser.scroll =
                        self.state.agent_chooser.scroll.saturating_add(3).min(max);
                    self.state.agent_chooser.selected = self.state.agent_chooser.scroll;
                    self.state.clamp_agent_chooser_selection();
                }
                _ => {}
            }
            return true;
        }

        if self.state.mode == Mode::KeybindHelp {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left)
                    if self
                        .state
                        .keybind_help_close_button_at(mouse.column, mouse.row) =>
                {
                    leave_modal(&mut self.state);
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(target) = self
                        .state
                        .keybind_help_scrollbar_target_at(mouse.column, mouse.row)
                    {
                        match target {
                            ScrollbarClickTarget::Thumb { grab_row_offset } => {
                                self.state.drag = Some(DragState {
                                    target: DragTarget::KeybindHelpScrollbar { grab_row_offset },
                                });
                            }
                            ScrollbarClickTarget::Track { offset_from_bottom } => {
                                self.state
                                    .set_keybind_help_offset_from_bottom(offset_from_bottom);
                            }
                        }
                    } else {
                        let rect = self.state.keybind_help_popup_rect();
                        let inside = mouse.column >= rect.x
                            && mouse.column < rect.x + rect.width
                            && mouse.row >= rect.y
                            && mouse.row < rect.y + rect.height;
                        if !inside {
                            leave_modal(&mut self.state);
                        }
                    }
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let Some(DragState {
                        target: DragTarget::KeybindHelpScrollbar { grab_row_offset },
                    }) = &self.state.drag
                    {
                        if let Some(offset_from_bottom) = self
                            .state
                            .keybind_help_offset_for_drag_row(mouse.row, *grab_row_offset)
                        {
                            self.state
                                .set_keybind_help_offset_from_bottom(offset_from_bottom);
                        }
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    self.state.drag = None;
                }
                MouseEventKind::ScrollUp => self.state.scroll_keybind_help(-3),
                MouseEventKind::ScrollDown => self.state.scroll_keybind_help(3),
                _ => {}
            }
            return true;
        }

        false
    }

    pub(crate) fn handle_repo_chooser_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                if self.state.repo_chooser.query.is_empty() {
                    leave_modal(&mut self.state);
                } else {
                    self.state.repo_chooser.query.clear();
                    self.state.clamp_repo_chooser_selection();
                }
            }
            KeyCode::Enter => self.accept_repo_chooser(),
            KeyCode::Up => self.state.move_repo_chooser_selection(-1),
            KeyCode::Down => self.state.move_repo_chooser_selection(1),
            KeyCode::Char('p') if key.modifiers == KeyModifiers::CONTROL => {
                self.state.move_repo_chooser_selection(-1)
            }
            KeyCode::Char('n') if key.modifiers == KeyModifiers::CONTROL => {
                self.state.move_repo_chooser_selection(1)
            }
            KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => {
                self.state.repo_chooser.query.clear();
                self.state.clamp_repo_chooser_selection();
            }
            KeyCode::Backspace => {
                self.state.repo_chooser.query.pop();
                self.state.clamp_repo_chooser_selection();
            }
            KeyCode::Char(c)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.state.repo_chooser.query.push(c);
                self.state.clamp_repo_chooser_selection();
            }
            _ => {}
        }
    }

    pub(super) fn accept_repo_chooser(&mut self) {
        let Some(repo) = self.state.repo_chooser_selected_repo() else {
            return;
        };
        match self.state.repo_chooser.intent {
            RepoChooserIntent::OpenWorkspace => self.open_or_switch_workspace_for_repo(repo),
            RepoChooserIntent::NewAgent => {
                self.state.open_branch_chooser(&repo);
                self.state.new_agent_flow = Some(NewAgentFlow {
                    repo: Some(repo),
                    branch: None,
                });
            }
        }
    }

    pub(crate) fn handle_branch_chooser_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                if self.state.branch_chooser.query.is_empty() {
                    // Back a step to the repo chooser (still in new-agent intent).
                    self.state.open_new_agent_flow();
                } else {
                    self.state.branch_chooser.query.clear();
                    self.state.clamp_branch_chooser_selection();
                }
            }
            KeyCode::Enter => self.accept_branch_chooser(),
            KeyCode::Up => self.state.move_branch_chooser_selection(-1),
            KeyCode::Down => self.state.move_branch_chooser_selection(1),
            KeyCode::Char('p') if key.modifiers == KeyModifiers::CONTROL => {
                self.state.move_branch_chooser_selection(-1)
            }
            KeyCode::Char('n') if key.modifiers == KeyModifiers::CONTROL => {
                self.state.move_branch_chooser_selection(1)
            }
            KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => {
                self.state.branch_chooser.query.clear();
                self.state.clamp_branch_chooser_selection();
            }
            KeyCode::Backspace => {
                self.state.branch_chooser.query.pop();
                self.state.clamp_branch_chooser_selection();
            }
            KeyCode::Char(c)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.state.branch_chooser.query.push(c);
                self.state.clamp_branch_chooser_selection();
            }
            _ => {}
        }
    }

    pub(super) fn accept_branch_chooser(&mut self) {
        let Some(choice) =
            crate::ui::branch_chooser::resolve_branch_choice(&self.state.branch_chooser)
        else {
            return;
        };
        // Carry the branch into the flow and advance to the agent step.
        if let Some(flow) = self.state.new_agent_flow.as_mut() {
            flow.branch = Some(choice);
            self.state.open_agent_chooser();
        } else {
            leave_modal(&mut self.state);
        }
    }

    pub(crate) fn handle_agent_chooser_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                if self.state.agent_chooser.query.is_empty() {
                    // Back a step to the branch chooser.
                    if let Some(repo) = self
                        .state
                        .new_agent_flow
                        .as_ref()
                        .and_then(|flow| flow.repo.clone())
                    {
                        self.state.open_branch_chooser(&repo);
                    } else {
                        leave_modal(&mut self.state);
                    }
                } else {
                    self.state.agent_chooser.query.clear();
                    self.state.clamp_agent_chooser_selection();
                }
            }
            KeyCode::Enter => self.accept_agent_chooser(),
            KeyCode::Up => self.state.move_agent_chooser_selection(-1),
            KeyCode::Down => self.state.move_agent_chooser_selection(1),
            KeyCode::Char('p') if key.modifiers == KeyModifiers::CONTROL => {
                self.state.move_agent_chooser_selection(-1)
            }
            KeyCode::Char('n') if key.modifiers == KeyModifiers::CONTROL => {
                self.state.move_agent_chooser_selection(1)
            }
            KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => {
                self.state.agent_chooser.query.clear();
                self.state.clamp_agent_chooser_selection();
            }
            KeyCode::Backspace => {
                self.state.agent_chooser.query.pop();
                self.state.clamp_agent_chooser_selection();
            }
            KeyCode::Char(c)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.state.agent_chooser.query.push(c);
                self.state.clamp_agent_chooser_selection();
            }
            _ => {}
        }
    }

    pub(super) fn accept_agent_chooser(&mut self) {
        let Some(agent) = self.state.agent_chooser_selected_agent() else {
            return;
        };
        let flow = self.state.new_agent_flow.take();
        leave_modal(&mut self.state);
        let Some(flow) = flow else {
            return;
        };
        let (Some(repo), Some(branch)) = (flow.repo, flow.branch) else {
            return;
        };
        let spec = match branch {
            crate::ui::branch_chooser::BranchChoice::Existing(name) => {
                crate::app::agents::AgentBranchSpec::Existing(name)
            }
            crate::ui::branch_chooser::BranchChoice::New { name, base } => {
                crate::app::agents::AgentBranchSpec::New { name, base }
            }
            crate::ui::branch_chooser::BranchChoice::StackOnto { base } => {
                crate::app::agents::AgentBranchSpec::NewFromAgentName { base }
            }
        };
        self.create_agent_in_worktree_for(&repo, spec, vec![agent]);
    }

    /// Open the picked repository: reuse an existing workspace already rooted at
    /// it (matched by git-common-dir key), otherwise spawn a fresh workspace
    /// there.
    fn open_or_switch_workspace_for_repo(&mut self, repo: crate::workspace::Repository) {
        let existing = self.state.workspaces.iter().position(|ws| {
            ws.cached_git_space
                .as_ref()
                .is_some_and(|space| space.key == repo.key)
        });
        if let Some(idx) = existing {
            self.state.switch_workspace(idx);
            self.state.mode = Mode::Terminal;
            return;
        }
        if let Err(err) = self.create_workspace_with_options(repo.root.clone(), true) {
            error!(error = %err, root = %repo.root.display(), "repo chooser: open workspace failed");
            leave_modal(&mut self.state);
        }
    }
}

impl AppState {
    pub(super) fn onboarding_full_area(&self) -> Rect {
        self.view.sidebar_rect.union(self.view.terminal_area)
    }

    pub(crate) fn navigator_popup_rect(&self) -> Rect {
        let area = self.onboarding_full_area();
        let margin_x = (area.width / 16).max(2);
        let margin_y = (area.height / 10).max(1);
        let width = area.width.saturating_sub(margin_x.saturating_mul(2));
        let height = area.height.saturating_sub(margin_y.saturating_mul(2));
        Rect::new(
            area.x + margin_x,
            area.y + margin_y,
            width.max(4),
            height.max(4),
        )
    }

    pub(crate) fn navigator_inner_rect(&self) -> Rect {
        Block::default()
            .borders(Borders::ALL)
            .inner(self.navigator_popup_rect())
    }

    pub(crate) fn navigator_search_rect(&self) -> Rect {
        let inner = self.navigator_inner_rect();
        Rect::new(inner.x, inner.y, inner.width, inner.height.min(1))
    }

    pub(crate) fn navigator_body_rect(&self) -> Rect {
        let inner = self.navigator_inner_rect();
        if inner.height <= 4 {
            return Rect::default();
        }
        Rect::new(
            inner.x,
            inner.y + 2,
            inner.width,
            inner.height.saturating_sub(4),
        )
    }

    pub(crate) fn navigator_detail_rect(&self) -> Rect {
        let inner = self.navigator_inner_rect();
        Rect::new(
            inner.x,
            inner.y + inner.height.saturating_sub(2),
            inner.width,
            inner.height.min(1),
        )
    }

    pub(crate) fn navigator_footer_rect(&self) -> Rect {
        let inner = self.navigator_inner_rect();
        Rect::new(
            inner.x,
            inner.y + inner.height.saturating_sub(1),
            inner.width,
            inner.height.min(1),
        )
    }

    pub(crate) fn navigator_popup_contains(&self, col: u16, row: u16) -> bool {
        rect_contains(self.navigator_popup_rect(), col, row)
    }

    pub(crate) fn navigator_search_contains(&self, col: u16, row: u16) -> bool {
        rect_contains(self.navigator_search_rect(), col, row)
    }

    pub(crate) fn navigator_row_index_at_from(
        &self,
        terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
        col: u16,
        row: u16,
    ) -> Option<usize> {
        let body = self.navigator_body_rect();
        if !rect_contains(body, col, row) {
            return None;
        }
        let idx = self
            .navigator
            .scroll
            .saturating_add(row.saturating_sub(body.y) as usize);
        (idx < self.navigator_rows_from(terminal_runtimes).len()).then_some(idx)
    }

    pub(crate) fn navigator_row_caret_at(&self, col: u16) -> bool {
        let body = self.navigator_body_rect();
        col <= body.x.saturating_add(3)
    }

    /// Populate the repo chooser by scanning the configured root, then open it.
    pub(crate) fn open_repo_chooser(&mut self) {
        let repos = crate::workspace::default_scan_root()
            .map(|root| crate::workspace::scan_repositories(&root))
            .unwrap_or_default();
        self.repo_chooser = RepoChooserState {
            repos,
            query: String::new(),
            selected: 0,
            scroll: 0,
            intent: RepoChooserIntent::OpenWorkspace,
        };
        self.mode = Mode::RepoChooser;
    }

    /// Start the new-agent flow: open the repo chooser in `NewAgent` intent so
    /// picking a repo advances to the branch step instead of opening a workspace.
    pub(crate) fn open_new_agent_flow(&mut self) {
        let repos = crate::workspace::default_scan_root()
            .map(|root| crate::workspace::scan_repositories(&root))
            .unwrap_or_default();
        self.repo_chooser = RepoChooserState {
            repos,
            query: String::new(),
            selected: 0,
            scroll: 0,
            intent: RepoChooserIntent::NewAgent,
        };
        self.new_agent_flow = Some(NewAgentFlow {
            repo: None,
            branch: None,
        });
        self.mode = Mode::RepoChooser;
    }

    /// Populate and open the agent chooser (final step): all known agents, with
    /// the configured default agent pre-selected.
    pub(crate) fn open_agent_chooser(&mut self) {
        let agents: Vec<String> = crate::detect::all_agent_labels()
            .into_iter()
            .map(str::to_string)
            .collect();
        let default = self
            .agent_worktree_command
            .first()
            .cloned()
            .unwrap_or_default();
        let selected = agents.iter().position(|a| *a == default).unwrap_or(0);
        self.agent_chooser = AgentChooserState {
            agents,
            query: String::new(),
            selected,
            scroll: 0,
        };
        self.ensure_agent_chooser_selection_visible();
        self.mode = Mode::AgentChooser;
    }

    /// Indices into `agent_chooser.agents` matching the query (case-insensitive
    /// substring); empty query matches all.
    pub(crate) fn agent_chooser_filtered_indices(&self) -> Vec<usize> {
        let query = self.agent_chooser.query.trim().to_ascii_lowercase();
        self.agent_chooser
            .agents
            .iter()
            .enumerate()
            .filter(|(_, agent)| query.is_empty() || agent.to_ascii_lowercase().contains(&query))
            .map(|(idx, _)| idx)
            .collect()
    }

    /// The agent label at the current selection within the filtered rows.
    pub(crate) fn agent_chooser_selected_agent(&self) -> Option<String> {
        let idx = *self
            .agent_chooser_filtered_indices()
            .get(self.agent_chooser.selected)?;
        self.agent_chooser.agents.get(idx).cloned()
    }

    pub(crate) fn move_agent_chooser_selection(&mut self, delta: isize) {
        let count = self.agent_chooser_filtered_indices().len();
        if count == 0 {
            self.agent_chooser.selected = 0;
            self.agent_chooser.scroll = 0;
            return;
        }
        let current = self.agent_chooser.selected.min(count - 1) as isize;
        self.agent_chooser.selected = (current + delta).clamp(0, count as isize - 1) as usize;
        self.ensure_agent_chooser_selection_visible();
    }

    pub(crate) fn clamp_agent_chooser_selection(&mut self) {
        let count = self.agent_chooser_filtered_indices().len();
        self.agent_chooser.selected = self.agent_chooser.selected.min(count.saturating_sub(1));
        self.ensure_agent_chooser_selection_visible();
    }

    pub(crate) fn ensure_agent_chooser_selection_visible(&mut self) {
        let viewport = self.agent_chooser_body_rect().height as usize;
        if viewport == 0 {
            self.agent_chooser.scroll = 0;
            return;
        }
        let max_scroll = self
            .agent_chooser_filtered_indices()
            .len()
            .saturating_sub(viewport);
        if self.agent_chooser.selected < self.agent_chooser.scroll {
            self.agent_chooser.scroll = self.agent_chooser.selected;
        } else if self.agent_chooser.selected >= self.agent_chooser.scroll.saturating_add(viewport)
        {
            self.agent_chooser.scroll = self
                .agent_chooser
                .selected
                .saturating_add(1)
                .saturating_sub(viewport);
        }
        self.agent_chooser.scroll = self.agent_chooser.scroll.min(max_scroll);
    }

    // The agent chooser reuses the branch chooser's centered-popup geometry
    // (identical size and layout); these delegate so there is one source of truth.
    pub(crate) fn agent_chooser_popup_rect(&self) -> Rect {
        self.branch_chooser_popup_rect()
    }

    pub(crate) fn agent_chooser_search_rect(&self) -> Rect {
        self.branch_chooser_search_rect()
    }

    pub(crate) fn agent_chooser_body_rect(&self) -> Rect {
        self.branch_chooser_body_rect()
    }

    pub(crate) fn agent_chooser_footer_rect(&self) -> Rect {
        self.branch_chooser_footer_rect()
    }

    pub(crate) fn agent_chooser_popup_contains(&self, col: u16, row: u16) -> bool {
        self.branch_chooser_popup_contains(col, row)
    }

    pub(crate) fn agent_chooser_row_index_at(&self, col: u16, row: u16) -> Option<usize> {
        let body = self.agent_chooser_body_rect();
        if !rect_contains(body, col, row) {
            return None;
        }
        let idx = self
            .agent_chooser
            .scroll
            .saturating_add(row.saturating_sub(body.y) as usize);
        (idx < self.agent_chooser_filtered_indices().len()).then_some(idx)
    }

    /// Populate and open the branch chooser for `repo` (the new agent's repo).
    pub(crate) fn open_branch_chooser(&mut self, repo: &crate::workspace::Repository) {
        let branches = crate::worktree::list_chooser_branches(&repo.root);
        let default_base = crate::worktree::default_base_branch(&repo.root);
        let graphite = crate::worktree::graphite_is_tracked(&repo.root);
        self.branch_chooser = BranchChooserState {
            branches,
            default_base,
            graphite,
            query: String::new(),
            selected: 0,
            scroll: 0,
        };
        self.mode = Mode::BranchChooser;
    }

    /// Indices into `branch_chooser.branches` matching the query, in display
    /// order. Empty query matches all.
    pub(crate) fn branch_chooser_filtered_indices(&self) -> Vec<usize> {
        crate::ui::branch_chooser::filtered_branch_indices(&self.branch_chooser)
    }

    pub(crate) fn move_branch_chooser_selection(&mut self, delta: isize) {
        let count = self.branch_chooser_filtered_indices().len();
        if count == 0 {
            self.branch_chooser.selected = 0;
            self.branch_chooser.scroll = 0;
            return;
        }
        let current = self.branch_chooser.selected.min(count - 1) as isize;
        self.branch_chooser.selected = (current + delta).clamp(0, count as isize - 1) as usize;
        self.ensure_branch_chooser_selection_visible();
    }

    pub(crate) fn clamp_branch_chooser_selection(&mut self) {
        let count = self.branch_chooser_filtered_indices().len();
        self.branch_chooser.selected = self.branch_chooser.selected.min(count.saturating_sub(1));
        self.ensure_branch_chooser_selection_visible();
    }

    pub(crate) fn ensure_branch_chooser_selection_visible(&mut self) {
        let viewport = self.branch_chooser_body_rect().height as usize;
        if viewport == 0 {
            self.branch_chooser.scroll = 0;
            return;
        }
        let max_scroll = self
            .branch_chooser_filtered_indices()
            .len()
            .saturating_sub(viewport);
        if self.branch_chooser.selected < self.branch_chooser.scroll {
            self.branch_chooser.scroll = self.branch_chooser.selected;
        } else if self.branch_chooser.selected
            >= self.branch_chooser.scroll.saturating_add(viewport)
        {
            self.branch_chooser.scroll = self
                .branch_chooser
                .selected
                .saturating_add(1)
                .saturating_sub(viewport);
        }
        self.branch_chooser.scroll = self.branch_chooser.scroll.min(max_scroll);
    }

    pub(crate) fn branch_chooser_popup_rect(&self) -> Rect {
        let area = self.onboarding_full_area();
        let margin_x = (area.width / 6).max(2);
        let margin_y = (area.height / 6).max(1);
        let width = area.width.saturating_sub(margin_x.saturating_mul(2));
        let height = area.height.saturating_sub(margin_y.saturating_mul(2));
        Rect::new(
            area.x + margin_x,
            area.y + margin_y,
            width.max(4),
            height.max(4),
        )
    }

    pub(crate) fn branch_chooser_inner_rect(&self) -> Rect {
        Block::default()
            .borders(Borders::ALL)
            .inner(self.branch_chooser_popup_rect())
    }

    pub(crate) fn branch_chooser_search_rect(&self) -> Rect {
        let inner = self.branch_chooser_inner_rect();
        Rect::new(inner.x, inner.y, inner.width, inner.height.min(1))
    }

    pub(crate) fn branch_chooser_body_rect(&self) -> Rect {
        let inner = self.branch_chooser_inner_rect();
        if inner.height <= 3 {
            return Rect::default();
        }
        Rect::new(
            inner.x,
            inner.y + 2,
            inner.width,
            inner.height.saturating_sub(3),
        )
    }

    pub(crate) fn branch_chooser_footer_rect(&self) -> Rect {
        let inner = self.branch_chooser_inner_rect();
        Rect::new(
            inner.x,
            inner.y + inner.height.saturating_sub(1),
            inner.width,
            inner.height.min(1),
        )
    }

    pub(crate) fn branch_chooser_popup_contains(&self, col: u16, row: u16) -> bool {
        rect_contains(self.branch_chooser_popup_rect(), col, row)
    }

    pub(crate) fn branch_chooser_row_index_at(&self, col: u16, row: u16) -> Option<usize> {
        let body = self.branch_chooser_body_rect();
        if !rect_contains(body, col, row) {
            return None;
        }
        let idx = self
            .branch_chooser
            .scroll
            .saturating_add(row.saturating_sub(body.y) as usize);
        (idx < self.branch_chooser_filtered_indices().len()).then_some(idx)
    }

    /// Indices into `repo_chooser.repos` that match the current query, in display
    /// order. An empty query matches every repository.
    pub(crate) fn repo_chooser_filtered_indices(&self) -> Vec<usize> {
        let query = self.repo_chooser.query.trim().to_lowercase();
        self.repo_chooser
            .repos
            .iter()
            .enumerate()
            .filter(|(_, repo)| {
                query.is_empty()
                    || crate::app::state::text_matches_query(&query, &repo.label.to_lowercase())
            })
            .map(|(idx, _)| idx)
            .collect()
    }

    /// The repository at the current selection within the filtered rows.
    pub(crate) fn repo_chooser_selected_repo(&self) -> Option<crate::workspace::Repository> {
        let repo_idx = *self
            .repo_chooser_filtered_indices()
            .get(self.repo_chooser.selected)?;
        self.repo_chooser.repos.get(repo_idx).cloned()
    }

    pub(crate) fn move_repo_chooser_selection(&mut self, delta: isize) {
        let count = self.repo_chooser_filtered_indices().len();
        if count == 0 {
            self.repo_chooser.selected = 0;
            self.repo_chooser.scroll = 0;
            return;
        }
        let current = self.repo_chooser.selected.min(count - 1) as isize;
        self.repo_chooser.selected = (current + delta).clamp(0, count as isize - 1) as usize;
        self.ensure_repo_chooser_selection_visible();
    }

    pub(crate) fn clamp_repo_chooser_selection(&mut self) {
        let count = self.repo_chooser_filtered_indices().len();
        self.repo_chooser.selected = self.repo_chooser.selected.min(count.saturating_sub(1));
        self.ensure_repo_chooser_selection_visible();
    }

    pub(crate) fn ensure_repo_chooser_selection_visible(&mut self) {
        let viewport = self.repo_chooser_body_rect().height as usize;
        if viewport == 0 {
            self.repo_chooser.scroll = 0;
            return;
        }
        let max_scroll = self
            .repo_chooser_filtered_indices()
            .len()
            .saturating_sub(viewport);
        if self.repo_chooser.selected < self.repo_chooser.scroll {
            self.repo_chooser.scroll = self.repo_chooser.selected;
        } else if self.repo_chooser.selected >= self.repo_chooser.scroll.saturating_add(viewport) {
            self.repo_chooser.scroll = self
                .repo_chooser
                .selected
                .saturating_add(1)
                .saturating_sub(viewport);
        }
        self.repo_chooser.scroll = self.repo_chooser.scroll.min(max_scroll);
    }

    pub(crate) fn repo_chooser_popup_rect(&self) -> Rect {
        let area = self.onboarding_full_area();
        let margin_x = (area.width / 6).max(2);
        let margin_y = (area.height / 6).max(1);
        let width = area.width.saturating_sub(margin_x.saturating_mul(2));
        let height = area.height.saturating_sub(margin_y.saturating_mul(2));
        Rect::new(
            area.x + margin_x,
            area.y + margin_y,
            width.max(4),
            height.max(4),
        )
    }

    pub(crate) fn repo_chooser_inner_rect(&self) -> Rect {
        Block::default()
            .borders(Borders::ALL)
            .inner(self.repo_chooser_popup_rect())
    }

    pub(crate) fn repo_chooser_search_rect(&self) -> Rect {
        let inner = self.repo_chooser_inner_rect();
        Rect::new(inner.x, inner.y, inner.width, inner.height.min(1))
    }

    /// The scrollable list area: inner minus the search row, a separator row, and
    /// the footer row.
    pub(crate) fn repo_chooser_body_rect(&self) -> Rect {
        let inner = self.repo_chooser_inner_rect();
        if inner.height <= 3 {
            return Rect::default();
        }
        Rect::new(
            inner.x,
            inner.y + 2,
            inner.width,
            inner.height.saturating_sub(3),
        )
    }

    pub(crate) fn repo_chooser_footer_rect(&self) -> Rect {
        let inner = self.repo_chooser_inner_rect();
        Rect::new(
            inner.x,
            inner.y + inner.height.saturating_sub(1),
            inner.width,
            inner.height.min(1),
        )
    }

    pub(crate) fn repo_chooser_popup_contains(&self, col: u16, row: u16) -> bool {
        rect_contains(self.repo_chooser_popup_rect(), col, row)
    }

    pub(crate) fn repo_chooser_row_index_at(&self, col: u16, row: u16) -> Option<usize> {
        let body = self.repo_chooser_body_rect();
        if !rect_contains(body, col, row) {
            return None;
        }
        let idx = self
            .repo_chooser
            .scroll
            .saturating_add(row.saturating_sub(body.y) as usize);
        (idx < self.repo_chooser_filtered_indices().len()).then_some(idx)
    }

    pub(super) fn onboarding_modal_inner(&self, popup_w: u16, popup_h: u16) -> Option<Rect> {
        let area = self.onboarding_full_area();
        let popup_w = popup_w.min(area.width.saturating_sub(4));
        let popup_h = popup_h.min(area.height.saturating_sub(2));
        if popup_w < 4 || popup_h < 4 {
            return None;
        }
        let popup_x = area.x + (area.width.saturating_sub(popup_w)) / 2;
        let popup_y = area.y + (area.height.saturating_sub(popup_h)) / 2;
        let popup = Rect::new(popup_x, popup_y, popup_w, popup_h);
        Some(Block::default().borders(Borders::ALL).inner(popup))
    }

    fn release_notes_modal_inner(&self) -> Option<Rect> {
        self.onboarding_modal_inner(
            crate::ui::RELEASE_NOTES_MODAL_SIZE.0,
            crate::ui::RELEASE_NOTES_MODAL_SIZE.1,
        )
    }

    fn product_announcement_modal_inner(&self) -> Option<Rect> {
        self.onboarding_modal_inner(
            crate::ui::PRODUCT_ANNOUNCEMENT_MODAL_SIZE.0,
            crate::ui::PRODUCT_ANNOUNCEMENT_MODAL_SIZE.1,
        )
    }

    fn release_notes_close_button_at(&self, col: u16, row: u16) -> bool {
        let Some(inner) = self.release_notes_modal_inner() else {
            return false;
        };
        if inner.height < 4 || inner.width < 12 {
            return false;
        }
        let button =
            crate::ui::release_notes_close_button_rect(Rect::new(inner.x, inner.y, inner.width, 1));
        col >= button.x
            && col < button.x + button.width
            && row >= button.y
            && row < button.y + button.height
    }

    pub(super) fn rename_modal_inner(&self) -> Option<Rect> {
        self.onboarding_modal_inner(56, 7)
    }

    fn release_notes_body_rect(&self) -> Option<Rect> {
        let inner = self.release_notes_modal_inner()?;
        if inner.height < 8 || inner.width < 4 {
            return None;
        }
        Some(crate::ui::modal_stack_areas(inner, 2, 1, 0, 1).content)
    }

    fn release_notes_scroll_metrics(&self) -> Option<crate::pane::ScrollMetrics> {
        let notes = self.release_notes.as_ref()?;
        let body = self.release_notes_body_rect()?;
        let viewport_rows = body.height.max(1) as usize;
        let lines = crate::ui::release_notes_display_lines(
            notes,
            &self.update_install_command,
            &self.palette,
        );

        let rows_for_width = |wrap_width: u16| {
            crate::ui::release_notes_wrapped_line_count(&lines, wrap_width.max(1))
        };

        let full_width = body.width.max(1);
        let mut total_rows = rows_for_width(full_width);
        let wrap_width = if total_rows > viewport_rows && full_width > 1 {
            body.width.saturating_sub(1).max(1)
        } else {
            full_width
        };
        total_rows = rows_for_width(wrap_width);

        let max_offset_from_bottom = total_rows.saturating_sub(viewport_rows);
        Some(crate::pane::ScrollMetrics {
            offset_from_bottom: max_offset_from_bottom.saturating_sub(notes.scroll as usize),
            max_offset_from_bottom,
            viewport_rows,
        })
    }

    pub(crate) fn release_notes_max_scroll(&self) -> u16 {
        self.release_notes_scroll_metrics()
            .map(|metrics| metrics.max_offset_from_bottom as u16)
            .unwrap_or(0)
    }

    fn release_notes_scrollbar_target_at(
        &self,
        col: u16,
        row: u16,
    ) -> Option<ScrollbarClickTarget> {
        let body = self.release_notes_body_rect()?;
        let metrics = self.release_notes_scroll_metrics()?;
        let track = crate::ui::release_notes_scrollbar_rect(body, metrics)?;
        if !(col >= track.x
            && col < track.x + track.width
            && row >= track.y
            && row < track.y + track.height)
        {
            return None;
        }
        if let Some(grab_row_offset) = crate::ui::scrollbar_thumb_grab_offset(metrics, track, row) {
            Some(ScrollbarClickTarget::Thumb { grab_row_offset })
        } else {
            Some(ScrollbarClickTarget::Track {
                offset_from_bottom: crate::ui::scrollbar_offset_from_row(metrics, track, row),
            })
        }
    }

    fn release_notes_offset_for_drag_row(&self, row: u16, grab_row_offset: u16) -> Option<usize> {
        let body = self.release_notes_body_rect()?;
        let metrics = self.release_notes_scroll_metrics()?;
        let track = crate::ui::release_notes_scrollbar_rect(body, metrics)?;
        Some(crate::ui::scrollbar_offset_from_drag_row(
            metrics,
            track,
            row,
            grab_row_offset,
        ))
    }

    fn set_release_notes_offset_from_bottom(&mut self, offset_from_bottom: usize) {
        let max_scroll = self.release_notes_max_scroll() as usize;
        if let Some(notes) = &mut self.release_notes {
            notes.scroll = max_scroll.saturating_sub(offset_from_bottom) as u16;
        }
    }

    fn product_announcement_close_button_at(&self, col: u16, row: u16) -> bool {
        let Some(inner) = self.product_announcement_modal_inner() else {
            return false;
        };
        if inner.height < 4 || inner.width < 12 {
            return false;
        }
        let button =
            crate::ui::release_notes_close_button_rect(Rect::new(inner.x, inner.y, inner.width, 1));
        col >= button.x
            && col < button.x + button.width
            && row >= button.y
            && row < button.y + button.height
    }

    fn product_announcement_body_rect(&self) -> Option<Rect> {
        let inner = self.product_announcement_modal_inner()?;
        if inner.height < 8 || inner.width < 4 {
            return None;
        }
        Some(crate::ui::modal_stack_areas(inner, 2, 1, 0, 1).content)
    }

    fn product_announcement_scroll_metrics(&self) -> Option<crate::pane::ScrollMetrics> {
        let announcement = self.product_announcement.as_ref()?;
        let body = self.product_announcement_body_rect()?;
        let viewport_rows = body.height.max(1) as usize;
        let lines = crate::ui::product_announcement_display_lines(announcement, &self.palette);

        let rows_for_width = |wrap_width: u16| {
            crate::ui::release_notes_wrapped_line_count(&lines, wrap_width.max(1))
        };

        let full_width = body.width.max(1);
        let mut total_rows = rows_for_width(full_width);
        let wrap_width = if total_rows > viewport_rows && full_width > 1 {
            body.width.saturating_sub(1).max(1)
        } else {
            full_width
        };
        total_rows = rows_for_width(wrap_width);

        let max_offset_from_bottom = total_rows.saturating_sub(viewport_rows);
        Some(crate::pane::ScrollMetrics {
            offset_from_bottom: max_offset_from_bottom.saturating_sub(announcement.scroll as usize),
            max_offset_from_bottom,
            viewport_rows,
        })
    }

    pub(crate) fn product_announcement_max_scroll(&self) -> u16 {
        self.product_announcement_scroll_metrics()
            .map(|metrics| metrics.max_offset_from_bottom as u16)
            .unwrap_or(0)
    }

    fn product_announcement_scrollbar_target_at(
        &self,
        col: u16,
        row: u16,
    ) -> Option<ScrollbarClickTarget> {
        let body = self.product_announcement_body_rect()?;
        let metrics = self.product_announcement_scroll_metrics()?;
        let track = crate::ui::release_notes_scrollbar_rect(body, metrics)?;
        if !(col >= track.x
            && col < track.x + track.width
            && row >= track.y
            && row < track.y + track.height)
        {
            return None;
        }
        if let Some(grab_row_offset) = crate::ui::scrollbar_thumb_grab_offset(metrics, track, row) {
            Some(ScrollbarClickTarget::Thumb { grab_row_offset })
        } else {
            Some(ScrollbarClickTarget::Track {
                offset_from_bottom: crate::ui::scrollbar_offset_from_row(metrics, track, row),
            })
        }
    }

    fn product_announcement_offset_for_drag_row(
        &self,
        row: u16,
        grab_row_offset: u16,
    ) -> Option<usize> {
        let body = self.product_announcement_body_rect()?;
        let metrics = self.product_announcement_scroll_metrics()?;
        let track = crate::ui::release_notes_scrollbar_rect(body, metrics)?;
        Some(crate::ui::scrollbar_offset_from_drag_row(
            metrics,
            track,
            row,
            grab_row_offset,
        ))
    }

    fn set_product_announcement_offset_from_bottom(&mut self, offset_from_bottom: usize) {
        let max_scroll = self.product_announcement_max_scroll() as usize;
        if let Some(announcement) = &mut self.product_announcement {
            announcement.scroll = max_scroll.saturating_sub(offset_from_bottom) as u16;
        }
    }

    pub(super) fn handle_onboarding_mouse(&mut self, mouse: MouseEvent) {
        if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            return;
        }

        let Some(inner) = self.onboarding_modal_inner(64, 16) else {
            return;
        };
        let actions = crate::ui::modal_stack_areas(inner, 2, 0, 1, 1)
            .actions
            .unwrap_or_default();
        let button = crate::ui::onboarding_welcome_continue_rect(actions);
        if modal_action_from_buttons(mouse.column, mouse.row, &[(button, ModalAction::Continue)])
            == Some(ModalAction::Continue)
        {
            self.request_complete_onboarding = true;
        }
    }

    pub(super) fn keybind_help_popup_rect(&self) -> Rect {
        crate::ui::centered_popup_rect(self.screen_rect(), 76, 22).unwrap_or_default()
    }

    fn keybind_help_modal_inner(&self) -> Option<Rect> {
        self.onboarding_modal_inner(76, 22)
    }

    fn keybind_help_close_button_at(&self, col: u16, row: u16) -> bool {
        let Some(inner) = self.keybind_help_modal_inner() else {
            return false;
        };
        if inner.height < 4 || inner.width < 12 {
            return false;
        }
        let button =
            crate::ui::release_notes_close_button_rect(Rect::new(inner.x, inner.y, inner.width, 1));
        col >= button.x
            && col < button.x + button.width
            && row >= button.y
            && row < button.y + button.height
    }

    fn keybind_help_body_rect(&self) -> Option<Rect> {
        let inner = self.keybind_help_modal_inner()?;
        if inner.height < 6 || inner.width < 4 {
            return None;
        }
        Some(crate::ui::modal_stack_areas(inner, 2, 1, 0, 1).content)
    }

    fn keybind_help_scroll_metrics(&self) -> Option<crate::pane::ScrollMetrics> {
        let body = self.keybind_help_body_rect()?;
        let viewport_rows = body.height.max(1) as usize;
        let wrap_width = body.width.max(1) as usize;
        let total_rows = crate::ui::keybind_help_lines(self)
            .into_iter()
            .map(|(width, _)| width.max(1).div_ceil(wrap_width))
            .sum::<usize>();
        let max_offset_from_bottom = total_rows.saturating_sub(viewport_rows);
        Some(crate::pane::ScrollMetrics {
            offset_from_bottom: max_offset_from_bottom
                .saturating_sub(self.keybind_help.scroll as usize),
            max_offset_from_bottom,
            viewport_rows,
        })
    }

    fn keybind_help_scrollbar_target_at(&self, col: u16, row: u16) -> Option<ScrollbarClickTarget> {
        let body = self.keybind_help_body_rect()?;
        let metrics = self.keybind_help_scroll_metrics()?;
        let track = crate::ui::release_notes_scrollbar_rect(body, metrics)?;
        if !(col >= track.x
            && col < track.x + track.width
            && row >= track.y
            && row < track.y + track.height)
        {
            return None;
        }
        if let Some(grab_row_offset) = crate::ui::scrollbar_thumb_grab_offset(metrics, track, row) {
            Some(ScrollbarClickTarget::Thumb { grab_row_offset })
        } else {
            Some(ScrollbarClickTarget::Track {
                offset_from_bottom: crate::ui::scrollbar_offset_from_row(metrics, track, row),
            })
        }
    }

    fn keybind_help_offset_for_drag_row(&self, row: u16, grab_row_offset: u16) -> Option<usize> {
        let body = self.keybind_help_body_rect()?;
        let metrics = self.keybind_help_scroll_metrics()?;
        let track = crate::ui::release_notes_scrollbar_rect(body, metrics)?;
        Some(crate::ui::scrollbar_offset_from_drag_row(
            metrics,
            track,
            row,
            grab_row_offset,
        ))
    }

    pub(crate) fn keybind_help_max_scroll(&self) -> u16 {
        self.keybind_help_scroll_metrics()
            .map(|metrics| metrics.max_offset_from_bottom as u16)
            .unwrap_or(0)
    }

    fn set_keybind_help_offset_from_bottom(&mut self, offset_from_bottom: usize) {
        let max_scroll = self.keybind_help_max_scroll() as usize;
        self.keybind_help.scroll = max_scroll.saturating_sub(offset_from_bottom) as u16;
    }

    pub(super) fn scroll_keybind_help(&mut self, delta: i16) {
        let max_scroll = self.keybind_help_max_scroll();
        let current = self.keybind_help.scroll as i16;
        self.keybind_help.scroll = current.saturating_add(delta).clamp(0, max_scroll as i16) as u16;
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{MouseButton, MouseEventKind};
    use ratatui::layout::Rect;

    use super::super::{app_for_mouse_test, mouse};
    use super::*;

    #[test]
    fn clicking_keybind_help_close_button_closes_overlay() {
        let mut app = app_for_mouse_test();
        app.state.mode = Mode::KeybindHelp;

        let rect = app.state.keybind_help_popup_rect();
        let inner = Rect::new(
            rect.x + 1,
            rect.y + 1,
            rect.width.saturating_sub(2),
            rect.height.saturating_sub(2),
        );
        let close =
            crate::ui::release_notes_close_button_rect(Rect::new(inner.x, inner.y, inner.width, 1));
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            close.x,
            close.y,
        ));

        assert_eq!(app.state.mode, Mode::Navigate);
    }

    #[test]
    fn onboarding_hover_does_not_change_selection() {
        let mut app = app_for_mouse_test();
        app.state.mode = Mode::Onboarding;

        let inner = app.state.onboarding_modal_inner(64, 16).unwrap();
        let content = crate::ui::modal_stack_areas(inner, 2, 0, 1, 1).content;
        app.handle_mouse(mouse(MouseEventKind::Moved, content.x + 2, content.y));

        assert!(!app.state.request_complete_onboarding);
    }

    #[test]
    fn onboarding_click_continue_requests_completion() {
        let mut app = app_for_mouse_test();
        app.state.mode = Mode::Onboarding;

        let inner = app.state.onboarding_modal_inner(64, 16).unwrap();
        let actions = crate::ui::modal_stack_areas(inner, 2, 0, 1, 1)
            .actions
            .unwrap();
        let continue_rect = crate::ui::onboarding_welcome_continue_rect(actions);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            continue_rect.x,
            continue_rect.y,
        ));

        assert!(app.state.request_complete_onboarding);
    }

    #[test]
    fn release_notes_preview_scrollbar_uses_full_content_body() {
        let mut app = app_for_mouse_test();
        app.state.view.sidebar_rect = Rect::new(0, 0, 24, 16);
        app.state.view.terminal_area = Rect::new(24, 0, 96, 16);
        app.state.release_notes = Some(crate::app::state::ReleaseNotesState {
            version: "9.9.9".into(),
            body: "### Added\n- Custom command keybindings now accept an optional description field.\n\n### Fixed\n- Sidebar Git status refresh now deduplicates workspaces.\n- Large restored sessions no longer leave panes without shells after startup.\n- Pane shutdown no longer warns after the direct child has already exited.\n- Closing the last pane or tab in a parent worktree workspace now shows the existing confirmation before closing the whole worktree group.\n- Update prompts, toasts, and docs now distinguish installing a new binary from stopping or reattaching a running Herdr session to use it."
                .into(),
            scroll: 0,
            preview: true,
        });
        app.state.update_install_command = "brew update && brew upgrade herdr".into();

        let inner = app.state.release_notes_modal_inner().unwrap();
        let expected_body = crate::ui::modal_stack_areas(inner, 2, 1, 0, 1).content;
        let body = app.state.release_notes_body_rect().unwrap();

        assert_eq!(body, expected_body);

        let metrics = app.state.release_notes_scroll_metrics().unwrap();
        assert_eq!(metrics.viewport_rows, body.height as usize);
        assert!(metrics.max_offset_from_bottom > 0);

        let track = crate::ui::release_notes_scrollbar_rect(body, metrics).unwrap();
        assert_eq!(track.y, body.y);
        assert!(matches!(
            app.state
                .release_notes_scrollbar_target_at(track.x, track.y),
            Some(ScrollbarClickTarget::Thumb { .. } | ScrollbarClickTarget::Track { .. })
        ));
    }
}
