//! With `RENDER_CSS_DROPS=1`, counts what the stylesheet parser drops: declarations
//! that yield nothing, at-rules it skips and pseudo-classes or pseudo-elements it
//! cannot match. `report` prints the counts to stderr as
//! `[css-drop] <kind> <name> <count>` lines, most frequent first. Without the
//! variable nothing is counted and nothing is printed.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RENDER_CSS_DROPS").is_some_and(|v| v == "1"))
}

fn counts() -> &'static Mutex<HashMap<(&'static str, String), u32>> {
    static COUNTS: OnceLock<Mutex<HashMap<(&'static str, String), u32>>> = OnceLock::new();
    COUNTS.get_or_init(Mutex::default)
}

/// Counts one dropped `name` of `kind` (`property`, `value`, `at-rule`, `pseudo`).
pub(crate) fn note(kind: &'static str, name: &str) {
    if enabled() {
        let mut map = counts().lock().unwrap_or_else(|e| e.into_inner());
        *map.entry((kind, name.to_string())).or_insert(0) += 1;
    }
}

/// Prints the counts gathered so far, when counting is on.
pub fn report() {
    if !enabled() {
        return;
    }
    let map = counts().lock().unwrap_or_else(|e| e.into_inner());
    let mut rows: Vec<_> = map.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    for ((kind, name), n) in rows {
        eprintln!("[css-drop] {kind} {name} {n}");
    }
}
