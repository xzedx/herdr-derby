//! Race mode's data: the config the popup edits, the lanes a race runs in,
//! and the race record kept in the state dir until the race is settled.
//!
//! A lane is one fresh agent in its own pane, tab or workspace, optionally
//! in its own git worktree. Everything here is plain data and planning, so
//! it can be tested without Herdr or git.

use crate::state;
use serde::{Deserialize, Serialize};

/// Fresh agents per race, all kinds together.
pub const MAX_LANES: usize = 8;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum Isolation {
    /// One git worktree and branch per lane.
    #[default]
    Worktree,
    /// Every lane works in the same directory.
    Shared,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    /// Split the current tab.
    #[default]
    Split,
    /// One tab per lane in the current workspace.
    Tab,
    /// One workspace per lane.
    Workspace,
}

impl Isolation {
    pub fn label(self) -> &'static str {
        match self {
            Isolation::Worktree => "worktree per agent",
            Isolation::Shared => "shared cwd",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Isolation::Worktree => Isolation::Shared,
            Isolation::Shared => Isolation::Worktree,
        }
    }
}

impl Layout {
    pub fn label(self) -> &'static str {
        match self {
            Layout::Split => "split this tab",
            Layout::Tab => "tab per agent",
            Layout::Workspace => "workspace per agent",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Layout::Split => Layout::Tab,
            Layout::Tab => Layout::Workspace,
            Layout::Workspace => Layout::Split,
        }
    }

    pub fn prev(self) -> Self {
        self.next().next()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct KindCount {
    pub kind: String,
    pub count: usize,
    /// Start this kind with its skip-the-approvals flag.
    #[serde(default)]
    pub yolo: bool,
}

/// The flags that start `kind` without approval prompts, if it has them.
pub fn yolo_args(kind: &str) -> Option<&'static [&'static str]> {
    Some(match kind {
        "claude" => &["--dangerously-skip-permissions"],
        "codex" | "gemini" | "qwen" => &["--yolo"],
        // Its --yolo still asks before risky actions.
        "kimi" => &["--auto"],
        "omp" => &["--auto-approve"],
        "copilot" => &["--allow-all-tools"],
        _ => return None,
    })
}

/// What the race popup remembers between races.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct RaceConfig {
    #[serde(default)]
    pub counts: Vec<KindCount>,
    #[serde(default)]
    pub isolation: Isolation,
    #[serde(default)]
    pub layout: Layout,
}

impl RaceConfig {
    pub fn load() -> Self {
        std::fs::read_to_string(state::state_dir().join("race-config.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Ok(s) = serde_json::to_string(self) {
            let _ = std::fs::write(state::state_dir().join("race-config.json"), s);
        }
    }

    /// What was remembered for `kind`: nothing means no lanes, no yolo.
    pub fn kind(&self, kind: &str) -> KindCount {
        self.counts.iter().find(|c| c.kind == kind).cloned().unwrap_or(KindCount {
            kind: kind.into(),
            count: 0,
            yolo: false,
        })
    }
}

/// The repository a race branches from.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Repo {
    /// Main checkout's root, also when the race starts in a linked worktree.
    pub root: String,
    /// Directory name of the main checkout, as Herdr names worktree folders.
    pub name: String,
    /// Commit the lanes branch from.
    pub base: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Lane {
    /// Live agent name, also the border label.
    pub name: String,
    pub kind: String,
    /// Extra agent arguments, such as the yolo flag.
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub worktree: Option<String>,
    #[serde(default)]
    pub pane_id: Option<String>,
    /// Set for the tab layout: the lane owns the whole tab.
    #[serde(default)]
    pub tab_id: Option<String>,
    /// Set for the workspace layout: the lane owns the whole workspace.
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// Finishing badge written by the race helper ("🥇 3.6s").
    #[serde(default)]
    pub finish: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Record {
    pub id: String,
    /// Epoch seconds, for picking the latest race.
    pub created: f64,
    pub cwd: String,
    /// Workspace the race was started from; tab lanes open there.
    pub workspace_id: String,
    #[serde(default)]
    pub repo: Option<Repo>,
    pub isolation: Isolation,
    pub layout: Layout,
    pub lanes: Vec<Lane>,
    #[serde(default)]
    pub settled: bool,
    /// What the agents were asked, to tell races apart in settle.
    #[serde(default)]
    pub prompt: String,
}

fn races_dir() -> std::path::PathBuf {
    let dir = state::state_dir().join("races");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

impl Record {
    pub fn save(&self) -> Result<(), String> {
        let body = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(races_dir().join(format!("{}.json", self.id)), body)
            .map_err(|e| format!("save race {}: {e}", self.id))
    }

    pub fn load(id: &str) -> Option<Record> {
        std::fs::read_to_string(races_dir().join(format!("{id}.json")))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    }

    /// Races not settled yet, newest first.
    pub fn open() -> Vec<Record> {
        let Ok(entries) = std::fs::read_dir(races_dir()) else {
            return Vec::new();
        };
        let mut races: Vec<Record> = entries
            .flatten()
            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
            .filter_map(|s| serde_json::from_str::<Record>(&s).ok())
            .filter(|r| !r.settled)
            .collect();
        races.sort_by(|a, b| b.created.total_cmp(&a.created));
        races
    }

    pub fn worktrees(&self) -> bool {
        self.isolation == Isolation::Worktree && self.repo.is_some()
    }
}

/// Race id from local time: "1019-143205". Short enough for branch names,
/// unique enough for one person starting races by hand.
pub fn run_id() -> String {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        format!(
            "{:02}{:02}-{:02}{:02}{:02}",
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}

/// Agent names must match `[a-z][a-z0-9_-]{0,31}` and be unique among live
/// agents: "claude-1", or the next free number.
pub fn unique_name(kind: &str, taken: &[String]) -> String {
    let mut base: String = kind
        .chars()
        .map(|c| c.to_ascii_lowercase())
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if !base.starts_with(|c: char| c.is_ascii_lowercase()) {
        base.insert(0, 'a');
    }
    base.truncate(24);
    (1..)
        .map(|n| format!("{base}-{n}"))
        .find(|n| !taken.contains(n))
        .unwrap()
}

/// Where a lane's worktree goes: next to the ones `herdr worktree create`
/// makes, `~/.herdr/worktrees/<repo>/<branch with / as ->`.
pub fn worktree_path(home: &str, repo_name: &str, branch: &str) -> String {
    format!("{home}/.herdr/worktrees/{repo_name}/{}", branch.replace('/', "-"))
}

/// One lane per picked agent, in config order. Agent names avoid live
/// ones, and each branch is named after its agent so the two match.
pub fn plan_lanes(
    counts: &[KindCount],
    id: &str,
    taken: &[String],
    repo: Option<&Repo>,
    isolation: Isolation,
    home: &str,
) -> Vec<Lane> {
    let mut taken = taken.to_vec();
    let mut lanes = Vec::new();
    for kc in counts {
        for _ in 0..kc.count {
            let name = unique_name(&kc.kind, &taken);
            taken.push(name.clone());
            let (branch, worktree) = match (isolation, repo) {
                (Isolation::Worktree, Some(r)) => {
                    let b = format!("race/{id}/{name}");
                    let p = worktree_path(home, &r.name, &b);
                    (Some(b), Some(p))
                }
                _ => (None, None),
            };
            let args = match yolo_args(&kc.kind) {
                Some(a) if kc.yolo => a.iter().map(|s| s.to_string()).collect(),
                _ => Vec::new(),
            };
            lanes.push(Lane {
                name,
                kind: kc.kind.clone(),
                args,
                branch,
                worktree,
                ..Lane::default()
            });
        }
    }
    lanes
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum From {
    Anchor,
    Lane(usize),
}

/// One `pane.split`: produce lane `lane` by splitting `from`. `ratio` is
/// the share the split pane keeps.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Split {
    pub lane: usize,
    pub from: From,
    pub right: bool,
    pub ratio: f64,
}

/// Lay `n` lanes out right of the anchor pane: columns of up to three
/// equal rows, the anchor keeping an equal column share. Column heads come
/// first, since splitting a head down afterwards only splits that column.
pub fn split_plan(n: usize) -> Vec<Split> {
    if n == 0 {
        return Vec::new();
    }
    let cols = n.div_ceil(3);
    let mut rows = vec![n / cols; cols];
    for r in rows.iter_mut().take(n % cols) {
        *r += 1;
    }
    let heads: Vec<usize> = rows
        .iter()
        .scan(0, |acc, &r| {
            let h = *acc;
            *acc += r;
            Some(h)
        })
        .collect();
    let mut plan = Vec::new();
    for (c, &head) in heads.iter().enumerate() {
        let from = if c == 0 { From::Anchor } else { From::Lane(heads[c - 1]) };
        // `from` still spans this many equal columns (the anchor's included).
        let parts = (cols - c + 1) as f64;
        plan.push(Split {
            lane: head,
            from,
            right: true,
            ratio: 1.0 / parts,
        });
    }
    for (c, &head) in heads.iter().enumerate() {
        for j in 1..rows[c] {
            plan.push(Split {
                lane: head + j,
                from: From::Lane(head + j - 1),
                right: false,
                ratio: 1.0 / (rows[c] - j + 1) as f64,
            });
        }
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kc(kind: &str, count: usize) -> KindCount {
        KindCount {
            kind: kind.into(),
            count,
            yolo: false,
        }
    }

    #[test]
    fn names_skip_live_agents() {
        let taken = vec!["claude-1".to_string(), "claude-2".to_string()];
        assert_eq!(unique_name("claude", &taken), "claude-3");
        assert_eq!(unique_name("codex", &taken), "codex-1");
        assert_eq!(unique_name("9x", &[]), "a9x-1");
    }

    #[test]
    fn lanes_get_branches_and_worktrees() {
        let repo = Repo {
            root: "/src/app".into(),
            name: "app".into(),
            base: "abc".into(),
        };
        let lanes = plan_lanes(
            &[kc("claude", 2), kc("codex", 1), kc("kimi", 0)],
            "1019-143205",
            &["claude-1".into()],
            Some(&repo),
            Isolation::Worktree,
            "/home/z",
        );
        let names: Vec<_> = lanes.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, vec!["claude-2", "claude-3", "codex-1"]);
        assert_eq!(lanes[1].branch.as_deref(), Some("race/1019-143205/claude-3"));
        assert_eq!(
            lanes[2].worktree.as_deref(),
            Some("/home/z/.herdr/worktrees/app/race-1019-143205-codex-1")
        );
    }

    #[test]
    fn shared_lanes_have_no_branch() {
        let lanes = plan_lanes(&[kc("claude", 1)], "x", &[], None, Isolation::Worktree, "/h");
        assert_eq!(lanes[0].branch, None);
        assert_eq!(lanes[0].worktree, None);
    }

    #[test]
    fn yolo_lanes_get_their_flag() {
        let mut claude = kc("claude", 1);
        claude.yolo = true;
        let mut pi = kc("pi", 1);
        pi.yolo = true; // no known flag: started plain
        let lanes = plan_lanes(&[claude, kc("codex", 1), pi], "x", &[], None, Isolation::Shared, "/h");
        assert_eq!(lanes[0].args, vec!["--dangerously-skip-permissions"]);
        assert!(lanes[1].args.is_empty());
        assert!(lanes[2].args.is_empty());
    }

    #[test]
    fn split_plan_single_column() {
        let p = split_plan(3);
        assert_eq!(p.len(), 3);
        assert_eq!((p[0].lane, p[0].from, p[0].right), (0, From::Anchor, true));
        assert!((p[0].ratio - 0.5).abs() < 1e-9);
        // Rows: lane 0 keeps a third, then lane 1 keeps half the rest.
        assert_eq!((p[1].lane, p[1].from, p[1].right), (1, From::Lane(0), false));
        assert!((p[1].ratio - 1.0 / 3.0).abs() < 1e-9);
        assert_eq!((p[2].lane, p[2].from), (2, From::Lane(1)));
        assert!((p[2].ratio - 0.5).abs() < 1e-9);
    }

    #[test]
    fn split_plan_columns_come_first() {
        // 5 lanes: two columns of 3 and 2 rows.
        let p = split_plan(5);
        let order: Vec<(usize, From, bool)> = p.iter().map(|s| (s.lane, s.from, s.right)).collect();
        assert_eq!(
            order,
            vec![
                (0, From::Anchor, true),
                (3, From::Lane(0), true),
                (1, From::Lane(0), false),
                (2, From::Lane(1), false),
                (4, From::Lane(3), false),
            ]
        );
        // Anchor keeps a third of the width, lane 0 half of the rest.
        assert!((p[0].ratio - 1.0 / 3.0).abs() < 1e-9);
        assert!((p[1].ratio - 0.5).abs() < 1e-9);
        let mut lanes: Vec<usize> = p.iter().map(|s| s.lane).collect();
        lanes.sort();
        assert_eq!(lanes, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn layout_cycles() {
        assert_eq!(Layout::Split.next(), Layout::Tab);
        assert_eq!(Layout::Split.prev(), Layout::Workspace);
        assert_eq!(Isolation::Shared.next(), Isolation::Worktree);
    }
}
