use crate::context_manager::ContextManager;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ResponseItem;

const CONTEXT_WINDOW_ERROR: &str = "remote compaction did not install because the exact latest complete tool transaction does not fit the model context window";

/// Reattaches the exact terminal tool transaction that caused a mid-turn compaction.
///
/// Remote compaction output is opaque and intentionally filters tool artifacts. The input trace is
/// the post-normalization, context-fitted history that was actually sent for compaction, so its tool
/// output already reflects the configured model truncation policy. Clone the pair exactly; do not
/// apply another ad-hoc size cap or synthesize an explanatory message in its place.
pub(crate) fn reattach_latest_complete_tool_transaction(
    mut compacted_history: Vec<ResponseItem>,
    trace_input_history: &[ResponseItem],
    context_window: Option<i64>,
    base_instructions: &BaseInstructions,
) -> CodexResult<Vec<ResponseItem>> {
    let Some((call, output)) = latest_terminal_tool_transaction(trace_input_history) else {
        return Ok(compacted_history);
    };

    let already_present = compacted_history
        .windows(2)
        .any(|items| items[0] == *call && items[1] == *output);
    if !already_present {
        let insertion_index = compacted_history
            .iter()
            .position(|item| {
                matches!(
                    item,
                    ResponseItem::Compaction { .. } | ResponseItem::ContextCompaction { .. }
                )
            })
            .unwrap_or(compacted_history.len());
        compacted_history.splice(
            insertion_index..insertion_index,
            [call.clone(), output.clone()],
        );
    }

    if let Some(context_window) = context_window {
        let mut candidate = ContextManager::new();
        candidate.replace(compacted_history.clone());
        if let Some(estimated_tokens) =
            candidate.estimate_token_count_with_base_instructions(base_instructions)
            && estimated_tokens > context_window
        {
            return Err(CodexErr::InvalidRequest(format!(
                "{CONTEXT_WINDOW_ERROR} (estimated {estimated_tokens} tokens, limit {context_window})"
            )));
        }
    }

    Ok(compacted_history)
}

fn latest_terminal_tool_transaction(
    history: &[ResponseItem],
) -> Option<(&ResponseItem, &ResponseItem)> {
    let (output, preceding) = history.split_last()?;
    if !matches!(
        output,
        ResponseItem::FunctionCallOutput { .. }
            | ResponseItem::CustomToolCallOutput { .. }
            | ResponseItem::ToolSearchOutput {
                call_id: Some(_),
                ..
            }
    ) {
        return None;
    }

    let call = preceding.last()?;
    tool_call_matches_output(call, output).then_some((call, output))
}

fn tool_call_matches_output(call: &ResponseItem, output: &ResponseItem) -> bool {
    match (call, output) {
        (
            ResponseItem::FunctionCall {
                call_id: call_id_a, ..
            }
            | ResponseItem::LocalShellCall {
                call_id: Some(call_id_a),
                ..
            },
            ResponseItem::FunctionCallOutput {
                call_id: call_id_b, ..
            },
        ) => call_id_a == call_id_b,
        (
            ResponseItem::CustomToolCall {
                call_id: call_id_a, ..
            },
            ResponseItem::CustomToolCallOutput {
                call_id: call_id_b, ..
            },
        ) => call_id_a == call_id_b,
        (
            ResponseItem::ToolSearchCall {
                call_id: Some(call_id_a),
                ..
            },
            ResponseItem::ToolSearchOutput {
                call_id: Some(call_id_b),
                ..
            },
        ) => call_id_a == call_id_b,
        _ => false,
    }
}

#[cfg(test)]
#[path = "compact_remote_tool_transaction_tests.rs"]
mod tests;
