//! Which models each agent kind offers, for the race popup's model picker.
//!
//! Lists come from the agents' own CLIs, read in the background when the
//! popup opens, and are cached in the state dir so the picker has something
//! to show at once. Claude has no list command; its aliases always mean the
//! latest model of each family. Models used in a race are remembered per
//! kind and offered first.

use crate::state;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Models remembered per kind.
const RECENT: usize = 5;
/// A lister that takes longer than this is given up on.
const LIST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Default, Debug)]
pub struct Catalog {
    /// Kind -> models, as `--model` takes them.
    pub lists: HashMap<String, Vec<String>>,
    /// Kinds still being listed.
    pub pending: HashSet<String>,
    /// Kind -> why listing failed.
    pub errors: HashMap<String, String>,
}

pub type Shared = Arc<Mutex<Catalog>>;

/// Whether derby knows how to list `kind`'s models.
pub fn listable(kind: &str) -> bool {
    matches!(kind, "claude" | "codex" | "kimi" | "omp" | "pi")
}

/// The cached lists, then a background refresh of every listable kind.
pub fn load(kinds: &[String]) -> Shared {
    let lists = std::fs::read_to_string(state::state_dir().join("models-cache.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let shared = Arc::new(Mutex::new(Catalog {
        lists,
        ..Catalog::default()
    }));
    for kind in kinds.iter().filter(|k| listable(k)) {
        shared.lock().unwrap().pending.insert(kind.clone());
        let shared = Arc::clone(&shared);
        let kind = kind.clone();
        std::thread::spawn(move || {
            let r = list(&kind);
            let mut c = shared.lock().unwrap();
            c.pending.remove(&kind);
            match r {
                Ok(models) => {
                    c.errors.remove(&kind);
                    c.lists.insert(kind, models);
                    if let Ok(s) = serde_json::to_string(&c.lists) {
                        let _ = std::fs::write(state::state_dir().join("models-cache.json"), s);
                    }
                }
                Err(e) => {
                    c.errors.insert(kind, e);
                }
            }
        });
    }
    shared
}

fn list(kind: &str) -> Result<Vec<String>, String> {
    match kind {
        "claude" => Ok(["opus", "sonnet", "haiku", "fable"].map(String::from).to_vec()),
        "codex" => parse_codex(&run("codex", &["debug", "models"])?),
        "kimi" => parse_kimi(&run("kimi", &["provider", "list", "--json"])?),
        "omp" => parse_omp(&run("omp", &["models", "--json"])?),
        "pi" => Ok(parse_pi(&run("pi", &["--list-models"])?)),
        _ => Ok(Vec::new()),
    }
}

/// A lister's stdout, killed when it hangs.
fn run(cmd: &str, args: &[&str]) -> Result<String, String> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("{cmd}: {e}"))?;
    let mut out = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut out, &mut s);
        s
    });
    let deadline = Instant::now() + LIST_TIMEOUT;
    let status = loop {
        if let Some(st) = child.try_wait().map_err(|e| format!("{cmd}: {e}"))? {
            break st;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{cmd}: no answer in {}s", LIST_TIMEOUT.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let s = reader.join().unwrap_or_default();
    if !status.success() {
        return Err(format!("{cmd} {}: {status}", args.join(" ")));
    }
    Ok(s)
}

fn json(s: &str, what: &str) -> Result<Value, String> {
    serde_json::from_str(s).map_err(|e| format!("{what}: {e}"))
}

/// `codex debug models`: the catalog its own /model menu shows.
pub fn parse_codex(s: &str) -> Result<Vec<String>, String> {
    let v = json(s, "codex debug models")?;
    Ok(v["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["visibility"].as_str() != Some("hide"))
        .filter_map(|m| m["slug"].as_str().map(String::from))
        .collect())
}

/// `kimi provider list --json`: the model aliases kimi's config defines.
pub fn parse_kimi(s: &str) -> Result<Vec<String>, String> {
    let v = json(s, "kimi provider list")?;
    let models = &v["models"];
    let names: Vec<String> = match models {
        Value::Array(a) => a.iter().filter_map(|m| m.as_str().map(String::from)).collect(),
        Value::Object(o) => o.keys().cloned().collect(),
        _ => Vec::new(),
    };
    Ok(names)
}

/// `omp models --json`: chat models as provider/id selectors.
pub fn parse_omp(s: &str) -> Result<Vec<String>, String> {
    let v = json(s, "omp models")?;
    Ok(v["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["kind"].as_str().is_none_or(|k| k == "chat"))
        .filter_map(|m| {
            m["selector"].as_str().map(String::from).or_else(|| {
                Some(format!("{}/{}", m["provider"].as_str()?, m["id"].as_str()?))
            })
        })
        .collect())
}

/// `pi --list-models`: a table whose first two columns are provider, model.
pub fn parse_pi(s: &str) -> Vec<String> {
    s.lines()
        .skip_while(|l| !l.starts_with("provider"))
        .skip(1)
        .filter_map(|l| {
            let mut cols = l.split_whitespace();
            Some(format!("{}/{}", cols.next()?, cols.next()?))
        })
        .collect()
}

pub fn load_recent() -> HashMap<String, Vec<String>> {
    std::fs::read_to_string(state::state_dir().join("models-recent.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Put `model` first among `kind`'s recent models.
pub fn remember(recent: &mut HashMap<String, Vec<String>>, kind: &str, model: &str) {
    let list = recent.entry(kind.to_string()).or_default();
    list.retain(|m| m != model);
    list.insert(0, model.to_string());
    list.truncate(RECENT);
}

pub fn save_recent(recent: &HashMap<String, Vec<String>>) {
    if let Ok(s) = serde_json::to_string(recent) {
        let _ = std::fs::write(state::state_dir().join("models-recent.json"), s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_hides_hidden() {
        let s = r#"{"models":[{"slug":"gpt-6-sol","visibility":"list"},{"slug":"gpt-reserve","visibility":"hide"}]}"#;
        assert_eq!(parse_codex(s).unwrap(), vec!["gpt-6-sol"]);
    }

    #[test]
    fn kimi_aliases() {
        let s = r#"{"providers":["managed:kimi-code"],"models":["kimi-code/k3","kimi-code/k3-256k"]}"#;
        assert_eq!(parse_kimi(s).unwrap(), vec!["kimi-code/k3", "kimi-code/k3-256k"]);
        assert!(parse_kimi("not json").is_err());
    }

    #[test]
    fn omp_chat_selectors() {
        let s = r#"{"models":[
            {"provider":"google","kind":"chat","id":"g1","selector":"google/g1"},
            {"provider":"xai","kind":"image","id":"img","selector":"xai/img"},
            {"provider":"xai","id":"grok"}]}"#;
        assert_eq!(parse_omp(s).unwrap(), vec!["google/g1", "xai/grok"]);
    }

    #[test]
    fn pi_table() {
        let s = "provider     model       context\ngoogle       gemini-2.5-flash  1.0M\n\nxai  grok-5  2M\n";
        assert_eq!(parse_pi(s), vec!["google/gemini-2.5-flash", "xai/grok-5"]);
    }

    #[test]
    fn recent_moves_to_front() {
        let mut r = HashMap::new();
        for m in ["a", "b", "c", "a", "d", "e", "f"] {
            remember(&mut r, "claude", m);
        }
        assert_eq!(r["claude"], vec!["f", "e", "d", "a", "c"]);
    }
}

#[cfg(test)]
mod live {
    /// The installed CLIs, through the real listers: cargo test -- --ignored live
    #[test]
    #[ignore]
    fn live_lists() {
        for kind in ["claude", "codex", "kimi", "omp", "pi"] {
            match super::list(kind) {
                Ok(m) => println!("{kind}: {} · {:?}", m.len(), &m[..m.len().min(4)]),
                Err(e) => println!("{kind}: error {e}"),
            }
        }
    }
}
