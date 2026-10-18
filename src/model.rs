//! The agents a broadcast can go to: every recognized agent in one tab.

use crate::rpc::Client;
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize, Clone, Debug)]
pub struct Agent {
    pub pane_id: String,
    #[serde(default)]
    pub tab_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub agent_status: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Target {
    pub pane_id: String,
    pub name: Option<String>,
    pub agent: String,
    pub status: String,
    pub checked: bool,
}

impl Target {
    fn from_agent(a: Agent) -> Self {
        let status = a.agent_status.unwrap_or_else(|| "unknown".into());
        Target {
            checked: ready(&status),
            pane_id: a.pane_id,
            name: a.name,
            agent: a.agent.unwrap_or_else(|| "agent".into()),
            status,
        }
    }

    /// What the border and the list call this agent: its live name when it
    /// has one, else its kind.
    pub fn label(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.agent)
    }

    /// Herdr refuses prompts to an agent waiting on an approval or question.
    pub fn sendable(&self) -> bool {
        self.status != "blocked"
    }
}

/// Idle and done agents are waiting for input. A working agent would take
/// the prompt mid-turn (Claude interleaves it with the running task), so it
/// is opt-in; unknown is opt-in too.
pub fn ready(status: &str) -> bool {
    matches!(status, "idle" | "done")
}

pub fn list_agents(client: &Client) -> Result<Vec<Agent>, String> {
    let r = client.call("agent.list", json!({}))?;
    serde_json::from_value(r.get("agents").cloned().unwrap_or_default())
        .map_err(|e| format!("agent.list: {e}"))
}

pub fn load_targets(client: &Client, tab: &str) -> Result<Vec<Target>, String> {
    Ok(list_agents(client)?
        .into_iter()
        .filter(|a| a.tab_id.as_deref() == Some(tab))
        .map(Target::from_agent)
        .collect())
}

/// Merge a fresh listing into the current one: keep the user's checks,
/// pick up status changes, add and drop agents as they come and go. An
/// agent that became blocked is unchecked, it would be refused anyway.
pub fn refresh(current: &mut Vec<Target>, fresh: Vec<Target>) {
    let merged = fresh
        .into_iter()
        .map(|mut t| {
            if let Some(old) = current.iter().find(|o| o.pane_id == t.pane_id) {
                t.checked = old.checked && t.sendable();
            }
            t
        })
        .collect();
    *current = merged;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(pane: &str, status: &str) -> Target {
        Target::from_agent(Agent {
            pane_id: pane.into(),
            tab_id: Some("w1:t1".into()),
            name: None,
            agent: Some("claude".into()),
            agent_status: Some(status.into()),
        })
    }

    #[test]
    fn only_ready_agents_start_checked() {
        assert!(t("p1", "idle").checked);
        assert!(t("p1", "done").checked);
        assert!(!t("p1", "working").checked);
        assert!(!t("p1", "blocked").checked);
        assert!(!t("p1", "unknown").checked);
    }

    #[test]
    fn refresh_keeps_checks_and_drops_blocked() {
        let mut cur = vec![t("p1", "idle"), t("p2", "working")];
        cur[1].checked = true; // user opted in
        cur[0].checked = false; // user opted out
        refresh(&mut cur, vec![t("p1", "idle"), t("p2", "blocked"), t("p3", "done")]);
        assert_eq!(
            cur.iter().map(|t| t.checked).collect::<Vec<_>>(),
            vec![false, false, true]
        );
    }
}
