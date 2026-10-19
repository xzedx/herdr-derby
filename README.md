<p align="center"><img src="docs/logo.png" width="160" alt="herdr-derby"></p>

# herdr-derby

English | [简体中文](README.zh-CN.md)

A [Herdr](https://herdr.dev) plugin that races coding agents on the same prompt.

- **Broadcast**: one prompt to every agent in the current tab. Pane borders
  show a live timer (`🏇 12.4s · claude`), then the finishing rank
  (`🥇 2.9s · codex`).
- **Race**: start fresh agents, any mix of kinds and models, each in its own
  git worktree. Give them the prompt, keep the best result, remove the rest.

It all runs inside Herdr (a popup and pane borders), so it works over
`herdr --remote` and SSH too.

## Broadcast

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

- Idle and finished agents start checked. Working agents start unchecked, so
  they don't get a prompt mid-turn. Agents waiting on you can't be picked.
- Prompts go through Herdr's `agent.prompt`, so multi-line and CJK text
  arrives as one message.
- Finishing is read from Herdr's own agent state. No marker files or wrapper
  scripts.
- An agent that stops for an approval or a question shows `⚠ needs you`. Time
  spent waiting on you isn't counted: `🥈 3m32s (+1m10s waiting)`.
- `✗ not sent` means the agent never got the prompt; `✗ failed` means it got
  it and something went wrong. A prompt is never delivered twice.

## Race

![Starting a race of seven agents, each in its own worktree](docs/start-race.gif)

```
 Derby  race 3 fresh agents · worktree per agent · split this tab
──────────────────────────────────────────────
 › Make the importer stream instead of
   loading the whole file.
──────────────────────────────────────────────
   pi         default                        ·
 ▾ claude                                    ×2  yolo
     opus                                    ×1  yolo
     sonnet                                  ×1  yolo
   codex      default                        ×1
 ▸ kimi       k3, kimi-for-coding            ·
 isolation    ‹ worktree per agent ›
 layout       ‹ split this tab ›
──────────────────────────────────────────────
 ←→ close/open · m models · y yolo · 0 none · ^a all · ^l none
```

- **Agents**: Herdr integrations that are installed and on PATH.
- **Models** (`m`): a filterable picker where you can check several models.
  Each checked model gets its own line, with its own count and yolo setting.
  The lists come from the agents themselves (`codex debug models`,
  `kimi provider list`, `omp models`, `pi --list-models`, plus claude's
  aliases), re-read every time and cached. You can type a name that isn't
  listed. `default` passes no `--model`.
- **yolo** (`y`): skip approval prompts. Uses claude
  `--dangerously-skip-permissions`, codex/gemini/qwen `--yolo`, kimi `--auto`,
  omp `--auto-approve`, copilot `--allow-all-tools`.
- **Isolation**: one worktree per agent, on branch `race/<id>/<agent>` from
  HEAD, under `~/.herdr/worktrees/<repo>/`. Or every agent shares the cwd,
  which is always the case outside git.
- **Layout**: split this tab, one tab per agent, or one workspace per agent.

The prompt goes out only once every agent is really ready (each gets 60 s).
While they start:

- If an agent stops at a folder-trust dialog, `y` trusts the folder and `n`
  drops that agent.
- `x` drops one lane.
- Esc removes everything created so far.

Your last setup is remembered.

### Settle

Finishing first isn't winning. Settle shows what each lane actually did:

```
 Derby  settle race 1019-143205 · 3 lanes · base a1b2c3d
──────────────────────────────────────────────────────────────────
     lane         finish           changes              branch
 [ ] claude-1     🥈 4m12s         5 files +120 −34     race/1019-143205/claude-1
 [•] codex-1      🥇 2m55s         2 files +18 −2       race/1019-143205/codex-1
 [ ] claude-2     ⚠ needs you      no changes           race/1019-143205/claude-2
──────────────────────────────────────────────────────────────────
 ↑↓ move · space pick winners · d diff · f go to pane · J judge · M merge · enter review · D drop race · esc
```

1. Pick one or more winners with Space. `d` shows a lane's diff and `f` jumps
   to its pane.
2. Enter shows what will be removed, with uncommitted work called out.
3. `y` confirms. The losers' panes, worktrees and branches are removed; the
   winners stay as they are.

`D` drops the whole race.

#### Referees

A referee is one more agent, started from settle. It doesn't race and has
no place. Pick its agent, model and yolo the same way as for a race; the
prompt is written for you, and you can edit it before it goes out.

- **Judge** (`J`): reads every lane's work (`git diff` in its worktree,
  and its conversation through `herdr agent read`) and writes
  `.derby-verdict.json` in its own worktree. The next time you open settle,
  the lanes it names are already picked, with its reason shown. They are
  only picks: you still review and confirm, and a judge can't win.
- **Merge** (`M`): gets its own worktree and branch from the race's base,
  takes the best of the lanes, runs the tests and commits. It shows up as a
  lane you can keep as the only winner. Needs worktree lanes.

Referees open the same way as the race's lanes (split, tab or workspace),
and the setup you used last is remembered. Derby itself still merges
nothing: the merge lane does it on its own branch.

![Dropping a race: every lane's pane, worktree and branch removed](docs/drop-race.gif)

Races you haven't settled are kept until you settle or drop them. If several
are open, settle starts with a list of them. Lanes you deleted by hand are
handled too: a lane with only its branch left can still win, and a lane with
nothing left is skipped.

## Requirements

- Herdr 0.9.1+, with the agent integrations you use
  (`herdr integration install <agent>`)
- Rust (`cargo`): installing builds from source
- `git` for worktrees

## Install

```bash
herdr plugin install xzedx/herdr-derby
# or from a checkout:
cargo build --release && herdr plugin link "$PWD"
```

Herdr plugins can't ship keybindings, so add them yourself in
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
description = "derby: settle or drop races"
```

## Keys

Ctrl+N / Ctrl+P work as ↓ / ↑ everywhere.

Prompt:

| Key | Action |
| --- | --- |
| Enter | send / start |
| Alt+Enter, Ctrl+J | new line |
| ↑ on an empty prompt | last prompt |
| Ctrl+U | clear |
| Tab | to the list below |
| Esc, Ctrl+C | cancel |

Broadcast targets:

| Key | Action |
| --- | --- |
| Space, x / 1-9 | toggle |
| a | toggle all |
| Ctrl+A / Ctrl+L | all / none |

Race list:

| Key | Action |
| --- | --- |
| → + / ← - | one more / one less; change isolation or layout |
| → / ← on an agent with several models | open / close |
| 0 | none of it |
| y | yolo on / off |
| m | pick models |
| d | remove that model |
| Ctrl+A / Ctrl+L | one of each / none |

Model picker:

| Key | Action |
| --- | --- |
| type | filter, or a name to use as is |
| Space | check |
| Ctrl+U | clear filter |
| Enter | use the checked ones (or the highlighted one) |
| Esc | back |

While agents start:

| Key | Action |
| --- | --- |
| x | drop that agent |
| y / n | trust / drop agents at a folder-trust dialog |
| r | check again |
| Enter | race the ready ones |
| Esc | stop and remove everything |

Settle:

| Key | Action |
| --- | --- |
| Space, x / 1-9 | pick a winner |
| d / f | diff / go to pane |
| J / M | start a judge / a merge lane |
| Enter | review |
| D | drop the race |
| y | confirm |
| Esc, q | back / close |

## Command line

```bash
herdr-derby --send [--tab w1:t1] "prompt"   # race without the popup
herdr-derby --dump [--tab w1:t1]            # print one popup frame
```

`DERBY_TRACE=<file>` logs timings. `DERBY_TEST_INPUT=<bytes>` feeds keys to
the popup, for tests.

## License

MIT
