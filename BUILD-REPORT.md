# Build Report — PR Inbox Sidebar

Remote build agent run on branch `pr-inbox-sidebar` (worktree `agent-a592607112bb5986e`).

## Phase A — Sidebar Redesign: Inline Agent + Status on Worktree Rows

### A1 — `primary_pane_detail()` on Workspace aggregate
- Added `#[derive(Clone)]` to `PaneDetail` in `src/workspace/aggregate.rs`.
- Added `primary_pane_detail(&self, terminals) -> Option<PaneDetail>` returning the detail for the focused pane (falling back to the first terminal pane).
- Re-exported `aggregate::PaneDetail` via `src/workspace.rs`.
- Tests: `primary_pane_detail_follows_focused_pane`, `primary_pane_detail_none_when_no_terminals`.

### A2 — Inline agent+status spans on worktree rows
- Added `worktree_agent_spans(primary, badge_state, badge_seen, extra, expanded, p)` in `src/ui/sidebar.rs`.
- Updated the indented-workspace branch in `render_workspace_list` to append these spans.
- Test: `worktree_agent_spans_includes_label_agent_and_count`.

### A3 — Expand/collapse per-agent sub-rows
- Added `expanded_worktree_agents: HashSet<String>` and `toggle_worktree_agents(&mut self, ws_id)` to `AppState`.
- Added `WorktreeAgentToggleArea { ws_idx, rect }` to `ViewState`.
- Extended `compute_workspace_list_areas()` to emit toggle areas and sub-rows.
- Mouse handler in `src/app/input/mouse.rs` hits the toggle area before the workspace card.
- Test: `toggle_worktree_agents_flips_membership`.

Committed: `feat(workspace)`, `feat(sidebar)` ×2

## Phase B — Personal PR Inbox as Server-Owned State

### B1 — PR inbox core types and gh parser
- Added `src/pr_inbox/mod.rs` with `PullRequestSummary`, `PullRequestInboxStatus`, `PullRequestInbox`.
- `parse_gh_search_prs()` parses `gh search prs --json` output.
- `fetch_pr_inbox()` maps every `gh` failure mode (not installed, not authed, error) to a status; never panics.
- Tests: `parses_gh_search_prs`, `parses_empty_list`.

Committed: `feat(pr-inbox): add core types and gh search parser`

### B3+B4 — Server-owned state, API schema, wire protocol
- Bumped `PROTOCOL_VERSION` to 15 in `src/protocol/wire.rs`.
- Added `Method::PrInboxList` and `Method::PrInboxRefresh` to `src/api/schema.rs`.
- Added `ResponseResult::PrInboxList` to `src/api/schema/response.rs`.
- Added `Subscription::PrInboxRefreshed`, `EventKind::PrInboxRefreshed`, `EventData::PrInboxRefreshed` to `src/api/schema/events.rs`; wired into `KNOWN_EVENT_KINDS`.
- Added `ActiveSubscription` arm in `src/api/subscriptions.rs`.
- Added `api_method_name` arms in `src/api/server.rs`.
- Added `pr_inbox: PullRequestInbox` and `pr_inbox_scroll: usize` to `AppState`.
- Added `PR_INBOX_REFRESH_INTERVAL`, `last_pr_inbox_refresh`, `pr_inbox_refresh_in_flight` to `App`.
- Added `AppEvent::PrInboxRefreshed` to `src/events.rs`.
- Added poller methods (`pr_inbox_refresh_deadline`, `start_pr_inbox_refresh_if_due`, `mark_pr_inbox_refresh_due`) to `src/app/runtime.rs`.
- Hooked poller into both `handle_scheduled_tasks` and headless scheduler (`src/server/headless.rs`).
- Added `handle_pr_inbox_list`, `handle_pr_inbox_refresh`, `PrInboxRefreshed` event handler to `src/app/api.rs`.
- Exhaustive match arm in `src/app/api/plugins/context.rs`.
- Test: `pr_inbox_refreshed_event_round_trips`.

Committed: `feat(api): add server-owned pr_inbox state, pr_inbox.list query, and refreshed event (protocol 15)`

### B6 — Render PR inbox in sidebar bottom section
- Replaced `render_agent_detail` call in `render_sidebar` with `render_pr_inbox`.
- Added `render_pr_inbox()`, `pr_inbox_body_rect()`, `pr_inbox_scroll_metrics()`, `pr_inbox_scrollbar_rect()` to `src/ui/sidebar.rs`.
- Renders status messages for Loading/GhNotInstalled/GhNotAuthed/Error states.
- Per-PR rows: `[draft]` prefix, truncated title, `#N repo` sub-line, gap row.
- Scroll support via `pr_inbox_scroll`.

### B7 — Remove dead render scaffolding
- Removed `render_agent_detail` (no longer called from render path).
- Removed `format_agent_panel_primary_label` (only used inside render_agent_detail).
- Removed `all_workspaces_primary_label_truncates_workspace_and_tab` test.
- Note: `agent_panel_body_rect`, `agent_panel_scroll_metrics`, etc. are retained because they are still used by mouse/input handlers (`input/sidebar.rs`, `actions.rs`, `mouse.rs`). Full removal is a follow-up migration.

Committed: `feat(sidebar): replace agent panel with pr inbox in sidebar bottom section`

### B8 — On-demand PR inbox refresh keybinding
- Added `RefreshPrInbox` to `NavigateAction` enum.
- Added `request_pr_inbox_refresh: bool` flag to `AppState`.
- Handle in `execute_navigate_action_in_context` (sets flag) and `handle_scheduled_tasks` (calls `mark_pr_inbox_refresh_due` → immediate re-fetch).
- Added `refresh_pr_inbox: BindingConfig` to `KeysConfig` and `Keybinds` structs; default is unbound (no key conflict).
- Wired in `action_for_key` table.

Committed: `feat(input): add refresh_pr_inbox keybinding action (unbound by default)`

## Validation

- `cargo check`: zero errors, zero warnings (after all tasks).
- `cargo nextest run -E 'not kind(test)'`: **2187/2187 tests pass**.
- `cargo fmt --check`: clean (formatting applied in final style commit).
- Integration tests in `tests/` skipped as documented (require live env).
- `just` not available in build sandbox; `cargo nextest` used directly per memory notes.

## Test failure fixed

`headless_next_loop_deadline_returns_none_when_resize_poll_is_only_deadline` began failing because the new PR inbox deadline contributed a future instant when all other deadlines were suppressed. Fixed by adding `app.pr_inbox_refresh_in_flight = true` in the test setup to suppress that deadline.

## Scope notes

- B5 (protocol 15 hardcoded test expectations) — not applicable; the protocol version is tested via round-trip tests, not hardcoded fixture values.
- B7 partial: `AgentPanelEntry`, `agent_panel_entries*`, `AgentPanelSort`, `agent_panel_scroll/sort` fields are retained because the input handlers still reference them. Removing them requires migrating `input/sidebar.rs` click/scroll handlers and several tests — scoped as a follow-up.
