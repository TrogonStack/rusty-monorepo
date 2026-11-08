use super::super::{PermissionGrant, Runner};

#[test]
fn claude_code_and_codex_enforce_exactly_what_was_requested() {
    for runner in [Runner::ClaudeCode, Runner::Codex] {
        assert_eq!(
            runner.effective_permission_grant(PermissionGrant::WorkspaceWrite),
            PermissionGrant::WorkspaceWrite
        );
        assert_eq!(
            runner.effective_permission_grant(PermissionGrant::Unrestricted),
            PermissionGrant::Unrestricted
        );
    }
}

/// cursor-agent's only non-interactive mode is `--force`, so a run under it always
/// executes unrestricted, regardless of what was requested.
#[test]
fn cursor_agent_always_runs_unrestricted() {
    assert_eq!(
        Runner::CursorAgent.effective_permission_grant(PermissionGrant::WorkspaceWrite),
        PermissionGrant::Unrestricted
    );
    assert_eq!(
        Runner::CursorAgent.effective_permission_grant(PermissionGrant::Unrestricted),
        PermissionGrant::Unrestricted
    );
}
