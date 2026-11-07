//! The `connection_probe` tool and the `ServerHandler` implementation.
//!
//! Schemas are hand-written constants and are the contract. Validation mirrors them exactly and
//! produces fixed, short tool-error messages so results stay bounded regardless of input.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Instant;

use rmcp::ErrorData as McpError;
use rmcp::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    JsonObject, ListToolsResult, PaginatedRequestParams, ProtocolVersion, ServerCapabilities,
    ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::{NotificationContext, RequestContext, RoleServer};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::diag::Event;
use crate::limits;
use crate::transport::{HandlerContext, Registry};

pub const SERVER_NAME: &str = "token-saver";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const TOOL_NAME: &str = "connection_probe";
pub const TOOL_TITLE: &str = "Connection probe";
pub const TOOL_DESCRIPTION: &str = "Confirms that the token-saver MCP server is reachable. Use this when asked to probe, test, or verify the token-saver connection. Pass a nonce of 1 to 64 ASCII letters or digits; the result echoes it with the server's instance_id, server_version, and status. It reads nothing and changes nothing.";
pub const INSTRUCTIONS: &str = "token-saver exposes one read-only tool, connection_probe. Call it with a nonce of 1 to 64 ASCII letters or digits. The result echoes the nonce and reports instance_id, server_version, and status. Use it only to confirm this server is reachable; it reads nothing and changes nothing.";
pub const SUPPORTED_VERSIONS_LABEL: &str = "2025-11-25,2026-07-28";

/// Asserted by tests to equal `limits::SUPPORTED_PROTOCOL_VERSIONS`.
pub fn supported_version_strings() -> Vec<&'static str> {
    SUPPORTED_VERSIONS.iter().map(|v| v.as_str()).collect()
}

static SUPPORTED_VERSIONS: [ProtocolVersion; 2] =
    [ProtocolVersion::V_2025_11_25, ProtocolVersion::V_2026_07_28];

pub const ERR_MISSING_NONCE: &str = "missing nonce";
pub const ERR_INVALID_NONCE: &str = "invalid nonce";
pub const ERR_UNEXPECTED_ARGUMENT: &str = "unexpected argument";
pub const ERR_ARGUMENTS_TOO_LARGE: &str = "arguments too large";
pub const ERR_INTERNAL: &str = "internal error";
pub const ERR_DEADLINE: &str = "processing deadline exceeded";
pub const ERR_CANCELLED: &str = "cancelled";

pub fn supported_versions() -> &'static [ProtocolVersion] {
    &SUPPORTED_VERSIONS
}

fn object(v: Value) -> Arc<JsonObject> {
    Arc::new(
        v.as_object()
            .cloned()
            .expect("schema constant is an object"),
    )
}

/// The input schema constant.
pub fn input_schema() -> Arc<JsonObject> {
    object(json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["nonce"],
        "properties": {
            "nonce": {
                "type": "string",
                "minLength": 1,
                "maxLength": 64,
                "pattern": "^[A-Za-z0-9]+$",
                "description": "Caller-chosen token of 1 to 64 ASCII letters or digits, echoed back verbatim."
            }
        }
    }))
}

/// The output schema constant.
pub fn output_schema() -> Arc<JsonObject> {
    object(json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["nonce", "instance_id", "server_version", "status"],
        "properties": {
            "nonce": { "type": "string", "minLength": 1, "maxLength": 64, "pattern": "^[A-Za-z0-9]+$" },
            "instance_id": { "type": "string", "pattern": "^[0-9a-f]{32}$" },
            "server_version": { "type": "string", "maxLength": 16 },
            "status": { "const": "ok" }
        }
    }))
}

/// The single tool definition.
pub fn tool() -> Tool {
    Tool::new(TOOL_NAME, TOOL_DESCRIPTION, input_schema())
        .with_title(TOOL_TITLE)
        .with_raw_output_schema(output_schema())
        .with_annotations(
            ToolAnnotations::new()
                .read_only(true)
                .destructive(false)
                .idempotent(true)
                .open_world(false),
        )
}

/// Nonce rule: 1 to 64 ASCII letters or digits.
pub fn valid_nonce(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// A fresh 32-character lowercase hexadecimal instance identifier.
pub fn new_instance_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn tool_error(message: &'static str) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message)])
}

/// Validates arguments and builds the result. Errors are fixed tool errors; `Err` carries the
/// serialized size of an oversized result, which the handler maps to a sanitized internal error.
pub fn evaluate(
    instance_id: &str,
    arguments: Option<&JsonObject>,
) -> Result<CallToolResult, usize> {
    let Some(args) = arguments else {
        return Ok(tool_error(ERR_MISSING_NONCE));
    };
    let serialized = serde_json::to_vec(args)
        .map(|v| v.len())
        .unwrap_or(usize::MAX);
    if serialized > limits::MAX_ARGUMENTS_BYTES {
        return Ok(tool_error(ERR_ARGUMENTS_TOO_LARGE));
    }
    if args.keys().any(|k| k != "nonce") {
        return Ok(tool_error(ERR_UNEXPECTED_ARGUMENT));
    }
    let nonce = match args.get("nonce") {
        None => return Ok(tool_error(ERR_MISSING_NONCE)),
        Some(Value::String(s)) if valid_nonce(s) => s.clone(),
        Some(_) => return Ok(tool_error(ERR_INVALID_NONCE)),
    };
    let value = json!({
        "nonce": nonce,
        "instance_id": instance_id,
        "server_version": VERSION,
        "status": "ok",
    });
    let result = CallToolResult::structured(value);
    let size = serde_json::to_vec(&result)
        .map(|v| v.len())
        .unwrap_or(usize::MAX);
    if size > limits::MAX_RESULT_BYTES {
        return Err(size);
    }
    Ok(result)
}

/// Runs handler work under the request token, the SDK token, and the processing deadline.
pub async fn run_with_deadline<F>(
    ctx: Option<HandlerContext>,
    sdk_ct: CancellationToken,
    work: F,
) -> Result<CallToolResult, McpError>
where
    F: Future<Output = Result<CallToolResult, McpError>>,
{
    let (token, deadline) = match ctx {
        Some(c) => (c.token, c.admit_at + limits::PROCESSING),
        None => (
            CancellationToken::new(),
            Instant::now() + limits::PROCESSING,
        ),
    };
    tokio::select! {
        biased;
        _ = token.cancelled() => Err(McpError::internal_error(ERR_CANCELLED, None)),
        _ = sdk_ct.cancelled() => Err(McpError::internal_error(ERR_CANCELLED, None)),
        _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
            Err(McpError::internal_error(ERR_DEADLINE, None))
        }
        r = work => r,
    }
}

/// The server handler.
#[derive(Clone)]
pub struct TokenSaver {
    registry: Arc<Registry>,
    instance_id: String,
}

impl TokenSaver {
    pub fn new(registry: Arc<Registry>, instance_id: String) -> TokenSaver {
        TokenSaver {
            registry,
            instance_id,
        }
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }
}

impl ServerHandler for TokenSaver {
    fn get_info(&self) -> ServerConfig {
        let mut config = ServerConfig::new(ServerCapabilities::builder().enable_tools().build());
        config.server_info = Implementation::new(SERVER_NAME, VERSION);
        config.instructions = Some(INSTRUCTIONS.to_owned());
        config
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(supported_versions())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(vec![tool()]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        if request.name != TOOL_NAME {
            return Err(McpError::invalid_params("tool not found", None));
        }
        let handler_ctx = self.registry.handler_context(&context.id);
        let instance_id = self.instance_id.clone();
        let diag = self.registry.diag().clone();
        let work = async move {
            match evaluate(&instance_id, request.arguments.as_ref()) {
                Ok(result) => Ok(result),
                Err(bytes) => {
                    diag.emit(Event::ResultOversized { bytes });
                    Err(McpError::internal_error(ERR_INTERNAL, None))
                }
            }
        };
        let result = run_with_deadline(handler_ctx, context.ct.clone(), work).await;
        self.registry.handler_finished(&context.id);
        result.map(Into::into)
    }

    async fn on_initialized(&self, _context: NotificationContext<RoleServer>) {
        self.registry.initialized_done();
    }
}
