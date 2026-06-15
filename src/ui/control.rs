//! Keyboard-first home sidebar: the Control half (repository list) stacked above
//! the Agents half (running agents). Replaces the legacy spaces/agents sidebar
//! when in [`Mode::Home`].

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::app::state::{CreateFormRow, FocusPane, Mode};
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
    let _ = terminal_runtimes;
    let (control_area, agents_area) = expanded_sidebar_sections(area, app.sidebar_section_split);

    // Each half is framed in a focus box: a THICK accent border when it holds
    // focus, a plain dim border otherwise. The Review picker lives in the
    // Control half, so it shares the Control focus state.
    let control_inner =
        render_home_pane_box(app, frame, control_area, app.control.focus == FocusPane::Control);
    if app.mode == Mode::Review {
        render_review_half(app, frame, control_inner);
    } else {
        render_control_half(app, frame, control_inner);
    }

    let agents_inner =
        render_home_pane_box(app, frame, agents_area, app.control.focus == FocusPane::Agents);
    render_agents_half(app, frame, agents_inner);
}

/// Draw a focus box around a home sidebar half and return its inner rect.
fn render_home_pane_box(app: &AppState, frame: &mut Frame, area: Rect, focused: bool) -> Rect {
    if area.width < 2 || area.height < 2 {
        return area;
    }
    let p = &app.palette;
    let (style, border_set) = if focused {
        (
            Style::default().fg(p.accent),
            ratatui::symbols::border::THICK,
        )
    } else {
        (
            Style::default().fg(p.surface_dim),
            ratatui::symbols::border::PLAIN,
        )
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(style)
        .border_set(border_set);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
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

    // The branch currently checked out in the Main pane (the active workspace's
    // worktree), when that worktree belongs to the repo being browsed. It gets a
    // distinct filled accent node in the list — like `gt ls`'s checked-out
    // marker — so you can see at a glance which branch is loaded in Main.
    let main_pane_branch = app
        .active
        .and_then(|idx| app.workspaces.get(idx))
        .and_then(|ws| {
            let space = ws.worktree_space()?;
            (crate::worktree::canonical_or_original(&space.repo_root)
                == crate::worktree::canonical_or_original(&review.repo.root))
            .then(|| ws.branch())
            .flatten()
        });

    let prs_shown = review.source == crate::app::state::PickerSource::ReviewRequests;
    let title = if prs_shown {
        format!(" review: {} · awaiting my review", review.repo.label)
    } else {
        format!(" review: {}", review.repo.label)
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title,
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

    if prs_shown {
        render_review_pr_rows(app, frame, body, review, main_pane_branch.as_deref(), list_rows);
    } else if review.branches.is_empty() {
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
            let is_main_pane = main_pane_branch.as_deref() == Some(branch.name.as_str());
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
            // With Graphite's stack graph, show its connector art (`│ ◯  `) and
            // let the node convey the current branch; otherwise fall back to a
            // simple `●` marker for the flat branch list. The branch loaded in
            // the Main pane gets a filled `◉` accent node either way.
            let spans = if branch.graph_prefix.is_empty() {
                let (marker, marker_color) = if is_main_pane {
                    ("◉ ", p.accent)
                } else if branch.is_current {
                    ("● ", p.green)
                } else {
                    ("  ", p.green)
                };
                vec![
                    Span::styled(marker, Style::default().fg(marker_color)),
                    Span::styled(
                        truncate(&branch.name, body.width.saturating_sub(3) as usize),
                        label_style,
                    ),
                ]
            } else {
                let prefix_w = branch.graph_prefix.chars().count();
                let avail = (body.width as usize).saturating_sub(1 + prefix_w);
                // Promote the Main pane's branch to a filled node in the accent
                // colour; gt already marks the repo's own HEAD green, and the
                // remaining stack nodes stay dim.
                let prefix = if is_main_pane {
                    branch.graph_prefix.replacen('◯', "◉", 1)
                } else {
                    branch.graph_prefix.clone()
                };
                let node_style = if is_main_pane {
                    Style::default().fg(p.accent)
                } else if branch.is_current {
                    Style::default().fg(p.green)
                } else {
                    Style::default().fg(p.overlay0)
                };
                vec![
                    Span::styled(" ", Style::default()),
                    Span::styled(prefix, node_style),
                    Span::styled(truncate(&branch.name, avail), label_style),
                ]
            };
            frame.render_widget(
                Paragraph::new(Line::from(spans))
                .style(if selected {
                    Style::default().bg(p.surface0)
                } else {
                    Style::default()
                }),
                Rect::new(body.x, y, body.width, 1),
            );
        }
    }

    // `O`'s PR-number input takes over the footer line while collecting
    // digits; the key hints come back when it closes.
    if let Some(input) = &review.pr_number_input {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    " open PR #: ",
                    Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{input}▏"),
                    Style::default().fg(p.text).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "  enter open · esc cancel",
                    Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
                ),
            ])),
            Rect::new(area.x, footer_y, area.width, 1),
        );
        return;
    }
    let footer = if prs_shown {
        " space open · c checkout · o branches · O pr# · esc back"
    } else {
        " space open · c checkout · alt+p pr · o prs · O pr# · esc back"
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            footer,
            Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
        ))),
        Rect::new(area.x, footer_y, area.width, 1),
    );
}

/// The review picker's PR list: one row per open PR awaiting the user's
/// review, `#number title · author`. The PR whose head branch is checked out
/// in the Main pane gets the same filled accent node as the branch list.
fn render_review_pr_rows(
    app: &AppState,
    frame: &mut Frame,
    body: Rect,
    review: &crate::app::state::ReviewState,
    main_pane_branch: Option<&str>,
    list_rows: usize,
) {
    let p = &app.palette;
    let prs = review.prs.as_deref().unwrap_or_default();
    if prs.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " no PRs awaiting review",
                Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
            ))),
            Rect::new(body.x, body.y, body.width, 1),
        );
        return;
    }

    let scroll = review.scroll.min(review.selected);
    for (row, (idx, pr)) in prs.iter().enumerate().skip(scroll).enumerate() {
        if row >= list_rows {
            break;
        }
        let y = body.y + row as u16;
        let selected = idx == review.selected;
        let is_main_pane = main_pane_branch == Some(pr.head_branch.as_str());
        let label_style = if selected {
            Style::default().fg(p.text).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0)
        };
        // The marker column carries the stack art (`◯ ` on stack tops, `│ `
        // below them, blank for standalone PRs). The PR checked out in the
        // Main pane is promoted to a filled accent node, like the branch list.
        let (marker, marker_color) = if is_main_pane {
            (
                if pr.graph_prefix.contains('◯') {
                    pr.graph_prefix.replacen('◯', "◉", 1)
                } else {
                    "◉ ".to_string()
                },
                p.accent,
            )
        } else if pr.graph_prefix.is_empty() {
            ("  ".to_string(), p.overlay0)
        } else {
            (pr.graph_prefix.clone(), p.overlay0)
        };
        let number = format!("#{} ", pr.number);
        let author = format!(" · {}", pr.author);
        let title_avail = (body.width as usize)
            .saturating_sub(2 + number.chars().count() + author.chars().count());
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(marker, Style::default().fg(marker_color)),
                Span::styled(number, Style::default().fg(p.accent)),
                Span::styled(truncate(&pr.title, title_avail), label_style),
                Span::styled(
                    author,
                    Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
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

/// Bottom half: every running agent with title, status, repo, and summary;
/// the selected agent is highlighted when the Agents pane has focus.
fn render_agents_half(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height < 2 {
        return;
    }
    let p = &app.palette;
    let focused = app.control.focus == FocusPane::Agents;
    let dim = Style::default().fg(p.overlay0).add_modifier(Modifier::DIM);

    let header_style = if focused {
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD)
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(" agents", header_style))),
        Rect::new(area.x, area.y, area.width, 1),
    );

    // Reserve the bottom row for an action hint when this pane has focus.
    let reserve_footer = focused && area.height >= 4;
    let body_height = area
        .height
        .saturating_sub(1)
        .saturating_sub(u16::from(reserve_footer));
    let body = Rect::new(area.x, area.y + 1, area.width, body_height);
    if reserve_footer {
        let hint_y = area.y + area.height.saturating_sub(1);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " r rename · x kill · p pr",
                Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
            ))),
            Rect::new(area.x, hint_y, area.width, 1),
        );
    }
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

    // When the Agents pane is unfocused, the mark tracks the agent shown in
    // Main instead of the (now-inert) navigable selection.
    let marked = app.marked_agent_index();
    let mut y = body.y;
    let body_bottom = body.y + body.height;
    for (idx, entry) in entries.iter().enumerate() {
        if y + 1 >= body_bottom {
            break;
        }
        let selected = Some(idx) == marked;
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
    let title = app
        .marked_agent_index()
        .and_then(|idx| agent_panel_entries_all(app).get(idx).map(|entry| entry.primary_label.clone()))
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
                " space new · t term",
                Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
            ))),
            Rect::new(area.x, hint_y, area.width, 1),
        );
    }
}

/// Modal form for configuring a new agent/worktree in the selected repository.
/// Rows (name, base branch, new-branch toggle, optional new-branch name) are
/// navigated with up/down; the active row carries a `▸` marker and is editable.
pub(super) fn render_create_agent_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    super::dim_background(frame, area);
    let p = &app.palette;
    let repo_label = app
        .control
        .selected_repository()
        .map(|repo| repo.label.clone())
        .unwrap_or_else(|| "?".to_string());

    let form_rows = CreateFormRow::visible(app.control.create_new_branch);
    // title + gap + field rows + gap + footer, plus 2 for the border.
    let popup_h = form_rows.len() as u16 + 6;
    let Some(inner) = super::widgets::render_modal_shell(frame, area, 60, popup_h, p) else {
        return;
    };
    if inner.height < 4 {
        return;
    }

    let mut constraints = vec![Constraint::Length(1), Constraint::Length(1)];
    constraints.extend(form_rows.iter().map(|_| Constraint::Length(1)));
    constraints.push(Constraint::Length(1));
    constraints.push(Constraint::Min(0));
    let rows = Layout::vertical(constraints).split(inner);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("new agent in {repo_label}"),
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        ))),
        rows[0],
    );

    let active = app.control.create_form_row;
    for (i, row) in form_rows.iter().enumerate() {
        render_create_form_row(app, frame, rows[2 + i], *row, *row == active);
    }

    let footer = rows[rows.len() - 1];
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "↑↓ row · space toggles · enter create · esc cancel",
            Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
        ))),
        footer,
    );
}

/// Render one row of the create-agent form. The active row gets a `▸` marker and
/// its value is highlighted; empty text fields show a dim placeholder.
fn render_create_form_row(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    row: CreateFormRow,
    active: bool,
) {
    let p = &app.palette;
    let marker = if active { "▸ " } else { "  " };
    let mut spans = vec![Span::styled(marker, Style::default().fg(p.accent))];

    if row == CreateFormRow::NewBranchToggle {
        let mark = if app.control.create_new_branch {
            "[x]"
        } else {
            "[ ]"
        };
        let style = if active {
            Style::default().fg(p.text).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.text)
        };
        spans.push(Span::styled(format!("{mark} create a new branch"), style));
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }

    let (label, value, placeholder) = match row {
        CreateFormRow::Name => ("name", app.name_input.clone(), "name…".to_string()),
        CreateFormRow::Base => (
            "branch",
            app.control.create_base_branch.clone().unwrap_or_default(),
            "new branch from HEAD".to_string(),
        ),
        CreateFormRow::NewBranchName => {
            let agent = app.name_input.trim();
            let hint = if agent.is_empty() {
                "agent name".to_string()
            } else {
                agent.to_string()
            };
            (
                "new branch",
                app.control.create_branch_name.clone(),
                hint,
            )
        }
        CreateFormRow::NewBranchToggle => unreachable!("handled above"),
    };

    let label_style = if active {
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.subtext0)
    };
    spans.push(Span::styled(format!("{label:<11}"), label_style));
    if value.is_empty() {
        spans.push(Span::styled(
            placeholder,
            Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
        ));
    } else {
        let value_style = if active {
            Style::default().fg(p.text).bg(p.surface0)
        } else {
            Style::default().fg(p.text)
        };
        spans.push(Span::styled(value, value_style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Confirm prompt for the create-agent flow: the chosen base branch is checked
/// out in another worktree, so offer to create a new branch on top instead.
pub(super) fn render_confirm_create_branch_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    super::dim_background(frame, area);
    let p = &app.palette;
    let base = app
        .control
        .create_base_branch
        .clone()
        .unwrap_or_else(|| "?".to_string());

    let Some(inner) = super::widgets::render_modal_shell(frame, area, 60, 6, p) else {
        return;
    };
    if inner.height < 2 {
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
            format!("Branch “{base}” is checked out in another worktree."),
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        )))
        .wrap(ratatui::widgets::Wrap { trim: true }),
        rows[0],
    );
    // The detach option is only meaningful when we know which worktree to detach.
    let can_detach = app.control.create_conflict_worktree.is_some();
    let hint = if can_detach {
        "y new branch · d detach other worktree · n cancel"
    } else {
        "y new branch · n cancel"
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
        )))
        .wrap(ratatui::widgets::Wrap { trim: true }),
        rows[2],
    );
}

/// Confirmation modal for the branch picker's `c` action when the selected
/// branch is checked out in another worktree: detach it to free the branch, or
/// cancel back to the picker.
pub(super) fn render_confirm_checkout_detach_overlay(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
) {
    super::dim_background(frame, area);
    let p = &app.palette;
    let Some(conflict) = app.control.checkout_conflict.as_ref() else {
        return;
    };
    let other = conflict
        .worktree
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| conflict.worktree.display().to_string());

    let Some(inner) = super::widgets::render_modal_shell(frame, area, 60, 6, p) else {
        return;
    };
    if inner.height < 2 {
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
            format!(
                "Branch “{}” is checked out in worktree “{other}”.",
                conflict.branch
            ),
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        )))
        .wrap(ratatui::widgets::Wrap { trim: true }),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "d detach it & check out here · n cancel",
            Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
        )))
        .wrap(ratatui::widgets::Wrap { trim: true }),
        rows[2],
    );
}

/// Confirmation modal for quitting herdr.
pub(super) fn render_confirm_quit_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    super::dim_background(frame, area);
    let p = &app.palette;

    let Some(inner) = super::widgets::render_modal_shell(frame, area, 44, 6, p) else {
        return;
    };
    if inner.height < 2 {
        return;
    }
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(0)])
        .split(inner);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "quit herdr?",
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        ))),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "y quit · n cancel",
            Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
        ))),
        rows[1],
    );
}

/// Modal form for renaming the selected agent (and its worktree directory).
pub(super) fn render_rename_agent_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    super::dim_background(frame, area);
    let p = &app.palette;

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
            "rename agent",
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        ))),
        rows[0],
    );

    let (input_text, input_style) = if app.name_input.is_empty() {
        (
            "name…".to_string(),
            Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
        )
    } else if app.name_input_replace_on_type {
        (
            app.name_input.clone(),
            Style::default().fg(p.text).bg(p.surface0),
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
            "enter rename · esc cancel",
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
