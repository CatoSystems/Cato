//! Terminal UX helpers: colored output (TTY-gated, `NO_COLOR`-aware) and
//! "did you mean" suggestions for mistyped flags and commands.

use std::io::IsTerminal;

fn color_enabled() -> bool {
    std::env::var_os("NO_COLOR").is_none() && std::io::stderr().is_terminal()
}

/// Colors for REPL prompts and results, which live on stdout.
pub fn stdout_color_enabled() -> bool {
    std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal()
}

/// Paint with an explicit enable flag (choose the gate at the call site).
pub fn paint(enabled: bool, code: &str, s: &str) -> String {
    if enabled {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

fn wrap(code: &str, s: &str) -> String {
    if color_enabled() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

/// A bold-red `cato: error:` lead for diagnostics.
pub fn error_lead() -> String {
    wrap("1;31", "cato: error:")
}

/// A green check mark for success lines.
pub fn check_mark() -> String {
    wrap("1;32", "✓")
}

/// Edit distance between two strings (classic DP, small inputs).
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// The closest candidate within a forgiving threshold, if any.
pub fn did_you_mean(input: &str, candidates: &[&str]) -> Option<String> {
    let input = input.trim_start_matches('-');
    candidates
        .iter()
        .filter_map(|c| {
            let d = levenshtein(input, c.trim_start_matches('-'));
            (d <= 2 && d < c.len()).then(|| (d, c))
        })
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| (*c).to_string())
}

/// Suggestion suffix (" — did you mean `x`?") or empty.
pub fn suggestion_suffix(input: &str, candidates: &[&str]) -> String {
    match did_you_mean(input, candidates) {
        Some(c) => format!(" — did you mean '{}'?", c),
        None => String::new(),
    }
}

/// Run `f`, converting a panic (the interpreter contains `panic!`
/// invariants that malformed input can reach) into an `Err` so bad input
/// costs an error message instead of a dead process or a raw backtrace.
/// The default panic hook is suppressed for the duration.
pub fn guard<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    std::panic::set_hook(previous_hook);
    match result {
        Ok(r) => r,
        Err(payload) => Err(panic_message(payload)),
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else {
        "internal error".to_string()
    }
}
