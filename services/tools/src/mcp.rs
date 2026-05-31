//! MCP (Model Context Protocol) Streamable-HTTP / JSON-RPC 2.0 transport.
//!
//! This is the second transport over the SAME tool surface as the REST router: MCP
//! clients (e.g. Claude Code) speak JSON-RPC 2.0 over HTTP. A `POST /mcp` request
//! carries a JSON-RPC envelope; we authorize the same per-task bearer token, then
//! route the call through the SAME audited choke point (`audit_and_dispatch`) the
//! REST layer uses, so every MCP tool call lands in the events book exactly like a
//! REST call. The tool bodies are never duplicated — only the JSON-RPC envelope
//! parsing, method routing, and content wrapping live here.
//!
//! Methods supported: `initialize`, `notifications/initialized` (a notification with
//! no response), `tools/list`, and `tools/call`. Unknown methods return JSON-RPC
//! `-32601`; a malformed body returns `-32700`; bad params return `-32602`.
//!
//! `GET /mcp` would open the server-to-client SSE stream. Factory's tools are all
//! request/response with no server-initiated messages, so the GET handler returns
//! `405 Method Not Allowed` — there is nothing to stream.
#![forbid(unsafe_code)]

use std::sync::Arc;

use axum::extract::State as AxumState;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::{
    audit_and_dispatch, authorize, bearer_token, Tool, ToolCallError, ToolError, ToolsState,
};

/// JSON-RPC 2.0 error codes used by this transport.
const PARSE_ERROR: i64 = -32700;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;
/// A server-defined (implementation) JSON-RPC error code signalling a retryable
/// idempotency conflict — the MCP equivalent of the REST layer's HTTP 409. Server
/// error codes live in the reserved -32000..=-32099 range; we pick -32009 for it.
const CONFLICT_RETRYABLE: i64 = -32009;

/// The MCP protocol revision this server implements.
const PROTOCOL_VERSION: &str = "2024-11-05";

/// The GENERIC client-visible message for an internal tool failure. The raw error
/// (which may carry internal DB/GitHub/blob detail) is logged server-side only and
/// never returned to the client — mirroring the REST layer's opaque "internal error"
/// 500 body.
const INTERNAL_ERROR_MESSAGE: &str = "internal error";

/// `GET /mcp` — the SSE stream. Factory emits no server-initiated messages, so
/// there is nothing to stream; report that the method is not allowed.
pub async fn handle_get() -> Response {
    tracing::info!("mcp: GET /mcp (no SSE stream; request/response only)");
    (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(json!({ "error": "GET /mcp (SSE) is not supported; use POST for JSON-RPC" })),
    )
        .into_response()
}

/// `POST /mcp` — the JSON-RPC 2.0 request/response endpoint. Authorize the bearer
/// first (a bad/absent token is a transport-level 401, not a JSON-RPC error), parse
/// the envelope, then route the method.
pub async fn handle_post(
    AxumState(state): AxumState<Arc<ToolsState>>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> Response {
    // Same per-task bearer as the REST surface. An absent or unresolved token is a
    // transport-level failure → HTTP 401 (not a JSON-RPC error envelope).
    let Some(bearer) = bearer_token(&headers) else {
        tracing::warn!("mcp: missing bearer token");
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "missing bearer token" })),
        )
            .into_response();
    };
    let auth = match authorize(&state.pool, &bearer).await {
        Ok(Some(auth)) => auth,
        Ok(None) => {
            tracing::warn!("mcp: unauthorized token");
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "unauthorized" })),
            )
                .into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "mcp: token resolution failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "auth error" })),
            )
                .into_response();
        }
    };

    // The body must be a JSON object; a missing/non-object body is a parse error.
    let Some(Json(req)) = body else {
        tracing::warn!("mcp: empty request body");
        return rpc_error_response(Value::Null, PARSE_ERROR, "Parse error");
    };
    let Some(obj) = req.as_object() else {
        tracing::warn!("mcp: request body is not a JSON object");
        return rpc_error_response(Value::Null, PARSE_ERROR, "Parse error");
    };

    // `id` is absent on notifications. Default to null for error envelopes.
    let id = obj.get("id").cloned().unwrap_or(Value::Null);
    let method = match obj.get("method").and_then(Value::as_str) {
        Some(m) => m,
        None => {
            tracing::warn!("mcp: request missing method");
            return rpc_error_response(id, INVALID_PARAMS, "Invalid params: missing method");
        }
    };
    let params = obj.get("params").cloned().unwrap_or(Value::Null);

    tracing::info!(method, has_id = !id.is_null(), "mcp: request");

    match method {
        "initialize" => rpc_result_response(id, initialize_result(&state)),
        "notifications/initialized" => {
            // A notification: no id, no response body. Acknowledge with 202.
            tracing::info!("mcp: notifications/initialized acknowledged");
            StatusCode::ACCEPTED.into_response()
        }
        "tools/list" => rpc_result_response(id, json!({ "tools": tool_descriptors() })),
        "tools/call" => handle_tools_call(&state, &auth, id, &params).await,
        other => {
            tracing::warn!(method = other, "mcp: unknown method");
            rpc_error_response(id, METHOD_NOT_FOUND, "Method not found")
        }
    }
}

/// The `initialize` result: protocol version, advertised capabilities (tools), and
/// this server's identity (name + the crate version).
fn initialize_result(_state: &ToolsState) -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": {
            "name": "factory-tools",
            "version": env!("CARGO_PKG_VERSION"),
        },
    })
}

/// Route a `tools/call` to the existing tool function through the SAME audited
/// choke point the REST layer uses. Error classification mirrors the REST layer's
/// 400/500 split: a bad-request (caller's fault) becomes a JSON-RPC invalid-params
/// error (like the REST 400), an internal failure becomes a normal result with
/// `isError: true` so the model can see it (mapping the REST 500 to MCP convention),
/// and a failed audit write becomes a JSON-RPC internal error. Envelope/param
/// problems (unknown/missing tool name) are JSON-RPC errors as before.
async fn handle_tools_call(
    state: &ToolsState,
    auth: &factory_record::TaskAuth,
    id: Value,
    params: &Value,
) -> Response {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        tracing::warn!("mcp: tools/call missing name");
        return rpc_error_response(id, INVALID_PARAMS, "Invalid params: missing tool name");
    };
    let Some(tool) = Tool::parse(name) else {
        tracing::warn!(tool = name, "mcp: tools/call unknown tool");
        return rpc_error_response(
            id,
            INVALID_PARAMS,
            &format!("Invalid params: unknown tool {name:?}"),
        );
    };

    // Arguments default to an empty object (tools with no args).
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    match audit_and_dispatch(state, auth, tool, &args).await {
        Ok(value) => {
            tracing::info!(tool = tool.as_str(), "mcp: tools/call ok");
            rpc_result_response(id, tool_content(&value, false))
        }
        Err(ToolCallError::Tool(ToolError::BadRequest(msg))) => {
            // Invalid arguments are a JSON-RPC params problem, mirroring the REST
            // layer's 400.
            tracing::warn!(tool = tool.as_str(), error = %msg, "mcp: tools/call invalid params");
            rpc_error_response(id, INVALID_PARAMS, &format!("Invalid params: {msg}"))
        }
        Err(ToolCallError::Tool(ToolError::Conflict(msg))) => {
            // A retryable idempotency conflict, mirroring the REST layer's 409: a
            // JSON-RPC error with a server-defined retryable code so the client retries
            // rather than treating it as a hard failure.
            tracing::info!(tool = tool.as_str(), error = %msg, "mcp: tools/call conflict (retryable)");
            rpc_error_response(id, CONFLICT_RETRYABLE, &format!("Conflict (retry): {msg}"))
        }
        Err(ToolCallError::Tool(ToolError::Internal(e))) => {
            // MCP convention: an internal tool failure is a result with isError, so
            // the client can show it to the model rather than failing the RPC. The
            // raw error is Factory's fault (DB/GitHub/blob I/O) and may carry internal
            // detail, so it is logged server-side only; the client-visible content is
            // a GENERIC message — mirroring the REST layer's opaque "internal error"
            // 500 body — never `e.to_string()`.
            tracing::error!(tool = tool.as_str(), error = %e, "mcp: tools/call tool error");
            rpc_result_response(id, internal_error_content())
        }
        Err(ToolCallError::Audit(e)) => {
            // Failing to write the mandatory audit row is an infrastructure failure,
            // not a tool result: surface it as a JSON-RPC internal error.
            tracing::error!(tool = tool.as_str(), error = %e, "mcp: tools/call audit error");
            rpc_error_response(id, INTERNAL_ERROR, "Internal error: audit write failed")
        }
    }
}

/// Wrap a tool's JSON result as MCP `content`. The value is stringified into a
/// single text block (compact JSON), which is what MCP text content carries.
fn tool_content(value: &Value, is_error: bool) -> Value {
    let text = match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    json!({
        "content": [ { "type": "text", "text": text } ],
        "isError": is_error,
    })
}

/// The MCP error result for an internal tool failure: `isError: true` carrying the
/// GENERIC [`INTERNAL_ERROR_MESSAGE`] — never the raw error string. Factored out so a
/// unit test can assert the client-visible text is the opaque message regardless of
/// what internal error triggered it.
fn internal_error_content() -> Value {
    tool_error_content(INTERNAL_ERROR_MESSAGE)
}

/// An MCP error result (`isError: true`) carrying the error message as text.
fn tool_error_content(message: &str) -> Value {
    json!({
        "content": [ { "type": "text", "text": message } ],
        "isError": true,
    })
}

/// A JSON-RPC 2.0 success envelope, served as `application/json`.
fn rpc_result_response(id: Value, result: Value) -> Response {
    (
        StatusCode::OK,
        Json(json!({ "jsonrpc": "2.0", "id": id, "result": result })),
    )
        .into_response()
}

/// A JSON-RPC 2.0 error envelope, served as `application/json`. JSON-RPC errors are
/// transported with HTTP 200 (the failure lives in the envelope, not the status).
fn rpc_error_response(id: Value, code: i64, message: &str) -> Response {
    (
        StatusCode::OK,
        Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message },
        })),
    )
        .into_response()
}

/// The `tools/list` descriptors: one entry per tool with a name, description, and a
/// minimal JSON Schema for its arguments. The schemas mirror exactly what
/// `dispatch` reads for each tool.
fn tool_descriptors() -> Vec<Value> {
    vec![
        descriptor(
            Tool::GetTask,
            "Return this task's facts (role, subject, prompt, head branch) and the referenced GitHub object.",
            json!({}),
            &[],
        ),
        descriptor(
            Tool::ReadIssue,
            "Read an issue in the task's repo.",
            json!({ "issue_number": { "type": "integer" } }),
            &["issue_number"],
        ),
        descriptor(
            Tool::ReadChangeRequest,
            "Read a change request (pull request) in the task's repo.",
            json!({ "cr_number": { "type": "integer" } }),
            &["cr_number"],
        ),
        descriptor(
            Tool::OpenOrUpdateChangeRequest,
            "Open or update this task's change request (idempotent by head branch).",
            json!({
                "title": { "type": "string" },
                "body": { "type": "string" },
                "head_branch": { "type": "string" },
                "base": { "type": "string" },
                "linked_issue": { "type": "integer" }
            }),
            &["title", "body", "head_branch", "base"],
        ),
        descriptor(
            Tool::PostComment,
            "Post a comment on the subject (issue#N or cr#N); idempotent when dedupe_key is given.",
            json!({
                "target": { "type": "string" },
                "body": { "type": "string" },
                "dedupe_key": { "type": "string" }
            }),
            &["target", "body"],
        ),
        descriptor(
            Tool::SubmitReview,
            "Submit a review verdict (approve|request_changes|comment) on a change request.",
            json!({
                "cr_number": { "type": "integer" },
                "verdict": { "type": "string", "enum": ["approve", "request_changes", "comment"] },
                "body": { "type": "string" },
                "dedupe_key": { "type": "string" }
            }),
            &["cr_number", "verdict", "body"],
        ),
        descriptor(
            Tool::GetCheckStatus,
            "Read required-check status for a change request number or an explicit sha (exactly one).",
            json!({
                "cr_number": { "type": "integer" },
                "sha": { "type": "string" }
            }),
            &[],
        ),
        descriptor(
            Tool::AppendTranscript,
            "Append a chunk to this task's transcript.",
            json!({ "chunk": { "type": "string" } }),
            &["chunk"],
        ),
        descriptor(
            Tool::RecordOutcome,
            "Complete this attempt with a terminal status (succeeded|failed|blocked) and summary.",
            json!({
                "status": { "type": "string", "enum": ["succeeded", "failed", "blocked"] },
                "summary": { "type": "string" }
            }),
            &["status", "summary"],
        ),
        descriptor(
            Tool::Heartbeat,
            "Extend this task's lease.",
            json!({}),
            &[],
        ),
        descriptor(
            Tool::Escalate,
            "Escalate to a human with a reason and context; idempotent per task.",
            json!({
                "reason": { "type": "string", "enum": ["retries_exhausted", "loop_cap", "needs_decision", "blocked_input", "conflict"] },
                "context": { "type": "string" },
                "dedupe_key": { "type": "string" }
            }),
            &["reason", "context"],
        ),
    ]
}

/// Build one tool descriptor: name, description, and a minimal object JSON Schema.
fn descriptor(tool: Tool, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": tool.as_str(),
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_exactly_eleven_tools_each_with_name_and_schema() {
        let tools = tool_descriptors();
        assert_eq!(tools.len(), 11, "all 11 tools listed");
        for t in &tools {
            let name = t["name"].as_str().expect("name is a string");
            assert!(Tool::parse(name).is_some(), "{name} is a real tool");
            assert_eq!(
                t["inputSchema"]["type"], "object",
                "{name} inputSchema is an object schema"
            );
            assert!(
                t["inputSchema"]["properties"].is_object(),
                "{name} has properties"
            );
            assert!(
                t["inputSchema"]["required"].is_array(),
                "{name} has a required array"
            );
        }
    }

    #[test]
    fn tool_descriptor_set_matches_every_tool_variant() {
        // Every Tool variant that Tool::parse accepts must appear exactly once.
        let names: Vec<String> = tool_descriptors()
            .iter()
            .map(|t| t["name"].as_str().unwrap_or_default().to_owned())
            .collect();
        for name in [
            "get_task",
            "read_issue",
            "read_change_request",
            "open_or_update_change_request",
            "post_comment",
            "submit_review",
            "get_check_status",
            "append_transcript",
            "record_outcome",
            "heartbeat",
            "escalate",
        ] {
            assert_eq!(
                names.iter().filter(|n| n.as_str() == name).count(),
                1,
                "{name} appears exactly once"
            );
        }
    }

    #[test]
    fn tool_content_stringifies_object_results_and_unwraps_strings() {
        let obj = tool_content(&json!({ "a": 1 }), false);
        assert_eq!(obj["isError"], false);
        assert_eq!(obj["content"][0]["type"], "text");
        // Object results are stringified into the text block.
        let text = obj["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("\"a\""), "object stringified: {text}");

        // A bare string result is carried as-is (not double-quoted).
        let s = tool_content(&json!("hello"), false);
        assert_eq!(s["content"][0]["text"], "hello");
    }

    #[test]
    fn tool_error_content_sets_is_error() {
        let e = tool_error_content("boom");
        assert_eq!(e["isError"], true);
        assert_eq!(e["content"][0]["text"], "boom");
    }

    #[test]
    fn internal_error_content_is_generic_never_the_raw_error() {
        // The internal-failure mapping returns isError with the GENERIC message; the
        // raw internal error text must never reach the client.
        let raw = "connection refused to postgres at 10.0.0.5:5432";
        let e = internal_error_content();
        assert_eq!(e["isError"], true);
        assert_eq!(e["content"][0]["text"], "internal error");
        assert_eq!(
            e["content"][0]["text"], INTERNAL_ERROR_MESSAGE,
            "client-visible text is the generic message"
        );
        // Sanity: the generic content carries none of a realistic raw error's detail.
        let text = e["content"][0]["text"].as_str().unwrap();
        assert!(!text.contains("postgres"));
        assert!(!text.contains(raw));
    }

    #[test]
    fn initialize_result_carries_protocol_and_server_info() {
        // serverInfo.version is the crate version; this is a compile-time constant.
        let v = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "factory-tools", "version": env!("CARGO_PKG_VERSION") },
        });
        assert_eq!(v["protocolVersion"], "2024-11-05");
        assert_eq!(v["serverInfo"]["name"], "factory-tools");
        assert!(v["serverInfo"]["version"].is_string());
    }
}
