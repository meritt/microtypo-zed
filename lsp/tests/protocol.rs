use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

const CORWIN: &str = include_str!("fixtures/corwin.md");
const CORWIN_TEXT: &str = include_str!("fixtures/corwin.txt");
const AMBER: &str = include_str!("fixtures/amber.json");

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Client {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_microtypo-lsp"))
            .arg("--stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self { child, stdin, stdout }
    }

    fn send(&mut self, message: &Value) {
        self.send_raw(&message.to_string());
    }

    fn send_raw(&mut self, body: &str) {
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        self.stdin.flush().unwrap();
    }

    fn receive(&mut self) -> Value {
        let mut length = 0;
        loop {
            let mut line = String::new();
            self.stdout.read_line(&mut line).unwrap();
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length: ") {
                length = value.parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        self.stdout.read_exact(&mut body).unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        let mut message = json!({ "jsonrpc": "2.0", "id": id, "method": method });
        message["params"] = params;
        self.send(&message);
        loop {
            let message = self.receive();
            if message.get("id") == Some(&json!(id)) {
                return message;
            }
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        let mut message = json!({ "jsonrpc": "2.0", "method": method });
        message["params"] = params;
        self.send(&message);
    }

    fn open(&mut self, uri: &str, language: &str, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": language, "version": 1, "text": text } }),
        );
    }

    fn stop(mut self) -> i32 {
        self.request(90, "shutdown", Value::Null);
        self.notify("exit", Value::Null);
        drop(self.stdin);
        self.child.wait().unwrap().code().unwrap()
    }
}

fn offset(lines: &[Vec<u16>], position: &Value) -> usize {
    let line = usize::try_from(position["line"].as_u64().unwrap()).unwrap();
    let character = usize::try_from(position["character"].as_u64().unwrap()).unwrap();
    lines[..line].iter().map(|text| text.len() + 1).sum::<usize>() + character
}

fn apply(text: &str, edits: &Value) -> String {
    let lines: Vec<Vec<u16>> = text.split('\n').map(|line| line.encode_utf16().collect()).collect();
    let mut units: Vec<u16> = text.encode_utf16().collect();
    let mut edits: Vec<&Value> = edits.as_array().unwrap().iter().collect();
    edits.sort_by_key(|edit| std::cmp::Reverse(offset(&lines, &edit["range"]["start"])));
    for edit in edits {
        let start = offset(&lines, &edit["range"]["start"]);
        let end = offset(&lines, &edit["range"]["end"]);
        units.splice(start..end, edit["newText"].as_str().unwrap().encode_utf16());
    }
    String::from_utf16(&units).unwrap()
}

#[test]
fn formats_a_document_over_stdio() {
    let mut client = Client::start();
    let init = client.request(1, "initialize", json!({ "initializationOptions": { "format": ["Markdown"] } }));
    assert_eq!(init["result"]["serverInfo"]["name"], "microtypo-lsp");
    assert!(init["result"]["capabilities"].get("documentFormattingProvider").is_none());

    client.notify("initialized", json!({}));
    let registration = client.receive();
    assert_eq!(registration["method"], "client/registerCapability");
    client.send(&json!({ "jsonrpc": "2.0", "id": registration["id"], "result": null }));

    client.open("file:///corwin.md", "markdown", CORWIN);
    let response = client.request(
        2,
        "textDocument/formatting",
        json!({ "textDocument": { "uri": "file:///corwin.md" }, "options": { "tabSize": 2, "insertSpaces": true } }),
    );
    let output = apply(CORWIN, &response["result"]);
    assert!(output.starts_with("---\ntitle: Донесение из\u{a0}Амбера\nauthor: Корвин\n---\n"));
    assert!(output.contains("«Амбер вечен»\u{a0}— и"));
    assert!(output.contains("`code -- не трогать`"));
    assert!(output.contains("(https://simonenko.xyz)"));
    assert!(output.ends_with('\n'));
    assert_eq!(client.stop(), 0);
}

fn format_file(language: &str, text: &str) -> String {
    let mut client = Client::start();
    let init = json!({ "initializationOptions": { "format": [language] } });
    client.request(1, "initialize", init);
    client.notify("initialized", json!({}));
    let registration = client.receive();
    client.send(&json!({ "jsonrpc": "2.0", "id": registration["id"], "result": null }));
    client.open("file:///amber", language, text);
    let params =
        json!({ "textDocument": { "uri": "file:///amber" }, "options": { "tabSize": 2, "insertSpaces": true } });
    let response = client.request(2, "textDocument/formatting", params);
    let output = apply(text, &response["result"]);
    assert_eq!(client.stop(), 0);
    output
}

#[test]
fn keeps_json_structure_and_typesets_values() {
    let output = format_file("json", AMBER);
    let value: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["title"], "Донесение из\u{a0}Амбера");
    assert_eq!(value["quote"], "Корвин сказал: «Амбер»\u{a0}— вечен.");
    assert_eq!(value["realm"], "Арден");
    assert_eq!(value["treasury"], 150_000);
    assert!(output.ends_with("}\n"));
}

#[test]
fn typesets_plain_text() {
    let output = format_file("plaintext", CORWIN_TEXT);
    assert!(output.starts_with("Корвин сказал: «Амбер»\u{a0}— вечен"));
    assert!(output.contains('₽'));
    assert!(!output.contains("руб"));
    assert!(!output.contains("150000"));
    assert!(output.ends_with("Арден.\n"));
}

#[test]
fn offers_and_resolves_a_code_action_over_stdio() {
    let mut client = Client::start();
    client.request(1, "initialize", json!({}));
    client.notify("initialized", json!({}));
    let text = "😀 Корвин - принц\nРэндом - король\n";
    client.open("file:///amber.txt", "plaintext", text);
    let response = client.request(
        2,
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": "file:///amber.txt" },
            "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 15 } },
            "context": { "diagnostics": [] },
        }),
    );
    let action = response["result"][0].clone();
    assert_eq!(action["title"], "Microtypo selection");
    let resolved = client.request(3, "codeAction/resolve", action);
    let edits = &resolved["result"]["edit"]["changes"]["file:///amber.txt"];
    assert_eq!(apply(text, edits), "😀 Корвин - принц\nРэндом\u{a0}— король\n");
    assert_eq!(client.stop(), 0);
}

#[test]
fn keeps_serving_after_malformed_json() {
    let mut client = Client::start();
    client.send_raw("{not json");
    assert_eq!(client.receive()["error"]["code"], -32700);
    let init = client.request(1, "initialize", json!({}));
    assert!(init.get("result").is_some());
    assert_eq!(client.stop(), 0);
}

#[test]
fn exits_when_the_client_disappears() {
    let mut client = Client::start();
    client.request(1, "initialize", json!({}));
    let Client { mut child, stdin, stdout } = client;
    drop(stdin);
    drop(stdout);
    assert_eq!(child.wait().unwrap().code(), Some(1));
}

#[test]
fn stops_on_a_corrupt_frame() {
    let mut client = Client::start();
    client.stdin.write_all(b"Content-Length: 99999999999\r\n\r\n").unwrap();
    client.stdin.flush().unwrap();
    assert_eq!(client.child.wait().unwrap().code(), Some(1));
}
