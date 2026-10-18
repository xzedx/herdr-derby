//! herdr-derby: send one prompt to every coding agent in the current Herdr
//! tab and watch them race.
//!
//! A popup takes the prompt (multi-line, paste-friendly) and lists the
//! tab's agents with checkboxes. Enter hands the race to a detached helper,
//! which submits the prompt to all of them at once, shows a live timer on
//! each pane border, then the finishing rank, and notifies when all are done.
//!
//! Usage:
//!   herdr-derby                  broadcast popup (manifest pane "broadcast")
//!   herdr-derby --open           open the popup on the current tab, sized to fit
//!   herdr-derby --race <file>    run a race described by a run file (detached helper)
//!   herdr-derby --send [--tab T] <text>
//!                                race without the popup, in the foreground
//!   herdr-derby --dump [--tab T] render one popup frame to stdout (debugging)
//!
//! The tab comes from --tab, then DERBY_TAB (set by --open for the popup),
//! then the plugin action context, then HERDR_TAB_ID.

mod editor;
mod model;
mod race;
mod rpc;
mod state;
mod term;
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
    race::run(run, true).map(|_| ())
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
    let (head, body) = race::run(
        Run {
            text,
            targets: run_targets(&chosen),
        },
        false,
    )?;
    println!("{head}\n{body}");
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
        k => prompt_key(app, k),
    }
    None
}

fn prompt_key(app: &mut App, key: Key) {
    let ed = &mut app.editor;
    match key {
        Key::Char(c) => ed.insert(c),
        Key::Paste(s) => ed.insert_str(&s),
        Key::Newline => ed.insert('\n'),
        Key::Backspace => ed.backspace(),
        Key::Delete => ed.delete(),
        Key::Left => ed.left(),
        Key::Right => ed.right(),
        Key::Home => ed.home(),
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
        })?;
        trace("popup:sent");
    }
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
        handle(&mut a, Key::Esc);
        assert_eq!(a.focus, Focus::Prompt);
        // Typing 'a' in the prompt is text, not "toggle all".
        handle(&mut a, Key::Char('a'));
        assert_eq!(a.editor.text(), "a");
    }
}
