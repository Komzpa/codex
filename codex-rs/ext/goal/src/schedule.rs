use chrono::DateTime;
use chrono::SecondsFormat;
use chrono::Utc;
use codex_protocol::goal::GoalQuotaSnapshot;
use codex_protocol::goal_execution::GoalExecutionContext;
use codex_protocol::goal_execution::GoalStageExecutionContext;
use codex_protocol::protocol::ThreadGoal;

// A byte cap also bounds byte-level tokenizer output, including arbitrary Unicode.
const REMINDER_BYTES: usize = 800;
const NUDGE: &str = "Deliver usable work; reserve review time; delegate bounded tasks cheaper when authorized. Adjust depth to time/quota. Overdue: show artifact or say none, then continue.\n";

pub fn local_goal_timezone() -> String {
    iana_time_zone::get_timezone()
        .unwrap_or_else(|_| chrono::Local::now().format("%:z").to_string())
}

pub fn summarize_goal_execution(goal: &ThreadGoal, now_at: i64) -> GoalExecutionContext {
    let delivered = goal
        .stages
        .iter()
        .filter(|stage| stage.delivered_at.is_some())
        .max_by_key(|stage| stage.delivered_at);
    let final_deadline_at = goal.stages.iter().map(|stage| stage.deadline_at).max();
    let current_stage = goal
        .stages
        .iter()
        .filter(|stage| stage.delivered_at.is_none())
        .min_by_key(|stage| stage.deadline_at)
        .map(|stage| GoalStageExecutionContext {
            id: bounded(&stage.id, 64),
            label: bounded(&stage.label, 120),
            expected_result: bounded(&stage.expected_result, 240),
            deadline_at: stage.deadline_at,
            remaining_seconds: stage.deadline_at.saturating_sub(now_at),
            overdue_seconds: now_at.saturating_sub(stage.deadline_at).max(0),
            remaining_percent: remaining_percent(goal.created_at, stage.deadline_at, now_at),
            delivered_at: stage.delivered_at,
            delivered_artifact: stage
                .delivered_artifact
                .as_deref()
                .map(|text| bounded(text, 240)),
        });
    GoalExecutionContext {
        objective: bounded(&goal.objective, 600),
        now_at,
        created_at: goal.created_at,
        timezone: goal.timezone.as_deref().map(|text| bounded(text, 80)),
        final_deadline_at,
        final_deadline_remaining_seconds: final_deadline_at.map(|due| due.saturating_sub(now_at)),
        final_deadline_overdue_seconds: final_deadline_at
            .map(|due| now_at.saturating_sub(due).max(0)),
        final_deadline_remaining_percent: final_deadline_at
            .and_then(|due| remaining_percent(goal.created_at, due, now_at)),
        current_stage,
        overdue_stage_count: goal
            .stages
            .iter()
            .filter(|stage| stage.delivered_at.is_none() && stage.deadline_at < now_at)
            .count(),
        latest_delivered_artifact: delivered
            .and_then(|stage| stage.delivered_artifact.as_deref())
            .map(|text| bounded(text, 240)),
        latest_delivered_at: delivered.and_then(|stage| stage.delivered_at),
        initial_token_budget: goal.initial_token_budget,
        current_token_budget: goal.token_budget,
        tokens_used: goal.tokens_used,
        time_used_seconds: goal.time_used_seconds,
        initial_quota_snapshots: goal
            .initial_quota_snapshots
            .iter()
            .take(2)
            .cloned()
            .collect(),
        current_quota_snapshots: Vec::new(),
    }
}

pub fn model_reminder(context: &GoalExecutionContext) -> String {
    let mut text = format!(
        "{NUDGE}Goal start={} now={} tz={}.\n",
        timestamp(context.created_at),
        timestamp(context.now_at),
        bounded(context.timezone.as_deref().unwrap_or("UTC"), 24)
    );
    if let Some(stage) = &context.current_stage {
        text.push_str(&format!(
            "Next {:?}: {}; ",
            bounded(&stage.label, 24),
            deadline(
                stage.deadline_at,
                stage.remaining_seconds,
                stage.remaining_percent
            )
        ));
    }
    if let Some(due) = context.final_deadline_at {
        text.push_str(&format!(
            "Final {}; ",
            deadline(
                due,
                context.final_deadline_remaining_seconds.unwrap_or(0),
                context.final_deadline_remaining_percent
            )
        ));
    }
    text.push_str(&format!(
        "overdue={}.\nTokens={} budget start={} now={}. ",
        context.overdue_stage_count,
        context.tokens_used,
        amount(context.initial_token_budget),
        amount(context.current_token_budget)
    ));
    text.push_str("Quota shared, not reserved; %left start>now; reset/seen=Unix UTC.\n");
    if context.current_quota_snapshots.is_empty() {
        text.push_str("Quota now=unknown.\n");
    }
    for snapshot in context.current_quota_snapshots.iter().take(2) {
        let initial = context.initial_quota_snapshots.iter().find(|old| {
            old.source == snapshot.source
                && old.scope_id == snapshot.scope_id
                && old.scope_id != "unknown"
        });
        let source = match snapshot.source.as_str() {
            "provider-key" => "key",
            "provider-pool" => "pool",
            "chatgpt-account" => "account",
            other => other,
        };
        text.push_str(&format!(
            "{}/{} {} seen={}>{}.\n",
            bounded(source, 12),
            bounded(&snapshot.scope_id, 10),
            quota_windows(snapshot, initial),
            initial
                .map(|old| old.captured_at.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            snapshot.captured_at
        ));
    }
    text.push_str(&format!(
        "Result={}; next output={}.\n",
        bounded(
            context
                .latest_delivered_artifact
                .as_deref()
                .unwrap_or("none recorded"),
            64
        ),
        bounded(
            context
                .current_stage
                .as_ref()
                .map_or("see get_goal", |stage| stage.expected_result.as_str()),
            48
        )
    ));
    if text.len() > REMINDER_BYTES {
        let suffix = "\n[More facts: get_goal.]";
        text = bounded(&text, REMINDER_BYTES - suffix.len());
        text.push_str(suffix);
    }
    text
}

fn quota_windows(snapshot: &GoalQuotaSnapshot, initial: Option<&GoalQuotaSnapshot>) -> String {
    let Some(limit) = snapshot.limits.first() else {
        return "unknown".to_string();
    };
    let initial_limit =
        initial.and_then(|old| old.limits.iter().find(|old| old.limit_id == limit.limit_id));
    let windows = [
        (
            limit.primary.as_ref(),
            initial_limit.and_then(|old| old.primary.as_ref()),
        ),
        (
            limit.secondary.as_ref(),
            initial_limit.and_then(|old| old.secondary.as_ref()),
        ),
    ]
    .into_iter()
    .filter_map(|(current, old)| current.map(|current| (current, old)))
    .map(|(window, old)| {
        format!(
            "{}m:{}>{:.0} reset={}",
            window
                .window_minutes
                .map(|minutes| minutes.to_string())
                .unwrap_or_else(|| "?".to_string()),
            old.filter(|old| old.window_minutes == window.window_minutes)
                .map(|old| format!("{:.0}", (100.0 - old.used_percent).clamp(0.0, 100.0)))
                .unwrap_or_else(|| "unknown".to_string()),
            (100.0 - window.used_percent).clamp(0.0, 100.0),
            window
                .resets_at
                .map(|at| at.to_string())
                .unwrap_or_else(|| "?".to_string())
        )
    })
    .collect::<Vec<_>>();
    if windows.is_empty() {
        "unknown".to_string()
    } else {
        windows.join(" ")
    }
}

fn deadline(due: i64, remaining: i64, percent: Option<i64>) -> String {
    format!(
        "{} {} {} ({}%)",
        timestamp(due),
        if remaining < 0 { "overdue" } else { "left" },
        duration(remaining.saturating_abs()),
        percent
            .map(|value| value.to_string())
            .unwrap_or_else(|| "n/a".to_string())
    )
}

fn timestamp(value: i64) -> String {
    DateTime::<Utc>::from_timestamp(value, 0)
        .map(|at| at.to_rfc3339_opts(SecondsFormat::Secs, true))
        .unwrap_or_else(|| format!("{value}sUTC"))
}

fn duration(seconds: i64) -> String {
    format!("{}h{:02}m", seconds / 3600, (seconds / 60) % 60)
}

fn amount(value: Option<i64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "none".to_string())
}

fn remaining_percent(start: i64, due: i64, now: i64) -> Option<i64> {
    if now >= due {
        return Some(0);
    }
    let total = i128::from(due) - i128::from(start);
    (total > 0).then(|| (((i128::from(due) - i128::from(now)) * 100 / total).clamp(0, 100)) as i64)
}

fn bounded(value: &str, bytes: usize) -> String {
    let mut end = value.len().min(bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}
