use codex_analytics::CompactionImplementation;
use codex_analytics::CompactionReason;
use codex_otel::SessionTelemetry;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use tracing::warn;

/// Retries failures that may be model-specific and succeed with a different model.
pub(crate) fn should_retry_with_current_model(error: &CodexErr) -> bool {
    matches!(
        error.details(),
        CodexErrorDetails::InvalidRequest(_)
            | CodexErrorDetails::UnexpectedStatus(_)
            | CodexErrorDetails::ContextWindowExceeded
            | CodexErrorDetails::UsageLimitReached(_)
            | CodexErrorDetails::ServerOverloaded
            | CodexErrorDetails::InternalServerError
            | CodexErrorDetails::RetryLimit(_)
    )
}

/// Falls back to local summarization after remote compaction v2 fails.
///
/// Remote compaction support is decided by the provider name, so an OpenAI-compatible proxy that
/// presents itself as OpenAI while routing to third-party models is treated as if it could serve
/// remote compaction. Those models cannot produce a `compaction` item, and the proxy may reject the
/// request outright, so a model- or provider-specific failure retries with local compaction instead
/// of aborting the turn. Aborts and tool collisions are never retried.
pub(crate) fn should_fall_back_to_local_compaction(error: &CodexErr) -> bool {
    should_retry_with_current_model(error)
}

pub(crate) fn record_model_fallback(
    session_telemetry: &SessionTelemetry,
    previous_model: &str,
    current_model: &str,
    reason: CompactionReason,
    implementation: CompactionImplementation,
    fallback_error: Option<&CodexErr>,
) {
    let reason_tag = match reason {
        CompactionReason::UserRequested => "user_requested",
        CompactionReason::ContextLimit => "context_limit",
        CompactionReason::ModelDownshift => "model_downshift",
        CompactionReason::CompHashChanged => "comp_hash_changed",
    };
    let implementation_tag = match implementation {
        CompactionImplementation::Responses => "responses",
        CompactionImplementation::ResponsesCompactionV2 => "responses_compaction_v2",
    };
    let outcome = if fallback_error.is_none() {
        "succeeded"
    } else {
        "failed"
    };
    session_telemetry.counter(
        "codex.compaction.model_fallback",
        /*inc*/ 1,
        &[
            ("reason", reason_tag),
            ("implementation", implementation_tag),
            ("outcome", outcome),
        ],
    );
    warn!(
        previous_model,
        current_model,
        ?reason,
        ?implementation,
        outcome,
        ?fallback_error,
        "previous-model compaction failed; retried with current model"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::error::UnexpectedResponseError;
    use codex_protocol::error::UsageLimitReachedError;

    #[test]
    fn falls_back_to_local_compaction_for_model_specific_failures() {
        let usage_limit = CodexErr::UsageLimitReached(UsageLimitReachedError {
            plan_type: None,
            resets_at: None,
            rate_limits: None,
            promo_message: None,
            rate_limit_reached_type: None,
        });
        assert!(should_fall_back_to_local_compaction(&usage_limit));
        assert!(should_fall_back_to_local_compaction(
            &CodexErr::InvalidRequest("unsupported input item".to_string())
        ));
        assert!(should_fall_back_to_local_compaction(
            &CodexErr::UnexpectedStatus(UnexpectedResponseError {
                status: http::StatusCode::BAD_GATEWAY,
                body: "proxy unavailable".to_string(),
                user_message: None,
                url: None,
                cf_ray: None,
                request_id: None,
                identity_authorization_error: None,
                identity_error_code: None,
            })
        ));
        assert!(should_fall_back_to_local_compaction(
            &CodexErr::InternalServerError
        ));
    }

    #[test]
    fn never_falls_back_to_local_compaction_for_aborts_or_collisions() {
        assert!(!should_fall_back_to_local_compaction(
            &CodexErr::TurnAborted
        ));
        assert!(!should_fall_back_to_local_compaction(&CodexErr::new(
            CodexErrorDetails::ToolCollision("duplicate tool".to_string())
        )));
        assert!(!should_fall_back_to_local_compaction(
            &CodexErr::Interrupted
        ));
    }
}
