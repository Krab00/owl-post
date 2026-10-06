//! `owl mcp`: the contact book as MCP resources over stdio (design §9), so Claude Code lists
//! every contact in its `@` typeahead as `@owl:to://<name-slug>.<email>` and attaches the
//! contact (name, fingerprint, e-mails) to the prompt. JSON-RPC 2.0, one object per line;
//! resources only — no tools, no prompts — so the server costs nothing in the model context.
//! The book is re-read from disk on every list/read, so a contact added mid-session shows up.

use std::io::{BufRead, Write};
use std::path::Path;

use owlpost::contacts::ContactBook;
use serde_json::{Value, json};

/// Protocol version offered when the client names none.
const PROTOCOL_VERSION: &str = "2025-06-18";
const MIME: &str = "application/json";

pub fn run(home: &Path) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = handle_line(home, &cwd, &line) {
            serde_json::to_writer(&mut stdout, &reply)?;
            stdout.write_all(b"\n")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

/// The reply for one input line, or `None` for a notification (a request without `id`).
fn handle_line(home: &Path, cwd: &Path, line: &str) -> Option<Value> {
    let req: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return Some(error(Value::Null, -32700, format!("parse error: {e}"))),
    };
    let Some(method) = req.get("method").and_then(Value::as_str) else {
        return Some(error(Value::Null, -32600, "invalid request: no method"));
    };
    let id = req.get("id").filter(|id| !id.is_null()).cloned();
    let params = req.get("params").cloned().unwrap_or(Value::Null);
    let outcome = dispatch(home, cwd, method, &params);
    let id = id?;
    Some(match outcome {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err((code, message)) => error(id, code, message),
    })
}

fn error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}})
}

fn dispatch(home: &Path, cwd: &Path, method: &str, params: &Value) -> Result<Value, (i64, String)> {
    match method {
        "initialize" => {
            let version = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(PROTOCOL_VERSION);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": {"resources": {}},
                "serverInfo": {"name": "owl", "version": env!("CARGO_PKG_VERSION")},
            }))
        }
        "ping" => Ok(json!({})),
        "resources/list" => {
            let book = load(home, cwd)?;
            let resources: Vec<Value> = book
                .uris()
                .into_iter()
                .map(|(uri, c)| {
                    json!({
                        "uri": uri,
                        "name": c.name,
                        "description": format!("{} · {}", c.fingerprint, c.source),
                        "mimeType": MIME,
                    })
                })
                .collect();
            Ok(json!({"resources": resources}))
        }
        "resources/read" => {
            let uri = params
                .get("uri")
                .and_then(Value::as_str)
                .ok_or_else(|| (-32602, "invalid params: uri is required".to_string()))?;
            let book = load(home, cwd)?;
            let (_, c) = book
                .uris()
                .into_iter()
                .find(|(u, _)| u == uri)
                .ok_or_else(|| (-32002, format!("resource not found: {uri}")))?;
            let text = json!({"name": c.name, "fingerprint": c.fingerprint, "emails": c.emails});
            Ok(json!({"contents": [{"uri": uri, "mimeType": MIME, "text": text.to_string()}]}))
        }
        "resources/templates/list" => Ok(json!({"resourceTemplates": []})),
        "tools/list" => Ok(json!({"tools": []})),
        "prompts/list" => Ok(json!({"prompts": []})),
        other => Err((-32601, format!("method not found: {other}"))),
    }
}

fn load(home: &Path, cwd: &Path) -> Result<ContactBook, (i64, String)> {
    ContactBook::load(home, cwd).map_err(|e| (-32603, format!("loading contacts: {e:#}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notifications_get_no_reply_and_bad_lines_get_null_ids() {
        let dir = tempfile::tempdir().unwrap();
        let (h, c) = (dir.path(), dir.path());
        assert_eq!(
            handle_line(
                h,
                c,
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
            ),
            None
        );
        let bad = handle_line(h, c, "{not json").unwrap();
        assert_eq!(bad["id"], Value::Null);
        assert_eq!(bad["error"]["code"], -32700);
        let not_request = handle_line(h, c, "[1,2]").unwrap();
        assert_eq!(not_request["id"], Value::Null);
        assert_eq!(not_request["error"]["code"], -32600);
        let unknown = handle_line(h, c, r#"{"jsonrpc":"2.0","id":7,"method":"nope"}"#).unwrap();
        assert_eq!(unknown["id"], 7);
        assert_eq!(unknown["error"]["code"], -32601);
        let ping = handle_line(h, c, r#"{"jsonrpc":"2.0","id":"p","method":"ping"}"#).unwrap();
        assert_eq!(ping, json!({"jsonrpc": "2.0", "id": "p", "result": {}}));
    }
}
