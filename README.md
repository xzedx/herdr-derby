# herdr-derby

Send one prompt to every coding agent in the current [Herdr](https://herdr.dev)
tab and watch them race.

Press a key, type (or paste) a prompt, press Enter. Every agent in the tab gets
the same message at the same moment. While they work, each pane border shows a
live timer (`🏇 12.4s · claude`); when they settle it shows the finishing rank
(`🥇 2.9s · codex`, `🥈 4.0s · claude`), and one notification sums up the
race.

Race mode goes one step further: it starts fresh agents of the kinds you pick
(the same kind more than once if you like), each in its own git worktree, gives
them the same prompt, and when they are done lets you compare their work, keep
the one you want and remove the rest.

Everything is drawn by Herdr: the prompt box is a Herdr terminal popup, the
timers and ranks are pane border titles. No separate window, and it works over
`herdr --remote` / SSH.

```
 Derby  broadcast to 4 of 4 agents in this tab
──────────────────────────────────────────────
 › Compare the two caching strategies in src/cache
   and recommend one.
──────────────────────────────────────────────
 [x] 1  kimi      idle
 [x] 2  omp       idle
 [x] 3  codex     idle
 [x] 4  claude    idle
──────────────────────────────────────────────
 enter send · alt+enter newline · tab targets · esc cancel
```

## How it works

- Targets are the agents Herdr recognizes in the tab (`agent.list`). Idle and
  done agents start checked. A working agent starts unchecked: it would take
  the prompt mid-turn. An agent waiting on an approval or question is shown
  but cannot be selected; Herdr would refuse the prompt.
- Delivery uses Herdr's own `agent.prompt`, which respects each pane's
  bracketed-paste mode, so multi-line and CJK prompts arrive as one message.
- Finish detection is Herdr's native agent state. Agents do not have to
  cooperate (no marker files, no wrapper scripts).
- Enter hands the race to a small detached helper, so the popup closes at once.
  The helper submits to all targets in parallel, keeps the border timers
  ticking, ranks the finishers and shows the notification.
- An agent that stops on an approval or question mid-race is not done: its
  border says `⚠ needs you · 33.5s`, one notification tells you, and the race
  follows it again once you answer. **Time spent waiting on you does not
  count**: ranks go by each agent's own working time, and the wait is shown
  next to it (`🥈 3m32s (+1m10s waiting)`).
- A prompt the agent never took is told apart from a run that went wrong:
  `✗ not sent` versus `✗ failed: <reason>`. If Herdr sees no activity after
  sending (`agent_prompt_stalled`), Derby reads the screen: only when the
  agent is idle and nothing of the prompt shows does it send once more;
  otherwise it marks the agent failed rather than risk sending it twice.

## Race mode

Press the race key, type the prompt, Tab to the list below it, set how many of
each agent to start, pick isolation and layout, press Enter.

```
 Derby  race 3 fresh agents · worktree per agent · split this tab
──────────────────────────────────────────────
 › Make the importer stream instead of
   loading the whole file.
──────────────────────────────────────────────
   claude       ×2
   codex        ×1
   kimi         ·
 isolation    ‹ worktree per agent ›
 layout       ‹ split this tab ›
──────────────────────────────────────────────
 ←→ change · ↑↓ move · tab prompt · enter start
```

- **Agents**: the Herdr integrations that are installed and on PATH.
- **Isolation**: one git worktree per agent, on a branch
  `race/<run-id>/<agent>` from the current HEAD, or every agent in the shared
  cwd. Outside a git repo it is always the shared cwd.
- **Layout**: split the current tab, one tab per agent, or one workspace per
  agent. Worktrees are made with `git worktree add` and live next to the ones
  `herdr worktree create` makes, in `~/.herdr/worktrees/<repo>/`. Started from
  the repo's main workspace, workspace lanes open under it in the sidebar
  (`worktree.open`); started anywhere else they are plain workspaces, so a
  race never leaves an extra main workspace behind.
- The popup stays open while the agents start and only hands the prompt over
  once every agent is ready: idle, taking input, and its screen holding still
  (codex reports `working` for a while after starting; claude and kimi read
  idle while a dialog or redraw is still up). Each agent gets 60 s; one that
  is not ready by then shows `not ready after 60s` with the last thing on its
  screen. One that needs signing in, or exits, says so instead of spinning.
- `↑↓` picks an agent and `x` removes it (pane, worktree and branch) while the
  others carry on. Enter races the ready ones and removes the rest. Esc
  removes everything created so far. Anything cleanup could not remove is
  named in a notification.
- **Folder trust**: a worktree of a repo you already trust in claude or codex
  is trusted too, so usually nothing asks. kimi trusts each folder on its own
  and asks in every new worktree. If an agent stops at its folder-trust
  dialog, the popup says so: `y` answers it (Derby moves the highlight to the
  trust option, checks the screen shows it highlighted, then presses Enter;
  Claude highlights "No, exit" by default), `n` drops those agents. Any other
  dialog is left for you; `r` looks again once you answered it.
- The last choice of agents, isolation and layout is remembered.

From there it is the same race: border timers, ranks, one notification.

### Settle

Finishing first is not winning. The settle popup lists the latest race:

```
 Derby  settle race 1019-143205 · 3 lanes · base a1b2c3d
──────────────────────────────────────────────────────────────────
     lane         finish           changes              branch
 ( ) claude-1     🥈 4m12s         5 files +120 −34     race/1019-143205/claude-1
 ( ) codex-1      🥇 2m55s         2 files +18 −2       race/1019-143205/codex-1
 ( ) claude-2     ⚠ needs you      no changes           race/1019-143205/claude-2
──────────────────────────────────────────────────────────────────
 ↑↓ move · space pick winner · d diff · f go to pane · enter review · esc
```

"changes" is each lane's work against the race's base: commits, edits and new
files, uncommitted ones included (measured through a temporary index, the
lane's own index is not touched). `d` shows the full diff in git's pager, `f`
jumps to the lane's pane to read what the agent said. Nothing is picked until
you pick; Enter then shows what will be removed, with uncommitted work called
out, and only `y` does it (Enter or any other key goes back): the other
lanes' panes, tabs or workspaces are closed, their worktrees force-removed and
their branches deleted. The winner's pane,
worktree and branch stay as they are. In a shared-cwd race only the other panes
are closed and no files are touched. No merging: that part is yours.

An unsettled race waits in the plugin state dir until you settle it.

## Requirements

- Herdr 0.9.1 or newer, with the agent integrations you use installed
  (`herdr integration install <agent>`). If an agent is not listed, check
  `herdr status`: the running server may be older than the client.
- A Rust toolchain (`cargo`); `herdr plugin install` runs
  `cargo build --release`. Four small dependencies: serde, serde_json, libc,
  unicode-width. No TUI framework.
- `git` on PATH for race mode's worktrees.

## Install

```bash
herdr plugin install xzedx/herdr-derby
```

or, for a local checkout:

```bash
cargo build --release
herdr plugin link "$PWD"
```

Herdr plugins cannot ship default keybindings. Bind the action in
`~/.config/herdr/config.toml`:

```toml
[[keys.command]]
key = "prefix+ctrl+b"
type = "plugin_action"
command = "xzedx.derby.broadcast"
description = "derby: broadcast prompt to this tab's agents"

[[keys.command]]
key = "prefix+ctrl+r"
type = "plugin_action"
command = "xzedx.derby.race"
description = "derby: race fresh agents"

[[keys.command]]
key = "prefix+ctrl+s"
type = "plugin_action"
command = "xzedx.derby.settle"
description = "derby: settle the latest race"
```

## Keys

Prompt:

| Key | Action |
| --- | --- |
| Enter | send |
| Alt+Enter, Ctrl+J | new line (Shift+Enter too, if the terminal reports it) |
| ↑ on an empty prompt | recall the last prompt |
| Ctrl+U | clear |
| Tab | go to the target list |
| Esc, Ctrl+C | cancel |

Target list:

| Key | Action |
| --- | --- |
| ↑ ↓ / j k | move |
| Space, x | toggle |
| 1-9 | toggle that agent |
| a | toggle all |
| Tab, Esc | back to the prompt |
| Enter | send |

Race popup, below the prompt:

| Key | Action |
| --- | --- |
| ↑ ↓ / j k | move |
| → + Space / ← - | one more / one less of that agent; change isolation or layout |
| 0 | none of that agent |
| Tab, Esc | back to the prompt |
| Enter | start |

While agents start:

| Key | Action |
| --- | --- |
| ↑ ↓ / j k | pick an agent |
| x | remove that agent; the others carry on |
| y / n | trust / remove the agents at their folder-trust dialog |
| r | look again at agents that needed you or ran out of time |
| Enter | race the ready ones, remove the rest |
| Esc | stop and remove everything |

Settle:

| Key | Action |
| --- | --- |
| ↑ ↓ / j k | move |
| Space, x / 1-9 | pick the winner |
| d | full diff of the lane |
| f | go to the lane's pane |
| Enter | review what will be removed |
| y | on the review: settle (nothing else does) |
| Esc, q | close |

## Command line

```bash
herdr-derby --send [--tab w1:t1] "prompt"   # race without the popup, print the result
herdr-derby --dump [--tab w1:t1]            # render one popup frame to stdout
```

`DERBY_TRACE=<file>` appends timing traces; `DERBY_TEST_INPUT=<bytes>` feeds
key bytes to the popup before the terminal (testing).

## License

MIT
