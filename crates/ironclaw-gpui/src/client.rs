//! Blocking REST client for ironclawd, polled on a background thread.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::json;

use crate::models::{AgentSummary, Capability, FarmTask, Health};

/// Latest known state of the daemon, shared between the poll thread and the UI.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub connected: bool,
    #[allow(dead_code)]
    pub health: Health,
    pub agents: Vec<AgentSummary>,
    pub tasks: Vec<FarmTask>,
    pub error: Option<String>,
    pub polls: u64,
}

pub type Shared = Arc<Mutex<Snapshot>>;

fn auth(req: ureq::Request) -> ureq::Request {
    match std::env::var("IRONCLAW_TOKEN") {
        Ok(token) if !token.is_empty() => req.set("authorization", &format!("Bearer {token}")),
        _ => req,
    }
}

fn get_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<T> {
    let response = auth(ureq::get(url))
        .timeout(Duration::from_secs(4))
        .call()
        .map_err(|err| anyhow!("{err}"))?;
    Ok(response.into_json::<T>()?)
}

fn poll_once(base_url: &str) -> Result<Snapshot> {
    let health: Health = get_json(&format!("{base_url}/api/health"))?;
    let agents: Vec<AgentSummary> = get_json(&format!("{base_url}/api/farm/agents"))?;
    let tasks: Vec<FarmTask> = get_json(&format!("{base_url}/api/farm/tasks"))?;
    Ok(Snapshot {
        connected: true,
        health,
        agents,
        tasks,
        error: None,
        polls: 0,
    })
}

/// Start polling `base_url` every two seconds, writing results into `shared`.
pub fn spawn_poller(base_url: String, shared: Shared) {
    std::thread::spawn(move || loop {
        match poll_once(&base_url) {
            Ok(mut snapshot) => {
                let mut guard = shared.lock().unwrap();
                snapshot.polls = guard.polls + 1;
                *guard = snapshot;
            }
            Err(err) => {
                let mut guard = shared.lock().unwrap();
                guard.connected = false;
                guard.error = Some(err.to_string());
                guard.polls += 1;
            }
        }
        std::thread::sleep(Duration::from_millis(2500));
    });
}

/// Fetch the a2a skill capabilities authorized for `agent_id`.
pub fn fetch_capabilities(base_url: &str, agent_id: &str) -> Result<Vec<Capability>> {
    let url = format!("{base_url}/api/farm/agents/{agent_id}/capabilities");
    let all: Vec<Capability> = get_json(&url)?;
    Ok(all
        .into_iter()
        .filter(|capability| capability.kind == "a2a_skill")
        .collect())
}

/// Parsed `agent://<assignee>/<skill>` capability URI.
pub fn parse_capability_uri(uri: &str) -> Option<(String, String)> {
    let rest = uri.strip_prefix("agent://")?;
    let (assignee, skill) = rest.split_once('/')?;
    if assignee.is_empty() || skill.is_empty() {
        return None;
    }
    Some((assignee.to_string(), skill.to_string()))
}

/// Create an A2A task via `POST /api/farm/tasks`.
pub fn create_task(
    base_url: &str,
    requester: &str,
    assignee: &str,
    skill: &str,
    request: &str,
) -> Result<FarmTask> {
    let url = format!("{base_url}/api/farm/tasks");
    let body = json!({
        "requester": requester,
        "assignee": assignee,
        "skill": skill,
        "input": { "request": request, "source": "gpui-workspace" },
    });
    let response = auth(ureq::post(&url))
        .timeout(Duration::from_secs(6))
        .send_json(body)
        .map_err(|err| anyhow!("{err}"))?;
    Ok(response.into_json::<FarmTask>()?)
}
