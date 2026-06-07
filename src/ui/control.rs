//! Keyboard-first home sidebar: the Control half (repository list) stacked above
//! the Agents half (running agents). Replaces the legacy spaces/agents sidebar
//! when in [`Mode::Home`].

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::state::{FocusPane, Mode};
use crate::app::AppState;
use crate::terminal::TerminalRuntimeRegistry;

use super::sidebar::{agent_panel_entries_all, expanded_sidebar_sections};
use super::status::{agent_icon, state_label, state_label_color};

const CONTROL_HEADER_ROWS: u16 = 2;

/// Render the home sidebar: repos on top, running agents on the bottom.
pub(super) fn render_home_sidebar(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
) {
    let p = &app.palette;

    // Right-edge separator, accented while the left column has focus.
    let left_focused = matches!(app.control.focus, FocusPane::Control | FocusPane::Agents);
    let sep_style = if left_focused {
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

    let _ = terminal_runtimes;
    let (control_area, agents_area) = expanded_sidebar_sections(area, app.sidebar_section_split);
    if app.mode == Mode::Review {
        render_review_half(app, frame, control_area);
    } else {
        render_control_half(app, frame, control_area);
    }
    render_agents_half(app, frame, agents_area);
}

/// Top half while reviewing: a branch picker for the repository under review.
fn render_review_half(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let p = &app.palette;
    let Some(review) = app.control.review.as_ref() else {
        return;
    };

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" review: {}", review.repo.label),
            Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
        ))),
        Rect::new(area.x, area.y, area.width, 1),
    );

    if area.height <= CONTROL_HEADER_ROWS {
        return;
    }
    let body = Rect::new(
        area.x,
        area.y + CONTROL_HEADER_ROWS,
        area.width,
        area.height - CONTROL_HEADER_ROWS,
    );
    let footer_y = area.y + area.height.saturating_sub(1);
    let list_rows = body.height.saturating_sub(1) as usize;

    if review.branches.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " no branches",
                Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
            ))),
            Rect::new(body.x, body.y, body.width, 1),
        );
    } else {
        let scroll = review.scroll.min(review.selected);
        for (row, (idx, branch)) in review
            .branches
            .iter()
            .enumerate()
            .skip(scroll)
            .enumerate()
        {
            if row >= list_rows {
                break;
            }
            let y = body.y + row as u16;
            let selected = idx == review.selected;
            if selected {
                let buf = frame.buffer_mut();
                for x in body.x..body.x + body.width {
                    buf[(x, y)].set_style(Style::default().bg(p.surface0));
                }
            }
            let label_style = if selected {
                Style::default().fg(p.text).add_modifier(Modifier::BOLD)
            } else if branch.is_remote {
                Style::default().fg(p.overlay0)
            } else {
                Style::default().fg(p.subtext0)
            };
            let marker = if branch.is_current { "● " } else { "  " };
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(marker, Style::default().fg(p.green)),
                    Span::styled(
                        truncate(&branch.name, body.width.saturating_sub(3) as usize),
                        label_style,
                    ),
                ]))
                .style(if selected {
                    Style::default().bg(p.surface0)
                } else {
                    Style::default()
                }),
                Rect::new(body.x, y, body.width, 1),
            );
        }
    }

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " enter open · alt+p pr · esc back",
            Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
        ))),
        Rect::new(area.x, footer_y, area.width, 1),
    );
}

/// Bottom half: every running agent with title, status, repo, and summary;
/// the selected agent is highlighted when the Agents pane has focus.
fn render_agents_half(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height < 2 {
        return;
    }
    let p = &app.palette;
    let focused = app.control.focus == FocusPane::Agents;
    let dim = Style::default().fg(p.overlay0).add_modifier(Modifier::DIM);

    let separator = "─".repeat(area.width as usize);
    frame.render_widget(
        Paragraph::new(Span::styled(separator, Style::default().fg(p.surface_dim))),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let header_style = if focused {
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD)
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(" agents", header_style))),
        Rect::new(area.x, area.y + 1, area.width, 1),
    );

    let body = Rect::new(area.x, area.y + 2, area.width, area.height.saturating_sub(2));
    if body.height == 0 {
        return;
    }

    let entries = agent_panel_entries_all(app);
    if entries.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(" no running agents", dim))),
            Rect::new(body.x, body.y, body.width, 1),
        );
        return;
    }

    let mut y = body.y;
    let body_bottom = body.y + body.height;
    for (idx, entry) in entries.iter().enumerate() {
        if y + 1 >= body_bottom {
            break;
        }
        let selected = idx == app.control.selected_agent;
        if selected {
            let bg = if focused { p.surface0 } else { p.surface_dim };
            let buf = frame.buffer_mut();
            for ry in y..(y + 2).min(body_bottom) {
                for x in body.x..body.x + body.width {
                    buf[(x, ry)].set_style(Style::default().bg(bg));
                }
            }
        }

        let (icon, icon_style) = agent_icon(entry.state, entry.seen, app.spinner_tick, p);
        let name_style = if selected {
            Style::default().fg(p.text).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0).add_modifier(Modifier::BOLD)
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" ", Style::default()),
                Span::styled(icon, icon_style),
                Span::styled(" ", Style::default()),
                Span::styled(
                    truncate(&entry.primary_label, body.width.saturating_sub(3) as usize),
                    name_style,
                ),
            ])),
            Rect::new(body.x, y, body.width, 1),
        );
        y += 1;

        let label = state_label(entry.state, entry.seen);
        let label_color = state_label_color(entry.state, entry.seen, p);
        let mut spans = vec![
            Span::styled("   ", Style::default()),
            Span::styled(label, Style::default().fg(label_color)),
        ];
        if let Some(repo) = app
            .workspaces
            .get(entry.ws_idx)
            .and_then(|ws| ws.worktree_space().map(|m| m.label.clone()))
        {
            spans.push(Span::styled(" · ", dim));
            spans.push(Span::styled(repo, dim));
        }
        if let Some(summary) = &entry.custom_status {
            spans.push(Span::styled(" · ", dim));
            spans.push(Span::styled(truncate(summary, 28), dim));
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(body.x, y, body.width, 1),
        );
        y += 2; // status line + a one-row gap
    }
}

/// Confirmation modal for killing the selected agent.
pub(super) fn render_confirm_kill_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    super::dim_background(frame, area);
    let p = &app.palette;
    let title = agent_panel_entries_all(app)
        .get(app.control.selected_agent)
        .map(|entry| entry.primary_label.clone())
        .unwrap_or_else(|| "agent".to_string());

    let Some(inner) = super::widgets::render_modal_shell(frame, area, 52, 6, p) else {
        return;
    };
    if inner.height < 2 {
        return;
    }
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(0)])
        .split(inner);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("kill agent “{title}”?"),
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        ))),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "y kill · n cancel",
            Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
        ))),
        rows[1],
    );
}

fn render_control_half(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let p = &app.palette;
    let focused = app.control.focus == FocusPane::Control;

    let header_style = if focused {
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD)
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(" repos", header_style))),
        Rect::new(area.x, area.y, area.width, 1),
    );

    if area.height <= CONTROL_HEADER_ROWS {
        return;
    }
    let body = Rect::new(
        area.x,
        area.y + CONTROL_HEADER_ROWS,
        area.width,
        area.height - CONTROL_HEADER_ROWS,
    );

    if app.control.repos.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " no repos in ~/workspace",
                Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
            ))),
            Rect::new(body.x, body.y, body.width, 1),
        );
        return;
    }

    let scroll = app.control.repo_scroll.min(app.control.repos.len().saturating_sub(1));
    for (row, (idx, repo)) in app
        .control
        .repos
        .iter()
        .enumerate()
        .skip(scroll)
        .enumerate()
    {
        if row as u16 >= body.height {
            break;
        }
        let y = body.y + row as u16;
        let selected = idx == app.control.selected_repo;
        let row_rect = Rect::new(body.x, y, body.width, 1);

        if selected {
            let bg = if focused { p.surface0 } else { p.surface_dim };
            let buf = frame.buffer_mut();
            for x in row_rect.x..row_rect.x + row_rect.width {
                buf[(x, y)].set_style(Style::default().bg(bg));
            }
        }

        let label_style = if selected {
            Style::default().fg(p.text).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0)
        };
        let marker = if selected { "▸ " } else { "  " };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(marker, Style::default().fg(p.accent)),
                Span::styled(truncate(&repo.label, body.width.saturating_sub(3) as usize), label_style),
            ]))
            .style(if selected {
                Style::default().bg(if focused { p.surface0 } else { p.surface_dim })
            } else {
                Style::default()
            }),
            row_rect,
        );
    }

    // Action hint footer when focused and a repo is selected.
    if focused && body.height >= 2 {
        let hint_y = area.y + area.height.saturating_sub(1);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " n new · r review",
                Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
            ))),
            Rect::new(area.x, hint_y, area.width, 1),
        );
    }
}

/// Modal form for naming a new agent/worktree in the selected repository.
pub(super) fn render_create_agent_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    super::dim_background(frame, area);
    let p = &app.palette;
    let repo_label = app
        .control
        .selected_repository()
        .map(|repo| repo.label.clone())
        .unwrap_or_else(|| "?".to_string());

    let Some(inner) = super::widgets::render_modal_shell(frame, area, 56, 7, p) else {
        return;
    };
    if inner.height < 3 {
        return;
    }

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .split(inner);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("new agent in {repo_label}"),
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        ))),
        rows[0],
    );

    let (input_text, input_style) = if app.name_input.is_empty() {
        (
            "name…".to_string(),
            Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
        )
    } else {
        (app.name_input.clone(), Style::default().fg(p.text))
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(input_text, input_style))),
        rows[1],
    );

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "enter create · esc cancel",
            Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
        ))),
        rows[2],
    );
}

fn truncate(text: &str, max_width: usize) -> String {
    let len = text.chars().count();
    if len <= max_width {
        return text.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }
    let prefix: String = text.chars().take(max_width - 1).collect();
    format!("{prefix}…")
}
