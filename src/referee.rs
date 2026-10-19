//! What a referee is asked. A judge reads the racers' work and writes a
//! verdict file that settle picks the winners from; a merge lane combines
//! their work on its own branch, which can then win like any racer.
//!
//! The prompt names paths relative to the referee's own directory, so it
//! can be written before the referee's agent, and with it its name and
//! worktree, is picked. It is shown in the popup and can be edited there.

use crate::lanes::{Lane, Record, Role, VERDICT_FILE};
use crate::settle::Place;

/// A racer as settle sees it: its finish and where its work is now.
pub struct Racer<'a> {
    pub lane: &'a Lane,
    pub status: &'a str,
    pub place: Place,
}

fn lane_lines(racers: &[Racer], shared: bool) -> String {
    racers
        .iter()
        .map(|r| {
            let l = r.lane;
            let model = l
                .args
                .iter()
                .position(|a| a == "--model")
                .and_then(|i| l.args.get(i + 1))
                .map(|m| format!(", model {m}"))
                .unwrap_or_default();
            let mut s = format!("- {} ({}{model}): {}", l.name, l.kind, r.status);
            if !shared {
                match (r.place, &l.worktree, &l.branch) {
                    (Place::Worktree, Some(w), Some(b)) => s.push_str(&format!("\n  worktree {w}\n  branch   {b}")),
                    (_, _, Some(b)) => s.push_str(&format!("\n  worktree gone · branch {b}")),
                    _ => s.push_str("\n  worktree and branch gone"),
                }
            }
            s
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// Lines indented here are indented in the prompt: no `\` continuations.
const READ_WORK: &str = concat!(
    "Their work is often not committed, so read the worktree, not only the branch:\n",
    "  git -C <worktree> status --short\n",
    "  git -C <worktree> diff {base}\n",
    "and read the untracked files that status lists. Where only the branch is\n",
    "left, it has what was committed: git diff {base}..<branch>. You may run a\n",
    "lane's tests in its worktree, but do not change the lanes' files.",
);

const READ_TALK: &str = concat!(
    "You run inside Herdr, so you can also read what each lane's agent said with\n",
    "the herdr CLI (see the herdr skill), for how it reasoned and whether it\n",
    "says it ran the tests. Only lanes whose pane is still open can be read:\n",
    "  herdr agent read <lane> --source recent-unwrapped --lines 300",
);

pub fn prompt(role: Role, rec: &Record, racers: &[Racer]) -> String {
    let shared = !rec.worktrees();
    let base = rec.repo.as_ref().map(|r| r.base.as_str()).unwrap_or("HEAD");
    let n = racers.len();
    let setup = if shared {
        format!("{n} coding agents were given the task below, all in the same directory, {}.", rec.cwd)
    } else {
        format!("{n} coding agents were given the task below, each in its own git worktree branched from {base}.")
    };
    let read = if shared {
        "They shared one directory, so their changes are mixed there: judge them\nmostly by what each one said.".to_string()
    } else {
        READ_WORK.replace("{base}", base)
    };
    let lanes = lane_lines(racers, shared);
    let task = rec.prompt.trim();
    match role {
        Role::Merge => format!(
            "You are merging Derby race {id} in Herdr. {setup} You did not race.\n\
             Your current directory is your own worktree, on your own branch from\n\
             {base}. Combine the best of their work there.\n\n\
             The task they were given:\n\"\"\"\n{task}\n\"\"\"\n\n\
             The lanes:\n{lanes}\n\n\
             {read}\n\n\
             {READ_TALK}\n\n\
             Start from the strongest lane and take what the others did better.\n\
             To bring a lane's work over, uncommitted work included:\n  \
             git -C <worktree> diff {base} | git apply -3\n\
             and copy its untracked files; for what it committed, git merge or\n\
             git cherry-pick <branch>. Run the tests, commit the result on your\n\
             branch, then say what you took from which lane.",
            id = rec.id,
        ),
        _ => format!(
            "You are judging Derby race {id} in Herdr. {setup} You did not race.\n\
             Decide which result is best. Finishing first is not winning.\n\n\
             The task they were given:\n\"\"\"\n{task}\n\"\"\"\n\n\
             The lanes:\n{lanes}\n\n\
             {read}\n\n\
             {READ_TALK}\n\n\
             When you have decided, write your verdict as JSON to {VERDICT_FILE} in\n\
             your current directory, and do not commit it:\n  \
             {{\"winners\": [\"<lane>\"], \"reason\": \"<one or two sentences>\"}}\n\
             Use lane names from the list. Name more than one winner only if they\n\
             are equally good. Then explain your verdict here.",
            id = rec.id,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lanes::{Isolation, Layout, Repo};

    fn record(isolation: Isolation) -> Record {
        let lane = |n: &str, args: &[&str]| Lane {
            name: n.into(),
            kind: "claude".into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            branch: Some(format!("race/x/{n}")),
            worktree: Some(format!("/wt/{n}")),
            ..Lane::default()
        };
        Record {
            id: "x".into(),
            created: 0.0,
            cwd: "/src".into(),
            workspace_id: "w1".into(),
            repo: Some(Repo { root: "/src".into(), name: "src".into(), base: "abc123".into() }),
            isolation,
            layout: Layout::Split,
            lanes: vec![lane("claude-opus-1", &["--model", "opus"]), lane("claude-1", &[])],
            settled: false,
            prompt: "Make the importer stream\n".into(),
        }
    }

    fn racers(rec: &Record) -> Vec<Racer<'_>> {
        vec![
            Racer { lane: &rec.lanes[0], status: "🥇 2m55s", place: Place::Worktree },
            Racer { lane: &rec.lanes[1], status: "⚠ needs you", place: Place::Branch },
        ]
    }

    #[test]
    fn judge_reads_worktrees_and_writes_a_verdict() {
        let rec = record(Isolation::Worktree);
        let p = prompt(Role::Judge, &rec, &racers(&rec));
        assert!(p.contains("judging Derby race x"), "{p}");
        assert!(p.contains("\"\"\"\nMake the importer stream\n\"\"\""), "{p}");
        assert!(p.contains("- claude-opus-1 (claude, model opus): 🥇 2m55s\n  worktree /wt/claude-opus-1\n  branch   race/x/claude-opus-1"), "{p}");
        assert!(p.contains("- claude-1 (claude): ⚠ needs you\n  worktree gone · branch race/x/claude-1"), "{p}");
        assert!(p.contains("git -C <worktree> diff abc123"), "{p}");
        assert!(p.contains("herdr agent read <lane>"), "{p}");
        assert!(p.contains(".derby-verdict.json"), "{p}");
        assert!(!p.contains("{base}"), "{p}");
        // Commands keep their indent.
        assert!(p.contains("\n  git -C <worktree> status --short\n"), "{p}");
        assert!(p.contains("\n  herdr agent read"), "{p}");
        assert!(p.contains("\n  {\"winners\""), "{p}");
    }

    #[test]
    fn merge_commits_on_its_branch() {
        let rec = record(Isolation::Worktree);
        let p = prompt(Role::Merge, &rec, &racers(&rec));
        assert!(p.contains("merging Derby race x"), "{p}");
        assert!(p.contains("\n  git -C <worktree> diff abc123 | git apply -3\n"), "{p}");
        assert!(!p.contains(VERDICT_FILE), "{p}");
    }

    #[test]
    fn shared_lanes_are_judged_by_what_they_said() {
        let rec = record(Isolation::Shared);
        let p = prompt(Role::Judge, &rec, &racers(&rec));
        assert!(p.contains("all in the same directory, /src"), "{p}");
        assert!(!p.contains("worktree /wt"), "{p}");
        assert!(p.contains("judge them\nmostly by what each one said"), "{p}");
    }
}
