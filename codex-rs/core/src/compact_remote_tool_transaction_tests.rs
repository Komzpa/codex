use super::*;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::LocalShellAction;
use codex_protocol::models::LocalShellExecAction;
use codex_protocol::models::LocalShellStatus;
use pretty_assertions::assert_eq;

fn envelope(item: ResponseItem) -> ResponseItemEnvelope {
    ResponseItemEnvelope::new(item)
}

fn annotated(item: ResponseItem) -> ResponseItemEnvelope {
    ResponseItemEnvelope {
        item,
        metadata: Some(codex_history::CodexHarnessMetadata {
            client_authored: true,
            ..Default::default()
        }),
    }
}

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

fn function_output(call_id: &str, output: impl Into<String>) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        id: None,
        call_id: Some(call_id.to_string()),
        name: None,
        namespace: None,
        output: FunctionCallOutputPayload::from_text(output.into()),
        internal_chat_message_metadata_passthrough: None,
    }
}

fn compaction() -> ResponseItemEnvelope {
    envelope(ResponseItem::Compaction {
        id: None,
        encrypted_content: "opaque-compaction".to_string(),
        internal_chat_message_metadata_passthrough: None,
    })
}

fn empty_instructions() -> BaseInstructions {
    BaseInstructions {
        text: String::new(),
        provenance: None,
    }
}

fn reattach(
    compacted_history: Vec<ResponseItemEnvelope>,
    source_history: &[ResponseItemEnvelope],
    context_window: Option<i64>,
) -> CodexResult<Vec<ResponseItemEnvelope>> {
    let transaction = latest_complete_tool_transaction(source_history);
    validate_and_reattach_terminal_tool_transaction(
        compacted_history,
        transaction.as_deref(),
        context_window,
        &empty_instructions(),
    )
}

#[test]
fn reattaches_parallel_transaction_in_order_with_source_metadata() {
    let transaction = vec![
        annotated(function_call("parallel-1", "read_first")),
        envelope(function_call("parallel-2", "read_second")),
        annotated(function_output("parallel-1", "first state")),
        envelope(function_output("parallel-2", "second state")),
    ];
    let context = envelope(message("developer", "fresh context"));
    let compacted_item = compaction();
    let mut expected = vec![context.clone()];
    expected.extend(transaction.clone());
    expected.push(compacted_item.clone());

    let result = reattach(vec![context, compacted_item], &transaction, Some(100_000))
        .expect("parallel transaction should fit");

    assert_eq!(result, expected);
}

#[test]
fn bounds_large_output_by_complete_serialized_item() {
    let call_id = format!("call-{}", "x".repeat(1_969));
    let call = annotated(function_call(&call_id, "exec"));
    let output = annotated(function_output(&call_id, "x".repeat(50_494)));

    let result = reattach(
        vec![compaction()],
        &[call.clone(), output.clone()],
        Some(100_000),
    )
    .expect("trimmable output should fit its response-item envelope");

    assert_eq!(result.len(), 3);
    assert_eq!(result[0], call);
    assert_eq!(result[1].metadata, output.metadata);
    assert_ne!(result[1].item, output.item);
    assert_eq!(
        estimate_item_tokens(&result[1].item),
        MAX_REATTACHED_ITEM_TOKENS
    );
}

#[test]
fn replaces_existing_transaction_without_duplication_and_restores_metadata() {
    let call = annotated(function_call("call-1", "wait"));
    let output = annotated(function_output("call-1", "state"));
    let compacted_history = vec![
        envelope(message("developer", "fresh context")),
        envelope(call.item.clone()),
        envelope(output.item.clone()),
        compaction(),
    ];

    let result = reattach(
        compacted_history,
        &[call.clone(), output.clone()],
        Some(100_000),
    )
    .expect("existing transaction should be reused");

    assert_eq!(
        result,
        vec![
            envelope(message("developer", "fresh context")),
            call,
            output,
            compaction(),
        ]
    );
}

#[test]
fn recognizes_supported_terminal_transaction_types() {
    let custom = vec![
        envelope(ResponseItem::CustomToolCall {
            id: None,
            status: Some("completed".to_string()),
            call_id: "custom-1".to_string(),
            name: "js_repl".to_string(),
            namespace: None,
            input: "return state".to_string(),
            internal_chat_message_metadata_passthrough: None,
        }),
        envelope(ResponseItem::CustomToolCallOutput {
            id: None,
            call_id: "custom-1".to_string(),
            name: Some("js_repl".to_string()),
            output: FunctionCallOutputPayload::from_text("custom state".to_string()),
            internal_chat_message_metadata_passthrough: None,
        }),
    ];
    let search = vec![
        envelope(ResponseItem::ToolSearchCall {
            id: None,
            call_id: Some("search-1".to_string()),
            status: Some("completed".to_string()),
            execution: "client".to_string(),
            arguments: serde_json::json!({"query": "state tool"}),
            internal_chat_message_metadata_passthrough: None,
        }),
        envelope(ResponseItem::ToolSearchOutput {
            id: None,
            call_id: Some("search-1".to_string()),
            status: "completed".to_string(),
            execution: "client".to_string(),
            tools: vec![serde_json::json!({"name": "state_tool"})],
            internal_chat_message_metadata_passthrough: None,
        }),
    ];
    let shell = vec![
        envelope(ResponseItem::LocalShellCall {
            id: None,
            call_id: Some("shell-1".to_string()),
            status: LocalShellStatus::Completed,
            action: LocalShellAction::Exec(LocalShellExecAction {
                command: vec!["printf".to_string(), "state".to_string()],
                timeout_ms: None,
                working_directory: None,
                env: None,
                user: None,
            }),
            internal_chat_message_metadata_passthrough: None,
        }),
        envelope(function_output("shell-1", "shell state")),
    ];

    for transaction in [custom, search, shell] {
        let result = reattach(vec![compaction()], &transaction, Some(100_000))
            .expect("supported transaction should fit");
        assert_eq!(&result[..transaction.len()], transaction.as_slice());
    }
}

#[test]
fn rejects_incomplete_or_ambiguous_terminal_blocks() {
    let unmatched = vec![
        envelope(function_call("parallel-1", "read_first")),
        envelope(function_call("parallel-2", "read_second")),
        envelope(function_output("parallel-1", "first state")),
        envelope(function_output("parallel-3", "third state")),
    ];
    let duplicate = vec![
        envelope(function_call("parallel-1", "read_first")),
        envelope(function_call("parallel-1", "read_duplicate")),
        envelope(function_output("parallel-1", "first state")),
        envelope(function_output("parallel-1", "duplicate state")),
    ];

    for history in [unmatched, duplicate] {
        assert_eq!(latest_complete_tool_transaction(&history), None);
    }
}

#[test]
fn fails_closed_when_compacted_baseline_exceeds_context_window() {
    let compacted_history = vec![envelope(message("developer", "fresh")), compaction()];
    let estimated_tokens = estimated_history_tokens(&compacted_history, &empty_instructions())
        .expect("baseline token estimate");

    let error = validate_and_reattach_terminal_tool_transaction(
        compacted_history,
        None,
        Some(estimated_tokens.saturating_sub(1)),
        &empty_instructions(),
    )
    .expect_err("oversized compacted baseline must not install");

    let CodexErrorDetails::InvalidRequest(message) = error.details() else {
        panic!("expected invalid-request reason, got {error}");
    };
    assert!(message.starts_with(BASELINE_CONTEXT_WINDOW_ERROR));
}

#[test]
fn fails_closed_when_transaction_crosses_context_window() {
    let compacted_history = vec![envelope(message("developer", "fresh")), compaction()];
    let context_window = estimated_history_tokens(&compacted_history, &empty_instructions())
        .expect("baseline token estimate");
    let source = vec![
        envelope(function_call("17", "wait")),
        envelope(function_output("17", "x".repeat(4_096))),
    ];

    let error = reattach(compacted_history, &source, Some(context_window))
        .expect_err("transaction that cannot fit must not install lossily");

    let CodexErrorDetails::InvalidRequest(message) = error.details() else {
        panic!("expected invalid-request reason, got {error}");
    };
    assert!(message.starts_with(CONTEXT_WINDOW_ERROR));
}

#[test]
fn fails_closed_when_untruncatable_call_exceeds_item_limit() {
    let mut call = function_call("17", "wait");
    let ResponseItem::FunctionCall { arguments, .. } = &mut call else {
        panic!("function_call helper must return a function call");
    };
    *arguments = "x".repeat(50_000);
    let source = vec![envelope(call), envelope(function_output("17", "complete"))];

    let error = reattach(vec![compaction()], &source, Some(100_000))
        .expect_err("oversized call item must not install");

    let CodexErrorDetails::InvalidRequest(message) = error.details() else {
        panic!("expected invalid-request reason, got {error}");
    };
    assert!(message.starts_with(ITEM_LIMIT_ERROR));
}
