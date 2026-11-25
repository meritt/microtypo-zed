use std::collections::BTreeMap;

use serde::{Serialize, Serializer};
use serde_json::{Map, Value, json};

use crate::config::{self, Settings};
use crate::detect::format_of;
use crate::document::{Change, Document, Documents, Lines, Position};
use crate::typeset::{Failure, Replace, Typesetter, trimmed};

const KIND: &str = "refactor.rewrite.microtypo";
const REGISTER_ID: &str = "microtypo/register";

const PARSE_ERROR: i64 = -32700;
const INTERNAL_ERROR: i64 = -32603;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const NOT_INITIALIZED: i64 = -32002;

#[derive(Debug, Default)]
pub struct Step {
    pub out: Vec<String>,
    pub exit: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Waiting,
    Running,
    Stopping,
}

#[derive(Debug)]
pub struct Server {
    phase: Phase,
    documents: Documents,
    typesetter: Option<Typesetter>,
}

fn log(method: &str, reason: &str) {
    eprintln!("microtypo-lsp: {method}: {reason}");
}

#[derive(Serialize)]
struct Response<'a, T> {
    jsonrpc: &'static str,
    id: &'a Value,
    result: T,
}

#[derive(Serialize)]
struct TextEdit {
    range: Span,
    #[serde(rename = "newText")]
    new_text: String,
}

#[derive(Serialize)]
struct Span {
    start: Position,
    end: Position,
}

struct Edits<'a> {
    units: &'a [u16],
    edits: &'a [Replace],
}

impl Serialize for Edits<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let lines = Lines::new(self.units);
        serializer.collect_seq(self.edits.iter().map(|edit| TextEdit {
            range: Span { start: lines.position(edit.span.start), end: lines.position(edit.span.end) },
            new_text: String::from_utf16_lossy(&edit.text),
        }))
    }
}

#[derive(Serialize)]
struct Resolved<'a> {
    #[serde(flatten)]
    action: Map<String, Value>,
    edit: WorkspaceEdit<'a>,
}

#[derive(Serialize)]
struct WorkspaceEdit<'a> {
    changes: BTreeMap<&'a str, Edits<'a>>,
}

fn success(id: &Value, result: impl Serialize) -> String {
    serde_json::to_string(&Response { jsonrpc: "2.0", id, result })
        .unwrap_or_else(|_| failure(id, INTERNAL_ERROR, "internal error"))
}

fn failure(id: &Value, code: i64, message: &str) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }).to_string()
}

fn warning(message: &str) -> String {
    json!({ "jsonrpc": "2.0", "method": "window/showMessage", "params": { "type": 2, "message": message } }).to_string()
}

pub fn parse_error() -> String {
    failure(&Value::Null, PARSE_ERROR, "parse error")
}

fn read_position(value: &Value) -> Option<Position> {
    Some(Position {
        line: u32::try_from(value.get("line")?.as_u64()?).ok()?,
        character: u32::try_from(value.get("character")?.as_u64()?).ok()?,
    })
}

fn read_range(value: &Value) -> Option<(Position, Position)> {
    Some((read_position(value.get("start")?)?, read_position(value.get("end")?)?))
}

fn with_edit<'a>(action: &Map<String, Value>, uri: &'a str, edits: Edits<'a>) -> Resolved<'a> {
    let action = action.iter().filter(|(key, _)| *key != "edit").map(|(key, value)| (key.clone(), value.clone()));
    Resolved { action: action.collect(), edit: WorkspaceEdit { changes: BTreeMap::from([(uri, edits)]) } }
}

fn document_uri(params: &Value) -> Option<&str> {
    params.get("textDocument")?.get("uri")?.as_str()
}

fn kind_requested(context: Option<&Value>) -> bool {
    let Some(only) = context.and_then(|value| value.get("only")).and_then(Value::as_array) else {
        return true;
    };
    only.iter().filter_map(Value::as_str).any(|kind| {
        kind.is_empty() || KIND == kind || KIND.strip_prefix(kind).is_some_and(|rest| rest.starts_with('.'))
    })
}

type Target<'a> = (&'a Map<String, Value>, &'a str, i64, Option<(Position, Position)>);

fn read_target(action: &Value) -> Option<Target<'_>> {
    let fields = action.as_object()?;
    let data = fields.get("data")?;
    let uri = data.get("uri")?.as_str()?;
    let version = data.get("version")?.as_i64()?;
    let range = match data.get("range") {
        None | Some(Value::Null) => None,
        Some(range) => Some(read_range(range)?),
    };
    Some((fields, uri, version, range))
}

fn typeset(
    typesetter: &mut Typesetter,
    document: &Document,
    range: Option<(Position, Position)>,
) -> Result<Vec<Replace>, Failure> {
    let format = format_of(&document.language).ok_or(Failure::Internal)?;
    let scope = range.map(|range| document.span(range).ok_or(Failure::Input)).transpose()?;
    typesetter.edits(&document.units, format, scope)
}

impl Server {
    pub fn new() -> Self {
        Self { phase: Phase::Waiting, documents: Documents::default(), typesetter: None }
    }

    pub fn handle(&mut self, message: &Value) -> Step {
        let method = message.get("method").map(Value::as_str);
        let id = message.get("id");
        match (method, id) {
            (Some(Some(method)), Some(id)) if id.is_string() || id.is_i64() || id.is_u64() => {
                let params = message.get("params").unwrap_or(&Value::Null);
                Step { out: self.request(id, method, params), exit: None }
            }
            (Some(Some(_)), Some(_)) => {
                Step { out: vec![failure(&Value::Null, INVALID_REQUEST, "invalid id")], exit: None }
            }
            (Some(Some(method)), None) => {
                let params = message.get("params").unwrap_or(&Value::Null);
                self.notification(method, params)
            }
            (None, Some(id)) => {
                if id == REGISTER_ID && message.get("error").is_some() {
                    log("client/registerCapability", "rejected");
                }
                Step::default()
            }
            (Some(None), _) | (None, None) => {
                Step { out: vec![failure(&Value::Null, INVALID_REQUEST, "invalid message")], exit: None }
            }
        }
    }

    fn request(&mut self, id: &Value, method: &str, params: &Value) -> Vec<String> {
        let message = match (self.phase, method) {
            (Phase::Waiting, "initialize") => success(id, self.initialize(params)),
            (Phase::Waiting, _) => failure(id, NOT_INITIALIZED, "server not initialized"),
            (Phase::Stopping, _) | (Phase::Running, "initialize") => failure(id, INVALID_REQUEST, "invalid request"),
            (Phase::Running, "shutdown") => {
                self.phase = Phase::Stopping;
                success(id, Value::Null)
            }
            (Phase::Running, "textDocument/formatting" | "textDocument/rangeFormatting") => {
                self.format(id, method, params).unwrap_or_else(|reason| {
                    log(method, reason);
                    success(id, [(); 0])
                })
            }
            (Phase::Running, "textDocument/codeAction") => success(id, self.actions(params)),
            (Phase::Running, "codeAction/resolve") => return self.resolve(id, params),
            (Phase::Running, _) => failure(id, METHOD_NOT_FOUND, "method not found"),
        };
        vec![message]
    }

    fn initialize(&mut self, params: &Value) -> Value {
        let settings = config::read(params.get("initializationOptions")).unwrap_or_else(|reason| {
            log("initialize", reason);
            Settings::default()
        });
        self.typesetter = Typesetter::new(settings)
            .or_else(|failure| {
                log("initialize", failure.kind());
                Typesetter::new(Settings::default())
            })
            .ok();
        self.phase = Phase::Running;
        json!({
            "capabilities": {
                "positionEncoding": "utf-16",
                "textDocumentSync": { "openClose": true, "change": 2 },
                "codeActionProvider": { "codeActionKinds": [KIND], "resolveProvider": true },
            },
            "serverInfo": { "name": "microtypo-lsp", "version": env!("CARGO_PKG_VERSION") },
        })
    }

    fn notification(&mut self, method: &str, params: &Value) -> Step {
        if method == "exit" {
            return Step { out: Vec::new(), exit: Some(u8::from(self.phase != Phase::Stopping)) };
        }
        if self.phase != Phase::Running {
            return Step::default();
        }
        match method {
            "initialized" => {
                return Step { out: self.registration().iter().map(Value::to_string).collect(), exit: None };
            }
            "textDocument/didOpen" => self.did_open(params),
            "textDocument/didChange" => self.did_change(params),
            "textDocument/didClose" => {
                if let Some(uri) = document_uri(params) {
                    self.documents.close(uri);
                }
            }
            _ => {}
        }
        Step::default()
    }

    fn registration(&self) -> Option<Value> {
        let languages: Vec<Value> =
            self.typesetter.as_ref()?.registered().map(|language| json!({ "language": language })).collect();
        if languages.is_empty() {
            return None;
        }
        let entry = |method: &str| {
            json!({
                "id": method,
                "method": method,
                "registerOptions": { "documentSelector": languages },
            })
        };
        Some(json!({
            "jsonrpc": "2.0",
            "id": REGISTER_ID,
            "method": "client/registerCapability",
            "params": {
                "registrations": [
                    entry("textDocument/formatting"),
                    entry("textDocument/rangeFormatting"),
                ],
            },
        }))
    }

    fn did_open(&mut self, params: &Value) {
        let opened = (|| {
            let document = params.get("textDocument")?;
            let uri = document.get("uri")?.as_str()?;
            self.documents.close(uri);
            let language = document.get("languageId")?.as_str()?;
            let version = document.get("version")?.as_i64()?;
            let text = document.get("text")?.as_str()?;
            format_of(language)?;
            Some(self.documents.open(uri, language, version, text))
        })();
        if opened != Some(true) {
            log("textDocument/didOpen", "document not tracked");
        }
    }

    fn did_change(&mut self, params: &Value) {
        let Some(uri) = document_uri(params).filter(|uri| self.documents.get(uri).is_some()) else {
            return;
        };
        let parsed = (|| {
            let version = params.get("textDocument")?.get("version")?.as_i64()?;
            let changes = params
                .get("contentChanges")?
                .as_array()?
                .iter()
                .map(|change| {
                    Some(Change {
                        range: match change.get("range") {
                            Some(range) => Some(read_range(range)?),
                            None => None,
                        },
                        text: change.get("text")?.as_str()?,
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            Some((version, changes))
        })();
        if !parsed.is_some_and(|(version, changes)| self.documents.change(uri, version, &changes)) {
            self.documents.close(uri);
            log("textDocument/didChange", "document dropped");
        }
    }

    fn format(&mut self, id: &Value, method: &str, params: &Value) -> Result<String, &'static str> {
        let uri = document_uri(params).ok_or("invalid params")?;
        let range = match method {
            "textDocument/rangeFormatting" => Some(params.get("range").and_then(read_range).ok_or("invalid params")?),
            _ => None,
        };
        let Self { documents, typesetter, .. } = self;
        let document = documents.get(uri).ok_or("document not formattable")?;
        let typesetter = typesetter
            .as_mut()
            .filter(|typesetter| typesetter.formats(&document.language))
            .ok_or("document not formattable")?;
        let edits = typeset(typesetter, document, range).map_err(Failure::kind)?;
        Ok(success(id, Edits { units: &document.units, edits: &edits }))
    }

    fn actions(&self, params: &Value) -> Value {
        let Some(uri) = document_uri(params) else {
            return json!([]);
        };
        let Some(document) = self.documents.get(uri) else {
            return json!([]);
        };
        if !kind_requested(params.get("context")) {
            return json!([]);
        }
        let action = |title: &str, range: Option<&Value>| {
            json!({
                "title": title,
                "kind": KIND,
                "data": { "uri": uri, "version": document.version, "range": range },
            })
        };
        let mut actions = Vec::with_capacity(2);
        if let Some(raw) = params.get("range")
            && let Some(range) = read_range(raw)
            && range.0 != range.1
            && let Some(span) = document.span(range)
            && !trimmed(&document.units, span.clone()).is_empty()
            && span != (0..document.units.len())
        {
            actions.push(action("Microtypo selection", Some(raw)));
        }
        actions.push(action("Microtypo file", None));
        Value::Array(actions)
    }

    fn resolve(&mut self, id: &Value, action: &Value) -> Vec<String> {
        let echo = || success(id, action);
        let Some((fields, uri, version, range)) = read_target(action) else {
            log("codeAction/resolve", "invalid params");
            return vec![echo()];
        };
        let Self { documents, typesetter, .. } = self;
        let Some(document) = documents.get(uri).filter(|document| document.version == version) else {
            return vec![warning("MicroTypo: the document changed, try again"), echo()];
        };
        let edits =
            typesetter.as_mut().ok_or(Failure::Internal).and_then(|typesetter| typeset(typesetter, document, range));
        match edits {
            Ok(edits) if edits.is_empty() => vec![echo()],
            Ok(edits) => vec![success(id, with_edit(fields, uri, Edits { units: &document.units, edits: &edits }))],
            Err(failure) => {
                log("codeAction/resolve", failure.kind());
                vec![warning(failure.message()), echo()]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: i64, method: &str, params: Value) -> Value {
        let mut message = json!({ "jsonrpc": "2.0", "id": id, "method": method });
        message["params"] = params;
        message
    }

    fn notify(method: &str, params: Value) -> Value {
        let mut message = json!({ "jsonrpc": "2.0", "method": method });
        message["params"] = params;
        message
    }

    fn start(options: Value) -> (Server, Step) {
        let mut server = Server::new();
        let mut params = json!({});
        params["initializationOptions"] = options;
        server.handle(&request(1, "initialize", params));
        let step = server.handle(&notify("initialized", json!({})));
        (server, step)
    }

    #[test]
    fn edits_next_to_crlf_keep_valid_positions() {
        let (mut server, _) = start(json!({ "format": ["Plain Text"] }));
        open(&mut server, "plaintext", "\"Амбер\"\r\nКорвин - принц\r\n");
        let step = server.handle(&request(2, "textDocument/formatting", json!({ "textDocument": doc() })));
        assert_eq!(
            result(&step),
            json!([
                { "range": range((0, 0), (0, 1)), "newText": "«" },
                { "range": range((0, 6), (0, 7)), "newText": "»" },
                { "range": range((1, 6), (1, 7)), "newText": "\u{a0}" },
                { "range": range((1, 7), (1, 8)), "newText": "—" },
            ])
        );
    }

    fn open(server: &mut Server, language: &str, text: &str) {
        server.handle(&notify(
            "textDocument/didOpen",
            json!({ "textDocument": {
                "uri": "file:///amber.txt", "languageId": language, "version": 1, "text": text,
            }}),
        ));
    }

    fn doc() -> Value {
        json!({ "uri": "file:///amber.txt" })
    }

    fn range(a: (u32, u32), b: (u32, u32)) -> Value {
        json!({
            "start": { "line": a.0, "character": a.1 },
            "end": { "line": b.0, "character": b.1 },
        })
    }

    fn out(step: &Step) -> Vec<Value> {
        step.out.iter().map(|message| serde_json::from_str(message).unwrap()).collect()
    }

    fn result(step: &Step) -> Value {
        out(step).pop().unwrap()["result"].take()
    }

    #[test]
    fn no_formatter_is_registered_by_default() {
        let (mut server, step) = start(Value::Null);
        assert_eq!(step.out, [] as [String; 0]);
        open(&mut server, "plaintext", "Амбер - город");
        let step = server.handle(&request(2, "textDocument/formatting", json!({ "textDocument": doc() })));
        assert_eq!(result(&step), json!([]));
    }

    #[test]
    fn registers_formatter_only_for_listed_languages() {
        let (_, step) = start(json!({ "format": ["Markdown"] }));
        let message = &out(&step)[0];
        assert_eq!(message["method"], "client/registerCapability");
        let registrations = message["params"]["registrations"].as_array().unwrap();
        assert_eq!(registrations.len(), 2);
        for registration in registrations {
            assert_eq!(registration["registerOptions"]["documentSelector"], json!([{ "language": "markdown" }]));
        }
    }

    #[test]
    fn invalid_options_fall_back_and_register_nothing() {
        let (_, step) = start(json!({ "format": ["Markdown"], "unknown": true }));
        assert_eq!(step.out, [] as [String; 0]);
        let (_, step) = start(json!({ "format": ["Markdown"], "rules": { "no.such.rule": true } }));
        assert_eq!(step.out, [] as [String; 0]);
    }

    #[test]
    fn formats_listed_language_and_skips_others() {
        let (mut server, _) = start(json!({ "format": ["Plain Text"] }));
        open(&mut server, "plaintext", "Амбер - город");
        let step = server.handle(&request(2, "textDocument/formatting", json!({ "textDocument": doc() })));
        assert_eq!(
            result(&step),
            json!([
                { "range": range((0, 5), (0, 6)), "newText": "\u{a0}" },
                { "range": range((0, 6), (0, 7)), "newText": "—" },
            ])
        );

        let (mut server, _) = start(json!({ "format": ["Markdown"] }));
        open(&mut server, "plaintext", "Амбер - город");
        let step = server.handle(&request(2, "textDocument/formatting", json!({ "textDocument": doc() })));
        assert_eq!(result(&step), json!([]));
    }

    #[test]
    fn range_edits_stay_inside_the_selection() {
        let (mut server, _) = start(json!({ "format": ["Plain Text"] }));
        open(&mut server, "plaintext", "Корвин - принц\nРэндом - король\nБлейз - брат\n");
        let step = server.handle(&request(
            2,
            "textDocument/rangeFormatting",
            json!({ "textDocument": doc(), "range": range((1, 0), (1, 15)) }),
        ));
        let edits = result(&step).as_array().cloned().unwrap();
        assert_ne!(edits.as_slice(), [] as [String; 0]);
        for edit in edits {
            assert_eq!(edit["range"]["start"]["line"], 1);
            assert_eq!(edit["range"]["end"]["line"], 1);
        }
    }

    #[test]
    fn offers_actions_without_computing_edits() {
        let (mut server, _) = start(Value::Null);
        open(&mut server, "plaintext", "Амбер - город");
        let step = server.handle(&request(
            2,
            "textDocument/codeAction",
            json!({ "textDocument": doc(), "range": range((0, 0), (0, 5)), "context": { "diagnostics": [] } }),
        ));
        let actions = result(&step).as_array().cloned().unwrap();
        let titles: Vec<_> = actions.iter().map(|action| action["title"].as_str().unwrap()).collect();
        assert_eq!(titles, ["Microtypo selection", "Microtypo file"]);
        assert!(actions.iter().all(|action| action.get("edit").is_none()));
    }

    #[test]
    fn empty_cursor_offers_only_the_file_action() {
        let (mut server, _) = start(Value::Null);
        open(&mut server, "plaintext", "Амбер - город");
        let step = server.handle(&request(
            2,
            "textDocument/codeAction",
            json!({ "textDocument": doc(), "range": range((0, 3), (0, 3)), "context": {} }),
        ));
        let actions = result(&step).as_array().cloned().unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0]["title"], "Microtypo file");
    }

    #[test]
    fn whole_buffer_range_offers_only_the_file_action() {
        let (mut server, _) = start(Value::Null);
        open(&mut server, "plaintext", "Амбер - город");
        let step = server.handle(&request(
            2,
            "textDocument/codeAction",
            json!({ "textDocument": doc(), "range": range((0, 0), (0, 13)), "context": {} }),
        ));
        assert_eq!(result(&step).as_array().unwrap().len(), 1);
    }

    #[test]
    fn filters_actions_by_requested_kind() {
        let (mut server, _) = start(Value::Null);
        open(&mut server, "plaintext", "Амбер - город");
        for (only, expected) in [
            (json!(["quickfix"]), 0),
            (json!(["refactorx"]), 0),
            (json!(["refactor"]), 1),
            (json!(["refactor.rewrite"]), 1),
            (json!([KIND]), 1),
        ] {
            let step = server.handle(&request(
                2,
                "textDocument/codeAction",
                json!({ "textDocument": doc(), "range": range((0, 0), (0, 0)), "context": { "only": only } }),
            ));
            assert_eq!(result(&step).as_array().unwrap().len(), expected, "{only}");
        }
    }

    #[test]
    fn resolve_returns_edit_for_the_selection() {
        let (mut server, _) = start(Value::Null);
        open(&mut server, "plaintext", "Корвин - принц. Рэндом - король.");
        let step = server.handle(&request(
            2,
            "textDocument/codeAction",
            json!({ "textDocument": doc(), "range": range((0, 16), (0, 31)), "context": {} }),
        ));
        let action = result(&step)[0].clone();
        let step = server.handle(&request(3, "codeAction/resolve", action));
        let edits = result(&step)["edit"]["changes"]["file:///amber.txt"].as_array().cloned().unwrap();
        assert_ne!(edits.as_slice(), [] as [String; 0]);
        for edit in edits {
            assert!(edit["range"]["start"]["character"].as_u64().unwrap() >= 16);
        }
    }

    #[test]
    fn resolve_refuses_a_stale_document() {
        let (mut server, _) = start(Value::Null);
        open(&mut server, "plaintext", "Амбер - город");
        let step = server.handle(&request(
            2,
            "textDocument/codeAction",
            json!({ "textDocument": doc(), "range": range((0, 0), (0, 0)), "context": {} }),
        ));
        let action = result(&step)[0].clone();
        server.handle(&notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": "file:///amber.txt", "version": 2 },
                "contentChanges": [{ "text": "Корвин - принц" }],
            }),
        ));
        let step = server.handle(&request(3, "codeAction/resolve", action));
        assert_eq!(out(&step)[0]["method"], "window/showMessage");
        assert!(result(&step).get("edit").is_none());
    }

    #[test]
    fn resolve_reports_malformed_document() {
        let (mut server, _) = start(Value::Null);
        open(&mut server, "json", "{\"a\": ");
        let step = server.handle(&request(
            2,
            "textDocument/codeAction",
            json!({ "textDocument": doc(), "range": range((0, 0), (0, 0)), "context": {} }),
        ));
        let action = result(&step)[0].clone();
        let step = server.handle(&request(3, "codeAction/resolve", action));
        assert_eq!(out(&step)[0]["params"]["message"], Failure::Input.message());
        assert!(result(&step).get("edit").is_none());
    }

    #[test]
    fn untracked_language_gets_no_actions() {
        let (mut server, _) = start(Value::Null);
        open(&mut server, "rust", "let a = \"Амбер - город\";");
        let step = server.handle(&request(
            2,
            "textDocument/codeAction",
            json!({ "textDocument": doc(), "range": range((0, 0), (0, 0)), "context": {} }),
        ));
        assert_eq!(result(&step), json!([]));
    }

    #[test]
    fn follows_the_lifecycle() {
        let mut server = Server::new();
        let step = server.handle(&request(1, "shutdown", Value::Null));
        assert_eq!(out(&step)[0]["error"]["code"], NOT_INITIALIZED);

        server.handle(&request(2, "initialize", json!({})));
        let step = server.handle(&request(3, "initialize", json!({})));
        assert_eq!(out(&step)[0]["error"]["code"], INVALID_REQUEST);
        let step = server.handle(&request(4, "no/such/method", Value::Null));
        assert_eq!(out(&step)[0]["error"]["code"], METHOD_NOT_FOUND);

        let step = server.handle(&request(5, "shutdown", Value::Null));
        assert_eq!(result(&step), Value::Null);
        let step = server.handle(&request(6, "textDocument/formatting", json!({})));
        assert_eq!(out(&step)[0]["error"]["code"], INVALID_REQUEST);
        assert_eq!(server.handle(&notify("exit", Value::Null)).exit, Some(0));
    }

    #[test]
    fn exit_without_shutdown_is_an_error() {
        let (mut server, _) = start(Value::Null);
        assert_eq!(server.handle(&notify("exit", Value::Null)).exit, Some(1));
    }

    #[test]
    fn rejects_invalid_ids_and_ignores_responses() {
        let (mut server, _) = start(Value::Null);
        let step = server.handle(&json!({ "id": {}, "method": "shutdown" }));
        assert_eq!(out(&step)[0]["error"]["code"], INVALID_REQUEST);
        let step = server.handle(&json!({ "id": REGISTER_ID, "result": null }));
        assert_eq!(step.out, [] as [String; 0]);
        let step = server.handle(&json!({ "id": 3, "method": 5 }));
        assert_eq!(out(&step)[0]["error"]["code"], INVALID_REQUEST);
    }

    #[test]
    fn malformed_change_drops_the_document() {
        for change in [
            json!({ "textDocument": doc(), "contentChanges": [{ "text": "Корвин - принц" }] }),
            json!({ "textDocument": { "uri": "file:///amber.txt", "version": 2 }, "contentChanges": { "text": "Корвин" } }),
            json!({ "textDocument": { "uri": "file:///amber.txt", "version": 2 }, "contentChanges": [{ "range": {} }] }),
        ] {
            let (mut server, _) = start(json!({ "format": ["Plain Text"] }));
            open(&mut server, "plaintext", "Амбер - город");
            server.handle(&notify("textDocument/didChange", change));
            let step = server.handle(&request(2, "textDocument/formatting", json!({ "textDocument": doc() })));
            assert_eq!(result(&step), json!([]));
        }
    }

    #[test]
    fn reopening_as_an_untracked_language_drops_the_document() {
        let (mut server, _) = start(json!({ "format": ["Plain Text"] }));
        open(&mut server, "plaintext", "Амбер - город");
        open(&mut server, "rust", "let a = \"Амбер - город\";");
        let step = server.handle(&request(2, "textDocument/formatting", json!({ "textDocument": doc() })));
        assert_eq!(result(&step), json!([]));
    }

    #[test]
    fn writes_edits_as_valid_json() {
        let units: Vec<u16> = "a\r\nb".encode_utf16().collect();
        let edits = [Replace { span: 3..4, text: "\"\\\n\u{1}😀".encode_utf16().collect() }];
        let edit = json!({ "range": range((1, 0), (1, 1)), "newText": "\"\\\n\u{1}😀" });
        let written = success(&json!(1), Edits { units: &units, edits: &edits });
        assert_eq!(serde_json::from_str::<Value>(&written).unwrap()["result"], json!([edit]));
        assert_eq!(success(&json!(1), Edits { units: &units, edits: &[] }), r#"{"jsonrpc":"2.0","id":1,"result":[]}"#);

        let action = json!({ "title": "Microtypo file", "edit": "stale" });
        let resolved = with_edit(action.as_object().unwrap(), "file:///a\"b", Edits { units: &units, edits: &edits });
        assert_eq!(
            serde_json::from_str::<Value>(&success(&json!(7), resolved)).unwrap(),
            json!({ "jsonrpc": "2.0", "id": 7, "result": {
                "title": "Microtypo file", "edit": { "changes": { "file:///a\"b": [edit] } },
            }})
        );
    }
}
