use super::*;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
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

fn function_call(call_id: &str, name: &str, arguments: &str) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: name.to_string(),
        namespace: None,
        arguments: arguments.to_string(),
        call_id: call_id.to_string(),
        internal_chat_message_metadata_passthrough: None,
    }
}

fn function_output(call_id: &str) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        id: None,
        call_id: call_id.to_string(),
        output: FunctionCallOutputPayload::from_text("Plan updated".to_string()),
        internal_chat_message_metadata_passthrough: None,
    }
}

#[test]
fn compact_install_preserves_latest_update_plan_pair_only() {
    let stale_call = function_call(
        "plan-stale",
        "update_plan",
        r#"{"plan":[{"step":"old","status":"in_progress"}]}"#,
    );
    let stale_output = function_output("plan-stale");
    let latest_call = function_call(
        "plan-latest",
        "update_plan",
        r#"{"plan":[{"step":"done 1","status":"completed"},{"step":"done 2","status":"completed"},{"step":"done 3","status":"completed"},{"step":"return to grill-me","status":"in_progress"}]}"#,
    );
    let latest_output = function_output("plan-latest");
    let stale_catalog = message(
        "user",
        "<skills_instructions>grill-me catalog</skills_instructions>",
    );
    let prose_checklist = message("assistant", "TODO review: [ ] prose checklist");
    let summary = message("assistant", "summary");
    let mut compacted_history = vec![
        stale_catalog,
        stale_call,
        stale_output,
        prose_checklist.clone(),
        latest_call.clone(),
        latest_output.clone(),
        summary.clone(),
    ];

    retain_compacted_history_items(&mut compacted_history);

    assert_eq!(
        compacted_history,
        vec![prose_checklist, latest_call, latest_output, summary]
    );
}

#[test]
fn compact_install_drops_unpaired_plan_and_non_state_function_calls() {
    let unpaired_plan = function_call(
        "plan-missing-output",
        "update_plan",
        r#"{"plan":[{"step":"lost","status":"in_progress"}]}"#,
    );
    let ordinary_call = function_call("ordinary", "wait_agent", "{}");
    let ordinary_output = function_output("ordinary");
    let summary = message("assistant", "summary");
    let mut compacted_history = vec![
        unpaired_plan,
        ordinary_call,
        ordinary_output,
        summary.clone(),
    ];

    retain_compacted_history_items(&mut compacted_history);

    assert_eq!(compacted_history, vec![summary]);
}

#[test]
fn compact_install_preserves_active_skill_context() {
    let active_grill_me = message(
        "user",
        "<skill>\n<name>grill-me</name>\n<path>/home/kom/.codex/skills/grill-me/SKILL.md</path>\nActive objective: review every TODO row.\n</skill>",
    );
    let catalog = message(
        "user",
        "<skills_instructions>\n- grill-me: Interview the user one question at a time.\n</skills_instructions>",
    );
    let summary = message("assistant", "summary");
    let mut compacted_history = vec![active_grill_me.clone(), catalog, summary.clone()];

    retain_compacted_history_items(&mut compacted_history);

    assert_eq!(compacted_history, vec![active_grill_me, summary]);
}
