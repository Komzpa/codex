use super::session::Session;
use super::turn_context::TurnContext;
use crate::context::ContextualUserFragment;
use crate::context::QueuedFollowupAwareness;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;

/// Coalesce UI changes and append a bounded snapshot at the next model boundary.
/// Zero invalidates notices retained by a resumed or compacted conversation
/// without rewriting history. Sessions with no queue notices stay unchanged.
pub(super) async fn maybe_record(sess: &Session, turn_context: &TurnContext, window_id: &str) {
    let count = {
        let mut state = sess.state.lock().await;
        let count = state.queued_followup_count;
        if state.queued_followup_last_reported.as_ref().is_some_and(
            |(previous_window, previous_count)| {
                previous_window == window_id && *previous_count == count
            },
        ) {
            return;
        }
        let needs_notice = count > 0
            || state
                .queued_followup_last_reported
                .as_ref()
                .is_some_and(|(_, n)| *n > 0)
            || state.history.raw_items().any(|item| {
                matches!(item, ResponseItem::Message { role, content, .. }
                if role == "developer" && content.iter().any(|part| {
                    matches!(part, ContentItem::InputText { text }
                        if QueuedFollowupAwareness::matches_text(text))
                }))
            });
        state.queued_followup_last_reported = Some((window_id.to_owned(), count));
        if !needs_notice {
            return;
        }
        count
    };
    let item = ContextualUserFragment::into(QueuedFollowupAwareness::new(count));
    sess.record_conversation_items(
        turn_context,
        turn_context.model_info(),
        std::slice::from_ref(&item),
    )
    .await;
}
