//! Settling a race: the user looks at what each lane did, picks the
//! winner, and the other lanes are removed.
//!
//! Finishing first is not winning. The list shows each lane's finish and
//! how much it changed against the race's base; `d` opens a lane's full
//! diff and `f` jumps to its pane to read the agent's answer. Nothing is
//! picked until the user picks, and removal is confirmed on a second screen
//! that names the uncommitted work it will throw away.

use crate::git::{self, Stat};
use crate::lanes::{Isolation, Record};
use crate::rpc::Client;
use crate::start;
use crate::term::Key;
use crate::ui::{fit, BOLD, DIM, GREEN, RED, RESET, REVERSE, YELLOW};
use serde_json::json;

pub struct Row {
    /// Finish badge, or the live agent state while it still runs.
    pub status: String,
    /// What the lane changed; None for shared-cwd races.
    pub stat: Option<Result<Stat, String>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    List,
    Confirm,
}

pub struct SettleApp {
    pub record: Record,
    pub rows: Vec<Row>,
    pub sel: usize,
    pub picked: Option<usize>,
    pub screen: Screen,
    pub message: Option<String>,
}

#[derive(Debug, PartialEq)]
pub enum Action {
    Diff(usize),
    Focus(usize),
    Settle,
    Quit,
}

/// A lane's status line before the helper wrote its badge.
fn live_status(agents: &[crate::model::Agent], lane: &crate::lanes::Lane) -> String {
    match agents.iter().find(|a| Some(&a.pane_id) == lane.pane_id.as_ref()) {
        Some(a) => match a.agent_status.as_deref() {
            Some("working") => "🏇 running".into(),
            Some(s) => s.to_string(),
            None => "unknown".into(),
        },
        None => "gone".into(),
    }
}

pub fn lane_stat(rec: &Record, i: usize) -> Option<Result<Stat, String>> {
    if !rec.worktrees() {
        return None;
    }
    let lane = &rec.lanes[i];
    let base = &rec.repo.as_ref()?.base;
    Some(match &lane.worktree {
        Some(p) if std::path::Path::new(p).exists() => git::diff_stat(p, base),
        _ => Err("worktree gone".into()),
    })
}

impl SettleApp {
    pub fn load(client: &Client, record: Record) -> Self {
        let agents = crate::model::list_agents(client).unwrap_or_default();
        let rows = (0..record.lanes.len())
            .map(|i| {
                let lane = &record.lanes[i];
                Row {
                    status: lane
                        .finish
                        .clone()
                        .unwrap_or_else(|| live_status(&agents, lane)),
                    stat: lane_stat(&record, i),
                }
            })
            .collect();
        SettleApp {
            record,
            rows,
            sel: 0,
            picked: None,
            screen: Screen::List,
            message: None,
        }
    }

    /// Lanes that will be removed: everything but the winner.
    pub fn losers(&self) -> Vec<usize> {
        (0..self.record.lanes.len())
            .filter(|&i| Some(i) != self.picked)
            .collect()
    }

    pub fn handle(&mut self, key: Key) -> Option<Action> {
        self.message = None;
        if self.screen == Screen::Confirm {
            return match key {
                Key::Char('y') => Some(Action::Settle),
                Key::CtrlC | Key::Eof => Some(Action::Quit),
                _ => {
                    self.screen = Screen::List;
                    None
                }
            };
        }
        let n = self.record.lanes.len();
        match key {
            Key::Esc | Key::CtrlC | Key::Eof | Key::Char('q') => return Some(Action::Quit),
            _ if n == 0 => {}
            Key::Up | Key::Char('k') => self.sel = (self.sel + n - 1) % n,
            Key::Down | Key::Char('j') => self.sel = (self.sel + 1) % n,
            Key::Char(' ') | Key::Char('x') => {
                self.picked = if self.picked == Some(self.sel) { None } else { Some(self.sel) };
            }
            Key::Char(d @ '1'..='9') => {
                let i = d as usize - '1' as usize;
                if i < n {
                    self.sel = i;
                    self.picked = Some(i);
                }
            }
            Key::Char('d') => {
                if self.record.worktrees() {
                    return Some(Action::Diff(self.sel));
                }
                self.message = Some("lanes share the cwd: no per-lane diff".into());
            }
            Key::Char('f') => return Some(Action::Focus(self.sel)),
            Key::Enter => {
                if self.picked.is_some() {
                    self.screen = Screen::Confirm;
                } else {
                    self.message = Some("pick the winner first · space".into());
                }
            }
            _ => {}
        }
        None
    }
}

fn changes(row: &Row) -> (String, &'static str) {
    match &row.stat {
        None => ("shared cwd".into(), DIM),
        Some(Ok(s)) if s.files == 0 => (s.describe(), DIM),
        Some(Ok(s)) => (s.describe(), GREEN),
        Some(Err(e)) => (format!("? {e}"), RED),
    }
}

fn header(app: &SettleApp, inner: usize) -> String {
    let r = &app.record;
    let n = r.lanes.len();
    let base = r
        .repo
        .as_ref()
        .map(|b| format!(" · base {}", &b.base[..b.base.len().min(7)]))
        .unwrap_or_default();
    let what = match app.screen {
        Screen::List => format!("settle race {} · {n} lane{}{base}", r.id, if n == 1 { "" } else { "s" }),
        Screen::Confirm => {
            let keep = app.picked.map(|i| r.lanes[i].name.as_str()).unwrap_or("?");
            let gone = app.losers().len();
            format!("keep {keep} · remove {gone} lane{}", if gone == 1 { "" } else { "s" })
        }
    };
    format!(" {BOLD}Derby{RESET}  {DIM}{}{RESET}", fit(&what, inner.saturating_sub(7)))
}

pub fn render(app: &SettleApp, cols: usize, rows: usize) -> Vec<String> {
    let inner = cols.saturating_sub(2);
    let rule = format!("{DIM}{}{RESET}", "─".repeat(cols));
    let mut lines = vec![header(app, inner), rule.clone()];
    match app.screen {
        Screen::List => {
            lines.push(format!(
                " {DIM}{}{RESET}",
                fit(&format!("    {:<12} {:<16} {:<20} branch", "lane", "finish", "changes"), inner)
            ));
            for (i, (lane, row)) in app.record.lanes.iter().zip(&app.rows).enumerate() {
                let mark = if app.picked == Some(i) { "(•)" } else { "( )" };
                let (chg, color) = changes(row);
                // Columns padded by display width: badges hold wide emoji.
                let name = pad(&fit(&lane.name, 12), 12);
                let status = pad(&fit(&row.status, 16), 16);
                let chg = pad(&fit(&chg, 20), 20);
                let branch = fit(lane.branch.as_deref().unwrap_or(""), inner.saturating_sub(55));
                if i == app.sel {
                    lines.push(format!("{REVERSE} {mark} {name} {status} {chg} {branch}{RESET}"));
                } else {
                    lines.push(format!(
                        " {mark} {name} {status} {color}{chg}{RESET} {DIM}{branch}{RESET}"
                    ));
                }
            }
        }
        Screen::Confirm => {
            let r = &app.record;
            if let Some(w) = app.picked {
                let lane = &r.lanes[w];
                let place = lane.worktree.as_deref().or(lane.branch.as_deref()).unwrap_or("its pane");
                lines.push(format!(" {GREEN}keep{RESET}    {:<12} {}", fit(&lane.name, 12), fit(place, inner.saturating_sub(22))));
            }
            for i in app.losers() {
                let lane = &r.lanes[i];
                let what = match (&app.rows[i].stat, r.isolation) {
                    (_, Isolation::Shared) | (None, _) => (format!("close its {}", place_word(r)), DIM),
                    (Some(Ok(s)), _) if s.files > 0 => {
                        (format!("{} will be discarded", s.describe()), RED)
                    }
                    (Some(Ok(_)), _) => ("no changes · worktree and branch removed".into(), DIM),
                    (Some(Err(_)), _) => ("worktree and branch removed".into(), DIM),
                };
                lines.push(format!(
                    " {RED}remove{RESET}  {:<12} {}{}{RESET}",
                    fit(&lane.name, 12),
                    what.1,
                    fit(&what.0, inner.saturating_sub(22))
                ));
            }
        }
    }
    while lines.len() + 2 < rows {
        lines.push(String::new());
    }
    lines.push(rule);
    let hint = match (&app.message, app.screen) {
        (Some(m), _) => format!(" {YELLOW}{}{RESET}", fit(m, inner)),
        (None, Screen::List) => format!(
            " {DIM}{}{RESET}",
            fit("↑↓ move · space pick winner · d diff · f go to pane · enter review · esc", inner)
        ),
        (None, Screen::Confirm) => format!(" {YELLOW}{}{RESET}", fit("y settle · any other key goes back", inner)),
    };
    lines.push(hint);
    lines
}

fn pad(s: &str, w: usize) -> String {
    let used = unicode_width::UnicodeWidthStr::width(s);
    format!("{s}{}", " ".repeat(w.saturating_sub(used)))
}

fn place_word(r: &Record) -> &'static str {
    match r.layout {
        crate::lanes::Layout::Split => "pane",
        crate::lanes::Layout::Tab => "tab",
        crate::lanes::Layout::Workspace => "workspace",
    }
}

/// Remove the losers, mark the race settled, and say how it went.
pub fn execute(client: &Client, app: &mut SettleApp) -> (String, String) {
    let Some(w) = app.picked else {
        return ("Derby: nothing picked".into(), String::new());
    };
    let mut errors = Vec::new();
    for i in app.losers() {
        errors.extend(start::remove_lane(client, &app.record, &app.record.lanes[i]));
    }
    app.record.settled = true;
    if let Err(e) = app.record.save() {
        errors.push(e);
    }
    let winner = &app.record.lanes[w];
    let head = format!("Derby: kept {}", winner.name);
    let mut body = match (&winner.branch, &winner.worktree) {
        (Some(b), Some(p)) => format!("branch {b}\n{p}"),
        _ => "lanes shared the cwd: only panes were closed".into(),
    };
    if !errors.is_empty() {
        body.push_str("\nsome cleanup failed:\n");
        body.push_str(&errors.join("\n"));
    }
    (head, body)
}

pub fn notify(client: &Client, head: &str, body: &str) {
    let _ = client.call(
        "notification.show",
        json!({ "title": head, "body": body, "sound": "done" }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lanes::{Lane, Layout, Repo};

    fn app(isolation: Isolation) -> SettleApp {
        let lane = |n: &str| Lane {
            name: n.into(),
            kind: "claude".into(),
            branch: Some(format!("race/x/{n}")),
            worktree: Some(format!("/wt/{n}")),
            pane_id: Some(format!("w1:{n}")),
            ..Lane::default()
        };
        let stat = |files| Row {
            status: "🥇 1.0s".into(),
            stat: Some(Ok(Stat { files, added: files, removed: 0 })),
        };
        SettleApp {
            record: Record {
                id: "x".into(),
                created: 0.0,
                cwd: "/src".into(),
                workspace_id: "w1".into(),
                repo: Some(Repo { root: "/src".into(), name: "src".into(), base: "abcdef123".into() }),
                isolation,
                layout: Layout::Split,
                lanes: vec![lane("claude-1"), lane("codex-1"), lane("claude-2")],
                settled: false,
            },
            rows: vec![stat(5), stat(0), stat(2)],
            sel: 0,
            picked: None,
            screen: Screen::List,
            message: None,
        }
    }

    #[test]
    fn nothing_is_picked_until_the_user_picks() {
        let mut a = app(Isolation::Worktree);
        assert_eq!(a.handle(Key::Enter), None);
        assert_eq!(a.screen, Screen::List);
        assert!(a.message.is_some());
        a.handle(Key::Down);
        a.handle(Key::Char(' '));
        assert_eq!(a.picked, Some(1));
        assert_eq!(a.losers(), vec![0, 2]);
        a.handle(Key::Enter);
        assert_eq!(a.screen, Screen::Confirm);
        // Anything but y backs out of the confirmation.
        assert_eq!(a.handle(Key::Char('n')), None);
        assert_eq!(a.screen, Screen::List);
        // A second Enter does not settle: only y does.
        a.handle(Key::Enter);
        assert_eq!(a.handle(Key::Enter), None);
        assert_eq!(a.screen, Screen::List);
        a.handle(Key::Enter);
        assert_eq!(a.handle(Key::Char('y')), Some(Action::Settle));
    }

    #[test]
    fn digits_pick_and_space_unpicks() {
        let mut a = app(Isolation::Worktree);
        a.handle(Key::Char('3'));
        assert_eq!((a.sel, a.picked), (2, Some(2)));
        a.handle(Key::Char(' '));
        assert_eq!(a.picked, None);
    }

    #[test]
    fn diff_and_focus() {
        let mut a = app(Isolation::Worktree);
        a.handle(Key::Down);
        assert_eq!(a.handle(Key::Char('d')), Some(Action::Diff(1)));
        assert_eq!(a.handle(Key::Char('f')), Some(Action::Focus(1)));
        let mut a = app(Isolation::Shared);
        assert_eq!(a.handle(Key::Char('d')), None);
        assert!(a.message.is_some());
    }

    #[test]
    fn confirm_names_discarded_work() {
        let mut a = app(Isolation::Worktree);
        a.handle(Key::Char('2'));
        a.handle(Key::Enter);
        let text = render(&a, 90, 12).join("\n");
        assert!(text.contains("keep codex-1"), "{text}");
        assert!(text.contains("5 files +5 −0 will be discarded"), "{text}");
        assert!(text.contains("2 files +2 −0 will be discarded"), "{text}");
    }

    #[test]
    fn list_fills_the_popup() {
        let a = app(Isolation::Worktree);
        let lines = render(&a, 90, 12);
        assert_eq!(lines.len(), 12);
        assert!(lines[3].contains("claude-1"));
    }
}
