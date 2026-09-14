//! A minimal LSP server (`cato lsp`): JSON-RPC over stdio, no deps.
//!
//! Capabilities: full-text sync diagnostics (recompiling on every change
//! through the real frontend, so error codes/locations are exact),
//! go-to-definition and hover over `defn` names. Enough to wire an editor
//! in; not a replacement for a real LSP stack.

use cato_core::Backend;
use std::collections::HashMap;
use std::io::{Read, Write};

// ---------------------------------------------------------------------------
// Tiny JSON: parse what LSP clients send, serialize what we answer.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
    fn as_num(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }
    fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(v) => Some(v),
            _ => None,
        }
    }

    fn esc(s: &str) -> String {
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t")
    }

    fn ser(&self) -> String {
        match self {
            Json::Null => "null".into(),
            Json::Bool(b) => b.to_string(),
            Json::Num(n) => {
                if n.fract() == 0.0 && n.abs() < 1e15 {
                    format!("{}", *n as i64)
                } else {
                    format!("{}", n)
                }
            }
            Json::Str(s) => format!("\"{}\"", Self::esc(s)),
            Json::Arr(v) => format!("[{}]", v.iter().map(|j| j.ser()).collect::<Vec<_>>().join(",")),
            Json::Obj(pairs) => format!(
                "{{{}}}",
                pairs
                    .iter()
                    .map(|(k, v)| format!("\"{}\":{}", Self::esc(k), v.ser()))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        }
    }
}

fn parse_json(s: &str) -> Option<Json> {
    let b = s.as_bytes();
    let mut i = 0usize;
    let v = parse_val(s, b, &mut i)?;
    Some(v)
}

fn skip_ws(b: &[u8], i: &mut usize) {
    while *i < b.len() && (b[*i] as char).is_ascii_whitespace() {
        *i += 1;
    }
}

fn parse_val(s: &str, b: &[u8], i: &mut usize) -> Option<Json> {
    skip_ws(b, i);
    match *b.get(*i)? {
        b'{' => {
            *i += 1;
            let mut pairs = Vec::new();
            loop {
                skip_ws(b, i);
                if *b.get(*i)? == b'}' {
                    *i += 1;
                    return Some(Json::Obj(pairs));
                }
                let k = parse_val(s, b, i)?;
                skip_ws(b, i);
                if *b.get(*i)? != b':' {
                    return None;
                }
                *i += 1;
                let v = parse_val(s, b, i)?;
                pairs.push((k.as_str()?.to_string(), v));
                skip_ws(b, i);
                match *b.get(*i)? {
                    b',' => {
                        *i += 1;
                    }
                    b'}' => {
                        *i += 1;
                        return Some(Json::Obj(pairs));
                    }
                    _ => return None,
                }
            }
        }
        b'[' => {
            *i += 1;
            let mut items = Vec::new();
            loop {
                skip_ws(b, i);
                if *b.get(*i)? == b']' {
                    *i += 1;
                    return Some(Json::Arr(items));
                }
                items.push(parse_val(s, b, i)?);
                skip_ws(b, i);
                match *b.get(*i)? {
                    b',' => {
                        *i += 1;
                    }
                    b']' => {
                        *i += 1;
                        return Some(Json::Arr(items));
                    }
                    _ => return None,
                }
            }
        }
        b'"' => {
            *i += 1;
            let start = *i;
            while *i < b.len() {
                if b[*i] == b'\\' {
                    *i += 2;
                    continue;
                }
                if b[*i] == b'"' {
                    break;
                }
                *i += 1;
            }
            let raw = &s[start..(*i).min(s.len())];
            *i += 1;
            // Unescape the common cases.
            Some(Json::Str(
                raw.replace("\\n", "\n")
                    .replace("\\r", "\r")
                    .replace("\\t", "\t")
                    .replace("\\\"", "\"")
                    .replace("\\\\", "\\"),
            ))
        }
        b't' => {
            *i += 4;
            Some(Json::Bool(true))
        }
        b'f' => {
            *i += 5;
            Some(Json::Bool(false))
        }
        b'n' => {
            *i += 4;
            Some(Json::Null)
        }
        _ => {
            let start = *i;
            while *i < b.len() && !" \t\r\n,]}".contains(b[*i] as char) {
                *i += 1;
            }
            s[start..*i].parse::<f64>().ok().map(Json::Num)
        }
    }
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

struct Doc {
    text: String,
}

fn send(method: &str, params: &Json) {
    let body = Json::Obj(vec![
        ("jsonrpc".into(), Json::Str("2.0".into())),
        ("method".into(), Json::Str(method.into())),
        ("params".into(), params.clone()),
    ])
    .ser();
    print!("Content-Length: {}\r\n\r\n{}", body.len(), body);
    std::io::stdout().flush().unwrap();
}

fn respond(id: &Json, result: Json) {
    let body = Json::Obj(vec![
        ("jsonrpc".into(), Json::Str("2.0".into())),
        ("id".into(), id.clone()),
        ("result".into(), result),
    ])
    .ser();
    print!("Content-Length: {}\r\n\r\n{}", body.len(), body);
    std::io::stdout().flush().unwrap();
}

/// Compile `text` and publish diagnostics (message + parsed location).
/// The frontend returns Result; the backend panics on unknown functions
/// and unsupported constructs, so it runs under catch_unwind and its
/// panic message becomes a diagnostic.
fn publish_diagnostics(uri: &str, text: &str) {
    let frontend = cato_core::compile_to_program(text);
    let backend = frontend.as_ref().ok().map(|p| {
        let p = p.clone();
        std::panic::catch_unwind(move || {
            cato_llvm::LlvmBackend { fast: false, threads: 1 }.emit(&p)
        })
    });
    let diags = match frontend {
        Err(e) => vec![mk_diag_from_msg(&e)],
        Ok(_) => match backend {
            Some(Ok(Ok(_))) => Vec::new(),
            Some(Ok(Err(e))) => vec![mk_diag(1, 1, e)],
            Some(Err(panic)) => {
                let msg = panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "backend error".into());
                vec![mk_diag(1, 1, format!("backend: {}", msg))]
            }
            None => Vec::new(),
        },
    };
    send(
        "textDocument/publishDiagnostics",
        &Json::Obj(vec![
            ("uri".into(), Json::Str(uri.into())),
            ("diagnostics".into(), Json::Arr(diags)),
        ]),
    );
}

fn mk_diag(line: usize, col: usize, message: String) -> Json {
    Json::Obj(vec![
        (
            "range".into(),
            Json::Obj(vec![
                (
                    "start".into(),
                    Json::Obj(vec![
                        ("line".into(), Json::Num(line as f64 - 1.0)),
                        ("character".into(), Json::Num(col as f64 - 1.0)),
                    ]),
                ),
                (
                    "end".into(),
                    Json::Obj(vec![
                        ("line".into(), Json::Num(line as f64)),
                        ("character".into(), Json::Num(0.0)),
                    ]),
                ),
            ]),
        ),
        ("severity".into(), Json::Num(1.0)),
        ("source".into(), Json::Str("cato".into())),
        ("message".into(), Json::Str(message)),
    ])
}

fn mk_diag_from_msg(e: &str) -> Json {
    let (line, col) = e
        .lines()
        .find_map(|l| l.trim().strip_prefix("--> "))
        .and_then(|loc| {
            let mut it = loc.rsplit(':');
            let col: usize = it.next()?.parse().ok()?;
            let line: usize = it.next()?.parse().ok()?;
            Some((line, col))
        })
        .unwrap_or((1, 1));
    mk_diag(line, col, e.to_string())
}


pub fn run() {
    let mut docs: HashMap<String, Doc> = HashMap::new();
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input).unwrap();
    let mut pos = 0usize;
    while pos < input.len() {
        // Header: Content-Length: N\r\n(\r\n)*
        let mut header_end = pos;
        while header_end + 3 < input.len()
            && &input[header_end..header_end + 4] != b"\r\n\r\n"
        {
            header_end += 1;
        }
        if header_end + 4 > input.len() {
            break;
        }
        let header = String::from_utf8_lossy(&input[pos..header_end]).to_string();
        let len: usize = header
            .lines()
            .find_map(|l| l.strip_prefix("Content-Length: "))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0);
        let body_start = header_end + 4;
        let body_end = (body_start + len).min(input.len());
        let body = String::from_utf8_lossy(&input[body_start..body_end]).to_string();
        pos = body_end;

        let Some(msg) = parse_json(&body) else { continue };
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = msg.get("id").cloned().unwrap_or(Json::Null);
        let params = msg.get("params").cloned().unwrap_or(Json::Null);

        match method {
            "initialize" => respond(
                &id,
                Json::Obj(vec![(
                    "capabilities".into(),
                    Json::Obj(vec![
                        ("textDocumentSync".into(), Json::Num(1.0)),
                        ("definitionProvider".into(), Json::Bool(true)),
                        ("hoverProvider".into(), Json::Bool(true)),
                    ]),
                )]),
            ),
            "shutdown" => respond(&id, Json::Null),
            "exit" => return,
            "textDocument/didOpen" | "textDocument/didChange" => {
                let td = params.get("textDocument");
                let uri = td.and_then(|t| t.get("uri")).and_then(|u| u.as_str()).unwrap_or("");
                let content = if method == "textDocument/didOpen" {
                    params.get("textDocument").and_then(|t| t.get("text")).and_then(|t| t.as_str())
                } else {
                    params
                        .get("contentChanges")
                        .and_then(|c| c.as_arr())
                        .and_then(|a| a.last())
                        .and_then(|c| c.get("text"))
                        .and_then(|t| t.as_str())
                };
                if let Some(text) = content {
                    docs.insert(uri.to_string(), Doc { text: text.to_string() });
                    publish_diagnostics(uri, text);
                }
            }
            "textDocument/definition" | "textDocument/hover" => {
                let uri = params
                    .get("textDocument")
                    .and_then(|t| t.get("uri"))
                    .and_then(|u| u.as_str())
                    .unwrap_or("");
                let posj = params.get("position");
                let line = posj.and_then(|p| p.get("line")).and_then(|l| l.as_num()).unwrap_or(0.0) as usize;
                let ch = posj
                    .and_then(|p| p.get("character"))
                    .and_then(|l| l.as_num())
                    .unwrap_or(0.0) as usize;
                let text = docs.get(uri).map(|d| d.text.clone()).unwrap_or_default();
                let word = word_at(&text, offset_of(&text, line, ch)).unwrap_or_default();
                match find_defn(&text, &word) {
                    Some((ln, sig)) if method == "textDocument/definition" => respond(
                        &id,
                        Json::Obj(vec![
                            ("uri".into(), Json::Str(uri.into())),
                            (
                                "range".into(),
                                Json::Obj(vec![
                                    (
                                        "start".into(),
                                        Json::Obj(vec![
                                            ("line".into(), Json::Num(ln as f64)),
                                            ("character".into(), Json::Num(0.0)),
                                        ]),
                                    ),
                                    (
                                        "end".into(),
                                        Json::Obj(vec![
                                            ("line".into(), Json::Num(ln as f64)),
                                            ("character".into(), Json::Num(999.0)),
                                        ]),
                                    ),
                                ]),
                            ),
                        ]),
                    ),
                    Some((_, sig)) if method == "textDocument/hover" => respond(
                        &id,
                        Json::Obj(vec![(
                            "contents".into(),
                            Json::Obj(vec![
                                ("kind".into(), Json::Str("markdown".into())),
                                ("value".into(), Json::Str(format!("```cato\n{}\n```", sig))),
                            ]),
                        )]),
                    ),
                    _ => respond(&id, Json::Null),
                }
            }
            _ => {}
        }
    }
}

/// (line, character) -> byte offset in `text`.
fn offset_of(text: &str, line: usize, character: usize) -> usize {
    let mut ln = 0usize;
    for (i, b) in text.bytes().enumerate() {
        if ln == line {
            return (i + character).min(text.len());
        }
        if b == b'\n' {
            ln += 1;
        }
    }
    text.len()
}

/// The word at a byte offset (identifier chars).
fn word_at(text: &str, off: usize) -> Option<String> {
    let b = text.as_bytes();
    let is_word = |c: u8| c.is_ascii_alphanumeric() || c == b'-' || c == b'?' || c == b'!';
    let off = off.min(b.len());
    let mut start = off;
    while start > 0 && is_word(b[start - 1]) {
        start -= 1;
    }
    let mut end = off;
    while end < b.len() && is_word(b[end]) {
        end += 1;
    }
    if start >= end {
        None
    } else {
        Some(text[start..end].to_string())
    }
}

/// Find `(defn name ...)` in the text; returns its 0-based line and the
/// signature line text.
fn find_defn(text: &str, name: &str) -> Option<(usize, String)> {
    for (ln, line) in text.lines().enumerate() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("(defn ") {
            if let Some(sym) = rest.split_whitespace().next() {
                if sym == name {
                    return Some((ln, t.to_string()));
                }
            }
        }
    }
    None
}
