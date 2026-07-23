//! Minimal MCP server over stdio.
//!
//! Implements what a tools+resources server needs from the spec: the `initialize`
//! handshake, `tools/list`, `tools/call`, `resources/list`, `resources/read`, and
//! `ping`. Messages are newline-delimited JSON-RPC 2.0 on stdin/stdout (the MCP
//! stdio transport). All logging goes to stderr so it never corrupts the protocol
//! stream on stdout.

use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

/// A tool handler maps a JSON `arguments` object to a text result (or an error string).
pub type Handler = Box<dyn Fn(&Value) -> Result<String, String> + Send + Sync>;

pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    pub handler: Handler,
}

pub struct Resource {
    pub uri: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub mime_type: &'static str,
    pub text: &'static str,
}

pub struct Server {
    pub tools: Vec<Tool>,
    pub resources: Vec<Resource>,
    pub server_name: String,
    pub server_version: String,
}

impl Server {
    pub fn run(&self) -> io::Result<()> {
        let stdin = io::stdin();
        let mut reader = stdin.lock();
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let mut line = String::new();

        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                break; // EOF: client closed the pipe.
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let req: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("[wordkeep] parse error: {e}");
                    continue;
                }
            };

            let method = req.get("method").and_then(Value::as_str).unwrap_or("");
            let params = req.get("params").cloned().unwrap_or(Value::Null);

            // Notifications carry no `id` and get no response.
            let id = match req.get("id") {
                Some(id) => id.clone(),
                None => continue,
            };

            let response = match method {
                "initialize" => self.handle_initialize(&params, id),
                "tools/list" => self.handle_list(id),
                "tools/call" => self.handle_call(&params, id),
                "resources/list" => self.handle_resources_list(id),
                "resources/read" => self.handle_resources_read(&params, id),
                "ping" => ok_result(id, json!({})),
                other => err_response(id, -32601, &format!("method not found: {other}")),
            };

            let s = serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string());
            out.write_all(s.as_bytes())?;
            out.write_all(b"\n")?;
            out.flush()?;
        }
        Ok(())
    }

    fn handle_initialize(&self, params: &Value, id: Value) -> Value {
        // Echo the client's protocol version for max compatibility.
        let pv = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or("2025-06-18");
        ok_result(
            id,
            json!({
                "protocolVersion": pv,
                "capabilities": {
                    "tools": { "listChanged": false },
                    "resources": { "listChanged": false, "subscribe": false }
                },
                "serverInfo": {
                    "name": self.server_name.clone(),
                    "version": self.server_version.clone()
                }
            }),
        )
    }

    fn handle_list(&self, id: Value) -> Value {
        let tools: Vec<Value> = self
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "inputSchema": t.input_schema.clone(),
                })
            })
            .collect();
        ok_result(id, json!({ "tools": tools }))
    }

    fn handle_call(&self, params: &Value, id: Value) -> Value {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));

        match self.tools.iter().find(|t| t.name == name) {
            Some(tool) => match (tool.handler)(&args) {
                // Tool-level failures are reported via `isError`, not a JSON-RPC error.
                Ok(text) => ok_result(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }], "isError": false }),
                ),
                Err(e) => ok_result(
                    id,
                    json!({ "content": [{ "type": "text", "text": format!("error: {e}") }], "isError": true }),
                ),
            },
            None => err_response(id, -32602, &format!("unknown tool: {name}")),
        }
    }

    fn handle_resources_list(&self, id: Value) -> Value {
        let resources: Vec<Value> = self
            .resources
            .iter()
            .map(|r| {
                json!({
                    "uri": r.uri,
                    "name": r.name,
                    "description": r.description,
                    "mimeType": r.mime_type,
                })
            })
            .collect();
        ok_result(id, json!({ "resources": resources }))
    }

    fn handle_resources_read(&self, params: &Value, id: Value) -> Value {
        let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
        let canonical = normalize_resource_uri(uri);
        match self
            .resources
            .iter()
            .find(|r| r.uri == canonical || r.uri == uri)
        {
            Some(r) => ok_result(
                id,
                json!({
                    "contents": [{
                        "uri": r.uri,
                        "mimeType": r.mime_type,
                        "text": r.text,
                    }]
                }),
            ),
            None => err_response(id, -32002, &format!("resource not found: {uri}")),
        }
    }
}

/// Accept `wordkeep://README` as an alias of the canonical `wordkeep://readme`.
pub fn normalize_resource_uri(uri: &str) -> &str {
    if uri.eq_ignore_ascii_case("wordkeep://readme") || uri == "wordkeep://README" {
        "wordkeep://readme"
    } else {
        uri
    }
}

fn ok_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn err_response(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readme_alias_normalizes() {
        assert_eq!(
            normalize_resource_uri("wordkeep://README"),
            "wordkeep://readme"
        );
        assert_eq!(
            normalize_resource_uri("wordkeep://readme"),
            "wordkeep://readme"
        );
        assert_eq!(
            normalize_resource_uri("wordkeep://other"),
            "wordkeep://other"
        );
    }
}
