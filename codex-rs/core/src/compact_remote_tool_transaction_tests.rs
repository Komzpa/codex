use super::*;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::LocalShellAction;
use codex_protocol::models::LocalShellExecAction;
use codex_protocol::models::LocalShellStatus;
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
    }
}

#[test]
fn reattaches_real_sized_terminal_function_transaction_exactly_before_compaction() {
    let old_call = function_call("17", "old_wait");
    let old_output = function_output("17", "old output".to_string());
    let call = function_call("17", "wait");
    // Mirrors the 50,494-character output in the observed failed compaction. At roughly 12K
    // heuristic tokens it must rely on the full-candidate context gate, never a new raw cap.
    let output = function_output("17", "x".repeat(50_494));
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
        output,
        compacted_item,
    ];

    let result = reattach_latest_complete_tool_transaction(
        compacted_history,
        &trace_input_history,
        Some(100_000),
        &empty_instructions(),
    )
    .expect("exact pair should fit");

    assert_eq!(result, expected);
    assert_eq!(
        serde_json::to_vec(&result[2..4]).expect("installed pair should serialize"),
        serde_json::to_vec(&trace_input_history[3..5]).expect("trace pair should serialize")
    );
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
fn fails_closed_when_exact_transaction_would_exceed_context_window() {
    let call = function_call("17", "wait");
    let output = function_output("17", "x".repeat(4096));
    let trace_input_history = vec![call, output];
    let compacted_history = vec![message("developer", "fresh"), compaction()];

    let error = reattach_latest_complete_tool_transaction(
        compacted_history,
        &trace_input_history,
        Some(0),
        &empty_instructions(),
    )
    .expect_err("an exact pair that cannot fit must not be installed lossily");

    let CodexErr::InvalidRequest(message) = error else {
        panic!("expected explicit invalid-request reason, got {error}");
    };
    assert!(message.starts_with(CONTEXT_WINDOW_ERROR));
    assert!(message.contains("estimated "));
    assert!(message.ends_with("tokens, limit 0)"));
}
