//! Settling a race: the user looks at what each lane did, picks the
//! winners, and the other lanes are removed.
//!
//! Finishing first is not winning. The list shows each lane's finish and
//! how much it changed against the race's base; `d` opens a lane's full
//! diff and `f` jumps to its pane to read the agent's answer. Nothing is
//! picked until the user picks, and removal is confirmed on a second screen
//! that names the uncommitted work it will throw away.
//!
//! With more than one open race, settle starts at a list of races, where a
//! race can also be dropped whole. Lanes the user removed by hand are taken
//! as they are: a lane whose worktree is gone still shows what its branch
//! committed, and a race with nothing left of any lane counts as settled.

use crate::git::{self, Stat};
use crate::lanes::{Isolation, Lane, Record};
use crate::rpc::Client;
use crate::start;
use crate::state;
use crate::term::Key;
use crate::ui::{fit, BOLD, DIM, GREEN, RED, RESET, REVERSE, YELLOW};
use serde_json::json;

/// Where a lane's work is now.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Place {
    /// Its worktree is there.
    Worktree,
    /// Its worktree was removed but its branch is left.
    Branch,
    /// A shared-cwd lane whose pane is open.
    Shared,
    /// No worktree and no branch (shared cwd: no pane): nothing to keep.
    Gone,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Presence {
    pub place: Place,
    /// The lane's pane is still open.
    pub pane: bool,
}

impl Presence {
    /// Nothing of the lane is left to keep or remove.
    pub fn vanished(self) -> bool {
        self.place == Place::Gone && !self.pane
    }
}

fn pane_alive(client: &Client, pane: &str) -> bool {
    // Only a definite "not found" counts as gone.
    !matches!(
        client.call("pane.get", json!({ "pane_id": pane })),
        Err(e) if e.contains("not_found")
    )
}

pub fn presence(client: &Client, rec: &Record, lane: &Lane) -> Presence {
    let pane = lane.pane_id.as_deref().is_some_and(|p| pane_alive(client, p));
    let place = match &rec.repo {
        Some(repo) if rec.worktrees() => {
            if lane.worktree.as_deref().is_some_and(|p| std::path::Path::new(p).exists()) {
                Place::Worktree
            } else if lane.branch.as_deref().is_some_and(|b| git::branch_exists(repo, b)) {
                Place::Branch
            } else {
                Place::Gone
            }
        }
        _ if pane => Place::Shared,
        _ => Place::Gone,
    };
    Presence { place, pane }
}

pub struct Row {
    /// Finish badge, or the live agent state while it still runs.
    pub status: String,
    pub presence: Presence,
    /// What the lane changed; None for shared-cwd and gone lanes.
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
    pub picked: Vec<bool>,
    pub screen: Screen,
    /// The confirmation removes the whole race, no winner kept.
    pub dropping: bool,
    /// Opened from the race list: Esc goes back there.
    pub from_list: bool,
    pub message: Option<String>,
}

#[derive(Debug, PartialEq)]
pub enum Action {
    Diff(usize),
    Focus(usize),
    Settle,
    Back,
    Quit,
}

/// A lane's status line before the helper wrote its badge.
fn live_status(agents: &[crate::model::Agent], lane: &Lane) -> String {
    match agents.iter().find(|a| Some(&a.pane_id) == lane.pane_id.as_ref()) {
        Some(a) => match a.agent_status.as_deref() {
            Some("working") => "🏇 running".into(),
            Some(s) => s.to_string(),
            None => "unknown".into(),
        },
        None => "gone".into(),
    }
}

pub fn lane_stat(rec: &Record, lane: &Lane, place: Place) -> Option<Result<Stat, String>> {
    let repo = rec.repo.as_ref()?;
    match place {
        Place::Worktree => Some(git::diff_stat(lane.worktree.as_deref()?, &repo.base)),
        Place::Branch => Some(git::branch_stat(repo, lane.branch.as_deref()?)),
        Place::Shared | Place::Gone => None,
    }
}

impl SettleApp {
    pub fn load(client: &Client, race: Summary, from_list: bool) -> Self {
        let agents = crate::model::list_agents(client).unwrap_or_default();
        let Summary { record, presence } = race;
        let rows = record
            .lanes
            .iter()
            .zip(presence)
            .map(|(lane, presence)| Row {
                status: lane.finish.clone().unwrap_or_else(|| live_status(&agents, lane)),
                presence,
                stat: lane_stat(&record, lane, presence.place),
            })
            .collect();
        Self::new(record, rows, from_list)
    }

    pub fn new(record: Record, rows: Vec<Row>, from_list: bool) -> Self {
        SettleApp {
            picked: vec![false; record.lanes.len()],
            record,
            rows,
            sel: 0,
            screen: Screen::List,
            dropping: false,
            from_list,
            message: None,
        }
    }

    /// Lanes kept; none while dropping the race.
    pub fn winners(&self) -> Vec<usize> {
        (0..self.picked.len())
            .filter(|&i| self.picked[i] && !self.dropping)
            .collect()
    }

    /// Lanes that will be removed: every lane not kept that has anything
    /// left to remove.
    pub fn losers(&self) -> Vec<usize> {
        (0..self.record.lanes.len())
            .filter(|&i| !(self.picked[i] && !self.dropping) && !self.rows[i].presence.vanished())
            .collect()
    }

    fn toggle(&mut self, i: usize) {
        if self.rows[i].presence.place == Place::Gone {
            self.message = Some(format!("{} is gone: nothing to keep", self.record.lanes[i].name));
        } else {
            self.picked[i] = !self.picked[i];
        }
    }

    pub fn handle(&mut self, key: Key) -> Option<Action> {
        self.message = None;
        if self.screen == Screen::Confirm {
            return match key {
                Key::Char('y') => Some(Action::Settle),
                Key::CtrlC | Key::Eof => Some(Action::Quit),
                _ => {
                    self.screen = Screen::List;
                    self.dropping = false;
                    None
                }
            };
        }
        let n = self.record.lanes.len();
        match key {
            Key::CtrlC | Key::Eof => return Some(Action::Quit),
            Key::Esc | Key::Char('q') if self.from_list => return Some(Action::Back),
            Key::Esc | Key::Char('q') => return Some(Action::Quit),
            Key::Char('D') => {
                self.dropping = true;
                self.screen = Screen::Confirm;
            }
            _ if n == 0 => {}
            Key::Up | Key::Char('k') => self.sel = (self.sel + n - 1) % n,
            Key::Down | Key::Char('j') => self.sel = (self.sel + 1) % n,
            Key::Char(' ') | Key::Char('x') => self.toggle(self.sel),
            Key::Char(d @ '1'..='9') => {
                let i = d as usize - '1' as usize;
                if i < n {
                    self.sel = i;
                    self.toggle(i);
                }
            }
            Key::Char('d') => match self.rows[self.sel].presence.place {
                Place::Worktree | Place::Branch => return Some(Action::Diff(self.sel)),
                Place::Shared => self.message = Some("lanes share the cwd: no per-lane diff".into()),
                Place::Gone => self.message = Some("worktree and branch are gone: no diff".into()),
            },
            Key::Char('f') => return Some(Action::Focus(self.sel)),
            Key::Enter => {
                if self.picked.contains(&true) {
                    self.screen = Screen::Confirm;
                } else {
                    self.message = Some("pick a winner first · space · or D drops the race".into());
                }
            }
            _ => {}
        }
        None
    }
}

fn changes(row: &Row) -> (String, &'static str) {
    match (&row.stat, row.presence.place) {
        (_, Place::Shared) => ("shared cwd".into(), DIM),
        (_, Place::Gone) => ("gone".into(), DIM),
        (Some(Ok(s)), Place::Branch) if s.files == 0 => ("worktree gone · no commits".into(), DIM),
        (Some(Ok(s)), Place::Branch) => (format!("worktree gone · {} committed", s.describe()), YELLOW),
        (Some(Ok(s)), _) if s.files == 0 => (s.describe(), DIM),
        (Some(Ok(s)), _) => (s.describe(), GREEN),
        (Some(Err(e)), _) => (format!("? {e}"), RED),
        (None, _) => (String::new(), DIM),
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
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
    let gone = app.losers().len();
    let what = match app.screen {
        Screen::List => format!("settle race {} · {n} lane{}{base}", r.id, plural(n)),
        Screen::Confirm if app.dropping => {
            format!("drop race {} · remove {gone} lane{}", r.id, plural(gone))
        }
        Screen::Confirm => {
            let keep: Vec<&str> = app.winners().iter().map(|&i| r.lanes[i].name.as_str()).collect();
            format!("keep {} · remove {gone} lane{}", keep.join(", "), plural(gone))
        }
    };
    format!(" {BOLD}Derby{RESET}  {DIM}{}{RESET}", fit(&what, inner.saturating_sub(7)))
}

/// What removing lane `i` will do, for the confirmation.
fn removal(app: &SettleApp, i: usize) -> (String, &'static str) {
    let r = &app.record;
    let row = &app.rows[i];
    if r.isolation == Isolation::Shared || row.stat.is_none() && row.presence.place != Place::Gone {
        return (format!("close its {}", place_word(r)), DIM);
    }
    match (&row.stat, row.presence.place) {
        (Some(Ok(s)), Place::Branch) if s.files > 0 => {
            (format!("{} committed will be discarded · branch removed", s.describe()), RED)
        }
        (Some(Ok(s)), _) if s.files > 0 => (format!("{} will be discarded", s.describe()), RED),
        (_, Place::Branch) => ("worktree already gone · branch removed".into(), DIM),
        (_, Place::Gone) => (format!("worktree and branch already gone · close its {}", place_word(r)), DIM),
        (Some(Ok(_)), _) => ("no changes · worktree and branch removed".into(), DIM),
        _ => ("worktree and branch removed".into(), DIM),
    }
}

pub fn render(app: &SettleApp, cols: usize, rows: usize) -> Vec<String> {
    let inner = cols.saturating_sub(2);
    let rule = format!("{DIM}{}{RESET}", "─".repeat(cols));
    let mut lines = vec![header(app, inner), rule.clone()];
    match app.screen {
        Screen::List => {
            lines.push(format!(
                " {DIM}{}{RESET}",
                fit(&format!("    {:<12} {:<16} {:<42} branch", "lane", "finish", "changes"), inner)
            ));
            for (i, (lane, row)) in app.record.lanes.iter().zip(&app.rows).enumerate() {
                let mark = if app.picked[i] { "[•]" } else { "[ ]" };
                let (chg, color) = changes(row);
                // Columns padded by display width: badges hold wide emoji.
                let name = pad(&fit(&lane.name, 12), 12);
                let status = pad(&fit(&row.status, 16), 16);
                let chg = pad(&fit(&chg, 42), 42);
                let branch = fit(lane.branch.as_deref().unwrap_or(""), inner.saturating_sub(77));
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
            for w in app.winners() {
                let lane = &r.lanes[w];
                let place = match app.rows[w].presence.place {
                    Place::Branch => lane.branch.as_deref(),
                    _ => lane.worktree.as_deref().or(lane.branch.as_deref()),
                }
                .unwrap_or("its pane");
                lines.push(format!(" {GREEN}keep{RESET}    {:<12} {}", fit(&lane.name, 12), fit(place, inner.saturating_sub(22))));
            }
            for i in app.losers() {
                let (what, color) = removal(app, i);
                lines.push(format!(
                    " {RED}remove{RESET}  {:<12} {color}{}{RESET}",
                    fit(&r.lanes[i].name, 12),
                    fit(&what, inner.saturating_sub(22))
                ));
            }
        }
    }
    while lines.len() + 2 < rows {
        lines.push(String::new());
    }
    lines.push(rule);
    let back = if app.from_list { "esc races" } else { "esc" };
    let hint = match (&app.message, app.screen) {
        (Some(m), _) => format!(" {YELLOW}{}{RESET}", fit(m, inner)),
        (None, Screen::List) => format!(
            " {DIM}{}{RESET}",
            fit(&format!("↑↓ move · space pick winners · d diff · f go to pane · enter review · D drop race · {back}"), inner)
        ),
        (None, Screen::Confirm) => {
            let verb = if app.dropping { "drop" } else { "settle" };
            format!(" {YELLOW}{}{RESET}", fit(&format!("y {verb} · any other key goes back"), inner))
        }
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
    let losers = app.losers();
    let mut errors = Vec::new();
    for &i in &losers {
        errors.extend(start::remove_lane(client, &app.record, &app.record.lanes[i]));
    }
    app.record.settled = true;
    if let Err(e) = app.record.save() {
        errors.push(e);
    }
    let r = &app.record;
    let (head, mut body) = if app.dropping {
        (
            format!("Derby: dropped race {}", r.id),
            format!("removed {} lane{}", losers.len(), plural(losers.len())),
        )
    } else {
        let winners = app.winners();
        let names: Vec<&str> = winners.iter().map(|&i| r.lanes[i].name.as_str()).collect();
        let body = if r.worktrees() {
            winners
                .iter()
                .map(|&i| {
                    let lane = &r.lanes[i];
                    let branch = lane.branch.as_deref().unwrap_or("");
                    match (app.rows[i].presence.place, &lane.worktree) {
                        (Place::Worktree, Some(p)) => format!("{branch}\n{p}"),
                        _ => format!("{branch} (worktree gone)"),
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            "lanes shared the cwd: only panes were closed".into()
        };
        (format!("Derby: kept {}", names.join(", ")), body)
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

/// An open race and where each of its lanes is now.
pub struct Summary {
    pub record: Record,
    pub presence: Vec<Presence>,
}

impl Summary {
    fn left(&self) -> usize {
        self.presence.iter().filter(|p| !p.vanished()).count()
    }
}

pub struct RaceList {
    pub races: Vec<Summary>,
    pub sel: usize,
    pub message: Option<String>,
}

#[derive(Debug, PartialEq)]
pub enum ListAction {
    Open(usize),
    Drop(usize),
    Quit,
}

impl RaceList {
    /// Open races, newest first. A race with nothing left of any lane was
    /// cleaned up by hand: it is marked settled and not listed.
    pub fn load(client: &Client) -> Self {
        let races = Record::open()
            .into_iter()
            .filter_map(|record| {
                let presence = record.lanes.iter().map(|l| presence(client, &record, l)).collect();
                let mut race = Summary { record, presence };
                if race.left() > 0 {
                    return Some(race);
                }
                race.record.settled = true;
                let _ = race.record.save();
                None
            })
            .collect();
        RaceList { races, sel: 0, message: None }
    }

    pub fn handle(&mut self, key: Key) -> Option<ListAction> {
        self.message = None;
        let n = self.races.len();
        match key {
            Key::Esc | Key::CtrlC | Key::Eof | Key::Char('q') => return Some(ListAction::Quit),
            _ if n == 0 => {}
            Key::Up | Key::Char('k') => self.sel = (self.sel + n - 1) % n,
            Key::Down | Key::Char('j') => self.sel = (self.sel + 1) % n,
            Key::Enter => return Some(ListAction::Open(self.sel)),
            Key::Char('D') => return Some(ListAction::Drop(self.sel)),
            _ => {}
        }
        None
    }
}

fn age(secs: f64) -> String {
    match secs.max(0.0) as u64 {
        s @ 0..=59 => format!("{s}s ago"),
        s @ 60..=3599 => format!("{}m ago", s / 60),
        s @ 3600..=86399 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86400),
    }
}

pub fn render_races(list: &RaceList, cols: usize, rows: usize) -> Vec<String> {
    let inner = cols.saturating_sub(2);
    let rule = format!("{DIM}{}{RESET}", "─".repeat(cols));
    let n = list.races.len();
    let what = match n {
        0 => "settle · no open race".to_string(),
        _ => format!("settle · {n} open race{}", plural(n)),
    };
    let mut lines = vec![
        format!(" {BOLD}Derby{RESET}  {DIM}{}{RESET}", fit(&what, inner.saturating_sub(7))),
        rule.clone(),
    ];
    if n == 0 {
        lines.push(String::new());
        lines.push(fit(" No race waits to be settled. Start one with \"Derby: race fresh agents on one prompt\".", inner));
    } else {
        lines.push(format!(
            " {DIM}{}{RESET}",
            fit(&format!("{:<12} {:<8} {:<20} prompt", "race", "started", "lanes"), inner)
        ));
    }
    let now = state::now_secs();
    for (i, race) in list.races.iter().enumerate() {
        let r = &race.record;
        let total = r.lanes.len();
        let left = race.left();
        let lanes = if left < total {
            format!("{total} lane{} · {left} left", plural(total))
        } else {
            format!("{total} lane{}", plural(total))
        };
        let prompt = r.prompt.lines().next().unwrap_or("");
        let row = format!(
            " {} {:<8} {} {}",
            pad(&fit(&r.id, 12), 12),
            age(now - r.created),
            pad(&lanes, 20),
            fit(prompt, inner.saturating_sub(44))
        );
        if i == list.sel {
            lines.push(format!("{REVERSE}{row}{RESET}"));
        } else {
            lines.push(row);
        }
    }
    while lines.len() + 2 < rows {
        lines.push(String::new());
    }
    lines.push(rule);
    lines.push(match &list.message {
        Some(m) => format!(" {YELLOW}{}{RESET}", fit(m, inner)),
        None if n == 0 => format!(" {DIM}{}{RESET}", fit("esc close", inner)),
        None => format!(" {DIM}{}{RESET}", fit("↑↓ move · enter open · D drop race · esc", inner)),
    });
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lanes::{Layout, Repo};

    fn record(isolation: Isolation) -> Record {
        let lane = |n: &str| Lane {
            name: n.into(),
            kind: "claude".into(),
            branch: Some(format!("race/x/{n}")),
            worktree: Some(format!("/wt/{n}")),
            pane_id: Some(format!("w1:{n}")),
            ..Lane::default()
        };
        Record {
            id: "x".into(),
            created: 0.0,
            cwd: "/src".into(),
            workspace_id: "w1".into(),
            repo: Some(Repo { root: "/src".into(), name: "src".into(), base: "abcdef123".into() }),
            isolation,
            layout: Layout::Split,
            lanes: vec![lane("claude-1"), lane("codex-1"), lane("claude-2")],
            settled: false,
            prompt: "Make the importer stream\nsecond line".into(),
        }
    }

    fn row(place: Place, pane: bool, files: usize) -> Row {
        Row {
            status: "🥇 1.0s".into(),
            presence: Presence { place, pane },
            stat: match place {
                Place::Worktree | Place::Branch => {
                    Some(Ok(Stat { files, added: files, removed: 0 }))
                }
                _ => None,
            },
        }
    }

    fn app(isolation: Isolation) -> SettleApp {
        let rows = vec![
            row(Place::Worktree, true, 5),
            row(Place::Worktree, true, 0),
            row(Place::Worktree, true, 2),
        ];
        SettleApp::new(record(isolation), rows, false)
    }

    #[test]
    fn nothing_is_picked_until_the_user_picks() {
        let mut a = app(Isolation::Worktree);
        assert_eq!(a.handle(Key::Enter), None);
        assert_eq!(a.screen, Screen::List);
        assert!(a.message.is_some());
        a.handle(Key::Down);
        a.handle(Key::Char(' '));
        assert_eq!(a.winners(), vec![1]);
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
    fn several_winners() {
        let mut a = app(Isolation::Worktree);
        a.handle(Key::Char('1'));
        a.handle(Key::Char('3'));
        assert_eq!((a.sel, a.winners()), (2, vec![0, 2]));
        assert_eq!(a.losers(), vec![1]);
        a.handle(Key::Char(' '));
        assert_eq!(a.winners(), vec![0]);
        a.handle(Key::Char('3'));
        a.handle(Key::Enter);
        let text = render(&a, 100, 12).join("\n");
        assert!(text.contains("keep claude-1, claude-2 · remove 1 lane"), "{text}");
    }

    #[test]
    fn drop_removes_every_lane() {
        let mut a = app(Isolation::Worktree);
        a.handle(Key::Char('2'));
        a.handle(Key::Char('D'));
        assert_eq!(a.screen, Screen::Confirm);
        let text = render(&a, 100, 12).join("\n");
        assert!(text.contains("drop race x · remove 3 lanes"), "{text}");
        assert!(text.contains("y drop"), "{text}");
        // Backing out of a drop leaves the picks alone.
        a.handle(Key::Esc);
        assert_eq!((a.screen, a.dropping), (Screen::List, false));
        assert_eq!(a.winners(), vec![1]);
    }

    #[test]
    fn lanes_removed_by_hand() {
        let rows = vec![
            row(Place::Branch, true, 3),
            row(Place::Gone, true, 0),
            row(Place::Gone, false, 0),
        ];
        let mut a = SettleApp::new(record(Isolation::Worktree), rows, true);
        let text = render(&a, 120, 12).join("\n");
        assert!(text.contains("worktree gone · 3 files +3 −0 committed"), "{text}");
        assert!(text.contains("esc races"), "{text}");
        // Its branch still has the work: it can win and be diffed.
        assert_eq!(a.handle(Key::Char('d')), Some(Action::Diff(0)));
        a.handle(Key::Char('1'));
        // Nothing to keep of a gone lane.
        a.handle(Key::Char('2'));
        assert_eq!(a.winners(), vec![0]);
        assert!(a.message.is_some());
        assert_eq!(a.handle(Key::Char('d')), None);
        // Its pane is still closed on settle; the vanished one is skipped.
        assert_eq!(a.losers(), vec![1]);
        a.handle(Key::Char('D'));
        assert_eq!(a.losers(), vec![0, 1]);
        let text = render(&a, 120, 12).join("\n");
        assert!(text.contains("3 files +3 −0 committed will be discarded"), "{text}");
        assert!(text.contains("already gone · close its pane"), "{text}");
        a.handle(Key::Char('n'));
        assert_eq!(a.handle(Key::Esc), Some(Action::Back));
    }

    #[test]
    fn diff_and_focus() {
        let mut a = app(Isolation::Worktree);
        a.handle(Key::Down);
        assert_eq!(a.handle(Key::Char('d')), Some(Action::Diff(1)));
        assert_eq!(a.handle(Key::Char('f')), Some(Action::Focus(1)));
        let rows = (0..3).map(|_| row(Place::Shared, true, 0)).collect();
        let mut a = SettleApp::new(record(Isolation::Shared), rows, false);
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

    #[test]
    fn race_list() {
        let mut rec = record(Isolation::Worktree);
        rec.created = state::now_secs() - 600.0;
        let present = Presence { place: Place::Worktree, pane: true };
        let vanished = Presence { place: Place::Gone, pane: false };
        let mut list = RaceList {
            races: vec![
                Summary { record: rec.clone(), presence: vec![present; 3] },
                Summary { record: rec, presence: vec![present, vanished, vanished] },
            ],
            sel: 0,
            message: None,
        };
        let text = render_races(&list, 100, 10).join("\n");
        assert!(text.contains("2 open races"), "{text}");
        assert!(text.contains("10m ago"), "{text}");
        assert!(text.contains("3 lanes · 1 left"), "{text}");
        assert!(text.contains("Make the importer stream"), "{text}");
        assert!(!text.contains("second line"), "{text}");
        assert_eq!(list.handle(Key::Up), None);
        assert_eq!(list.handle(Key::Enter), Some(ListAction::Open(1)));
        assert_eq!(list.handle(Key::Char('D')), Some(ListAction::Drop(1)));
        assert_eq!(list.handle(Key::Esc), Some(ListAction::Quit));
    }

    #[test]
    fn no_race_says_so() {
        let mut list = RaceList { races: Vec::new(), sel: 0, message: None };
        let text = render_races(&list, 100, 10).join("\n");
        assert!(text.contains("no open race"), "{text}");
        assert!(text.contains("No race waits to be settled"), "{text}");
        assert!(text.contains("esc close"), "{text}");
        assert_eq!(list.handle(Key::Enter), None);
        assert_eq!(list.handle(Key::Char('D')), None);
        assert_eq!(list.handle(Key::Esc), Some(ListAction::Quit));
    }
}
