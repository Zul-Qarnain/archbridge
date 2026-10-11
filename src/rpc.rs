use crate::doctor::run_doctor;
use crate::engine::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{self, BufRead, Write};

/// Largest request frame the server accepts, in bytes (see docs/IPC.md).
pub const MAX_FRAME_BYTES: usize = 1_048_576;

/// JSON-RPC 2.0 reserved codes plus the server-defined operational code.
pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
/// Engine/operational failure: the request was well formed and the action could
/// not be carried out.
pub const SERVER_ERROR: i32 = -32000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
}

impl JsonRpcError {
    fn parse(message: impl Into<String>) -> Self {
        Self {
            code: PARSE_ERROR,
            message: message.into(),
        }
    }
    fn invalid_request(message: impl Into<String>) -> Self {
        Self {
            code: INVALID_REQUEST,
            message: message.into(),
        }
    }
    fn method_not_found(message: impl Into<String>) -> Self {
        Self {
            code: METHOD_NOT_FOUND,
            message: message.into(),
        }
    }
    fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: INVALID_PARAMS,
            message: message.into(),
        }
    }
    fn server(message: impl Into<String>) -> Self {
        Self {
            code: SERVER_ERROR,
            message: message.into(),
        }
    }
}

/// A decoded request and whether the wire omitted `id`, which makes it a
/// notification: executed, but never answered.
#[derive(Debug, Clone)]
struct Decoded {
    request: JsonRpcRequest,
    notification: bool,
}

/// What one bounded read of the transport produced.
enum Frame {
    Line(Vec<u8>),
    /// A line longer than [`MAX_FRAME_BYTES`], already drained to its newline.
    Oversized,
    Eof,
}

pub struct RpcServer {
    pub engine: Engine,
}

impl Default for RpcServer {
    fn default() -> Self {
        Self::new()
    }
}

impl RpcServer {
    pub fn new() -> Self {
        Self {
            engine: Engine::new(),
        }
    }

    /// A server bound to a preferences file at `config_path` rather than the
    /// user's real one, so tests cannot rewrite discovery settings.
    pub fn with_config_path(config_path: &std::path::Path) -> Self {
        Self {
            engine: Engine::with_config_path(config_path),
        }
    }

    /// Answer one request object directly, for in-process clients (the CLI and
    /// the GUI). The notification rule — a request with no `id` is executed but
    /// never answered — belongs to the wire layer, see [`RpcServer::handle_frame`].
    pub fn handle_request(&mut self, req: JsonRpcRequest) -> JsonRpcResponse {
        let outcome = self.evaluate(&req);
        Self::response(req.id.clone(), outcome)
    }

    fn response(id: Option<Value>, outcome: Result<Value, JsonRpcError>) -> JsonRpcResponse {
        let (result, error) = match outcome {
            Ok(value) => (Some(value), None),
            Err(error) => (None, Some(error)),
        };
        JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            // An unknown id is reported as null, which is what a client can
            // still correlate against; it is never omitted.
            id: id.unwrap_or(Value::Null),
            result,
            error,
        }
    }

    /// Execute one request for its `result`/`error` pair, with no framing.
    fn evaluate(&mut self, req: &JsonRpcRequest) -> Result<Value, JsonRpcError> {
        if req.jsonrpc != "2.0" {
            return Err(JsonRpcError::invalid_request(
                "Invalid Request: jsonrpc must be '2.0'",
            ));
        }
        // Every method here addresses its inputs by name, so an array or scalar
        // `params` can never mean what the caller intended.
        if !req.params.is_null() && !req.params.is_object() {
            return Err(JsonRpcError::invalid_params(format!(
                "Invalid params: 'params' must be a JSON object, got {}",
                json_type(&req.params)
            )));
        }

        let phase_label = match req.method.as_str() {
            "v1.search" | "v1.info" => Some("Discovering sources"),
            "v1.inspect" => Some("Inspecting foreign package"),
            "v1.doctor" => Some("Running system diagnostics"),
            "v1.authenticate" => Some("Authenticating administrator session"),
            "v1.prepare" => Some("Preparing plan"),
            _ => None,
        };
        if let Some(label) = phase_label {
            crate::process::begin_phase(label, 0);
        }

        let res = self.dispatch(req);

        if phase_label.is_some() {
            crate::process::end_phase();
        }
        res
    }

    fn dispatch(&mut self, req: &JsonRpcRequest) -> Result<Value, JsonRpcError> {
        let params = &req.params;
        match req.method.as_str() {
            "v1.capabilities" => Ok(serde_json::json!({
                "protocol": "v1.0",
                "methods": ["v1.capabilities", "v1.search", "v1.info", "v1.inspect", "v1.doctor", "v1.config.get", "v1.authenticate", "v1.deauthenticate", "v1.sudo_advice", "v1.optimize_sudo_auth", "v1.prepare", "v1.execute"],
                "chroot_build": true,
                "runtime_smoke_test": true,
                "foreign_script_execution": false,
                "notifications": true,
                "batch": true,
                "max_request_bytes": MAX_FRAME_BYTES
            })),
            "v1.search" | "v1.info" => {
                let target = require_str(params, "target", "the package or path to look for")?;
                let repo = optional_str(params, "repo")?;
                let plan = self
                    .engine
                    .prepare_search(target, repo.as_deref())
                    .map_err(JsonRpcError::server)?;
                serde_json::to_value(plan.decision).map_err(|e| JsonRpcError::server(e.to_string()))
            }
            "v1.inspect" => {
                let target = require_str(params, "target", "the .deb or .rpm to inspect")?;
                // v1.inspect is the read-only review path, so it answers with the
                // report itself (the findings a human reads). The staged plan is
                // returned by `v1.prepare` with action "inspect" instead.
                let (_, report) = self
                    .engine
                    .prepare_inspect(target)
                    .map_err(JsonRpcError::server)?;
                serde_json::to_value(report).map_err(|e| JsonRpcError::server(e.to_string()))
            }
            "v1.doctor" => {
                let doc = run_doctor();
                serde_json::to_value(doc).map_err(|e| JsonRpcError::server(e.to_string()))
            }
            "v1.authenticate" => {
                let password = require_str(params, "password", "the administrator password")?;
                self.engine
                    .authenticate(password)
                    .map(|_| serde_json::json!({"authenticated": true}))
                    .map_err(JsonRpcError::server)
            }
            "v1.deauthenticate" => self
                .engine
                .deauthenticate()
                .map(|_| serde_json::json!({"authenticated": false}))
                .map_err(JsonRpcError::server),
            "v1.sudo_advice" => Ok(serde_json::json!({
                "fingerprint_before_password": crate::engine::Engine::fingerprint_before_password()
            })),
            "v1.optimize_sudo_auth" => {
                let password = require_str(params, "password", "the administrator password")?;
                crate::engine::Engine::optimize_sudo_auth(password)
                    .map(|message| {
                        serde_json::json!({
                            "message": message,
                            "fingerprint_before_password": crate::engine::Engine::fingerprint_before_password()
                        })
                    })
                    .map_err(JsonRpcError::server)
            }
            "v1.config.get" => {
                let key = match params.get("key") {
                    None | Some(Value::Null) => "all",
                    Some(Value::String(key)) => key.as_str(),
                    Some(other) => {
                        return Err(JsonRpcError::invalid_params(format!(
                            "Invalid params: 'key' must be a string, got {}",
                            json_type(other)
                        )))
                    }
                };
                // An unreadable preferences file is the server's problem, not the
                // caller's parameter: -32602 would send them editing a request
                // that was already correct.
                if let Some(error) = self.engine.config_error() {
                    return Err(JsonRpcError::server(error.to_string()));
                }
                self.engine
                    .config_get(key)
                    .map_err(JsonRpcError::invalid_params)
            }
            "v1.prepare" => self.prepare_plan(params).and_then(|p| {
                serde_json::to_value(p).map_err(|e| JsonRpcError::server(e.to_string()))
            }),
            "v1.execute" => {
                let plan_id = require_str(params, "plan_id", "the plan id from v1.prepare")?;
                let confirmed = optional_bool(params, "confirmed")?.unwrap_or(false);
                self.engine
                    .execute_plan(plan_id, confirmed)
                    .map_err(JsonRpcError::server)
                    .and_then(|res| {
                        serde_json::to_value(res).map_err(|e| JsonRpcError::server(e.to_string()))
                    })
            }
            _ => Err(JsonRpcError::method_not_found(format!(
                "Method not found: '{}'",
                req.method
            ))),
        }
    }

    /// `v1.prepare` accepts its request fields either nested under `request`
    /// (the documented shape) or directly on `params`, which is what the CLI and
    /// older clients send. `options` is likewise read in both places, so a client
    /// can never be silently ignored because it chose the other shape.
    fn prepare_plan(&mut self, params: &Value) -> Result<crate::engine::Plan, JsonRpcError> {
        let action = require_str(params, "action", "the plan action")?;
        let mut carriers: Vec<&Value> = Vec::new();
        if let Some(request @ Value::Object(_)) = params.get("request") {
            carriers.push(request);
            push_options(&mut carriers, request)?;
        } else if let Some(other) = params.get("request") {
            if !other.is_null() {
                return Err(JsonRpcError::invalid_params(format!(
                    "Invalid params: 'request' must be a JSON object, got {}",
                    json_type(other)
                )));
            }
        }
        carriers.push(params);
        push_options(&mut carriers, params)?;

        // `request` wins, then its `options`, then the top level.
        let find = |key: &str| {
            carriers
                .iter()
                .filter_map(|carrier| carrier.get(key))
                .find(|value| !value.is_null())
                .cloned()
                .unwrap_or(Value::Null)
        };
        let str_field = |key: &str| -> Result<Option<String>, JsonRpcError> {
            match find(key) {
                Value::Null => Ok(None),
                Value::String(s) => Ok(Some(s)),
                other => Err(JsonRpcError::invalid_params(format!(
                    "Invalid params: '{key}' must be a string, got {}",
                    json_type(&other)
                ))),
            }
        };

        let target = str_field("target")?;
        let name = str_field("name")?;
        let version = str_field("version")?;
        let entry = str_field("entry")?;
        let repo = str_field("repo")?;
        let dependencies = str_list(&find("dependencies"), "dependencies")?;
        let smoke_args = str_list(&find("smoke_args"), "smoke_args")?;

        let need_target = || -> Result<&str, JsonRpcError> {
            target
                .as_deref()
                .filter(|t| !t.trim().is_empty())
                .ok_or_else(|| {
                    JsonRpcError::invalid_params(
                        "Invalid params: 'target' is required and must be a non-empty string",
                    )
                })
        };

        let engine = &mut self.engine;
        let plan_res: Result<crate::engine::Plan, JsonRpcError> = match action {
            "search" | "info" => engine
                .prepare_search(need_target()?, repo.as_deref())
                .map_err(JsonRpcError::server),
            "inspect" => engine
                .prepare_inspect(need_target()?)
                .map(|(plan, _)| plan)
                .map_err(JsonRpcError::server),
            "config.set" => {
                let key = match find("key") {
                    Value::String(k) if !k.trim().is_empty() => k,
                    Value::Null => {
                        return Err(JsonRpcError::invalid_params(
                            "Invalid params: 'key' is required for action 'config.set'",
                        ))
                    }
                    other => {
                        return Err(JsonRpcError::invalid_params(format!(
                            "Invalid params: 'key' must be a string, got {}",
                            json_type(&other)
                        )))
                    }
                };
                let value = match find("value") {
                    Value::String(v) => v,
                    Value::Bool(v) => v.to_string(),
                    Value::Null => {
                        return Err(JsonRpcError::invalid_params(
                            "Invalid params: 'value' is required for action 'config.set'",
                        ))
                    }
                    other => {
                        return Err(JsonRpcError::invalid_params(format!(
                            "Invalid params: 'value' must be a string or boolean, got {}",
                            json_type(&other)
                        )))
                    }
                };
                // With a readable store the only way this fails is a key or value
                // the store rejects, which is the caller's parameter to fix, not an
                // engine fault. A store nobody can read is the other case, and that
                // is -32000.
                if let Some(error) = engine.config_error() {
                    return Err(JsonRpcError::server(error.to_string()));
                }
                engine
                    .prepare_config_set(&key, &value)
                    .map_err(JsonRpcError::invalid_params)
            }
            "build" => {
                let target = need_target()?;
                engine
                    .prepare_build(
                        target,
                        name.as_deref(),
                        version.as_deref(),
                        entry.as_deref(),
                        &dependencies,
                        &smoke_args,
                    )
                    .map_err(JsonRpcError::server)
            }
            "install" => engine
                .prepare_install(need_target()?)
                .map_err(JsonRpcError::server),
            "uninstall" => engine
                .prepare_uninstall(need_target()?)
                .map_err(JsonRpcError::server),
            "test" => engine
                .prepare_test(need_target()?, entry.as_deref(), &smoke_args)
                .map_err(JsonRpcError::server),
            other => {
                return Err(JsonRpcError::invalid_params(format!(
                    "Invalid params: unknown prepare action '{other}'"
                )))
            }
        };
        plan_res
    }
}

/// `options` carries the same fields as its parent, so it joins the lookup list.
/// Anything but an object there is the caller's mistake, not the engine's.
fn push_options<'a>(carriers: &mut Vec<&'a Value>, parent: &'a Value) -> Result<(), JsonRpcError> {
    if let Some(options) = parent.get("options") {
        if !options.is_null() {
            if !options.is_object() {
                return Err(JsonRpcError::invalid_params(format!(
                    "Invalid params: 'options' must be a JSON object, got {}",
                    json_type(options)
                )));
            }
            carriers.push(options);
        }
    }
    Ok(())
}

fn require_str<'a>(params: &'a Value, key: &str, what: &str) -> Result<&'a str, JsonRpcError> {
    match params.get(key) {
        None | Some(Value::Null) => Err(JsonRpcError::invalid_params(format!(
            "Invalid params: '{key}' is required ({what})"
        ))),
        Some(Value::String(value)) if value.trim().is_empty() => Err(JsonRpcError::invalid_params(
            format!("Invalid params: '{key}' must not be empty"),
        )),
        Some(Value::String(value)) => Ok(value.as_str()),
        Some(other) => Err(JsonRpcError::invalid_params(format!(
            "Invalid params: '{key}' must be a string, got {}",
            json_type(other)
        ))),
    }
}

fn optional_str(params: &Value, key: &str) -> Result<Option<String>, JsonRpcError> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(other) => Err(JsonRpcError::invalid_params(format!(
            "Invalid params: '{key}' must be a string, got {}",
            json_type(other)
        ))),
    }
}

fn optional_bool(params: &Value, key: &str) -> Result<Option<bool>, JsonRpcError> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(other) => Err(JsonRpcError::invalid_params(format!(
            "Invalid params: '{key}' must be a boolean, got {}",
            json_type(other)
        ))),
    }
}

fn str_list(value: &Value, key: &str) -> Result<Vec<String>, JsonRpcError> {
    match value {
        Value::Null => Ok(Vec::new()),
        Value::Array(items) => items
            .iter()
            .map(|item| {
                item.as_str().map(str::to_string).ok_or_else(|| {
                    JsonRpcError::invalid_params(format!(
                        "Invalid params: every '{key}' entry must be a string, got {}",
                        json_type(item)
                    ))
                })
            })
            .collect(),
        other => Err(JsonRpcError::invalid_params(format!(
            "Invalid params: '{key}' must be an array of strings, got {}",
            json_type(other)
        ))),
    }
}

/// Serialize a response frame. A value that cannot be serialized is replaced by
/// an internal-error frame rather than dropped, so a client is never left
/// waiting for a reply that vanished.
fn json_to_frame(value: &Value) -> String {
    serde_json::to_string(value)
        .unwrap_or_else(|_| serde_json::to_string(&internal_error_frame()).unwrap_or_default())
}

fn internal_error_frame() -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": null,
        "error": { "code": SERVER_ERROR, "message": "Internal error: response could not be encoded" }
    })
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

impl RpcServer {
    /// Handle one already-trimmed transport frame: a request object or a batch
    /// array. `None` means the frame needs no answer (a pure notification).
    pub fn handle_frame(&mut self, frame: &str) -> Option<String> {
        let value = match serde_json::from_str::<Value>(frame) {
            Ok(value) => value,
            Err(e) => {
                return Some(json_to_frame(&Self::response_value(
                    None,
                    Err(JsonRpcError::parse(format!("Parse error: {e}"))),
                )))
            }
        };
        self.handle_value(value).map(|value| json_to_frame(&value))
    }

    fn response_value(id: Option<Value>, outcome: Result<Value, JsonRpcError>) -> Value {
        serde_json::to_value(Self::response(id, outcome)).unwrap_or_else(|_| internal_error_frame())
    }

    fn handle_value(&mut self, value: Value) -> Option<Value> {
        match value {
            Value::Array(items) => self.handle_batch(items),
            Value::Object(object) => match decode_request(Value::Object(object)) {
                Err((id, error)) => Some(Self::response_value(id, Err(error))),
                Ok(decoded) => self.finish(decoded).map(|response| {
                    serde_json::to_value(response).unwrap_or_else(|_| internal_error_frame())
                }),
            },
            other => Some(Self::response_value(
                None,
                Err(JsonRpcError::invalid_request(format!(
                    "Invalid Request: expected a request object or batch array, got {}",
                    json_type(&other)
                ))),
            )),
        }
    }

    fn handle_batch(&mut self, items: Vec<Value>) -> Option<Value> {
        if items.is_empty() {
            return Some(Self::response_value(
                None,
                Err(JsonRpcError::invalid_request(
                    "Invalid Request: a batch must contain at least one request",
                )),
            ));
        }
        let mut responses = Vec::new();
        for item in items {
            let single = match decode_request(item) {
                Err((id, error)) => Some(Self::response(id, Err(error))),
                Ok(decoded) => self.finish(decoded),
            };
            if let Some(response) = single {
                responses.push(
                    serde_json::to_value(response).unwrap_or_else(|_| internal_error_frame()),
                );
            }
        }
        // A batch of nothing but notifications is answered with nothing.
        if responses.is_empty() {
            return None;
        }
        Some(Value::Array(responses))
    }

    fn finish(&mut self, decoded: Decoded) -> Option<JsonRpcResponse> {
        let Decoded {
            request,
            notification,
        } = decoded;
        let id = request.id.clone();
        let outcome = self.evaluate(&request);
        if notification {
            return None;
        }
        Some(Self::response(id, outcome))
    }

    pub fn run_loop<R: BufRead, W: Write>(
        &mut self,
        mut reader: R,
        mut writer: W,
    ) -> Result<(), String> {
        loop {
            let frame = match read_frame(&mut reader, MAX_FRAME_BYTES) {
                Ok(frame) => frame,
                Err(e) => return Err(format!("Failed to read request frame: {e}")),
            };
            match frame {
                Frame::Eof => return Ok(()),
                Frame::Oversized => {
                    // The whole line was consumed, so nothing of it can be read
                    // back as the next request.
                    let note = Self::response_value(
                        None,
                        Err(JsonRpcError::parse(
                            "Parse error: request frame exceeds the maximum size of 1 MiB",
                        )),
                    );
                    let _ = write_frame(&mut writer, &json_to_frame(&note));
                    // docs/IPC.md: an oversized frame closes the session.
                    return Ok(());
                }
                Frame::Line(bytes) => {
                    if bytes.iter().all(|b| b.is_ascii_whitespace()) {
                        continue;
                    }
                    // Undecodable bytes are a parse error for this frame only;
                    // the session survives them.
                    let text = match String::from_utf8(bytes) {
                        Ok(text) => text,
                        Err(_) => {
                            let note = Self::response_value(
                                None,
                                Err(JsonRpcError::parse(
                                    "Parse error: request frame is not valid UTF-8",
                                )),
                            );
                            if !write_frame(&mut writer, &json_to_frame(&note)) {
                                return Ok(());
                            }
                            continue;
                        }
                    };
                    let trimmed = text.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if let Some(response) = self.handle_frame(trimmed) {
                        if !write_frame(&mut writer, &response) {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
}

/// Read one newline-terminated frame. At most `limit` plus one transport chunk
/// is ever buffered, and an oversized line is dropped whole — including the
/// bytes past the limit — so none of it can be re-read as the next request.
fn read_frame<R: BufRead>(reader: &mut R, limit: usize) -> io::Result<Frame> {
    let mut buffer: Vec<u8> = Vec::new();
    let mut draining = false;
    loop {
        let (newline, take, empty) = {
            let slice = reader.fill_buf()?;
            let newline = slice.iter().position(|byte| *byte == b'\n');
            (
                newline,
                newline.map(|index| index + 1).unwrap_or(slice.len()),
                slice.is_empty(),
            )
        };
        if empty {
            return Ok(if draining {
                Frame::Oversized
            } else if buffer.is_empty() {
                Frame::Eof
            } else {
                Frame::Line(buffer)
            });
        }

        if draining {
            reader.consume(take);
            if newline.is_some() {
                return Ok(Frame::Oversized);
            }
            continue;
        }

        {
            let slice = reader.fill_buf()?;
            buffer.extend_from_slice(&slice[..take]);
        }
        reader.consume(take);

        if buffer.len() > limit {
            draining = true;
            buffer = Vec::new();
            // The newline can have been inside the chunk that broke the limit.
            if newline.is_some() {
                return Ok(Frame::Oversized);
            }
            continue;
        }
        if newline.is_some() {
            return Ok(Frame::Line(buffer));
        }
    }
}

/// Write one response line. Returns false when the peer went away (EPIPE),
/// which ends the session without panicking.
fn write_frame<W: Write>(writer: &mut W, response: &str) -> bool {
    if writeln!(writer, "{response}").is_err() {
        return false;
    }
    writer.flush().is_ok()
}

/// Decode a JSON value into a request, reporting the structural problems the
/// spec assigns to `-32600` rather than a parse error. The `Err` payload carries
/// the id to echo back, which is null whenever it could not be determined.
fn decode_request(value: Value) -> Result<Decoded, (Option<Value>, JsonRpcError)> {
    let object = match value {
        Value::Object(map) => map,
        other => {
            return Err((
                None,
                JsonRpcError::invalid_request(format!(
                    "Invalid Request: expected a request object, got {}",
                    json_type(&other)
                )),
            ))
        }
    };

    let mut id: Option<Value> = None;
    let mut notification = false;
    match object.get("id") {
        None => notification = true,
        Some(Value::Null) => id = Some(Value::Null),
        Some(value @ (Value::String(_) | Value::Number(_))) => id = Some(value.clone()),
        Some(other) => {
            return Err((
                None,
                JsonRpcError::invalid_request(format!(
                    "Invalid Request: 'id' must be a string, number, or null, got {}",
                    json_type(other)
                )),
            ))
        }
    }

    let method = match object.get("method") {
        Some(Value::String(method)) if !method.trim().is_empty() => method.clone(),
        Some(Value::String(_)) => {
            return Err((
                id,
                JsonRpcError::invalid_request("Invalid Request: 'method' must not be empty"),
            ))
        }
        Some(other) => {
            return Err((
                id,
                JsonRpcError::invalid_request(format!(
                    "Invalid Request: 'method' must be a string, got {}",
                    json_type(other)
                )),
            ))
        }
        None => {
            return Err((
                id,
                JsonRpcError::invalid_request("Invalid Request: 'method' is required"),
            ))
        }
    };

    let jsonrpc = match object.get("jsonrpc") {
        Some(Value::String(version)) => version.clone(),
        None => {
            return Err((
                id,
                JsonRpcError::invalid_request("Invalid Request: 'jsonrpc' must be \"2.0\""),
            ))
        }
        Some(other) => {
            return Err((
                id,
                JsonRpcError::invalid_request(format!(
                    "Invalid Request: 'jsonrpc' must be a string, got {}",
                    json_type(other)
                )),
            ))
        }
    };

    let params = object
        .get("params")
        .cloned()
        .filter(|value| !value.is_null())
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));

    Ok(Decoded {
        request: JsonRpcRequest {
            jsonrpc,
            id,
            method,
            params,
        },
        notification,
    })
}
