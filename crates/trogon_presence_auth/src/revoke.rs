use tracing::Instrument;

use crate::commands::{AuthCommands, CommandError, CommandOutcome, UserTarget};
use crate::rows::SweepStatus;
use crate::sweep::{SweepDeadline, SweepError, SweepExecutor, SweepReport};
use crate::telemetry;

/// Operational result of a revoke: the committed authority state and the sweep, reported separately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevokeReport {
    pub outcome: CommandOutcome,
    pub sweep: Option<SweepReport>,
}

impl RevokeReport {
    pub fn state_committed(&self) -> bool {
        self.outcome.state_committed()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RevokeError {
    #[error("revoking the user: {0}")]
    Command(#[from] CommandError),
    #[error("sweeping obsolete grants: {0}")]
    Sweep(#[from] SweepError),
}

/// Commits the user revocation and its durable sweep, then drives the sweep until it completes
/// or the deadline passes. A sweep left incomplete stays stored for the auth process to resume.
pub async fn revoke(
    commands: &AuthCommands,
    executor: &SweepExecutor,
    target: &UserTarget,
    deadline: SweepDeadline,
) -> Result<RevokeReport, RevokeError> {
    let span = tracing::info_span!(telemetry::REVOKE_SPAN, "presence.auth.kicked" = tracing::field::Empty);
    async {
        let outcome = commands.revoke_user(target).await?;
        let sweep = match outcome.sweep() {
            SweepStatus::NotRequired => None,
            SweepStatus::Pending { .. } => Some(executor.drive(target, deadline).await?),
        };
        tracing::Span::current().record(
            "presence.auth.kicked",
            sweep.as_ref().map_or(0, |report| report.kicked.len()),
        );
        telemetry::record_revocation();
        Ok(RevokeReport { outcome, sweep })
    }
    .instrument(span)
    .await
}
