//! Framing and error-code contract for the newline-delimited JSON-RPC server.
//! Every case here failed before the dispatch layer was rewritten: notifications
//! were answered, `-32600` was reported as a parse error, a missing `target` was
//! silently defaulted to `""`, and one oversized or non-UTF-8 line killed or
//! desynchronized the whole session.

use archbridge::rpc::{
    RpcServer, INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR, SERVER_ERROR,
};
use serde_json::{json, Value};
use std::io::{self, BufRead, Cursor, Write};

/// A server whose preferences file lives in a throwaway directory.
fn temp_server(name: &str) -> (tempfile::TempDir, RpcServer) {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = RpcServer::with_config_path(&dir.path().join(format!("{name}.json")));
    (dir, server)
}

fn session(input: &str) -> (Vec<String>, Result<(), String>) {
    let reader = Cursor::new(input.as_bytes().to_vec());
    let mut writer: Vec<u8> = Vec::new();
    let (_dir, mut server) = temp_server("rpc-framing");
    let result = server.run_loop(reader, &mut writer);
    let text = String::from_utf8_lossy(&writer).to_string();
    (text.lines().map(str::to_string).collect::<Vec<_>>(), result)
}

fn frames(input: &str) -> Vec<Value> {
    session(input)
        .0
        .iter()
        .map(|line| serde_json::from_str(line).expect("every response frame is valid JSON"))
        .collect()
}

fn one(input: &str) -> Value {
    let mut responses = frames(input);
    assert_eq!(
        responses.len(),
        1,
        "expected exactly one frame, got {responses:?}"
    );
    responses.remove(0)
}

fn request(id: Value, method: &str, params: Value) -> String {
    let mut object = json!({"jsonrpc": "2.0", "id": id, "method": method});
    if let Value::Object(ref mut map) = object {
        map.insert("params".to_string(), params);
    }
    format!("{object}\n")
}

#[test]
fn a_notification_is_executed_but_never_answered() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("config.json");
    let mut server = RpcServer::with_config_path(&config_path);

    // Stage the change as a normal request so the plan id is known.
    let prepared = server
        .handle_frame(&request(
            json!(1),
            "v1.prepare",
            json!({"action": "config.set", "key": "aur", "value": "false"}),
        ))
        .expect("a request gets a response");
    let plan_id = serde_json::from_str::<Value>(&prepared).expect("frame")[&"result"]["plan_id"]
        .as_str()
        .expect("plan id")
        .to_string();

    // The same call as a notification: no `id` member at all.
    let notification = format!(
        "{{\"jsonrpc\":\"2.0\",\"method\":\"v1.execute\",\"params\":{{\"plan_id\":\"{plan_id}\",\"confirmed\":true}}}}\n"
    );
    let mut writer: Vec<u8> = Vec::new();
    server
        .run_loop(Cursor::new(notification.as_bytes().to_vec()), &mut writer)
        .expect("the session ends cleanly");
    assert!(
        writer.is_empty(),
        "a notification must not be answered, wrote {}",
        String::from_utf8_lossy(&writer)
    );

    // It still ran: the preference reached the file the engine owns.
    let reloaded = archbridge::config::Config::load_from(&config_path).expect("reload");
    assert!(
        !reloaded.aur,
        "the notification was dropped instead of executed"
    );
}

#[test]
fn an_explicit_null_id_is_a_request_not_a_notification() {
    let response = one(&request(Value::Null, "v1.capabilities", json!({})));
    assert!(response.get("id").is_some(), "{response}");
    assert_eq!(response["id"], Value::Null);
    assert!(response.get("result").is_some(), "{response}");
}

#[test]
fn the_id_type_survives_the_round_trip() {
    for (id, expected) in [
        (json!("abc"), json!("abc")),
        (json!(7), json!(7)),
        (json!(-3), json!(-3)),
        (json!(7.5), json!(7.5)),
        (json!("0"), json!("0")),
    ] {
        let response = one(&request(id.clone(), "v1.capabilities", json!({})));
        assert_eq!(response["id"], expected, "id {id} was not preserved");
    }
}

#[test]
fn an_unknown_method_is_32601_not_32000() {
    let response = one(&request(json!(1), "v1.nope", json!({})));
    assert_eq!(response["error"]["code"], METHOD_NOT_FOUND, "{response}");
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("v1.nope"),
        "{response}"
    );
}

#[test]
fn an_unknown_prepare_action_is_a_params_error() {
    let response = one(&request(
        json!(1),
        "v1.prepare",
        json!({"action": "levitate", "target": "htop"}),
    ));
    assert_eq!(response["error"]["code"], INVALID_PARAMS, "{response}");
}

#[test]
fn missing_or_wrongly_typed_params_are_32602_and_never_run() {
    // An absent target used to default to "" and query every source with it.
    let cases: Vec<(&str, Value)> = vec![
        ("v1.search", json!({})),
        ("v1.search", json!({"target": ""})),
        ("v1.search", json!({"target": 42})),
        ("v1.search", json!({"target": ["htop"]})),
        ("v1.search", json!({"target": "htop", "repo": 3})),
        ("v1.search", Value::String("htop".to_string())),
        ("v1.inspect", json!({})),
        ("v1.inspect", json!({"target": null})),
        ("v1.authenticate", json!({})),
        ("v1.authenticate", json!({"password": 1234})),
        ("v1.optimize_sudo_auth", json!({"password": null})),
        ("v1.config.get", json!({"key": 5})),
        ("v1.prepare", json!({"target": "htop"})),
        ("v1.prepare", json!({"action": "build"})),
        ("v1.prepare", json!({"action": "build", "target": 1})),
        ("v1.prepare", json!({"action": "config.set", "key": "aur"})),
        (
            "v1.prepare",
            json!({"action": "config.set", "value": "false"}),
        ),
        (
            "v1.prepare",
            json!({"action": "config.set", "key": "aur", "value": ["false"]}),
        ),
        ("v1.prepare", json!({"action": "build", "request": "src"})),
        (
            "v1.prepare",
            json!({"action": "build", "target": "x", "options": {"dependencies": "glibc"}}),
        ),
        (
            "v1.prepare",
            json!({"action": "build", "target": "x", "options": {"smoke_args": [1]}}),
        ),
        ("v1.execute", json!({})),
        ("v1.execute", json!({"plan_id": 9})),
        ("v1.execute", json!({"plan_id": "x", "confirmed": "yes"})),
        // An array `params` names nothing this server can read.
        ("v1.capabilities", json!([1])),
        ("v1.search", json!("htop")),
    ];

    for (method, params) in cases {
        let response = one(&request(json!("r"), method, params.clone()));
        assert_eq!(
            response["error"]["code"], INVALID_PARAMS,
            "{method} {params} was not rejected as invalid params: {response}"
        );
        assert_eq!(response["id"], json!("r"), "{method} lost its id");
        assert!(
            response.get("result").is_none(),
            "{method} {params} answered with a result: {response}"
        );
    }
}

#[test]
fn valid_params_still_reach_the_engine_and_report_operational_failures_as_32000() {
    // A well formed request whose *execution* fails must keep -32000.
    let response = one(&request(
        json!(1),
        "v1.execute",
        json!({"plan_id": "0000000000000000000000000000000000000000000000000000000000000000", "confirmed": true}),
    ));
    assert_eq!(response["error"]["code"], SERVER_ERROR, "{response}");

    let response = one(&request(
        json!(2),
        "v1.config.get",
        json!({"key": "no-such-key"}),
    ));
    assert_eq!(response["error"]["code"], INVALID_PARAMS, "{response}");
}

#[test]
fn structurally_invalid_requests_are_32600_with_a_null_id() {
    // Valid JSON that is not a usable request object: not a parse error.
    let invalid: Vec<&str> = vec![
        "{\"jsonrpc\":\"2.0\",\"id\":1}\n",                 // no method
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":5}\n",    // non-string method
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"\"}\n", // empty method
        "{\"id\":1,\"method\":\"v1.capabilities\"}\n",      // no jsonrpc
        "{\"jsonrpc\":\"1.0\",\"id\":1,\"method\":\"v1.capabilities\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":{\"a\":1},\"method\":\"v1.capabilities\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":[1],\"method\":\"v1.capabilities\"}\n",
        "[]\n",
        "\"just a string\"\n",
        "12345\n",
        "true\n",
        "null\n",
    ];
    for line in invalid {
        let response = one(line);
        assert_eq!(
            response["error"]["code"], INVALID_REQUEST,
            "{line} was not an invalid request: {response}"
        );
        // The id is only echoed when it was a valid string/number.
        let echoed = response["id"].clone();
        assert!(
            echoed.is_null() || echoed.is_string() || echoed.is_number(),
            "{line} answered with a non-correlatable id: {response}"
        );
    }
}

/// A batch element that is not a request object is answered on its own, and the
/// valid siblings still run.
#[test]
fn a_non_object_batch_element_is_32600_while_its_siblings_answer() {
    let response = one("[{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"v1.capabilities\"},7]\n");
    let answers = response.as_array().expect("a batch answers with an array");
    assert_eq!(answers.len(), 2, "{response}");
    assert!(answers[0].get("result").is_some(), "{response}");
    assert_eq!(answers[1]["error"]["code"], INVALID_REQUEST, "{response}");
    assert!(answers[1]["id"].is_null(), "{response}");
}

/// A batch keeps going past a bad element: the good request is still answered
/// and the bad one is reported on its own, inside the batch reply.
#[test]
fn a_batch_reports_each_element_separately() {
    let response = one(&format!(
        "{}\n",
        json!([
            {"jsonrpc": "2.0", "id": 1, "method": "v1.capabilities"},
            7
        ])
    ));
    let answers = response.as_array().expect("a batch answers with an array");
    assert_eq!(answers.len(), 2, "{response}");
    assert!(answers[0].get("result").is_some(), "{}", answers[0]);
    assert_eq!(
        answers[1]["error"]["code"], INVALID_REQUEST,
        "{}",
        answers[1]
    );
    assert!(answers[1]["id"].is_null(), "{}", answers[1]);
}

#[test]
fn invalid_json_is_a_parse_error() {
    for line in [
        "{not json}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\n",
        "{\"jsonrpc\":\"2.0\",\"id\":1,}\n",
    ] {
        let response = one(line);
        assert_eq!(
            response["error"]["code"], PARSE_ERROR,
            "{line} -> {response}"
        );
        assert!(response["id"].is_null(), "{line} -> {response}");
    }
}

#[test]
fn batches_answer_only_the_requests_in_order() {
    let batch = json!([
        {"jsonrpc": "2.0", "id": 1, "method": "v1.capabilities"},
        {"jsonrpc": "2.0", "method": "v1.config.get", "params": {"key": "aur"}},
        {"jsonrpc": "2.0", "id": "two", "method": "v1.nope"},
        {"jsonrpc": "2.0", "id": 3, "method": "v1.config.get", "params": {"key": "rpm"}},
    ])
    .to_string();
    let response = one(&format!("{batch}\n"));
    assert!(
        response.is_array(),
        "a batch must answer with an array: {response}"
    );
    let answers = response.as_array().unwrap();
    assert_eq!(
        answers.len(),
        3,
        "the notification answered nothing: {response}"
    );
    assert_eq!(answers[0]["id"], json!(1));
    assert_eq!(answers[1]["id"], json!("two"));
    assert_eq!(answers[1]["error"]["code"], METHOD_NOT_FOUND);
    assert_eq!(answers[2]["id"], json!(3));
    assert_eq!(answers[2]["result"], json!(true));
}

#[test]
fn a_batch_of_only_notifications_is_silent() {
    let batch = json!([
        {"jsonrpc": "2.0", "method": "v1.config.get", "params": {"key": "aur"}},
        {"jsonrpc": "2.0", "method": "v1.capabilities"},
    ])
    .to_string();
    let (lines, result) = session(&format!("{batch}\n"));
    assert!(lines.is_empty(), "notifications produced {lines:?}");
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn an_empty_batch_is_rejected_with_32600() {
    let response = one("[]\n");
    assert_eq!(response["error"]["code"], INVALID_REQUEST, "{response}");
    assert!(response["id"].is_null(), "{response}");
}

#[test]
fn an_oversized_frame_is_answered_then_the_session_closes_without_reading_the_tail() {
    let mut input = vec![b'a'; 1_048_577];
    input.extend_from_slice(b"\n");
    // A perfectly valid request smuggled onto the end of the oversized line must
    // not be reparsed as the next request.
    let mut smuggled = input.clone();
    smuggled.extend_from_slice(request(json!(9), "v1.capabilities", json!({})).as_bytes());

    let mut writer: Vec<u8> = Vec::new();
    let (_dir, mut server) = temp_server("rpc-oversized");
    let result = server.run_loop(Cursor::new(smuggled), &mut writer);
    assert!(result.is_ok(), "{result:?}");

    let text = String::from_utf8_lossy(&writer).to_string();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "the tail was parsed as another request: {lines:?}"
    );
    let response: Value = serde_json::from_str(lines[0]).expect("frame");
    assert_eq!(response["error"]["code"], PARSE_ERROR, "{response}");
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("1 MiB"),
        "{response}"
    );
    assert!(input.len() > 1_048_576);
}

#[test]
fn invalid_utf8_is_a_parse_error_that_leaves_the_session_usable() {
    let mut input = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"v1.cap\",\"x\":\"".to_vec();
    input.push(0xff);
    input.push(0xfe);
    input.extend_from_slice(b"\"}\n");
    input.extend_from_slice(request(json!(2), "v1.capabilities", json!({})).as_bytes());

    let mut writer: Vec<u8> = Vec::new();
    let (_dir, mut server) = temp_server("rpc-utf8");
    let result = server.run_loop(Cursor::new(input), &mut writer);
    assert!(
        result.is_ok(),
        "invalid UTF-8 aborted the server: {result:?}"
    );

    let text = String::from_utf8_lossy(&writer).to_string();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "the session died on the bad line: {lines:?}"
    );
    let first: Value = serde_json::from_str(lines[0]).expect("frame");
    assert_eq!(first["error"]["code"], PARSE_ERROR, "{first}");
    let second: Value = serde_json::from_str(lines[1]).expect("frame");
    assert_eq!(second["id"], json!(2), "{second}");
    assert!(second.get("result").is_some(), "{second}");
}

/// A closed stdout is the ordinary way a client disappears.
#[test]
fn a_broken_pipe_ends_the_session_without_a_panic_or_a_spin() {
    struct Closed;
    impl Write for Closed {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
        }
    }

    let (_dir, mut server) = temp_server("rpc-epipe");
    let input = request(json!(1), "v1.capabilities", json!({}))
        + &request(json!(2), "v1.capabilities", json!({}));
    let result = server.run_loop(Cursor::new(input.as_bytes().to_vec()), Closed);
    assert!(result.is_ok(), "EPIPE surfaced as an error: {result:?}");
}

/// A reader that hands out one byte at a time exercises the buffered frame
/// reader, which is what previously read an unbounded line into memory.
#[test]
fn a_frame_split_across_many_reads_still_works() {
    struct OneByte<'a>(&'a [u8]);
    impl std::io::Read for OneByte<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let amount = buffer.len().min(self.0.len());
            buffer[..amount].copy_from_slice(&self.0[..amount]);
            self.0 = &self.0[amount..];
            Ok(amount)
        }
    }
    impl BufRead for OneByte<'_> {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            Ok(match self.0.is_empty() {
                true => &[],
                false => &self.0[..1],
            })
        }
        fn consume(&mut self, amount: usize) {
            self.0 = self.0.get(amount..).unwrap_or(&[]);
        }
    }

    let input = request(json!(1), "v1.capabilities", json!({}));
    let mut writer: Vec<u8> = Vec::new();
    let (_dir, mut server) = temp_server("rpc-bytes");
    server
        .run_loop(OneByte(input.as_bytes()), &mut writer)
        .expect("session");
    let text = String::from_utf8_lossy(&writer).to_string();
    let response: Value = serde_json::from_str(text.trim()).expect("frame");
    assert_eq!(response["id"], json!(1), "{response}");
    assert!(response.get("result").is_some(), "{response}");
}

#[test]
fn prepare_accepts_config_set_in_both_param_shapes() {
    let nested = one(&request(
        json!(1),
        "v1.prepare",
        json!({"action": "config.set", "request": {"key": "flatpak", "value": "false"}}),
    ));
    assert!(nested.get("result").is_some(), "{nested}");

    let top_level = one(&request(
        json!(2),
        "v1.prepare",
        json!({"action": "config.set", "key": "flatpak", "value": "false"}),
    ));
    assert!(top_level.get("result").is_some(), "{top_level}");

    // Booleans are accepted for convenience and normalized to the stored string.
    let boolean = one(&request(
        json!(3),
        "v1.prepare",
        json!({"action": "config.set", "key": "flatpak", "value": false}),
    ));
    assert!(boolean.get("result").is_some(), "{boolean}");
    assert!(
        boolean["result"]["summary"]
            .as_str()
            .unwrap_or_default()
            .contains("false"),
        "{boolean}"
    );
}

/// Deterministic pseudo-random frames: nothing here may panic, and anything the
/// server does answer must be a well formed JSON-RPC envelope.
#[test]
fn fuzz_frames_never_panic_and_always_answer_well_formed_json() {
    let alphabet: Vec<&str> = vec![
        "",
        "{",
        "}",
        "[",
        "]",
        ",",
        "null",
        "true",
        "1e999",
        "\"2.0\"",
        "\"v1.capabilities\"",
        "\"v1.deauthenticate\"",
        "\"v1.sudo_advice\"",
        "\"v1.config.get\"",
        "\"id\"",
        "\"method\"",
        "\"jsonrpc\"",
        "\"params\"",
        "3",
        ":",
        "\u{0}",
        "\u{1f600}",
        "\"target\"",
        "{}",
        "[]",
    ];

    // xorshift64: reproducible without a rand dependency.
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };

    let (_dir, mut server) = temp_server("rpc-fuzz");
    for round in 0..600 {
        let pieces = 1 + (next() % 14) as usize;
        let mut line = String::new();
        for index in 0..pieces {
            line.push_str(alphabet[(next() as usize) % alphabet.len()]);
            if index + 1 < pieces && next() % 4 == 0 {
                line.push(' ');
            }
        }
        let mut writer: Vec<u8> = Vec::new();
        let input = format!("{line}\n");
        let result = server.run_loop(Cursor::new(input.into_bytes()), &mut writer);
        assert!(result.is_ok(), "round {round} aborted: {result:?}");

        let text = String::from_utf8_lossy(&writer).to_string();
        for emitted in text.lines() {
            let value: Value = serde_json::from_str(emitted)
                .unwrap_or_else(|e| panic!("round {round} emitted {emitted:?}: {e}"));
            assert_eq!(value["jsonrpc"], json!("2.0"), "{value}");
            assert!(value.get("id").is_some(), "a response without id: {value}");
            let has_result = value.get("result").is_some();
            let has_error = value.get("error").is_some();
            assert!(
                has_result != has_error,
                "a response needs exactly one of result/error: {value}"
            );
            if let Some(error) = value.get("error") {
                let code = error["code"].as_i64().unwrap_or_default();
                assert!(
                    matches!(code, -32700 | -32600 | -32601 | -32602 | -32000),
                    "undocumented error code {code} for input {line:?}"
                );
            }
        }
    }
}

#[test]
fn blank_lines_are_ignored_without_a_response() {
    let input = format!(
        "\n   \n{}{}\n\n",
        request(json!(1), "v1.capabilities", json!({})),
        request(json!(2), "v1.capabilities", json!({}))
    );
    let responses = frames(&input);
    assert_eq!(responses.len(), 2, "{responses:?}");
}
