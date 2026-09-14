//! The `cato repl` session: a form-at-a-time frontend over the interpreter.
//!
//! Interaction model mirrors a Clojure REPL: the prompt is the current
//! namespace (`user=> `), input may span lines until the parens balance
//! (continuation prompt `#_=> `), definitions echo `#'user/name`, and
//! results print on their own line. Definitions (`defn`, `deftype`,
//! `defgeneric`, `defmethod`, `extern`, `require`) accumulate into the
//! session program; any other form is evaluated immediately by compiling
//! the session plus a synthetic `__repl` wrapper and running it in the
//! interpreter. Everything recompiles per entry — slow at very large
//! session sizes, exact in semantics, and zero extra machinery.
//!
//! Line editing (arrows, history, home/end, Ctrl-A/E/K/U/W/L) is
//! hand-rolled on `stty` raw mode — no external dependencies. When stdin
//! is not a terminal (piped scripts, tests) or `stty` is unavailable, the
//! REPL falls back to plain line reading with the same evaluation logic.

use cato_core::ast::Expr;
use cato_core::reader::read_all_spanned;
use cato_core::compile_named;
use cato_interp::fmt_value;
use std::io::{BufRead, IsTerminal, Read, Write};

/// Heads whose forms accumulate into the session program.
const DEF_HEADS: [&str; 6] = ["defn", "deftype", "extern", "defgeneric", "defmethod", "require"];
/// Heads whose forms echo a `#'ns/name` confirmation.
const NAMED_HEADS: [&str; 3] = ["defn", "defgeneric", "deftype"];
const HISTORY_FILE: &str = ".cato_history";
const HISTORY_MAX: usize = 1000;

pub fn run() {
    let colors = crate::ui::stdout_color_enabled();
    let interactive = std::io::stdin().is_terminal();
    let history = if interactive { load_history() } else { Vec::new() };

    // Plain startup lines: no ANSI, so the first line is exactly
    // "Cato REPL <version>" on every terminal.
    println!("Cato REPL {}", env!("CARGO_PKG_VERSION"));
    println!(
        "definitions accumulate, forms evaluate via the interpreter; :quit or Ctrl-D exits"
    );

    let mut session = Session::new();
    let mut pending = String::new();
    let mut last_interrupt = false;
    let mut reader = LineReader::new(interactive, history);

    loop {
        let prompt = if pending.is_empty() {
            format!("{}{} ", session.ns, crate::ui::paint(colors, "1;36", "=>"))
        } else {
            format!(
                "{}{} ",
                crate::ui::paint(colors, "2", "#_"),
                crate::ui::paint(colors, "1;36", "=>")
            )
        };
        match reader.read_line(&prompt) {
            ReadOutcome::Eof => {
                if pending.is_empty() {
                    println!();
                    break;
                }
                // Ctrl-D mid-form: evaluate what we have, then exit.
                evaluate(&pending, &mut session, colors);
                break;
            }
            ReadOutcome::Interrupt => {
                let was_top_level = pending.is_empty();
                pending.clear();
                println!();
                // Two consecutive Ctrl-C at an empty prompt also exits —
                // one abandons the (possibly pending) form.
                if was_top_level && last_interrupt {
                    println!("{}", crate::ui::paint(colors, "2", "bye"));
                    break;
                }
                last_interrupt = was_top_level;
                continue;
            }
            ReadOutcome::Line(line) => {
                last_interrupt = false;
                let trimmed = line.trim();
                if pending.is_empty() {
                    if trimmed.is_empty() {
                        continue;
                    }
                    if matches!(trimmed, ":quit" | ":q" | ":exit" | "exit" | "quit") {
                        break;
                    }
                    if trimmed == ":help" {
                        repl_help(colors);
                        continue;
                    }
                }
                pending.push_str(&line);
                pending.push('\n');
                match paren_balance(&pending) {
                    b if b > 0 => continue, // keep reading: form still open
                    b if b < 0 => {
                        repl_error(colors, "unbalanced closing paren");
                        pending.clear();
                        continue;
                    }
                    _ => {}
                }
                let entry = std::mem::take(&mut pending);
                evaluate(&entry, &mut session, colors);
            }
        }
    }

    if interactive {
        save_history(&reader.history());
    }
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Session {
    /// Accumulated definition forms (ns forms excluded).
    parts: Vec<String>,
    /// The most recent `(ns ...)` form; only one is kept, prepended on
    /// compile (a second `ns` form would fail the duplicate-ns check).
    ns_form: Option<String>,
    /// Current namespace, shown in the prompt.
    ns: String,
}

impl Session {
    fn new() -> Session {
        Session {
            parts: Vec::new(),
            ns_form: None,
            ns: "user".to_string(),
        }
    }
    fn source(&self) -> String {
        let mut src = String::new();
        if let Some(ns) = &self.ns_form {
            src.push_str(ns);
            src.push('\n');
        }
        for p in &self.parts {
            src.push_str(p);
            src.push('\n');
        }
        src
    }

    fn compile(&self) -> Result<cato_core::Program, String> {
        compile_named("<repl>", &self.source())
    }
}

fn evaluate(entry: &str, session: &mut Session, colors: bool) {
    let forms = crate::ui::guard(|| read_all_spanned(entry).map_err(|e| e.msg));
    let (exprs, spans) = match forms {
        Ok(pair) => pair,
        Err(e) => {
            repl_error(colors, &e);
            return;
        }
    };
    for (expr, span) in exprs.into_iter().zip(spans) {
        let text = &entry[span.lo..span.hi];
        process_form(text, &expr, session, colors);
    }
}

fn process_form(text: &str, expr: &Expr, session: &mut Session, colors: bool) {
    let head = match expr {
        Expr::List(items) => match items.first() {
            Some(Expr::LitSym(h)) => h.clone(),
            _ => String::new(),
        },
        _ => String::new(),
    };

    if head == "ns" {
        let name = match expr {
            Expr::List(items) => match items.get(1) {
                Some(Expr::LitSym(n)) => n.clone(),
                _ => String::new(),
            },
            _ => String::new(),
        };
        let previous = (session.ns_form.clone(), session.ns.clone());
        session.ns_form = Some(text.to_string());
        session.ns = name;
        if let Err(e) = crate::ui::guard(|| session.compile()) {
            (session.ns_form, session.ns) = previous;
            repl_error(colors, &e);
        }
        return;
    }

    if DEF_HEADS.contains(&head.as_str()) {
        session.parts.push(text.to_string());
        match crate::ui::guard(|| session.compile()) {
            Ok(_) => {
                if NAMED_HEADS.contains(&head.as_str()) {
                    if let Expr::List(items) = expr {
                        if let Some(Expr::LitSym(n)) = items.get(1) {
                            repl_defined(colors, &session.ns, n);
                        }
                    }
                }
            }
            Err(e) => {
                // Roll back the failed definition.
                session.parts.pop();
                repl_error(colors, &e);
            }
        }
        return;
    }

    // Expression: wrap in a synthetic defn and run it. Both compile and
    // execution are panic-guarded: malformed input must cost an error
    // message, never the session.
    let src = format!("{}\n(defn __repl [] {})", session.source(), text);
    let run = crate::ui::guard(move || {
        let program = compile_named("<repl>", &src)?;
        cato_interp::Interpreter::new(&program).run_entry("__repl")
    });
    match run {
        Ok((v, out)) => {
            if !out.is_empty() {
                print!("{}", out);
            }
            println!("{}", crate::ui::paint(colors, "1", &fmt_value(&v)));
        }
        Err(e) => repl_error(colors, &e),
    }
}

fn repl_defined(colors: bool, ns: &str, name: &str) {
    println!(
        "{}",
        crate::ui::paint(colors, "2;32", &format!("#'{}/{}", ns, name))
    );
}

fn repl_error(colors: bool, msg: &str) {
    println!(
        "{} {}",
        crate::ui::paint(colors, "1;31", "error:"),
        crate::ui::paint(colors, "0;31", msg)
    );
}

fn repl_help(colors: bool) {
    let dim = |s: &str| crate::ui::paint(colors, "2", s);
    println!(":quit, :q, :exit, exit, quit   leave the REPL (also Ctrl-D on an empty line)");
    println!("Ctrl-C       abandon the current form; twice on an empty prompt exits");
    println!("Ctrl-L       clear the screen");
    println!(
        "{}",
        dim("forms may span lines: the REPL keeps reading until the parens balance")
    );
}

// ---------------------------------------------------------------------------
// Reading: multi-line accumulation over either editor
// ---------------------------------------------------------------------------

enum ReadOutcome {
    Line(String),
    Eof,
    Interrupt,
}

struct LineReader {
    interactive: bool,
    history: Vec<String>,
}

impl LineReader {
    fn new(interactive: bool, history: Vec<String>) -> LineReader {
        LineReader { interactive, history }
    }

    fn history(&self) -> Vec<String> {
        self.history.clone()
    }

    fn read_line(&mut self, prompt: &str) -> ReadOutcome {
        if self.interactive {
            match read_line_raw(prompt, &self.history) {
                Ok(outcome) => {
                    if let ReadOutcome::Line(line) = &outcome {
                        self.remember(line);
                    }
                    return outcome;
                }
                Err(_) => self.interactive = false, // stty failed: fall through
            }
        }
        let outcome = read_line_plain(prompt);
        if let ReadOutcome::Line(line) = &outcome {
            self.remember(line);
        }
        outcome
    }

    fn remember(&mut self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        if self.history.last().map(|l| l == line).unwrap_or(false) {
            return;
        }
        self.history.push(line.to_string());
    }
}

/// Plain (non-TTY) reading: prompt, one line from stdin. The line is
/// echoed so piped sessions produce readable transcripts.
fn read_line_plain(prompt: &str) -> ReadOutcome {
    print!("{prompt}");
    std::io::stdout().flush().unwrap();
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) => ReadOutcome::Eof,
        Ok(_) => {
            while line.ends_with('\n') || line.ends_with('\r') {
                line.pop();
            }
            println!("{line}");
            ReadOutcome::Line(line)
        }
        Err(_) => ReadOutcome::Eof,
    }
}

// ---------------------------------------------------------------------------
// Raw-mode line editor (arrows, history, home/end, kill lines) on stty
// ---------------------------------------------------------------------------

fn stty(args: &[&str]) -> std::io::Result<String> {
    use std::process::{Command, Stdio};
    let out = Command::new("stty")
        .args(args)
        .stdin(Stdio::inherit())
        .stderr(Stdio::null())
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "stty failed",
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn read_byte(stdin: &mut impl Read) -> Option<u8> {
    let mut b = [0u8; 1];
    match stdin.read(&mut b) {
        Ok(1) => Some(b[0]),
        _ => None,
    }
}

struct Editor {
    buf: Vec<char>,
    cursor: usize,
}

impl Editor {
    fn redraw(&self, prompt: &str) {
        let mut out = std::io::stdout().lock();
        let text: String = self.buf.iter().collect();
        let _ = write!(out, "\r\x1b[K{prompt}{text}\x1b[K");
        let from_end = self.buf.len() - self.cursor;
        if from_end > 0 {
            let _ = write!(out, "\x1b[{from_end}D");
        }
        let _ = out.flush();
    }

    fn insert(&mut self, c: char, prompt: &str) {
        self.buf.insert(self.cursor, c);
        self.cursor += 1;
        self.redraw(prompt);
    }

    fn backspace(&mut self, prompt: &str) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.buf.remove(self.cursor);
            self.redraw(prompt);
        }
    }

    fn delete(&mut self, prompt: &str) {
        if self.cursor < self.buf.len() {
            self.buf.remove(self.cursor);
            self.redraw(prompt);
        }
    }

    fn set_text(&mut self, text: &str, prompt: &str) {
        self.buf = text.chars().collect();
        self.cursor = self.buf.len();
        self.redraw(prompt);
    }

    fn kill_to_end(&mut self, prompt: &str) {
        self.buf.truncate(self.cursor);
        self.redraw(prompt);
    }

    fn kill_to_start(&mut self, prompt: &str) {
        self.buf.drain(..self.cursor);
        self.cursor = 0;
        self.redraw(prompt);
    }

    fn kill_word(&mut self, prompt: &str) {
        let mut i = self.cursor;
        while i > 0 && self.buf[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !self.buf[i - 1].is_whitespace() {
            i -= 1;
        }
        self.buf.drain(i..self.cursor);
        self.cursor = i;
        self.redraw(prompt);
    }
}

/// Read one line with full editing; the terminal is left in cooked mode on
/// every return path. Errors mean `stty` could not be driven — the caller
/// falls back to plain reading.
fn read_line_raw(prompt: &str, history: &[String]) -> std::io::Result<ReadOutcome> {
    let saved = stty(&["-g"])?;
    stty(&["raw", "-echo"])?;
    let result = read_line_raw_inner(prompt, history);
    stty(&[&saved])?;
    result
}

fn read_line_raw_inner(prompt: &str, history: &[String]) -> std::io::Result<ReadOutcome> {
    use std::io::Write as _;
    let mut stdin = std::io::stdin().lock();
    let mut ed = Editor {
        buf: Vec::new(),
        cursor: 0,
    };
    let mut hidx = history.len();
    ed.redraw(prompt);

    loop {
        let Some(b) = read_byte(&mut stdin) else {
            return Ok(ReadOutcome::Eof);
        };
        match b {
            b'\r' | b'\n' => {
                println!();
                return Ok(ReadOutcome::Line(ed.buf.into_iter().collect()));
            }
            0x03 => {
                // Ctrl-C: abandon the current line.
                println!();
                return Ok(ReadOutcome::Interrupt);
            }
            0x04 => {
                // Ctrl-D: EOF on an empty line, delete char otherwise.
                if ed.buf.is_empty() {
                    return Ok(ReadOutcome::Eof);
                }
                ed.delete(prompt);
            }
            0x7f | 0x08 => ed.backspace(prompt),
            0x01 => {
                ed.cursor = 0;
                ed.redraw(prompt);
            }
            0x05 => {
                ed.cursor = ed.buf.len();
                ed.redraw(prompt);
            }
            0x0b => ed.kill_to_end(prompt),
            0x15 => ed.kill_to_start(prompt),
            0x17 => ed.kill_word(prompt),
            0x0c => {
                // Ctrl-L: clear screen, redraw prompt + line.
                print!("\x1b[2J\x1b[H");
                ed.redraw(prompt);
            }
            0x1b => {
                let Some(b2) = read_byte(&mut stdin) else {
                    return Ok(ReadOutcome::Eof);
                };
                if b2 != b'[' {
                    continue;
                }
                let Some(code) = read_byte(&mut stdin) else {
                    return Ok(ReadOutcome::Eof);
                };
                match code {
                    b'A' => {
                        if hidx > 0 {
                            hidx -= 1;
                            ed.set_text(&history[hidx], prompt);
                        }
                    }
                    b'B' => {
                        if hidx + 1 < history.len() {
                            hidx += 1;
                            ed.set_text(&history[hidx], prompt);
                        } else if hidx < history.len() {
                            hidx = history.len();
                            ed.set_text("", prompt);
                        }
                    }
                    b'C' => {
                        if ed.cursor < ed.buf.len() {
                            ed.cursor += 1;
                            print!("\x1b[C");
                            std::io::stdout().flush().unwrap();
                        }
                    }
                    b'D' => {
                        if ed.cursor > 0 {
                            ed.cursor -= 1;
                            print!("\x1b[D");
                            std::io::stdout().flush().unwrap();
                        }
                    }
                    b'H' => {
                        ed.cursor = 0;
                        ed.redraw(prompt);
                    }
                    b'F' => {
                        ed.cursor = ed.buf.len();
                        ed.redraw(prompt);
                    }
                    b'1' | b'4' => {
                        let _ = read_byte(&mut stdin); // consume '~'
                        if code == b'1' {
                            ed.cursor = 0;
                        } else {
                            ed.cursor = ed.buf.len();
                        }
                        ed.redraw(prompt);
                    }
                    b'3' => {
                        let _ = read_byte(&mut stdin); // consume '~'
                        ed.delete(prompt);
                    }
                    _ => {}
                }
            }
            b'\t' => {} // no completion yet; ignore
            _ => {
                // Assemble one (possibly multi-byte) UTF-8 char.
                let extra = if b >= 0xF0 {
                    3
                } else if b >= 0xE0 {
                    2
                } else if b >= 0xC0 {
                    1
                } else {
                    0
                };
                let mut bytes = vec![b];
                for _ in 0..extra {
                    match read_byte(&mut stdin) {
                        Some(nb) => bytes.push(nb),
                        None => return Ok(ReadOutcome::Eof),
                    }
                }
                if let Ok(s) = std::str::from_utf8(&bytes) {
                    for c in s.chars() {
                        ed.insert(c, prompt);
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Paren balance for multi-line forms
// ---------------------------------------------------------------------------

/// Net open-paren count over `text`, respecting string literals and `;`
/// line comments; `(` and `[` both count.
fn paren_balance(text: &str) -> i64 {
    let mut bal = 0i64;
    let mut in_string = false;
    let mut escaped = false;
    let mut in_comment = false;
    for c in text.chars() {
        if in_comment {
            if c == '\n' {
                in_comment = false;
            }
            continue;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            ';' => in_comment = true,
            '(' | '[' => bal += 1,
            ')' | ']' => bal -= 1,
            _ => {}
        }
    }
    bal
}

// ---------------------------------------------------------------------------
// History persistence (interactive sessions only)
// ---------------------------------------------------------------------------

fn history_path() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|h| {
        let mut p = std::path::PathBuf::from(h);
        p.push(HISTORY_FILE);
        p
    })
}

fn load_history() -> Vec<String> {
    let Some(path) = history_path() else { return Vec::new() };
    match std::fs::read_to_string(&path) {
        Ok(text) => text.lines().map(|l| l.to_string()).collect(),
        Err(_) => Vec::new(),
    }
}

fn save_history(history: &[String]) {
    let Some(path) = history_path() else { return };
    let tail: Vec<&str> = history
        .iter()
        .rev()
        .take(HISTORY_MAX)
        .map(|l| l.as_str())
        .collect();
    let mut text = tail
        .into_iter()
        .rev()
        .collect::<Vec<&str>>()
        .join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    let _ = std::fs::write(&path, text);
}
