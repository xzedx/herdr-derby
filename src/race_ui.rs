//! The race popup: the prompt, which fresh agents to start and how, then
//! the startup progress until every lane is ready for the prompt.

use crate::editor::Editor;
use crate::lanes::{yolo_args, Isolation, KindCount, Layout, RaceConfig, MAX_LANES};
use crate::start::{Phase, Startup, LANE_TIMEOUT};
use crate::term::Key;
use crate::ui::{editor_block, fit, BOLD, DIM, GREEN, RED, RESET, REVERSE, YELLOW};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Prompt,
    Config,
}

pub struct RaceApp {
    pub editor: Editor,
    /// Every kind that can be started, with how many lanes it gets.
    pub kinds: Vec<KindCount>,
    isolation: Isolation,
    pub layout: Layout,
    /// Worktrees need a git repo with a commit to branch from.
    pub git: bool,
    pub focus: Focus,
    /// Config row: one per kind, then isolation, then layout.
    pub sel: usize,
    pub message: Option<String>,
}

pub enum Outcome {
    Start,
    Cancel,
}

impl RaceApp {
    /// Counts come from the last race; the first race gets one claude and
    /// one codex when they are installed, else the first kind.
    pub fn new(kinds: Vec<String>, cfg: &RaceConfig, git: bool) -> Self {
        let mut rows: Vec<KindCount> = kinds.iter().map(|kind| cfg.kind(kind)).collect();
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
            kinds: rows,
            isolation: cfg.isolation,
            layout: cfg.layout,
            git,
            focus: Focus::Prompt,
            sel: 0,
            message: None,
        }
    }

    pub fn total(&self) -> usize {
        self.kinds.iter().map(|k| k.count).sum()
    }

    /// Outside git every lane shares the cwd, whatever was remembered.
    pub fn isolation(&self) -> Isolation {
        if self.git {
            self.isolation
        } else {
            Isolation::Shared
        }
    }

    pub fn config(&self) -> RaceConfig {
        RaceConfig {
            counts: self.kinds.iter().filter(|k| k.count > 0 || k.yolo).cloned().collect(),
            isolation: self.isolation,
            layout: self.layout,
        }
    }

    pub fn picked(&self) -> Vec<KindCount> {
        self.kinds.iter().filter(|k| k.count > 0).cloned().collect()
    }

    /// Handle one key. Returns Some when the config step is done.
    pub fn handle(&mut self, key: Key) -> Option<Outcome> {
        self.message = None;
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
        let k = self.kinds.len();
        let rows = k + 2;
        match key {
            Key::CtrlA => self.pick_all(),
            Key::CtrlL => self.kinds.iter_mut().for_each(|r| r.count = 0),
            Key::Up | Key::Char('k') => self.sel = (self.sel + rows - 1) % rows,
            Key::Down | Key::Char('j') => self.sel = (self.sel + 1) % rows,
            _ if self.sel < k => self.count_key(key),
            Key::Left | Key::Right | Key::Char(' ') | Key::Char('h') | Key::Char('l')
                if self.sel == k =>
            {
                if self.git {
                    self.isolation = self.isolation.next();
                } else {
                    self.message = Some("not a git repo: lanes share the cwd".into());
                }
            }
            Key::Right | Key::Char(' ') | Key::Char('l') => self.layout = self.layout.next(),
            Key::Left | Key::Char('h') => self.layout = self.layout.prev(),
            _ => {}
        }
    }

    /// One lane for every kind that has none, as far as the cap allows.
    fn pick_all(&mut self) {
        let room = MAX_LANES - self.total();
        let empty = self.kinds.iter().filter(|r| r.count == 0).count();
        if empty > room {
            self.message = Some(format!("at most {MAX_LANES} agents per race"));
        }
        for r in self.kinds.iter_mut().filter(|r| r.count == 0).take(room) {
            r.count = 1;
        }
    }

    fn count_key(&mut self, key: Key) {
        let total = self.total();
        let row = &mut self.kinds[self.sel];
        match key {
            Key::Char('y') => {
                if yolo_args(&row.kind).is_some() {
                    row.yolo = !row.yolo;
                } else {
                    self.message = Some(format!("no yolo flag known for {}", row.kind));
                }
            }
            Key::Right | Key::Char(' ') | Key::Char('+') | Key::Char('=') | Key::Char('l') => {
                if total < MAX_LANES {
                    row.count += 1;
                } else {
                    self.message = Some(format!("at most {MAX_LANES} agents per race"));
                }
            }
            Key::Left | Key::Backspace | Key::Char('-') | Key::Char('h') => {
                row.count = row.count.saturating_sub(1)
            }
            Key::Char('0') => row.count = 0,
            _ => {}
        }
    }
}

/// Rows besides the editor: header, three rules, kinds, isolation, layout,
/// hints.
pub fn chrome_rows(kinds: usize) -> usize {
    7 + kinds.max(1)
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

pub fn render(app: &RaceApp, cols: usize, rows: usize) -> (Vec<String>, Option<(usize, usize)>) {
    let mut lines = Vec::new();
    let inner = cols.saturating_sub(2);
    let rule = format!("{DIM}{}{RESET}", "─".repeat(cols));
    let n = app.total();
    lines.push(format!(
        " {BOLD}Derby{RESET}  {DIM}{}{RESET}",
        fit(
            &format!(
                "race {n} fresh agent{} · {} · {}",
                plural(n),
                app.isolation().label(),
                app.layout.label()
            ),
            inner.saturating_sub(7)
        )
    ));
    lines.push(rule.clone());
    let ed_rows = rows.saturating_sub(chrome_rows(app.kinds.len()));
    let cursor = editor_block(&mut lines, &app.editor, ed_rows, cols, app.focus == Focus::Prompt);
    lines.push(rule.clone());

    let config = app.focus == Focus::Config;
    let mut row = |i: usize, plain: String, colored: String| {
        if config && i == app.sel {
            lines.push(format!("{REVERSE}{}{RESET}", fit(&plain, inner + 1)));
        } else {
            lines.push(colored);
        }
    };
    if app.kinds.is_empty() {
        row(0, String::new(), format!(" {DIM}no agent integrations found · herdr integration install <agent>{RESET}"));
    }
    for (i, k) in app.kinds.iter().enumerate() {
        let name = fit(&k.kind, 12);
        let (count, color) = if k.count > 0 {
            (format!("×{}", k.count), GREEN)
        } else {
            ("·".to_string(), DIM)
        };
        let yolo = if k.yolo { "yolo" } else { "" };
        row(
            i,
            format!("   {name:<12} {count:<3} {yolo}"),
            format!("   {name:<12} {color}{count:<3}{RESET} {RED}{yolo}{RESET}"),
        );
    }
    let k = app.kinds.len();
    let iso = if app.git {
        format!("‹ {} ›", app.isolation().label())
    } else {
        "shared cwd · not a git repo".to_string()
    };
    row(k, format!(" isolation    {iso}"), format!(" isolation    {iso}"));
    let lay = format!("‹ {} ›", app.layout.label());
    row(k + 1, format!(" layout       {lay}"), format!(" layout       {lay}"));
    lines.push(rule);

    let hint = match (&app.message, app.focus) {
        (Some(m), _) => format!(" {YELLOW}{}{RESET}", fit(m, inner)),
        (None, Focus::Prompt) => format!(
            " {DIM}{}{RESET}",
            fit("enter start · alt+enter newline · tab agents & layout · esc cancel", inner)
        ),
        (None, Focus::Config) => format!(
            " {DIM}{}{RESET}",
            fit("←→ count · y yolo · ^a all · ^l none · ↑↓ move · tab prompt", inner)
        ),
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
        let counts: Vec<usize> = a.kinds.iter().map(|k| k.count).collect();
        assert_eq!(counts, vec![0, 1, 1]);
    }

    #[test]
    fn remembered_counts_win() {
        let cfg = RaceConfig {
            counts: vec![KindCount { kind: "claude".into(), count: 2, yolo: true }],
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
        assert_eq!(a.kinds[1].count, 3);
        a.handle(Key::Char('-'));
        assert_eq!(a.kinds[1].count, 2);
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
        for k in a.kinds.iter_mut() {
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
        assert!(!a.kinds[0].yolo);
        assert!(a.message.as_deref().unwrap().contains("no yolo"));
        a.handle(Key::Down);
        a.handle(Key::Char('y'));
        assert!(a.kinds[1].yolo);
        a.handle(Key::CtrlL);
        assert_eq!(a.total(), 0);
        // Yolo is remembered even for a kind with no lanes.
        assert!(a.config().counts.iter().any(|k| k.kind == "claude" && k.yolo));
        a.kinds[2].count = 3;
        a.handle(Key::CtrlA);
        let counts: Vec<usize> = a.kinds.iter().map(|k| k.count).collect();
        assert_eq!(counts, vec![1, 1, 3]);
        // The cap still holds.
        a.handle(Key::CtrlL);
        a.kinds[2].count = MAX_LANES - 1;
        a.handle(Key::CtrlA);
        assert_eq!(a.total(), MAX_LANES);
        assert!(a.message.is_some());
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

    #[test]
    fn frames_fill_the_popup() {
        let a = app();
        let rows = 6 + chrome_rows(a.kinds.len());
        let (lines, cursor) = render(&a, 60, rows);
        assert_eq!(lines.len(), rows);
        assert_eq!(cursor, Some((2, 3)));
    }
}
