//! Goal summary for the bare `/goal` command.

use super::*;
use crate::goal_display::format_goal_editor_text;
use crate::goal_display::format_goal_elapsed_seconds;
use crate::goal_files;
use crate::status::format_tokens_compact;
use chrono::DateTime;
use chrono::Utc;
use codex_goal_extension::summarize_goal_execution;

impl ChatWidget {
    pub(crate) fn show_goal_summary(&mut self, goal: AppThreadGoal) {
        self.add_plain_history_lines(goal_summary_lines_at(&goal, Utc::now().timestamp()));
    }

    pub(crate) fn show_goal_edit_prompt(&mut self, thread_id: ThreadId, goal: AppThreadGoal) {
        let tx = self.app_event_tx.clone();
        let status = edited_goal_status(goal.status);
        let token_budget = goal.token_budget;
        let timezone = goal.timezone.clone();
        let stages = goal.stages.clone();
        let editor_text =
            format_goal_editor_text(&goal.objective, goal.timezone.as_deref(), &goal.stages);
        let view = CustomPromptView::new(
            "Edit goal".to_string(),
            "Type a goal objective and press Enter".to_string(),
            editor_text,
            /*context_label*/ None,
            Box::new(move |objective: String| {
                tx.send(AppEvent::SetThreadGoalDraft {
                    thread_id,
                    draft: goal_files::GoalDraft {
                        objective: objective.clone(),
                        schedule_editor_text: Some(objective),
                        timezone: timezone.clone(),
                        stages: Some(stages.clone()),
                        ..Default::default()
                    },
                    mode: crate::app_event::ThreadGoalSetMode::UpdateExisting {
                        status,
                        token_budget,
                    },
                });
            }),
        );
        self.bottom_pane.show_text_prompt(view);
    }

    pub(crate) fn show_resume_paused_goal_prompt(
        &mut self,
        thread_id: ThreadId,
        objective: String,
    ) {
        let resume_actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
            tx.send(AppEvent::SetThreadGoalStatus {
                thread_id,
                status: AppThreadGoalStatus::Active,
            });
        })];
        self.show_selection_view(SelectionViewParams {
            title: Some("Resume paused goal?".to_string()),
            subtitle: Some(format!("Goal: {objective}")),
            footer_hint: Some(standard_popup_hint_line()),
            initial_selected_idx: Some(0),
            items: vec![
                SelectionItem {
                    name: "Resume goal".to_string(),
                    description: Some("Mark it active and continue when idle".to_string()),
                    actions: resume_actions,
                    dismiss_on_select: true,
                    ..Default::default()
                },
                SelectionItem {
                    name: "Leave paused".to_string(),
                    description: Some("Keep it paused; use /goal resume later".to_string()),
                    dismiss_on_select: true,
                    ..Default::default()
                },
            ],
            ..SelectionViewParams::picker()
        });
    }

    pub(crate) fn on_thread_goal_cleared(&mut self, thread_id: &str) {
        if self
            .thread_id
            .is_some_and(|active_thread_id| active_thread_id.to_string() == thread_id)
        {
            self.current_goal_status = None;
            self.update_collaboration_mode_indicator();
        }
    }
}

pub(super) fn goal_summary_lines_at(goal: &AppThreadGoal, now_at: i64) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from("Goal".bold()),
        Line::from(vec![
            "Status: ".dim(),
            goal_status_label(goal.status).to_string().into(),
        ]),
        Line::from(vec!["Objective: ".dim(), goal.objective.clone().into()]),
        Line::from(vec![
            "Time used: ".dim(),
            format_goal_elapsed_seconds(goal.time_used_seconds).into(),
        ]),
        Line::from(vec![
            "Tokens used: ".dim(),
            format_tokens_compact(goal.tokens_used).into(),
        ]),
    ];
    if let Some(token_budget) = goal.token_budget {
        lines.push(Line::from(vec![
            "Token budget: ".dim(),
            format_tokens_compact(token_budget).into(),
        ]));
    }
    if let Some(timezone) = goal.timezone.as_deref() {
        lines.push(Line::from(vec![
            "Timezone: ".dim(),
            timezone.to_string().into(),
        ]));
    }
    if !goal.stages.is_empty() {
        lines.push(Line::from("Stages".bold()));
        for stage in &goal.stages {
            let deadline = DateTime::<Utc>::from_timestamp(stage.deadline_at, 0)
                .map(|timestamp| timestamp.format("%Y-%m-%d %H:%M UTC").to_string())
                .unwrap_or_else(|| "invalid deadline".to_string());
            let delivered = stage
                .delivered_artifact
                .as_deref()
                .map(|artifact| format!("; result: {artifact}"))
                .unwrap_or_default();
            lines.push(Line::from(format!(
                "  {} [{}] — {}{}",
                stage.label, deadline, stage.expected_result, delivered
            )));
        }
        let execution_goal = codex_protocol::protocol::ThreadGoal {
            thread_id: codex_protocol::ThreadId::new(),
            objective: goal.objective.clone(),
            status: codex_protocol::protocol::ThreadGoalStatus::Active,
            token_budget: goal.token_budget,
            tokens_used: goal.tokens_used,
            time_used_seconds: goal.time_used_seconds,
            created_at: goal.created_at,
            updated_at: goal.updated_at,
            timezone: goal.timezone.clone(),
            stages: goal.stages.clone(),
            initial_quota_snapshots: Vec::new(),
            initial_token_budget: goal.initial_token_budget,
        };
        let context = summarize_goal_execution(&execution_goal, now_at);
        if let Some(stage) = context.current_stage {
            lines.push(Line::from(format!(
                "  Current: {} — {}",
                stage.label,
                format_stage_remaining(stage.remaining_seconds, stage.overdue_seconds)
            )));
        }
        if let Some(deadline) = context.final_deadline_at {
            lines.push(Line::from(format!(
                "  Final: {} — {}",
                deadline,
                format_stage_remaining(
                    context.final_deadline_remaining_seconds.unwrap_or_default(),
                    context.final_deadline_overdue_seconds.unwrap_or_default()
                )
            )));
        }
    }
    let command_hint = match goal.status {
        AppThreadGoalStatus::Active => "Commands: /goal edit, /goal pause, /goal clear",
        AppThreadGoalStatus::Paused
        | AppThreadGoalStatus::Blocked
        | AppThreadGoalStatus::UsageLimited => "Commands: /goal edit, /goal resume, /goal clear",
        AppThreadGoalStatus::BudgetLimited | AppThreadGoalStatus::Complete => {
            "Commands: /goal edit, /goal clear"
        }
    };
    lines.push(Line::default());
    lines.push(Line::from(command_hint.dim()));
    lines
}

fn format_stage_remaining(remaining_seconds: i64, overdue_seconds: i64) -> String {
    if overdue_seconds > 0 {
        format!(
            "overdue by {}",
            format_goal_elapsed_seconds(overdue_seconds)
        )
    } else {
        format!(
            "{} remaining",
            format_goal_elapsed_seconds(remaining_seconds)
        )
    }
}

fn goal_status_label(status: AppThreadGoalStatus) -> &'static str {
    match status {
        AppThreadGoalStatus::Active => "active",
        AppThreadGoalStatus::Paused => "paused",
        AppThreadGoalStatus::Blocked => "stalled",
        AppThreadGoalStatus::UsageLimited => "usage limited",
        AppThreadGoalStatus::BudgetLimited => "limited by budget",
        AppThreadGoalStatus::Complete => "complete",
    }
}

fn edited_goal_status(status: AppThreadGoalStatus) -> AppThreadGoalStatus {
    match status {
        AppThreadGoalStatus::Active => AppThreadGoalStatus::Active,
        AppThreadGoalStatus::Paused
        | AppThreadGoalStatus::Blocked
        | AppThreadGoalStatus::UsageLimited => status,
        AppThreadGoalStatus::BudgetLimited | AppThreadGoalStatus::Complete => {
            AppThreadGoalStatus::Active
        }
    }
}
