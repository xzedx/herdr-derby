//! herdr-derby: send one prompt to every coding agent in the current Herdr
//! tab and watch them race.
//!
//! A popup takes the prompt (multi-line, paste-friendly) and lists the
//! tab's agents with checkboxes. Enter hands the race to a detached helper,
//! which submits the prompt to all of them at once, shows a live timer on
//! each pane border, then the finishing rank, and notifies when all are done.
//!
//! Race mode starts fresh agents instead, each in its own pane, tab or
//! workspace and optionally its own git worktree, then races them the same
//! way. Settle keeps the lane the user picks and removes the rest.
//!
//! Usage:
//!   herdr-derby                  broadcast popup (manifest pane "broadcast")
//!   herdr-derby --open           open the popup on the current tab, sized to fit
//!   herdr-derby --race-popup     race popup (manifest pane "race")
//!   herdr-derby --open-race      open the race popup on the current tab
//!   herdr-derby --settle-popup   settle popup (manifest pane "settle")
//!   herdr-derby --open-settle    open the settle popup (race list when several are open)
//!   herdr-derby --race <file>    run a race described by a run file (detached helper)
//!   herdr-derby --send [--tab T] <text>
//!                                race without the popup, in the foreground
//!   herdr-derby --dump [--tab T] render one popup frame to stdout (debugging)
//!
//! The tab comes from --tab, then DERBY_TAB (set by --open for the popup),
//! then the plugin action context, then HERDR_TAB_ID.

mod editor;
mod git;
mod lanes;
mod model;
mod race;
mod race_ui;
mod rpc;
mod settle;
mod start;
mod state;
mod term;
mod trust;
mod ui;

use model::{load_targets, refresh, Target};
use race::{Run, RunTarget};
use rpc::Client;
use serde_json::{json, Value};
use state::trace;
use std::sync::atomic::Ordering;
use term::Key;
use ui::{App, Focus};

/// How often the popup re-reads agent states while it waits for keys.
const REFRESH_MS: i32 = 1000;
/// Editor rows the popup opens with.
const EDITOR_ROWS: usize = 6;
const POPUP_WIDTH: usize = 76;

fn plugin_id() -> String {
    std::env::var("HERDR_PLUGIN_ID")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "xzedx.derby".into())
}

fn env_nonempty(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|s| !s.is_empty())
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

fn current_tab(args: &[String]) -> Result<String, String> {
    if let Some(t) = arg_value(args, "--tab") {
        return Ok(t);
    }
    if let Some(t) = env_nonempty("DERBY_TAB") {
        return Ok(t);
    }
    let ctx = env_nonempty("HERDR_PLUGIN_CONTEXT_JSON")
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|v| v.get("tab_id").and_then(Value::as_str).map(String::from));
    ctx.or_else(|| env_nonempty("HERDR_TAB_ID"))
        .ok_or_else(|| "no current tab: pass --tab".into())
}

/// Action half: open the popup on the current tab, tall enough for its
/// agent list, and tell it which tab it is for.
fn open_popup(args: &[String]) -> Result<(), String> {
    let client = Client::connect()?;
    let tab = current_tab(args)?;
    let n = load_targets(&client, &tab)?.len();
    let height = (EDITOR_ROWS + ui::chrome_rows(n) + 2).min(40);
    client
        .call(
            "plugin.pane.open",
            json!({
                "plugin_id": plugin_id(),
                "entrypoint": "broadcast",
                "placement": "popup",
                "width": POPUP_WIDTH,
                "height": height,
                "env": { "DERBY_TAB": tab },
            }),
        )
        .map(|_| ())
}

fn run_targets(targets: &[&Target]) -> Vec<RunTarget> {
    targets
        .iter()
        .map(|t| RunTarget {
            pane_id: t.pane_id.clone(),
            label: t.label().to_string(),
        })
        .collect()
}

/// Write the run file and start `--race` in its own process group, so it
/// survives the popup being closed right after.
fn spawn_race(run: &Run) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let dir = state::state_dir().join("runs");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{:.0}.json", state::now_secs() * 1000.0));
    let body = serde_json::to_string(run).map_err(|e| e.to_string())?;
    std::fs::write(&path, body).map_err(|e| e.to_string())?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    std::process::Command::new(exe)
        .arg("--race")
        .arg(&path)
        .env_remove("DERBY_TEST_INPUT")
        .process_group(0)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn race: {e}"))?;
    Ok(())
}

fn race_from_file(path: &str) -> Result<(), String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let _ = std::fs::remove_file(path);
    let run: Run = serde_json::from_str(&raw).map_err(|e| format!("{path}: {e}"))?;
    let record = run.record.clone();
    let panes: Vec<String> = run.targets.iter().map(|t| t.pane_id.clone()).collect();
    let report = race::run(run, true)?;
    // Race mode: settle shows the badges next to each lane's changes.
    if let Some(mut rec) = record.as_deref().and_then(lanes::Record::load) {
        for (pane, badge) in panes.iter().zip(report.badges) {
            if let Some(l) = rec.lanes.iter_mut().find(|l| l.pane_id.as_ref() == Some(pane)) {
                l.finish = Some(badge);
            }
        }
        rec.save()?;
    }
    Ok(())
}

/// No popup: race the tab's ready agents and print the result.
fn send(args: &[String]) -> Result<(), String> {
    let client = Client::connect()?;
    let tab = current_tab(args)?;
    let text = args
        .iter()
        .skip_while(|a| *a != "--send")
        .skip(1)
        .filter(|a| !a.starts_with("--"))
        .find(|a| Some(*a) != arg_value(args, "--tab").as_ref())
        .cloned()
        .ok_or("--send needs the prompt text")?;
    let targets = load_targets(&client, &tab)?;
    let chosen: Vec<&Target> = targets.iter().filter(|t| t.checked).collect();
    if chosen.is_empty() {
        return Err(format!("no idle agents in {tab}"));
    }
    let report = race::run(
        Run {
            text,
            targets: run_targets(&chosen),
            record: None,
        },
        false,
    )?;
    println!("{}\n{}", report.head, report.body);
    Ok(())
}

enum Outcome {
    Send,
    Cancel,
}

/// Handle one key. Returns Some when the popup is done.
fn handle(app: &mut App, key: Key) -> Option<Outcome> {
    app.message = None;
    match key {
        Key::Esc if app.focus == Focus::Targets => app.focus = Focus::Prompt,
        Key::Esc | Key::CtrlC | Key::Eof => return Some(Outcome::Cancel),
        Key::Enter => {
            if app.editor.is_blank() {
                app.message = Some("type a prompt first".into());
            } else if app.checked().is_empty() {
                app.message = Some("no agent selected · tab to pick some".into());
            } else {
                return Some(Outcome::Send);
            }
        }
        Key::Tab | Key::BackTab => {
            app.focus = match app.focus {
                Focus::Prompt if !app.targets.is_empty() => Focus::Targets,
                _ => Focus::Prompt,
            };
        }
        k if app.focus == Focus::Targets => targets_key(app, k),
        k => prompt_key(&mut app.editor, k),
    }
    None
}

/// Editing keys shared by the broadcast and race prompts.
pub fn prompt_key(ed: &mut editor::Editor, key: Key) {
    match key {
        Key::Char(c) => ed.insert(c),
        Key::Paste(s) => ed.insert_str(&s),
        Key::Newline => ed.insert('\n'),
        Key::Backspace => ed.backspace(),
        Key::Delete => ed.delete(),
        Key::Left => ed.left(),
        Key::Right => ed.right(),
        Key::Home | Key::CtrlA => ed.home(),
        Key::End => ed.end(),
        Key::CtrlU => ed.clear(),
        Key::Up => {
            if !ed.up() && ed.text().is_empty() {
                if let Some(last) = state::last_prompt() {
                    ed.set(&last);
                }
            }
        }
        Key::Down => {
            ed.down();
        }
        _ => {}
    }
}

fn targets_key(app: &mut App, key: Key) {
    let n = app.targets.len();
    if n == 0 {
        return;
    }
    let toggle = |t: &mut Target| {
        if t.sendable() {
            t.checked = !t.checked;
        }
    };
    match key {
        Key::Up | Key::Char('k') => app.sel = (app.sel + n - 1) % n,
        Key::Down | Key::Char('j') => app.sel = (app.sel + 1) % n,
        Key::Char(' ') | Key::Char('x') => {
            let sel = app.sel.min(n - 1);
            toggle(&mut app.targets[sel]);
        }
        Key::Char(d @ '1'..='9') => {
            let i = d as usize - '1' as usize;
            if i < n {
                app.sel = i;
                toggle(&mut app.targets[i]);
            }
        }
        Key::Char('a') => {
            let all = app.targets.iter().filter(|t| t.sendable()).all(|t| t.checked);
            for t in app.targets.iter_mut().filter(|t| t.sendable()) {
                t.checked = !all;
            }
        }
        Key::CtrlA | Key::CtrlL => {
            for t in app.targets.iter_mut().filter(|t| t.sendable()) {
                t.checked = key == Key::CtrlA;
            }
        }
        _ => {}
    }
}

fn new_app(client: &Client, tab: &str) -> Result<App, String> {
    let mut editor = editor::Editor::default();
    if let Some(t) = env_nonempty("DERBY_TEST_TEXT") {
        editor.set(&t);
    }
    Ok(App {
        editor,
        targets: load_targets(client, tab)?,
        focus: Focus::Prompt,
        sel: 0,
        message: None,
    })
}

fn run_popup(args: &[String]) -> Result<(), String> {
    trace("popup:start");
    let client = Client::connect()?;
    let tab = current_tab(args)?;
    let mut app = new_app(&client, &tab)?;
    term::install_signal_handlers();
    let raw = term::RawMode::enable();
    term::enter_screen();
    // DERBY_TEST_INPUT=<bytes> is fed in before the terminal (testing).
    let mut input = term::Stdin::new(&std::env::var("DERBY_TEST_INPUT").unwrap_or_default());
    let outcome = loop {
        let (cols, rows) = term::size();
        let (lines, cursor) = ui::render(&app, cols, rows);
        term::draw(&lines, cursor);
        if term::INTERRUPTED.load(Ordering::SeqCst) {
            break Outcome::Cancel;
        }
        match term::read_key(&mut input, REFRESH_MS) {
            None => {
                if let Ok(fresh) = load_targets(&client, &tab) {
                    refresh(&mut app.targets, fresh);
                    app.sel = app.sel.min(app.targets.len().saturating_sub(1));
                }
            }
            Some(key) => {
                trace(&format!("popup:key:{key:?}"));
                if let Some(o) = handle(&mut app, key) {
                    break o;
                }
            }
        }
    };
    term::leave_screen();
    drop(raw);
    if let Outcome::Send = outcome {
        let text = app.editor.text().trim_end().to_string();
        state::save_last_prompt(&text);
        spawn_race(&Run {
            text,
            targets: run_targets(&app.checked()),
            record: None,
        })?;
        trace("popup:sent");
    }
    Ok(())
}

fn context_str(key: &str) -> Option<String> {
    env_nonempty("HERDR_PLUGIN_CONTEXT_JSON")
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|v| v.get(key).and_then(Value::as_str).map(String::from))
}

/// Action half of race mode: open the race popup on the current tab,
/// remembering the pane it was opened from (split lanes go next to it).
fn open_race(args: &[String]) -> Result<(), String> {
    let client = Client::connect()?;
    let tab = current_tab(args)?;
    let anchor = model::anchor_pane(&client, &tab, context_str("pane_id").as_deref())?;
    let kinds = model::agent_kinds(&client)?.len();
    let height = (EDITOR_ROWS + race_ui::chrome_rows(kinds) + 2).min(40);
    client
        .call(
            "plugin.pane.open",
            json!({
                "plugin_id": plugin_id(),
                "entrypoint": "race",
                "placement": "popup",
                "width": POPUP_WIDTH,
                "height": height,
                "env": { "DERBY_TAB": tab, "DERBY_PANE": anchor.pane_id },
            }),
        )
        .map(|_| ())
}

fn run_race_popup(args: &[String]) -> Result<(), String> {
    trace("race-popup:start");
    let client = Client::connect()?;
    let tab = current_tab(args)?;
    let anchor = model::anchor_pane(&client, &tab, env_nonempty("DERBY_PANE").as_deref())?;
    let cwd = anchor.dir().ok_or("the pane has no cwd")?.to_string();
    let repo = git::repo(&cwd);
    let mut app = race_ui::RaceApp::new(
        model::agent_kinds(&client)?,
        &lanes::RaceConfig::load(),
        repo.is_some(),
    );
    if let Some(t) = env_nonempty("DERBY_TEST_TEXT") {
        app.editor.set(&t);
    }
    term::install_signal_handlers();
    let raw = term::RawMode::enable();
    term::enter_screen();
    let mut input = term::Stdin::new(&std::env::var("DERBY_TEST_INPUT").unwrap_or_default());
    let outcome = loop {
        let (cols, rows) = term::size();
        let (lines, cursor) = race_ui::render(&app, cols, rows);
        term::draw(&lines, cursor);
        if term::INTERRUPTED.load(Ordering::SeqCst) {
            break race_ui::Outcome::Cancel;
        }
        if let Some(key) = term::read_key(&mut input, REFRESH_MS) {
            trace(&format!("race-popup:key:{key:?}"));
            if let Some(o) = app.handle(key) {
                break o;
            }
        }
    };
    let result = match outcome {
        race_ui::Outcome::Cancel => Ok(()),
        race_ui::Outcome::Start => {
            let text = app.editor.text().trim_end().to_string();
            state::save_last_prompt(&text);
            app.config().save();
            let taken: Vec<String> = model::list_agents(&client)?
                .into_iter()
                .filter_map(|a| a.name)
                .collect();
            let id = lanes::run_id();
            let home = std::env::var("HOME").unwrap_or_default();
            let isolation = app.isolation();
            let record = lanes::Record {
                lanes: lanes::plan_lanes(&app.picked(), &id, &taken, repo.as_ref(), isolation, &home),
                id,
                created: state::now_secs(),
                cwd,
                workspace_id: model::workspace_of(&tab).to_string(),
                repo,
                isolation,
                layout: app.layout,
                settled: false,
                prompt: String::new(),
            };
            start_lanes(&client, record, anchor.pane_id, text, &mut input)
        }
    };
    term::leave_screen();
    drop(raw);
    result
}

/// Bring the lanes up with the popup showing progress, let the user decide
/// on trust dialogs and drop lanes one by one, then hand the ready lanes to
/// the race helper. Lanes that do not race are removed. Leaving (Esc, or the
/// popup closing) removes every lane created so far.
fn start_lanes(
    client: &Client,
    record: lanes::Record,
    anchor: String,
    text: String,
    input: &mut term::Stdin,
) -> Result<(), String> {
    use race_ui::Action;
    use start::Phase;
    use std::sync::{Arc, Mutex};
    let n = record.lanes.len();
    let shared = Arc::new(Mutex::new(start::Startup {
        record,
        anchor,
        phases: vec![Phase::Queued; n],
        aborted: false,
    }));
    start::launch(client.clone(), Arc::clone(&shared));
    let mut aborting = false;
    let mut sel = 0;
    loop {
        let phases = {
            let s = shared.lock().unwrap();
            let (cols, rows) = term::size();
            term::draw(&race_ui::render_startup(&s, &text, sel, aborting, cols, rows), None);
            s.phases.clone()
        };
        // Lanes still being created are waited for, so none is left behind.
        if aborting && !phases.iter().any(Phase::creating) {
            let errors = start::drop_lanes(client, &shared, |_, _| true);
            trace(&format!("race-popup:aborted errors={errors:?}"));
            leftovers(client, &errors);
            return Ok(());
        }
        if !aborting && race_ui::all_ready(&phases) {
            break;
        }
        if term::INTERRUPTED.load(Ordering::SeqCst) {
            aborting = true;
            shared.lock().unwrap().aborted = true;
            std::thread::sleep(std::time::Duration::from_millis(200));
            continue;
        }
        let Some(key) = term::read_key(input, 200) else {
            continue;
        };
        if key == Key::Eof {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        match race_ui::startup_action(&phases, &key) {
            Action::TrustAll => start::trust_all(client, &shared),
            Action::DropTrust => {
                let errors = start::drop_lanes(client, &shared, |_, p| *p == Phase::Trust);
                leftovers(client, &errors);
            }
            Action::Recheck => start::recheck(client, &shared),
            Action::Up => sel = (sel + n - 1) % n,
            Action::Down => sel = (sel + 1) % n,
            Action::DropOne if race_ui::droppable(&phases[sel]) => {
                let errors = start::drop_lanes(client, &shared, |i, _| i == sel);
                leftovers(client, &errors);
            }
            Action::DropOne => {}
            Action::Race => break,
            Action::Abort => {
                aborting = true;
                shared.lock().unwrap().aborted = true;
            }
            Action::None => {}
        }
    }
    // Lanes that did not make it are removed; the ready ones race.
    let errors = start::drop_lanes(client, &shared, |_, p| *p != Phase::Ready);
    leftovers(client, &errors);
    let mut record = {
        let s = shared.lock().unwrap();
        let mut r = s.record.clone();
        r.lanes = r
            .lanes
            .into_iter()
            .zip(&s.phases)
            .filter(|(_, p)| **p == Phase::Ready)
            .map(|(l, _)| l)
            .collect();
        r
    };
    record.lanes.retain(|l| l.pane_id.is_some());
    record.prompt = text.clone();
    record.save()?;
    let targets = record
        .lanes
        .iter()
        .map(|l| RunTarget {
            pane_id: l.pane_id.clone().unwrap_or_default(),
            label: l.name.clone(),
        })
        .collect();
    spawn_race(&Run {
        text,
        targets,
        record: Some(record.id.clone()),
    })?;
    trace("race-popup:raced");
    Ok(())
}

/// Say what cleanup could not remove, so nothing is left behind unnoticed.
fn leftovers(client: &Client, errors: &[String]) {
    if errors.is_empty() {
        return;
    }
    trace(&format!("race-popup:leftovers {errors:?}"));
    settle::notify(client, "Derby: some lanes were left behind", &errors.join("\n"));
}

fn open_settle() -> Result<(), String> {
    Client::connect()?
        .call(
            "plugin.pane.open",
            json!({
                "plugin_id": plugin_id(),
                "entrypoint": "settle",
                "placement": "popup",
                "width": "90%",
                "height": "80%",
            }),
        )
        .map(|_| ())
}

/// One open race goes straight to its lanes; more start at the race list,
/// none shows the empty list saying so.
fn run_settle_popup() -> Result<(), String> {
    use settle::{Action, ListAction};
    trace("settle:start");
    let client = Client::connect()?;
    let mut list = settle::RaceList::load(&client);
    let mut view = if list.races.len() == 1 {
        Some(settle::SettleApp::load(&client, list.races.remove(0), false))
    } else {
        None
    };
    term::install_signal_handlers();
    let mut raw = term::RawMode::enable();
    term::enter_screen();
    let mut input = term::Stdin::new(&std::env::var("DERBY_TEST_INPUT").unwrap_or_default());
    loop {
        let (cols, rows) = term::size();
        match &view {
            Some(app) => term::draw(&settle::render(app, cols, rows), None),
            None => term::draw(&settle::render_races(&list, cols, rows), None),
        }
        if term::INTERRUPTED.load(Ordering::SeqCst) {
            break;
        }
        let Some(key) = term::read_key(&mut input, REFRESH_MS) else {
            continue;
        };
        trace(&format!("settle:key:{key:?}"));
        let Some(app) = view.as_mut() else {
            match list.handle(key) {
                Some(ListAction::Open(i)) => {
                    view = Some(settle::SettleApp::load(&client, list.races.remove(i), true));
                }
                Some(ListAction::Drop(i)) => {
                    let mut app = settle::SettleApp::load(&client, list.races.remove(i), true);
                    app.handle(Key::Char('D'));
                    view = Some(app);
                }
                Some(ListAction::Quit) => break,
                None => {}
            }
            continue;
        };
        match app.handle(key) {
            None => {}
            Some(Action::Diff(i)) => {
                // Hand the terminal to git's pager, then measure again.
                let lane = &app.record.lanes[i];
                let place = app.rows[i].presence.place;
                if let Some(repo) = &app.record.repo {
                    term::leave_screen();
                    drop(raw.take());
                    let shown = match (place, &lane.worktree, &lane.branch) {
                        (settle::Place::Worktree, Some(path), _) => git::show_diff(path, &repo.base),
                        (_, _, Some(branch)) => git::show_branch_diff(repo, branch),
                        _ => Ok(()),
                    };
                    if let Err(e) = shown {
                        app.message = Some(e);
                    }
                    raw = term::RawMode::enable();
                    term::enter_screen();
                    app.rows[i].stat = settle::lane_stat(&app.record, lane, place);
                }
            }
            Some(Action::Focus(i)) => {
                let focused = match &app.record.lanes[i].pane_id {
                    Some(pane) => client.call("pane.focus", json!({ "pane_id": pane })).is_ok(),
                    None => false,
                };
                if focused {
                    break;
                }
                app.message = Some(format!("{}'s pane is gone", app.record.lanes[i].name));
            }
            Some(Action::Settle) => {
                let (cols, rows) = term::size();
                app.message = Some(if app.dropping { "dropping…" } else { "settling…" }.into());
                term::draw(&settle::render(app, cols, rows), None);
                let (head, body) = settle::execute(&client, app);
                trace(&format!("settle:done {head}"));
                settle::notify(&client, &head, &body);
                // More open races: back to the list to carry on.
                list = settle::RaceList::load(&client);
                if list.races.is_empty() {
                    break;
                }
                list.message = Some(head);
                view = None;
            }
            Some(Action::Back) => {
                list = settle::RaceList::load(&client);
                if list.races.is_empty() {
                    break;
                }
                view = None;
            }
            Some(Action::Quit) => break,
        }
    }
    term::leave_screen();
    drop(raw);
    Ok(())
}

fn dump(args: &[String]) -> Result<(), String> {
    let client = Client::connect()?;
    let tab = current_tab(args)?;
    let app = new_app(&client, &tab)?;
    let env = |k: &str, d: usize| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let n = app.targets.len();
    let (lines, _) = ui::render(
        &app,
        env("COLUMNS", POPUP_WIDTH - 2),
        env("LINES", EDITOR_ROWS + ui::chrome_rows(n)),
    );
    for l in lines {
        println!("{l}\x1b[0m");
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |f: &str| args.iter().any(|a| a == f);
    let result = if has("--open") {
        open_popup(&args)
    } else if has("--open-race") {
        open_race(&args)
    } else if has("--race-popup") {
        run_race_popup(&args)
    } else if has("--open-settle") {
        open_settle()
    } else if has("--settle-popup") {
        run_settle_popup()
    } else if has("--race") {
        match arg_value(&args, "--race") {
            Some(path) => race_from_file(&path),
            None => Err("--race needs a run file".into()),
        }
    } else if has("--send") {
        send(&args)
    } else if has("--dump") {
        dump(&args)
    } else {
        run_popup(&args)
    };
    if let Err(e) = result {
        trace(&format!("error {e}"));
        eprintln!("herdr-derby: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let t = |p: &str, s: &str, c: bool| Target {
            pane_id: p.into(),
            name: None,
            agent: "claude".into(),
            status: s.into(),
            checked: c,
        };
        App {
            editor: editor::Editor::default(),
            targets: vec![t("p1", "idle", true), t("p2", "blocked", false), t("p3", "working", false)],
            focus: Focus::Prompt,
            sel: 0,
            message: None,
        }
    }

    #[test]
    fn enter_needs_text_and_a_target() {
        let mut a = app();
        assert!(handle(&mut a, Key::Enter).is_none());
        assert!(a.message.is_some());
        handle(&mut a, Key::Char('x'));
        assert!(matches!(handle(&mut a, Key::Enter), Some(Outcome::Send)));
        a.targets[0].checked = false;
        assert!(handle(&mut a, Key::Enter).is_none());
    }

    #[test]
    fn target_keys_skip_blocked() {
        let mut a = app();
        handle(&mut a, Key::Tab);
        assert_eq!(a.focus, Focus::Targets);
        handle(&mut a, Key::Char('2')); // blocked: stays unchecked
        assert!(!a.targets[1].checked);
        handle(&mut a, Key::Char('3'));
        assert!(a.targets[2].checked);
        handle(&mut a, Key::Char('a')); // all sendable checked -> uncheck all
        assert!(a.targets.iter().all(|t| !t.checked));
        handle(&mut a, Key::CtrlA);
        assert!(a.targets.iter().all(|t| t.checked == t.sendable()));
        handle(&mut a, Key::CtrlA); // select all again, not a toggle
        assert!(a.targets[2].checked);
        handle(&mut a, Key::CtrlL);
        assert!(a.targets.iter().all(|t| !t.checked));
        handle(&mut a, Key::Esc);
        assert_eq!(a.focus, Focus::Prompt);
        // Typing 'a' in the prompt is text, not "toggle all".
        handle(&mut a, Key::Char('a'));
        assert_eq!(a.editor.text(), "a");
    }
}
