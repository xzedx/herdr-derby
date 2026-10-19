//! The race itself, run by a detached helper so it outlives the popup:
//! submit the prompt to every target in parallel, keep a live timer on each
//! pane border, then the finishing rank, and notify once all have settled.
//!
//! Each target gets its own thread blocked in `agent.prompt` with a wait.
//! That call has the right semantics built in: it sends the text with the
//! pane's bracketed-paste mode, insists on seeing the agent start working,
//! and returns at the first settled state (idle, done or blocked).
//!
//! Blocked is not an end: an agent waiting on an approval gets "needs you"
//! on its border and one notification, its clock stops, and it is followed
//! again once the user has answered. Ranks go by each agent's own time.

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
const NOT_READY_RETRIES: usize = 3;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RunTarget {
    pub pane_id: String,
    pub label: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Run {
    pub text: String,
    pub targets: Vec<RunTarget>,
    /// Race-mode record to write the finishing badges into.
    #[serde(default)]
    pub record: Option<String>,
    /// A referee's run: it finishes, it does not place.
    #[serde(default)]
    pub unranked: bool,
}

/// How a race ended: the notification text, and each target's badge.
pub struct Report {
    pub head: String,
    pub body: String,
    pub badges: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Finished,
    /// The prompt never reached the agent.
    Unsent(String),
    /// It was sent, then something went wrong.
    Failed(String),
}

#[derive(Clone, Debug)]
struct Slot {
    target: RunTarget,
    /// Race time at which it stopped on an approval or question.
    blocked_since: Option<Duration>,
    /// Time spent waiting on the user, left out of its own time.
    waited: Duration,
    result: Option<(Outcome, Duration)>,
}

impl Slot {
    /// Its own time at race time `now`: the clock stops while it waits on
    /// the user.
    fn own(&self, now: Duration) -> Duration {
        let waiting = self.blocked_since.map_or(Duration::ZERO, |b| now.saturating_sub(b));
        now.saturating_sub(self.waited + waiting)
    }
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

/// Rank among finishers by their own time; None for anything else.
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

/// The title without the label: "🥇 3.6s", "🏇 12.4s".
fn badge(slot: &Slot, rank: Option<usize>, now: Duration) -> String {
    match &slot.result {
        None if slot.blocked_since.is_some() => {
            format!("⚠ needs you · {}", fmt_elapsed(slot.own(now)))
        }
        None => format!("🏇 {}", fmt_elapsed(slot.own(now))),
        Some((Outcome::Finished, d)) => {
            let mark = rank.map_or_else(|| "✓".to_string(), medal);
            let mut b = format!("{mark} {}", fmt_elapsed(*d));
            if slot.waited >= Duration::from_secs(1) {
                b.push_str(&format!(" (+{} waiting)", fmt_elapsed(slot.waited)));
            }
            b
        }
        Some((Outcome::Unsent(_), _)) => "✗ not sent".into(),
        Some((Outcome::Failed(e), _)) => format!("✗ failed: {}", short_error(e)),
    }
}

fn title(slot: &Slot, rank: Option<usize>, now: Duration) -> String {
    format!("{} · {}", badge(slot, rank, now), slot.target.label)
}

fn publish(client: &Client, slots: &[Slot], now: Duration, ttl: u64, ranked: bool) {
    let r = if ranked { ranks(slots) } else { vec![None; slots.len()] };
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
    let count = |f: fn(&Outcome) -> bool| {
        slots
            .iter()
            .filter(|s| s.result.as_ref().is_some_and(|(o, _)| f(o)))
            .count()
    };
    let unsent = count(|o| matches!(o, Outcome::Unsent(_)));
    let failed = count(|o| matches!(o, Outcome::Failed(_)));
    let head = match (unsent, failed) {
        (0, 0) => format!("Derby: all {} finished", slots.len()),
        (0, f) => format!("Derby: {f} failed"),
        (u, 0) => format!("Derby: {u} not sent"),
        (u, f) => format!("Derby: {u} not sent, {f} failed"),
    };
    (head, lines.join("\n"))
}

fn status_of(v: &Value) -> &str {
    v.pointer("/agent/agent_status")
        .and_then(Value::as_str)
        .unwrap_or("")
}

/// Refused before any input was written: the prompt did not go out.
fn refused(e: &str) -> bool {
    ["agent_not_ready", "agent_blocked", "agent_not_found"]
        .iter()
        .any(|c| e.starts_with(c))
}

fn outcome_of_error(e: String) -> Outcome {
    if refused(&e) {
        Outcome::Unsent(e)
    } else {
        Outcome::Failed(e)
    }
}

/// Whether the start of the prompt shows on the screen, in its input box or
/// the conversation. Whitespace is ignored: the input box wraps lines.
fn on_screen(screen: &str, text: &str) -> bool {
    let squash = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let head: String = squash(text).chars().take(16).collect();
    head.is_empty() || squash(screen).contains(&head)
}

/// After `agent_prompt_stalled`: true only when the agent sits idle and
/// nothing of the prompt shows, so sending it again cannot double it.
fn surely_lost(client: &Client, pane: &str, text: &str) -> bool {
    let idle = client
        .call("agent.get", json!({ "target": pane }))
        .is_ok_and(|v| matches!(status_of(&v), "idle" | "done"));
    let screen = client
        .call("agent.read", json!({ "target": pane, "source": "visible" }))
        .ok()
        .and_then(|v| v.pointer("/read/text").and_then(Value::as_str).map(String::from));
    idle && screen.is_some_and(|s| !on_screen(&s, text))
}

fn notify_blocked(client: &Client, label: &str) {
    let _ = client.call(
        "notification.show",
        json!({
            "title": format!("Derby: {label} needs you"),
            "body": "answer it in its pane; its clock stops until then",
        }),
    );
}

/// One target, from sending the prompt to its final outcome.
fn race_one(client: &Client, slots: &Mutex<Vec<Slot>>, i: usize, text: &str, start: Instant) -> Outcome {
    let (pane, label) = {
        let s = slots.lock().unwrap();
        (s[i].target.pane_id.clone(), s[i].target.label.clone())
    };
    let prompt = || {
        client.call(
            "agent.prompt",
            json!({
                "target": pane,
                "text": text,
                "wait": { "timeout_ms": RACE_TIMEOUT_MS },
            }),
        )
    };
    let mut resp = prompt();
    // Refused before anything was sent, so trying again is safe: a
    // freshly started agent can be briefly unregistered.
    for _ in 0..NOT_READY_RETRIES {
        match &resp {
            Err(e) if e.starts_with("agent_not_ready") => {
                std::thread::sleep(Duration::from_secs(1));
                resp = prompt();
            }
            _ => break,
        }
    }
    if matches!(&resp, Err(e) if e.starts_with("agent_prompt_stalled")) {
        if !surely_lost(client, &pane, text) {
            trace(&format!("race:stalled-unsure {pane}"));
            return Outcome::Failed("prompt stalled, may be in its input".into());
        }
        trace(&format!("race:resend {pane}"));
        resp = prompt();
        if matches!(&resp, Err(e) if e.starts_with("agent_prompt_stalled")) {
            return if surely_lost(client, &pane, text) {
                Outcome::Unsent("not taken twice".into())
            } else {
                Outcome::Failed("prompt stalled, may be in its input".into())
            };
        }
    }
    let mut notified = false;
    loop {
        let v = match resp {
            Ok(v) => v,
            Err(e) => return outcome_of_error(e),
        };
        if status_of(&v) != "blocked" {
            return Outcome::Finished;
        }
        // An approval or question is not the end: wait for the user, with
        // its clock stopped, then follow it to its next settled state.
        slots.lock().unwrap()[i].blocked_since = Some(start.elapsed());
        if !notified {
            notify_blocked(client, &label);
            notified = true;
        }
        let left = client.call(
            "agent.wait",
            json!({
                "target": pane,
                "until": ["working", "idle", "done"],
                "timeout_ms": RACE_TIMEOUT_MS,
            }),
        );
        {
            let mut s = slots.lock().unwrap();
            let now = start.elapsed();
            if let Some(b) = s[i].blocked_since.take() {
                s[i].waited += now.saturating_sub(b);
            }
        }
        resp = match left {
            Ok(v) if status_of(&v) == "working" => client.call(
                "agent.wait",
                json!({ "target": pane, "timeout_ms": RACE_TIMEOUT_MS }),
            ),
            r => r,
        };
    }
}

/// Run a race to the end. Returns the notification text it showed.
pub fn run(run: Run, notify: bool) -> Result<Report, String> {
    let client = Client::connect()?;
    let start = Instant::now();
    let slots: Arc<Mutex<Vec<Slot>>> = Arc::new(Mutex::new(
        run.targets
            .iter()
            .map(|t| Slot {
                target: t.clone(),
                blocked_since: None,
                waited: Duration::ZERO,
                result: None,
            })
            .collect(),
    ));
    trace(&format!("race:start n={}", run.targets.len()));

    let mut handles = Vec::new();
    for i in 0..run.targets.len() {
        let client = client.clone();
        let slots = Arc::clone(&slots);
        let text = run.text.clone();
        handles.push(std::thread::spawn(move || {
            let outcome = race_one(&client, &slots, i, &text, start);
            let mut s = slots.lock().unwrap();
            trace(&format!("race:settled {} {outcome:?}", s[i].target.pane_id));
            let own = s[i].own(start.elapsed());
            s[i].result = Some((outcome, own));
        }));
    }

    loop {
        let snapshot = slots.lock().unwrap().clone();
        if snapshot.iter().all(|s| s.result.is_some()) {
            publish(&client, &snapshot, start.elapsed(), FINAL_TTL_MS, !run.unranked);
            let (head, mut body) = summary(&snapshot);
            if run.unranked {
                body.push_str("\nnext: Derby settle, to see what it did");
            } else if run.record.is_some() {
                body.push_str("\nnext: Derby settle, to keep a winner");
            }
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
            let r = if run.unranked { vec![None; snapshot.len()] } else { ranks(&snapshot) };
            let badges = snapshot
                .iter()
                .zip(r)
                .map(|(s, rank)| badge(s, rank, Duration::ZERO))
                .collect();
            return Ok(Report { head, body, badges });
        }
        publish(&client, &snapshot, start.elapsed(), LIVE_TTL_MS, !run.unranked);
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
            blocked_since: None,
            waited: Duration::ZERO,
            result: result.map(|(o, ms)| (o, Duration::from_millis(ms))),
        }
    }

    #[test]
    fn ranks_only_finishers_by_time() {
        let slots = vec![
            slot("a", Some((Outcome::Finished, 5500))),
            slot("b", Some((Outcome::Unsent("x".into()), 1000))),
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
        let s = slot("x", Some((Outcome::Failed("agent_gone: no".into()), 0)));
        assert_eq!(title(&s, None, Duration::ZERO), "✗ failed: agent_gone · x");
        let s = slot("k", Some((Outcome::Unsent("agent_blocked: x".into()), 0)));
        assert_eq!(title(&s, None, Duration::ZERO), "✗ not sent · k");
    }

    #[test]
    fn waiting_on_the_user_stops_the_clock() {
        let mut s = slot("codex", None);
        s.blocked_since = Some(Duration::from_secs(30));
        // Blocked for 20 s at race time 50 s: its own time is still 30 s.
        assert_eq!(title(&s, None, Duration::from_secs(50)), "⚠ needs you · 30.0s · codex");
        s.blocked_since = None;
        s.waited = Duration::from_secs(70);
        assert_eq!(s.own(Duration::from_secs(282)), Duration::from_secs(212));
        s.result = Some((Outcome::Finished, Duration::from_secs(212)));
        assert_eq!(badge(&s, Some(2), Duration::ZERO), "🥈 3m32s (+1m10s waiting)");
        // A referee finishes without a place.
        assert_eq!(badge(&s, None, Duration::ZERO), "✓ 3m32s (+1m10s waiting)");
    }

    #[test]
    fn summary_head() {
        let slots = vec![
            slot("a", Some((Outcome::Finished, 1))),
            slot("b", Some((Outcome::Unsent("x".into()), 2))),
        ];
        assert_eq!(summary(&slots).0, "Derby: 1 not sent");
        let slots = vec![slot("a", Some((Outcome::Finished, 1)))];
        assert_eq!(summary(&slots).0, "Derby: all 1 finished");
        let slots = vec![
            slot("a", Some((Outcome::Failed("x".into()), 1))),
            slot("b", Some((Outcome::Unsent("x".into()), 2))),
        ];
        assert_eq!(summary(&slots).0, "Derby: 1 not sent, 1 failed");
    }

    #[test]
    fn errors_before_sending_are_unsent() {
        assert_eq!(
            outcome_of_error("agent_blocked: x".into()),
            Outcome::Unsent("agent_blocked: x".into())
        );
        assert!(matches!(outcome_of_error("timeout: x".into()), Outcome::Failed(_)));
    }

    #[test]
    fn prompt_on_screen_ignores_wrapping() {
        let screen = "│ > Add tests for the model\n│   and render modules │";
        assert!(on_screen(screen, "Add tests for the model and render modules"));
        assert!(!on_screen("No session yet — one will be created", "Add tests for the model"));
    }
}
