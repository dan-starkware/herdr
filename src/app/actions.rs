//! Pure state mutations on AppState.
//! These don't need channels, async, or PTY runtime.

use tracing::{info, warn};

use crate::detect::{Agent, AgentState};
use crate::events::AppEvent;
use crate::layout::{find_in_direction, NavDirection, PaneId};
use crate::terminal::{EffectiveStateChange, TerminalStateMutation};
use crate::workspace::WorkspaceGitStatus;

use super::state::{
    AgentNotificationDelivery, AppState, Mode, PaneFocusTarget, PendingAgentNotification, ToastKind,
    ToastNotification, ToastTarget,
};

fn is_background_completion_transition(prev_state: AgentState, new_state: AgentState) -> bool {
    matches!(new_state, AgentState::Idle)
        && matches!(prev_state, AgentState::Working | AgentState::Blocked)
}

pub fn active_tab_suppresses_notifications(
    is_active_tab: bool,
    outer_terminal_focus: Option<bool>,
) -> bool {
    is_active_tab && outer_terminal_focus != Some(false)
}

pub fn notification_sound_for_state_change(
    suppress_active_tab_notifications: bool,
    prev_state: AgentState,
    new_state: AgentState,
) -> Option<crate::sound::Sound> {
    if new_state == prev_state {
        return None;
    }

    match new_state {
        AgentState::Blocked => Some(crate::sound::Sound::Request),
        AgentState::Idle
            if is_background_completion_transition(prev_state, new_state)
                && !suppress_active_tab_notifications =>
        {
            Some(crate::sound::Sound::Done)
        }
        _ => None,
    }
}

pub fn notification_toast_for_state_change(
    suppress_active_tab_notifications: bool,
    prev_state: AgentState,
    new_state: AgentState,
) -> Option<ToastKind> {
    if suppress_active_tab_notifications || new_state == prev_state {
        return None;
    }

    match new_state {
        AgentState::Blocked => Some(ToastKind::NeedsAttention),
        AgentState::Idle if is_background_completion_transition(prev_state, new_state) => {
            Some(ToastKind::Finished)
        }
        _ => None,
    }
}

fn toast_agent_label(agent_label: &str) -> &str {
    agent_label
}

fn toast_event_text(kind: ToastKind) -> &'static str {
    match kind {
        ToastKind::NeedsAttention => "needs attention",
        ToastKind::Finished => "finished",
        ToastKind::UpdateInstalled => "updated",
    }
}

fn sound_for_toast_kind(
    kind: ToastKind,
    suppress_active_tab_notifications: bool,
) -> Option<crate::sound::Sound> {
    match kind {
        ToastKind::NeedsAttention => Some(crate::sound::Sound::Request),
        ToastKind::Finished if !suppress_active_tab_notifications => {
            Some(crate::sound::Sound::Done)
        }
        ToastKind::Finished | ToastKind::UpdateInstalled => None,
    }
}

pub fn notification_context(
    ws: &crate::workspace::Workspace,
    workspace_label: &str,
    ws_idx: usize,
    pane_id: PaneId,
) -> String {
    let mut context = format!("{} · {}", workspace_label, ws_idx + 1);
    if ws.tabs.len() > 1 {
        if let Some(tab_idx) = ws.find_tab_index_for_pane(pane_id) {
            let tab = &ws.tabs[tab_idx];
            context.push_str(&format!(" · {}", tab.display_name()));
        }
    }
    context
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneStateUpdate {
    pub pane_id: PaneId,
    pub ws_idx: usize,
    pub previous_agent_label: Option<String>,
    pub previous_known_agent: Option<Agent>,
    pub previous_state: AgentState,
    pub previous_seen: bool,
    pub previous_presentation: crate::terminal::EffectivePresentation,
    pub agent_label: Option<String>,
    pub known_agent: Option<Agent>,
    pub state: AgentState,
    pub seen: bool,
    pub presentation: crate::terminal::EffectivePresentation,
}

// ---------------------------------------------------------------------------
// Pane focus tracking
// ---------------------------------------------------------------------------

impl AppState {
    pub(crate) fn current_pane_focus_target(&self) -> Option<PaneFocusTarget> {
        let ws_idx = self.active?;
        let ws = self.workspaces.get(ws_idx)?;
        let pane_id = ws.focused_pane_id()?;
        Some(PaneFocusTarget {
            workspace_id: ws.id.clone(),
            pane_id,
        })
    }

    fn pane_focus_target_indices(&self, target: &PaneFocusTarget) -> Option<(usize, usize)> {
        let ws_idx = self
            .workspaces
            .iter()
            .position(|ws| ws.id == target.workspace_id)?;
        let tab_idx = self.workspaces[ws_idx].find_tab_index_for_pane(target.pane_id)?;
        Some((ws_idx, tab_idx))
    }

    pub(crate) fn record_pane_focus_change(
        &mut self,
        previous: Option<PaneFocusTarget>,
        ws_idx: usize,
        pane_id: PaneId,
    ) {
        let Some(ws) = self.workspaces.get(ws_idx) else {
            return;
        };
        let target = PaneFocusTarget {
            workspace_id: ws.id.clone(),
            pane_id,
        };
        if previous.as_ref() != Some(&target) {
            self.previous_pane_focus = previous;
        }
    }

    fn record_pane_focus_after_navigation(&mut self, previous: Option<PaneFocusTarget>) {
        let current = self.current_pane_focus_target();
        if previous != current {
            self.previous_pane_focus = previous;
        }
    }

    pub(crate) fn focus_pane_in_workspace(&mut self, ws_idx: usize, pane_id: PaneId) -> bool {
        let Some(ws) = self.workspaces.get(ws_idx) else {
            return false;
        };
        let Some(tab_idx) = ws.find_tab_index_for_pane(pane_id) else {
            return false;
        };
        let previous = self.current_pane_focus_target();
        let target = PaneFocusTarget {
            workspace_id: ws.id.clone(),
            pane_id,
        };
        if previous.as_ref() == Some(&target) {
            return false;
        }

        self.switch_workspace_tab(ws_idx, tab_idx);
        if let Some(tab) = self
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.tabs.get_mut(tab_idx))
        {
            tab.layout.focus_pane(pane_id);
            self.previous_pane_focus = previous;
            self.mark_session_dirty();
            return true;
        }
        false
    }
}

// ---------------------------------------------------------------------------
// Workspace operations
// ---------------------------------------------------------------------------

impl AppState {
    pub(crate) fn next_agent_metadata_expiry(&self) -> Option<std::time::Instant> {
        self.terminals
            .values()
            .filter_map(|terminal| terminal.next_agent_metadata_expiry())
            .min()
    }

    pub(crate) fn expire_agent_metadata_at(
        &mut self,
        scheduled_deadline: std::time::Instant,
        now: std::time::Instant,
    ) -> Vec<PaneStateUpdate> {
        let pane_terminals: Vec<_> = self
            .workspaces
            .iter()
            .enumerate()
            .flat_map(|(ws_idx, ws)| {
                ws.tabs.iter().flat_map(move |tab| {
                    tab.layout
                        .pane_ids()
                        .into_iter()
                        .filter_map(move |pane_id| {
                            ws.pane_state(pane_id)
                                .map(|pane| (ws_idx, pane_id, pane.attached_terminal_id.clone()))
                        })
                })
            })
            .collect();
        pane_terminals
            .into_iter()
            .filter_map(|(ws_idx, pane_id, terminal_id)| {
                let previous_seen = self.workspaces[ws_idx].pane_state(pane_id)?.seen;
                let mutation = self
                    .terminals
                    .get_mut(&terminal_id)?
                    .expire_agent_metadata_at(scheduled_deadline, now)?;
                let change = mutation.effective_state_change?;
                let seen = self.apply_pane_state_change(ws_idx, pane_id, &change)?;
                let update = PaneStateUpdate {
                    pane_id,
                    ws_idx,
                    previous_agent_label: change.previous_agent_label.clone(),
                    previous_known_agent: change.previous_known_agent,
                    previous_state: change.previous_state,
                    previous_seen,
                    previous_presentation: change.previous_presentation.clone(),
                    agent_label: change.agent_label.clone(),
                    known_agent: change.known_agent,
                    state: change.state,
                    seen,
                    presentation: change.presentation.clone(),
                };
                Some(update)
            })
            .collect()
    }

    pub(crate) fn pane_is_in_active_tab(&self, ws_idx: usize, pane_id: PaneId) -> bool {
        let Some(active_ws_idx) = self.active else {
            return false;
        };
        if active_ws_idx != ws_idx {
            return false;
        }
        self.workspaces[ws_idx]
            .find_tab_index_for_pane(pane_id)
            .is_some_and(|tab_idx| tab_idx == self.workspaces[ws_idx].active_tab)
    }

    pub fn switch_workspace(&mut self, idx: usize) {
        if idx < self.workspaces.len() {
            let previous_focus = self.current_pane_focus_target();
            self.selection = None;
            self.selection_autoscroll = None;
            self.active = Some(idx);
            self.selected = idx;
            let workspace_id = self.workspaces[idx].id.clone();
            crate::logging::workspace_focused(&workspace_id);
            self.mark_session_dirty();
            if matches!(
                self.agent_panel_scope,
                crate::app::state::AgentPanelScope::CurrentWorkspace
            ) {
                self.agent_panel_scroll = 0;
            }
            self.ensure_workspace_visible(idx);
            if let Some(ws) = self.workspaces.get_mut(idx) {
                let active_tab = ws.active_tab;
                ws.switch_tab(active_tab);
                let tab_id = format!("{}:{}", workspace_id, active_tab + 1);
                crate::logging::tab_focused(&workspace_id, &tab_id);
            }
            self.tab_scroll_follow_active = true;
            self.refresh_tab_bar_view();
            self.record_pane_focus_after_navigation(previous_focus);
        }
    }

    pub(crate) fn switch_workspace_tab(&mut self, ws_idx: usize, tab_idx: usize) -> bool {
        if ws_idx >= self.workspaces.len() {
            return false;
        }
        if self
            .workspaces
            .get(ws_idx)
            .is_none_or(|ws| tab_idx >= ws.tabs.len())
        {
            return false;
        }

        let previous_focus = self.current_pane_focus_target();
        let workspace_changed = self.active != Some(ws_idx);
        self.selection = None;
        self.selection_autoscroll = None;
        self.active = Some(ws_idx);
        self.selected = ws_idx;
        let workspace_id = self.workspaces[ws_idx].id.clone();
        if workspace_changed {
            crate::logging::workspace_focused(&workspace_id);
        }
        self.mark_session_dirty();
        if workspace_changed
            && matches!(
                self.agent_panel_scope,
                crate::app::state::AgentPanelScope::CurrentWorkspace
            )
        {
            self.agent_panel_scroll = 0;
        }
        self.ensure_workspace_visible(ws_idx);
        if let Some(ws) = self.workspaces.get_mut(ws_idx) {
            ws.switch_tab(tab_idx);
            let tab_id = format!("{}:{}", workspace_id, tab_idx + 1);
            crate::logging::tab_focused(&workspace_id, &tab_id);
        }
        self.tab_scroll_follow_active = true;
        self.refresh_tab_bar_view();
        self.record_pane_focus_after_navigation(previous_focus);
        true
    }

    /// No-op: the keyboard-first home has no scrollable workspace list.
    pub(crate) fn ensure_workspace_visible(&mut self, _idx: usize) {}

    pub fn switch_tab(&mut self, idx: usize) {
        if let Some(ws_idx) = self.active {
            let previous_focus = self.current_pane_focus_target();
            self.selection = None;
            self.selection_autoscroll = None;
            let Some(ws) = self.workspaces.get_mut(ws_idx) else {
                return;
            };
            ws.switch_tab(idx);
            let workspace_id = ws.id.clone();
            let tab_id = format!("{}:{}", workspace_id, idx + 1);
            crate::logging::tab_focused(&workspace_id, &tab_id);
            self.mark_session_dirty();
            self.tab_scroll_follow_active = true;
            self.refresh_tab_bar_view();
            self.record_pane_focus_after_navigation(previous_focus);
        }
    }

    pub(crate) fn mark_active_tab_seen(&mut self) -> bool {
        let Some(ws_idx) = self.active else {
            return false;
        };
        let Some(tab) = self
            .workspaces
            .get_mut(ws_idx)
            .and_then(crate::workspace::Workspace::active_tab_mut)
        else {
            return false;
        };

        let mut changed = false;
        for pane in tab.panes.values_mut() {
            if !pane.seen {
                pane.seen = true;
                changed = true;
            }
        }
        changed
    }

    pub(crate) fn visible_workspace_order(&self) -> Vec<usize> {
        (0..self.workspaces.len()).collect()
    }

    pub(crate) fn workspace_at_visible_position(&self, position: usize) -> Option<usize> {
        self.visible_workspace_order().get(position).copied()
    }

    pub(crate) fn move_selected_workspace_by_visible_delta(&mut self, delta: isize) {
        if self.workspaces.is_empty() {
            return;
        }
        let order = self.visible_workspace_order();
        let current_pos = order
            .iter()
            .position(|idx| *idx == self.selected)
            .unwrap_or(0);
        let target_pos = current_pos
            .saturating_add_signed(delta)
            .min(order.len().saturating_sub(1));
        if let Some(ws_idx) = order.get(target_pos).copied() {
            self.selected = ws_idx;
            self.ensure_workspace_visible(ws_idx);
        }
    }

    pub fn next_workspace(&mut self) {
        if self.workspaces.is_empty() {
            return;
        }
        let current = self.active.unwrap_or(self.selected);
        let order = self.visible_workspace_order();
        let current_pos = order.iter().position(|idx| *idx == current).unwrap_or(0);
        let next = order[(current_pos + 1) % order.len()];
        self.switch_workspace(next);
    }

    pub fn previous_workspace(&mut self) {
        if self.workspaces.is_empty() {
            return;
        }
        let current = self.active.unwrap_or(self.selected);
        let order = self.visible_workspace_order();
        let current_pos = order.iter().position(|idx| *idx == current).unwrap_or(0);
        let prev = if current_pos == 0 {
            order[order.len() - 1]
        } else {
            order[current_pos - 1]
        };
        self.switch_workspace(prev);
    }

    pub fn move_workspace(&mut self, source_idx: usize, insert_idx: usize) {
        if source_idx >= self.workspaces.len() || insert_idx > self.workspaces.len() {
            return;
        }

        self.mark_session_dirty();

        let active_id = self.active.map(|idx| self.workspaces[idx].id.clone());
        let selected_id = self
            .workspaces
            .get(self.selected)
            .map(|workspace| workspace.id.clone());

        let workspace = self.workspaces.remove(source_idx);
        let target_idx = if source_idx < insert_idx {
            insert_idx.saturating_sub(1)
        } else {
            insert_idx
        }
        .min(self.workspaces.len());
        self.workspaces.insert(target_idx, workspace);

        self.active = active_id.and_then(|id| self.workspaces.iter().position(|ws| ws.id == id));
        self.selected = selected_id
            .and_then(|id| self.workspaces.iter().position(|ws| ws.id == id))
            .unwrap_or(0);
        self.ensure_workspace_visible(self.selected);
    }

    pub fn scroll_tabs_left(&mut self) {
        self.tab_scroll_follow_active = false;
        self.tab_scroll = self.tab_scroll.saturating_sub(1);
        self.refresh_tab_bar_view();
    }

    pub fn scroll_tabs_right(&mut self) {
        self.tab_scroll_follow_active = false;
        self.tab_scroll = self.tab_scroll.saturating_add(1);
        self.refresh_tab_bar_view();
    }

    pub fn move_tab(&mut self, source_idx: usize, insert_idx: usize) {
        if let Some(ws) = self.active.and_then(|i| self.workspaces.get_mut(i)) {
            if ws.move_tab(source_idx, insert_idx) {
                self.mark_session_dirty();
                self.tab_scroll_follow_active = true;
                self.refresh_tab_bar_view();
            }
        }
    }

    pub fn next_tab(&mut self) {
        if let Some(ws) = self.active.and_then(|i| self.workspaces.get(i)) {
            if !ws.tabs.is_empty() {
                let next = (ws.active_tab + 1) % ws.tabs.len();
                self.switch_tab(next);
            }
        }
    }

    pub fn previous_tab(&mut self) {
        if let Some(ws) = self.active.and_then(|i| self.workspaces.get(i)) {
            if !ws.tabs.is_empty() {
                let prev = if ws.active_tab == 0 {
                    ws.tabs.len() - 1
                } else {
                    ws.active_tab - 1
                };
                self.switch_tab(prev);
            }
        }
    }

    pub fn next_agent(&mut self) {
        self.cycle_agent_entry(true);
    }

    pub fn previous_agent(&mut self) {
        self.cycle_agent_entry(false);
    }

    pub fn focus_agent_entry(&mut self, idx: usize) -> bool {
        let entries = crate::ui::agent_panel_entries_all(self);
        let Some(target) = entries.get(idx) else {
            return false;
        };
        let ws_idx = target.ws_idx;
        let pane_id = target.pane_id;

        if self.active == Some(ws_idx) && self.workspaces[ws_idx].focused_pane_id() == Some(pane_id)
        {
            return true;
        }
        self.focus_pane_in_workspace(ws_idx, pane_id)
    }

    fn cycle_agent_entry(&mut self, forward: bool) {
        let entries = crate::ui::agent_panel_entries_all(self);
        if entries.is_empty() {
            return;
        }

        let focused = self
            .active
            .and_then(|idx| self.workspaces.get(idx))
            .and_then(crate::workspace::Workspace::focused_pane_id);
        let current_idx =
            focused.and_then(|pane_id| entries.iter().position(|entry| entry.pane_id == pane_id));
        let target_idx = match (current_idx, forward) {
            (Some(idx), true) => (idx + 1) % entries.len(),
            (Some(0), false) => entries.len() - 1,
            (Some(idx), false) => idx - 1,
            (None, true) => 0,
            (None, false) => entries.len() - 1,
        };

        self.focus_agent_entry(target_idx);
    }

    pub(crate) fn terminal_ids_for_workspace(
        &self,
        ws_idx: usize,
    ) -> Vec<crate::terminal::TerminalId> {
        let Some(ws) = self.workspaces.get(ws_idx) else {
            return Vec::new();
        };
        let mut ids: Vec<_> = ws
            .tabs
            .iter()
            .flat_map(|tab| tab.panes.values())
            .map(|pane| pane.attached_terminal_id.clone())
            .collect();
        // Detached (kept-alive) review/terminal rows must also be torn down.
        ids.extend(ws.detached_review.clone());
        ids.extend(ws.detached_terminal.clone());
        ids
    }

    pub(crate) fn terminal_ids_for_tab(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Vec<crate::terminal::TerminalId> {
        self.workspaces
            .get(ws_idx)
            .and_then(|ws| ws.tabs.get(tab_idx))
            .into_iter()
            .flat_map(|tab| tab.panes.values())
            .map(|pane| pane.attached_terminal_id.clone())
            .collect()
    }

    pub(crate) fn terminal_id_for_pane(
        &self,
        ws_idx: usize,
        pane_id: PaneId,
    ) -> Option<crate::terminal::TerminalId> {
        self.workspaces
            .get(ws_idx)?
            .pane_state(pane_id)
            .map(|pane| pane.attached_terminal_id.clone())
    }

    pub(crate) fn remove_unattached_terminal_ids(
        &mut self,
        terminal_ids: impl IntoIterator<Item = crate::terminal::TerminalId>,
    ) {
        for terminal_id in terminal_ids {
            let still_attached = self.workspaces.iter().any(|ws| {
                ws.tabs.iter().any(|tab| {
                    tab.panes
                        .values()
                        .any(|pane| pane.attached_terminal_id == terminal_id)
                })
            });
            if !still_attached
                && self.terminals.remove(&terminal_id).is_some()
                && !self.terminal_runtime_shutdowns.contains(&terminal_id)
            {
                self.terminal_runtime_shutdowns.push(terminal_id);
            }
        }
    }

    pub fn close_selected_workspace(&mut self) {
        if self.workspaces.is_empty() {
            return;
        }
        self.selection = None;
        self.selection_autoscroll = None;
        self.mark_session_dirty();
        let close_indices = self
            .workspaces
            .get(self.selected)
            .and_then(|ws| ws.worktree_space())
            .filter(|space| !space.is_linked_worktree)
            .map(|space| {
                self.workspaces
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, ws)| {
                        ws.worktree_space()
                            .is_some_and(|member| member.key == space.key)
                            .then_some(idx)
                    })
                    .collect::<Vec<_>>()
            })
            .filter(|indices| indices.len() >= 2)
            .unwrap_or_else(|| vec![self.selected]);

        let mut terminal_ids = Vec::new();
        for idx in &close_indices {
            terminal_ids.extend(self.terminal_ids_for_workspace(*idx));
            if let Some(workspace_id) = self.workspaces.get(*idx).map(|ws| ws.id.clone()) {
                crate::logging::workspace_closed(&workspace_id);
            }
        }
        for idx in close_indices.iter().rev() {
            self.workspaces.remove(*idx);
        }
        self.remove_unattached_terminal_ids(terminal_ids);
        if self.workspaces.is_empty() {
            self.active = None;
            self.selected = 0;
            self.workspace_scroll = 0;
            self.tab_scroll = 0;
            self.tab_scroll_follow_active = true;
        } else {
            if self.selected >= self.workspaces.len() {
                self.selected = self.workspaces.len() - 1;
            }
            self.active = Some(self.selected);
            self.workspace_scroll = self
                .workspace_scroll
                .min(self.workspaces.len().saturating_sub(1));
            self.ensure_workspace_visible(self.selected);
            self.tab_scroll_follow_active = true;
            self.refresh_tab_bar_view();
        }
    }

    /// No-op: the keyboard-first home has no tab bar to lay out.
    fn refresh_tab_bar_view(&mut self) {}
}

// ---------------------------------------------------------------------------
// Pane operations
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaneZoomCommand {
    Toggle,
    On,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaneZoomNoopReason {
    SinglePane,
    AlreadyZoomed,
    AlreadyUnzoomed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneZoomOutcome {
    pub changed: bool,
    pub focus_changed: bool,
    pub reason: Option<PaneZoomNoopReason>,
    pub zoomed: bool,
}

impl AppState {
    pub fn navigate_pane(&mut self, direction: NavDirection) {
        let Some(ws_idx) = self.active else {
            return;
        };
        let Some(tab) = self.workspaces.get(ws_idx).and_then(|ws| ws.active_tab()) else {
            return;
        };
        let panes = if tab.zoomed {
            tab.layout.panes(self.view.terminal_area)
        } else {
            self.view.pane_infos.clone()
        };

        if let Some(focused) = panes.iter().find(|p| p.is_focused) {
            if let Some(target) = find_in_direction(focused, direction, &panes) {
                self.focus_pane_in_workspace(ws_idx, target);
            }
        }
    }

    pub fn swap_pane(&mut self, direction: NavDirection) -> bool {
        let Some(ws_idx) = self.active else {
            return false;
        };
        let Some(tab) = self.workspaces.get(ws_idx).and_then(|ws| ws.active_tab()) else {
            return false;
        };
        let panes = if tab.zoomed {
            tab.layout.panes(self.view.terminal_area)
        } else {
            self.view.pane_infos.clone()
        };

        let Some(focused) = panes.iter().find(|p| p.is_focused) else {
            return false;
        };
        let Some(target) = find_in_direction(focused, direction, &panes) else {
            return false;
        };
        let source = focused.id;
        let Some(tab) = self
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.active_tab_mut())
        else {
            return false;
        };
        if tab.layout.swap_panes(source, target) {
            self.mark_session_dirty();
            true
        } else {
            false
        }
    }

    pub fn resize_pane(&mut self, direction: NavDirection) {
        if let Some(first) = self.view.pane_infos.first() {
            let area = self
                .view
                .pane_infos
                .iter()
                .fold(first.rect, |acc, p| acc.union(p.rect));
            if let Some(tab) = self
                .active
                .and_then(|i| self.workspaces.get_mut(i))
                .and_then(|ws| ws.active_tab_mut())
            {
                tab.layout.resize_focused(direction, 0.05, area);
                self.mark_session_dirty();
            }
        }
    }

    pub fn cycle_pane(&mut self, reverse: bool) {
        let Some(ws_idx) = self.active else {
            return;
        };
        let Some(tab) = self.workspaces.get(ws_idx).and_then(|ws| ws.active_tab()) else {
            return;
        };
        let ids = tab.layout.pane_ids();
        if let Some(pos) = ids.iter().position(|id| *id == tab.layout.focused()) {
            let target = if reverse {
                ids[(pos + ids.len() - 1) % ids.len()]
            } else {
                ids[(pos + 1) % ids.len()]
            };
            self.focus_pane_in_workspace(ws_idx, target);
        }
    }

    pub fn last_pane(&mut self) {
        let Some(target) = self.previous_pane_focus.clone() else {
            return;
        };
        let Some((ws_idx, tab_idx)) = self.pane_focus_target_indices(&target) else {
            self.previous_pane_focus = None;
            return;
        };
        let current = self.current_pane_focus_target();
        if current.as_ref() == Some(&target) {
            self.previous_pane_focus = None;
            return;
        }

        self.switch_workspace_tab(ws_idx, tab_idx);
        if let Some(tab) = self
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.tabs.get_mut(tab_idx))
        {
            tab.layout.focus_pane(target.pane_id);
            self.previous_pane_focus = current;
            self.mark_session_dirty();
        }
    }

    pub(crate) fn apply_pane_zoom(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        command: PaneZoomCommand,
    ) -> Option<PaneZoomOutcome> {
        let tab_idx = self
            .workspaces
            .get(ws_idx)?
            .find_tab_index_for_pane(pane_id)?;
        let focus_changed = self.focus_pane_in_workspace(ws_idx, pane_id);
        let tab = self
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.tabs.get_mut(tab_idx))?;
        if tab.layout.pane_count() <= 1 {
            return Some(PaneZoomOutcome {
                changed: false,
                focus_changed,
                reason: Some(PaneZoomNoopReason::SinglePane),
                zoomed: tab.zoomed,
            });
        }

        let desired = match command {
            PaneZoomCommand::Toggle => !tab.zoomed,
            PaneZoomCommand::On => true,
            PaneZoomCommand::Off => false,
        };
        let reason = match (command, tab.zoomed) {
            (PaneZoomCommand::On, true) => Some(PaneZoomNoopReason::AlreadyZoomed),
            (PaneZoomCommand::Off, false) => Some(PaneZoomNoopReason::AlreadyUnzoomed),
            _ => None,
        };
        if reason.is_some() {
            return Some(PaneZoomOutcome {
                changed: false,
                focus_changed,
                reason,
                zoomed: tab.zoomed,
            });
        }

        tab.zoomed = desired;
        let zoomed = tab.zoomed;
        self.mark_session_dirty();
        Some(PaneZoomOutcome {
            changed: true,
            focus_changed,
            reason: None,
            zoomed,
        })
    }

    pub fn toggle_zoom(&mut self) {
        let Some(ws_idx) = self.active else {
            return;
        };
        let Some(pane_id) = self
            .workspaces
            .get(ws_idx)
            .and_then(crate::workspace::Workspace::focused_pane_id)
        else {
            return;
        };
        self.apply_pane_zoom(ws_idx, pane_id, PaneZoomCommand::Toggle);
    }

    pub(crate) fn workspace_close_would_close_worktree_group(&self, ws_idx: usize) -> bool {
        self.workspaces
            .get(ws_idx)
            .and_then(|ws| ws.worktree_space())
            .filter(|space| !space.is_linked_worktree)
            .is_some_and(|space| {
                self.workspaces
                    .iter()
                    .filter(|ws| {
                        ws.worktree_space()
                            .is_some_and(|member| member.key == space.key)
                    })
                    .count()
                    >= 2
            })
    }

    /// The legacy close-confirmation dialog was removed with the old UI; agents
    /// are torn down via the keyboard-first kill-with-confirm flow instead.
    pub(crate) fn confirm_implicit_worktree_group_close(&mut self, _ws_idx: usize) -> bool {
        false
    }

    fn close_focused_pane_would_close_workspace(&self, ws_idx: usize) -> bool {
        self.workspaces.get(ws_idx).is_some_and(|ws| {
            let pane_count = ws
                .active_tab()
                .map(|tab| tab.layout.pane_count())
                .unwrap_or(0);
            pane_count <= 1 && ws.tabs.len() <= 1
        })
    }

    pub(crate) fn close_pane_would_close_workspace(&self, ws_idx: usize, pane_id: PaneId) -> bool {
        self.workspaces.get(ws_idx).is_some_and(|ws| {
            ws.find_tab_index_for_pane(pane_id).is_some_and(|tab_idx| {
                ws.tabs[tab_idx].layout.pane_count() <= 1 && ws.tabs.len() <= 1
            })
        })
    }

    /// Close the focused pane. Returns true when the close was deferred to confirmation.
    pub fn close_pane(&mut self) -> bool {
        let active = self.active;
        if active.is_some_and(|ws_idx| {
            self.close_focused_pane_would_close_workspace(ws_idx)
                && self.workspace_close_would_close_worktree_group(ws_idx)
        }) {
            if let Some(ws_idx) = active {
                if self.confirm_implicit_worktree_group_close(ws_idx) {
                    return true;
                }
            }
        }

        self.selection = None;
        self.selection_autoscroll = None;
        self.mark_session_dirty();
        let terminal_ids = active
            .and_then(|i| {
                self.workspaces
                    .get(i)
                    .and_then(|ws| ws.focused_pane_id().map(|pane_id| (i, pane_id)))
            })
            .and_then(|(i, pane_id)| self.terminal_id_for_pane(i, pane_id))
            .into_iter()
            .collect::<Vec<_>>();
        let should_close_workspace = active
            .and_then(|i| self.workspaces.get_mut(i))
            .is_some_and(|ws| ws.close_focused());
        if should_close_workspace {
            if let Some(active) = active {
                self.selected = active;
            }
            self.close_selected_workspace();
        } else {
            self.remove_unattached_terminal_ids(terminal_ids);
        }
        false
    }

    /// Close the active tab. Returns true when the close was deferred to confirmation.
    pub fn close_tab(&mut self) -> bool {
        if self.active.is_some_and(|ws_idx| {
            self.workspaces
                .get(ws_idx)
                .is_some_and(|ws| ws.tabs.len() <= 1)
                && self.workspace_close_would_close_worktree_group(ws_idx)
        }) {
            if let Some(ws_idx) = self.active {
                if self.confirm_implicit_worktree_group_close(ws_idx) {
                    return true;
                }
            }
        }

        self.selection = None;
        self.selection_autoscroll = None;
        self.mark_session_dirty();
        let should_close_workspace = self
            .active
            .and_then(|i| self.workspaces.get(i))
            .is_some_and(|ws| ws.tabs.len() <= 1);
        if should_close_workspace {
            if let Some(active) = self.active {
                self.selected = active;
            }
            self.close_selected_workspace();
            return false;
        }
        if let Some(ws_idx) = self.active {
            let terminal_ids = self
                .workspaces
                .get(ws_idx)
                .map(|ws| self.terminal_ids_for_tab(ws_idx, ws.active_tab))
                .unwrap_or_default();
            let Some(ws) = self.workspaces.get_mut(ws_idx) else {
                return false;
            };
            let workspace_id = ws.id.clone();
            let closing_tab_id = format!("{}:{}", workspace_id, ws.active_tab + 1);
            ws.close_active_tab();
            self.remove_unattached_terminal_ids(terminal_ids);
            crate::logging::tab_closed(&workspace_id, &closing_tab_id);
            self.tab_scroll_follow_active = true;
            self.refresh_tab_bar_view();
        }
        false
    }
}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

impl AppState {
    pub fn clear_selection(&mut self) {
        self.selection = None;
        self.selection_autoscroll = None;
    }

    pub(crate) fn stop_selection_autoscroll_state(&mut self) {
        self.selection_autoscroll = None;
    }

    pub fn copy_selection(&mut self, terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry) {
        let mut sel = match self.selection.take() {
            Some(sel) => sel,
            None => return,
        };
        if !sel.finish() {
            return;
        }

        let ws_idx = match self.active {
            Some(ws_idx) if self.workspaces.get(ws_idx).is_some() => ws_idx,
            _ => return,
        };

        let text = self
            .runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, sel.pane_id)
            .and_then(|rt| rt.extract_selection(&sel));
        if let Some(text) = text {
            if !text.is_empty() {
                self.request_clipboard_write = Some(text.into_bytes());
                info!("copied selection to clipboard");
            }
        }

        self.clear_selection();
    }
}

// ---------------------------------------------------------------------------
// Event handling
// ---------------------------------------------------------------------------

impl AppState {
    pub fn apply_workspace_git_statuses(
        &mut self,
        terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
        results: Vec<WorkspaceGitStatus>,
    ) -> bool {
        let mut changed = false;
        for result in results {
            let Some(ws_idx) = self
                .workspaces
                .iter()
                .position(|ws| ws.id == result.workspace_id)
            else {
                continue;
            };

            if self.workspaces[ws_idx]
                .resolved_identity_cwd_from(&self.terminals, terminal_runtimes)
                .as_ref()
                != Some(&result.resolved_identity_cwd)
            {
                continue;
            }

            let ws = &mut self.workspaces[ws_idx];
            if ws.cached_git_branch != result.branch {
                ws.cached_git_branch = result.branch;
                changed = true;
            }
            if ws.cached_git_ahead_behind != result.ahead_behind {
                ws.cached_git_ahead_behind = result.ahead_behind;
                changed = true;
            }
            if ws.cached_git_space != result.space {
                ws.cached_git_space = result.space;
                changed = true;
            }
        }
        changed
    }

    pub fn handle_app_event(&mut self, event: AppEvent) -> Vec<PaneStateUpdate> {
        match event {
            AppEvent::PaneDied { pane_id } => {
                self.handle_pane_died(pane_id);
                Vec::new()
            }
            AppEvent::UpdateReady {
                version,
                install_command,
            } => {
                self.update_available = Some(version.clone());
                self.update_install_command = install_command.clone();
                self.latest_release_notes_available = true;
                self.update_dismissed = true;
                if matches!(
                    self.toast_config.delivery,
                    crate::config::ToastDelivery::Herdr
                ) {
                    self.toast = Some(ToastNotification {
                        kind: ToastKind::UpdateInstalled,
                        title: format!("v{version} available"),
                        context: crate::update::update_install_instruction(&install_command),
                        position: None,
                        target: None,
                    });
                }
                Vec::new()
            }
            AppEvent::StateChanged {
                pane_id,
                agent,
                state,
                visible_blocker,
                visible_idle,
                visible_working,
                process_exited,
                observed_at,
            } => self
                .update_terminal_state(pane_id, |terminal| {
                    Some(terminal.set_detected_state_with_screen_signals_at(
                        agent,
                        state,
                        visible_blocker,
                        visible_idle,
                        visible_working,
                        process_exited,
                        observed_at,
                    ))
                })
                .into_iter()
                .collect(),
            AppEvent::HookStateReported {
                pane_id,
                source,
                agent_label,
                state,
                message,
                custom_status,
                seq,
                session_ref,
            } => {
                if crate::agent_resume::is_reserved_native_state_source(&source, &agent_label) {
                    self.update_terminal_state(pane_id, |terminal| {
                        terminal.set_agent_session_ref(source, agent_label, session_ref, seq)
                    })
                    .into_iter()
                    .collect()
                } else {
                    self.update_terminal_state(pane_id, |terminal| {
                        terminal.set_hook_authority_with_session_ref(
                            source,
                            agent_label,
                            state,
                            message,
                            custom_status,
                            session_ref,
                            seq,
                        )
                    })
                    .into_iter()
                    .collect()
                }
            }
            AppEvent::AgentSessionReported {
                pane_id,
                source,
                agent_label,
                seq,
                session_ref,
            } => self
                .update_terminal_state(pane_id, |terminal| {
                    terminal.set_agent_session_ref(source, agent_label, session_ref, seq)
                })
                .into_iter()
                .collect(),
            AppEvent::HookMetadataReported {
                pane_id,
                source,
                agent_label,
                applies_to_source,
                title,
                display_agent,
                custom_status,
                state_labels,
                clear_title,
                clear_display_agent,
                clear_custom_status,
                clear_state_labels,
                seq,
                ttl,
            } => self
                .update_terminal_state(pane_id, |terminal| {
                    terminal.set_agent_metadata(crate::terminal::AgentMetadataReport {
                        source,
                        agent_label,
                        applies_to_source,
                        title,
                        display_agent,
                        custom_status,
                        state_labels,
                        clear_title,
                        clear_display_agent,
                        clear_custom_status,
                        clear_state_labels,
                        ttl,
                        seq,
                    })
                })
                .into_iter()
                .collect(),
            AppEvent::HookAuthorityCleared {
                pane_id,
                source,
                seq,
            } => self
                .update_terminal_state(pane_id, |terminal| {
                    terminal.clear_hook_authority_with_mutation(source.as_deref(), seq)
                })
                .into_iter()
                .collect(),
            AppEvent::HookAgentReleased {
                pane_id,
                source,
                agent_label,
                seq,
                ..
            } => {
                if crate::agent_resume::is_reserved_native_state_source(&source, &agent_label) {
                    Vec::new()
                } else {
                    self.update_terminal_state(pane_id, |terminal| {
                        terminal.release_agent_with_mutation(&source, &agent_label, seq)
                    })
                    .into_iter()
                    .collect()
                }
            }
            // Intercepted in App::handle_internal_event before reaching this
            // dispatch; never touches AppState.
            AppEvent::ClipboardWrite { .. } => Vec::new(),
            // Also intercepted in App::handle_internal_event (it needs to move
            // pane focus and request a render).
            AppEvent::PaneFocusSignal { .. } => Vec::new(),
            AppEvent::GitStatusRefreshed {
                results,
                cache_updates,
            } => {
                let _ = results;
                let _ = cache_updates;
                Vec::new()
            }
            AppEvent::WorktreeAddFinished(_) => Vec::new(),
            AppEvent::WorktreeRemoveFinished(_) => Vec::new(),
        }
    }

    fn update_terminal_state<F>(&mut self, pane_id: PaneId, update: F) -> Option<PaneStateUpdate>
    where
        F: FnOnce(&mut crate::terminal::TerminalState) -> Option<TerminalStateMutation>,
    {
        let ws_idx = self
            .workspaces
            .iter()
            .position(|ws| ws.pane_state(pane_id).is_some())?;
        let terminal_id = self.workspaces[ws_idx]
            .pane_state(pane_id)?
            .attached_terminal_id
            .clone();
        let previous_seen = self.workspaces[ws_idx].pane_state(pane_id)?.seen;
        let mutation = {
            let terminal = self.terminals.get_mut(&terminal_id)?;
            update(terminal)?
        };
        if mutation.session_ref_changed {
            self.mark_session_dirty();
        }
        let change = mutation.effective_state_change?;
        let seen = self.apply_pane_state_change(ws_idx, pane_id, &change)?;
        let update = PaneStateUpdate {
            pane_id,
            ws_idx,
            previous_agent_label: change.previous_agent_label.clone(),
            previous_known_agent: change.previous_known_agent,
            previous_state: change.previous_state,
            previous_seen,
            previous_presentation: change.previous_presentation.clone(),
            agent_label: change.agent_label.clone(),
            known_agent: change.known_agent,
            state: change.state,
            seen,
            presentation: change.presentation.clone(),
        };
        Some(update)
    }

    fn apply_pane_state_change(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        change: &EffectiveStateChange,
    ) -> Option<bool> {
        let is_active_tab = self.pane_is_in_active_tab(ws_idx, pane_id);
        let suppress_active_tab_notifications =
            active_tab_suppresses_notifications(is_active_tab, self.outer_terminal_focus);
        let pane = self.workspaces[ws_idx]
            .tabs
            .iter_mut()
            .find_map(|tab| tab.panes.get_mut(&pane_id))?;

        if change.state != AgentState::Idle {
            pane.seen = true;
        } else if is_background_completion_transition(change.previous_state, change.state) {
            pane.seen = suppress_active_tab_notifications;
        }
        let seen = pane.seen;

        if let Some(delivery) = self.record_or_deliver_agent_notification(ws_idx, pane_id, change) {
            self.apply_agent_notification_delivery(&delivery);
        }

        Some(seen)
    }

    fn record_or_deliver_agent_notification(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        change: &EffectiveStateChange,
    ) -> Option<AgentNotificationDelivery> {
        self.pending_agent_notifications.remove(&pane_id);

        let is_active_tab = self.pane_is_in_active_tab(ws_idx, pane_id);
        let suppress_active_tab_notifications =
            active_tab_suppresses_notifications(is_active_tab, self.outer_terminal_focus);

        let client_notification_kind = notification_toast_for_state_change(
            suppress_active_tab_notifications,
            change.previous_state,
            change.state,
        );
        let sound = notification_sound_for_state_change(
            suppress_active_tab_notifications,
            change.previous_state,
            change.state,
        );
        if client_notification_kind.is_none() && sound.is_none() {
            return None;
        }

        let agent_label = change.agent_label.clone()?;
        let kind = client_notification_kind.unwrap_or(match sound {
            Some(crate::sound::Sound::Request) => ToastKind::NeedsAttention,
            Some(crate::sound::Sound::Done) | None => ToastKind::Finished,
        });
        let workspace_id = self.workspaces[ws_idx].id.clone();

        if self.toast_config.delay_seconds == 0 {
            return self.agent_notification_delivery(
                ws_idx,
                pane_id,
                workspace_id,
                agent_label,
                change.known_agent,
                kind,
                change.state,
            );
        }

        self.pending_agent_notifications.insert(
            pane_id,
            PendingAgentNotification {
                pane_id,
                workspace_id,
                agent_label,
                known_agent: change.known_agent,
                kind,
                state: change.state,
                deadline: {
                    let now = std::time::Instant::now();
                    let delay_seconds = self
                        .toast_config
                        .delay_seconds
                        .min(crate::config::MAX_TOAST_DELAY_SECONDS);
                    now.checked_add(std::time::Duration::from_secs(delay_seconds))
                        .unwrap_or(now)
                },
            },
        );
        None
    }

    fn agent_notification_delivery(
        &self,
        ws_idx: usize,
        pane_id: PaneId,
        workspace_id: String,
        agent_label: String,
        known_agent: Option<Agent>,
        kind: ToastKind,
        expected_state: AgentState,
    ) -> Option<AgentNotificationDelivery> {
        let terminal_state = self
            .workspaces
            .get(ws_idx)?
            .pane_state(pane_id)
            .and_then(|pane| self.terminals.get(&pane.attached_terminal_id))?;
        if terminal_state.state != expected_state {
            return None;
        }
        if terminal_state.effective_agent_label() != Some(agent_label.as_str()) {
            return None;
        }

        let is_active_tab = self.pane_is_in_active_tab(ws_idx, pane_id);
        let suppress_active_tab_notifications =
            active_tab_suppresses_notifications(is_active_tab, self.outer_terminal_focus);
        let sound = sound_for_toast_kind(kind, suppress_active_tab_notifications)
            .filter(|_| self.sound.allows(known_agent));
        let build_toast = || {
            let workspace_label = self.workspaces[ws_idx].display_name();
            let context =
                notification_context(&self.workspaces[ws_idx], &workspace_label, ws_idx, pane_id);
            ToastNotification {
                kind,
                title: format!(
                    "{} {}",
                    toast_agent_label(&agent_label),
                    toast_event_text(kind)
                ),
                context,
                position: None,
                target: Some(ToastTarget {
                    workspace_id: workspace_id.clone(),
                    pane_id,
                }),
            }
        };
        let toast = (!is_active_tab).then(&build_toast);
        let client_notification = (!suppress_active_tab_notifications).then(build_toast);

        if toast.is_none() && client_notification.is_none() && sound.is_none() {
            return None;
        }

        Some(AgentNotificationDelivery {
            pane_id,
            workspace_id,
            agent_label,
            known_agent,
            kind,
            toast,
            client_notification,
            sound,
        })
    }

    fn apply_agent_notification_delivery(&mut self, delivery: &AgentNotificationDelivery) {
        if self.local_sound_playback {
            if let Some(sound) = delivery.sound {
                crate::sound::play(sound, &self.sound);
            }
        }

        if matches!(
            self.toast_config.delivery,
            crate::config::ToastDelivery::Herdr
        ) {
            if let Some(toast) = delivery.toast.clone() {
                self.toast = Some(toast);
            }
        }
    }

    pub fn next_pending_agent_notification_deadline(&self) -> Option<std::time::Instant> {
        self.pending_agent_notifications
            .values()
            .map(|pending| pending.deadline)
            .min()
    }

    pub fn drain_due_agent_notifications(
        &mut self,
        now: std::time::Instant,
    ) -> Vec<AgentNotificationDelivery> {
        let due_panes: Vec<PaneId> = self
            .pending_agent_notifications
            .iter()
            .filter_map(|(&pane_id, pending)| (pending.deadline <= now).then_some(pane_id))
            .collect();
        let mut deliveries = Vec::new();

        for pane_id in due_panes {
            let Some(pending) = self.pending_agent_notifications.remove(&pane_id) else {
                continue;
            };
            let Some(ws_idx) = self
                .workspaces
                .iter()
                .position(|ws| ws.id == pending.workspace_id)
            else {
                continue;
            };
            let Some(delivery) = self.agent_notification_delivery(
                ws_idx,
                pending.pane_id,
                pending.workspace_id,
                pending.agent_label,
                pending.known_agent,
                pending.kind,
                pending.state,
            ) else {
                continue;
            };
            self.apply_agent_notification_delivery(&delivery);
            deliveries.push(delivery);
        }

        deliveries
    }

    fn handle_pane_died(&mut self, pane_id: PaneId) {
        self.pending_agent_notifications.remove(&pane_id);
        let ws_idx = self
            .workspaces
            .iter()
            .position(|ws| ws.find_tab_index_for_pane(pane_id).is_some());

        let Some(ws_idx) = ws_idx else {
            warn!(pane = pane_id.raw(), "PaneDied for unknown pane");
            return;
        };

        if self
            .selection
            .as_ref()
            .is_some_and(|s| s.pane_id == pane_id)
        {
            self.selection = None;
            self.selection_autoscroll = None;
        }

        let pane_terminal_id = self.terminal_id_for_pane(ws_idx, pane_id);
        let workspace_terminal_ids = self.terminal_ids_for_workspace(ws_idx);
        self.pane_id_aliases.retain(|_, alias| *alias != pane_id);
        let should_close_workspace = {
            let ws = &mut self.workspaces[ws_idx];
            ws.remove_pane(pane_id)
        };
        self.mark_session_dirty();

        if should_close_workspace {
            self.workspaces.remove(ws_idx);
            self.remove_unattached_terminal_ids(workspace_terminal_ids);
            if self.workspaces.is_empty() {
                self.active = None;
                self.selected = 0;
            } else {
                if let Some(active) = self.active {
                    if active >= self.workspaces.len() {
                        self.active = Some(self.workspaces.len() - 1);
                    }
                }
                if self.selected >= self.workspaces.len() {
                    self.selected = self.workspaces.len() - 1;
                }
            }
        } else {
            self.remove_unattached_terminal_ids(pane_terminal_id);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Agent, AgentState};
    use crate::workspace::Workspace;
    use ratatui::layout::Direction;

    fn app_with_workspaces(names: &[&str]) -> AppState {
        let mut state = AppState::test_new();
        state.toast_config.delay_seconds = 0;
        for name in names {
            let ws = Workspace::test_new(name);
            state.workspaces.push(ws);
        }
        state.ensure_test_terminals();
        if !state.workspaces.is_empty() {
            state.active = Some(0);
            state.mode = Mode::Home;
        }
        state
    }

    fn mark_linked_worktree(state: &mut AppState, ws_idx: usize) {
        state.workspaces[ws_idx].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: format!("/repo/worktree-{ws_idx}").into(),
            is_linked_worktree: true,
        });
    }

    fn mark_parent_worktree(state: &mut AppState, ws_idx: usize) {
        state.workspaces[ws_idx].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr".into(),
            is_linked_worktree: false,
        });
    }

    #[test]
    fn notification_context_formats_resolved_workspace_label() {
        let state = app_with_workspaces(&["stale"]);
        let root = state.workspaces[0].tabs[0].root_pane;

        assert_eq!(
            notification_context(&state.workspaces[0], "__herdr_projects__", 0, root),
            "__herdr_projects__ · 1"
        );
    }

    #[test]
    fn apply_workspace_git_statuses_updates_matching_workspace() {
        let mut state = app_with_workspaces(&["one", "two"]);
        let first_id = state.workspaces[0].id.clone();
        let first_cwd = state.workspaces[0].resolved_identity_cwd().unwrap();
        let second_id = state.workspaces[1].id.clone();

        let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let changed = state.apply_workspace_git_statuses(
            &terminal_runtimes,
            vec![WorkspaceGitStatus {
                workspace_id: first_id,
                resolved_identity_cwd: first_cwd,
                branch: Some("main".into()),
                ahead_behind: Some((2, 1)),
                space: None,
            }],
        );

        assert!(changed);
        assert_eq!(state.workspaces[0].branch().as_deref(), Some("main"));
        assert_eq!(state.workspaces[0].git_ahead_behind(), Some((2, 1)));
        assert_eq!(state.workspaces[1].id, second_id);
        assert_eq!(state.workspaces[1].git_ahead_behind(), None);
    }

    #[test]
    fn apply_workspace_git_statuses_ignores_stale_cwd() {
        let mut state = app_with_workspaces(&["one"]);
        let workspace_id = state.workspaces[0].id.clone();
        state.workspaces[0].cached_git_branch = Some("old".into());
        state.workspaces[0].cached_git_ahead_behind = Some((1, 0));

        let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let changed = state.apply_workspace_git_statuses(
            &terminal_runtimes,
            vec![WorkspaceGitStatus {
                workspace_id,
                resolved_identity_cwd: std::path::PathBuf::from("/definitely/not/current"),
                branch: Some("main".into()),
                ahead_behind: Some((0, 1)),
                space: None,
            }],
        );

        assert!(!changed);
        assert_eq!(state.workspaces[0].branch().as_deref(), Some("old"));
        assert_eq!(state.workspaces[0].git_ahead_behind(), Some((1, 0)));
    }

    #[test]
    fn apply_workspace_git_statuses_clears_missing_git_status() {
        let mut state = app_with_workspaces(&["one"]);
        let workspace_id = state.workspaces[0].id.clone();
        let cwd = state.workspaces[0].resolved_identity_cwd().unwrap();
        state.workspaces[0].cached_git_branch = Some("main".into());
        state.workspaces[0].cached_git_ahead_behind = Some((1, 2));

        let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let changed = state.apply_workspace_git_statuses(
            &terminal_runtimes,
            vec![WorkspaceGitStatus {
                workspace_id,
                resolved_identity_cwd: cwd,
                branch: None,
                ahead_behind: None,
                space: None,
            }],
        );

        assert!(changed);
        assert_eq!(state.workspaces[0].branch(), None);
        assert_eq!(state.workspaces[0].git_ahead_behind(), None);
    }

    #[test]
    fn apply_workspace_git_statuses_does_not_change_worktree_membership() {
        let mut state = app_with_workspaces(&["one"]);
        mark_linked_worktree(&mut state, 0);
        let workspace_id = state.workspaces[0].id.clone();
        let cwd = state.workspaces[0].resolved_identity_cwd().unwrap();
        let membership = state.workspaces[0].worktree_space().cloned();

        let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let changed = state.apply_workspace_git_statuses(
            &terminal_runtimes,
            vec![WorkspaceGitStatus {
                workspace_id,
                resolved_identity_cwd: cwd,
                branch: Some("scratch".into()),
                ahead_behind: None,
                space: Some(crate::workspace::GitSpaceMetadata {
                    key: "other-repo-key".into(),
                    checkout_key: "/other/checkout".into(),
                    label: "other".into(),
                    repo_root: "/other/repo".into(),
                    is_linked_worktree: false,
                }),
            }],
        );

        assert!(changed);
        assert_eq!(state.workspaces[0].worktree_space().cloned(), membership);
    }

    #[test]
    fn update_ready_sets_explicit_upgrade_toast() {
        let mut state = AppState::test_new();
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;

        let updates = state.handle_app_event(crate::events::AppEvent::UpdateReady {
            version: "0.5.0".into(),
            install_command: "herdr update".into(),
        });

        assert!(updates.is_empty());
        assert_eq!(state.update_available.as_deref(), Some("0.5.0"));
        assert!(state.latest_release_notes_available);
        let toast = state.toast.as_ref().expect("update toast");
        assert_eq!(toast.title, "v0.5.0 available");
        assert_eq!(
            toast.context,
            "detach, run `herdr update`, then follow its restart guidance"
        );
    }

    fn mark_agent(state: &mut AppState, ws_idx: usize, tab_idx: usize, pane_id: PaneId) {
        state.ensure_test_terminals();
        let terminal_id = state.workspaces[ws_idx].tabs[tab_idx]
            .panes
            .get(&pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        if let Some(terminal) = state.terminals.get_mut(&terminal_id) {
            terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        }
    }

    #[test]
    fn next_agent_cycles_agent_panel_entries_in_all_scope() {
        // One entry per open worktree, so each agent is its own workspace.
        let mut first = Workspace::test_new("one");
        first.attach_test_worktree();
        let first_root = first.tabs[0].root_pane;
        let mut second = Workspace::test_new("two");
        second.attach_test_worktree();
        let second_root = second.tabs[0].root_pane;

        let mut state = AppState::test_new();
        state.workspaces = vec![first, second];
        state.ensure_test_terminals();
        state.active = Some(0);
        state.selected = 0;
        state.mode = Mode::Home;
        state.agent_panel_scope = crate::app::state::AgentPanelScope::AllWorkspaces;
        mark_agent(&mut state, 0, 0, first_root);
        mark_agent(&mut state, 1, 0, second_root);

        state.next_agent();
        assert_eq!(state.active, Some(1));
        assert_eq!(state.workspaces[1].focused_pane_id(), Some(second_root));

        state.next_agent();
        assert_eq!(state.active, Some(0));
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(first_root));

        state.previous_agent();
        assert_eq!(state.active, Some(1));
        assert_eq!(state.workspaces[1].focused_pane_id(), Some(second_root));
    }

    #[test]
    fn focus_agent_entry_uses_agent_panel_order() {
        let mut first = Workspace::test_new("one");
        first.attach_test_worktree();
        let mut second = Workspace::test_new("two");
        second.attach_test_worktree();
        let second_root = second.tabs[0].root_pane;

        let mut state = AppState::test_new();
        state.workspaces = vec![first, second];
        state.active = Some(0);
        state.selected = 0;
        state.mode = Mode::Home;
        state.agent_panel_scope = crate::app::state::AgentPanelScope::AllWorkspaces;
        let first_root = state.workspaces[0].tabs[0].root_pane;
        mark_agent(&mut state, 0, 0, first_root);
        mark_agent(&mut state, 1, 0, second_root);

        // Entries are ordered by workspace: index 1 is the second worktree.
        assert!(state.focus_agent_entry(1));

        assert_eq!(state.active, Some(1));
        assert_eq!(state.workspaces[1].focused_pane_id(), Some(second_root));
    }

    #[test]
    fn focus_agent_entry_succeeds_for_already_focused_agent() {
        let mut state = app_with_workspaces(&["one"]);
        state.workspaces[0].attach_test_worktree();
        let root = state.workspaces[0].tabs[0].root_pane;
        state.agent_panel_scope = crate::app::state::AgentPanelScope::AllWorkspaces;
        mark_agent(&mut state, 0, 0, root);

        assert!(state.focus_agent_entry(0));
        assert_eq!(state.active, Some(0));
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(root));
    }

    #[test]
    fn switch_workspace_updates_active_and_selected() {
        let mut state = app_with_workspaces(&["a", "b", "c"]);
        state.switch_workspace(2);
        assert_eq!(state.active, Some(2));
        assert_eq!(state.selected, 2);
    }

    #[test]
    fn last_pane_toggles_to_previous_focus_in_active_tab() {
        let mut state = app_with_workspaces(&["test"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let right = state.workspaces[0].test_split(Direction::Horizontal);

        state.focus_pane_in_workspace(0, root);
        state.focus_pane_in_workspace(0, right);
        state.last_pane();

        assert_eq!(state.workspaces[0].focused_pane_id(), Some(root));

        state.last_pane();

        assert_eq!(state.workspaces[0].focused_pane_id(), Some(right));
    }

    #[test]
    fn removing_background_pane_preserves_last_pane_history() {
        let mut state = app_with_workspaces(&["test"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let right = state.workspaces[0].test_split(Direction::Horizontal);
        let background = state.workspaces[0].test_split(Direction::Horizontal);

        state.focus_pane_in_workspace(0, root);
        state.focus_pane_in_workspace(0, right);
        state.workspaces[0].remove_pane(background);
        state.last_pane();

        assert_eq!(state.workspaces[0].focused_pane_id(), Some(root));
    }

    #[test]
    fn last_pane_jumps_across_workspaces_and_tabs() {
        let mut state = app_with_workspaces(&["one", "two"]);
        let first_root = state.workspaces[0].tabs[0].root_pane;
        let second_tab = state.workspaces[1].test_add_tab(Some("logs"));
        let second_tab_root = state.workspaces[1].tabs[second_tab].root_pane;

        state.focus_pane_in_workspace(0, first_root);
        state.focus_pane_in_workspace(1, second_tab_root);
        state.last_pane();

        assert_eq!(state.active, Some(0));
        assert_eq!(state.workspaces[0].active_tab, 0);
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(first_root));

        state.last_pane();

        assert_eq!(state.active, Some(1));
        assert_eq!(state.workspaces[1].active_tab, second_tab);
        assert_eq!(state.workspaces[1].focused_pane_id(), Some(second_tab_root));
    }

    #[test]
    fn last_pane_tracks_tab_and_workspace_switches() {
        let mut state = app_with_workspaces(&["one", "two"]);
        let first_root = state.workspaces[0].tabs[0].root_pane;
        let first_second_tab = state.workspaces[0].test_add_tab(Some("logs"));
        let first_second_root = state.workspaces[0].tabs[first_second_tab].root_pane;
        let second_root = state.workspaces[1].tabs[0].root_pane;

        state.switch_tab(first_second_tab);
        state.last_pane();

        assert_eq!(state.active, Some(0));
        assert_eq!(state.workspaces[0].active_tab, 0);
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(first_root));

        state.last_pane();

        assert_eq!(state.active, Some(0));
        assert_eq!(state.workspaces[0].active_tab, first_second_tab);
        assert_eq!(
            state.workspaces[0].focused_pane_id(),
            Some(first_second_root)
        );

        state.switch_workspace(1);
        state.last_pane();

        assert_eq!(state.active, Some(0));
        assert_eq!(state.workspaces[0].active_tab, first_second_tab);
        assert_eq!(
            state.workspaces[0].focused_pane_id(),
            Some(first_second_root)
        );

        state.last_pane();

        assert_eq!(state.active, Some(1));
        assert_eq!(state.workspaces[1].focused_pane_id(), Some(second_root));
    }

    #[test]
    fn last_pane_tracks_cross_workspace_tab_selection() {
        let mut state = app_with_workspaces(&["one", "two"]);
        let first_root = state.workspaces[0].tabs[0].root_pane;
        let second_first_root = state.workspaces[1].tabs[0].root_pane;
        let second_tab = state.workspaces[1].test_add_tab(Some("logs"));
        let second_tab_root = state.workspaces[1].tabs[second_tab].root_pane;

        state.switch_workspace_tab(1, second_tab);
        state.last_pane();

        assert_eq!(state.active, Some(0));
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(first_root));

        state.last_pane();

        assert_eq!(state.active, Some(1));
        assert_eq!(state.workspaces[1].active_tab, second_tab);
        assert_eq!(state.workspaces[1].focused_pane_id(), Some(second_tab_root));
        assert_ne!(second_first_root, second_tab_root);
    }

    #[test]
    fn switch_workspace_marks_panes_seen() {
        let mut state = app_with_workspaces(&["a", "b"]);
        // Mark a pane in workspace 1 as unseen
        let id = *state.workspaces[1].panes.keys().next().unwrap();
        state.workspaces[1].panes.get_mut(&id).unwrap().seen = false;

        state.switch_workspace(1);
        assert!(state.workspaces[1].panes.get(&id).unwrap().seen);
    }

    #[test]
    fn switch_workspace_out_of_bounds_is_noop() {
        let mut state = app_with_workspaces(&["a"]);
        state.switch_workspace(5);
        assert_eq!(state.active, Some(0));
    }

    #[test]
    fn move_workspace_reorders_without_changing_logical_selection() {
        let mut state = app_with_workspaces(&["a", "b", "c"]);
        let active_id = state.workspaces[1].id.clone();
        let selected_id = state.workspaces[2].id.clone();
        state.active = Some(1);
        state.selected = 2;

        state.move_workspace(1, 0);

        let names: Vec<_> = state
            .workspaces
            .iter()
            .map(|ws| ws.display_name())
            .collect();
        assert_eq!(names, vec!["b", "a", "c"]);
        assert_eq!(state.active, Some(0));
        assert_eq!(state.selected, 2);
        assert_eq!(state.workspaces[state.active.unwrap()].id, active_id);
        assert_eq!(state.workspaces[state.selected].id, selected_id);
    }

    #[test]
    fn move_workspace_accepts_insert_at_end() {
        let mut state = app_with_workspaces(&["a", "b", "c"]);

        state.move_workspace(0, state.workspaces.len());

        let names: Vec<_> = state
            .workspaces
            .iter()
            .map(|ws| ws.display_name())
            .collect();
        assert_eq!(names, vec!["b", "c", "a"]);
    }

    #[test]
    fn close_workspace_adjusts_indices() {
        let mut state = app_with_workspaces(&["a", "b", "c"]);
        state.selected = 1;
        state.active = Some(1);

        state.close_selected_workspace();

        assert_eq!(state.workspaces.len(), 2);
        assert_eq!(state.selected, 1);
        assert_eq!(state.active, Some(1));
        assert_eq!(state.workspaces[1].custom_name.as_deref(), Some("c"));
    }

    #[test]
    fn close_parent_worktree_workspace_closes_group() {
        let mut state = app_with_workspaces(&["main", "issue", "notes"]);
        state.workspaces[0].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr".into(),
            is_linked_worktree: false,
        });
        state.workspaces[1].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr-issue".into(),
            is_linked_worktree: true,
        });
        state.selected = 0;
        state.active = Some(0);

        state.close_selected_workspace();

        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].display_name(), "notes");
        assert_eq!(state.active, Some(0));
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn close_last_workspace_clears_active() {
        let mut state = app_with_workspaces(&["only"]);
        state.selected = 0;
        state.close_selected_workspace();

        assert!(state.workspaces.is_empty());
        assert_eq!(state.active, None);
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn close_workspace_at_end_adjusts_selected() {
        let mut state = app_with_workspaces(&["a", "b"]);
        state.selected = 1;
        state.active = Some(1);

        state.close_selected_workspace();

        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.selected, 0);
        assert_eq!(state.active, Some(0));
    }

    #[test]
    fn pane_died_last_pane_removes_workspace() {
        let mut state = app_with_workspaces(&["a", "b"]);
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();

        state.handle_pane_died(pane_id);

        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].custom_name.as_deref(), Some("b"));
    }

    #[test]
    fn pane_died_last_workspace_enters_navigate() {
        let mut state = app_with_workspaces(&["only"]);
        state.mode = Mode::Home;
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();

        state.handle_pane_died(pane_id);

        assert!(state.workspaces.is_empty());
        assert_eq!(state.mode, Mode::Home);
    }

    #[test]
    fn pane_died_multi_pane_keeps_workspace() {
        let mut state = app_with_workspaces(&["test"]);
        let second_id = state.workspaces[0].test_split(Direction::Horizontal);

        state.handle_pane_died(second_id);

        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].panes.len(), 1);
    }

    #[test]
    fn pane_died_unknown_pane_is_noop() {
        let mut state = app_with_workspaces(&["test"]);
        let fake_id = PaneId::from_raw(9999);

        state.handle_pane_died(fake_id);

        assert_eq!(state.workspaces.len(), 1);
    }

    #[test]
    fn pane_died_unrelated_pane_preserves_selection() {
        // Two workspaces; user is selecting text in workspace 0.
        // A pane in workspace 1 dies — selection must be preserved.
        let mut state = app_with_workspaces(&["active", "bg"]);
        let active_pane = *state.workspaces[0].panes.keys().next().unwrap();
        let bg_pane = *state.workspaces[1].panes.keys().next().unwrap();

        state.selection = Some(crate::selection::Selection::anchor(active_pane, 0, 0, None));
        state.selection_autoscroll = Some(crate::app::state::SelectionAutoscroll {
            direction: crate::app::state::SelectionAutoscrollDirection::Down,
            last_mouse_screen_col: 0,
            last_mouse_screen_row: 23,
            inner_rect: ratatui::layout::Rect::new(0, 0, 80, 24),
        });

        state.handle_pane_died(bg_pane);

        assert!(state.selection.is_some());
        assert!(state.selection_autoscroll.is_some());
    }

    #[test]
    fn pane_died_same_pane_clears_selection() {
        let mut state = app_with_workspaces(&["test"]);
        let first_id = state.workspaces[0].tabs[0].root_pane;
        let second_id = state.workspaces[0].test_split(Direction::Horizontal);

        state.selection = Some(crate::selection::Selection::anchor(second_id, 0, 0, None));
        state.selection_autoscroll = Some(crate::app::state::SelectionAutoscroll {
            direction: crate::app::state::SelectionAutoscrollDirection::Down,
            last_mouse_screen_col: 0,
            last_mouse_screen_row: 23,
            inner_rect: ratatui::layout::Rect::new(0, 0, 80, 24),
        });

        state.handle_pane_died(second_id);

        // first_id still alive, workspace stays, but selection was on the dying pane
        assert!(state.selection.is_none());
        assert!(state.selection_autoscroll.is_none());
        assert_eq!(state.workspaces[0].panes.len(), 1);
        assert_eq!(state.workspaces[0].panes.keys().next().unwrap(), &first_id);
    }

    #[test]
    fn state_changed_updates_pane() {
        let mut state = app_with_workspaces(&["test"]);
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Working,
            visible_blocker: false,
            visible_idle: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        let terminal_id = state.workspaces[0]
            .panes
            .get(&pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        let terminal = state.terminals.get(&terminal_id).unwrap();
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(terminal.detected_agent, Some(Agent::Pi));
    }

    #[test]
    fn state_changed_idle_in_background_marks_unseen() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        let bg_pane_id = *state.workspaces[1].panes.keys().next().unwrap();

        // First set it to Working
        let bg_terminal_id = state.workspaces[1]
            .panes
            .get(&bg_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        state.terminals.get_mut(&bg_terminal_id).unwrap().state = AgentState::Working;

        // Now transition to Idle while in background
        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_idle: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        let pane = state.workspaces[1].panes.get(&bg_pane_id).unwrap();
        assert!(!pane.seen);
    }

    #[test]
    fn active_tab_completion_marks_pane_seen() {
        let mut state = app_with_workspaces(&["active"]);
        state.active = Some(0);
        state.outer_terminal_focus = Some(true);
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();
        let terminal_id = state.workspaces[0]
            .panes
            .get(&pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        state.terminals.get_mut(&terminal_id).unwrap().state = AgentState::Working;
        state.workspaces[0].panes.get_mut(&pane_id).unwrap().seen = false;

        state.handle_app_event(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_idle: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        let terminal = state.terminals.get(&terminal_id).unwrap();
        assert_eq!(terminal.state, AgentState::Idle);
        let pane = state.workspaces[0].panes.get(&pane_id).unwrap();
        assert!(pane.seen);
    }

    #[test]
    fn initial_idle_in_background_stays_seen() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        let bg_pane_id = *state.workspaces[1].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_idle: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        let pane = state.workspaces[1].panes.get(&bg_pane_id).unwrap();
        assert!(pane.seen);
    }

    #[test]
    fn waiting_sound_plays_even_in_active_workspace() {
        assert_eq!(
            notification_sound_for_state_change(true, AgentState::Working, AgentState::Blocked),
            Some(crate::sound::Sound::Request)
        );
    }

    #[test]
    fn done_sound_only_plays_in_background() {
        assert_eq!(
            notification_sound_for_state_change(false, AgentState::Working, AgentState::Idle),
            Some(crate::sound::Sound::Done)
        );
        assert_eq!(
            notification_sound_for_state_change(true, AgentState::Working, AgentState::Idle),
            None
        );
        assert_eq!(
            notification_sound_for_state_change(false, AgentState::Unknown, AgentState::Idle),
            None
        );
    }

    #[test]
    fn background_waiting_sets_attention_toast() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        let bg_pane_id = *state.workspaces[1].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
            visible_blocker: false,
            visible_idle: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        let toast = state.toast.as_ref().unwrap();
        assert_eq!(toast.kind, ToastKind::NeedsAttention);
        assert_eq!(toast.title, "pi needs attention");
        assert_eq!(toast.context, "background · 2");
    }

    #[test]
    fn delayed_background_waiting_schedules_before_toast() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        state.toast_config.delay_seconds = 1;
        let bg_pane_id = *state.workspaces[1].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
            visible_blocker: false,
            visible_idle: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        assert!(state.toast.is_none());
        assert!(state.pending_agent_notifications.contains_key(&bg_pane_id));

        let deadline = state.next_pending_agent_notification_deadline().unwrap();
        let deliveries = state.drain_due_agent_notifications(deadline);
        assert_eq!(deliveries.len(), 1);

        let toast = state.toast.as_ref().unwrap();
        assert_eq!(toast.kind, ToastKind::NeedsAttention);
        assert_eq!(toast.title, "pi needs attention");
        assert_eq!(toast.context, "background · 2");
        assert!(state.pending_agent_notifications.is_empty());
    }

    #[test]
    fn delayed_background_waiting_cancels_when_agent_resumes_working() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        state.toast_config.delay_seconds = 1;
        let bg_pane_id = *state.workspaces[1].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
            visible_blocker: false,
            visible_idle: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });
        let deadline = state.next_pending_agent_notification_deadline().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Working,
            visible_blocker: false,
            visible_idle: false,
            visible_working: true,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        assert!(state.pending_agent_notifications.is_empty());
        assert!(state.drain_due_agent_notifications(deadline).is_empty());
        assert!(state.toast.is_none());
    }

    #[test]
    fn delayed_background_waiting_is_suppressed_if_pane_becomes_active() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        state.toast_config.delay_seconds = 1;
        let bg_pane_id = *state.workspaces[1].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
            visible_blocker: false,
            visible_idle: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });
        let deadline = state.next_pending_agent_notification_deadline().unwrap();
        state.active = Some(1);

        assert!(state.drain_due_agent_notifications(deadline).is_empty());
        assert!(state.toast.is_none());
    }

    #[test]
    fn delayed_active_tab_unfocused_keeps_client_notification_available() {
        let mut state = app_with_workspaces(&["active"]);
        state.active = Some(0);
        state.outer_terminal_focus = Some(false);
        state.toast_config.delivery = crate::config::ToastDelivery::System;
        state.toast_config.delay_seconds = 1;
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
            visible_blocker: false,
            visible_idle: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        let deadline = state.next_pending_agent_notification_deadline().unwrap();
        let deliveries = state.drain_due_agent_notifications(deadline);

        assert_eq!(deliveries.len(), 1);
        assert!(deliveries[0].toast.is_none());
        assert!(deliveries[0].client_notification.is_some());
        assert!(state.toast.is_none());
    }

    #[test]
    fn delayed_background_waiting_is_cleared_when_pane_dies() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        state.toast_config.delay_seconds = 1;
        let bg_pane_id = *state.workspaces[1].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
            visible_blocker: false,
            visible_idle: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });
        let deadline = state.next_pending_agent_notification_deadline().unwrap();
        state.handle_app_event(AppEvent::PaneDied {
            pane_id: bg_pane_id,
        });

        assert!(state.pending_agent_notifications.is_empty());
        assert!(state.drain_due_agent_notifications(deadline).is_empty());
        assert!(state.toast.is_none());
    }

    #[test]
    fn hook_reported_unknown_agent_sets_toast_title_from_label() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        let bg_pane_id = *state.workspaces[1].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::HookStateReported {
            pane_id: bg_pane_id,
            source: "custom:hermes".into(),
            agent_label: "hermes".into(),
            state: AgentState::Blocked,
            message: None,
            custom_status: None,
            seq: None,
            session_ref: None,
        });

        let toast = state.toast.as_ref().unwrap();
        assert_eq!(toast.kind, ToastKind::NeedsAttention);
        assert_eq!(toast.title, "hermes needs attention");
        assert_eq!(toast.context, "background · 2");
    }

    #[test]
    fn visible_blocker_overrides_hook_working_and_notifies() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        let bg_pane_id = *state.workspaces[1].panes.keys().next().unwrap();
        let bg_terminal_id = state.workspaces[1]
            .panes
            .get(&bg_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Codex),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_idle: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });
        state.handle_app_event(AppEvent::HookStateReported {
            pane_id: bg_pane_id,
            source: "herdr:codex".into(),
            agent_label: "codex".into(),
            state: AgentState::Working,
            message: None,
            custom_status: None,
            seq: Some(1),
            session_ref: None,
        });
        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Codex),
            state: AgentState::Blocked,
            visible_blocker: true,
            visible_idle: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        let terminal = state.terminals.get(&bg_terminal_id).unwrap();
        assert_eq!(terminal.state, AgentState::Blocked);
        let toast = state.toast.as_ref().unwrap();
        assert_eq!(toast.kind, ToastKind::NeedsAttention);
        assert_eq!(toast.title, "codex needs attention");
    }

    #[test]
    fn reserved_native_state_report_does_not_override_screen_state() {
        let mut state = app_with_workspaces(&["active"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();
        let terminal_id = state.workspaces[0]
            .panes
            .get(&pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Claude),
            state: AgentState::Working,
            visible_blocker: false,
            visible_idle: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });
        state.handle_app_event(AppEvent::HookStateReported {
            pane_id,
            source: "herdr:claude".into(),
            agent_label: "claude".into(),
            state: AgentState::Blocked,
            message: None,
            custom_status: None,
            seq: Some(1),
            session_ref: crate::agent_resume::AgentSessionRef::id("claude-session"),
        });
        let terminal = state.terminals.get(&terminal_id).unwrap();
        assert_eq!(terminal.state, AgentState::Working);
        assert!(terminal.hook_authority.is_none());
        assert!(terminal.persisted_agent_session.is_some());

        state.handle_app_event(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Claude),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_idle: true,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        let terminal = state.terminals.get(&terminal_id).unwrap();
        assert_eq!(terminal.state, AgentState::Idle);
        assert!(state.toast.is_none());
    }

    #[test]
    fn reserved_native_release_report_does_not_clear_screen_state() {
        let mut state = app_with_workspaces(&["active"]);
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();
        let terminal_id = state.workspaces[0]
            .panes
            .get(&pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Claude),
            state: AgentState::Working,
            visible_blocker: false,
            visible_idle: false,
            visible_working: true,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });
        state.handle_app_event(AppEvent::HookAgentReleased {
            pane_id,
            source: "herdr:claude".into(),
            agent_label: "claude".into(),
            known_agent: Some(Agent::Claude),
            seq: Some(1),
        });

        let terminal = state.terminals.get(&terminal_id).unwrap();
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(terminal.detected_agent, Some(Agent::Claude));
    }

    #[test]
    fn hidden_session_ref_only_update_marks_session_dirty_without_visible_update() {
        let mut state = app_with_workspaces(&["active"]);
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();

        let first_updates = state.handle_app_event(AppEvent::HookStateReported {
            pane_id,
            source: "herdr:pi".into(),
            agent_label: "pi".into(),
            state: AgentState::Working,
            message: None,
            custom_status: None,
            seq: Some(20),
            session_ref: crate::agent_resume::AgentSessionRef::path("/tmp/one.jsonl"),
        });
        assert_eq!(first_updates.len(), 1);
        state.session_dirty = false;

        let second_updates = state.handle_app_event(AppEvent::HookStateReported {
            pane_id,
            source: "herdr:pi".into(),
            agent_label: "pi".into(),
            state: AgentState::Working,
            message: None,
            custom_status: None,
            seq: Some(21),
            session_ref: crate::agent_resume::AgentSessionRef::path("/tmp/two.jsonl"),
        });

        assert!(second_updates.is_empty());
        assert!(state.session_dirty);
    }

    #[test]
    fn background_idle_sets_finished_toast() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        let bg_pane_id = *state.workspaces[1].panes.keys().next().unwrap();
        let bg_terminal_id = state.workspaces[1]
            .panes
            .get(&bg_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        state.terminals.get_mut(&bg_terminal_id).unwrap().state = AgentState::Working;

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Droid),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_idle: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        let toast = state.toast.as_ref().unwrap();
        assert_eq!(toast.kind, ToastKind::Finished);
        assert_eq!(toast.title, "droid finished");
        assert_eq!(toast.context, "background · 2");
        let target = toast.target.as_ref().expect("toast target");
        assert_eq!(&target.workspace_id, &state.workspaces[1].id);
        assert_eq!(target.pane_id, bg_pane_id);
    }

    #[test]
    fn background_toast_includes_tab_name_when_workspace_has_multiple_tabs() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        state.workspaces[1].tabs[0].set_custom_name("main".into());
        let second_tab = state.workspaces[1].test_add_tab(Some("logs"));
        state.ensure_test_terminals();
        let bg_pane_id = state.workspaces[1].tabs[second_tab].root_pane;

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
            visible_blocker: false,
            visible_idle: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        let toast = state.toast.as_ref().unwrap();
        assert_eq!(toast.kind, ToastKind::NeedsAttention);
        assert_eq!(toast.title, "pi needs attention");
        assert_eq!(toast.context, "background · 2 · logs");
    }

    #[test]
    fn background_tab_in_active_workspace_still_sets_toast() {
        let mut state = app_with_workspaces(&["active"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        state.workspaces[0].tabs[0].set_custom_name("main".into());
        let second_tab = state.workspaces[0].test_add_tab(Some("logs"));
        state.ensure_test_terminals();
        let bg_pane_id = state.workspaces[0].tabs[second_tab].root_pane;

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
            visible_blocker: false,
            visible_idle: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        let toast = state.toast.as_ref().unwrap();
        assert_eq!(toast.kind, ToastKind::NeedsAttention);
        assert_eq!(toast.title, "pi needs attention");
        assert_eq!(toast.context, "active · 1 · logs");
    }

    #[test]
    fn active_workspace_active_tab_does_not_set_toast() {
        let mut state = app_with_workspaces(&["active"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
            visible_blocker: false,
            visible_idle: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        assert!(state.toast.is_none());
    }

    #[test]
    fn active_workspace_active_tab_keeps_herdr_toast_suppressed_when_outer_terminal_is_unfocused() {
        let mut state = app_with_workspaces(&["active"]);
        state.active = Some(0);
        state.outer_terminal_focus = Some(false);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
            visible_blocker: false,
            visible_idle: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        assert!(state.toast.is_none());
    }

    #[test]
    fn active_tab_suppression_preserves_unknown_focus_behavior() {
        assert!(active_tab_suppresses_notifications(true, None));
        assert!(active_tab_suppresses_notifications(true, Some(true)));
        assert!(!active_tab_suppresses_notifications(true, Some(false)));
        assert!(!active_tab_suppresses_notifications(false, None));
    }

    #[test]
    fn update_ready_sets_manual_update_toast() {
        let mut state = AppState::test_new();
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;

        let updates = state.handle_app_event(AppEvent::UpdateReady {
            version: "0.5.0".into(),
            install_command: "herdr update".into(),
        });

        assert!(updates.is_empty());
        assert_eq!(state.update_available.as_deref(), Some("0.5.0"));
        assert!(state.latest_release_notes_available);
        assert!(state.update_dismissed);
        let toast = state.toast.as_ref().expect("update toast");
        assert_eq!(toast.kind, ToastKind::UpdateInstalled);
        assert_eq!(toast.title, "v0.5.0 available");
        assert_eq!(
            toast.context,
            "detach, run `herdr update`, then follow its restart guidance"
        );
    }

    #[test]
    fn update_ready_uses_event_install_command_in_toast() {
        let mut state = AppState::test_new();
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;

        state.handle_app_event(AppEvent::UpdateReady {
            version: "0.5.0".into(),
            install_command: "brew update && brew upgrade herdr".into(),
        });

        assert_eq!(
            state.update_install_command,
            "brew update && brew upgrade herdr"
        );
        let toast = state.toast.as_ref().expect("update toast");
        assert_eq!(
            toast.context,
            "detach, run `brew update && brew upgrade herdr`, then restart this Herdr session when ready"
        );
    }

    #[test]
    fn toggle_zoom_works() {
        let mut state = app_with_workspaces(&["test"]);
        state.workspaces[0].test_split(Direction::Horizontal);

        assert!(!state.workspaces[0].zoomed);
        state.toggle_zoom();
        assert!(state.workspaces[0].zoomed);
        state.toggle_zoom();
        assert!(!state.workspaces[0].zoomed);
    }

    #[test]
    fn toggle_zoom_single_pane_noop() {
        let mut state = app_with_workspaces(&["test"]);
        state.toggle_zoom();
        assert!(!state.workspaces[0].zoomed);
    }

    #[test]
    fn navigate_pane_changes_focus_while_zoomed() {
        let mut state = app_with_workspaces(&["test"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let right = state.workspaces[0].test_split(Direction::Horizontal);
        state.workspaces[0].layout.focus_pane(root);
        state.workspaces[0].zoomed = true;
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 100, 20));

        assert_eq!(state.view.pane_infos.len(), 1);
        assert_eq!(state.view.pane_infos[0].id, root);

        state.navigate_pane(NavDirection::Right);
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 100, 20));

        assert!(state.workspaces[0].zoomed);
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(right));
        assert_eq!(state.view.pane_infos.len(), 1);
        assert_eq!(state.view.pane_infos[0].id, right);
        assert!(state.view.pane_infos[0].inner_rect.x > state.view.pane_infos[0].rect.x);
    }

    #[test]
    fn swap_pane_direction_preserves_focus_and_swaps_layout_cells() {
        let mut state = app_with_workspaces(&["test"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let right = state.workspaces[0].test_split(Direction::Horizontal);
        state.workspaces[0].layout.focus_pane(root);
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let before_root_rect = state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == root)
            .unwrap()
            .rect;
        let before_right_rect = state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == right)
            .unwrap()
            .rect;

        assert!(state.swap_pane(NavDirection::Right));
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 100, 20));

        assert_eq!(state.workspaces[0].focused_pane_id(), Some(root));
        assert_eq!(
            state
                .view
                .pane_infos
                .iter()
                .find(|info| info.id == root)
                .unwrap()
                .rect,
            before_right_rect
        );
        assert_eq!(
            state
                .view
                .pane_infos
                .iter()
                .find(|info| info.id == right)
                .unwrap()
                .rect,
            before_root_rect
        );
    }

    #[test]
    fn swap_pane_direction_stays_zoomed_and_mutates_hidden_layout() {
        let mut state = app_with_workspaces(&["test"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let right = state.workspaces[0].test_split(Direction::Horizontal);
        state.workspaces[0].layout.focus_pane(root);
        state.workspaces[0].zoomed = true;
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 100, 20));

        assert!(state.swap_pane(NavDirection::Right));
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 100, 20));

        assert!(state.workspaces[0].zoomed);
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(root));
        assert_eq!(state.view.pane_infos.len(), 1);
        assert_eq!(state.view.pane_infos[0].id, root);

        state.workspaces[0].zoomed = false;
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let root_rect = state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == root)
            .unwrap()
            .rect;
        let right_rect = state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == right)
            .unwrap()
            .rect;

        assert!(root_rect.x > right_rect.x);
    }

    #[test]
    fn close_pane_removes_from_workspace() {
        let mut state = app_with_workspaces(&["test"]);
        state.workspaces[0].test_split(Direction::Horizontal);
        assert_eq!(state.workspaces[0].panes.len(), 2);

        state.close_pane();
        assert_eq!(state.workspaces[0].panes.len(), 1);
    }

    #[test]
    fn close_pane_removes_unattached_terminal_state() {
        let mut state = app_with_workspaces(&["test"]);
        let pane_id = state.workspaces[0].test_split(Direction::Horizontal);
        state.ensure_test_terminals();
        let terminal_id = state.terminal_id_for_pane(0, pane_id).unwrap();

        state.close_pane();

        assert!(!state.terminals.contains_key(&terminal_id));
    }

    #[test]
    fn close_tab_removes_unattached_terminal_states() {
        let mut state = app_with_workspaces(&["test"]);
        let tab_idx = state.workspaces[0].test_add_tab(Some("logs"));
        state.ensure_test_terminals();
        state.workspaces[0].switch_tab(tab_idx);
        let pane_id = state.workspaces[0].tabs[tab_idx].root_pane;
        let terminal_id = state.terminal_id_for_pane(0, pane_id).unwrap();

        state.close_tab();

        assert!(!state.terminals.contains_key(&terminal_id));
    }

    #[test]
    fn close_workspace_removes_unattached_terminal_states() {
        let mut state = app_with_workspaces(&["one", "two"]);
        let terminal_id = state
            .terminal_id_for_pane(0, state.workspaces[0].tabs[0].root_pane)
            .unwrap();

        state.close_selected_workspace();

        assert!(!state.terminals.contains_key(&terminal_id));
    }

    #[test]
    fn close_tab_closes_active_workspace_not_selected_workspace() {
        let mut state = app_with_workspaces(&["selected", "active"]);
        let active_terminal_id = state
            .terminal_id_for_pane(1, state.workspaces[1].tabs[0].root_pane)
            .unwrap();
        state.active = Some(1);
        state.selected = 0;

        state.close_tab();

        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].display_name(), "selected");
        assert!(!state.terminals.contains_key(&active_terminal_id));
    }

    #[test]
    fn close_pane_last_pane_closes_active_workspace_not_selected_workspace() {
        let mut state = app_with_workspaces(&["selected", "active"]);
        let active_terminal_id = state
            .terminal_id_for_pane(1, state.workspaces[1].tabs[0].root_pane)
            .unwrap();
        state.active = Some(1);
        state.selected = 0;

        state.close_pane();

        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].display_name(), "selected");
        assert!(!state.terminals.contains_key(&active_terminal_id));
    }

    #[test]
    fn close_tab_in_linked_worktree_closes_workspace_only() {
        let mut state = app_with_workspaces(&["selected", "active"]);
        mark_linked_worktree(&mut state, 1);
        state.active = Some(1);
        state.selected = 0;

        state.close_tab();

        assert_eq!(state.request_remove_linked_worktree, None);
        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].display_name(), "selected");
    }

    #[test]
    fn close_pane_last_pane_in_linked_worktree_closes_workspace_only() {
        let mut state = app_with_workspaces(&["selected", "active"]);
        mark_linked_worktree(&mut state, 1);
        state.active = Some(1);
        state.selected = 0;

        state.close_pane();

        assert_eq!(state.request_remove_linked_worktree, None);
        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].display_name(), "selected");
    }

    #[test]
    fn close_pane_last_pane_in_parent_worktree_group_closes_when_confirmation_disabled() {
        let mut state = app_with_workspaces(&["parent", "child", "notes"]);
        mark_parent_worktree(&mut state, 0);
        mark_linked_worktree(&mut state, 1);
        state.confirm_close = false;
        state.active = Some(0);
        state.selected = 0;

        let deferred = state.close_pane();

        assert!(!deferred);
        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].display_name(), "notes");
    }
}
