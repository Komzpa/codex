use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::Mutex;
use tokio::sync::RwLock;
use tokio_util::either::Either;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;
use tracing::Instrument;
use tracing::instrument;
use tracing::trace_span;

use crate::function_tool::FunctionCallError;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::tools::context::AbortedToolOutput;
use crate::tools::context::SharedTurnDiffTracker;
use crate::tools::context::ToolPayload;
use crate::tools::registry::AnyToolResult;
use crate::tools::registry::ToolArgumentDiffConsumer;
use crate::tools::router::ToolCall;
use crate::tools::router::ToolCallSource;
use crate::tools::router::ToolRouter;
use codex_protocol::error::CodexErr;
use codex_protocol::models::ResponseInputItem;
use codex_tools::ToolSpec;

#[derive(Clone)]
pub(crate) struct ToolCallRuntime {
    router: Arc<ToolRouter>,
    session: Arc<Session>,
    turn_context: Arc<TurnContext>,
    tracker: SharedTurnDiffTracker,
    parallel_execution: Arc<RwLock<()>>,
    duplicate_tool_calls: Arc<Mutex<HashMap<String, Arc<Mutex<DuplicateToolCallGroup>>>>>,
}

#[derive(Default)]
struct DuplicateToolCallGroup {
    call_ids: Vec<String>,
    response: Option<ResponseInputItem>,
}

impl ToolCallRuntime {
    pub(crate) fn new(
        router: Arc<ToolRouter>,
        session: Arc<Session>,
        turn_context: Arc<TurnContext>,
        tracker: SharedTurnDiffTracker,
    ) -> Self {
        Self {
            router,
            session,
            turn_context,
            tracker,
            parallel_execution: Arc::new(RwLock::new(())),
            duplicate_tool_calls: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) fn find_spec(&self, tool_name: &codex_tools::ToolName) -> Option<ToolSpec> {
        self.router.find_spec(tool_name)
    }

    pub(crate) fn create_diff_consumer(
        &self,
        tool_name: &codex_tools::ToolName,
    ) -> Option<Box<dyn ToolArgumentDiffConsumer>> {
        self.router.create_diff_consumer(tool_name)
    }

    pub(crate) async fn clear_duplicate_tool_calls(&self) {
        self.duplicate_tool_calls.lock().await.clear();
    }

    #[instrument(level = "trace", skip_all)]
    pub(crate) fn handle_tool_call(
        self,
        call: ToolCall,
        cancellation_token: CancellationToken,
    ) -> impl std::future::Future<Output = Result<Vec<ResponseInputItem>, CodexErr>> {
        let error_call = call.clone();
        let dedupe_key = tool_call_dedupe_key(&call);
        let duplicate_tool_calls = Arc::clone(&self.duplicate_tool_calls);
        async move {
            let (duplicate_group, is_duplicate) = {
                let mut duplicate_tool_calls = duplicate_tool_calls.lock().await;
                if let Some(duplicate_group) = duplicate_tool_calls.get(&dedupe_key) {
                    (Arc::clone(duplicate_group), true)
                } else {
                    let duplicate_group = Arc::new(Mutex::new(DuplicateToolCallGroup {
                        call_ids: vec![error_call.call_id.clone()],
                        response: None,
                    }));
                    duplicate_tool_calls.insert(dedupe_key, Arc::clone(&duplicate_group));
                    (duplicate_group, false)
                }
            };
            if is_duplicate {
                return Ok(clone_duplicate_tool_output(
                    duplicate_group,
                    error_call.call_id.clone(),
                )
                .await);
            }

            let future =
                self.handle_tool_call_with_source(call, ToolCallSource::Direct, cancellation_token);
            match future.await {
                Ok(response) => Ok(expand_duplicate_tool_outputs(
                    response.into_response(),
                    duplicate_group,
                )
                .await),
                Err(FunctionCallError::Fatal(message)) => Err(CodexErr::Fatal(message)),
                Err(other) => Ok(expand_duplicate_tool_outputs(
                    Self::failure_response(error_call, other),
                    duplicate_group,
                )
                .await),
            }
        }
        .in_current_span()
    }

    #[instrument(level = "trace", skip_all)]
    pub(crate) fn handle_tool_call_with_source(
        self,
        call: ToolCall,
        source: ToolCallSource,
        cancellation_token: CancellationToken,
    ) -> impl std::future::Future<Output = Result<AnyToolResult, FunctionCallError>> {
        let supports_parallel = self.router.tool_supports_parallel(&call);
        let router = Arc::clone(&self.router);
        let session = Arc::clone(&self.session);
        let turn = Arc::clone(&self.turn_context);
        let tracker = Arc::clone(&self.tracker);
        let lock = Arc::clone(&self.parallel_execution);
        let invocation_cancellation_token = cancellation_token.clone();
        let started = Instant::now();
        let display_name = call.tool_name.display();

        let dispatch_span = trace_span!(
            "dispatch_tool_call_with_code_mode_result",
            otel.name = display_name.as_str(),
            tool_name = display_name.as_str(),
            call_id = call.call_id.as_str(),
            aborted = false,
        );

        let handle: AbortOnDropHandle<Result<AnyToolResult, FunctionCallError>> =
            AbortOnDropHandle::new(tokio::spawn(async move {
                tokio::select! {
                    _ = cancellation_token.cancelled() => {
                        let secs = started.elapsed().as_secs_f32().max(0.1);
                        dispatch_span.record("aborted", true);
                        Ok(Self::aborted_response(&call, secs))
                    },
                    res = async {
                        let _guard = if supports_parallel {
                            Either::Left(lock.read().await)
                        } else {
                            Either::Right(lock.write().await)
                        };

                        router
                            .dispatch_tool_call_with_code_mode_result(
                                session,
                                turn,
                                invocation_cancellation_token,
                                tracker,
                                call.clone(),
                                source,
                            )
                            .instrument(dispatch_span.clone())
                            .await
                    } => res,
                }
            }));

        async move {
            handle.await.map_err(|err| {
                FunctionCallError::Fatal(format!("tool task failed to receive: {err:?}"))
            })?
        }
        .in_current_span()
    }
}

fn tool_call_dedupe_key(call: &ToolCall) -> String {
    let payload_key = match &call.payload {
        ToolPayload::Function { arguments } => format!("function:{arguments}"),
        ToolPayload::ToolSearch { arguments } => serde_json::json!({
            "kind": "tool_search",
            "query": arguments.query,
            "limit": arguments.limit,
        })
        .to_string(),
        ToolPayload::Custom { input } => format!("custom:{input}"),
        ToolPayload::LocalShell { params } => format!("local_shell:{params:?}"),
        ToolPayload::Mcp {
            server,
            tool,
            raw_arguments,
        } => serde_json::json!({
            "kind": "mcp",
            "server": server,
            "tool": tool,
            "raw_arguments": raw_arguments,
        })
        .to_string(),
    };
    format!("{}:{payload_key}", call.tool_name)
}

async fn expand_duplicate_tool_outputs(
    response: ResponseInputItem,
    duplicate_group: Arc<Mutex<DuplicateToolCallGroup>>,
) -> Vec<ResponseInputItem> {
    let mut duplicate_group = duplicate_group.lock().await;
    duplicate_group.response = Some(response.clone());
    duplicate_group
        .call_ids
        .iter()
        .map(|call_id| response_with_call_id(&response, call_id.clone()))
        .collect()
}

async fn clone_duplicate_tool_output(
    duplicate_group: Arc<Mutex<DuplicateToolCallGroup>>,
    call_id: String,
) -> Vec<ResponseInputItem> {
    let mut duplicate_group = duplicate_group.lock().await;
    if let Some(response) = duplicate_group.response.as_ref() {
        vec![response_with_call_id(response, call_id)]
    } else {
        duplicate_group.call_ids.push(call_id);
        Vec::new()
    }
}

fn response_with_call_id(response: &ResponseInputItem, call_id: String) -> ResponseInputItem {
    match response.clone() {
        ResponseInputItem::FunctionCallOutput { output, .. } => {
            ResponseInputItem::FunctionCallOutput { call_id, output }
        }
        ResponseInputItem::McpToolCallOutput { output, .. } => {
            ResponseInputItem::McpToolCallOutput { call_id, output }
        }
        ResponseInputItem::CustomToolCallOutput { name, output, .. } => {
            ResponseInputItem::CustomToolCallOutput {
                call_id,
                name,
                output,
            }
        }
        ResponseInputItem::ToolSearchOutput {
            status,
            execution,
            tools,
            ..
        } => ResponseInputItem::ToolSearchOutput {
            call_id,
            status,
            execution,
            tools,
        },
        ResponseInputItem::Message { .. } => response.clone(),
    }
}

#[cfg(test)]
mod tests {
    use codex_protocol::models::FunctionCallOutputBody;
    use codex_protocol::models::FunctionCallOutputPayload;
    use pretty_assertions::assert_eq;

    use super::*;

    #[tokio::test]
    async fn expands_tool_output_to_every_duplicate_call_id() {
        let duplicate_group = Arc::new(Mutex::new(DuplicateToolCallGroup {
            call_ids: vec!["call_original".to_string(), "call_duplicate".to_string()],
            response: None,
        }));
        let response = ResponseInputItem::FunctionCallOutput {
            call_id: "call_original".to_string(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("done".to_string()),
                success: Some(true),
            },
        };

        let expanded = expand_duplicate_tool_outputs(response, duplicate_group).await;

        assert_eq!(
            expanded,
            vec![
                ResponseInputItem::FunctionCallOutput {
                    call_id: "call_original".to_string(),
                    output: FunctionCallOutputPayload {
                        body: FunctionCallOutputBody::Text("done".to_string()),
                        success: Some(true),
                    },
                },
                ResponseInputItem::FunctionCallOutput {
                    call_id: "call_duplicate".to_string(),
                    output: FunctionCallOutputPayload {
                        body: FunctionCallOutputBody::Text("done".to_string()),
                        success: Some(true),
                    },
                },
            ]
        );
    }

    #[tokio::test]
    async fn late_duplicate_gets_cached_tool_output() {
        let duplicate_group = Arc::new(Mutex::new(DuplicateToolCallGroup {
            call_ids: vec!["call_original".to_string()],
            response: None,
        }));
        let response = ResponseInputItem::FunctionCallOutput {
            call_id: "call_original".to_string(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("done".to_string()),
                success: Some(true),
            },
        };
        let _ = expand_duplicate_tool_outputs(response, Arc::clone(&duplicate_group)).await;

        let cloned =
            clone_duplicate_tool_output(duplicate_group, "call_duplicate".to_string()).await;

        assert_eq!(
            cloned,
            vec![ResponseInputItem::FunctionCallOutput {
                call_id: "call_duplicate".to_string(),
                output: FunctionCallOutputPayload {
                    body: FunctionCallOutputBody::Text("done".to_string()),
                    success: Some(true),
                },
            }]
        );
    }
}

impl ToolCallRuntime {
    fn failure_response(call: ToolCall, err: FunctionCallError) -> ResponseInputItem {
        let message = err.to_string();
        match call.payload {
            ToolPayload::ToolSearch { .. } => ResponseInputItem::ToolSearchOutput {
                call_id: call.call_id,
                status: "completed".to_string(),
                execution: "client".to_string(),
                tools: Vec::new(),
            },
            ToolPayload::Custom { .. } => ResponseInputItem::CustomToolCallOutput {
                call_id: call.call_id,
                name: None,
                output: codex_protocol::models::FunctionCallOutputPayload {
                    body: codex_protocol::models::FunctionCallOutputBody::Text(message),
                    success: Some(false),
                },
            },
            _ => ResponseInputItem::FunctionCallOutput {
                call_id: call.call_id,
                output: codex_protocol::models::FunctionCallOutputPayload {
                    body: codex_protocol::models::FunctionCallOutputBody::Text(message),
                    success: Some(false),
                },
            },
        }
    }

    fn aborted_response(call: &ToolCall, secs: f32) -> AnyToolResult {
        AnyToolResult {
            call_id: call.call_id.clone(),
            payload: call.payload.clone(),
            result: Box::new(AbortedToolOutput {
                message: Self::abort_message(call, secs),
            }),
            post_tool_use_payload: None,
        }
    }

    fn abort_message(call: &ToolCall, secs: f32) -> String {
        if call.tool_name.namespace.is_none()
            && matches!(
                call.tool_name.name.as_str(),
                "shell" | "container.exec" | "local_shell" | "shell_command" | "unified_exec"
            )
        {
            format!("Wall time: {secs:.1} seconds\naborted by user")
        } else {
            format!("aborted by user after {secs:.1}s")
        }
    }
}
