use super::*;
use pretty_assertions::assert_eq;

fn args_with_fork_turns(fork_turns: Option<&str>) -> SpawnAgentArgs {
    SpawnAgentArgs {
        message: "inspect this repo".to_string(),
        task_name: "worker".to_string(),
        agent_type: None,
        model: None,
        reasoning_effort: None,
        service_tier: None,
        fork_turns: fork_turns.map(str::to_string),
        fork_context: None,
    }
}

#[test]
fn omitted_fork_turns_defaults_to_three_recent_turns() {
    assert_eq!(
        args_with_fork_turns(None).fork_mode(),
        Ok(Some(SpawnAgentForkMode::LastNTurns(3)))
    );
}

#[test]
fn explicit_none_forks_without_parent_context() {
    assert_eq!(args_with_fork_turns(Some("none")).fork_mode(), Ok(None));
}

#[test]
fn explicit_all_forks_full_parent_history() {
    assert_eq!(
        args_with_fork_turns(Some("all")).fork_mode(),
        Ok(Some(SpawnAgentForkMode::FullHistory))
    );
}
