//! Bringing a race's lanes up, and taking them down again.
//!
//! The race popup stays open while this runs: lanes are created one after
//! another (worktrees and splits depend on each other), then every agent
//! starts in parallel and is watched until it can take the prompt, because
//! codex reports `working` for a while after it starts. An agent on its
//! folder-trust dialog is held for the user to decide in the popup; one
//! that needs signing in, exits, or is not ready in time says why there.

use crate::git;
use crate::lanes::{self, From, Isolation, Lane, Layout, Record};
use crate::rpc::Client;
use crate::state::trace;
use crate::trust;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long an agent gets to become ready, counted again after the user
/// answers for it. Seen: kimi and grok ~5 s, codex ~25 s after trust.
pub const LANE_TIMEOUT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(750);
/// How long the screen has to hold still. One poll was not enough: after
/// its trust dialog claude held still for 0.75 s mid-boot and lost the
/// Enter of a prompt sent then; 3 s after the answer it takes prompts.
const STEADY: Duration = Duration::from_millis(1500);
/// Idle this long counts as ready even if the screen keeps moving (an
/// animated footer never holds still).
const IDLE_ENOUGH: Duration = Duration::from_secs(8);
/// The shell back in front this long after the start means the agent quit.
const EXIT_GRACE: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Queued,
    Creating,
    Starting,
    /// Started, waiting for it to settle before it can take the prompt.
    Settling,
    /// Waiting on its folder-trust dialog; the user decides.
    Trust,
    /// Waiting on something the user has to do in its pane: why.
    NeedsYou(String),
    Ready,
    Failed(String),
    /// Not ready in time: what its screen last showed.
    TimedOut(String),
    /// Removed at the user's request.
    Dropped,
}

impl Phase {
    pub fn busy(&self) -> bool {
        matches!(
            self,
            Phase::Queued | Phase::Creating | Phase::Starting | Phase::Settling
        )
    }

    /// Its pane, worktree or branch may not exist yet: not safe to remove.
    pub fn creating(&self) -> bool {
        matches!(self, Phase::Queued | Phase::Creating)
    }
}

pub struct Startup {
    pub record: Record,
    /// Pane to split for the split layout.
    pub anchor: String,
    pub phases: Vec<Phase>,
    /// Set when the user aborts: lanes not created yet never will be.
    pub aborted: bool,
}

pub type Shared = Arc<Mutex<Startup>>;

/// A dropped lane stays dropped: a thread still starting it may finish
/// after the user removed it.
fn set(shared: &Shared, i: usize, phase: Phase) {
    let mut s = shared.lock().unwrap();
    if s.phases[i] == Phase::Dropped {
        return;
    }
    trace(&format!("start:{i}:{phase:?}"));
    s.phases[i] = phase;
}

fn str_at(v: &Value, ptr: &str) -> Option<String> {
    v.pointer(ptr).and_then(Value::as_str).map(String::from)
}

/// Whether the race's workspace is the repo's main one, the one Herdr
/// groups linked worktrees under. Opening a lane there groups it without
/// Herdr creating a main workspace of its own.
fn hosts_repo(client: &Client, rec: &Record) -> bool {
    let Some(repo) = &rec.repo else { return false };
    client
        .call("worktree.list", json!({ "workspace_id": rec.workspace_id }))
        .is_ok_and(|v| {
            str_at(&v, "/source/source_workspace_id").as_deref() == Some(rec.workspace_id.as_str())
                && str_at(&v, "/source/repo_root").as_deref() == Some(repo.root.as_str())
        })
}

fn split(client: &Client, target: &str, split: &lanes::Split, cwd: &str) -> Result<Value, String> {
    client.call(
        "pane.split",
        json!({
            "target_pane_id": target,
            "direction": if split.right { "right" } else { "down" },
            "ratio": split.ratio,
            "cwd": cwd,
            "focus": false,
        }),
    )
}

/// Create lane `i`'s worktree and its pane, tab or workspace, recording the
/// ids as they appear.
fn create(client: &Client, shared: &Shared, i: usize, from: Option<From>) -> Result<(), String> {
    let (rec, anchor) = {
        let s = shared.lock().unwrap();
        (s.record.clone(), s.anchor.clone())
    };
    let lane = &rec.lanes[i];
    let worktree = rec.worktrees();
    if worktree {
        git::worktree_add(
            rec.repo.as_ref().unwrap(),
            lane.worktree.as_deref().unwrap_or_default(),
            lane.branch.as_deref().unwrap_or_default(),
        )?;
    }
    let cwd = lane.worktree.clone().unwrap_or_else(|| rec.cwd.clone());
    let r = match rec.layout {
        Layout::Split => {
            let plan = lanes::split_plan(rec.lanes.len())
                .into_iter()
                .find(|s| s.lane == i)
                .ok_or("no split for lane")?;
            // A lane that failed or was dropped has no pane: split the anchor.
            match from.and_then(|f| match f {
                From::Lane(j) => rec.lanes[j].pane_id.clone(),
                From::Anchor => None,
            }) {
                Some(target) => split(client, &target, &plan, &cwd)
                    .or_else(|_| split(client, &anchor, &plan, &cwd))?,
                None => split(client, &anchor, &plan, &cwd)?,
            }
        }
        Layout::Tab => client.call(
            "tab.create",
            json!({ "workspace_id": rec.workspace_id, "cwd": cwd, "label": lane.name, "focus": false }),
        )?,
        Layout::Workspace if worktree && hosts_repo(client, &rec) => client.call(
            "worktree.open",
            json!({ "workspace_id": rec.workspace_id, "path": cwd, "label": lane.name, "focus": false }),
        )?,
        Layout::Workspace => client.call(
            "workspace.create",
            json!({ "cwd": cwd, "label": lane.name, "focus": false }),
        )?,
    };
    let mut s = shared.lock().unwrap();
    let l = &mut s.record.lanes[i];
    l.pane_id = str_at(&r, "/pane/pane_id").or_else(|| str_at(&r, "/root_pane/pane_id"));
    if rec.layout == Layout::Tab {
        l.tab_id = str_at(&r, "/tab/tab_id");
    }
    if rec.layout == Layout::Workspace {
        l.workspace_id = str_at(&r, "/workspace/workspace_id");
    }
    Ok(())
}

/// Create every lane, then start all agents in parallel. Returns at once.
pub fn launch(client: Client, shared: Shared) {
    std::thread::spawn(move || {
        let (n, layout) = {
            let s = shared.lock().unwrap();
            (s.record.lanes.len(), s.record.layout)
        };
        // Creation order: the split plan for splits, lane order otherwise.
        let order: Vec<(usize, Option<From>)> = if layout == Layout::Split {
            lanes::split_plan(n).iter().map(|s| (s.lane, Some(s.from))).collect()
        } else {
            (0..n).map(|i| (i, None)).collect()
        };
        for (i, from) in order {
            if shared.lock().unwrap().aborted {
                set(&shared, i, Phase::Dropped);
                continue;
            }
            set(&shared, i, Phase::Creating);
            match create(&client, &shared, i, from) {
                Ok(()) => set(&shared, i, Phase::Starting),
                Err(e) => set(&shared, i, Phase::Failed(e)),
            }
        }
        // A fresh shell needs a moment to reach its prompt.
        std::thread::sleep(Duration::from_millis(800));
        for i in 0..n {
            if shared.lock().unwrap().phases[i] != Phase::Starting {
                continue;
            }
            let client = client.clone();
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || start_agent(&client, &shared, i));
        }
    });
}

fn start_agent(client: &Client, shared: &Shared, i: usize) {
    let lane = shared.lock().unwrap().record.lanes[i].clone();
    let Some(pane) = lane.pane_id.clone() else {
        return set(shared, i, Phase::Failed("no pane".into()));
    };
    let deadline = Instant::now() + LANE_TIMEOUT;
    // agent.start holds on until Herdr sees the agent, which never happens
    // if it exits at once: watch the pane meanwhile.
    let (tx, rx) = std::sync::mpsc::channel();
    {
        let client = client.clone();
        let params = json!({
            "name": lane.name,
            "kind": lane.kind,
            "pane_id": pane,
            "timeout_ms": LANE_TIMEOUT.as_millis() as u64,
        });
        std::thread::spawn(move || {
            let _ = tx.send(client.call("agent.start", params));
        });
    }
    let begun = Instant::now();
    let mut ran = false;
    let r = loop {
        match rx.recv_timeout(POLL) {
            Ok(r) => break r,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => break Err("agent.start: no answer".into()),
        }
        match shell_in_front(client, &pane) {
            Some(false) => ran = true,
            Some(true) if ran || begun.elapsed() > EXIT_GRACE => {
                return set(shared, i, Phase::Failed(exited(client, &pane)));
            }
            _ => {}
        }
    };
    let phase = match r {
        // Ready per Herdr is not always ready, and blocked at start is
        // reported as not ready: watch it either way.
        Ok(_) => watch(client, &lane.name, &pane, deadline),
        Err(e) if e.starts_with("agent_not_ready") => watch(client, &lane.name, &pane, deadline),
        Err(e) if e.starts_with("timeout") => Phase::TimedOut(gist(&pane_screen(client, &pane))),
        Err(e) => Phase::Failed(e),
    };
    set(shared, i, phase);
}

fn screen(client: &Client, name: &str) -> Result<String, String> {
    let r = client.call(
        "agent.read",
        json!({ "target": name, "source": "visible" }),
    )?;
    Ok(str_at(&r, "/read/text").unwrap_or_default())
}

/// The pane's screen, for when there is no agent to read through.
fn pane_screen(client: &Client, pane: &str) -> String {
    client
        .call("pane.read", json!({ "pane_id": pane, "source": "visible" }))
        .ok()
        .and_then(|v| str_at(&v, "/read/text"))
        .unwrap_or_default()
}

/// Whether the pane's shell is in the foreground again: no agent running.
fn shell_in_front(client: &Client, pane: &str) -> Option<bool> {
    let v = client
        .call("pane.process_info", json!({ "pane_id": pane }))
        .ok()?;
    let info = v.get("process_info")?;
    Some(info.get("shell_pid")?.as_u64()? == info.get("foreground_process_group_id")?.as_u64()?)
}

/// Why an agent is gone: what it printed above the shell's prompt.
fn exited(client: &Client, pane: &str) -> String {
    format!("exited · {}", gist(&drop_last_line(&pane_screen(client, pane))))
}

/// The screen without its last non-blank line (the shell prompt).
pub fn drop_last_line(screen: &str) -> String {
    let lines: Vec<&str> = screen.lines().collect();
    match lines.iter().rposition(|l| !l.trim().is_empty()) {
        Some(i) => lines[..i].join("\n"),
        None => String::new(),
    }
}

/// The last line of a screen that says something, without box drawing.
pub fn gist(screen: &str) -> String {
    let frame = |c: char| "│╭╮╰╯─━┃>❯›".contains(c) || c.is_whitespace();
    screen
        .lines()
        .map(|l| l.trim_matches(frame))
        .rfind(|l| l.chars().filter(|c| c.is_alphanumeric()).count() >= 3)
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_else(|| "nothing on screen".into())
}

const LOGIN_WORDS: [&str; 4] = ["sign in", "log in", "login", "authenticate"];

/// A sign-in screen, which Herdr cannot classify (cursor reads `unknown`):
/// the line that asks.
pub fn login(screen: &str) -> Option<String> {
    let line = screen.lines().rfind(|l| {
        let l = l.to_lowercase();
        LOGIN_WORDS.iter().any(|w| l.contains(w))
    })?;
    // Only the words that ask: a logo can share the line.
    let ask = line
        .split("  ")
        .filter(|part| {
            let p = part.to_lowercase();
            LOGIN_WORDS.iter().any(|w| p.contains(w))
        })
        .last()
        .unwrap_or(line);
    Some(format!("not signed in · {}", gist(ask)))
}

/// A blocked agent, or an idle one on a dialog Herdr reads as idle.
fn classify(screen: &str) -> Phase {
    if trust::parse(screen).is_some() {
        Phase::Trust
    } else {
        Phase::NeedsYou(format!("blocked · {}", gist(screen)))
    }
}

/// Follow a started agent until it can take the prompt, needs the user,
/// exits, or runs out of time. Ready means idle, taking input, and its
/// screen held still for a while (or idle long enough): after a trust
/// dialog closes claude reads idle while it redraws, and a prompt sent then
/// is lost.
fn watch(client: &Client, name: &str, pane: &str, deadline: Instant) -> Phase {
    let mut last = String::new();
    let mut idle_since: Option<Instant> = None;
    let mut still_since = Instant::now();
    let mut missing = 0;
    let begun = Instant::now();
    let mut shell = 0;
    loop {
        std::thread::sleep(POLL);
        // Herdr keeps a launch it never saw come up as `unknown`.
        if shell_in_front(client, pane) == Some(true) {
            shell += 1;
            if shell >= 2 && begun.elapsed() > EXIT_GRACE {
                return Phase::Failed(exited(client, pane));
            }
        } else {
            shell = 0;
        }
        let agent = match client.call("agent.get", json!({ "target": name })) {
            Ok(v) => v,
            // Gone from Herdr: the agent exited, or never came up.
            Err(e) if e.starts_with("agent_not_found") => {
                missing += 1;
                if missing >= 2 {
                    return Phase::Failed(exited(client, pane));
                }
                continue;
            }
            Err(e) => return Phase::Failed(e),
        };
        missing = 0;
        let status = str_at(&agent, "/agent/agent_status").unwrap_or_default();
        let now = screen(client, name).unwrap_or_default();
        if now != last {
            still_since = Instant::now();
        }
        match status.as_str() {
            "blocked" => return classify(&now),
            "idle" | "done"
                if agent.pointer("/agent/interactive_ready").and_then(Value::as_bool) != Some(false) =>
            {
                // kimi reads idle on its trust dialog.
                if trust::parse(&now).is_some() {
                    return Phase::Trust;
                }
                let since = *idle_since.get_or_insert_with(Instant::now);
                if still_since.elapsed() >= STEADY || since.elapsed() >= IDLE_ENOUGH {
                    return Phase::Ready;
                }
            }
            "unknown" => {
                idle_since = None;
                if let Some(why) = login(&now) {
                    return Phase::NeedsYou(why);
                }
            }
            _ => idle_since = None,
        }
        if Instant::now() >= deadline {
            return Phase::TimedOut(format!("{status} · {}", gist(&now)));
        }
        last = now;
    }
}

/// The user said yes: move the highlight to the trust option, check the
/// screen shows it highlighted, and only then press Enter.
fn answer_trust(client: &Client, name: &str) -> Result<(), String> {
    for _ in 0..4 {
        let Some(d) = trust::parse(&screen(client, name)?) else {
            // Answered in the pane already, or something else came up:
            // watching it tells which.
            return Ok(());
        };
        if d.selected == d.trust {
            client.call("agent.send_keys", json!({ "target": name, "keys": ["enter"] }))?;
            return Ok(());
        }
        let key = if d.trust > d.selected { "down" } else { "up" };
        let keys = vec![key; d.trust.abs_diff(d.selected)];
        client.call("agent.send_keys", json!({ "target": name, "keys": keys }))?;
        std::thread::sleep(Duration::from_millis(300));
    }
    Err("could not highlight the trust option".into())
}

/// Answer every lane waiting on its trust dialog. Returns at once.
pub fn trust_all(client: &Client, shared: &Shared) {
    let waiting: Vec<usize> = {
        let s = shared.lock().unwrap();
        (0..s.phases.len()).filter(|&i| s.phases[i] == Phase::Trust).collect()
    };
    for i in waiting {
        set(shared, i, Phase::Settling);
        let client = client.clone();
        let shared = Arc::clone(shared);
        std::thread::spawn(move || {
            let lane = shared.lock().unwrap().record.lanes[i].clone();
            let pane = lane.pane_id.clone().unwrap_or_default();
            let phase = match answer_trust(&client, &lane.name) {
                Ok(()) => watch(&client, &lane.name, &pane, Instant::now() + LANE_TIMEOUT),
                Err(e) => Phase::Failed(e),
            };
            set(&shared, i, phase);
        });
    }
}

/// Look again at lanes that needed the user or ran out of time: the user
/// may have answered, signed in, or it was just slow. Each gets a fresh
/// timeout.
pub fn recheck(client: &Client, shared: &Shared) {
    let waiting: Vec<usize> = {
        let s = shared.lock().unwrap();
        (0..s.phases.len())
            .filter(|&i| matches!(s.phases[i], Phase::NeedsYou(_) | Phase::TimedOut(_)))
            .collect()
    };
    for i in waiting {
        set(shared, i, Phase::Settling);
        let client = client.clone();
        let shared = Arc::clone(shared);
        std::thread::spawn(move || {
            let lane = shared.lock().unwrap().record.lanes[i].clone();
            let pane = lane.pane_id.clone().unwrap_or_default();
            let phase = watch(&client, &lane.name, &pane, Instant::now() + LANE_TIMEOUT);
            set(&shared, i, phase);
        });
    }
}

/// Lanes whose phase matches are marked dropped and removed, unless still
/// being created. Errors are returned, not fatal: cleanup does what it can.
pub fn drop_lanes(client: &Client, shared: &Shared, pick: impl Fn(usize, &Phase) -> bool) -> Vec<String> {
    let (rec, pick): (Record, Vec<usize>) = {
        let mut s = shared.lock().unwrap();
        let pick: Vec<usize> = (0..s.phases.len())
            .filter(|&i| {
                let p = &s.phases[i];
                *p != Phase::Dropped && !p.creating() && pick(i, p)
            })
            .collect();
        for &i in &pick {
            trace(&format!("start:{i}:Dropped"));
            s.phases[i] = Phase::Dropped;
        }
        (s.record.clone(), pick)
    };
    let mut errors = Vec::new();
    for i in pick {
        errors.extend(remove_lane(client, &rec, &rec.lanes[i]));
    }
    errors
}

fn ignore_gone(r: Result<Value, String>) -> Result<(), String> {
    match r {
        Err(e) if e.contains("not_found") => Ok(()),
        r => r.map(|_| ()),
    }
}

/// Close a lane's pane, tab or workspace, then remove its worktree and
/// branch. Used for losers at settle time and for lanes dropped at start.
/// Closing a linked worktree's workspace leaves the checkout and the repo's
/// main workspace alone, so the checkout goes through git here.
pub fn remove_lane(client: &Client, rec: &Record, lane: &Lane) -> Vec<String> {
    let mut errors = Vec::new();
    let mut note = |what: &str, r: Result<(), String>| {
        if let Err(e) = r {
            errors.push(format!("{}: {what}: {e}", lane.name));
        }
    };
    match rec.layout {
        Layout::Workspace => {
            if let Some(ws) = &lane.workspace_id {
                note("close workspace", ignore_gone(client.call("workspace.close", json!({ "workspace_id": ws }))));
            }
        }
        Layout::Tab => {
            if let Some(tab) = &lane.tab_id {
                note("close tab", ignore_gone(client.call("tab.close", json!({ "tab_id": tab }))));
            }
        }
        Layout::Split => {
            if let Some(pane) = &lane.pane_id {
                note("close pane", ignore_gone(client.call("pane.close", json!({ "pane_id": pane }))));
            }
        }
    }
    if rec.isolation == Isolation::Worktree {
        if let Some(repo) = &rec.repo {
            if let Some(path) = &lane.worktree {
                note("remove worktree", git::worktree_remove(repo, path));
            }
            if let Some(branch) = &lane.branch {
                note("delete branch", git::branch_delete(repo, branch));
            }
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_phases() {
        assert!(Phase::Starting.busy());
        assert!(Phase::Settling.busy());
        assert!(!Phase::Trust.busy());
        assert!(!Phase::Failed("x".into()).busy());
        assert!(!Phase::TimedOut("x".into()).busy());
        assert!(!Phase::Ready.busy());
        assert!(Phase::Creating.creating());
        assert!(!Phase::Starting.creating());
    }

    #[test]
    fn gist_skips_frames_and_blanks() {
        let s = " ╭──────╮\n │ ❯    │\n ╰──────╯\n   Press any key to sign in...\n\n  ─────\n";
        assert_eq!(gist(s), "Press any key to sign in...");
        assert_eq!(gist("\n \n"), "nothing on screen");
        assert_eq!(gist("[1]    49153 killed     opencode\n❯❯❯ "), "[1] 49153 killed opencode");
    }

    #[test]
    fn sign_in_screens() {
        assert_eq!(
            login("CURSOR AGENT\n   Press any key to sign in...\n").as_deref(),
            Some("not signed in · Press any key to sign in...")
        );
        assert_eq!(login("Welcome to Kimi Code!\n > \n"), None);
        // The line that asks, not whatever is drawn below it.
        assert_eq!(
            login("   Press any key to sign in...\n      ttttt/tZZfff^\n").as_deref(),
            Some("not signed in · Press any key to sign in...")
        );
        assert_eq!(
            login("    ]]?_~<i!li+++qqww;IIII          Press any key to sign in...\n").as_deref(),
            Some("not signed in · Press any key to sign in...")
        );
    }

    #[test]
    fn exit_reason_is_above_the_prompt() {
        let s = "❯❯❯ opencode\n[1]    49153 killed     opencode\n~/w ❯❯❯ ✘ 137 main\n\n";
        assert_eq!(gist(&drop_last_line(s)), "[1] 49153 killed opencode");
    }
}
