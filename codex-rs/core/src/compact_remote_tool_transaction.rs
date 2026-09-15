use crate::context_manager::ContextManager;
use codex_history::ResponseItemEnvelope;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use codex_utils_audio::estimate_audio_token_count;
use codex_utils_output_truncation::TruncationPolicy;
use codex_utils_output_truncation::approx_token_count;
use codex_utils_output_truncation::truncate_function_output_payload;

const CONTEXT_WINDOW_ERROR: &str = "remote compaction did not install because the latest complete tool transaction does not fit the model context window";
const BASELINE_CONTEXT_WINDOW_ERROR: &str = "remote compaction did not install because the compacted replacement history does not fit the model context window";
const ITEM_LIMIT_ERROR: &str = "remote compaction did not install because the latest complete tool transaction contains an item above the 10000-token hard limit";
const MAX_REATTACHED_ITEM_TOKENS: i64 = 10_000;
const MAX_REATTACHED_OUTPUT_TOKENS: usize = 9_500;

/// Captures the complete call/output block at the end of the history.
///
/// A transaction may contain parallel calls followed by their corresponding outputs. The
/// envelopes come from the post-normalization prompt input, so output truncation and harness
/// metadata reflect what the compaction request actually consumed.
pub(super) fn latest_complete_tool_transaction(
    history: &[ResponseItemEnvelope],
) -> Option<Vec<ResponseItemEnvelope>> {
    let mut calls_end = history.len();
    while calls_end > 0 && is_tool_output(&history[calls_end - 1].item) {
        calls_end -= 1;
    }
    if calls_end == history.len() {
        return None;
    }

    let mut transaction_start = calls_end;
    while transaction_start > 0 && is_tool_call(&history[transaction_start - 1].item) {
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
            .filter(|output| tool_call_matches_output(&call.item, &output.item))
            .count()
            == 1
    });
    let outputs_match_once = outputs.iter().all(|output| {
        calls
            .iter()
            .filter(|call| tool_call_matches_output(&call.item, &output.item))
            .count()
            == 1
    });
    (calls_match_once && outputs_match_once).then(|| history[transaction_start..].to_vec())
}

/// Validates the complete compacted context and reattaches a captured terminal transaction.
pub(super) fn validate_and_reattach_terminal_tool_transaction(
    mut compacted_history: Vec<ResponseItemEnvelope>,
    source_transaction: Option<&[ResponseItemEnvelope]>,
    context_window: Option<i64>,
    base_instructions: &BaseInstructions,
) -> CodexResult<Vec<ResponseItemEnvelope>> {
    validate_context_window(
        &compacted_history,
        context_window,
        base_instructions,
        BASELINE_CONTEXT_WINDOW_ERROR,
    )?;

    let Some(source_transaction) = source_transaction else {
        return Ok(compacted_history);
    };
    let transaction = bounded_terminal_tool_transaction(source_transaction)?;

    let source_index = find_transaction(&compacted_history, source_transaction);
    let bounded_index = find_transaction(&compacted_history, &transaction);
    match (source_index, bounded_index) {
        (Some(source_index), Some(mut bounded_index)) if source_index != bounded_index => {
            drop(compacted_history.drain(source_index..source_index + source_transaction.len()));
            if source_index < bounded_index {
                bounded_index -= source_transaction.len();
            }
            compacted_history.splice(
                bounded_index..bounded_index + transaction.len(),
                transaction.iter().cloned(),
            );
        }
        (Some(source_index), _) => {
            compacted_history.splice(
                source_index..source_index + source_transaction.len(),
                transaction.iter().cloned(),
            );
        }
        (None, Some(bounded_index)) => {
            compacted_history.splice(
                bounded_index..bounded_index + transaction.len(),
                transaction.iter().cloned(),
            );
        }
        (None, None) => {
            let insertion_index = compacted_history
                .iter()
                .position(|envelope| {
                    matches!(
                        &envelope.item,
                        ResponseItem::Compaction { .. } | ResponseItem::ContextCompaction { .. }
                    )
                })
                .unwrap_or(compacted_history.len());
            compacted_history.splice(
                insertion_index..insertion_index,
                transaction.iter().cloned(),
            );
        }
    }

    validate_context_window(
        &compacted_history,
        context_window,
        base_instructions,
        CONTEXT_WINDOW_ERROR,
    )?;
    Ok(compacted_history)
}

fn validate_context_window(
    history: &[ResponseItemEnvelope],
    context_window: Option<i64>,
    base_instructions: &BaseInstructions,
    error_message: &str,
) -> CodexResult<()> {
    if let Some(context_window) = context_window
        && let Some(estimated_tokens) = estimated_history_tokens(history, base_instructions)
        && estimated_tokens > context_window
    {
        return Err(CodexErr::InvalidRequest(format!(
            "{error_message} (estimated {estimated_tokens} tokens, limit {context_window})"
        )));
    }
    Ok(())
}

fn find_transaction(
    history: &[ResponseItemEnvelope],
    transaction: &[ResponseItemEnvelope],
) -> Option<usize> {
    history.windows(transaction.len()).position(|items| {
        items
            .iter()
            .zip(transaction)
            .all(|(item, expected)| item.item == expected.item)
    })
}

fn estimated_history_tokens(
    history: &[ResponseItemEnvelope],
    base_instructions: &BaseInstructions,
) -> Option<i64> {
    let mut context = ContextManager::new();
    context.replace_annotated(history.to_vec());
    context.estimate_token_count_with_base_instructions(base_instructions)
}

fn bounded_terminal_tool_transaction(
    transaction: &[ResponseItemEnvelope],
) -> CodexResult<Vec<ResponseItemEnvelope>> {
    let mut transaction = transaction.to_vec();
    for envelope in &mut transaction {
        if matches!(
            &envelope.item,
            ResponseItem::FunctionCallOutput { .. } | ResponseItem::CustomToolCallOutput { .. }
        ) {
            bound_function_output_item(&mut envelope.item)?;
        }

        let estimated_tokens = estimate_item_tokens(&envelope.item);
        if estimated_tokens > MAX_REATTACHED_ITEM_TOKENS {
            return Err(CodexErr::InvalidRequest(format!(
                "{ITEM_LIMIT_ERROR} (estimated {estimated_tokens} tokens)"
            )));
        }
    }
    Ok(transaction)
}

fn bound_function_output_item(item: &mut ResponseItem) -> CodexResult<()> {
    let original_output = match item {
        ResponseItem::FunctionCallOutput { output, .. }
        | ResponseItem::CustomToolCallOutput { output, .. } => output.clone(),
        _ => return Ok(()),
    };
    let mut output_tokens =
        function_output_text_token_count(&original_output).min(MAX_REATTACHED_OUTPUT_TOKENS);

    loop {
        match item {
            ResponseItem::FunctionCallOutput { output, .. }
            | ResponseItem::CustomToolCallOutput { output, .. } => {
                *output = original_output.clone();
                truncate_function_output_payload(
                    output,
                    TruncationPolicy::Tokens(output_tokens),
                    estimate_audio_token_count,
                );
            }
            _ => unreachable!("response item kind was checked before bounding output"),
        }
        let estimated_tokens = estimate_item_tokens(item);
        if estimated_tokens <= MAX_REATTACHED_ITEM_TOKENS {
            return Ok(());
        }
        if output_tokens == 0 {
            return Err(CodexErr::InvalidRequest(format!(
                "{ITEM_LIMIT_ERROR} (estimated {estimated_tokens} tokens)"
            )));
        }

        let excess_tokens =
            usize::try_from(estimated_tokens.saturating_sub(MAX_REATTACHED_ITEM_TOKENS))
                .unwrap_or(usize::MAX)
                .max(1);
        output_tokens = output_tokens.saturating_sub(excess_tokens);
    }
}

fn function_output_text_token_count(output: &FunctionCallOutputPayload) -> usize {
    match &output.body {
        FunctionCallOutputBody::Text(text) => approx_token_count(text),
        FunctionCallOutputBody::ContentItems(items) => items
            .iter()
            .filter_map(|item| match item {
                FunctionCallOutputContentItem::InputText { text } => Some(approx_token_count(text)),
                FunctionCallOutputContentItem::InputImage { .. }
                | FunctionCallOutputContentItem::InputAudio { .. }
                | FunctionCallOutputContentItem::EncryptedContent { .. } => None,
            })
            .fold(0usize, usize::saturating_add),
    }
}

fn estimate_item_tokens(item: &ResponseItem) -> i64 {
    let mut item_context = ContextManager::new();
    item_context.replace_annotated(vec![ResponseItemEnvelope::new(item.clone())]);
    item_context
        .estimate_token_count_with_base_instructions(&BaseInstructions {
            text: String::new(),
            provenance: None,
        })
        .unwrap_or(i64::MAX)
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
                call_id: Some(call_id_b),
                ..
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
