use crate::terminal::TerminalId;

/// The role a pane plays inside a workspace's stacked layout.
///
/// The agent pane is the tab's `root_pane` (the bottom row). A `Review` row is
/// the diff stacked above it (see [`crate::app::App::toggle_review_row`]). The
/// role keeps non-agent rows out of the agents list and lets actions find the
/// review row again to reload or close it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PaneRole {
    #[default]
    Agent,
    Review,
}

/// Viewport state for a pane.
///
/// Terminal identity, cwd, labels, and agent metadata live in TerminalState.
pub struct PaneState {
    pub attached_terminal_id: TerminalId,
    /// Whether the user has seen this pane since its last state change to Idle.
    /// False = "Done" (agent finished while user was in another workspace).
    pub seen: bool,
    /// What this pane represents within its workspace's stacked layout.
    pub role: PaneRole,
}

impl PaneState {
    pub fn new(attached_terminal_id: TerminalId) -> Self {
        Self {
            attached_terminal_id,
            seen: true,
            role: PaneRole::Agent,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_state_defaults_to_agent_role() {
        let pane = PaneState::new(TerminalId::alloc());
        assert_eq!(pane.role, PaneRole::Agent);
    }

    #[test]
    fn pane_role_default_is_agent() {
        assert_eq!(PaneRole::default(), PaneRole::Agent);
    }
}
