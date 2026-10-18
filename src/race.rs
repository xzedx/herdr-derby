//! The race itself, run by a detached helper so it outlives the popup:
//! submit the prompt to every target in parallel, keep a live timer on each
//! pane border, then the finishing rank, and notify once all have settled.
//!
//! Each target gets its own thread blocked in `agent.prompt` with a wait.
//! That call has the right semantics built in: it sends the text with the
//! pane's bracketed-paste mode, insists on seeing the agent start working,
//! and returns at the first settled state (idle, done or blocked).

use crate::rpc::Client;
use crate::state::trace;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SOURCE: &str = "xzedx.derby";
/// Upper bound for one agent's turn.
const RACE_TIMEOUT_MS: u64 = 3 * 60 * 60 * 1000;
/// Border titles while running are refreshed every tick and lapse on their
/// own if this helper dies; final ones stay up for a while.
const LIVE_TTL_MS: u64 = 5_000;
const FINAL_TTL_MS: u64 = 10 * 60 * 1000;
const TICK: Duration = Duration::from_secs(1);

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RunTarget {
    pub pane_id: String,
    pub label: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Run {
    pub text: String,
    pub targets: Vec<RunTarget>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Finished,
    /// Settled on an approval or question: it needs the user.
    Blocked,
    Failed(String),
}

#[derive(Clone, Debug)]
struct Slot {
    target: RunTarget,
    result: Option<(Outcome, Duration)>,
}

pub fn fmt_elapsed(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s < 60.0 {
        format!("{s:.1}s")
    } else {
        let m = (s / 60.0).floor() as u64;
        format!("{m}m{:02}s", (s as u64) % 60)
    }
}

/// Rank among finishers by elapsed time; None for anything else.
fn ranks(slots: &[Slot]) -> Vec<Option<usize>> {
    let mut finished: Vec<(usize, Duration)> = slots
        .iter()
        .enumerate()
        .filter_map(|(i, s)| match &s.result {
            Some((Outcome::Finished, d)) => Some((i, *d)),
            _ => None,
        })
        .collect();
    finished.sort_by_key(|&(_, d)| d);
    let mut out = vec![None; slots.len()];
    for (rank, (i, _)) in finished.into_iter().enumerate() {
        out[i] = Some(rank + 1);
    }
    out
}

fn medal(rank: usize) -> String {
    match rank {
        1 => "🥇".into(),
        2 => "🥈".into(),
        3 => "🥉".into(),
        n => format!("#{n}"),
    }
}

fn short_error(e: &str) -> &str {
    e.split(':').next().unwrap_or(e)
}

fn title(slot: &Slot, rank: Option<usize>, now: Duration) -> String {
    let label = &slot.target.label;
    match &slot.result {
        None => format!("🏇 {} · {label}", fmt_elapsed(now)),
        Some((Outcome::Finished, d)) => {
            format!("{} {} · {label}", medal(rank.unwrap_or(0)), fmt_elapsed(*d))
        }
        Some((Outcome::Blocked, d)) => format!("⚠ needs you {} · {label}", fmt_elapsed(*d)),
        Some((Outcome::Failed(e), _)) => format!("✗ {} · {label}", short_error(e)),
    }
}

fn publish(client: &Client, slots: &[Slot], now: Duration, ttl: u64) {
    let r = ranks(slots);
    for (slot, rank) in slots.iter().zip(r) {
        let _ = client.call(
            "pane.report_metadata",
            json!({
                "pane_id": slot.target.pane_id,
                "source": SOURCE,
                "title": title(slot, rank, now),
                "ttl_ms": ttl,
            }),
        );
    }
}

fn summary(slots: &[Slot]) -> (String, String) {
    let r = ranks(slots);
    let mut order: Vec<usize> = (0..slots.len()).collect();
    order.sort_by_key(|&i| r[i].unwrap_or(usize::MAX));
    let lines: Vec<String> = order
        .iter()
        .map(|&i| title(&slots[i], r[i], Duration::ZERO))
        .collect();
    let blocked = slots
        .iter()
        .filter(|s| matches!(s.result, Some((Outcome::Blocked, _))))
        .count();
    let failed = slots
        .iter()
        .filter(|s| matches!(s.result, Some((Outcome::Failed(_), _))))
        .count();
    let head = if blocked + failed == 0 {
        format!("Derby: all {} finished", slots.len())
    } else if blocked > 0 {
        format!("Derby: {blocked} need{} you", if blocked == 1 { "s" } else { "" })
    } else {
        format!("Derby: {failed} failed")
    };
    (head, lines.join("\n"))
}

fn outcome_of(resp: Result<Value, String>) -> Outcome {
    match resp {
        Ok(v) => {
            let status = v
                .pointer("/agent/agent_status")
                .and_then(Value::as_str)
                .unwrap_or("");
            if status == "blocked" {
                Outcome::Blocked
            } else {
                Outcome::Finished
            }
        }
        Err(e) if e.starts_with("agent_blocked") => Outcome::Blocked,
        Err(e) => Outcome::Failed(e),
    }
}

/// Run a race to the end. Returns the notification text it showed.
pub fn run(run: Run, notify: bool) -> Result<(String, String), String> {
    let client = Client::connect()?;
    let start = Instant::now();
    let slots: Arc<Mutex<Vec<Slot>>> = Arc::new(Mutex::new(
        run.targets
            .iter()
            .map(|t| Slot {
                target: t.clone(),
                result: None,
            })
            .collect(),
    ));
    trace(&format!("race:start n={}", run.targets.len()));

    let mut handles = Vec::new();
    for (i, t) in run.targets.iter().enumerate() {
        let client = client.clone();
        let slots = Arc::clone(&slots);
        let pane = t.pane_id.clone();
        let text = run.text.clone();
        handles.push(std::thread::spawn(move || {
            let resp = client.call(
                "agent.prompt",
                json!({
                    "target": pane,
                    "text": text,
                    "wait": { "timeout_ms": RACE_TIMEOUT_MS },
                }),
            );
            let outcome = outcome_of(resp);
            trace(&format!("race:settled {pane} {outcome:?}"));
            slots.lock().unwrap()[i].result = Some((outcome, start.elapsed()));
        }));
    }

    loop {
        let snapshot = slots.lock().unwrap().clone();
        if snapshot.iter().all(|s| s.result.is_some()) {
            publish(&client, &snapshot, start.elapsed(), FINAL_TTL_MS);
            let (head, body) = summary(&snapshot);
            if notify {
                let _ = client.call(
                    "notification.show",
                    json!({ "title": head, "body": body, "sound": "done" }),
                );
            }
            trace("race:done");
            for h in handles {
                let _ = h.join();
            }
            return Ok((head, body));
        }
        publish(&client, &snapshot, start.elapsed(), LIVE_TTL_MS);
        std::thread::sleep(TICK);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(label: &str, result: Option<(Outcome, u64)>) -> Slot {
        Slot {
            target: RunTarget {
                pane_id: format!("w1:{label}"),
                label: label.into(),
            },
            result: result.map(|(o, ms)| (o, Duration::from_millis(ms))),
        }
    }

    #[test]
    fn ranks_only_finishers_by_time() {
        let slots = vec![
            slot("a", Some((Outcome::Finished, 5500))),
            slot("b", Some((Outcome::Blocked, 1000))),
            slot("c", Some((Outcome::Finished, 3600))),
            slot("d", None),
        ];
        assert_eq!(ranks(&slots), vec![Some(2), None, Some(1), None]);
    }

    #[test]
    fn titles() {
        let s = slot("claude", Some((Outcome::Finished, 3600)));
        assert_eq!(title(&s, Some(1), Duration::ZERO), "🥇 3.6s · claude");
        let s = slot("codex", None);
        assert_eq!(title(&s, None, Duration::from_secs(133)), "🏇 2m13s · codex");
        let s = slot("x", Some((Outcome::Failed("agent_prompt_stalled: no".into()), 0)));
        assert_eq!(title(&s, None, Duration::ZERO), "✗ agent_prompt_stalled · x");
    }

    #[test]
    fn summary_head() {
        let slots = vec![
            slot("a", Some((Outcome::Finished, 1))),
            slot("b", Some((Outcome::Blocked, 2))),
        ];
        assert_eq!(summary(&slots).0, "Derby: 1 needs you");
        let slots = vec![slot("a", Some((Outcome::Finished, 1)))];
        assert_eq!(summary(&slots).0, "Derby: all 1 finished");
    }

    #[test]
    fn outcome_mapping() {
        assert_eq!(
            outcome_of(Ok(json!({"agent": {"agent_status": "done"}}))),
            Outcome::Finished
        );
        assert_eq!(
            outcome_of(Ok(json!({"agent": {"agent_status": "blocked"}}))),
            Outcome::Blocked
        );
        assert_eq!(outcome_of(Err("agent_blocked: x".into())), Outcome::Blocked);
    }
}
