use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::state::AppState;

use super::widgets::{panel_contrast_fg, render_panel_shell};

pub(super) fn render_repo_chooser_overlay(app: &AppState, frame: &mut Frame) {
    let popup = app.repo_chooser_popup_rect();
    if render_panel_shell(frame, popup, app.palette.accent, app.palette.panel_bg).is_none() {
        return;
    }
    render_search(app, frame, app.repo_chooser_search_rect());
    render_rows(app, frame, app.repo_chooser_body_rect());
    render_footer(app, frame, app.repo_chooser_footer_rect());
}

fn render_search(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.height == 0 {
        return;
    }
    let p = &app.palette;
    let query = app.repo_chooser.query.trim();
    let mut spans = vec![Span::styled(
        " / ",
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
    )];
    if query.is_empty() {
        spans.push(Span::styled(
            "search repos",
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
    let indices = app.repo_chooser_filtered_indices();

    if indices.is_empty() {
        let message = if app.repo_chooser.repos.is_empty() {
            crate::workspace::default_scan_root()
                .map(|root| format!("no repos under {}", root.display()))
                .unwrap_or_else(|| "no repos found".to_string())
        } else {
            "no matching repos".to_string()
        };
        frame.render_widget(
            Paragraph::new(format!(" {message}")).style(Style::default().fg(p.overlay0)),
            Rect::new(body.x, body.y, body.width, 1),
        );
        return;
    }

    let start = app.repo_chooser.scroll.min(indices.len());
    let end = indices
        .len()
        .min(start.saturating_add(body.height as usize));
    for (visible_idx, &repo_idx) in indices[start..end].iter().enumerate() {
        let row_idx = start + visible_idx;
        let Some(repo) = app.repo_chooser.repos.get(repo_idx) else {
            continue;
        };
        let rect = Rect::new(body.x, body.y + visible_idx as u16, body.width, 1);
        let selected = row_idx == app.repo_chooser.selected;
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
        let label = truncate(&repo.label, body.width.saturating_sub(2) as usize);
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
    let shown = app.repo_chooser_filtered_indices().len();
    let total = app.repo_chooser.repos.len();
    let line = Line::from(vec![
        Span::styled(format!(" {shown}/{total} repos  "), dim),
        Span::styled("enter", key),
        Span::styled(" open  ", dim),
        Span::styled("↑↓", key),
        Span::styled(" move  ", dim),
        Span::styled("esc", key),
        Span::styled(" close", dim),
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
