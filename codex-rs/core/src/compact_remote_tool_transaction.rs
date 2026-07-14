use crate::context_manager::ContextManager;
use crate::context_manager::truncate_function_output_payload;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ResponseItem;
use codex_utils_output_truncation::TruncationPolicy;

const CONTEXT_WINDOW_ERROR: &str = "remote compaction did not install because the latest complete tool transaction does not fit the model context window";
const ITEM_LIMIT_ERROR: &str = "remote compaction did not install because the latest complete tool transaction contains an item above the 10000-token hard limit";
const MAX_REATTACHED_ITEM_TOKENS: i64 = 10_000;
// Leave room inside the per-item limit for the response-item JSON envelope.
const MAX_REATTACHED_OUTPUT_TOKENS: usize = 9_500;

/// Reattaches the complete terminal tool transaction that caused a mid-turn compaction.
///
/// Remote compaction output is opaque and intentionally filters tool artifacts. The input trace is
/// the post-normalization, context-fitted history that was actually sent for compaction, so its tool
/// output already reflects the configured model truncation policy. Preserve the complete
/// call/output block and ordering, while enforcing the model-context hard cap for each reattached
/// item. A transaction may contain parallel calls followed by their corresponding outputs.
pub(crate) fn reattach_latest_complete_tool_transaction(
    mut compacted_history: Vec<ResponseItem>,
    trace_input_history: &[ResponseItem],
    context_window: Option<i64>,
    base_instructions: &BaseInstructions,
) -> CodexResult<Vec<ResponseItem>> {
    let Some(source_transaction) = latest_terminal_tool_transaction(trace_input_history) else {
        return Ok(compacted_history);
    };
    let transaction = bounded_terminal_tool_transaction(source_transaction)?;

    let bounded_index = compacted_history
        .windows(transaction.len())
        .position(|items| items == transaction.as_slice());
    let source_index = compacted_history
        .windows(source_transaction.len())
        .position(|items| items == source_transaction);
    match (source_index, bounded_index) {
        (Some(source_index), Some(bounded_index)) if source_index != bounded_index => {
            drop(compacted_history.drain(source_index..source_index + source_transaction.len()));
        }
        (Some(source_index), None) => {
            compacted_history.splice(
                source_index..source_index + source_transaction.len(),
                transaction.iter().cloned(),
            );
        }
        (None, None) => {
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
                transaction.iter().cloned(),
            );
        }
        (Some(_), Some(_)) | (None, Some(_)) => {}
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

fn bounded_terminal_tool_transaction(
    transaction: &[ResponseItem],
) -> CodexResult<Vec<ResponseItem>> {
    let mut transaction = transaction.to_vec();
    for item in &mut transaction {
        match item {
            ResponseItem::FunctionCallOutput { output, .. }
            | ResponseItem::CustomToolCallOutput { output, .. } => {
                *output = truncate_function_output_payload(
                    output,
                    TruncationPolicy::Tokens(MAX_REATTACHED_OUTPUT_TOKENS),
                );
            }
            _ => {}
        }
    }

    let empty_instructions = BaseInstructions {
        text: String::new(),
    };
    for item in &transaction {
        let mut item_context = ContextManager::new();
        item_context.replace(vec![item.clone()]);
        if let Some(estimated_tokens) =
            item_context.estimate_token_count_with_base_instructions(&empty_instructions)
            && estimated_tokens > MAX_REATTACHED_ITEM_TOKENS
        {
            return Err(CodexErr::InvalidRequest(format!(
                "{ITEM_LIMIT_ERROR} (estimated {estimated_tokens} tokens)"
            )));
        }
    }

    Ok(transaction)
}

fn latest_terminal_tool_transaction(history: &[ResponseItem]) -> Option<&[ResponseItem]> {
    let mut calls_end = history.len();
    while calls_end > 0 && is_tool_output(&history[calls_end - 1]) {
        calls_end -= 1;
    }
    if calls_end == history.len() {
        return None;
    }

    let mut transaction_start = calls_end;
    while transaction_start > 0 && is_tool_call(&history[transaction_start - 1]) {
        transaction_start -= 1;
    }

    let calls = &history[transaction_start..calls_end];
    let outputs = &history[calls_end..];
    if calls.is_empty() || calls.len() != outputs.len() {
        return None;
    }

    let calls_match_once = calls.iter().all(|call| {
        outputs
            .iter()
            .filter(|output| tool_call_matches_output(call, output))
            .count()
            == 1
    });
    let outputs_match_once = outputs.iter().all(|output| {
        calls
            .iter()
            .filter(|call| tool_call_matches_output(call, output))
            .count()
            == 1
    });
    (calls_match_once && outputs_match_once).then_some(&history[transaction_start..])
}

fn is_tool_output(item: &ResponseItem) -> bool {
    matches!(
        item,
        ResponseItem::FunctionCallOutput { .. }
            | ResponseItem::CustomToolCallOutput { .. }
            | ResponseItem::ToolSearchOutput {
                call_id: Some(_),
                ..
            }
    )
}

fn is_tool_call(item: &ResponseItem) -> bool {
    matches!(
        item,
        ResponseItem::FunctionCall { .. }
            | ResponseItem::LocalShellCall {
                call_id: Some(_),
                ..
            }
            | ResponseItem::CustomToolCall { .. }
            | ResponseItem::ToolSearchCall {
                call_id: Some(_),
                ..
            }
    )
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
