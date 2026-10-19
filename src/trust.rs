//! Recognize an agent's folder-trust dialog on its screen and say which
//! option is highlighted and which one trusts the folder.
//!
//! A new worktree of a trusted repo inherits the trust with claude and
//! codex, so for them this only shows up in repos never trusted; kimi
//! trusts each folder on its own and asks in every new worktree. kimi also
//! reads as idle on the dialog, so an idle agent is checked for it too.
//! The dialog is only answered after the user said yes in the popup, and
//! only once the screen shows the trust option highlighted: Claude
//! highlights "No, exit" by default.

#[derive(Debug, PartialEq)]
pub struct Dialog {
    pub options: Vec<String>,
    pub selected: usize,
    pub trust: usize,
}

const MARKERS: [char; 3] = ['❯', '›', '>'];

/// Phrases of a folder-trust question. Other dialogs that mention trust
/// (Codex's "Hooks need review") are not folder trust and stay unanswered.
const QUESTIONS: [&str; 4] = [
    "trust this folder",
    "trust this directory",
    "trust the files",
    "trust the contents",
];

/// Split a screen line into (highlighted, text column, text).
fn option_line(line: &str) -> (bool, usize, &str) {
    let indent = line.len() - line.trim_start().len();
    let rest = line.trim_start();
    match rest.chars().next() {
        Some(m) if MARKERS.contains(&m) && rest[m.len_utf8()..].starts_with(' ') => {
            let after = &rest[m.len_utf8()..];
            let text = after.trim_start();
            let col = indent + 1 + (after.len() - text.len());
            (true, col, text)
        }
        _ => (false, indent, rest),
    }
}

fn numbered(text: &str) -> bool {
    let digits = text.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && text[digits..].starts_with(". ")
}

fn trusts(option: &str) -> bool {
    let o = option.to_lowercase();
    let o = o.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == ' ');
    (o.starts_with("yes") || o.starts_with("trust")) && !o.contains("all")
}

pub fn parse(screen: &str) -> Option<Dialog> {
    let lower = screen.to_lowercase();
    if !QUESTIONS.iter().any(|q| lower.contains(q)) {
        return None;
    }
    let lines: Vec<&str> = screen.lines().collect();
    // The last highlighted line: the shell prompt above the agent can carry
    // the same glyphs, the dialog is at the bottom.
    let sel = lines.iter().rposition(|l| option_line(l).0)?;
    let (_, col, text) = option_line(lines[sel]);
    let is_numbered = numbered(text);
    let belongs = |l: &str| {
        let (_, c, t) = option_line(l);
        !t.is_empty() && c == col && (!is_numbered || numbered(t))
    };
    let mut start = sel;
    while start > 0 && belongs(lines[start - 1]) {
        start -= 1;
    }
    let mut end = sel + 1;
    while end < lines.len() && belongs(lines[end]) {
        end += 1;
    }
    let options: Vec<String> = lines[start..end]
        .iter()
        .map(|l| option_line(l).2.trim_end().to_string())
        .collect();
    let trust = options.iter().position(|o| trusts(o))?;
    Some(Dialog {
        options,
        selected: sel - start,
        trust,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE: &str = "\
/p/t/c/-/7/s/untrusted-repo ❯❯❯ claude                                        ✘ 1 main
─────────────────────────────────────────────────────────────────────────────
 Accessing workspace:
 /private/tmp/scratchpad/untrust
 ed-repo
 Quick safety check: Is this a project you created or one you trust? (Like your own code, a well-known open source project, or
 work from your team). If not, take a moment to review what's in this folder first.
 Claude Code'll be able to read, edit, and execute files here.
 Security guide

 ❯ No, exit
   Yes, I trust this folder

 Enter to confirm · Esc to cancel
";

    const CODEX: &str = "\
  Folder access
  /private/tmp/scratchpad/untr
  usted-repo
  Trust this folder? Codex can read, edit, and run files here, subject to your permission settings. Folder settings can run
  code automatically, even without a model request. Continue only if you trust these files. Your trust decision will be
  saved.

› 1. Trust and continue
  2. Back to Agent Command Center

  enter continue · esc back
";

    const HOOKS: &str = "\
  Hooks need review
  2 hooks are new or changed.
  Hooks can run outside the sandbox after you trust them.
› 1. Review hooks
  2. Trust all and continue
  3. Continue without trusting (hooks won't run)
  enter confirm · esc skip
";

    const KIMI: &str = "\
/p/t/c/-/7/s/kw4 ❯❯❯ kimi                                                  
 ──────────────────────────────────────────────────────────────────────────
  Trust this folder?
  ↑↓ navigate · Enter select · Esc exit

  /private/tmp/scratchpad/kw4

  No project integrations or instructions to activate.

  Trust is remembered for this folder, including future project config changes.
  Tool approvals follow your permission settings.

   ❯ Trust and continue
     Exit
 ──────────────────────────────────────────────────────────────────────────
";

    #[test]
    fn kimi_defaults_to_trust() {
        let d = parse(KIMI).unwrap();
        assert_eq!(d.options, vec!["Trust and continue", "Exit"]);
        assert_eq!((d.selected, d.trust), (0, 0));
    }

    #[test]
    fn claude_defaults_to_no() {
        let d = parse(CLAUDE).unwrap();
        assert_eq!(d.options, vec!["No, exit", "Yes, I trust this folder"]);
        assert_eq!((d.selected, d.trust), (0, 1));
    }

    #[test]
    fn claude_after_moving_down() {
        let moved = CLAUDE
            .replace(" ❯ No, exit", "   No, exit")
            .replace("   Yes, I trust", " ❯ Yes, I trust");
        let d = parse(&moved).unwrap();
        assert_eq!((d.selected, d.trust), (1, 1));
    }

    #[test]
    fn codex_numbered_options() {
        let d = parse(CODEX).unwrap();
        assert_eq!(
            d.options,
            vec!["1. Trust and continue", "2. Back to Agent Command Center"]
        );
        assert_eq!((d.selected, d.trust), (0, 0));
    }

    #[test]
    fn other_dialogs_are_not_trust() {
        assert_eq!(parse(HOOKS), None);
        assert_eq!(parse("❯ Try \"fix lint errors\"\n"), None);
    }
}
