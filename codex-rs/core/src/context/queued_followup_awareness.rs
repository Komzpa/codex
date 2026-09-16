use super::ContextualUserFragment;
use codex_protocol::models::ContentItemKind;

/// Bounded, content-free awareness of later TUI follow-ups.
pub(crate) struct QueuedFollowupAwareness {
    count: u32,
}

impl QueuedFollowupAwareness {
    pub(crate) fn new(count: u32) -> Self {
        Self { count }
    }
}

impl ContextualUserFragment for QueuedFollowupAwareness {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("queued_followup.awareness".to_string())
    }

    fn role(&self) -> &'static str {
        "developer"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        (
            "<queued_followup_awareness>",
            "</queued_followup_awareness>",
        )
    }

    fn body(&self) -> String {
        if self.count == 0 {
            return "The user has 0 queued follow-up messages. This clears any earlier queue notice; do not assume another user iteration is pending. Continue the current task normally.".to_string();
        }
        format!(
            "The user has {} queued follow-up messages. This supersedes earlier queue counts. Their contents are withheld until the next normal turn. Finish the meaningful current work, report remaining checks and yield when practical. Defer optional exhaustive tests to the next iteration; required correctness and safety checks still apply.",
            self.count
        )
    }
}
