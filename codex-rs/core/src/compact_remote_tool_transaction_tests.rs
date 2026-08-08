use super::*;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::LocalShellAction;
use codex_protocol::models::LocalShellExecAction;
use codex_protocol::models::LocalShellStatus;
use codex_utils_output_truncation::truncate_text;
use pretty_assertions::assert_eq;

fn message(role: &str, text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: role.to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn function_call(call_id: &str, name: &str) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: name.to_string(),
        namespace: None,
        arguments: "{}".to_string(),
        encrypted_function_args: None,
        call_id: call_id.to_string(),
        internal_chat_message_metadata_passthrough: None,
    }
}

fn function_output(call_id: &str, output: String) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        id: None,
        call_id: call_id.to_string(),
        output: FunctionCallOutputPayload::from_text(output),
        internal_chat_message_metadata_passthrough: None,
    }
}

fn custom_tool_transaction(call_id: &str) -> (ResponseItem, ResponseItem) {
    (
        ResponseItem::CustomToolCall {
            id: None,
            status: Some("completed".to_string()),
            call_id: call_id.to_string(),
            name: "js_repl".to_string(),
            namespace: None,
            input: "return state".to_string(),
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::CustomToolCallOutput {
            id: None,
            call_id: call_id.to_string(),
            name: Some("js_repl".to_string()),
            output: FunctionCallOutputPayload::from_text("custom state".to_string()),
            internal_chat_message_metadata_passthrough: None,
        },
    )
}

fn tool_search_transaction(call_id: &str) -> (ResponseItem, ResponseItem) {
    (
        ResponseItem::ToolSearchCall {
            id: None,
            call_id: Some(call_id.to_string()),
            status: Some("completed".to_string()),
            execution: "client".to_string(),
            arguments: serde_json::json!({"query": "state tool"}),
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::ToolSearchOutput {
            id: None,
            call_id: Some(call_id.to_string()),
            status: "completed".to_string(),
            execution: "client".to_string(),
            tools: vec![serde_json::json!({"name": "state_tool"})],
            internal_chat_message_metadata_passthrough: None,
        },
    )
}

fn local_shell_transaction(call_id: &str) -> (ResponseItem, ResponseItem) {
    (
        ResponseItem::LocalShellCall {
            id: None,
            call_id: Some(call_id.to_string()),
            status: LocalShellStatus::Completed,
            action: LocalShellAction::Exec(LocalShellExecAction {
                command: vec!["printf".to_string(), "state".to_string()],
                timeout_ms: None,
                working_directory: None,
                env: None,
                user: None,
            }),
            internal_chat_message_metadata_passthrough: None,
        },
        function_output(call_id, "shell state".to_string()),
    )
}

fn compaction() -> ResponseItem {
    ResponseItem::Compaction {
        id: None,
        encrypted_content: "opaque-compaction".to_string(),
        internal_chat_message_metadata_passthrough: None,
    }
}

fn empty_instructions() -> BaseInstructions {
    BaseInstructions {
        text: String::new(),
        provenance: None,
    }
}

#[test]
fn reattaches_observed_large_terminal_transaction_with_hard_item_cap() {
    let old_call = function_call("17", "old_wait");
    let old_output = function_output("17", "old output".to_string());
    let call = function_call("17", "wait");
    // Mirrors the 50,494-character output in the observed failed compaction. Preserve the
    // transaction while bounding its output below the model-context per-item limit.
    let output_text = "x".repeat(50_494);
    let output = function_output("17", output_text.clone());
    let bounded_output = function_output(
        "17",
        truncate_text(
            &output_text,
            TruncationPolicy::Tokens(MAX_REATTACHED_OUTPUT_TOKENS),
        ),
    );
    let trace_input_history = vec![
        message("user", "original request"),
        old_call,
        old_output,
        call.clone(),
        output.clone(),
    ];
    let injected_context = [
        message("developer", "fresh permissions"),
        message("user", "fresh environment"),
    ];
    let compacted_item = compaction();
    let compacted_history = vec![
        injected_context[0].clone(),
        injected_context[1].clone(),
        compacted_item.clone(),
    ];
    let expected = vec![
        injected_context[0].clone(),
        injected_context[1].clone(),
        call,
        bounded_output.clone(),
        compacted_item,
    ];

    let result = reattach_latest_complete_tool_transaction(
        compacted_history,
        &trace_input_history,
        Some(100_000),
        &empty_instructions(),
    )
    .expect("bounded complete transaction should fit");

    assert_eq!(result, expected);
    assert_ne!(bounded_output, output);
}

#[test]
fn shrinks_terminal_output_for_its_serialized_response_item_envelope() {
    // A long, but still valid, call id makes the old 9,500-token body budget serialize as a
    // 10,015-token response item. The reattached output must give up body tokens, not drop its
    // matching call/output transaction.
    let call_id = format!("call-{}", "x".repeat(1_969));
    let call = function_call(&call_id, "exec");
    let output_text = "x".repeat(50_494);
    let output = function_output(&call_id, output_text.clone());
    let old_bounded_output = function_output(
        &call_id,
        truncate_text(
            &output_text,
            TruncationPolicy::Tokens(MAX_REATTACHED_OUTPUT_TOKENS),
        ),
    );
    assert_eq!(estimate_item_tokens(&old_bounded_output), 10_015);

    let result = reattach_latest_complete_tool_transaction(
        vec![compaction()],
        &[call.clone(), output],
        Some(100_000),
        &empty_instructions(),
    )
    .expect("trimmable output should fit its complete response-item envelope");

    assert_eq!(result.len(), 3);
    assert_eq!(result[0], call);
    assert_ne!(result[1], old_bounded_output);
    assert_eq!(estimate_item_tokens(&result[1]), MAX_REATTACHED_ITEM_TOKENS);
    assert!(matches!(result[1], ResponseItem::FunctionCallOutput { .. }));
}

#[test]
fn removes_unbounded_source_when_bounded_transaction_is_already_present() {
    let call = function_call("17", "wait");
    let output_text = "x".repeat(50_494);
    let output = function_output("17", output_text.clone());
    let bounded_output = function_output(
        "17",
        truncate_text(
            &output_text,
            TruncationPolicy::Tokens(MAX_REATTACHED_OUTPUT_TOKENS),
        ),
    );
    let compacted_item = compaction();

    let result = reattach_latest_complete_tool_transaction(
        vec![
            call.clone(),
            output.clone(),
            call.clone(),
            bounded_output.clone(),
            compacted_item.clone(),
        ],
        &[call.clone(), output],
        Some(100_000),
        &empty_instructions(),
    )
    .expect("duplicate source form should be removed");

    assert_eq!(result, vec![call, bounded_output, compacted_item]);
}

#[test]
fn reattaches_each_supported_terminal_transaction_with_deep_equality() {
    let cases = vec![
        custom_tool_transaction("custom-1"),
        tool_search_transaction("search-1"),
        local_shell_transaction("shell-1"),
    ];

    for (call, output) in cases {
        let context = message("developer", "fresh context");
        let compacted_item = compaction();
        let expected = vec![
            context.clone(),
            call.clone(),
            output.clone(),
            compacted_item.clone(),
        ];
        let result = reattach_latest_complete_tool_transaction(
            vec![context, compacted_item],
            &[call, output],
            Some(100_000),
            &empty_instructions(),
        )
        .expect("supported exact transaction should fit");

        assert_eq!(result, expected);
    }
}

#[test]
fn reattaches_complete_terminal_parallel_transaction_in_original_order() {
    let first_call = function_call("parallel-1", "read_first");
    let second_call = function_call("parallel-2", "read_second");
    let first_output = function_output("parallel-1", "first state".to_string());
    let second_output = function_output("parallel-2", "second state".to_string());
    let transaction = vec![first_call, second_call, first_output, second_output];
    let context = message("developer", "fresh context");
    let compacted_item = compaction();
    let mut expected = vec![context.clone()];
    expected.extend(transaction.clone());
    expected.push(compacted_item.clone());

    let result = reattach_latest_complete_tool_transaction(
        vec![context, compacted_item],
        &transaction,
        Some(100_000),
        &empty_instructions(),
    )
    .expect("parallel transaction should fit");

    assert_eq!(result, expected);
}

#[test]
fn rejects_terminal_parallel_block_with_duplicate_or_missing_match() {
    let compacted_history = vec![message("developer", "fresh"), compaction()];
    let unmatched = vec![
        function_call("parallel-1", "read_first"),
        function_call("parallel-2", "read_second"),
        function_output("parallel-1", "first state".to_string()),
        function_output("parallel-3", "third state".to_string()),
    ];
    let duplicate = vec![
        function_call("parallel-1", "read_first"),
        function_call("parallel-1", "read_duplicate"),
        function_output("parallel-1", "first state".to_string()),
        function_output("parallel-1", "duplicate state".to_string()),
    ];

    for trace_input_history in [unmatched, duplicate] {
        let result = reattach_latest_complete_tool_transaction(
            compacted_history.clone(),
            &trace_input_history,
            Some(100_000),
            &empty_instructions(),
        )
        .expect("invalid terminal block should be ignored");

        assert_eq!(result, compacted_history);
    }
}

#[test]
fn exact_pair_already_present_is_not_duplicated() {
    let call = function_call("call-1", "wait");
    let output = function_output("call-1", "state".to_string());
    let compacted_history = vec![
        message("developer", "fresh context"),
        call.clone(),
        output.clone(),
        compaction(),
    ];

    let result = reattach_latest_complete_tool_transaction(
        compacted_history.clone(),
        &[call, output],
        Some(100_000),
        &empty_instructions(),
    )
    .expect("already-present exact transaction should fit");

    assert_eq!(result, compacted_history);
}

#[test]
fn reattaches_immediately_before_the_first_compaction_item() {
    let call = function_call("call-1", "wait");
    let output = function_output("call-1", "state".to_string());
    let first_compaction = compaction();
    let second_compaction = ResponseItem::ContextCompaction {
        id: None,
        encrypted_content: Some("second-opaque-compaction".to_string()),
        internal_chat_message_metadata_passthrough: None,
    };
    let expected = vec![
        message("developer", "fresh context"),
        call.clone(),
        output.clone(),
        first_compaction.clone(),
        second_compaction.clone(),
    ];

    let result = reattach_latest_complete_tool_transaction(
        vec![
            message("developer", "fresh context"),
            first_compaction,
            second_compaction,
        ],
        &[call, output],
        Some(100_000),
        &empty_instructions(),
    )
    .expect("exact transaction should precede every compaction item");

    assert_eq!(result, expected);
}

#[test]
fn does_not_move_a_nonterminal_or_unmatched_transaction_across_compaction() {
    let compacted_history = vec![message("developer", "fresh"), compaction()];
    let completed_then_final = vec![
        function_call("call-1", "wait"),
        function_output("call-1", "state".to_string()),
        message("assistant", "done"),
    ];
    let unmatched_terminal_output = vec![
        function_call("call-1", "wait"),
        function_output("call-2", "state".to_string()),
    ];
    let nonadjacent_terminal_output = vec![
        function_call("call-1", "wait"),
        message("assistant", "intervening state"),
        function_output("call-1", "state".to_string()),
    ];

    for trace_input_history in [
        completed_then_final,
        unmatched_terminal_output,
        nonadjacent_terminal_output,
    ] {
        let result = reattach_latest_complete_tool_transaction(
            compacted_history.clone(),
            &trace_input_history,
            Some(100_000),
            &empty_instructions(),
        )
        .expect("history without a terminal complete transaction should be unchanged");
        assert_eq!(result, compacted_history);
    }
}

#[test]
fn fails_closed_when_compacted_baseline_exceeds_context_window() {
    let compacted_history = vec![message("developer", "fresh"), compaction()];
    let estimated_tokens = estimated_history_tokens(&compacted_history, &empty_instructions())
        .expect("baseline token estimate");
    let context_window = estimated_tokens.saturating_sub(1);

    let error = reattach_latest_complete_tool_transaction(
        compacted_history,
        &[],
        Some(context_window),
        &empty_instructions(),
    )
    .expect_err("an oversized compacted baseline must not be installed");

    let CodexErrorDetails::InvalidRequest(message) = error.details() else {
        panic!("expected explicit invalid-request reason, got {error}");
    };
    assert_eq!(
        message.as_str(),
        format!(
            "{BASELINE_CONTEXT_WINDOW_ERROR} (estimated {estimated_tokens} tokens, limit {context_window})"
        )
    );
}

#[test]
fn fails_closed_when_reattached_transaction_crosses_context_window() {
    let call = function_call("17", "wait");
    let output = function_output("17", "x".repeat(4096));
    let trace_input_history = vec![call, output];
    let compacted_history = vec![message("developer", "fresh"), compaction()];
    let context_window = estimated_history_tokens(&compacted_history, &empty_instructions())
        .expect("baseline token estimate");

    let error = reattach_latest_complete_tool_transaction(
        compacted_history,
        &trace_input_history,
        Some(context_window),
        &empty_instructions(),
    )
    .expect_err("an exact pair that cannot fit must not be installed lossily");

    let CodexErrorDetails::InvalidRequest(message) = error.details() else {
        panic!("expected explicit invalid-request reason, got {error}");
    };
    assert!(message.starts_with(CONTEXT_WINDOW_ERROR));
    assert!(message.contains("estimated "));
    assert!(message.ends_with(format!("tokens, limit {context_window})").as_str()));
}

#[test]
fn fails_closed_when_an_untruncatable_transaction_item_exceeds_hard_cap() {
    let mut call = function_call("17", "wait");
    let ResponseItem::FunctionCall { arguments, .. } = &mut call else {
        panic!("function_call helper must return a function call");
    };
    *arguments = "x".repeat(50_000);
    let output = function_output("17", "complete".to_string());

    let error = reattach_latest_complete_tool_transaction(
        vec![compaction()],
        &[call, output],
        Some(100_000),
        &empty_instructions(),
    )
    .expect_err("an oversized call item must not be installed");

    let CodexErrorDetails::InvalidRequest(message) = error.details() else {
        panic!("expected explicit invalid-request reason, got {error}");
    };
    assert!(message.starts_with(ITEM_LIMIT_ERROR));
    assert!(message.ends_with("tokens)"));
}
