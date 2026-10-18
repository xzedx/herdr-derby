//! Tiny bits of durable state: the last prompt, race run files, tracing.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn state_dir() -> PathBuf {
    let dir = std::env::var("HERDR_PLUGIN_STATE_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_default();
            PathBuf::from(home).join(".local/state/herdr/plugins/xzedx.derby")
        });
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub fn save_last_prompt(text: &str) {
    let _ = std::fs::write(state_dir().join("last-prompt.txt"), text);
}

pub fn last_prompt() -> Option<String> {
    std::fs::read_to_string(state_dir().join("last-prompt.txt"))
        .ok()
        .filter(|s| !s.is_empty())
}

/// DERBY_TRACE=<file> appends "<label> <epoch seconds>" lines.
pub fn trace(label: &str) {
    if let Ok(path) = std::env::var("DERBY_TRACE") {
        if path.is_empty() {
            return;
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            use std::io::Write;
            let _ = writeln!(f, "{label} {:.4}", now_secs());
        }
    }
}
