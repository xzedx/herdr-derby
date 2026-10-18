# herdr-derby

Send one prompt to every coding agent in the current [Herdr](https://herdr.dev)
tab and watch them race.

Press a key, type (or paste) a prompt, press Enter. Every agent in the tab gets
the same message at the same moment. While they work, each pane border shows a
live timer (`🏇 12.4s · claude`); when they settle it shows the finishing rank
(`🥇 2.9s · codex`, `🥈 4.0s · claude`), and one notification sums up the
race.

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

## Requirements

- Herdr 0.9.1 or newer, with the agent integrations you use installed
  (`herdr integration install <agent>`). If an agent is not listed, check
  `herdr status`: the running server may be older than the client.
- A Rust toolchain (`cargo`); `herdr plugin install` runs
  `cargo build --release`. Four small dependencies: serde, serde_json, libc,
  unicode-width. No TUI framework.

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

## Command line

```bash
herdr-derby --send [--tab w1:t1] "prompt"   # race without the popup, print the result
herdr-derby --dump [--tab w1:t1]            # render one popup frame to stdout
```

`DERBY_TRACE=<file>` appends timing traces; `DERBY_TEST_INPUT=<bytes>` feeds
key bytes to the popup before the terminal (testing).

## Roadmap

- Race mode: start fresh agents of the kinds you pick, each in its own git
  worktree, then keep the winner's branch and clean up the rest.

## License

MIT
