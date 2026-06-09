use ratatui::layout::Rect;

use crate::app::AppState;
use crate::detect::AgentState;
use crate::terminal::TerminalRuntimeRegistry;

pub(crate) struct AgentPanelEntry {
    pub ws_idx: usize,
    pub tab_idx: usize,
    pub pane_id: crate::layout::PaneId,
    pub primary_label: String,
    pub primary_tab_label: Option<String>,
    pub agent_label: Option<String>,
    pub state: AgentState,
    pub seen: bool,
    pub custom_status: Option<String>,
    pub state_labels: std::collections::HashMap<String, String>,
    /// Raw start time of the current `Working` stretch; formatted per frame.
    pub working_since: Option<std::time::Instant>,
}

fn sidebar_section_heights(total_h: u16, split_ratio: f32) -> (u16, u16) {
    if total_h == 0 {
        return (0, 0);
    }

    if total_h < 6 {
        let ws_h = total_h.div_ceil(2);
        return (ws_h, total_h.saturating_sub(ws_h));
    }

    let ratio = split_ratio.clamp(0.1, 0.9);
    let ws_h = ((total_h as f32) * ratio).round() as u16;
    let ws_h = ws_h.clamp(3, total_h.saturating_sub(3));
    let detail_h = total_h.saturating_sub(ws_h);
    (ws_h, detail_h)
}

pub(crate) fn expanded_sidebar_sections(area: Rect, split_ratio: f32) -> (Rect, Rect) {
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.width == 0 || content.height == 0 {
        return (Rect::default(), Rect::default());
    }

    let (ws_h, detail_h) = sidebar_section_heights(content.height, split_ratio);
    let ws_area = Rect::new(content.x, content.y, content.width, ws_h);
    let detail_area = Rect::new(content.x, content.y + ws_h, content.width, detail_h);
    (ws_area, detail_area)
}

/// Every agent across all workspaces. An agent exists iff its worktree is open,
/// so this yields exactly one entry per open worktree — collapsing a worktree's
/// extra rows (review/terminal) and any extra tabs into a single agent entry,
/// represented by the worktree's agent (root) pane. Used by the keyboard-first
/// home agents-half.
pub(crate) fn agent_panel_entries_all(app: &AppState) -> Vec<AgentPanelEntry> {
    let empty_runtimes = TerminalRuntimeRegistry::new();
    all_workspace_agent_panel_entries(app, &empty_runtimes)
}

fn all_workspace_agent_panel_entries(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Vec<AgentPanelEntry> {
    app.workspaces
        .iter()
        .enumerate()
        .filter_map(|(ws_idx, ws)| {
            // One entry per OPEN WORKTREE: skip workspaces with no worktree.
            ws.worktree_space()?;
            let multi_tab = ws.tabs.len() > 1;
            let workspace_label = ws.display_name_from(&app.terminals, terminal_runtimes);
            // Represent the worktree by its agent (root) pane; fall back to the
            // first agent pane detail if the active tab's root isn't one.
            let details = ws.pane_details(&app.terminals);
            let agent_pane = ws.agent_pane();
            let detail = agent_pane
                .and_then(|id| details.iter().find(|d| d.pane_id == id))
                .or_else(|| details.first())?;
            Some(AgentPanelEntry {
                ws_idx,
                tab_idx: detail.tab_idx,
                pane_id: detail.pane_id,
                primary_label: workspace_label,
                primary_tab_label: multi_tab.then(|| detail.tab_label.clone()),
                agent_label: Some(detail.agent_label.clone()),
                state: detail.state,
                seen: detail.seen,
                custom_status: detail.custom_status.clone(),
                state_labels: detail.state_labels.clone(),
                working_since: detail.working_since,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expanded_sidebar_sections_handle_tiny_heights() {
        let (ws_area, detail_area) = expanded_sidebar_sections(Rect::new(0, 0, 20, 5), 0.9);

        assert_eq!(ws_area, Rect::new(0, 0, 19, 3));
        assert_eq!(detail_area, Rect::new(0, 3, 19, 2));
    }
}
