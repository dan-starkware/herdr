use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::scrollbar::{render_scrollbar, should_show_scrollbar};
use super::status::{agent_icon, state_dot, state_label, state_label_color};
use crate::app::state::{AgentPanelSort, Palette};
use crate::app::{AppState, Mode};
use crate::detect::AgentState;
use crate::terminal::TerminalRuntimeRegistry;

const WORKSPACE_SECTION_HEADER_ROWS: u16 = 2;
const AGENT_PANEL_HEADER_ROWS: u16 = 3;

pub(crate) struct AgentPanelEntry {
    pub ws_idx: usize,
    pub tab_idx: usize,
    pub pane_id: crate::layout::PaneId,
    pub primary_label: String,
    pub primary_tab_label: Option<String>,
    pub agent_label: Option<String>,
    pub state: AgentState,
    pub seen: bool,
    pub last_agent_state_change_seq: Option<u64>,
    pub custom_status: Option<String>,
    pub state_labels: std::collections::HashMap<String, String>,
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

pub(crate) fn sidebar_section_divider_rect(area: Rect, split_ratio: f32) -> Rect {
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.width == 0 || content.height < 6 {
        return Rect::default();
    }

    let (ws_h, _) = sidebar_section_heights(content.height, split_ratio);
    Rect::new(content.x, content.y + ws_h, content.width, 1)
}

fn agent_panel_sort_label(sort: AgentPanelSort) -> &'static str {
    match sort {
        AgentPanelSort::Spaces => "grouped",
        AgentPanelSort::Priority => "priority",
    }
}

pub(crate) fn agent_panel_toggle_rect(area: Rect, sort: AgentPanelSort) -> Rect {
    if area.width == 0 || area.height < 2 {
        return Rect::default();
    }

    let label = agent_panel_sort_label(sort);
    let width = label.chars().count() as u16;
    Rect::new(
        area.x + area.width.saturating_sub(width),
        area.y + 1,
        width,
        1,
    )
}

pub(crate) fn agent_panel_entries(app: &AppState) -> Vec<AgentPanelEntry> {
    agent_panel_entries_with_runtimes(app, None)
}

pub(crate) fn agent_panel_entries_from(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Vec<AgentPanelEntry> {
    agent_panel_entries_with_runtimes(app, Some(terminal_runtimes))
}

fn agent_panel_entries_with_runtimes(
    app: &AppState,
    terminal_runtimes: Option<&TerminalRuntimeRegistry>,
) -> Vec<AgentPanelEntry> {
    let empty_runtimes;
    let terminal_runtimes = match terminal_runtimes {
        Some(terminal_runtimes) => terminal_runtimes,
        None => {
            empty_runtimes = TerminalRuntimeRegistry::new();
            &empty_runtimes
        }
    };

    let mut entries: Vec<_> = app
        .workspaces
        .iter()
        .enumerate()
        .flat_map(|(ws_idx, ws)| {
            let multi_tab = ws.tabs.len() > 1;
            // Linked worktrees lead with their branch (the self-identifying key
            // shared with the spaces panel); other workspaces keep the
            // live-cwd-derived display name.
            let is_linked_worktree = ws
                .worktree_space()
                .is_some_and(|space| space.is_linked_worktree);
            let workspace_label = is_linked_worktree
                .then(|| ws.branch())
                .flatten()
                .unwrap_or_else(|| ws.display_name_from(&app.terminals, terminal_runtimes));
            ws.pane_details(&app.terminals)
                .into_iter()
                .map(move |detail| AgentPanelEntry {
                    ws_idx,
                    tab_idx: detail.tab_idx,
                    pane_id: detail.pane_id,
                    primary_label: workspace_label.clone(),
                    primary_tab_label: multi_tab.then_some(detail.tab_label),
                    agent_label: Some(detail.agent_label),
                    state: detail.state,
                    seen: detail.seen,
                    last_agent_state_change_seq: detail.last_agent_state_change_seq,
                    custom_status: detail.custom_status,
                    state_labels: detail.state_labels,
                })
        })
        .collect();

    if matches!(app.agent_panel_sort, AgentPanelSort::Priority) {
        entries.sort_by_key(|entry| {
            (
                std::cmp::Reverse(workspace_attention_priority(entry.state, entry.seen)),
                std::cmp::Reverse(entry.last_agent_state_change_seq),
            )
        });
    }

    entries
}

pub(super) fn agent_panel_status_key(state: AgentState, seen: bool) -> &'static str {
    match (state, seen) {
        (AgentState::Idle, false) => "done",
        (AgentState::Idle, true) => "idle",
        (AgentState::Working, _) => "working",
        (AgentState::Blocked, _) => "blocked",
        (AgentState::Unknown, _) => "unknown",
    }
}

fn truncate_text(text: &str, max_width: usize) -> String {
    let len = text.chars().count();
    if len <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_string();
    }
    let prefix: String = text.chars().take(max_width.saturating_sub(1)).collect();
    format!("{prefix}…")
}

fn workspace_row_height(ws: &crate::workspace::Workspace) -> u16 {
    if ws.branch().is_some() {
        2
    } else {
        1
    }
}

/// Spans for a worktree child's diff-stats sub-line: committed (`base...HEAD`)
/// then uncommitted working changes, e.g. `+412 -98  ·  ~18 -3` or `… · clean`.
/// Renders a muted placeholder until the first git refresh populates the stats.
/// `is_last` controls the tree rail: the vertical connector continues (`│`)
/// under non-last children and is blank under the last child in a group.
fn worktree_stats_spans(
    stats: Option<crate::workspace::WorktreeDiffStats>,
    is_last: bool,
    p: &Palette,
) -> Vec<Span<'static>> {
    let dim = Style::default().fg(p.overlay0).add_modifier(Modifier::DIM);
    // Align under the branch line's connector glyph (col 1), keeping the total
    // leading width at 7 columns so the stats line up with the label above.
    let rail = if is_last { " " } else { "│" };
    let indent = || {
        vec![
            Span::styled(" ", Style::default()),
            Span::styled(rail, Style::default().fg(p.overlay0)),
            Span::styled("     ", Style::default()),
        ]
    };
    let Some(stats) = stats else {
        let mut spans = indent();
        spans.push(Span::styled("—", dim));
        return spans;
    };
    let mut spans = indent();
    spans.extend([
        Span::styled(
            format!("+{}", stats.committed.added),
            Style::default().fg(p.green),
        ),
        Span::styled(" ", Style::default()),
        Span::styled(
            format!("-{}", stats.committed.removed),
            Style::default().fg(p.red),
        ),
        Span::styled("  ·  ", dim),
    ]);
    if stats.wip.is_empty() {
        spans.push(Span::styled("clean", dim));
    } else {
        spans.push(Span::styled(
            format!("~{}", stats.wip.added),
            Style::default().fg(p.yellow),
        ));
        spans.push(Span::styled(" ", Style::default()));
        spans.push(Span::styled(
            format!("-{}", stats.wip.removed),
            Style::default().fg(p.red),
        ));
    }
    spans
}

fn workspace_attention_priority(state: AgentState, seen: bool) -> u8 {
    match (state, seen) {
        (AgentState::Blocked, _) => 4,
        (AgentState::Idle, false) => 3,
        (AgentState::Working, _) => 2,
        (AgentState::Idle, true) => 1,
        (AgentState::Unknown, _) => 0,
    }
}

/// Inline agent + status spans appended to a worktree child's branch line.
fn worktree_agent_spans(
    primary: &crate::workspace::PaneDetail,
    badge_state: AgentState,
    badge_seen: bool,
    extra: usize,
    expanded: bool,
    p: &Palette,
) -> Vec<Span<'static>> {
    let (dot, dot_style) = state_dot(badge_state, badge_seen, p);
    let label = primary
        .state_labels
        .get(agent_panel_status_key(primary.state, primary.seen))
        .map(String::as_str)
        .unwrap_or_else(|| state_label(primary.state, primary.seen))
        .to_string();
    let mut spans = vec![
        Span::styled("  ", Style::default()),
        Span::styled(dot, dot_style),
        Span::styled(" ", Style::default()),
        Span::styled(
            label,
            Style::default().fg(state_label_color(primary.state, primary.seen, p)),
        ),
        Span::styled(" ", Style::default()),
        Span::styled(primary.agent_label.clone(), Style::default().fg(p.overlay1)),
    ];
    if extra > 0 {
        let caret = if expanded { " ▾" } else { " ▸" };
        spans.push(Span::styled(
            format!("{caret} +{extra}"),
            Style::default().fg(p.overlay0),
        ));
    }
    spans
}

fn space_aggregate_state(app: &AppState, key: &str) -> (AgentState, bool) {
    app.workspaces
        .iter()
        .filter(|ws| ws.worktree_space().is_some_and(|space| space.key == key))
        .map(|ws| ws.aggregate_state(&app.terminals))
        .max_by_key(|(state, seen)| workspace_attention_priority(*state, *seen))
        .unwrap_or((AgentState::Unknown, true))
}

pub(crate) fn workspace_parent_group_state(
    app: &AppState,
    ws_idx: usize,
) -> Option<(String, bool)> {
    let space = app.workspaces.get(ws_idx)?.worktree_space()?;
    if space.is_linked_worktree {
        return None;
    }
    let member_count = app
        .workspaces
        .iter()
        .filter(|ws| {
            ws.worktree_space()
                .is_some_and(|member| member.key == space.key)
        })
        .count();
    (member_count >= 2).then(|| {
        (
            space.key.clone(),
            app.collapsed_space_keys.contains(&space.key),
        )
    })
}

fn grouped_child_display_label(label: &str, branch: Option<&str>, has_custom_name: bool) -> String {
    if has_custom_name {
        return label.to_string();
    }
    let Some(branch) = branch else {
        return label.to_string();
    };
    branch
        .strip_prefix("worktree/")
        .unwrap_or(branch)
        .to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkspaceListEntry {
    Workspace {
        ws_idx: usize,
        indented: bool,
    },
    /// A synthesized repo group header shown above linked worktrees whose
    /// primary checkout is not open as its own workspace. Carries the repo
    /// group key so render/collapse can resolve its label and aggregate state.
    RepoHeader {
        key: String,
    },
}

/// Resolve the repo grouping coordinates for a workspace: the git common-dir
/// key, the repo root, and whether this workspace is a linked worktree.
/// Worktree membership takes precedence over the cached git space.
/// Row height of a synthesized repo group header (the repo name only).
const REPO_HEADER_ROW_HEIGHT: u16 = 1;

/// Row height of an indented worktree child: branch line + diff-stats sub-line.
const CHILD_ROW_HEIGHT: u16 = 2;

fn workspace_repo_group(
    ws: &crate::workspace::Workspace,
) -> Option<(&str, &std::path::Path, bool)> {
    if let Some(space) = ws.worktree_space() {
        return Some((&space.key, &space.repo_root, space.is_linked_worktree));
    }
    if let Some(space) = ws.git_space() {
        return Some((&space.key, &space.repo_root, space.is_linked_worktree));
    }
    None
}

fn next_entry_is_indented_workspace(entries: &[WorkspaceListEntry], idx: usize) -> bool {
    matches!(
        entries.get(idx.saturating_add(1)),
        Some(WorkspaceListEntry::Workspace { indented: true, .. })
    )
}

pub(crate) fn normalized_workspace_scroll(app: &AppState, area: Rect, requested: usize) -> usize {
    let ws_area = workspace_list_rect(area, app.sidebar_section_split);
    let body = workspace_list_body_rect(ws_area, false);
    if body.height == 0 {
        return requested;
    }

    let entry_count = workspace_list_entries(app).len();
    if entry_count == 0 {
        0
    } else {
        requested.min(entry_count.saturating_sub(1))
    }
}

pub(crate) fn workspace_list_entries(app: &AppState) -> Vec<WorkspaceListEntry> {
    // Linked worktree children grouped by repo (git common-dir) key, plus the
    // common repo root per key used to disambiguate the true primary checkout.
    let mut children_by_key = std::collections::HashMap::<&str, Vec<usize>>::new();
    let mut repo_root_by_key = std::collections::HashMap::<&str, &std::path::Path>::new();
    for (ws_idx, ws) in app.workspaces.iter().enumerate() {
        if let Some((key, repo_root, true)) = workspace_repo_group(ws) {
            children_by_key.entry(key).or_default().push(ws_idx);
            repo_root_by_key.entry(key).or_insert(repo_root);
        }
    }

    // A repo forms a group as soon as it has at least one linked worktree.
    // The primary header is the non-linked workspace whose repo root matches the
    // group's (so an unrelated workspace that merely shares a key is excluded);
    // groups without an open primary get a synthesized header.
    let mut primary_by_key = std::collections::HashMap::<&str, usize>::new();
    for (ws_idx, ws) in app.workspaces.iter().enumerate() {
        if let Some((key, repo_root, false)) = workspace_repo_group(ws) {
            if children_by_key.contains_key(key) && repo_root_by_key.get(key) == Some(&repo_root) {
                primary_by_key.entry(key).or_insert(ws_idx);
            }
        }
    }

    let role = |ws_idx: usize| -> Option<(&str, bool)> {
        let (key, _, linked) = workspace_repo_group(app.workspaces.get(ws_idx)?)?;
        if linked && children_by_key.contains_key(key) {
            Some((key, true)) // child
        } else if !linked && primary_by_key.get(key) == Some(&ws_idx) {
            Some((key, false)) // primary header
        } else {
            None // standalone
        }
    };

    let visible_group_idx = if matches!(app.mode, Mode::Navigate) {
        Some(app.selected)
    } else {
        app.active
    };
    let active_group = visible_group_idx.and_then(&role).map(|(key, _)| key);

    let mut emitted_groups = std::collections::HashSet::<&str>::new();
    let mut entries = Vec::new();
    for ws_idx in 0..app.workspaces.len() {
        let Some((key, _)) = role(ws_idx) else {
            entries.push(WorkspaceListEntry::Workspace {
                ws_idx,
                indented: false,
            });
            continue;
        };
        if !emitted_groups.insert(key) {
            continue;
        }

        // Header: the open primary checkout, or a synthesized repo header.
        let primary_idx = primary_by_key.get(key).copied();
        match primary_idx {
            Some(parent_idx) => entries.push(WorkspaceListEntry::Workspace {
                ws_idx: parent_idx,
                indented: false,
            }),
            None => entries.push(WorkspaceListEntry::RepoHeader {
                key: key.to_string(),
            }),
        }

        let collapsed = app.collapsed_space_keys.contains(key);
        let children = &children_by_key[key];
        if collapsed {
            if let Some(active_idx) = visible_group_idx
                .filter(|idx| Some(*idx) != primary_idx)
                .filter(|idx| children.contains(idx))
                .filter(|_| active_group == Some(key))
            {
                entries.push(WorkspaceListEntry::Workspace {
                    ws_idx: active_idx,
                    indented: true,
                });
            }
        } else {
            for child_idx in children {
                entries.push(WorkspaceListEntry::Workspace {
                    ws_idx: *child_idx,
                    indented: true,
                });
            }
        }
    }
    entries
}

pub(crate) fn workspace_list_rect(area: Rect, split_ratio: f32) -> Rect {
    let (ws_area, _) = expanded_sidebar_sections(area, split_ratio);
    ws_area
}

pub(crate) fn workspace_list_body_rect(area: Rect, has_scrollbar: bool) -> Rect {
    if area.width == 0 || area.height <= WORKSPACE_SECTION_HEADER_ROWS {
        return Rect::default();
    }

    let body_y = area.y.saturating_add(WORKSPACE_SECTION_HEADER_ROWS);
    let footer_y = area.y + area.height.saturating_sub(1);
    let body_height = footer_y.saturating_sub(body_y);
    let body_width = area.width.saturating_sub(u16::from(has_scrollbar));
    Rect::new(area.x, body_y, body_width, body_height)
}

fn workspace_list_visible_count(app: &AppState, area: Rect, scroll: usize) -> usize {
    let body = workspace_list_body_rect(area, false);
    if body.width == 0 || body.height == 0 {
        return 0;
    }

    let mut used_rows = 0u16;
    let mut visible = 0usize;
    let entries = workspace_list_entries(app);
    for (entry_idx, entry) in entries.iter().enumerate().skip(scroll) {
        let needed = match entry {
            WorkspaceListEntry::Workspace { ws_idx, indented } => {
                let Some(ws) = app.workspaces.get(*ws_idx) else {
                    continue;
                };
                let row_height = if *indented {
                    CHILD_ROW_HEIGHT
                } else {
                    workspace_row_height(ws)
                };
                let gap = u16::from(
                    !(*indented && next_entry_is_indented_workspace(&entries, entry_idx)),
                );
                row_height.saturating_add(gap)
            }
            // Synthesized repo header: a single name row plus a trailing gap.
            WorkspaceListEntry::RepoHeader { .. } => REPO_HEADER_ROW_HEIGHT.saturating_add(1),
        };
        if used_rows.saturating_add(needed) > body.height {
            break;
        }
        used_rows = used_rows.saturating_add(needed);
        visible += 1;
    }
    visible
}

pub(crate) fn workspace_list_scroll_metrics(
    app: &AppState,
    area: Rect,
) -> crate::pane::ScrollMetrics {
    let entries = workspace_list_entries(app);
    let total_rows = entries.len();
    let scroll = app.workspace_scroll.min(total_rows.saturating_sub(1));
    let viewport_rows = workspace_list_visible_count(app, area, scroll);
    let max_offset_from_bottom = total_rows.saturating_sub(viewport_rows);
    let offset_from_bottom = total_rows
        .saturating_sub(scroll)
        .saturating_sub(viewport_rows);

    crate::pane::ScrollMetrics {
        offset_from_bottom,
        max_offset_from_bottom,
        viewport_rows,
    }
}

pub(crate) fn workspace_list_scrollbar_rect(app: &AppState, area: Rect) -> Option<Rect> {
    let metrics = workspace_list_scroll_metrics(app, area);
    let body = workspace_list_body_rect(area, true);
    (should_show_scrollbar(metrics) && body.width > 0 && body.height > 0).then_some(Rect::new(
        area.x + area.width.saturating_sub(1),
        body.y,
        1,
        body.height,
    ))
}

pub(crate) fn agent_panel_body_rect(area: Rect, has_scrollbar: bool) -> Rect {
    if area.width == 0 || area.height <= AGENT_PANEL_HEADER_ROWS {
        return Rect::default();
    }

    let body_y = area.y.saturating_add(AGENT_PANEL_HEADER_ROWS);
    let body_height = (area.y + area.height).saturating_sub(body_y);
    let body_width = area.width.saturating_sub(u16::from(has_scrollbar));
    Rect::new(area.x, body_y, body_width, body_height)
}

fn agent_panel_visible_count(area: Rect) -> usize {
    let body = agent_panel_body_rect(area, false);
    if body.width == 0 || body.height < 2 {
        return 0;
    }

    let mut used_rows = 0u16;
    let mut visible = 0usize;
    while used_rows.saturating_add(2) <= body.height {
        used_rows = used_rows.saturating_add(2);
        visible += 1;
        if used_rows < body.height {
            used_rows = used_rows.saturating_add(1);
        }
    }
    visible
}

pub(crate) fn agent_panel_scroll_metrics(app: &AppState, area: Rect) -> crate::pane::ScrollMetrics {
    let viewport_rows = agent_panel_visible_count(area);
    let total_rows = agent_panel_entries(app).len();
    let max_offset_from_bottom = total_rows.saturating_sub(viewport_rows);
    let offset_from_bottom = total_rows
        .saturating_sub(app.agent_panel_scroll)
        .saturating_sub(viewport_rows);

    crate::pane::ScrollMetrics {
        offset_from_bottom,
        max_offset_from_bottom,
        viewport_rows,
    }
}

pub(crate) fn agent_panel_scrollbar_rect(app: &AppState, area: Rect) -> Option<Rect> {
    let metrics = agent_panel_scroll_metrics(app, area);
    let body = agent_panel_body_rect(area, true);
    (should_show_scrollbar(metrics) && body.width > 0 && body.height > 0).then_some(Rect::new(
        area.x + area.width.saturating_sub(1),
        body.y,
        1,
        body.height,
    ))
}

pub(crate) fn compute_workspace_list_areas(
    app: &AppState,
    area: Rect,
) -> (
    Vec<crate::app::state::WorkspaceCardArea>,
    Vec<crate::app::state::WorkspaceHeaderArea>,
    Vec<crate::app::state::WorktreeAgentToggleArea>,
) {
    let ws_area = workspace_list_rect(area, app.sidebar_section_split);
    if ws_area == Rect::default() {
        return (Vec::new(), Vec::new(), Vec::new());
    }

    let metrics = workspace_list_scroll_metrics(app, ws_area);
    let body = workspace_list_body_rect(ws_area, should_show_scrollbar(metrics));
    if body.width == 0 || body.height == 0 {
        return (Vec::new(), Vec::new(), Vec::new());
    }

    let scroll = app.workspace_scroll;
    let mut row_y = body.y;
    let body_bottom = body.y + body.height;
    let mut cards = Vec::new();
    let mut headers = Vec::new();
    let mut toggle_areas = Vec::new();

    let entries = workspace_list_entries(app);
    for (entry_idx, entry) in entries.iter().enumerate().skip(scroll) {
        match entry {
            WorkspaceListEntry::Workspace { ws_idx, indented } => {
                let Some(ws) = app.workspaces.get(*ws_idx) else {
                    continue;
                };
                let row_height = if *indented {
                    CHILD_ROW_HEIGHT
                } else {
                    workspace_row_height(ws)
                };
                let gap = u16::from(
                    !(*indented && next_entry_is_indented_workspace(&entries, entry_idx)),
                );
                let details_count = if *indented {
                    ws.pane_details(&app.terminals).len()
                } else {
                    0
                };
                let expanded = *indented && app.expanded_worktree_agents.contains(&ws.id);
                let sub_rows = if expanded {
                    details_count.saturating_sub(1) as u16
                } else {
                    0
                };
                let total_height = row_height.saturating_add(sub_rows);
                if row_y.saturating_add(total_height).saturating_add(gap) > body_bottom {
                    break;
                }
                cards.push(crate::app::state::WorkspaceCardArea {
                    ws_idx: *ws_idx,
                    rect: Rect::new(body.x, row_y, body.width, row_height),
                    indented: *indented,
                });
                // Emit a toggle area (for the caret at the right of the agent spans) when
                // the indented row has more than one agent pane.
                if *indented && details_count > 1 {
                    let caret_width = 6.min(body.width);
                    let caret_x = body.x + body.width.saturating_sub(caret_width);
                    toggle_areas.push(crate::app::state::WorktreeAgentToggleArea {
                        ws_idx: *ws_idx,
                        rect: Rect::new(caret_x, row_y, caret_width, 1),
                    });
                }
                row_y = row_y.saturating_add(total_height.saturating_add(gap));
            }
            WorkspaceListEntry::RepoHeader { key } => {
                let row_height = REPO_HEADER_ROW_HEIGHT;
                let gap = 1;
                if row_y.saturating_add(row_height).saturating_add(gap) > body_bottom {
                    break;
                }
                headers.push(crate::app::state::WorkspaceHeaderArea {
                    key: key.clone(),
                    rect: Rect::new(body.x, row_y, body.width, row_height),
                });
                row_y = row_y.saturating_add(row_height + gap);
            }
        }
    }

    (cards, headers, toggle_areas)
}

pub(crate) fn compute_workspace_card_areas(
    app: &AppState,
    area: Rect,
) -> Vec<crate::app::state::WorkspaceCardArea> {
    compute_workspace_list_areas(app, area).0
}

/// Auto-scale sidebar width based on workspace identity + agent summary.
pub(crate) fn collapsed_sidebar_sections(area: Rect) -> (Rect, Option<u16>, Rect) {
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.width == 0 || content.height == 0 {
        return (Rect::default(), None, Rect::default());
    }

    if content.height < 7 {
        return (content, None, Rect::default());
    }

    let total_h = content.height as usize;
    let ws_h = total_h.div_ceil(2);
    let detail_h = total_h.saturating_sub(ws_h + 1);
    if ws_h == 0 || detail_h == 0 {
        return (content, None, Rect::default());
    }

    let divider_y = content.y + ws_h as u16;
    let ws_area = Rect::new(content.x, content.y, content.width, ws_h as u16);
    let detail_area = Rect::new(content.x, divider_y + 1, content.width, detail_h as u16);
    (ws_area, Some(divider_y), detail_area)
}

/// Collapsed sidebar: workspace glance on top, compact agent list below.
pub(super) fn render_sidebar_collapsed(app: &AppState, frame: &mut Frame, area: Rect) {
    let is_navigating = matches!(app.mode, Mode::Navigate);

    let p = &app.palette;
    let sep_style = if is_navigating {
        Style::default().fg(p.accent)
    } else {
        Style::default().fg(p.surface_dim)
    };
    let sep_x = area.x + area.width.saturating_sub(1);
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        buf[(sep_x, y)].set_symbol("│");
        buf[(sep_x, y)].set_style(sep_style);
    }

    let (ws_area, divider_y, detail_area) = collapsed_sidebar_sections(area);
    if ws_area == Rect::default() {
        render_sidebar_toggle(app, frame, area, true, p);
        return;
    }

    for (visible_idx, ws) in app.workspaces.iter().enumerate() {
        let y = ws_area.y + visible_idx as u16;
        if y >= ws_area.y + ws_area.height {
            break;
        }
        let (agg_state, agg_seen) = ws.aggregate_state(&app.terminals);
        let (icon, icon_style) = state_dot(agg_state, agg_seen, p);
        let is_selected = visible_idx == app.selected && is_navigating;
        let is_active = Some(visible_idx) == app.active;
        let row_style = if is_selected {
            Style::default().bg(p.surface0)
        } else if is_active {
            Style::default().bg(p.surface_dim)
        } else {
            Style::default()
        };
        let num_style = if is_selected {
            Style::default().fg(p.overlay1).bg(p.surface0)
        } else if is_active {
            Style::default().fg(p.text).bg(p.surface_dim)
        } else {
            Style::default().fg(p.overlay0)
        };

        if is_selected || is_active {
            let buf = frame.buffer_mut();
            for x in ws_area.x..ws_area.x + ws_area.width {
                buf[(x, y)].set_style(row_style);
            }
        }

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("{}", visible_idx + 1), num_style),
                Span::styled(" ", row_style),
                Span::styled(icon, icon_style),
            ])),
            Rect::new(ws_area.x, y, ws_area.width, 1),
        );
    }

    if let Some(divider_y) = divider_y {
        let buf = frame.buffer_mut();
        for x in ws_area.x..ws_area.x + ws_area.width {
            buf[(x, divider_y)].set_symbol("─");
            buf[(x, divider_y)].set_style(Style::default().fg(p.surface_dim));
        }
    }

    let detail_ws_idx = if is_navigating {
        Some(app.selected)
    } else {
        app.active
    };
    let detail_content_area = Rect::new(
        detail_area.x,
        detail_area.y,
        detail_area.width,
        detail_area.height.saturating_sub(1),
    );
    if detail_content_area != Rect::default() {
        if let Some(ws_idx) = detail_ws_idx {
            if let Some(ws) = app.workspaces.get(ws_idx) {
                for (detail_idx, detail) in ws.pane_details(&app.terminals).iter().enumerate() {
                    let y = detail_content_area.y + detail_idx as u16;
                    if y >= detail_content_area.y + detail_content_area.height {
                        break;
                    }
                    let pane_num = ws
                        .public_pane_number(detail.pane_id)
                        .unwrap_or(detail_idx + 1);
                    let pane_style = Style::default().fg(p.overlay0);
                    let (icon, icon_style) =
                        agent_icon(detail.state, detail.seen, app.spinner_tick, p);
                    frame.render_widget(
                        Paragraph::new(Line::from(vec![
                            Span::styled(format!("{pane_num}"), pane_style),
                            Span::styled(" ", pane_style),
                            Span::styled(icon, icon_style),
                        ])),
                        Rect::new(detail_content_area.x, y, detail_content_area.width, 1),
                    );
                }
            }
        }
    }

    render_sidebar_toggle(app, frame, area, true, p);
}

pub(crate) fn workspace_drop_indicator_row(
    cards: &[crate::app::state::WorkspaceCardArea],
    area: Rect,
    insert_idx: usize,
) -> Option<u16> {
    if area.height == 0 {
        return None;
    }
    let list_bottom = area.y + area.height.saturating_sub(1);

    let first = cards.first()?;
    if insert_idx == first.ws_idx {
        return first.rect.y.checked_sub(1).filter(|y| *y < list_bottom);
    }

    if let Some(row) = cards
        .last()
        .filter(|card| insert_idx == card.ws_idx.saturating_add(1))
        .map(|card| card.rect.y.saturating_add(card.rect.height))
        .filter(|y| *y < list_bottom)
    {
        return Some(row);
    }

    if let Some(card) = cards.iter().find(|card| card.ws_idx == insert_idx) {
        return card.rect.y.checked_sub(1).filter(|y| *y < list_bottom);
    }

    None
}

pub(super) fn render_sidebar(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
) {
    let p = &app.palette;
    let is_navigating = matches!(app.mode, Mode::Navigate);
    let sep_style = if is_navigating {
        Style::default().fg(p.accent)
    } else {
        Style::default().fg(p.surface_dim)
    };

    let sep_x = area.x + area.width.saturating_sub(1);
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        buf[(sep_x, y)].set_symbol("│");
        buf[(sep_x, y)].set_style(sep_style);
    }

    let (ws_area, detail_area) = expanded_sidebar_sections(area, app.sidebar_section_split);

    render_workspace_list(app, terminal_runtimes, frame, ws_area, is_navigating);
    render_pr_inbox(app, frame, detail_area);
    render_sidebar_toggle(app, frame, area, false, p);
}

fn render_workspace_list(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
    is_navigating: bool,
) {
    let p = &app.palette;
    let dragged_ws_idx = match app.drag.as_ref().map(|drag| &drag.target) {
        Some(crate::app::state::DragTarget::WorkspaceReorder { source_ws_idx, .. }) => {
            Some(*source_ws_idx)
        }
        _ => None,
    };
    let insertion_row = match app.drag.as_ref().map(|drag| &drag.target) {
        Some(crate::app::state::DragTarget::WorkspaceReorder {
            insert_idx: Some(insert_idx),
            ..
        }) => workspace_drop_indicator_row(&app.view.workspace_card_areas, area, *insert_idx),
        _ => None,
    };

    let list_bottom = area.y + area.height.saturating_sub(1);
    if area.height > 0 {
        frame.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                " spaces",
                Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
            )])),
            Rect::new(area.x, area.y, area.width, 1),
        );
    }

    let metrics = workspace_list_scroll_metrics(app, area);
    let scrollbar_rect = workspace_list_scrollbar_rect(app, area);
    let cards = &app.view.workspace_card_areas;

    // Worktree children that are the last in their repo group (groups are
    // separated by repo headers in the entry list, so this must come from the
    // entries, not from card adjacency). Drives the `└` vs `├` tree connector.
    let entries = workspace_list_entries(app);
    let last_children: std::collections::HashSet<usize> = entries
        .iter()
        .enumerate()
        .filter_map(|(idx, entry)| match entry {
            WorkspaceListEntry::Workspace {
                ws_idx,
                indented: true,
            } if !next_entry_is_indented_workspace(&entries, idx) => Some(*ws_idx),
            _ => None,
        })
        .collect();

    for card in cards {
        let i = card.ws_idx;
        let ws = &app.workspaces[i];
        let row_y = card.rect.y;
        let row_height = card.rect.height;
        let selected = i == app.selected && is_navigating;
        let is_active = Some(i) == app.active;
        let is_dragged = dragged_ws_idx == Some(i);
        let highlighted = selected || is_active || is_dragged;
        let (agg_state, agg_seen) = ws.aggregate_state(&app.terminals);

        if highlighted {
            let bg = if selected {
                p.surface0
            } else if is_dragged {
                p.surface1
            } else {
                p.surface_dim
            };
            let buf = frame.buffer_mut();
            for y in row_y..row_y + row_height {
                if y >= list_bottom {
                    break;
                }
                for x in card.rect.x..card.rect.x + card.rect.width {
                    buf[(x, y)].set_style(Style::default().bg(bg));
                }
            }
        }

        let name_style = if selected || is_active || is_dragged {
            Style::default().fg(p.text).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0)
        };

        let (icon, icon_style) = state_dot(agg_state, agg_seen, p);
        let label = ws.display_name_from(&app.terminals, terminal_runtimes);
        let mut line1 = Vec::new();
        let mut show_workspace_icon = true;
        let is_last_child = card.indented && last_children.contains(&i);
        if card.indented {
            // Tree connector linking the worktree child to its repo header.
            let connector = if is_last_child { " └ " } else { " ├ " };
            line1.push(Span::styled(connector, Style::default().fg(p.overlay0)));
        } else if let Some((key, collapsed)) = workspace_parent_group_state(app, i) {
            let icon = if collapsed { "▸" } else { "▾" };
            let (state_icon, state_style) = if collapsed {
                let (state, seen) = space_aggregate_state(app, &key);
                state_dot(state, seen, p)
            } else {
                (icon, Style::default().fg(p.accent))
            };
            line1.push(Span::styled(icon, Style::default().fg(p.accent)));
            if collapsed {
                line1.push(Span::styled(" ", Style::default()));
                line1.push(Span::styled(state_icon, state_style));
                show_workspace_icon = false;
            }
            line1.push(Span::styled(" ", Style::default()));
        } else {
            line1.push(Span::styled(" ", Style::default()));
        }
        if show_workspace_icon {
            line1.push(Span::styled(icon, icon_style));
            line1.push(Span::styled(" ", Style::default()));
        }
        if card.indented {
            let display_label = grouped_child_display_label(
                &label,
                ws.branch().as_deref(),
                ws.custom_name.is_some(),
            );
            line1.push(Span::styled(display_label, name_style));
            let details = ws.pane_details(&app.terminals);
            if let Some(primary) = ws.primary_pane_detail(&app.terminals) {
                let (badge_state, badge_seen) = details
                    .iter()
                    .max_by_key(|d| workspace_attention_priority(d.state, d.seen))
                    .map(|d| (d.state, d.seen))
                    .unwrap_or((primary.state, primary.seen));
                let extra = details.len().saturating_sub(1);
                let expanded = app.expanded_worktree_agents.contains(&ws.id);
                line1.extend(worktree_agent_spans(
                    &primary,
                    badge_state,
                    badge_seen,
                    extra,
                    expanded,
                    p,
                ));
            }
        } else {
            line1.push(Span::styled(label, name_style));
        }

        frame.render_widget(
            Paragraph::new(Line::from(line1)),
            Rect::new(card.rect.x, row_y, card.rect.width, 1),
        );

        if row_height > 1 && row_y + 1 < list_bottom {
            if card.indented {
                // Worktree child: diff-stats sub-line (committed · wip).
                frame.render_widget(
                    Paragraph::new(Line::from(worktree_stats_spans(
                        ws.git_diff_stats(),
                        is_last_child,
                        p,
                    ))),
                    Rect::new(card.rect.x, row_y + 1, card.rect.width, 1),
                );
            } else if let Some(branch) = ws.branch() {
                let upstream_label = ws.git_ahead_behind().and_then(|(ahead, behind)| {
                    let mut parts = Vec::new();
                    if ahead > 0 {
                        parts.push((format!("↑{}", ahead), p.green));
                    }
                    if behind > 0 {
                        parts.push((format!("↓{}", behind), p.red));
                    }
                    (!parts.is_empty()).then_some(parts)
                });
                let reserved = upstream_label
                    .as_ref()
                    .map(|parts| {
                        parts.iter().map(|(label, _)| label.len()).sum::<usize>() + parts.len()
                    })
                    .unwrap_or(0);
                let max_branch_len = (card.rect.width as usize).saturating_sub(5 + reserved);
                let branch_display = truncate_text(&branch, max_branch_len);
                let branch_color = if selected || is_active {
                    p.mauve
                } else {
                    p.overlay0
                };
                let mut spans = vec![
                    Span::styled("   ", Style::default()),
                    Span::styled(branch_display, Style::default().fg(branch_color)),
                ];
                if let Some(parts) = upstream_label {
                    spans.push(Span::styled(" ", Style::default()));
                    for (idx, (label, color)) in parts.into_iter().enumerate() {
                        if idx > 0 {
                            spans.push(Span::styled(" ", Style::default()));
                        }
                        spans.push(Span::styled(label, Style::default().fg(color)));
                    }
                }
                frame.render_widget(
                    Paragraph::new(Line::from(spans)),
                    Rect::new(card.rect.x, row_y + 1, card.rect.width, 1),
                );
            }
        }

        // Render per-agent sub-rows when this indented worktree is expanded.
        if card.indented && app.expanded_worktree_agents.contains(&ws.id) {
            let details = ws.pane_details(&app.terminals);
            if details.len() > 1 {
                let sub_row_start = row_y + row_height;
                for (sub_idx, detail) in details.iter().skip(1).enumerate() {
                    let sub_y = sub_row_start + sub_idx as u16;
                    if sub_y >= list_bottom {
                        break;
                    }
                    let (dot, dot_style) = state_dot(detail.state, detail.seen, p);
                    let sub_label = detail
                        .state_labels
                        .get(agent_panel_status_key(detail.state, detail.seen))
                        .map(String::as_str)
                        .unwrap_or_else(|| state_label(detail.state, detail.seen));
                    let spans = vec![
                        Span::styled("      ", Style::default()),
                        Span::styled(dot, dot_style),
                        Span::styled(" ", Style::default()),
                        Span::styled(
                            sub_label.to_string(),
                            Style::default().fg(state_label_color(detail.state, detail.seen, p)),
                        ),
                        Span::styled(" ", Style::default()),
                        Span::styled(detail.agent_label.clone(), Style::default().fg(p.overlay1)),
                    ];
                    frame.render_widget(
                        Paragraph::new(Line::from(spans)),
                        Rect::new(card.rect.x, sub_y, card.rect.width, 1),
                    );
                }
            }
        }
    }

    for header in &app.view.workspace_header_areas {
        let row_y = header.rect.y;
        if row_y >= list_bottom {
            continue;
        }
        let collapsed = app.collapsed_space_keys.contains(&header.key);
        let caret = if collapsed { "▸" } else { "▾" };
        let (state, seen) = space_aggregate_state(app, &header.key);
        let (state_icon, state_style) = state_dot(state, seen, p);
        // The repo label comes from any linked worktree member's membership.
        let repo_label = app
            .workspaces
            .iter()
            .find_map(|ws| {
                ws.worktree_space()
                    .filter(|space| space.key == header.key)
                    .map(|space| space.label.clone())
            })
            .unwrap_or_else(|| "repo".into());
        let line = Line::from(vec![
            Span::styled(caret, Style::default().fg(p.accent)),
            Span::styled(" ", Style::default()),
            Span::styled(state_icon, state_style),
            Span::styled(" ", Style::default()),
            Span::styled(
                repo_label,
                Style::default().fg(p.subtext0).add_modifier(Modifier::BOLD),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(line),
            Rect::new(header.rect.x, row_y, header.rect.width, 1),
        );
    }

    if let Some(y) = insertion_row.filter(|y| *y < list_bottom) {
        let indicator_right = scrollbar_rect
            .map(|rect| rect.x)
            .unwrap_or(area.x + area.width);
        let buf = frame.buffer_mut();
        for x in area.x..indicator_right {
            buf[(x, y)].set_symbol("─");
            buf[(x, y)].set_style(Style::default().fg(p.accent));
        }
    }

    if let Some(track) = scrollbar_rect {
        render_scrollbar(frame, metrics, track, p.surface_dim, p.overlay0, "▕");
    }

    if app.mouse_capture && list_bottom > area.y {
        let new_rect = app.sidebar_new_button_rect();
        frame.render_widget(
            Paragraph::new(Span::styled(" new", Style::default().fg(p.overlay0))),
            new_rect,
        );

        let menu_rect = app.global_launcher_rect();
        let menu_line = if app.global_menu_attention_badge_visible() {
            Line::from(vec![
                Span::styled(
                    "● ",
                    Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled("menu", Style::default().fg(p.overlay0)),
            ])
        } else {
            Line::from(vec![Span::styled("menu", Style::default().fg(p.overlay0))])
        };
        frame.render_widget(
            Paragraph::new(menu_line).alignment(Alignment::Right),
            menu_rect,
        );
    }
}

const PR_INBOX_HEADER_ROWS: u16 = 3;

fn pr_inbox_body_rect(area: Rect, has_scrollbar: bool) -> Rect {
    if area.width == 0 || area.height <= PR_INBOX_HEADER_ROWS {
        return Rect::default();
    }
    let body_y = area.y.saturating_add(PR_INBOX_HEADER_ROWS);
    let body_height = (area.y + area.height).saturating_sub(body_y);
    let body_width = area.width.saturating_sub(u16::from(has_scrollbar));
    Rect::new(area.x, body_y, body_width, body_height)
}

/// Rows consumed per PR entry (name line + repo line + gap).
const PR_ROW_HEIGHT: u16 = 3;

fn pr_inbox_visible_count(area: Rect) -> usize {
    let body = pr_inbox_body_rect(area, false);
    if body.height == 0 {
        return 0;
    }
    (body.height / PR_ROW_HEIGHT) as usize
}

pub(crate) fn pr_inbox_scroll_metrics(app: &AppState, area: Rect) -> crate::pane::ScrollMetrics {
    let viewport_rows = pr_inbox_visible_count(area);
    let total_rows = app.pr_inbox.prs.len();
    let max_offset_from_bottom = total_rows.saturating_sub(viewport_rows);
    let offset_from_bottom = total_rows
        .saturating_sub(app.pr_inbox_scroll)
        .saturating_sub(viewport_rows);
    crate::pane::ScrollMetrics {
        offset_from_bottom,
        max_offset_from_bottom,
        viewport_rows,
    }
}

pub(crate) fn pr_inbox_scrollbar_rect(app: &AppState, area: Rect) -> Option<Rect> {
    let metrics = pr_inbox_scroll_metrics(app, area);
    let body = pr_inbox_body_rect(area, true);
    (should_show_scrollbar(metrics) && body.width > 0 && body.height > 0).then_some(Rect::new(
        area.x + area.width.saturating_sub(1),
        body.y,
        1,
        body.height,
    ))
}

fn render_pr_inbox(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;

    if area.height < 3 {
        return;
    }

    // Separator line.
    let sep_line = "─".repeat(area.width as usize);
    frame.render_widget(
        Paragraph::new(Span::styled(&sep_line, Style::default().fg(p.surface_dim))),
        Rect::new(area.x, area.y, area.width, 1),
    );

    // Section header.
    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            " prs",
            Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
        )])),
        Rect::new(area.x, area.y + 1, area.width, 1),
    );

    let body = pr_inbox_body_rect(
        area,
        should_show_scrollbar(pr_inbox_scroll_metrics(app, area)),
    );
    if body == Rect::default() {
        return;
    }

    use crate::pr_inbox::PullRequestInboxStatus;
    match &app.pr_inbox.status {
        PullRequestInboxStatus::Loading => {
            frame.render_widget(
                Paragraph::new(Span::styled(" loading…", Style::default().fg(p.overlay0))),
                Rect::new(body.x, body.y, body.width, 1),
            );
            return;
        }
        PullRequestInboxStatus::GhNotInstalled => {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    " gh not installed",
                    Style::default().fg(p.overlay0),
                )),
                Rect::new(body.x, body.y, body.width, 1),
            );
            return;
        }
        PullRequestInboxStatus::GhNotAuthed => {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    " gh auth login",
                    Style::default().fg(p.overlay0),
                )),
                Rect::new(body.x, body.y, body.width, 1),
            );
            return;
        }
        PullRequestInboxStatus::Error { .. } => {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    " error fetching prs",
                    Style::default().fg(p.red),
                )),
                Rect::new(body.x, body.y, body.width, 1),
            );
            return;
        }
        PullRequestInboxStatus::Ok => {}
    }

    if app.pr_inbox.prs.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                " no open prs",
                Style::default().fg(p.overlay0),
            )),
            Rect::new(body.x, body.y, body.width, 1),
        );
        return;
    }

    let metrics = pr_inbox_scroll_metrics(app, area);
    let scrollbar_rect = pr_inbox_scrollbar_rect(app, area);

    let mut row_y = body.y;
    let body_bottom = body.y + body.height;
    for pr in app.pr_inbox.prs.iter().skip(app.pr_inbox_scroll) {
        if row_y.saturating_add(2) > body_bottom {
            break;
        }

        // Title line: [draft] prefix + title.
        let indent = " ";
        let draft_prefix = if pr.is_draft { "[draft] " } else { "" };
        let draft_reserved = draft_prefix.chars().count() + indent.chars().count();
        let max_title = (body.width as usize).saturating_sub(draft_reserved);
        let title_display = truncate_text(&pr.title, max_title);

        let mut title_spans = vec![Span::styled(indent, Style::default())];
        if pr.is_draft {
            title_spans.push(Span::styled("[draft] ", Style::default().fg(p.overlay0)));
        }
        title_spans.push(Span::styled(
            title_display,
            Style::default().fg(p.subtext0).add_modifier(Modifier::BOLD),
        ));
        frame.render_widget(
            Paragraph::new(Line::from(title_spans)),
            Rect::new(body.x, row_y, body.width, 1),
        );
        row_y += 1;

        // Repo + PR number line.
        if row_y < body_bottom {
            let repo_label = format!("   #{} {}", pr.number, pr.repo);
            let repo_display = truncate_text(&repo_label, body.width as usize);
            frame.render_widget(
                Paragraph::new(Span::styled(repo_display, Style::default().fg(p.overlay0))),
                Rect::new(body.x, row_y, body.width, 1),
            );
            row_y += 1;
        }

        // Gap row between entries.
        if row_y < body_bottom {
            row_y += 1;
        }
    }

    if let Some(track) = scrollbar_rect {
        render_scrollbar(frame, metrics, track, p.surface_dim, p.overlay0, "▕");
    }
}

pub(crate) fn collapsed_sidebar_toggle_rect(area: Rect) -> Rect {
    let bottom_y = area.y + area.height.saturating_sub(1);
    let content_w = area.width.saturating_sub(1);
    if content_w == 0 || area.height == 0 {
        return Rect::default();
    }
    let x = area.x + content_w / 2;
    Rect::new(x, bottom_y, 1, 1)
}

pub(crate) fn expanded_sidebar_toggle_rect(area: Rect) -> Rect {
    if area.width <= 1 || area.height == 0 {
        return Rect::default();
    }
    Rect::new(
        area.x + area.width.saturating_sub(2),
        area.y + area.height.saturating_sub(1),
        1,
        1,
    )
}

fn render_sidebar_toggle(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    collapsed: bool,
    p: &Palette,
) {
    let toggle_area = if collapsed {
        collapsed_sidebar_toggle_rect(area)
    } else {
        expanded_sidebar_toggle_rect(area)
    };
    if toggle_area == Rect::default() {
        return;
    }
    let icon = if collapsed { "»" } else { "«" };
    let icon_style = if collapsed && app.global_menu_attention_badge_visible() {
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.overlay0)
    };
    frame.render_widget(Paragraph::new(Span::styled(icon, icon_style)), toggle_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{detect::Agent, workspace::Workspace};
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn worktree_stats_rail_reflects_last_child() {
        let app = crate::app::state::AppState::test_new();
        let p = &app.palette;
        // The tree rail sits at column 1; total leading width stays 7 columns.
        for is_last in [false, true] {
            let spans = worktree_stats_spans(None, is_last, p);
            let lead: usize = spans[..3].iter().map(|s| s.content.chars().count()).sum();
            assert_eq!(lead, 7);
        }
        // Non-last child continues the vertical connector; the last child blanks it.
        assert_eq!(
            worktree_stats_spans(None, false, p)[1].content.as_ref(),
            "│"
        );
        assert_eq!(worktree_stats_spans(None, true, p)[1].content.as_ref(), " ");
    }

    #[test]
    fn render_sidebar_toggle_draws_expanded_collapse_icon() {
        let app = crate::app::state::AppState::test_new();
        let area = Rect::new(0, 0, 26, 20);
        let mut terminal =
            Terminal::new(TestBackend::new(26, 20)).expect("test terminal should initialize");

        terminal
            .draw(|frame| render_sidebar_toggle(&app, frame, area, false, &app.palette))
            .expect("sidebar toggle should render");

        let toggle = expanded_sidebar_toggle_rect(area);
        assert_eq!(
            terminal.backend().buffer()[(toggle.x, toggle.y)].symbol(),
            "«"
        );
    }

    #[test]
    fn expanded_sidebar_toggle_sits_inside_sidebar_content() {
        let area = Rect::new(0, 0, 26, 20);
        let toggle = expanded_sidebar_toggle_rect(area);

        assert_eq!(toggle.x, area.x + area.width - 2);
        assert_eq!(toggle.y, area.y + area.height - 1);
    }

    #[test]
    fn all_workspaces_agent_panel_entries_use_workspace_and_optional_tab_labels() {
        let mut app = crate::app::state::AppState::test_new();
        let first = Workspace::test_new("one");
        let first_pane = first.tabs[0].root_pane;
        let mut second = Workspace::test_new("two");
        let second_tab = second.test_add_tab(Some("logs"));
        let second_pane = second.tabs[second_tab].root_pane;

        app.workspaces = vec![first, second];
        app.ensure_test_terminals();
        let first_terminal_id = app.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        let second_terminal_id = app.workspaces[1].tabs[second_tab].panes[&second_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&second_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Claude);
        app.active = Some(0);
        app.selected = 0;

        let entries = agent_panel_entries(&app);
        assert_eq!(entries[0].primary_label, "one");
        assert!(entries[0].primary_tab_label.is_none());
        assert_eq!(entries[0].agent_label.as_deref(), Some("pi"));
        assert_eq!(entries[1].primary_label, "two");
        assert_eq!(entries[1].primary_tab_label.as_deref(), Some("logs"));
        assert_eq!(entries[1].agent_label.as_deref(), Some("claude"));
    }

    fn linked_worktree_membership() -> crate::workspace::WorktreeSpaceMembership {
        crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: std::path::PathBuf::from("/repo"),
            checkout_path: std::path::PathBuf::from("/repo/wt"),
            is_linked_worktree: true,
        }
    }

    #[test]
    fn agent_panel_entry_leads_with_branch_for_linked_worktree() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("herdr-1");
        ws.cached_git_branch = Some("issue/82-fix".into());
        ws.worktree_space = Some(linked_worktree_membership());
        let pane = ws.tabs[0].root_pane;
        app.workspaces = vec![ws];
        app.ensure_test_terminals();
        let term_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        app.terminals.get_mut(&term_id).unwrap().detected_agent = Some(Agent::Claude);
        app.active = Some(0);
        app.selected = 0;

        // A worktree agent leads with its branch (its self-identifying key),
        // not the workspace display name.
        let entries = agent_panel_entries(&app);
        assert_eq!(entries[0].primary_label, "issue/82-fix");
    }

    #[test]
    fn agent_panel_entry_worktree_without_branch_falls_back_to_display_name() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("plain-agent");
        ws.cached_git_branch = None;
        ws.worktree_space = Some(linked_worktree_membership());
        let pane = ws.tabs[0].root_pane;
        app.workspaces = vec![ws];
        app.ensure_test_terminals();
        let term_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        app.terminals.get_mut(&term_id).unwrap().detected_agent = Some(Agent::Claude);
        app.active = Some(0);
        app.selected = 0;

        // A worktree with no resolvable branch falls back to the display name.
        let entries = agent_panel_entries(&app);
        assert_eq!(entries[0].primary_label, "plain-agent");
    }

    #[test]
    fn priority_agent_panel_sort_uses_attention_then_space_order() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![
            Workspace::test_new("one"),
            Workspace::test_new("two"),
            Workspace::test_new("three"),
            Workspace::test_new("four"),
        ];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.selected = 0;
        app.agent_panel_sort = crate::app::state::AgentPanelSort::Priority;

        let set_state = |app: &mut crate::app::state::AppState, ws_idx: usize, state| {
            let pane = app.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            let terminal = app.terminals.get_mut(&terminal_id).unwrap();
            terminal.detected_agent = Some(Agent::Claude);
            terminal.state = state;
        };
        set_state(&mut app, 0, AgentState::Working);
        set_state(&mut app, 1, AgentState::Idle);
        set_state(&mut app, 2, AgentState::Working);
        set_state(&mut app, 3, AgentState::Blocked);

        let done_pane = app.workspaces[1].tabs[0].root_pane;
        app.workspaces[1].tabs[0]
            .panes
            .get_mut(&done_pane)
            .unwrap()
            .seen = false;

        let labels: Vec<String> = agent_panel_entries(&app)
            .into_iter()
            .map(|entry| entry.primary_label)
            .collect();

        assert_eq!(labels, ["four", "two", "one", "three"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn all_workspaces_agent_panel_entries_use_live_root_runtime_cwd_for_workspace_label() {
        let unique = format!(
            "herdr-agent-panel-runtime-cwd-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let stale_cwd = root.join("issue-264-nix-support");
        let live_cwd = root.join("herdr");
        std::fs::create_dir_all(stale_cwd.join(".git")).unwrap();
        std::fs::create_dir_all(live_cwd.join(".git")).unwrap();

        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = Workspace::test_new("stale-name");
        workspace.custom_name = None;
        workspace.identity_cwd = stale_cwd.clone();
        let pane = workspace.tabs[0].root_pane;

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.cwd = stale_cwd;
        terminal.detected_agent = Some(Agent::Pi);
        app.active = Some(0);
        app.selected = 0;

        let (events, _) = tokio::sync::mpsc::channel(4);
        let runtime = crate::terminal::TerminalRuntime::spawn(
            pane,
            24,
            80,
            live_cwd.clone(),
            0,
            crate::terminal_theme::TerminalTheme::default(),
            crate::pane::PaneShellConfig::new("/bin/sh", crate::config::ShellModeConfig::NonLogin),
            &crate::pane::PaneLaunchEnv::default(),
            events,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while runtime.cwd() != Some(live_cwd.clone()) && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let mut runtime_registry = TerminalRuntimeRegistry::new();
        runtime_registry.insert(terminal_id, runtime);
        let entries = agent_panel_entries_from(&app, &runtime_registry);
        let primary_label = entries[0].primary_label.clone();

        for (_, runtime) in runtime_registry.drain() {
            runtime.shutdown();
        }
        let _ = std::fs::remove_dir_all(root);

        assert_eq!(primary_label, "herdr");
    }

    #[test]
    fn all_workspaces_agent_panel_entries_prefer_agent_names_for_agent_identity() {
        let mut app = crate::app::state::AppState::test_new();
        let workspace = Workspace::test_new("bridge");
        let first_pane = workspace.tabs[0].root_pane;

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let first_terminal_id = app.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        app.terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .set_agent_name("planner".into());
        app.active = Some(0);
        app.selected = 0;

        let entries = agent_panel_entries(&app);
        assert_eq!(entries[0].primary_label, "bridge");
        assert_eq!(entries[0].agent_label.as_deref(), Some("planner"));
    }

    #[test]
    fn expanded_sidebar_sections_handle_tiny_heights() {
        let (ws_area, detail_area) = expanded_sidebar_sections(Rect::new(0, 0, 20, 5), 0.9);

        assert_eq!(ws_area, Rect::new(0, 0, 19, 3));
        assert_eq!(detail_area, Rect::new(0, 3, 19, 2));
    }

    #[test]
    fn sidebar_section_divider_is_hidden_for_tiny_heights() {
        let divider = sidebar_section_divider_rect(Rect::new(0, 0, 20, 5), 0.5);

        assert_eq!(divider, Rect::default());
    }

    #[test]
    fn grouped_child_label_keeps_custom_workspace_name() {
        assert_eq!(
            grouped_child_display_label("renamed issue", Some("worktree/issue-137"), true),
            "renamed issue"
        );
    }

    #[test]
    fn grouped_child_label_uses_short_branch_for_auto_named_workspace() {
        assert_eq!(
            grouped_child_display_label("herdr-issue", Some("worktree/issue-137"), false),
            "issue-137"
        );
    }

    #[test]
    fn workspace_list_truncates_cjk_branch_without_panic() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("repo");
        ws.cached_git_branch = Some("feature/中文-分支-644".into());
        app.workspaces = vec![ws];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        app.view.workspace_card_areas = vec![crate::app::state::WorkspaceCardArea {
            ws_idx: 0,
            rect: Rect::new(0, 1, 15, 2),
            indented: false,
        }];

        let mut terminal = Terminal::new(TestBackend::new(15, 6)).expect("test terminal");
        let runtimes = crate::terminal::TerminalRuntimeRegistry::new();

        terminal
            .draw(|frame| {
                render_workspace_list(&app, &runtimes, frame, Rect::new(0, 0, 15, 6), false)
            })
            .expect("workspace list should render");
    }

    fn workspace_with_worktree_space(
        name: &str,
        key: Option<&str>,
        checkout_key: &str,
    ) -> crate::workspace::Workspace {
        let mut ws = crate::workspace::Workspace::test_new(name);
        if let Some(key) = key {
            ws.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
                key: key.into(),
                label: "herdr".into(),
                repo_root: std::path::PathBuf::from("/repo/herdr"),
                checkout_path: std::path::PathBuf::from(checkout_key),
                is_linked_worktree: name != "main",
            });
        }
        ws
    }

    fn workspace_with_git_space(name: &str, key: &str) -> crate::workspace::Workspace {
        let mut ws = crate::workspace::Workspace::test_new(name);
        ws.cached_git_space = Some(crate::workspace::GitSpaceMetadata {
            key: key.into(),
            checkout_key: format!("/repo/{name}"),
            label: "herdr".into(),
            repo_root: std::path::PathBuf::from(format!("/repo/{name}")),
            is_linked_worktree: false,
        });
        ws
    }

    #[test]
    fn parent_workspace_row_stays_clickable_when_grouped() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];

        let (cards, headers, _toggle_areas) = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 20));

        assert!(headers.is_empty());
        assert_eq!(cards[0].ws_idx, 0);
        assert!(!cards[0].indented);
        assert_eq!(cards[1].ws_idx, 1);
        assert!(cards[1].indented);
        assert_eq!(cards[1].rect.y, cards[0].rect.y + cards[0].rect.height + 1);
    }

    #[test]
    fn linked_only_worktree_members_nest_under_synthesized_repo_header() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            workspace_with_worktree_space("review", Some("repo-key"), "/repo/herdr-review"),
        ];

        // No primary checkout is open, so the worktrees nest under a synthesized
        // repo header instead of rendering flat.
        let entries = workspace_list_entries(&app);

        assert_eq!(
            entries,
            vec![
                WorkspaceListEntry::RepoHeader {
                    key: "repo-key".into()
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: true
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true
                },
            ]
        );
    }

    #[test]
    fn parentless_worktree_group_emits_synthesized_header_area_above_children() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            workspace_with_worktree_space("review", Some("repo-key"), "/repo/herdr-review"),
        ];

        let (cards, headers, _toggle_areas) = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 20));

        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].key, "repo-key");
        assert_eq!(cards.len(), 2);
        assert!(cards.iter().all(|card| card.indented));
        assert!(headers[0].rect.y < cards[0].rect.y);
    }

    #[test]
    fn worktree_nests_under_primary_checkout_matched_by_git_space() {
        // The real-world case: the primary checkout is open as a workspace but
        // carries only a cached git space (no worktree membership). Its repo
        // root matches the worktree's, so the worktree nests under it rather
        // than under a synthesized header.
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_git_space("herdr", "repo-key"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true
                },
            ]
        );
    }

    #[test]
    fn compact_space_group_scroll_offset_can_start_inside_group() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("one", Some("repo-key"), "/repo/herdr-one"),
            workspace_with_worktree_space("two", Some("repo-key"), "/repo/herdr-two"),
        ];
        let area = Rect::new(0, 0, 30, 20);
        app.workspace_scroll = normalized_workspace_scroll(&app, area, 2);

        let (cards, headers, _toggle_areas) = compute_workspace_list_areas(&app, area);

        assert!(headers.is_empty());
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].ws_idx, 2);
    }

    #[test]
    fn workspace_scroll_metrics_count_display_entries_not_raw_workspaces() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            Workspace::test_new("notes"),
        ];
        app.collapsed_space_keys.insert("repo-key".into());
        app.active = None;
        app.mode = Mode::Terminal;

        let ws_area = Rect::new(0, 0, 30, 6);
        let metrics = workspace_list_scroll_metrics(&app, ws_area);

        assert_eq!(metrics.viewport_rows, 1);
        assert_eq!(metrics.max_offset_from_bottom, 1);
        assert_eq!(metrics.offset_from_bottom, 1);
    }

    #[test]
    fn workspace_scroll_offset_applies_to_group_children() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            Workspace::test_new("notes"),
        ];
        app.collapsed_space_keys.insert("repo-key".into());
        app.active = None;
        app.mode = Mode::Terminal;
        app.workspace_scroll = 1;

        let (cards, headers, _toggle_areas) = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 12));

        assert!(headers.is_empty());
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].ws_idx, 2);
    }

    #[test]
    fn workspace_list_entries_group_multiple_workspaces_in_same_git_space() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true,
                },
            ]
        );
    }

    #[test]
    fn workspace_list_entries_group_non_contiguous_explicit_members() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_git_space("normal", "other-key"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 2,
                    indented: true,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn workspace_list_entries_do_not_group_normal_git_workspaces() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_git_space("one", "repo-key"),
            workspace_with_git_space("two", "repo-key"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn workspace_list_entries_do_not_auto_attach_normal_git_workspace_to_group() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_git_space("scratch", "repo-key"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 2,
                    indented: true,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn workspace_list_entries_leave_single_git_and_non_git_workspaces_flat() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_git_space("one", "repo-key"),
            workspace_with_worktree_space("notes", None, "/notes"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn collapsed_group_hides_inactive_children_but_keeps_active_visible() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];
        app.active = Some(1);
        app.mode = Mode::Terminal;
        app.collapsed_space_keys.insert("repo-key".into());

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true,
                },
            ]
        );

        app.active = None;
        app.mode = Mode::Terminal;
        assert_eq!(
            workspace_list_entries(&app),
            vec![WorkspaceListEntry::Workspace {
                ws_idx: 0,
                indented: false,
            }]
        );
    }

    #[test]
    fn collapsed_group_keeps_selected_child_visible_in_navigate_mode() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];
        app.mode = Mode::Navigate;
        app.selected = 1;
        app.active = Some(1);
        app.collapsed_space_keys.insert("repo-key".into());

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true,
                },
            ]
        );
    }

    #[test]
    fn worktree_agent_spans_includes_label_agent_and_count() {
        let p = Palette::catppuccin();
        let detail = crate::workspace::PaneDetail {
            pane_id: crate::layout::PaneId::alloc(),
            tab_idx: 0,
            tab_label: "1".into(),
            label: "claude".into(),
            agent_label: "claude".into(),
            agent: None,
            state: crate::detect::AgentState::Working,
            seen: true,
            last_agent_state_change_seq: None,
            custom_status: None,
            state_labels: std::collections::HashMap::new(),
        };
        let spans = worktree_agent_spans(
            &detail,
            crate::detect::AgentState::Blocked,
            true,
            2,
            false,
            &p,
        );
        let content: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            content.contains("working"),
            "expected 'working' in: {content}"
        );
        assert!(
            content.contains("claude"),
            "expected 'claude' in: {content}"
        );
        assert!(content.contains("+2"), "expected '+2' in: {content}");
    }
}
