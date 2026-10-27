use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

use crate::goal::GoalQuotaSnapshot;

/// Bounded canonical facts for a live goal execution.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GoalExecutionContext {
    pub objective: String,
    pub now_at: i64,
    pub created_at: i64,
    pub timezone: Option<String>,
    pub final_deadline_at: Option<i64>,
    pub final_deadline_remaining_seconds: Option<i64>,
    pub final_deadline_overdue_seconds: Option<i64>,
    pub final_deadline_remaining_percent: Option<i64>,
    pub current_stage: Option<GoalStageExecutionContext>,
    pub overdue_stage_count: usize,
    pub latest_delivered_artifact: Option<String>,
    pub latest_delivered_at: Option<i64>,
    pub initial_token_budget: Option<i64>,
    pub current_token_budget: Option<i64>,
    pub tokens_used: i64,
    pub time_used_seconds: i64,
    pub initial_quota_snapshots: Vec<GoalQuotaSnapshot>,
    pub current_quota_snapshots: Vec<GoalQuotaSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GoalStageExecutionContext {
    pub id: String,
    pub label: String,
    pub expected_result: String,
    pub deadline_at: i64,
    pub remaining_seconds: i64,
    pub overdue_seconds: i64,
    pub remaining_percent: Option<i64>,
    pub delivered_at: Option<i64>,
    pub delivered_artifact: Option<String>,
}
