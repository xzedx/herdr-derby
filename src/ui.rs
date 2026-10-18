//! Popup frame: header, prompt editor, target list, key hints.

use crate::editor::Editor;
use crate::model::Target;
use unicode_width::UnicodeWidthChar;

// Catppuccin Mocha, matching the rest of Zed's Herdr setup.
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[38;2;108;112;134m";
const GREEN: &str = "\x1b[38;2;166;227;161m";
const YELLOW: &str = "\x1b[38;2;249;226;175m";
const RED: &str = "\x1b[38;2;243;139;168m";
const BLUE: &str = "\x1b[38;2;137;180;250m";
const REVERSE: &str = "\x1b[7m";
const RESET: &str = "\x1b[0m";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Prompt,
    Targets,
}

pub struct App {
    pub editor: Editor,
    pub targets: Vec<Target>,
    pub focus: Focus,
    pub sel: usize,
    pub message: Option<String>,
}

impl App {
    pub fn checked(&self) -> Vec<&Target> {
        self.targets.iter().filter(|t| t.checked).collect()
    }
}

/// Rows the popup needs besides the editor: header, three rules, hints,
/// one per target (at least one for the "no agents" line).
pub fn chrome_rows(targets: usize) -> usize {
    5 + targets.max(1)
}

/// Cut `s` to at most `cols` display cells.
pub fn fit(s: &str, cols: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > cols {
            break;
        }
        used += w;
        out.push(c);
    }
    out
}

fn status_color(status: &str) -> &'static str {
    match status {
        "idle" | "done" => GREEN,
        "working" => YELLOW,
        "blocked" => RED,
        _ => DIM,
    }
}

/// The whole frame, plus where the terminal cursor goes (prompt focus only).
pub fn render(app: &App, cols: usize, rows: usize) -> (Vec<String>, Option<(usize, usize)>) {
    let mut lines = Vec::new();
    let inner = cols.saturating_sub(2);
    let rule = format!("{DIM}{}{RESET}", "─".repeat(cols));

    let n = app.targets.len();
    let k = app.checked().len();
    lines.push(format!(
        " {BOLD}Derby{RESET}  {DIM}{}{RESET}",
        fit(
            &format!("broadcast to {k} of {n} agent{} in this tab", if n == 1 { "" } else { "s" }),
            inner.saturating_sub(7)
        )
    ));
    lines.push(rule.clone());

    // Prompt editor, scrolled so the cursor row stays visible.
    let ed_rows = rows.saturating_sub(chrome_rows(n)).max(1);
    let ed_cols = cols.saturating_sub(3);
    let (text_rows, (cur_row, cur_col)) = app.editor.layout(ed_cols);
    let top = cur_row.saturating_sub(ed_rows - 1);
    let first_line = lines.len();
    for i in 0..ed_rows {
        let prefix = if i == 0 && top == 0 {
            format!("{BLUE}›{RESET} ")
        } else {
            "  ".to_string()
        };
        let body = match text_rows.get(top + i) {
            Some(r) => r.clone(),
            None => String::new(),
        };
        if i == 0 && app.editor.text().is_empty() {
            lines.push(format!(
                " {prefix}{DIM}{}{RESET}",
                fit("type a prompt · ↑ recalls the last one", ed_cols)
            ));
        } else {
            lines.push(format!(" {prefix}{body}"));
        }
    }
    let cursor = (app.focus == Focus::Prompt)
        .then_some((first_line + cur_row - top, 3 + cur_col));
    lines.push(rule.clone());

    if app.targets.is_empty() {
        lines.push(format!(" {DIM}no agent panes in this tab{RESET}"));
    }
    for (i, t) in app.targets.iter().enumerate() {
        let mark = if !t.sendable() {
            "[-]"
        } else if t.checked {
            "[x]"
        } else {
            "[ ]"
        };
        let name = fit(t.label(), 14);
        let kind = if t.name.is_some() { fit(&t.agent, 10) } else { String::new() };
        let plain = format!(
            " {mark} {:<2} {name:<14} {kind:<10} ",
            i + 1,
        );
        let status = if t.sendable() {
            t.status.clone()
        } else {
            "blocked · answer it first".into()
        };
        let row = format!("{}{}{}{RESET}", plain, status_color(&t.status), status);
        if app.focus == Focus::Targets && i == app.sel {
            lines.push(format!("{REVERSE}{}{RESET}", strip_for_reverse(&plain, &status, inner)));
        } else {
            lines.push(row);
        }
    }
    lines.push(rule);

    let hint = match (&app.message, app.focus) {
        (Some(m), _) => format!(" {YELLOW}{}{RESET}", fit(m, inner)),
        (None, Focus::Prompt) => format!(
            " {DIM}{}{RESET}",
            fit("enter send · alt+enter newline · tab targets · esc cancel", inner)
        ),
        (None, Focus::Targets) => format!(
            " {DIM}{}{RESET}",
            fit("space toggle · a all · tab/esc prompt · enter send", inner)
        ),
    };
    lines.push(hint);
    (lines, cursor)
}

fn strip_for_reverse(plain: &str, status: &str, cols: usize) -> String {
    fit(&format!("{plain}{status}"), cols + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(text: &str) -> App {
        let mut editor = Editor::default();
        editor.set(text);
        App {
            editor,
            targets: vec![Target {
                pane_id: "w1:p1".into(),
                name: Some("c1".into()),
                agent: "claude".into(),
                status: "idle".into(),
                checked: true,
            }],
            focus: Focus::Prompt,
            sel: 0,
            message: None,
        }
    }

    #[test]
    fn frame_fills_the_popup_and_places_cursor() {
        let (lines, cursor) = render(&app("hi"), 40, 12);
        assert_eq!(lines.len(), 12);
        // header, rule, then the first editor row: " › hi", cursor after "hi".
        assert_eq!(cursor, Some((2, 5)));
    }

    #[test]
    fn editor_scrolls_to_cursor() {
        let text = (1..=20).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
        let (lines, cursor) = render(&app(&text), 40, 12);
        assert_eq!(lines.len(), 12);
        let (row, _) = cursor.unwrap();
        assert!(lines[row].contains("20"));
    }

    #[test]
    fn fit_counts_wide_chars() {
        assert_eq!(fit("赛马ab", 5), "赛马a");
        assert_eq!(fit("赛马", 3), "赛");
    }
}
