use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
    Frame,
};

use super::widgets::panel_contrast_fg;
use crate::{
    app::state::{Palette, ToastKind},
    app::AppState,
    detect::AgentState,
};

/// One-line notification status bar at the bottom of the screen. It shows, in
/// priority order: a review-base fetch in flight, clipboard copy feedback, the
/// active toast, or (dimmed) the last toast that already expired.
pub(super) fn render_status_line(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let p = &app.palette;
    let base = Style::default().bg(p.panel_bg);

    let line = if let Some(fetch) = &app.control.review_base_fetch {
        // A review-base fetch is in flight: a loading message takes the line
        // (suppressing any toast underneath) and disappears exactly when the
        // fetch lands and the review row opens.
        Line::from(vec![
            Span::styled(
                format!(" {} ", super::spinner_frame(app.spinner_tick)),
                Style::default().fg(p.yellow),
            ),
            Span::styled(
                format!("syncing PR refs (origin/{}…)", fetch.base_branch),
                Style::default().fg(p.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" · review for PR #{} opens when done", fetch.pr_number),
                Style::default().fg(p.overlay0),
            ),
        ])
    } else if let Some(feedback) = &app.copy_feedback {
        Line::from(vec![
            Span::styled(" ● ", Style::default().fg(p.green)),
            Span::styled(
                feedback.message.as_str(),
                Style::default().fg(p.text).add_modifier(Modifier::BOLD),
            ),
        ])
    } else if let Some(toast) = &app.toast {
        let dot_color = match toast.kind {
            ToastKind::NeedsAttention => p.red,
            ToastKind::Finished => p.blue,
            ToastKind::UpdateInstalled => p.accent,
        };
        let mut spans = vec![
            Span::styled(" ● ", Style::default().fg(dot_color)),
            Span::styled(
                toast.title.as_str(),
                Style::default().fg(p.text).add_modifier(Modifier::BOLD),
            ),
        ];
        if !toast.context.is_empty() {
            spans.push(Span::styled(
                format!(" · {}", toast.context),
                Style::default().fg(p.overlay0),
            ));
        }
        Line::from(spans)
    } else if let Some(toast) = &app.last_toast {
        // Expired: keep the last notified message around, fully dimmed.
        let dim = Style::default().fg(p.overlay0);
        let mut spans = vec![
            Span::styled(" ● ", dim),
            Span::styled(toast.title.as_str(), dim),
        ];
        if !toast.context.is_empty() {
            spans.push(Span::styled(format!(" · {}", toast.context), dim));
        }
        Line::from(spans)
    } else {
        Line::default()
    };

    frame.render_widget(Paragraph::new(line).style(base), area);
}

pub(super) fn render_config_diagnostic(frame: &mut Frame, area: Rect, message: &str, p: &Palette) {
    let style = Style::default()
        .fg(panel_contrast_fg(p))
        .bg(p.yellow)
        .add_modifier(Modifier::BOLD);

    for (row, line) in message
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(area.height as usize)
        .enumerate()
    {
        let text = format!(" config warning: {line} ");
        let width = (text.len() as u16).min(area.width);
        let notif_area = Rect::new(
            area.x + area.width.saturating_sub(width),
            area.y + row as u16,
            width,
            1,
        );

        frame.render_widget(Clear, notif_area);
        frame.render_widget(Paragraph::new(Span::styled(text, style)), notif_area);
    }
}

pub(super) fn agent_icon(
    state: AgentState,
    seen: bool,
    tick: u32,
    p: &Palette,
) -> (&'static str, Style) {
    match (state, seen) {
        (AgentState::Blocked, _) => ("◉", Style::default().fg(p.red)),
        (AgentState::Working, _) => (super::spinner_frame(tick), Style::default().fg(p.yellow)),
        (AgentState::Idle, false) => ("●", Style::default().fg(p.teal)),
        (AgentState::Idle, true) => ("✓", Style::default().fg(p.green)),
        (AgentState::Unknown, _) => ("○", Style::default().fg(p.overlay0)),
    }
}

pub(super) fn state_label(state: AgentState, seen: bool) -> &'static str {
    match (state, seen) {
        (AgentState::Blocked, _) => "blocked",
        (AgentState::Working, _) => "working",
        (AgentState::Idle, false) => "done",
        (AgentState::Idle, true) => "idle",
        (AgentState::Unknown, _) => "idle",
    }
}

pub(super) fn state_label_color(state: AgentState, seen: bool, p: &Palette) -> Color {
    match (state, seen) {
        (AgentState::Blocked, _) => p.red,
        (AgentState::Working, _) => p.yellow,
        (AgentState::Idle, false) => p.teal,
        (AgentState::Idle, true) => p.green,
        (AgentState::Unknown, _) => p.overlay0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::{CopyFeedback, ToastNotification};
    use ratatui::{backend::TestBackend, Terminal};

    fn toast(title: &str, context: &str) -> ToastNotification {
        ToastNotification {
            kind: ToastKind::NeedsAttention,
            title: title.to_string(),
            context: context.to_string(),
            position: None,
            target: None,
        }
    }

    fn rendered_status_line(app: &crate::app::state::AppState) -> String {
        let backend = TestBackend::new(80, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_status_line(app, frame, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.width)
            .map(|x| buffer[(x, 0)].symbol().to_string())
            .collect()
    }

    #[test]
    fn status_line_shows_active_toast_on_one_line() {
        let mut app = crate::app::state::AppState::test_new();
        app.toast = Some(toast("reviewing", "PR #14449 · my_branch"));

        let text = rendered_status_line(&app);
        assert!(
            text.contains("● reviewing · PR #14449 · my_branch"),
            "unexpected status line: {text:?}"
        );
    }

    #[test]
    fn status_line_keeps_showing_last_toast_after_expiry() {
        let mut app = crate::app::state::AppState::test_new();
        app.toast = None;
        app.last_toast = Some(toast("checked out", "my_branch"));

        let text = rendered_status_line(&app);
        assert!(
            text.contains("● checked out · my_branch"),
            "unexpected status line: {text:?}"
        );
    }

    #[test]
    fn status_line_prefers_copy_feedback_over_toast() {
        let mut app = crate::app::state::AppState::test_new();
        app.toast = Some(toast("reviewing", "PR #1"));
        app.copy_feedback = Some(CopyFeedback {
            message: "copied to clipboard".to_string(),
        });

        let text = rendered_status_line(&app);
        assert!(
            text.contains("copied to clipboard"),
            "unexpected status line: {text:?}"
        );
        assert!(!text.contains("reviewing"));
    }
}
