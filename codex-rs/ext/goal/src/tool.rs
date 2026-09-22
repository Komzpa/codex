use std::sync::Arc;

use codex_extension_api::FunctionCallError;
use codex_extension_api::JsonToolOutput;
use codex_extension_api::ToolCall;
use codex_extension_api::ToolExecutor;
use codex_extension_api::ToolName;
use codex_extension_api::ToolOutput;
use codex_extension_api::ToolSpec;
use codex_protocol::ThreadId;
use codex_protocol::goal::ThreadGoalStage;
use codex_protocol::protocol::ThreadGoal;
use codex_protocol::protocol::ThreadGoalStatus;
use codex_protocol::protocol::validate_thread_goal_objective;
use serde::Deserialize;
use serde::Serialize;

use crate::accounting::BudgetLimitedGoalDisposition;
use crate::accounting::GoalAccountingState;
use crate::analytics::GoalAnalytics;
use crate::analytics::GoalEventAttribution;
use crate::events::GoalEventEmitter;
use crate::metrics::GoalMetrics;
use crate::spec::CREATE_GOAL_TOOL_NAME;
use crate::spec::GET_GOAL_TOOL_NAME;
use crate::spec::UPDATE_GOAL_TOOL_NAME;
use crate::spec::create_create_goal_tool;
use crate::spec::create_get_goal_tool;
use crate::spec::create_update_goal_tool;

#[derive(Clone)]
pub(crate) struct GoalToolExecutor {
    pub(crate) execution_allowed: bool,
    kind: GoalToolKind,
    thread_id: ThreadId,
    state_db: Arc<codex_state::StateRuntime>,
    accounting_state: Arc<GoalAccountingState>,
    analytics: GoalAnalytics,
    event_emitter: GoalEventEmitter,
    metrics: GoalMetrics,
    max_goal_token_budget: Option<i64>,
    pub(crate) quota_provider: Option<Arc<codex_core::GoalQuotaProvider>>,
}

#[derive(Clone, Copy)]
enum GoalToolKind {
    Get,
    Create,
    Update,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CreateGoalRequest {
    pub objective: String,
    pub token_budget: Option<i64>,
    pub timezone: Option<String>,
    pub stages: Option<Vec<ThreadGoalStage>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct UpdateGoalArgs {
    status: Option<ThreadGoalStatus>,
    timezone: Option<String>,
    stages: Option<Vec<ThreadGoalStage>>,
    stage_id: Option<String>,
    delivered_artifact: Option<String>,
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct GoalToolResponse {
    goal: Option<ThreadGoal>,
    remaining_tokens: Option<i64>,
    completion_budget_report: Option<String>,
    current_quota_snapshots: Vec<codex_protocol::goal::GoalQuotaSnapshot>,
}

#[derive(Clone, Copy)]
enum CompletionBudgetReport {
    Include,
    Omit,
}

impl GoalToolExecutor {
    pub(crate) fn get(
        thread_id: ThreadId,
        state_db: Arc<codex_state::StateRuntime>,
        accounting_state: Arc<GoalAccountingState>,
        analytics: GoalAnalytics,
        event_emitter: GoalEventEmitter,
        metrics: GoalMetrics,
    ) -> Self {
        Self {
            kind: GoalToolKind::Get,
            execution_allowed: true,
            thread_id,
            state_db,
            accounting_state,
            analytics,
            event_emitter,
            metrics,
            max_goal_token_budget: None,
            quota_provider: None,
        }
    }

    pub(crate) fn create(
        thread_id: ThreadId,
        state_db: Arc<codex_state::StateRuntime>,
        accounting_state: Arc<GoalAccountingState>,
        analytics: GoalAnalytics,
        event_emitter: GoalEventEmitter,
        metrics: GoalMetrics,
        max_goal_token_budget: Option<i64>,
    ) -> Self {
        Self {
            kind: GoalToolKind::Create,
            execution_allowed: true,
            thread_id,
            state_db,
            accounting_state,
            analytics,
            event_emitter,
            metrics,
            max_goal_token_budget,
            quota_provider: None,
        }
    }

    pub(crate) fn update(
        thread_id: ThreadId,
        state_db: Arc<codex_state::StateRuntime>,
        accounting_state: Arc<GoalAccountingState>,
        analytics: GoalAnalytics,
        event_emitter: GoalEventEmitter,
        metrics: GoalMetrics,
    ) -> Self {
        Self {
            kind: GoalToolKind::Update,
            execution_allowed: true,
            thread_id,
            state_db,
            accounting_state,
            analytics,
            event_emitter,
            metrics,
            max_goal_token_budget: None,
            quota_provider: None,
        }
    }
}

impl<'call> ToolExecutor<ToolCall<'call>> for GoalToolExecutor {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(match self.kind {
            GoalToolKind::Get => GET_GOAL_TOOL_NAME,
            GoalToolKind::Create => CREATE_GOAL_TOOL_NAME,
            GoalToolKind::Update => UPDATE_GOAL_TOOL_NAME,
        })
    }

    fn spec(&self) -> ToolSpec {
        match self.kind {
            GoalToolKind::Get => create_get_goal_tool(),
            GoalToolKind::Create => create_create_goal_tool(),
            GoalToolKind::Update => create_update_goal_tool(),
        }
    }

    fn handle<'a>(
        &'a self,
        invocation: ToolCall<'call>,
    ) -> codex_extension_api::ToolExecutorFuture<'a>
    where
        'call: 'a,
    {
        Box::pin(async move {
            if !self.execution_allowed {
                return Err(FunctionCallError::RespondToModel(
                    "Goal tools require a persistent thread.".to_string(),
                ));
            }
            match self.kind {
                GoalToolKind::Get => self.handle_get(invocation).await,
                GoalToolKind::Create => self.handle_create(invocation).await,
                GoalToolKind::Update => self.handle_update(invocation).await,
            }
        })
    }
}

impl GoalToolExecutor {
    async fn handle_get(
        &self,
        invocation: ToolCall<'_>,
    ) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
        let _ = invocation.function_arguments()?;
        let goal = self
            .state_db
            .thread_goals()
            .get_thread_goal(self.thread_id)
            .await
            .map(|goal| goal.map(protocol_goal_from_state))
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!("failed to read goal: {err}"))
            })?;
        goal_response(
            goal,
            CompletionBudgetReport::Omit,
            self.quota_provider.as_deref(),
        )
        .await
    }

    async fn handle_create(
        &self,
        invocation: ToolCall<'_>,
    ) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
        let mut request: CreateGoalRequest = parse_arguments(invocation.function_arguments()?)?;
        request.objective = request.objective.trim().to_string();
        validate_thread_goal_objective(&request.objective)
            .map_err(FunctionCallError::RespondToModel)?;
        request.token_budget = request.token_budget.or(self.max_goal_token_budget);
        validate_goal_budget(request.token_budget, self.max_goal_token_budget)
            .map_err(FunctionCallError::RespondToModel)?;
        if let Some(stages) = request.stages.as_ref() {
            crate::api::validate_goal_stages(stages).map_err(FunctionCallError::RespondToModel)?;
        }

        let goal = self
            .state_db
            .thread_goals()
            .insert_thread_goal(
                self.thread_id,
                request.objective.as_str(),
                codex_state::ThreadGoalStatus::Active,
                request.token_budget,
            )
            .await
            .map_err(|err| FunctionCallError::RespondToModel(format!("failed to create goal: {err}")))?
            .ok_or_else(|| {
                FunctionCallError::RespondToModel(
                    "cannot create a new goal because this thread has an unfinished goal; complete the existing goal first"
                        .to_string(),
                )
            })?;
        let goal = if request.stages.is_some() || request.timezone.is_some() {
            self.state_db
                .thread_goals()
                .update_goal_schedule(
                    self.thread_id,
                    codex_state::GoalScheduleUpdate {
                        timezone: request
                            .timezone
                            .or_else(|| Some(crate::schedule::local_goal_timezone())),
                        stages: request.stages.unwrap_or_default(),
                        expected_goal_id: Some(goal.goal_id.clone()),
                    },
                )
                .await
                .map_err(|err| {
                    FunctionCallError::RespondToModel(format!("failed to schedule goal: {err}"))
                })?
                .ok_or_else(|| {
                    FunctionCallError::RespondToModel(
                        "goal disappeared while scheduling".to_string(),
                    )
                })?
        } else {
            goal
        };
        let snapshots = if let Some(provider) = self.quota_provider.as_ref() {
            provider.snapshot_many().await.unwrap_or_default()
        } else {
            Vec::new()
        };
        let goal = self
            .state_db
            .thread_goals()
            .initialize_goal_baseline(self.thread_id, &goal.goal_id, &snapshots)
            .await
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!("failed to capture goal baseline: {err}"))
            })?
            .unwrap_or(goal);
        fill_empty_thread_preview_if_possible(self.state_db.as_ref(), self.thread_id, &goal).await;
        let turn_id = self
            .accounting_state
            .mark_current_turn_goal_active(goal.goal_id.clone());
        self.metrics.record_created();
        self.analytics.created(
            &goal,
            GoalEventAttribution::Turn(invocation.turn_id.as_str()),
        );
        let goal = protocol_goal_from_state(goal);
        self.emit_goal_updated_from_tool_call(&invocation, turn_id, goal.clone());
        goal_response(
            Some(goal),
            CompletionBudgetReport::Omit,
            self.quota_provider.as_deref(),
        )
        .await
    }

    async fn handle_update(
        &self,
        invocation: ToolCall<'_>,
    ) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
        let args: UpdateGoalArgs = parse_arguments(invocation.function_arguments()?)?;
        let scheduling = args.stages.is_some() || args.timezone.is_some();
        let delivery = args.stage_id.is_some() || args.delivered_artifact.is_some();
        if usize::from(scheduling) + usize::from(delivery) + usize::from(args.status.is_some()) > 1
        {
            return Err(FunctionCallError::RespondToModel(
                "update_goal accepts one operation: status, schedule, or stage delivery"
                    .to_string(),
            ));
        }
        if delivery {
            let (Some(stage_id), Some(artifact)) = (args.stage_id, args.delivered_artifact) else {
                return Err(FunctionCallError::RespondToModel(
                    "stage delivery requires both stage_id and delivered_artifact".to_string(),
                ));
            };
            if stage_id.trim().is_empty() || artifact.trim().is_empty() || artifact.len() > 8192 {
                return Err(FunctionCallError::RespondToModel(
                    "stage delivery needs a nonempty stage_id and artifact reference (at most 8192 bytes)".to_string(),
                ));
            }
            let goal = self
                .state_db
                .thread_goals()
                .record_goal_stage_delivery(self.thread_id, &stage_id, &artifact)
                .await
                .map_err(|err| {
                    FunctionCallError::RespondToModel(format!(
                        "failed to record stage delivery: {err}"
                    ))
                })?
                .ok_or_else(|| {
                    FunctionCallError::RespondToModel(
                        "goal or stage not found, or changed concurrently; read get_goal"
                            .to_string(),
                    )
                })?;
            let goal = protocol_goal_from_state(goal);
            self.emit_goal_updated_from_tool_call(&invocation, None, goal.clone());
            return goal_response(
                Some(goal),
                CompletionBudgetReport::Omit,
                self.quota_provider.as_deref(),
            )
            .await;
        }
        if args.status.is_none() && args.stages.is_some() {
            let stages = args.stages.unwrap_or_default();
            crate::api::validate_goal_stages(&stages).map_err(FunctionCallError::RespondToModel)?;
            let goal = self
                .state_db
                .thread_goals()
                .update_goal_schedule(
                    self.thread_id,
                    codex_state::GoalScheduleUpdate {
                        timezone: args.timezone,
                        stages,
                        expected_goal_id: None,
                    },
                )
                .await
                .map_err(|err| {
                    FunctionCallError::RespondToModel(format!(
                        "failed to update goal schedule: {err}"
                    ))
                })?
                .ok_or_else(|| {
                    FunctionCallError::RespondToModel(
                        "cannot update goal schedule because this thread has no goal".to_string(),
                    )
                })?;
            let goal = protocol_goal_from_state(goal);
            self.emit_goal_updated_from_tool_call(&invocation, None, goal.clone());
            return goal_response(
                Some(goal),
                CompletionBudgetReport::Omit,
                self.quota_provider.as_deref(),
            )
            .await;
        }
        let status = args.status.ok_or_else(|| {
            FunctionCallError::RespondToModel(
                "update_goal requires status, stages, or stage delivery".to_string(),
            )
        })?;
        if !matches!(
            status,
            ThreadGoalStatus::Complete | ThreadGoalStatus::Blocked | ThreadGoalStatus::Paused
        ) {
            return Err(FunctionCallError::RespondToModel(
                "update_goal can only mark the existing goal complete, blocked, or paused at the user's explicit request; resume, budget-limited, and usage-limited status changes are controlled by the user or system"
                    .to_string(),
            ));
        }

        self.account_active_goal_progress(
            match status {
                ThreadGoalStatus::Complete => codex_state::GoalAccountingMode::ActiveOrComplete,
                ThreadGoalStatus::Blocked | ThreadGoalStatus::Paused => {
                    codex_state::GoalAccountingMode::ActiveOrStopped
                }
                ThreadGoalStatus::Active
                | ThreadGoalStatus::UsageLimited
                | ThreadGoalStatus::BudgetLimited => unreachable!("status validated above"),
            },
            invocation.call_id.as_str(),
            BudgetLimitedGoalDisposition::ClearActive,
        )
        .await?;
        let previous_status = self
            .current_goal_status_for_metrics(/*expected_goal_id*/ None)
            .await?;
        let goal = self
            .state_db
            .thread_goals()
            .update_thread_goal(
                self.thread_id,
                codex_state::GoalUpdate {
                    objective: None,
                    status: Some(state_status_from_protocol(status)),
                    token_budget: None,
                    expected_goal_id: None,
                },
            )
            .await
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!("failed to update goal: {err}"))
            })?
            .ok_or_else(|| {
                FunctionCallError::RespondToModel(
                    "cannot update goal because this thread has no goal".to_string(),
                )
            })?;
        self.metrics
            .record_terminal_if_status_changed(previous_status, &goal);
        self.analytics.status_changed(
            &goal,
            previous_status,
            GoalEventAttribution::Turn(invocation.turn_id.as_str()),
        );
        let goal = protocol_goal_from_state(goal);
        let turn_id = self.accounting_state.clear_current_turn_goal();
        self.emit_goal_updated_from_tool_call(&invocation, turn_id, goal.clone());
        goal_response(
            Some(goal),
            if status == ThreadGoalStatus::Complete {
                CompletionBudgetReport::Include
            } else {
                CompletionBudgetReport::Omit
            },
            self.quota_provider.as_deref(),
        )
        .await
    }

    fn emit_goal_updated_from_tool_call(
        &self,
        invocation: &ToolCall<'_>,
        turn_id: Option<String>,
        goal: ThreadGoal,
    ) {
        self.event_emitter
            .thread_goal_updated(invocation.call_id.clone(), turn_id, goal);
    }

    async fn account_active_goal_progress(
        &self,
        mode: codex_state::GoalAccountingMode,
        event_id: &str,
        budget_limited_goal_disposition: BudgetLimitedGoalDisposition,
    ) -> Result<Option<ThreadGoal>, FunctionCallError> {
        let Some(turn_id) = self.accounting_state.current_turn_id() else {
            return Ok(None);
        };
        let _accounting_permit = self
            .accounting_state
            .progress_accounting_permit()
            .await
            .map_err(|err| {
                FunctionCallError::Fatal(format!(
                    "goal progress accounting semaphore closed: {err}"
                ))
            })?;
        let Some(snapshot) = self.accounting_state.progress_snapshot(turn_id.as_str()) else {
            return Ok(None);
        };
        let previous_status = self
            .current_goal_status_for_metrics(Some(snapshot.expected_goal_id.as_str()))
            .await?;
        let outcome = self
            .state_db
            .thread_goals()
            .account_thread_goal_usage(
                self.thread_id,
                snapshot.time_delta_seconds,
                snapshot.token_delta,
                mode,
                Some(snapshot.expected_goal_id.as_str()),
            )
            .await
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!("failed to account goal progress: {err}"))
            })?;
        Ok(match outcome {
            codex_state::GoalAccountingOutcome::Updated(goal) => {
                self.metrics
                    .record_terminal_if_status_changed(previous_status, &goal);
                self.analytics
                    .usage_accounted(&goal, GoalEventAttribution::Turn(turn_id.as_str()));
                self.analytics.status_changed(
                    &goal,
                    previous_status,
                    GoalEventAttribution::Turn(turn_id.as_str()),
                );
                self.accounting_state.mark_progress_accounted_for_status(
                    turn_id.as_str(),
                    &snapshot,
                    goal.status,
                    budget_limited_goal_disposition,
                );
                let goal = protocol_goal_from_state(goal);
                self.event_emitter.thread_goal_updated(
                    event_id.to_string(),
                    Some(turn_id),
                    goal.clone(),
                );
                Some(goal)
            }
            codex_state::GoalAccountingOutcome::Unchanged(_) => None,
        })
    }

    async fn current_goal_status_for_metrics(
        &self,
        expected_goal_id: Option<&str>,
    ) -> Result<Option<codex_state::ThreadGoalStatus>, FunctionCallError> {
        let goal = self
            .state_db
            .thread_goals()
            .get_thread_goal(self.thread_id)
            .await
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "failed to read goal metrics status: {err}"
                ))
            })?;
        Ok(goal.and_then(|goal| {
            expected_goal_id
                .is_none_or(|expected_goal_id| goal.goal_id == expected_goal_id)
                .then_some(goal.status)
        }))
    }
}

fn parse_arguments<T>(arguments: &str) -> Result<T, FunctionCallError>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_str(arguments)
        .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))
}

pub(crate) fn validate_goal_budget(
    value: Option<i64>,
    max_goal_token_budget: Option<i64>,
) -> Result<(), String> {
    if let Some(value) = value
        && value <= 0
    {
        return Err("goal budgets must be positive when provided".to_string());
    }
    if let Some(value) = value
        && let Some(max_goal_token_budget) = max_goal_token_budget
        && value > max_goal_token_budget
    {
        return Err(format!(
            "goal token budget {value} exceeds the maximum allowed goal token budget of {max_goal_token_budget}"
        ));
    }
    Ok(())
}

async fn goal_response(
    goal: Option<ThreadGoal>,
    completion_budget_report: CompletionBudgetReport,
    quota_provider: Option<&codex_core::GoalQuotaProvider>,
) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
    let mut response = GoalToolResponse::new(goal, completion_budget_report);
    if let Some(provider) = quota_provider {
        response.current_quota_snapshots = provider.snapshot_many().await.unwrap_or_default();
    }
    let value =
        serde_json::to_value(response).map_err(|err| FunctionCallError::Fatal(err.to_string()))?;
    Ok(Box::new(JsonToolOutput::new(value)))
}

impl GoalToolResponse {
    fn new(goal: Option<ThreadGoal>, report_mode: CompletionBudgetReport) -> Self {
        let remaining_tokens = goal.as_ref().and_then(|goal| {
            goal.token_budget
                .map(|budget| (budget - goal.tokens_used).max(0))
        });
        let completion_budget_report = match report_mode {
            CompletionBudgetReport::Include => goal
                .as_ref()
                .filter(|goal| goal.status == ThreadGoalStatus::Complete)
                .and_then(completion_budget_report),
            CompletionBudgetReport::Omit => None,
        };
        Self {
            goal,
            remaining_tokens,
            completion_budget_report,
            current_quota_snapshots: Vec::new(),
        }
    }
}

pub(crate) async fn fill_empty_thread_preview_if_possible(
    state_db: &codex_state::StateRuntime,
    thread_id: ThreadId,
    goal: &codex_state::ThreadGoal,
) {
    if let Err(err) = state_db
        .set_thread_preview_if_empty(thread_id, goal.objective.as_str())
        .await
    {
        tracing::warn!(
            "failed to set empty thread preview from goal objective for {thread_id}: {err}"
        );
    }
}

pub(crate) fn protocol_goal_from_state(goal: codex_state::ThreadGoal) -> ThreadGoal {
    ThreadGoal {
        thread_id: goal.thread_id,
        objective: goal.objective,
        status: protocol_status_from_state(goal.status),
        token_budget: goal.token_budget,
        tokens_used: goal.tokens_used,
        time_used_seconds: goal.time_used_seconds,
        created_at: goal.created_at.timestamp(),
        updated_at: goal.updated_at.timestamp(),
        timezone: goal.timezone,
        stages: goal.stages,
        initial_quota_snapshots: goal.initial_quota_snapshots,
        initial_token_budget: goal.initial_token_budget,
    }
}

fn protocol_status_from_state(status: codex_state::ThreadGoalStatus) -> ThreadGoalStatus {
    match status {
        codex_state::ThreadGoalStatus::Active => ThreadGoalStatus::Active,
        codex_state::ThreadGoalStatus::Paused => ThreadGoalStatus::Paused,
        codex_state::ThreadGoalStatus::Blocked => ThreadGoalStatus::Blocked,
        codex_state::ThreadGoalStatus::UsageLimited => ThreadGoalStatus::UsageLimited,
        codex_state::ThreadGoalStatus::BudgetLimited => ThreadGoalStatus::BudgetLimited,
        codex_state::ThreadGoalStatus::Complete => ThreadGoalStatus::Complete,
    }
}

pub(crate) fn state_status_from_protocol(
    status: ThreadGoalStatus,
) -> codex_state::ThreadGoalStatus {
    match status {
        ThreadGoalStatus::Active => codex_state::ThreadGoalStatus::Active,
        ThreadGoalStatus::Paused => codex_state::ThreadGoalStatus::Paused,
        ThreadGoalStatus::Blocked => codex_state::ThreadGoalStatus::Blocked,
        ThreadGoalStatus::UsageLimited => codex_state::ThreadGoalStatus::UsageLimited,
        ThreadGoalStatus::BudgetLimited => codex_state::ThreadGoalStatus::BudgetLimited,
        ThreadGoalStatus::Complete => codex_state::ThreadGoalStatus::Complete,
    }
}

fn completion_budget_report(goal: &ThreadGoal) -> Option<String> {
    if goal.token_budget.is_none() && goal.time_used_seconds <= 0 {
        None
    } else {
        Some(
            "Goal achieved. Report final usage from this tool result's structured goal fields. If `goal.tokenBudget` is present, include token usage from `goal.tokensUsed` and `goal.tokenBudget`. If `goal.timeUsedSeconds` is greater than 0, summarize elapsed time in a concise, human-friendly form appropriate to the response language."
                .to_string(),
        )
    }
}
