use crate::status::format_tokens_compact;
use anyhow::Result;
use codex_app_server_protocol::ThreadGoal;
use codex_app_server_protocol::ThreadGoalStatus;
use codex_protocol::goal::ThreadGoalStage;

pub(crate) const GOAL_USAGE: &str = "Usage: /goal [<objective>|clear|edit|pause|resume]";

const SCHEDULE_MARKER: &str = "\n--- schedule ---\n";

pub(crate) fn format_goal_editor_text(
    objective: &str,
    timezone: Option<&str>,
    stages: &[ThreadGoalStage],
) -> String {
    let mut text = format!("{objective}{SCHEDULE_MARKER}");
    let timezone = timezone
        .map(ToOwned::to_owned)
        .unwrap_or_else(codex_goal_extension::local_goal_timezone);
    text.push_str(&format!("timezone: {timezone}\n"));
    for stage in stages {
        text.push_str(&format!(
            "stage: {} | {} | {} | {}\n",
            stage.id,
            stage.label,
            stage.expected_result,
            chrono::DateTime::<chrono::Utc>::from_timestamp(stage.deadline_at, 0)
                .map(|timestamp| timestamp.to_rfc3339())
                .unwrap_or_else(|| stage.deadline_at.to_string())
        ));
    }
    text
}

pub(crate) fn parse_goal_editor_text(
    text: &str,
    fallback_timezone: Option<String>,
    fallback_stages: Vec<ThreadGoalStage>,
) -> Result<(String, Option<String>, Vec<ThreadGoalStage>)> {
    let Some((objective, schedule)) = text.split_once(SCHEDULE_MARKER) else {
        return Ok((text.trim().to_string(), fallback_timezone, fallback_stages));
    };
    let mut timezone = None;
    let mut stages = Vec::new();
    for line in schedule
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if let Some(value) = line.strip_prefix("timezone:") {
            let value = value.trim();
            if value.is_empty() {
                anyhow::bail!("Goal timezone must not be empty.");
            }
            timezone = Some(value.to_string());
            continue;
        }
        let Some(value) = line.strip_prefix("stage:") else {
            anyhow::bail!("Unknown schedule line: {line}");
        };
        let fields = value.split('|').map(str::trim).collect::<Vec<_>>();
        if fields.len() != 4 || fields.iter().any(|field| field.is_empty()) {
            anyhow::bail!(
                "Each stage must use: stage: id | label | expected result | RFC3339 deadline"
            );
        }
        if fields[0].chars().any(char::is_whitespace) {
            anyhow::bail!("Stage ids must not contain whitespace: {}", fields[0]);
        }
        if stages
            .iter()
            .any(|stage: &ThreadGoalStage| stage.id == fields[0])
        {
            anyhow::bail!("Stage ids must be unique: {}", fields[0]);
        }
        let deadline = chrono::DateTime::parse_from_rfc3339(fields[3])?.timestamp();
        let mut stage = ThreadGoalStage {
            id: fields[0].to_string(),
            label: fields[1].to_string(),
            expected_result: fields[2].to_string(),
            deadline_at: deadline,
            delivered_at: None,
            delivered_artifact: None,
        };
        if let Some(previous) = fallback_stages
            .iter()
            .find(|previous| previous.id == stage.id)
        {
            if previous.delivered_at.is_some() {
                stage = previous.clone();
            } else {
                stage.delivered_at = previous.delivered_at;
                stage.delivered_artifact = previous.delivered_artifact.clone();
            }
        }
        stages.push(stage);
    }
    Ok((objective.trim().to_string(), timezone, stages))
}

pub(crate) fn format_goal_elapsed_seconds(seconds: i64) -> String {
    let seconds = seconds.max(0) as u64;
    if seconds < 60 {
        return format!("{seconds}s");
    }

    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }

    let hours = minutes / 60;
    let remaining_minutes = minutes % 60;
    if hours >= 24 {
        let days = hours / 24;
        let remaining_hours = hours % 24;
        return format!("{days}d {remaining_hours}h {remaining_minutes}m");
    }

    if remaining_minutes == 0 {
        format!("{hours}h")
    } else {
        format!("{hours}h {remaining_minutes}m")
    }
}

pub(crate) fn goal_status_label(status: ThreadGoalStatus) -> &'static str {
    match status {
        ThreadGoalStatus::Active => "active",
        ThreadGoalStatus::Paused => "paused",
        ThreadGoalStatus::Blocked => "stalled",
        ThreadGoalStatus::UsageLimited => "usage limited",
        ThreadGoalStatus::BudgetLimited => "limited by budget",
        ThreadGoalStatus::Complete => "complete",
    }
}

pub(crate) fn goal_usage_summary(goal: &ThreadGoal) -> String {
    let mut parts = vec![format!("Objective: {}", goal.objective)];
    if goal.time_used_seconds > 0 {
        parts.push(format!(
            "Time: {}.",
            format_goal_elapsed_seconds(goal.time_used_seconds)
        ));
    }
    if let Some(token_budget) = goal.token_budget {
        parts.push(format!(
            "Tokens: {}/{}.",
            format_tokens_compact(goal.tokens_used),
            format_tokens_compact(token_budget)
        ));
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_app_server_protocol::ThreadGoal;
    use codex_app_server_protocol::ThreadGoalStatus;
    use pretty_assertions::assert_eq;

    #[test]
    fn format_goal_elapsed_seconds_is_compact() {
        assert_eq!(format_goal_elapsed_seconds(/*seconds*/ 0), "0s");
        assert_eq!(format_goal_elapsed_seconds(/*seconds*/ 59), "59s");
        assert_eq!(format_goal_elapsed_seconds(/*seconds*/ 60), "1m");
        assert_eq!(format_goal_elapsed_seconds(30 * 60), "30m");
        assert_eq!(format_goal_elapsed_seconds(90 * 60), "1h 30m");
        assert_eq!(format_goal_elapsed_seconds(2 * 60 * 60), "2h");
        let just_before_one_day = 24 * 60 * 60 - 1;
        assert_eq!(format_goal_elapsed_seconds(just_before_one_day), "23h 59m");

        let one_day = 24 * 60 * 60;
        assert_eq!(format_goal_elapsed_seconds(one_day), "1d 0h 0m");

        let almost_three_days = 2 * 24 * 60 * 60 + 23 * 60 * 60 + 42 * 60;
        assert_eq!(format_goal_elapsed_seconds(almost_three_days), "2d 23h 42m");
    }

    fn test_thread_goal(token_budget: Option<i64>, tokens_used: i64) -> ThreadGoal {
        ThreadGoal {
            timezone: None,
            stages: Vec::new(),
            initial_quota_snapshots: Vec::new(),
            initial_token_budget: None,
            thread_id: "thread-1".to_string(),
            objective: "Complete the task described in ../gameboy-long-running-prompt5.txt"
                .to_string(),
            status: ThreadGoalStatus::BudgetLimited,
            token_budget,
            tokens_used,
            time_used_seconds: 120,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn goal_usage_summary_formats_time_and_budgeted_tokens() {
        assert_eq!(
            goal_usage_summary(&test_thread_goal(
                /*token_budget*/ Some(50_000),
                /*tokens_used*/ 63_876,
            )),
            "Objective: Complete the task described in ../gameboy-long-running-prompt5.txt Time: 2m. Tokens: 63.9K/50K."
        );
    }

    #[test]
    fn schedule_editor_round_trips_explicit_rfc3339_stages() {
        let text = "Ship the release\n--- schedule ---\ntimezone: Asia/Tbilisi\nstage: alpha | Alpha | Build artifact | 2026-09-30T18:00:00+04:00\n";
        let (objective, timezone, stages) =
            parse_goal_editor_text(text, None, Vec::new()).expect("valid schedule");
        assert_eq!(objective, "Ship the release");
        assert_eq!(timezone.as_deref(), Some("Asia/Tbilisi"));
        assert_eq!(stages.len(), 1);
        assert_eq!(stages[0].id, "alpha");
        assert_eq!(stages[0].deadline_at, 1_790_776_800);
    }

    #[test]
    fn schedule_editor_rejects_ambiguous_deadlines_and_ids() {
        let error = parse_goal_editor_text(
            "Goal\n--- schedule ---\nstage: bad id | Label | Result | 2026-09-30 18:00\n",
            None,
            Vec::new(),
        )
        .expect_err("invalid stage should be rejected");
        assert!(
            error
                .to_string()
                .contains("Stage ids must not contain whitespace")
        );
    }

    #[test]
    fn schedule_editor_preserves_server_delivered_stage() {
        let previous = ThreadGoalStage {
            id: "done".to_string(),
            label: "Server label".to_string(),
            expected_result: "Server result".to_string(),
            deadline_at: 1_790_776_800,
            delivered_at: Some(1_790_000_000),
            delivered_artifact: Some("artifact://server".to_string()),
        };
        let (_, _, stages) = parse_goal_editor_text(
            "Goal\n--- schedule ---\nstage: done | Edited | Spoof | 2027-01-01T00:00:00Z\n",
            Some("UTC".to_string()),
            vec![previous.clone()],
        )
        .expect("valid schedule");
        assert_eq!(stages, vec![previous]);
    }
}
