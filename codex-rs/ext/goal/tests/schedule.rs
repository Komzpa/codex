use codex_goal_extension::model_reminder;
use codex_goal_extension::summarize_goal_execution;
use codex_protocol::ThreadId;
use codex_protocol::goal::ThreadGoalStage;
use codex_protocol::protocol::ThreadGoal;
use codex_protocol::protocol::ThreadGoalStatus;
use pretty_assertions::assert_eq;

fn goal() -> ThreadGoal {
    ThreadGoal {
        thread_id: ThreadId::new(),
        objective: "Deliver the video".to_string(),
        status: ThreadGoalStatus::Active,
        token_budget: Some(10000),
        initial_token_budget: Some(20000),
        tokens_used: 100,
        time_used_seconds: 20,
        created_at: 1_000,
        updated_at: 1_000,
        timezone: Some("Asia/Tbilisi".to_string()),
        initial_quota_snapshots: vec![],
        stages: [4600, 8200]
            .into_iter()
            .enumerate()
            .map(|(i, deadline_at)| ThreadGoalStage {
                id: i.to_string(),
                label: format!("stage {i}"),
                expected_result: "watchable video".to_string(),
                deadline_at,
                delivered_at: None,
                delivered_artifact: None,
            })
            .collect(),
    }
}

#[test]
fn wall_clock_deadlines_survive_pause_and_revisions() {
    let mut goal = goal();
    let context = summarize_goal_execution(&goal, 2800);
    assert_eq!(
        context.current_stage.as_ref().unwrap().remaining_percent,
        Some(50)
    );
    assert_eq!(context.final_deadline_remaining_percent, Some(75));
    let reminder = model_reminder(&context);
    assert!(reminder.contains("left 0h30m (50%)"));
    assert!(reminder.contains("left 1h30m (75%)"));
    assert!(reminder.contains("1970-01-01T00:16:40Z"));
    assert!(reminder.contains("start=20000 now=10000"));
    goal.status = ThreadGoalStatus::Paused;
    goal.stages[0].deadline_at = 6400;
    let context = summarize_goal_execution(&goal, 2800);
    assert_eq!(context.current_stage.unwrap().remaining_percent, Some(66));
    assert_eq!(context.created_at, 1000);
}

#[test]
fn overdue_and_degenerate_deadlines_do_not_claim_completion() {
    let mut goal = goal();
    goal.stages[0].deadline_at = goal.created_at;
    let context = summarize_goal_execution(&goal, 4600);
    assert_eq!(
        context.current_stage.as_ref().unwrap().remaining_percent,
        Some(0)
    );
    assert_eq!(context.overdue_stage_count, 1);
    let reminder = model_reminder(&context);
    assert!(reminder.contains("overdue 1h00m (0%)"));
    assert!(reminder.contains("show artifact or say none, then continue"));
    assert!(reminder.contains("delegate bounded tasks cheaper"));
    assert_eq!(goal.status, ThreadGoalStatus::Active);
}

#[test]
fn delivered_stage_advances_without_changing_final_deadline() {
    let mut goal = goal();
    goal.stages[0].delivered_at = Some(2000);
    goal.stages[0].delivered_artifact = Some("/tmp/draft.mp4".to_string());
    let context = summarize_goal_execution(&goal, 5000);
    assert_eq!(context.current_stage.as_ref().unwrap().id, "1");
    assert!(model_reminder(&context).contains("/tmp/draft.mp4"));
    assert_eq!(context.overdue_stage_count, 0);
    assert_eq!(context.final_deadline_at, Some(8200));
}

#[test]
fn reminder_has_hard_byte_bound_including_unicode_and_retains_imperative() {
    let mut goal = goal();
    goal.objective = "🦀ѣ中文".repeat(10000);
    goal.stages[0].label = "🦀ѣ中文".repeat(10000);
    let mut context = summarize_goal_execution(&goal, i64::MAX);
    context.timezone = Some("🦀ѣ中文".repeat(10000));
    let reminder = model_reminder(&context);
    assert!(reminder.len() <= 800);
    assert!(reminder.starts_with("Deliver usable work"));
    assert!(reminder.contains("Quota now=unknown"));
    assert!(reminder.contains("delegate bounded tasks cheaper"));
    goal.stages.clear();
    let context = summarize_goal_execution(&goal, 2000);
    assert_eq!(context.current_stage, None);
    assert_eq!(context.final_deadline_at, None);
}

#[test]
fn both_quota_scopes_and_delivery_fit_the_reminder_and_scope_changes_are_unknown() {
    use codex_protocol::goal::GoalQuotaSnapshot;
    let mut goal = goal();
    goal.created_at = 1_790_000_000;
    goal.stages[0].deadline_at = 1_790_004_600;
    goal.stages[1].deadline_at = 1_790_008_200;
    goal.stages[0].delivered_at = Some(1_790_001_000);
    goal.stages[0].delivered_artifact = Some("/tmp/preview.webm".to_string());
    let snapshot = |source: &str, used, captured_at| GoalQuotaSnapshot {
        captured_at,
        source: source.to_string(),
        scope_id: "account-a".to_string(),
        limits: vec![serde_json::from_value(serde_json::json!({
            "limit_id": "codex",
            "primary": {"used_percent": used, "window_minutes": 300, "resets_at": 1790009000},
            "secondary": {"used_percent": 45.0, "window_minutes": 10080, "resets_at": 1790080000}
        })).unwrap()],
    };
    goal.initial_quota_snapshots = vec![
        snapshot("provider-key", 10.0, goal.created_at),
        snapshot("provider-pool", 20.0, goal.created_at),
    ];
    let mut context = summarize_goal_execution(&goal, 1_790_002_000);
    context.current_quota_snapshots = vec![
        snapshot("provider-key", 30.0, context.now_at),
        snapshot("provider-pool", 40.0, context.now_at),
    ];
    let text = model_reminder(&context);
    assert!(text.len() <= 800, "{text}");
    assert!(!text.contains("More facts"), "{text}");
    assert!(text.contains("300m:90>70"), "{text}");
    assert!(text.contains("300m:80>60"), "{text}");
    assert!(text.contains("/tmp/preview.webm"), "{text}");
    assert!(text.contains("reset=1790009000"), "{text}");
    context.current_quota_snapshots[0].scope_id = "account-b".to_string();
    assert!(model_reminder(&context).contains("300m:unknown>70"));
    context.current_quota_snapshots[0].limits[0]
        .primary
        .as_mut()
        .unwrap()
        .used_percent = 0.0;
    assert!(model_reminder(&context).contains("300m:unknown>100"));
}
