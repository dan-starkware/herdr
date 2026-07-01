# Backlog (fork: dan/agent-cockpit)

**These are *possible* tasks, not commitments.** They capture ideas and
follow-ups noted while building the agent-cockpit features. They are not
prioritized and may already be stale.

> Before starting any item below, **check whether it is still valid**: the code
> may have changed, the behavior may already be fixed, or the idea may no longer
> be wanted. Re-confirm the problem reproduces and the approach still fits before
> implementing, then update or remove the entry.

## Graphite / agent worktree cleanup

- **Use `gt delete` (not `git branch -d`) to clean up agent branches in Graphite
  repos.** The current cleanup on agent-worktree removal
  (`worktree::try_delete_merged_branch`, wired in `app/worktrees.rs`) runs
  `git branch -d`. In a gt-tracked repo that usually *refuses* — a stacked agent
  branch carries its parent's commits, so git sees it as "unmerged" vs the
  current branch — and it also leaves Graphite metadata stale and does not
  restack children. For gt repos, remove the branch with `gt delete <branch>`
  (stack-aware: untracks + restacks). Gate it to branches with no *unique* work
  vs their gt parent so real changes are never dropped.

- **Reconsider auto-`gt track` of agent branches.** Creating an agent worktree
  in a gt repo currently `gt track`s the new branch (see
  `worktree::graphite_track`, called from `create_agent_in_worktree_for`), so
  every ephemeral agent joins the user's real stack and shows up in `gt ls` /
  `gt log`. Consider not tracking agent branches by default (keep them plain git
  branches, invisible to Graphite) and only "promote" one into the stack on
  demand. This is the root cause of stack clutter.

- **One-off: clean existing orphaned agent branches.** As of 2026-06-29 the
  sequencer repo had orphaned `sequencer-3/4/5/6/7` branches (no worktree) left
  by killed agents; `-6`/`-7` were empty. Likely already handled — verify with
  `gt ls` before acting, and prefer `gt delete` for tracked ones.

## Other parked ideas

- **Clipboard "copied" toast can lie.** When no native clipboard tool is
  installed and the terminal ignores OSC 52, the copy silently fails but the
  toast still says "copied". OSC 52 has no ack, so success isn't detectable —
  at best differentiate "copied (native)" vs "sent to terminal". Low value;
  parked.

- **Branch picker: name a new branch *and* pick a stack base in one step.** The
  diff/agent branch chooser is a single input (filter == new-branch name), so
  you can't both type a name and highlight a base. A second input (like Shahak's
  separate name field) would allow it.

- **Release hygiene: squash the stray merge commit.** The agent-cockpit line
  contains a `merge dan/agent-cockpit into herdr-1` commit (`f18e1db`). Before
  landing on a release, consider rebasing/squashing for clean release notes.

- **`just install` recipe.** A one-shot `server stop && install -m755 … && herdr`
  to avoid running a stale binary after each rebuild.

## PR inbox + inline-agent sidebar (landed 2026-07-01)

Follow-ups deferred when the PR-inbox / inline-agent-row feature was integrated
onto `master`:

- **Wire PR-inbox scrollbar input.** `render_pr_inbox` draws a scrollbar and
  `pr_inbox_scroll`/`pr_inbox_scroll_metrics` exist, but no input handler
  mutates `pr_inbox_scroll` — wheel/click/drag on the PR list do nothing (the
  list is capped at 30, so it always shows from the top). Wire it the way the
  old agent panel's scroll was.

- **Bind a key to the manual PR refresh.** The `RefreshPrInbox` navigate action
  and `pr_inbox.refresh` API method exist, but `refresh_pr_inbox` is unbound by
  default (to avoid clashes). Pick a default binding.

- **Per-PR review/CI indicator.** `gh search prs --json` cannot supply
  `reviewDecision`/`statusCheckRollup`, so rows show only repo · #num · title ·
  draft/open. Enriching needs a per-PR `gh pr view`/`gh pr checks` call or a
  GraphQL query.

- **Finish removing the dead agent-panel scaffolding (B7 was partial).**
  `render_agent_detail` is gone, but `AgentPanelEntry`, `agent_panel_sort`,
  `agent_panel_scroll`, and `AgentPanelSort` are retained because
  `input/sidebar.rs` and the mouse/scroll handlers still reference them.
  Removing them requires migrating those handlers.

- **UX: worktree-row line 1 may be cluttered.** After integrating onto the
  spaces/worktree sidebar, an indented row's line 1 can carry both the existing
  branch / agent-by-branch label and the new inline agent+status+caret. Eyeball
  it on the running TUI and trim redundancy.

- **Cross-highlight a PR against its local worktree.** If a PR's head branch
  matches an open worktree's branch, highlight across the two sidebar sections
  (deferred from the original PR-inbox spec).
