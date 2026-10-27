use std::sync::Arc;

use super::SessionTask;
use super::SessionTaskResult;
use super::emit_compact_metric;
use crate::compact::run_compact_task;
use crate::compact::run_compact_task_in_started_turn;
use crate::compact::summarization_input;
use crate::compact_model_fallback::should_fall_back_to_local_compaction;
use crate::compact_remote_v2::run_remote_compact_task;
use crate::session::TurnInput;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::state::TaskKind;
use codex_features::Feature;
use codex_model_provider::RemoteCompactionSupport;
use codex_protocol::error::CodexErrorDetails;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Default)]
pub(crate) struct CompactTask;

impl SessionTask for CompactTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Compact
    }

    fn span_name(&self) -> &'static str {
        "session_task.compact"
    }

    async fn run(
        self: Arc<Self>,
        session: Arc<Session>,
        ctx: Arc<TurnContext>,
        _input: Vec<TurnInput>,
        _cancellation_token: CancellationToken,
    ) -> SessionTaskResult {
        let _profile_guard = ctx.turn_timing_state.begin_compaction();
        if ctx.config.features.enabled(Feature::TokenBudget) {
            crate::compact_token_budget::run_manual_compact_task(session, ctx).await?;
            return Ok(None);
        }

        let result = match ctx.provider.capabilities().remote_compaction {
            RemoteCompactionSupport::V2 => {
                emit_compact_metric(
                    &session.services.session_telemetry,
                    "remote_v2",
                    /*manual*/ true,
                );
                match run_remote_compact_task(session.clone(), Arc::clone(&ctx)).await {
                    Err(err) if should_fall_back_to_local_compaction(&err) => {
                        emit_compact_metric(
                            &session.services.session_telemetry,
                            "local",
                            /*manual*/ true,
                        );
                        // The remote attempt already emitted `TurnStarted` for this turn.
                        let input = summarization_input(&ctx.config);
                        run_compact_task_in_started_turn(session.clone(), ctx, input).await
                    }
                    result => result,
                }
            }
            RemoteCompactionSupport::Unsupported => {
                emit_compact_metric(
                    &session.services.session_telemetry,
                    "local",
                    /*manual*/ true,
                );
                let input = summarization_input(&ctx.config);
                run_compact_task(session.clone(), ctx, input).await
            }
        };
        if let Err(err) = result
            && matches!(err.details(), CodexErrorDetails::TurnAborted)
        {
            return Err(err);
        }
        Ok(None)
    }
}
