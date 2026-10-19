//! The race popup: the prompt, which fresh agents to start and how, then
//! the startup progress until every lane is ready for the prompt.
//!
//! The agent list has two levels: a line per kind, and under a kind with
//! several models a line per model. A kind with one model shows it inline.
//!
//! Settle opens the same popup to start a referee: one agent, a role
//! instead of isolation and layout, and the prompt written for that role.

use crate::editor::Editor;
use crate::lanes::{yolo_args, Isolation, KindCount, Layout, RaceConfig, Role, MAX_LANES};
use crate::models;
use crate::start::{Phase, Startup, LANE_TIMEOUT};
use crate::term::Key;
use crate::ui::{editor_block, fit, BOLD, DIM, GREEN, RED, RESET, REVERSE, YELLOW};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Prompt,
    Config,
}

/// One line of the config list.
#[derive(Clone, Debug, PartialEq)]
pub enum Line {
    Kind(String),
    /// A model under its kind, by index into `rows`.
    Row(usize),
    Isolation,
    Layout,
    /// Referee setup: judge or merge.
    Role,
    /// Referee setup: where it will run.
    Where,
}

/// What the popup is starting when it starts a referee.
#[derive(Clone, Debug)]
pub struct Referee {
    pub race: String,
    pub role: Role,
    /// The racers have worktrees, so their work can be merged.
    pub can_merge: bool,
    /// The prompt each role starts with.
    pub judge_prompt: String,
    pub merge_prompt: String,
}

impl Referee {
    fn prompt(&self, role: Role) -> &str {
        match role {
            Role::Merge => &self.merge_prompt,
            _ => &self.judge_prompt,
        }
    }
}

/// The model picker over one kind: a filter, the matching models, and the
/// checked ones (None is the default model), in checking order. It opens
/// with the kind's current models checked; Enter makes them the kind's rows.
#[derive(Debug, Default)]
pub struct Picker {
    pub kind: String,
    pub filter: String,
    pub sel: usize,
    pub checked: Vec<Option<String>>,
}

/// One entry in the picker.
#[derive(Clone, Debug, PartialEq)]
pub enum Choice {
    /// No `--model`: the agent's own default.
    Default,
    Model(String),
    /// What was typed, when nothing listed is exactly that.
    Typed(String),
}

impl Choice {
    fn model(&self) -> Option<String> {
        match self {
            Choice::Default => None,
            Choice::Model(m) | Choice::Typed(m) => Some(m.clone()),
        }
    }
}

pub struct RaceApp {
    pub editor: Editor,
    /// One row per kind and model, a kind's rows next to each other.
    pub rows: Vec<KindCount>,
    isolation: Isolation,
    pub layout: Layout,
    /// Worktrees need a git repo with a commit to branch from.
    pub git: bool,
    pub focus: Focus,
    /// Selected line of the config list.
    pub sel: usize,
    pub message: Option<String>,
    pub picker: Option<Picker>,
    /// Kinds whose model lines are folded away.
    pub collapsed: HashSet<String>,
    /// What each kind's CLI says it offers.
    pub catalog: models::Shared,
    /// Models used before, per kind, newest first.
    pub recent: HashMap<String, Vec<String>>,
    /// Agent lines that fit in the popup.
    pub room: usize,
    /// Set when the popup starts a referee instead of a race.
    pub referee: Option<Referee>,
}

pub enum Outcome {
    Start,
    Cancel,
}

impl RaceApp {
    /// Rows come from the last race; the first race gets one claude and
    /// one codex when they are installed, else the first kind.
    pub fn new(kinds: Vec<String>, cfg: &RaceConfig, git: bool) -> Self {
        let mut rows = cfg.rows(&kinds);
        if rows.iter().all(|r| r.count == 0) {
            for r in rows.iter_mut().filter(|r| r.kind == "claude" || r.kind == "codex") {
                r.count = 1;
            }
            if rows.iter().all(|r| r.count == 0) {
                if let Some(r) = rows.first_mut() {
                    r.count = 1;
                }
            }
        }
        RaceApp {
            editor: Editor::default(),
            rows,
            isolation: cfg.isolation,
            layout: cfg.layout,
            git,
            focus: Focus::Prompt,
            sel: 0,
            message: None,
            picker: None,
            collapsed: HashSet::new(),
            catalog: models::Shared::default(),
            recent: HashMap::new(),
            room: usize::MAX,
            referee: None,
        }
    }

    /// The popup for one referee: the first picked row of the last
    /// referee setup, else the race default's first.
    pub fn for_referee(kinds: Vec<String>, cfg: &RaceConfig, referee: Referee) -> Self {
        let mut app = RaceApp::new(kinds, cfg, true);
        let mut seen = false;
        for r in app.rows.iter_mut() {
            r.count = usize::from(r.count > 0 && !seen);
            seen |= r.count > 0;
        }
        app.editor.set(referee.prompt(referee.role));
        app.referee = Some(referee);
        app
    }

    /// Agents the popup may start at once.
    fn cap(&self) -> usize {
        if self.referee.is_some() {
            1
        } else {
            MAX_LANES
        }
    }

    pub fn role(&self) -> Option<Role> {
        self.referee.as_ref().map(|r| r.role)
    }

    pub fn total(&self) -> usize {
        self.rows.iter().map(|k| k.count).sum()
    }

    /// Outside git every lane shares the cwd, whatever was remembered.
    pub fn isolation(&self) -> Isolation {
        if self.git {
            self.isolation
        } else {
            Isolation::Shared
        }
    }

    /// Rows worth keeping: picked, or set up with yolo or a model.
    pub fn config(&self) -> RaceConfig {
        RaceConfig {
            counts: self
                .rows
                .iter()
                .filter(|k| k.count > 0 || k.yolo || k.model.is_some())
                .cloned()
                .collect(),
            isolation: self.isolation,
            layout: self.layout,
        }
    }

    pub fn picked(&self) -> Vec<KindCount> {
        self.rows.iter().filter(|k| k.count > 0).cloned().collect()
    }

    /// Remember the models of the picked rows, newest first.
    pub fn remember_models(&mut self) {
        for r in self.rows.iter().filter(|r| r.count > 0) {
            if let Some(m) = &r.model {
                models::remember(&mut self.recent, &r.kind, m);
            }
        }
    }

    /// Kinds in list order.
    pub fn kinds(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for r in &self.rows {
            if !out.contains(&r.kind) {
                out.push(r.kind.clone());
            }
        }
        out
    }

    /// Indexes of `kind`'s rows.
    fn kind_rows(&self, kind: &str) -> Vec<usize> {
        (0..self.rows.len()).filter(|&i| self.rows[i].kind == kind).collect()
    }

    /// The config list as shown: each kind, its models when it has several
    /// and is open, then isolation and layout.
    pub fn lines(&self) -> Vec<Line> {
        let mut out = Vec::new();
        for kind in self.kinds() {
            let rows = self.kind_rows(&kind);
            out.push(Line::Kind(kind.clone()));
            if rows.len() > 1 && !self.collapsed.contains(&kind) {
                out.extend(rows.into_iter().map(Line::Row));
            }
        }
        if self.referee.is_some() {
            out.push(Line::Role);
            out.push(Line::Where);
        } else {
            out.push(Line::Isolation);
            out.push(Line::Layout);
        }
        out
    }

    /// Handle one key. Returns Some when the config step is done.
    pub fn handle(&mut self, key: Key) -> Option<Outcome> {
        self.message = None;
        if self.picker.is_some() {
            if let Key::CtrlC | Key::Eof = key {
                return Some(Outcome::Cancel);
            }
            self.picker_key(key);
            return None;
        }
        match key {
            Key::Esc if self.focus == Focus::Config => self.focus = Focus::Prompt,
            Key::Esc | Key::CtrlC | Key::Eof => return Some(Outcome::Cancel),
            Key::Enter => {
                if self.editor.is_blank() {
                    self.message = Some("type a prompt first".into());
                } else if self.total() == 0 {
                    self.message = Some("pick at least one agent · tab to the list".into());
                } else {
                    return Some(Outcome::Start);
                }
            }
            Key::Tab | Key::BackTab => {
                self.focus = match self.focus {
                    Focus::Prompt => Focus::Config,
                    Focus::Config => Focus::Prompt,
                };
            }
            k if self.focus == Focus::Config => self.config_key(k),
            k => crate::prompt_key(&mut self.editor, k),
        }
        None
    }

    fn config_key(&mut self, key: Key) {
        let lines = self.lines();
        let n = lines.len();
        self.sel = self.sel.min(n - 1);
        match key {
            Key::CtrlA => self.pick_all(),
            Key::CtrlL => self.rows.iter_mut().for_each(|r| r.count = 0),
            Key::Up | Key::Char('k') => self.sel = (self.sel + n - 1) % n,
            Key::Down | Key::Char('j') => self.sel = (self.sel + 1) % n,
            _ => match &lines[self.sel] {
                Line::Kind(kind) => self.kind_key(kind.clone(), key),
                Line::Row(i) => self.row_key(*i, key),
                Line::Isolation => match key {
                    Key::Left | Key::Right | Key::Char(' ') | Key::Char('h') | Key::Char('l') => {
                        if self.git {
                            self.isolation = self.isolation.next();
                        } else {
                            self.message = Some("not a git repo: lanes share the cwd".into());
                        }
                    }
                    _ => {}
                },
                Line::Layout => match key {
                    Key::Right | Key::Char(' ') | Key::Char('l') => self.layout = self.layout.next(),
                    Key::Left | Key::Char('h') => self.layout = self.layout.prev(),
                    _ => {}
                },
                Line::Role => match key {
                    Key::Left | Key::Right | Key::Char(' ') | Key::Char('h') | Key::Char('l') => self.switch_role(),
                    _ => {}
                },
                Line::Where => {}
            },
        }
    }

    /// Judge to merge and back. An unedited prompt follows the role; an
    /// edited one is the user's and stays.
    fn switch_role(&mut self) {
        let Some(r) = self.referee.as_mut() else { return };
        if !r.can_merge {
            self.message = Some("the racers share the cwd: nothing to merge".into());
            return;
        }
        let to = if r.role == Role::Merge { Role::Judge } else { Role::Merge };
        let edited = self.editor.text().trim_end() != r.prompt(r.role).trim_end();
        if !edited {
            self.editor.set(r.prompt(to));
        }
        r.role = to;
        if edited {
            self.message = Some("prompt edited: kept as it is".into());
        }
    }

    /// One lane for every row that has none, as far as the cap allows.
    fn pick_all(&mut self) {
        if self.referee.is_some() {
            self.message = Some("a referee is one agent".into());
            return;
        }
        let room = MAX_LANES - self.total();
        let empty = self.rows.iter().filter(|r| r.count == 0).count();
        if empty > room {
            self.message = Some(format!("at most {MAX_LANES} agents per race"));
        }
        for r in self.rows.iter_mut().filter(|r| r.count == 0).take(room) {
            r.count = 1;
        }
    }

    /// A kind's line. With one model it stands for that model's row;
    /// with several it opens and closes, and y and 0 apply to all of them.
    fn kind_key(&mut self, kind: String, key: Key) {
        let rows = self.kind_rows(&kind);
        if key == Key::Char('m') {
            return self.open_picker(&kind);
        }
        if rows.len() == 1 {
            return self.row_key(rows[0], key);
        }
        match key {
            Key::Right | Key::Char('l') => {
                self.collapsed.remove(&kind);
            }
            Key::Left | Key::Char('h') => {
                self.collapsed.insert(kind);
            }
            Key::Char(' ') => {
                if !self.collapsed.remove(&kind) {
                    self.collapsed.insert(kind);
                }
            }
            Key::Char('y') => {
                if yolo_args(&kind).is_none() {
                    self.message = Some(format!("no yolo flag known for {kind}"));
                    return;
                }
                let on = !rows.iter().all(|&i| self.rows[i].yolo);
                rows.iter().for_each(|&i| self.rows[i].yolo = on);
            }
            Key::Char('0') => rows.iter().for_each(|&i| self.rows[i].count = 0),
            Key::Char('+') | Key::Char('=') | Key::Char('-') | Key::Backspace => {
                self.message = Some("counts are per model · → to open".into());
            }
            Key::Char('d') => self.message = Some("d removes one model · m to change them all".into()),
            _ => {}
        }
    }

    fn row_key(&mut self, i: usize, key: Key) {
        let total = self.total();
        match key {
            Key::Char('m') => {
                let kind = self.rows[i].kind.clone();
                self.open_picker(&kind);
            }
            Key::Char('d') => {
                let kind = self.rows[i].kind.clone();
                if self.kind_rows(&kind).len() > 1 {
                    self.rows.remove(i);
                } else {
                    self.message = Some(format!("{kind} needs one model · m to change it"));
                }
            }
            Key::Char('y') => {
                let row = &mut self.rows[i];
                if yolo_args(&row.kind).is_some() {
                    row.yolo = !row.yolo;
                } else {
                    self.message = Some(format!("no yolo flag known for {}", row.kind));
                }
            }
            // A referee is one agent: picking a row picks it instead.
            Key::Right | Key::Char(' ') | Key::Char('+') | Key::Char('=') | Key::Char('l')
                if self.referee.is_some() =>
            {
                self.rows.iter_mut().for_each(|r| r.count = 0);
                self.rows[i].count = 1;
            }
            Key::Right | Key::Char(' ') | Key::Char('+') | Key::Char('=') | Key::Char('l') => {
                if total < MAX_LANES {
                    self.rows[i].count += 1;
                } else {
                    self.message = Some(format!("at most {MAX_LANES} agents per race"));
                }
            }
            Key::Left | Key::Backspace | Key::Char('-') | Key::Char('h') => {
                let row = &mut self.rows[i];
                row.count = row.count.saturating_sub(1)
            }
            Key::Char('0') => self.rows[i].count = 0,
            _ => {}
        }
    }

    /// A kind's models as the picker checks them: none for a lone unused
    /// default row, which is only a placeholder.
    fn current_models(&self, kind: &str) -> Vec<Option<String>> {
        let rows = self.kind_rows(kind);
        if let [i] = rows[..] {
            if self.rows[i].model.is_none() && self.rows[i].count == 0 {
                return Vec::new();
            }
        }
        rows.iter().map(|&i| self.rows[i].model.clone()).collect()
    }

    fn open_picker(&mut self, kind: &str) {
        let checked = self.current_models(kind);
        let choices = self.choices(kind, "");
        let sel = checked
            .first()
            .and_then(|m| choices.iter().position(|c| c.model() == *m))
            .unwrap_or(0);
        self.picker = Some(Picker {
            kind: kind.to_string(),
            filter: String::new(),
            sel,
            checked,
        });
    }

    /// The picker's entries for `kind`: default, the kind's models now,
    /// recent ones, then the listed ones, those containing `filter`; then
    /// what was typed, unless that is listed exactly.
    pub fn choices(&self, kind: &str, filter: &str) -> Vec<Choice> {
        let f = filter.trim().to_lowercase();
        let mut seen: Vec<String> = Vec::new();
        let mut out = Vec::new();
        if f.is_empty() || "default".contains(&f) {
            out.push(Choice::Default);
        }
        let cat = self.catalog.lock().unwrap();
        let current = self.rows.iter().filter(|r| r.kind == kind).filter_map(|r| r.model.clone());
        let recent = self.recent.get(kind).into_iter().flatten().cloned();
        let listed = cat.lists.get(kind).into_iter().flatten().cloned();
        for m in current.chain(recent).chain(listed) {
            if seen.contains(&m) {
                continue;
            }
            if m.to_lowercase().contains(&f) {
                out.push(Choice::Model(m.clone()));
            }
            seen.push(m);
        }
        let typed = filter.trim();
        if !typed.is_empty() && !seen.iter().any(|m| m == typed) {
            out.push(Choice::Typed(typed.to_string()));
        }
        out
    }

    fn picker_key(&mut self, key: Key) {
        let Some(p) = self.picker.as_mut() else { return };
        match key {
            Key::Esc => {
                self.picker = None;
                return;
            }
            Key::Up => p.sel = p.sel.saturating_sub(1),
            Key::Down => p.sel += 1,
            Key::Char(' ') => {
                let (kind, filter, sel) = (p.kind.clone(), p.filter.clone(), p.sel);
                if let Some(m) = self.choices(&kind, &filter).get(sel).map(Choice::model) {
                    let p = self.picker.as_mut().unwrap();
                    match p.checked.iter().position(|c| *c == m) {
                        Some(i) => {
                            p.checked.remove(i);
                        }
                        None => p.checked.push(m),
                    }
                    p.sel += 1;
                }
            }
            Key::Char(c) => {
                p.filter.push(c);
                p.sel = 0;
            }
            Key::Paste(s) => {
                p.filter.push_str(s.lines().next().unwrap_or(""));
                p.sel = 0;
            }
            Key::Backspace => {
                p.filter.pop();
                p.sel = 0;
            }
            Key::CtrlU => {
                p.filter.clear();
                p.sel = 0;
            }
            Key::Enter => {
                let (kind, filter, sel) = (p.kind.clone(), p.filter.clone(), p.sel);
                let mut models = p.checked.clone();
                // Nothing checked: the highlighted one.
                if models.is_empty() {
                    let choices = self.choices(&kind, &filter);
                    if let Some(c) = choices.get(sel.min(choices.len().saturating_sub(1))) {
                        models.push(c.model());
                    }
                }
                self.picker = None;
                self.set_models(&kind, models);
                return;
            }
            _ => {}
        }
        let Some((kind, filter)) = self.picker.as_ref().map(|p| (p.kind.clone(), p.filter.clone())) else {
            return;
        };
        let n = self.choices(&kind, &filter).len();
        if let Some(p) = self.picker.as_mut() {
            p.sel = p.sel.min(n.saturating_sub(1));
        }
    }

    /// Make `models` the kind's rows, in that order: rows already on one of
    /// them keep their count and yolo, new ones get one lane while the cap
    /// allows and the kind's yolo, the rest go. New rows that would not fit
    /// in the popup are left out.
    pub fn set_models(&mut self, kind: &str, models: Vec<Option<String>>) {
        let old = self.kind_rows(kind);
        let (Some(&first), false) = (old.first(), models.is_empty()) else {
            return;
        };
        let yolo = old.iter().any(|&i| self.rows[i].yolo);
        let mut total = self.total() - old.iter().map(|&i| self.rows[i].count).sum::<usize>();
        // Lines this kind may use: what it has now plus what is free.
        let shown = self.lines().len() - 2;
        let mine = if old.len() > 1 && !self.collapsed.contains(kind) { old.len() + 1 } else { 1 };
        let budget = self.room.saturating_sub(shown - mine).max(1);
        let mut group: Vec<KindCount> = Vec::new();
        let mut left_out = 0;
        for m in models {
            if group.iter().any(|r| r.model == m) {
                continue;
            }
            let row = match old.iter().find(|&&i| self.rows[i].model == m) {
                Some(&i) => self.rows[i].clone(),
                None => {
                    // One line for the kind, plus one per model when several.
                    if group.len() + 2 > budget {
                        left_out += 1;
                        continue;
                    }
                    let mut r = KindCount::new(kind);
                    r.model = m;
                    r.yolo = yolo;
                    r.count = usize::from(total < self.cap());
                    r
                }
            };
            total += row.count;
            group.push(row);
        }
        self.rows.retain(|r| r.kind != kind);
        let at = first.min(self.rows.len());
        self.rows.splice(at..at, group);
        self.collapsed.remove(kind);
        if left_out > 0 {
            self.message = Some(format!("no room for {left_out} more model{}", plural(left_out)));
        }
    }
}

/// Rows besides the editor: header, three rules, agent lines, isolation,
/// layout, hints.
pub fn chrome_rows(lines: usize) -> usize {
    7 + lines.max(1)
}

/// Agent lines a popup `height` rows tall has room for, keeping one editor row.
pub fn room(height: usize) -> usize {
    height.saturating_sub(chrome_rows(0)).max(1)
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// The last part of a model, which is what tells models apart in a list.
fn short(model: &str) -> &str {
    model.rsplit('/').next().unwrap_or(model)
}

pub fn render(app: &RaceApp, cols: usize, rows: usize) -> (Vec<String>, Option<(usize, usize)>) {
    if let Some(p) = &app.picker {
        return render_picker(app, p, cols, rows);
    }
    let mut lines = Vec::new();
    let inner = cols.saturating_sub(2);
    let rule = format!("{DIM}{}{RESET}", "─".repeat(cols));
    let n = app.total();
    let what = match &app.referee {
        Some(r) => format!("{} race {} · {}", r.role.word(), r.race, app.layout.label()),
        None => format!(
            "race {n} fresh agent{} · {} · {}",
            plural(n),
            app.isolation().label(),
            app.layout.label()
        ),
    };
    lines.push(format!(" {BOLD}Derby{RESET}  {DIM}{}{RESET}", fit(&what, inner.saturating_sub(7))));
    lines.push(rule.clone());
    let list = app.lines();
    let ed_rows = rows.saturating_sub(chrome_rows(list.len() - 2));
    let cursor = editor_block(&mut lines, &app.editor, ed_rows, cols, app.focus == Focus::Prompt);
    lines.push(rule.clone());

    let config = app.focus == Focus::Config;
    let sel = app.sel.min(list.len() - 1);
    if app.rows.is_empty() {
        lines.push(format!(" {DIM}no agent integrations found · herdr integration install <agent>{RESET}"));
    }
    let count_text = |c: usize| {
        if c > 0 {
            (format!("×{c}"), GREEN)
        } else {
            ("·".to_string(), DIM)
        }
    };
    for (li, line) in list.iter().enumerate() {
        let (plain, colored) = match line {
            Line::Kind(kind) => {
                let rows = app.kind_rows(kind);
                let name = fit(kind, 10);
                let (arrow, summary, count, yolo) = if let [i] = rows[..] {
                    let r = &app.rows[i];
                    (" ", r.model.as_deref().unwrap_or("default").to_string(), r.count, r.yolo)
                } else {
                    let open = !app.collapsed.contains(kind);
                    let summary = if open {
                        String::new()
                    } else {
                        rows.iter()
                            .map(|&i| app.rows[i].model.as_deref().map_or("default", short))
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    let count = rows.iter().map(|&i| app.rows[i].count).sum();
                    let yolo = rows.iter().any(|&i| app.rows[i].yolo);
                    (if open { "▾" } else { "▸" }, summary, count, yolo)
                };
                let summary = fit(&summary, 30);
                let dim = if summary == "default" { DIM } else { "" };
                let (count, color) = count_text(count);
                let yolo = if yolo { "yolo" } else { "" };
                (
                    format!(" {arrow} {name:<10} {summary:<30} {count:<3} {yolo}"),
                    format!(" {arrow} {name:<10} {dim}{summary:<30}{RESET} {color}{count:<3}{RESET} {RED}{yolo}{RESET}"),
                )
            }
            Line::Row(i) => {
                let r = &app.rows[*i];
                let model = fit(r.model.as_deref().unwrap_or("default"), 39);
                let dim = if r.model.is_none() { DIM } else { "" };
                let (count, color) = count_text(r.count);
                let yolo = if r.yolo { "yolo" } else { "" };
                (
                    format!("     {model:<39} {count:<3} {yolo}"),
                    format!("     {dim}{model:<39}{RESET} {color}{count:<3}{RESET} {RED}{yolo}{RESET}"),
                )
            }
            Line::Isolation => {
                let iso = if app.git {
                    format!("‹ {} ›", app.isolation().label())
                } else {
                    "shared cwd · not a git repo".to_string()
                };
                let s = format!(" isolation    {iso}");
                (s.clone(), s)
            }
            Line::Layout => {
                let s = format!(" layout       ‹ {} ›", app.layout.label());
                (s.clone(), s)
            }
            Line::Role => {
                let role = app.role().unwrap_or_default();
                let what = match role {
                    Role::Merge => "combine the racers' work, can win",
                    _ => "read the racers' work, name the winners",
                };
                let s = format!(" role         ‹ {} ›  {what}", role.word());
                (s.clone(), format!(" role         ‹ {} ›  {DIM}{what}{RESET}", role.word()))
            }
            Line::Where => {
                let what = match app.referee.as_ref().is_some_and(|r| r.can_merge) {
                    true => "its own worktree from the race's base",
                    false => "the race's cwd",
                };
                let s = format!(" runs in      {what}");
                (s.clone(), format!(" runs in      {DIM}{what}{RESET}"))
            }
        };
        if config && li == sel {
            lines.push(format!("{REVERSE}{}{RESET}", fit(&plain, inner + 1)));
        } else {
            lines.push(colored);
        }
    }
    lines.push(rule);

    let hint = match (&app.message, app.focus) {
        (Some(m), _) => m.clone(),
        (None, Focus::Prompt) if app.referee.is_some() => {
            "enter start · alt+enter newline · tab agent & role · esc back".into()
        }
        (None, Focus::Prompt) => "enter start · alt+enter newline · tab agents & layout · esc cancel".into(),
        (None, Focus::Config) => match &list[sel] {
            Line::Kind(k) if app.kind_rows(k).len() > 1 => {
                "←→ close/open · m models · y yolo · 0 none · ^a all · ^l none".into()
            }
            Line::Kind(_) | Line::Row(_) if app.referee.is_some() => {
                "→ pick · ← none · m models · y yolo · tab prompt".into()
            }
            Line::Kind(_) => "←→ count · m models · y yolo · ^a all · ^l none · tab prompt".into(),
            Line::Row(_) => "←→ count · m models · d remove · y yolo · ^a all · ^l none".into(),
            _ => "←→ change · ↑↓ move · tab prompt · enter start".into(),
        },
    };
    let color = if app.message.is_some() { YELLOW } else { DIM };
    lines.push(format!(" {color}{}{RESET}", fit(&hint, inner)));
    (lines, cursor)
}

fn render_picker(app: &RaceApp, p: &Picker, cols: usize, rows: usize) -> (Vec<String>, Option<(usize, usize)>) {
    let mut lines = Vec::new();
    let inner = cols.saturating_sub(2);
    let rule = format!("{DIM}{}{RESET}", "─".repeat(cols));
    let kind = &p.kind;
    let choices = app.choices(kind, &p.filter);
    let (pending, error) = {
        let c = app.catalog.lock().unwrap();
        (c.pending.contains(kind), c.errors.get(kind).cloned())
    };
    let listed = choices.iter().filter(|c| matches!(c, Choice::Model(_))).count();
    let mut head = format!("models for {kind} · {listed} match{}", if listed == 1 { "" } else { "es" });
    if !p.checked.is_empty() {
        head.push_str(&format!(" · {} checked", p.checked.len()));
    }
    if pending {
        head.push_str(" · listing…");
    }
    lines.push(format!(" {BOLD}Derby{RESET}  {DIM}{}{RESET}", fit(&head, inner.saturating_sub(7))));
    lines.push(rule.clone());
    let filter = fit(&p.filter, inner.saturating_sub(3));
    lines.push(format!(" › {filter}"));
    let cursor = Some((2, 3 + filter.chars().count()));
    lines.push(rule.clone());
    let list_rows = rows.saturating_sub(6).max(1);
    let top = p.sel.saturating_sub(list_rows - 1);
    let in_use: Vec<Option<String>> = app.rows.iter().filter(|r| &r.kind == kind).map(|r| r.model.clone()).collect();
    for (i, c) in choices.iter().enumerate().skip(top).take(list_rows) {
        let (plain, color) = match c {
            Choice::Default => ("default · no --model".to_string(), DIM),
            Choice::Model(m) if in_use.contains(&Some(m.clone())) => (format!("{m}  · in the list"), ""),
            Choice::Model(m) if app.recent.get(kind).is_some_and(|r| r.contains(m)) => (format!("{m}  · used before"), ""),
            Choice::Model(m) => (m.clone(), ""),
            Choice::Typed(m) => (format!("use “{m}”"), YELLOW),
        };
        let mark = if p.checked.contains(&c.model()) { "[x]" } else { "[ ]" };
        let text = fit(&plain, inner.saturating_sub(6));
        if i == p.sel {
            lines.push(format!("{REVERSE}{}{RESET}", fit(&format!(" {mark} {text}"), inner + 1)));
        } else {
            lines.push(format!(" {GREEN}{mark}{RESET} {color}{text}{RESET}"));
        }
    }
    while lines.len() + 2 < rows {
        lines.push(String::new());
    }
    lines.push(rule);
    let hint = match error {
        Some(e) if !pending => format!(" {YELLOW}{}{RESET}", fit(&format!("couldn't list: {e}"), inner)),
        _ => format!(" {DIM}{}{RESET}", fit("type to filter · space check · ^n^p move · enter apply · esc back", inner)),
    };
    lines.push(hint);
    (lines, cursor)
}

/// What a key means while lanes start.
#[derive(Debug, PartialEq)]
pub enum Action {
    None,
    TrustAll,
    DropTrust,
    Recheck,
    Up,
    Down,
    /// Remove the selected lane; the others carry on.
    DropOne,
    Race,
    Abort,
}

/// The race can start on its own: everything is ready.
pub fn all_ready(phases: &[Phase]) -> bool {
    let live: Vec<&Phase> = phases.iter().filter(|p| **p != Phase::Dropped).collect();
    !live.is_empty() && live.iter().all(|p| **p == Phase::Ready)
}

fn recheckable(p: &Phase) -> bool {
    matches!(p, Phase::NeedsYou(_) | Phase::TimedOut(_))
}

/// A lane `x` can remove now: created, and not removed already.
pub fn droppable(p: &Phase) -> bool {
    *p != Phase::Dropped && !p.creating()
}

pub fn startup_action(phases: &[Phase], key: &Key) -> Action {
    let busy = phases.iter().any(Phase::busy);
    let trust = phases.contains(&Phase::Trust);
    let ready = phases.contains(&Phase::Ready);
    match key {
        Key::Esc | Key::CtrlC | Key::Eof => Action::Abort,
        Key::Char('y') if trust => Action::TrustAll,
        Key::Char('n') if trust => Action::DropTrust,
        Key::Char('r') if phases.iter().any(recheckable) => Action::Recheck,
        Key::Up | Key::Char('k') => Action::Up,
        Key::Down | Key::Char('j') => Action::Down,
        Key::Char('x') => Action::DropOne,
        Key::Enter if ready && !busy && !trust => Action::Race,
        _ => Action::None,
    }
}

fn phase_text(p: &Phase) -> (String, &'static str) {
    match p {
        Phase::Queued => ("queued".into(), DIM),
        Phase::Creating => ("creating…".into(), YELLOW),
        Phase::Starting => ("starting…".into(), YELLOW),
        Phase::Settling => ("getting ready…".into(), YELLOW),
        Phase::Trust => ("asks to trust its folder".into(), RED),
        Phase::NeedsYou(why) => (format!("{why} · see its pane"), RED),
        Phase::Ready => ("ready".into(), GREEN),
        Phase::Failed(e) => (format!("✗ {e}"), RED),
        Phase::TimedOut(why) => (format!("not ready after {}s · {why}", LANE_TIMEOUT.as_secs()), RED),
        Phase::Dropped => ("dropped".into(), DIM),
    }
}

pub fn startup_hint(phases: &[Phase], aborting: bool) -> String {
    if aborting {
        return "stopping · cleaning up the lanes…".into();
    }
    let busy = phases.iter().any(Phase::busy);
    let trust = phases.iter().filter(|p| **p == Phase::Trust).count();
    let ready = phases.iter().filter(|p| **p == Phase::Ready).count();
    let left = phases
        .iter()
        .filter(|p| **p != Phase::Ready && **p != Phase::Dropped)
        .count();
    let mut parts: Vec<String> = Vec::new();
    if trust > 0 {
        parts.push(format!(
            "{trust} ask{} to trust {} folder · y trust · n drop",
            if trust == 1 { "s" } else { "" },
            if trust == 1 { "its" } else { "their" }
        ));
    }
    if busy {
        parts.push("starting".into());
    }
    if phases.iter().any(recheckable) {
        parts.push("r recheck".into());
    }
    if ready > 0 && !busy && trust == 0 {
        parts.push(if left > 0 {
            format!("enter race {ready}, remove the rest")
        } else {
            format!("enter race {ready}")
        });
    }
    if phases.iter().any(droppable) {
        parts.push("x drop".into());
    }
    if parts.is_empty() {
        parts.push("nothing started".into());
    }
    parts.push("esc abort".into());
    parts.join(" · ")
}

pub fn render_startup(
    st: &Startup,
    prompt: &str,
    sel: usize,
    aborting: bool,
    cols: usize,
    rows: usize,
) -> Vec<String> {
    let mut lines = Vec::new();
    let inner = cols.saturating_sub(2);
    let rule = format!("{DIM}{}{RESET}", "─".repeat(cols));
    let n = st.record.lanes.len();
    lines.push(format!(
        " {BOLD}Derby{RESET}  {DIM}{}{RESET}",
        fit(
            &format!("race {} · {n} agent{}", st.record.id, plural(n)),
            inner.saturating_sub(7)
        )
    ));
    lines.push(rule.clone());
    let first = prompt.lines().next().unwrap_or("");
    lines.push(format!(" {DIM}› {}{RESET}", fit(first, inner.saturating_sub(2))));
    lines.push(rule.clone());
    for (i, (lane, phase)) in st.record.lanes.iter().zip(&st.phases).enumerate() {
        let name = fit(&lane.name, 12);
        let place = fit(lane.branch.as_deref().unwrap_or("shared cwd"), 26);
        let (text, color) = phase_text(phase);
        let mark = if i == sel { "›" } else { " " };
        let left = format!(" {mark} {name:<12} {place:<26} ");
        let room = inner.saturating_sub(left.chars().count());
        lines.push(format!("{left}{color}{}{RESET}", fit(&text, room)));
    }
    // The selected lane's whole story, when its row cuts it short.
    if let Some(phase) = st.phases.get(sel) {
        let (text, color) = phase_text(phase);
        if text.chars().count() + 44 > inner {
            lines.push(format!("   {color}{}{RESET}", fit(&text, inner.saturating_sub(2))));
        }
    }
    // Keep the hint on the last row.
    while lines.len() + 2 < rows {
        lines.push(String::new());
    }
    lines.push(rule);
    lines.push(format!(" {YELLOW}{}{RESET}", fit(&startup_hint(&st.phases, aborting), inner)));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> RaceApp {
        RaceApp::new(
            vec!["pi".into(), "claude".into(), "codex".into()],
            &RaceConfig::default(),
            true,
        )
    }

    #[test]
    fn first_race_defaults_to_claude_and_codex() {
        let a = app();
        let counts: Vec<usize> = a.rows.iter().map(|k| k.count).collect();
        assert_eq!(counts, vec![0, 1, 1]);
    }

    #[test]
    fn remembered_counts_win() {
        let cfg = RaceConfig {
            counts: vec![KindCount { count: 2, yolo: true, ..KindCount::new("claude") }],
            isolation: Isolation::Shared,
            layout: Layout::Workspace,
        };
        let a = RaceApp::new(vec!["claude".into(), "codex".into()], &cfg, true);
        assert_eq!(a.total(), 2);
        assert_eq!(a.isolation(), Isolation::Shared);
        assert_eq!(a.config(), cfg);
    }

    #[test]
    fn config_keys() {
        let mut a = app();
        a.handle(Key::Tab);
        assert_eq!(a.focus, Focus::Config);
        a.handle(Key::Down); // claude
        a.handle(Key::Right);
        a.handle(Key::Char('+'));
        assert_eq!(a.rows[1].count, 3);
        a.handle(Key::Char('-'));
        assert_eq!(a.rows[1].count, 2);
        a.handle(Key::Down);
        a.handle(Key::Char('0')); // codex off
        assert_eq!(a.total(), 2);
        a.handle(Key::Down); // isolation
        a.handle(Key::Char(' '));
        assert_eq!(a.isolation(), Isolation::Shared);
        a.handle(Key::Down); // layout
        a.handle(Key::Left);
        assert_eq!(a.layout, Layout::Workspace);
        a.handle(Key::Down); // wraps to the first kind
        assert_eq!(a.sel, 0);
        // Typing in the prompt is text, not config.
        a.handle(Key::Esc);
        a.handle(Key::Char('+'));
        assert_eq!(a.editor.text(), "+");
        assert!(matches!(a.handle(Key::Enter), Some(Outcome::Start)));
    }

    #[test]
    fn lane_cap_and_empty_checks() {
        let mut a = app();
        a.handle(Key::Tab);
        for _ in 0..20 {
            a.handle(Key::Char('+'));
        }
        assert_eq!(a.total(), MAX_LANES);
        assert!(a.message.is_some());
        for k in a.rows.iter_mut() {
            k.count = 0;
        }
        a.editor.set("go");
        assert!(a.handle(Key::Enter).is_none());
        assert!(a.message.as_deref().unwrap().contains("pick"));
    }

    #[test]
    fn yolo_and_select_all() {
        let mut a = app();
        a.handle(Key::Tab);
        a.handle(Key::Char('y')); // pi has no yolo flag
        assert!(!a.rows[0].yolo);
        assert!(a.message.as_deref().unwrap().contains("no yolo"));
        a.handle(Key::Down);
        a.handle(Key::Char('y'));
        assert!(a.rows[1].yolo);
        a.handle(Key::CtrlL);
        assert_eq!(a.total(), 0);
        // Yolo is remembered even for a kind with no lanes.
        assert!(a.config().counts.iter().any(|k| k.kind == "claude" && k.yolo));
        a.rows[2].count = 3;
        a.handle(Key::CtrlA);
        let counts: Vec<usize> = a.rows.iter().map(|k| k.count).collect();
        assert_eq!(counts, vec![1, 1, 3]);
        // The cap still holds.
        a.handle(Key::CtrlL);
        a.rows[2].count = MAX_LANES - 1;
        a.handle(Key::CtrlA);
        assert_eq!(a.total(), MAX_LANES);
        assert!(a.message.is_some());
    }

    fn with_models(mut a: RaceApp) -> RaceApp {
        let mut c = a.catalog.lock().unwrap();
        c.lists.insert("claude".into(), vec!["opus".into(), "sonnet".into(), "haiku".into()]);
        drop(c);
        a.recent.insert("claude".into(), vec!["sonnet".into()]);
        a
    }

    fn show(a: &RaceApp) -> Vec<(String, Option<String>, usize, bool)> {
        a.rows.iter().map(|r| (r.kind.clone(), r.model.clone(), r.count, r.yolo)).collect()
    }

    fn row(kind: &str, model: Option<&str>, count: usize, yolo: bool) -> (String, Option<String>, usize, bool) {
        (kind.into(), model.map(String::from), count, yolo)
    }

    #[test]
    fn picker_lists_default_current_recent_then_listed() {
        let mut a = with_models(app());
        use Choice::*;
        assert_eq!(
            a.choices("claude", ""),
            vec![Default, Model("sonnet".into()), Model("opus".into()), Model("haiku".into())]
        );
        assert_eq!(
            a.choices("claude", "o"),
            vec![Model("sonnet".into()), Model("opus".into()), Typed("o".into())]
        );
        assert_eq!(a.choices("claude", "opus"), vec![Model("opus".into())]);
        assert_eq!(a.choices("claude", "opus[1m]"), vec![Typed("opus[1m]".into())]);
        assert_eq!(a.choices("pi", ""), vec![Default]);
        // A model only the list has (typed earlier) is still offered.
        a.rows[1].model = Some("opus[1m]".into());
        assert_eq!(a.choices("claude", "")[1], Model("opus[1m]".into()));
    }

    #[test]
    fn several_models_make_a_kind_open_up() {
        let mut a = with_models(app());
        a.rows[1].yolo = true;
        a.handle(Key::Tab);
        a.handle(Key::Down); // claude
        a.handle(Key::Char('m'));
        // Its default row is in use, so default starts checked.
        assert_eq!(a.picker.as_ref().unwrap().checked, vec![None]);
        a.handle(Key::Char(' ')); // uncheck default
        a.handle(Key::Char(' ')); // sonnet
        a.handle(Key::Char(' ')); // opus
        assert_eq!(a.picker.as_ref().unwrap().checked, vec![Some("sonnet".into()), Some("opus".into())]);
        a.handle(Key::Enter);
        assert_eq!(
            show(&a),
            vec![
                row("pi", None, 0, false),
                row("claude", Some("sonnet"), 1, true),
                row("claude", Some("opus"), 1, true),
                row("codex", None, 1, false),
            ]
        );
        assert_eq!(
            a.lines(),
            vec![
                Line::Kind("pi".into()),
                Line::Kind("claude".into()),
                Line::Row(1),
                Line::Row(2),
                Line::Kind("codex".into()),
                Line::Isolation,
                Line::Layout,
            ]
        );
        // On the kind line: arrows fold, y and 0 cover every model.
        assert_eq!(a.sel, 1);
        a.handle(Key::Left);
        assert_eq!(a.lines().len(), 5);
        a.handle(Key::Right);
        a.handle(Key::Char('y'));
        assert!(a.rows[1..3].iter().all(|r| !r.yolo));
        a.handle(Key::Char('+'));
        assert!(a.message.as_deref().unwrap().contains("per model"));
        // On a model line: its own count.
        a.handle(Key::Down);
        a.handle(Key::Char('+'));
        assert_eq!(a.rows[1].count, 2);
        // m again opens with both checked; unchecking one removes its row
        // and the other keeps its count.
        a.handle(Key::Char('m'));
        assert_eq!(a.picker.as_ref().unwrap().checked, vec![Some("sonnet".into()), Some("opus".into())]);
        a.handle(Key::Down); // the cursor starts on sonnet; move to opus
        a.handle(Key::Char(' '));
        a.handle(Key::Enter);
        assert_eq!(show(&a)[1..2], [row("claude", Some("sonnet"), 2, false)]);
        assert_eq!(a.lines().len(), 5); // one model: inline again
        // d removes a model, but never a kind's last one.
        a.sel = 1;
        a.handle(Key::Char('d'));
        assert!(a.message.as_deref().unwrap().contains("needs one model"));
    }

    #[test]
    fn a_lone_unused_default_is_not_checked() {
        let mut a = with_models(app());
        a.handle(Key::Tab);
        a.handle(Key::Char('m')); // pi: default ×0
        assert!(a.picker.as_ref().unwrap().checked.is_empty());
        // Nothing checked: enter takes the highlighted one.
        a.handle(Key::Esc);
        a.sel = 1;
        a.rows[1].count = 0;
        a.handle(Key::Char('m'));
        for c in "haiku".chars() {
            a.handle(Key::Char(c));
        }
        a.handle(Key::Enter);
        assert_eq!(show(&a)[1], row("claude", Some("haiku"), 1, false)); // a new model gets a lane
    }

    #[test]
    fn models_that_do_not_fit_are_left_out() {
        let mut a = with_models(app());
        a.room = 5; // pi, claude + 2 models, codex
        a.handle(Key::Tab);
        a.handle(Key::Down);
        a.handle(Key::Char('m'));
        for _ in 0..4 {
            a.handle(Key::Char(' ')); // default off, sonnet, opus, haiku
        }
        a.handle(Key::Enter);
        assert_eq!(a.lines().len() - 2, 5);
        assert!(a.message.as_deref().unwrap().contains("no room for 1 more model"));
    }

    #[test]
    fn picker_frame_fills_the_popup() {
        let mut a = with_models(app());
        a.handle(Key::Tab);
        a.handle(Key::Down);
        a.handle(Key::Char('m'));
        let (lines, cursor) = render(&a, 60, 12);
        assert_eq!(lines.len(), 12);
        assert_eq!(cursor, Some((2, 3)));
        assert!(lines[0].contains("models for claude"));
    }

    #[test]
    fn no_git_means_shared() {
        let mut a = RaceApp::new(vec!["claude".into()], &RaceConfig::default(), false);
        assert_eq!(a.isolation(), Isolation::Shared);
        a.handle(Key::Tab);
        a.handle(Key::Down); // isolation row
        a.handle(Key::Char(' '));
        assert_eq!(a.isolation(), Isolation::Shared);
        assert!(a.message.is_some());
    }

    #[test]
    fn startup_keys() {
        use Phase::*;
        let p = vec![Ready, Trust, Starting];
        assert_eq!(startup_action(&p, &Key::Char('y')), Action::TrustAll);
        assert_eq!(startup_action(&p, &Key::Char('n')), Action::DropTrust);
        // Enter waits until nothing is starting or asking.
        assert_eq!(startup_action(&p, &Key::Enter), Action::None);
        assert_eq!(startup_action(&p, &Key::Char('r')), Action::None);
        let p = vec![Ready, NeedsYou("x".into()), Failed("x".into())];
        assert_eq!(startup_action(&p, &Key::Char('r')), Action::Recheck);
        assert_eq!(startup_action(&p, &Key::Enter), Action::Race);
        assert_eq!(startup_action(&p, &Key::Esc), Action::Abort);
        assert_eq!(startup_action(&p, &Key::Char('j')), Action::Down);
        assert_eq!(startup_action(&p, &Key::Up), Action::Up);
        assert_eq!(startup_action(&p, &Key::Char('x')), Action::DropOne);
        let p = vec![Ready, TimedOut("x".into())];
        assert_eq!(startup_action(&p, &Key::Char('r')), Action::Recheck);
        assert!(!all_ready(&p));
        assert!(all_ready(&[Ready, Ready]));
        // A dropped lane does not hold the race back.
        assert!(all_ready(&[Ready, Dropped]));
        assert!(!all_ready(&[Dropped]));
        assert!(!droppable(&Creating));
        assert!(droppable(&Starting));
    }

    #[test]
    fn startup_hints() {
        use Phase::*;
        assert_eq!(
            startup_hint(&[Trust, Ready], false),
            "1 asks to trust its folder · y trust · n drop · x drop · esc abort"
        );
        assert_eq!(startup_hint(&[Starting, Ready], false), "starting · x drop · esc abort");
        assert_eq!(
            startup_hint(&[Ready, Failed("x".into())], false),
            "enter race 1, remove the rest · x drop · esc abort"
        );
        assert_eq!(
            startup_hint(&[Ready, TimedOut("x".into()), Dropped], false),
            "r recheck · enter race 1, remove the rest · x drop · esc abort"
        );
        assert_eq!(startup_hint(&[Ready, Dropped], false), "enter race 1 · x drop · esc abort");
        assert!(startup_hint(&[Ready], true).starts_with("stopping"));
    }

    fn referee(can_merge: bool) -> RaceApp {
        RaceApp::for_referee(
            vec!["pi".into(), "claude".into(), "codex".into()],
            &RaceConfig::default(),
            Referee {
                race: "x".into(),
                role: Role::Judge,
                can_merge,
                judge_prompt: "judge them".into(),
                merge_prompt: "merge them".into(),
            },
        )
    }

    #[test]
    fn a_referee_is_one_agent() {
        let mut a = referee(true);
        // The race default picks claude and codex: only the first stays.
        assert_eq!(a.picked().iter().map(|r| r.kind.as_str()).collect::<Vec<_>>(), vec!["claude"]);
        assert_eq!(a.editor.text(), "judge them");
        a.handle(Key::Tab);
        a.handle(Key::Down);
        a.handle(Key::Down); // codex
        a.handle(Key::Right);
        assert_eq!(a.picked().iter().map(|r| r.kind.as_str()).collect::<Vec<_>>(), vec!["codex"]);
        a.handle(Key::Right);
        assert_eq!(a.total(), 1);
        a.handle(Key::CtrlA);
        assert_eq!((a.total(), a.message.as_deref()), (1, Some("a referee is one agent")));
        a.handle(Key::Left);
        assert_eq!(a.total(), 0);
        assert!(a.handle(Key::Enter).is_none());
        a.handle(Key::Right);
        assert!(matches!(a.handle(Key::Enter), Some(Outcome::Start)));
    }

    #[test]
    fn switching_role_swaps_an_unedited_prompt() {
        let mut a = referee(true);
        assert_eq!(a.lines()[3..], [Line::Role, Line::Where]);
        a.handle(Key::Tab);
        a.sel = 3;
        a.handle(Key::Right);
        assert_eq!((a.role(), a.editor.text()), (Some(Role::Merge), "merge them".to_string()));
        let text = render(&a, 76, 6 + chrome_rows(3)).0.join("\n");
        assert!(text.contains("merge race x"), "{text}");
        assert!(text.contains("‹ merge ›"), "{text}");
        assert!(text.contains("its own worktree"), "{text}");
        // Edited: the prompt is the user's now.
        a.editor.set("merge them carefully");
        a.handle(Key::Right);
        assert_eq!((a.role(), a.editor.text()), (Some(Role::Judge), "merge them carefully".to_string()));
        assert_eq!(a.message.as_deref(), Some("prompt edited: kept as it is"));
        // Shared racers: there is nothing to merge.
        let mut a = referee(false);
        a.handle(Key::Tab);
        a.sel = 3;
        a.handle(Key::Right);
        assert_eq!(a.role(), Some(Role::Judge));
        assert!(a.message.is_some());
    }

    #[test]
    fn referee_frames_fill_the_popup() {
        let a = referee(true);
        let rows = 6 + chrome_rows(a.rows.len());
        assert_eq!(render(&a, 60, rows).0.len(), rows);
    }

    #[test]
    fn frames_fill_the_popup() {
        let a = app();
        let rows = 6 + chrome_rows(a.rows.len());
        let (lines, cursor) = render(&a, 60, rows);
        assert_eq!(lines.len(), rows);
        assert_eq!(cursor, Some((2, 3)));
    }
}
