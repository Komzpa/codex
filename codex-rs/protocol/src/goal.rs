//! Durable, user-authored goal scheduling data.
//!
//! These types intentionally carry only schedule and accounting baselines. Goal
//! execution remains owned by the runtime; an overdue stage is informational and
//! never changes a goal's status on its own.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

use crate::protocol::RateLimitSnapshot;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "protocol/")]
pub struct ThreadGoalStage {
    /// Stable user-visible identity. Revisions retain a stage's id.
    pub id: String,
    pub label: String,
    #[serde(alias = "expected_result")]
    pub expected_result: String,
    /// Unix seconds at which this deliverable is due.
    #[serde(alias = "deadline_at")]
    #[ts(type = "number")]
    pub deadline_at: i64,
    /// Server-stamped Unix seconds when evidence was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub delivered_at: Option<i64>,
    /// Reference to an actually delivered artifact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub delivered_artifact: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "protocol/")]
pub struct GoalQuotaSnapshot {
    /// Unix seconds when this host observed the limits.
    pub captured_at: i64,
    /// The provider or runtime that supplied the limits.
    pub source: String,
    /// Account/workspace scope to which the limits apply.
    pub scope_id: String,
    pub limits: Vec<RateLimitSnapshot>,
}
