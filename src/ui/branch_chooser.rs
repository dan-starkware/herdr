use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::state::{AppState, BranchChooserState};

use super::widgets::{panel_contrast_fg, render_panel_shell};

/// The action a branch-chooser selection resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BranchChoice {
    /// Check out an existing local branch in the new worktree.
    Existing(String),
    /// Create a new branch `name` off `base` in the new worktree.
    New { name: String, base: String },
}

/// Indices into `state.branches` matching the current query (case-insensitive
/// substring). An empty query matches every branch.
pub(crate) fn filtered_branch_indices(state: &BranchChooserState) -> Vec<usize> {
    let query = state.query.trim().to_ascii_lowercase();
    state
        .branches
        .iter()
        .enumerate()
        .filter(|(_, branch)| query.is_empty() || branch.to_ascii_lowercase().contains(&query))
        .map(|(idx, _)| idx)
        .collect()
}

/// Resolve the current branch-chooser state into a [`BranchChoice`]:
/// - empty query → check out the highlighted existing branch;
/// - query exactly matches an existing branch → check it out;
/// - otherwise → create a new branch named by the query, off `default_base`.
pub(crate) fn resolve_branch_choice(state: &BranchChooserState) -> Option<BranchChoice> {
    let query = state.query.trim();
    if query.is_empty() {
        let idx = *filtered_branch_indices(state).get(state.selected)?;
        return Some(BranchChoice::Existing(state.branches.get(idx)?.clone()));
    }
    if state.branches.iter().any(|branch| branch == query) {
        return Some(BranchChoice::Existing(query.to_string()));
    }
    Some(BranchChoice::New {
        name: query.to_string(),
        base: state.default_base.clone(),
    })
}

pub(super) fn render_branch_chooser_overlay(app: &AppState, frame: &mut Frame) {
    let popup = app.branch_chooser_popup_rect();
    if render_panel_shell(frame, popup, app.palette.accent, app.palette.panel_bg).is_none() {
        return;
    }
    render_search(app, frame, app.branch_chooser_search_rect());
    render_rows(app, frame, app.branch_chooser_body_rect());
    render_footer(app, frame, app.branch_chooser_footer_rect());
}

fn render_search(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.height == 0 {
        return;
    }
    let p = &app.palette;
    let query = app.branch_chooser.query.trim();
    let mut spans = vec![Span::styled(
        " / ",
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
    )];
    if query.is_empty() {
        spans.push(Span::styled(
            "filter or type a new branch",
            Style::default().fg(p.overlay0),
        ));
    } else {
        spans.push(Span::styled(query.to_string(), Style::default().fg(p.text)));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_rows(app: &AppState, frame: &mut Frame, body: Rect) {
    if body.height == 0 || body.width == 0 {
        return;
    }
    let p = &app.palette;
    let indices = filtered_branch_indices(&app.branch_chooser);

    if indices.is_empty() {
        let message = if app.branch_chooser.branches.is_empty() {
            "no branches — type a name to create one".to_string()
        } else {
            "no match — enter to create this branch".to_string()
        };
        frame.render_widget(
            Paragraph::new(format!(" {message}")).style(Style::default().fg(p.overlay0)),
            Rect::new(body.x, body.y, body.width, 1),
        );
        return;
    }

    let start = app.branch_chooser.scroll.min(indices.len());
    let end = indices
        .len()
        .min(start.saturating_add(body.height as usize));
    for (visible_idx, &branch_idx) in indices[start..end].iter().enumerate() {
        let row_idx = start + visible_idx;
        let Some(branch) = app.branch_chooser.branches.get(branch_idx) else {
            continue;
        };
        let rect = Rect::new(body.x, body.y + visible_idx as u16, body.width, 1);
        let selected = row_idx == app.branch_chooser.selected;
        let base = if selected {
            Style::default().bg(p.accent).fg(panel_contrast_fg(p))
        } else {
            Style::default().bg(p.panel_bg).fg(p.text)
        };
        let label_style = if selected {
            base.add_modifier(Modifier::BOLD)
        } else {
            base
        };
        let marker = if selected { "▸ " } else { "  " };
        let label = truncate(branch, body.width.saturating_sub(2) as usize);
        let line = Line::from(vec![
            Span::styled(marker, base),
            Span::styled(label, label_style),
        ]);
        frame.render_widget(Paragraph::new(line).style(base), rect);
    }
}

fn render_footer(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.height == 0 {
        return;
    }
    let p = &app.palette;
    let key = Style::default().fg(p.accent).add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(p.overlay0);
    let action = match resolve_branch_choice(&app.branch_chooser) {
        Some(BranchChoice::Existing(name)) => format!(" check out {name}  "),
        Some(BranchChoice::New { name, base }) => format!(" create {name} off {base}  "),
        None => " pick a branch  ".to_string(),
    };
    let line = Line::from(vec![
        Span::styled("enter", key),
        Span::styled(action, dim),
        Span::styled("↑↓", key),
        Span::styled(" move  ", dim),
        Span::styled("esc", key),
        Span::styled(" back", dim),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

/// Truncate `text` to `max` columns, appending an ellipsis when it overflows.
fn truncate(text: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if text.chars().count() <= max {
        return text.to_string();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = text.chars().take(keep).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(branches: &[&str], query: &str) -> BranchChooserState {
        BranchChooserState {
            branches: branches.iter().map(|b| b.to_string()).collect(),
            default_base: "main".into(),
            query: query.into(),
            selected: 0,
            scroll: 0,
        }
    }

    #[test]
    fn filter_is_case_insensitive_substring() {
        let s = state(&["main", "feature/Login", "fix-bug"], "log");
        assert_eq!(filtered_branch_indices(&s), vec![1]);
    }

    #[test]
    fn exact_match_resolves_to_existing() {
        let s = state(&["main", "dev"], "dev");
        assert_eq!(
            resolve_branch_choice(&s),
            Some(BranchChoice::Existing("dev".into()))
        );
    }

    #[test]
    fn new_name_resolves_to_new_off_default_base() {
        let s = state(&["main"], "feature/new-thing");
        assert_eq!(
            resolve_branch_choice(&s),
            Some(BranchChoice::New {
                name: "feature/new-thing".into(),
                base: "main".into()
            })
        );
    }

    #[test]
    fn empty_query_uses_highlighted_existing() {
        let mut s = state(&["main", "dev"], "");
        s.selected = 1;
        assert_eq!(
            resolve_branch_choice(&s),
            Some(BranchChoice::Existing("dev".into()))
        );
    }
}
