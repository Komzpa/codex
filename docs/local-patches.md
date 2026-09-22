# Local source-refresh patches

This branch is rebased on upstream `e8cd975a55`. It retains focused
patches that upstream did not yet cover. The archive used to reconstruct the
set was `9c57bef5d1`; the entries below describe the current behavior, not the
old replay chronology.

Before dropping any entry, compare the current upstream implementation and its
tests with the stated behavior. Matching names or a nearby refactor is not
enough. Run the named source check and the same installed CLI oracle that
exposed the problem.

| Patch | Problem and canonical owner | Current upstream verdict and removal condition | Regression evidence |
| --- | --- | --- | --- |
| `fix(tui): expose Markdown destinations in Konsole` | Konsole can render an OSC 8 link as only an underlined label, leaving no recoverable URL. `codex-rs/tui/src/markdown_render/web_links.rs` now renders label plus destination for Konsole; tests live beside it. | **Keep.** Upstream's clickable-label work still classifies Konsole as `LabelOnly`. Drop only when upstream exposes the full destination for Konsole through an equally safe terminal-capability rule. | `unknown_terminals_and_multiplexers_keep_visible_destinations`; installed Konsole must render the original Markdown fixture with its URL visible. |
| `fix(tui): allow saving Ultra as the default reasoning effort` | An explicit Ultra choice was removed while saving fresh-session settings, so new CLI processes reopened at another effort. The owners are `tui/src/app/config_persistence.rs` and `tui/src/chatwidget/model_popups.rs`, with popup tests and snapshot. | **Keep.** Upstream has reasoning controls but does not retain explicit Ultra in fresh-session configuration or offer an equivalent save action. Drop only when both are present upstream. | `conversation_reasoning_preserves_explicit_ultra_default`; `ultra_reasoning_can_be_saved_as_default`; advanced-reasoning snapshot. Installed oracle: save Ultra, start a new CLI process, and read Ultra back. |
| `fix(api): retry planned server draining beyond the stream budget` | A Responses `503` whose code is `server_draining` is planned replacement work. `codex-api/src/api_bridge.rs`, `protocol/src/error.rs`, and `core/src/responses_retry.rs` map it to a delayed Retry-After retry without spending the ordinary stream budget. | **Keep.** Upstream's app-server admission state does not supply this Responses-client mapping or retry semantic. Drop only for the same client behavior with an equivalent integration test. | `map_api_error_maps_server_draining_503_to_delayed_retry`; `server_draining_retries_beyond_the_normal_stream_budget` (four drain replies, stream budget one, then success). |
| `fix(agents): default to three turns and enforce full-history inheritance` | Default `fork_turns=all` carried stale context. Current V2 owners are `tools/handlers/multi_agents_spec.rs`, `tools/handlers/multi_agents_v2/spawn.rs`, and `agent/child_config.rs`; full-history children inherit the invoking parent's role, model, and effort, including when different child defaults are configured. | **Keep.** Current upstream does not provide all four semantics: default three recent turns, explicit `none`, explicit `all`, and rejection of child overrides for `all`. Drop only when all four are covered upstream. | `omitted_fork_turns_defaults_to_three_recent_turns`; `explicit_none_forks_without_parent_context`; `explicit_all_forks_full_parent_history`; `multi_agent_v2_spawn_fork_turns_all_rejects_child_overrides`; `multi_agent_v2_full_history_fork_rejects_configuration_overrides`; `multi_agent_v2_default_spawn_forks_latest_three_real_turns`; `spawned_full_history_v2_child_inherits_settings_without_dropping_context`. |
| `fix(cli): bound default doctor integrity checks` | Ordinary `codex doctor` can stall on large SQLite histories. `cli/src/doctor.rs` applies a one-second per-database deadline by default; `--full-integrity-check` is the deliberate unbounded operator action, while feedback stays bounded. | **Keep.** Upstream attachment limits do not bound this direct CLI path. Drop only when normal direct doctor and the full-scan escape hatch have the same semantics upstream. | CLI oracle: the database integrity check reports its one-second timeout on a slow database; `--full-integrity-check` permits completion; feedback remains bounded. |
| `fix(rollout): bound doctor inventory header reads` | Inventory needs metadata, not an unbounded decompressed line. `cli/src/doctor/thread_inventory.rs` calls the canonical bounded reader exposed by `rollout/src/compression.rs` and `rollout/src/lib.rs`, with a 256 KiB limit. | **Keep.** Upstream lacks the bounded reader and inventory cap; its `ReadMetrics` instrumentation (`da18000cae`) is kept and the limit is threaded through `open_rollout_line_reader_with_limit` beside it. Drop only when both are available upstream; do not move decompression policy into the caller. | `thread_id_from_rollout_uses_metadata_at_header_line_limit`; `thread_id_from_rollout_stops_after_legacy_header_line_limit`; focused doctor inventory run with an oversized header. |
| `fix(protocol): accept models without a truncation policy` | Older catalog entries can omit only `truncation_policy`. `protocol/src/openai_models.rs` defaults that field to the existing 10,000-token policy and keeps other required fields required. | **Keep.** The base still rejects a missing policy. Drop only for the same narrow serde default and negative controls upstream. | `model_info_defaults_missing_truncation_policy_to_tokens`; `model_info_preserves_explicit_truncation_policy`; `model_info_still_rejects_other_missing_required_fields`. |
| `build: support OpenSSL 4 in the local source build` | The local native host provides OpenSSL 4.0.2. `codex-rs/Cargo.lock` moves `openssl` 0.10.75 to 0.10.81 and `openssl-sys` 0.9.111 to 0.9.117, because the old binding refuses OpenSSL 4. | **Keep locally.** Native OpenSSL 4 probes require these versions; OpenSSL 3 SDK test builds also remain supported. This is host build compatibility. Drop when upstream's lockfile accepts the host library or the host moves to a supported library. | Clean native-host probes must fail at `openssl-sys` 0.9.111 and pass at 0.9.117, including through the parent `openssl` crate. |
| `fix(core): preserve v2 compaction turn state` | Mid-turn compaction discarded the completed terminal tool transaction and selected skill instructions. The current V2 owner retains annotated skill fragments, restores the complete terminal call/output block, and checks item and full-context limits before advancing the window. Guardian retains its own complete-request budget and recovery path; identify that reviewer through `guardian::is_basic_session_source`, including the upstream extension session source. | **Keep.** Upstream V2 still loses both. Drop only when the same continuation keeps skills, the latest correction and the exact terminal transaction without duplication, and oversized replacements leave the previous window intact. | `remote_mid_turn_compact_v2_preserves_terminal_transaction_correction_and_skill` fails before this patch on the missing selected skill; parallel, metadata, duplicate, item-limit and context-limit cases are covered beside `compact_remote_tool_transaction.rs`. Existing `guardian_context_budget::review_respects_complete_context_budget` cases must keep passing. |
| `fix(core): clear inherited effort when an agent role changes model` | A role selecting a different model without an explicit reasoning effort inherited the parent effort, causing native spawn to reject non-effort OpenRouter routes before creating a child. The role now clears only that inherited effort; same-model roles and explicit effort pins keep their behavior. | **Keep locally.** Drop when upstream supplies the same role semantics. Provider, credentials and sandbox authority still inherit from the parent. | `apply_role_clears_inherited_effort_only_for_a_different_model` covers configured parent effort, model change, same-model preservation and explicit override. Native OpenRouter child/tool acceptance must also pass. |
| `fix(tui): make queued follow-ups visible to the active model turn` | TUI-local `queued_user_messages` were invisible while the current turn ran. The owners are `tui/src/chatwidget/input_*`, app-server's count-only `thread/queuedFollowupCount/update` path, and Core's incremental `QueuedFollowupAwareness` snapshots. It sends only the current count; durable `thread/queue/*` content and pending steers remain separate. | **Keep.** Drop only when upstream sends the latest TUI-local count to the active Core session without creating/steering/interrupting a turn, emits no queue contents, and clears stale positive state at zero or resume. | `queued_followup_count_tracks_queue_without_submitting_contents`; `queued_followup_awareness_is_count_only_and_does_not_start_a_turn`; app-server RPC roundtrip plus installed TUI probe: queue two messages during active work, confirm the next sample sees count two but no content, then clear/dequeue and confirm zero supersedes the prior notice without rewriting history. |
| `fix(core): fall back to local compaction when remote compaction v2 fails` | Remote compaction support is chosen by provider name (`model-provider/src/provider.rs`), so an OpenAI-compatible proxy named `OpenAI` that routes to third-party models (`openrouter/auto` through codex-lb) sends the `compaction_trigger` request; a `429 usage_limit_reached` or `400` from that path aborted the whole turn although the model itself was healthy. `core/src/compact_model_fallback.rs::should_fall_back_to_local_compaction` classifies model/provider-specific failures; `core/src/session/turn.rs::run_auto_compact` and `core/src/tasks/compact.rs` retry them with the summarization-prompt path, and `run_turn` names the implementation that actually failed. The provider gate itself is unchanged. | **Keep locally; upstream candidate.** Drop when upstream retries failed remote compaction locally for the auto and manual paths without emitting the remote error, or when remote-compaction support is decided by model/provider capability instead of the provider name. | `auto_compact_falls_back_to_local_when_remote_v2_hits_usage_limit`; `manual_compact_falls_back_to_local_when_remote_v2_hits_usage_limit` (single `TurnStarted`); negative control `auto_compact_reports_remote_v2_failures_that_do_not_fall_back` (`usage_not_included` keeps the remote error and issues no local request); predicate unit tests in `compact_model_fallback.rs`. Installed oracle: `model = "openrouter/auto"` via codex-lb with the ChatGPT pool exhausted, a long thread must compact locally and the turn must complete. |
| `fix(hooks): expose engine context accounting to Stop and PreCompact commands` | Command hooks previously received no live context accounting. `core/src/hook_runtime.rs` now passes the existing `ContextWindowTokenStatus` values, including the already-computed buffered auto-compact limit, into the `Stop` and `PreCompact` stdin payloads; `hooks/src/events/common.rs` owns the additive optional wire fields and generated input schemas. | **Keep locally; upstream candidate.** Drop when upstream command-hook payloads expose the same engine-owned active/scope/threshold/remaining figures for both `Stop` and `PreCompact`, with old payloads and no-op `{}` outputs remaining compatible. | `events::stop::tests::stop_input_includes_engine_context_usage`; `events::compact::tests::pre_compact_input_includes_lifecycle_metadata`; `events::common::tests::context_window_usage_accepts_payloads_without_new_fields`; `events::stop::tests::empty_stop_hook_output_changes_nothing`; generated hook schema fixture test. Installed oracle: isolated `CODEX_HOME` Stop hook capture must match the session's rollout `token_usage_record`. |
| `fix(hooks): let Stop hooks request one mid-turn compaction` | Stop hooks could judge that compaction was timely but had no control output that reached the engine. `hooks/src/schema.rs`, `hooks/src/events/stop.rs`, and `core/src/session/turn.rs` now carry `hookSpecificOutput.requestCompaction` through the existing control-effect gate and invoke the existing mid-turn `run_auto_compact` path once per turn with `CompactionReason::HookRequested`. The call deliberately falls through instead of continuing the turn loop: the hook asked for room, not for another sampling request, so a compaction-only request lets the turn end and a hook that also blocked gets its continuation injected below, in that order. | **Keep locally; upstream candidate.** Drop only when upstream Stop output accepts the same optional `hookSpecificOutput.requestCompaction` field and the engine performs one same-turn compaction while keeping `{}`, explicit `false`, and `async: true` non-operative. | `stop_hook_request_compaction_sets_control_effect`; `stop_hook_request_compaction_false_does_not_set_control_effect`; `async_stop_hook_request_compaction_does_not_set_control_effect`; `suite::hooks::stop_hook_request_compaction_runs_in_same_turn`, which mounts exactly one sampling response and asserts one assistant message and one request, so restarting the turn fails it. Installed oracle: isolated `CODEX_HOME` Stop hook returning `requestCompaction: true` must produce a compaction in that turn's rollout. |
| `fix(goals): refresh active objective after compaction` | Goal continuation was transient internal user context, removed during compaction; only a new turn restored it. The goal extension now contributes the current active goal from its canonical database through the existing context rebuilding API used by local and remote compaction. Complete, paused, and missing goals contribute nothing. | **Keep.** Drop when upstream restores the current goal before the next sample inside the same turn, without retaining stale internal messages or reviving inactive goals. | `goal_context_tracks_current_objective_and_status`; app-server `mid_turn_compaction_rehydrates_current_goal` for local and remote paths, including an older plan request and a completed-goal control. Installed oracle: capture the first post-compaction request from the actual binary and verify current-goal developer context. |
| `feat(goals): track staged deadlines and shared quota on every sample` | Goals retain UTC stage deadlines, timezone, delivery evidence and starting token/quota snapshots. The goal extension formats a fresh bounded reminder before each sampling request; native descendants inherit it read-only. The quota provider observes the same response quota as `/status`, without requiring a proxy change. An optional passive usage endpoint has a 60-second cache and keeps key allowance separate from pool headers. Stop/PreCompact hooks carry the same canonical facts to Jev. | **Keep.** Drop when upstream preserves this state through schedule edits, resume and compaction and supplies a fresh per-sample reminder with shared quota, without relying on transcript retention. `usage_url` enables the passive status endpoint for custom providers. Deadlines never stop a goal or move themselves; the explicit token budget still stops at its limit. | Goal extension schedule/backend suites; separate goals DB migration and accounting tests; provider scope/cache/failure tests; app-server local/remote compaction request capture; TUI schedule snapshots. Installed acceptance uses two isolated CLI processes with a shared fixture quota, tool follow-up, artifact delivery and resume. |

## Dropped upstream-superseded families

- **Remote environment test initializer:** dropped during the 2026-09-21
  rebase. Upstream `62ea6d41e8` already initializes the same
  `EnvironmentConfig` fields in `core/tests/suite/remote_env.rs`, so Git
  skipped local test-only commit `4c5f29b7f9`. Keep the upstream test rather
  than duplicating the initializer.

- **Linux sandbox mountinfo namespace roots:** external companion patch
  `67a5e7f920d4d4b4db43327f4560ac2b327a2fc3` is superseded by upstream
  `22dea110204c8a07abcbd6495d7eeae65faa0d9f` (#46535). Both parse a mount root
  only for the socket directory device, while every destination remains strict;
  upstream additionally covers nested namespace mounts and socket aliases. Do
  not cherry-pick the local patch.

- **Bounded malformed model catalogue diagnostics:** dropped on 2026-09-18.
  Upstream `977193486d` (#45928) makes `codex-api/src/endpoint/models.rs::list_models`
  report only the JSON error category, line, column and response byte count,
  and asserts it in `codex-api/tests/models_integration.rs::invalid_models_response_reports_bounded_decode_metadata`,
  including a 4096-repeat payload value that must not appear. Do not re-add
  the local `MAX_MODELS_DECODE_ERROR_DETAIL_BYTES` bound or its unit tests.

- **Static root/subagent and spawn usage hints:** dropped. Upstream generates hints from
  the configured concurrency limit and whether `wait_agent` is enabled. The
  old copied strings hid that guidance. Keep the upstream generator and its
  `multi_agent_v2_default_usage_hints_use_configured_thread_cap` regression;
  the local three-turn/model-inheritance contract stays in the tool schema and spawn handler.

- **Legacy remote compaction V1:** dropped. Upstream removed its V1 modules;
  restoring them would create a second implementation. The canonical remote
  path is `core/src/compact_remote_v2.rs` with
  `core/src/compact_remote_history.rs`.
- **Blanket compaction-image stripping:** dropped. The V2 image-budget path
  already defaults to charging retained images; its regression is
  `remote_compact_v2_charges_retained_images_to_token_budget` with the
  `default_trims_images` case. Do not re-add broad image removal outside that
  owner.

## Updating this stack

Use GitButler to keep this ledger and the implementation aligned:

```text
but pull
but status -fv
but diff <one-change-id>
but amend -t <commit-or-branch> <file-or-hunk-id>
but reword <commit-id> -m "new subject"
but push <top-branch>
```

Get file and hunk IDs from the current `but status -fv` or `but diff` output;
do not copy them from a previous review. Use `but pull` to rebase the applied
stack, resolve conflicts oldest first, and repeat the applicable source check
plus installed CLI oracle before declaring an entry superseded.

Generated artefacts in this stack are never hand-merged. The
`fix(agents)` entry changes the collaboration tool schema, so the
`core/tests/suite/snapshots/all__suite__scenarios__astra_*.snap` tool hashes
must be regenerated with `INSTA_UPDATE=always cargo test -p codex-core --test all scenarios::astra`
and `CARGO_BIN_EXE_codex-code-mode-host` pointing at a built host binary (without
it the code-mode scenarios record spawn failures instead of output). The
`fix(tui)` queued-follow-up entry adds an experimental app-server RPC, so
`app-server-protocol/schema/precomputed/app-server-exports-experimental.json.zst`
must be regenerated with `just write-app-server-schema --experimental` (or the
`schema_fixtures_tests::write_schema_fixtures_from_env` ignored test with
`CODEX_APP_SERVER_SCHEMA_EXPERIMENTAL=1`) and checked with
`cargo test -p codex-app-server-protocol --lib schema_fixtures_tests`.

Run scenario snapshots with an isolated process home and explicit Cargo and
Rustup cache paths. An isolated `CODEX_HOME` alone does not hide skills from
the operating-system home. Do not add local skill names to the test fixtures
or accept those skills into the snapshots.
