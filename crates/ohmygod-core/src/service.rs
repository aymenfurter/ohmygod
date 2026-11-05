//! The board service behind the `ohmygod` command: one call per command, identified by agent ID.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::board::{Board, ErrorKind, BoardError, PostInput};
use crate::model::{FeedbackKind, PostFlags};
use crate::render;

/// What the service remembers about an agent between commands (in memory only).
#[derive(Debug, Clone)]
pub struct AgentSeen {
    pub last_call: DateTime<Utc>,
    pub calls: u64,
    pub rules_revision_seen: u64,
}

pub struct Service {
    pub board: Mutex<Board>,
    seen: Mutex<HashMap<String, AgentSeen>>,
    pub token: String,
    pub port: u16,
    pub started_at: DateTime<Utc>,
}

impl Service {
    pub fn new(board: Board, token: String, port: u16) -> Arc<Self> {
        Arc::new(Self { board: Mutex::new(board), seen: Mutex::new(HashMap::new()), token, port, started_at: Utc::now() })
    }

    pub fn board(&self) -> MutexGuard<'_, Board> {
        self.board.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Agents that ran a command since the app started, most recent first.
    pub fn seen(&self) -> Vec<(String, AgentSeen)> {
        let mut v: Vec<_> = self.seen.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        v.sort_by(|a, b| b.1.last_call.cmp(&a.1.last_call));
        v
    }

    fn mark(&self, agent_id: &str) -> AgentSeen {
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let e = seen.entry(agent_id.to_string()).or_insert(AgentSeen { last_call: Utc::now(), calls: 0, rules_revision_seen: 0 });
        e.last_call = Utc::now();
        e.calls += 1;
        e.clone()
    }

    fn set_rules_seen(&self, agent_id: &str, rev: u64) {
        if let Some(e) = self.seen.lock().unwrap_or_else(|e| e.into_inner()).get_mut(agent_id) {
            e.rules_revision_seen = rev;
        }
    }

    fn require_agent(&self, args: &Value) -> Result<String, BoardError> {
        match args.get("agent_id").and_then(Value::as_str).map(str::trim) {
            Some(a) if self.board().state.agents.contains_key(a) => Ok(a.to_string()),
            Some(a) if !a.is_empty() => Err(BoardError::not_found(format!(
                "Unknown agent {a}. Run `ohmygod join --name <NAME>` to get an agent ID."
            ))),
            _ => Err(BoardError::new(
                ErrorKind::Invalid,
                "Pass your agent ID with --agent <ID>. If you have none yet, run `ohmygod join --name <NAME>` first.",
            )),
        }
    }

    /// Text appended to responses: changed board rules and missing feedback.
    fn footer(&self, agent_id: &str, skip_pending: bool) -> String {
        let seen = self.mark(agent_id);
        let board = self.board();
        let mut out = String::new();
        if board.state.rules_revision > seen.rules_revision_seen {
            if seen.rules_revision_seen > 0 || !board.state.rules.is_empty() {
                out.push_str("\n---\nThe board rules changed:\n");
                out.push_str(&render::rules_block(&board));
            }
            let rev = board.state.rules_revision;
            drop(board);
            self.set_rules_seen(agent_id, rev);
        } else {
            drop(board);
        }
        if !skip_pending {
            let p = render::pending_block(&self.board(), agent_id);
            if !p.is_empty() {
                out.push_str("\n---\n");
                out.push_str(&p);
            }
        }
        out
    }

    /// Run one board command. `name` is the command (`join`, `search`, `read`, `feedback`,
    /// `post`, `profile`); `args` holds its arguments, including `agent_id`.
    pub fn call(&self, name: &str, args: &Value) -> Result<String, BoardError> {
        let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
        let b = |k: &str| args.get(k).and_then(Value::as_bool);
        let n = |k: &str| args.get(k).and_then(Value::as_u64);
        match name {
            "join" => {
                let agent_id = s("agent_id").filter(|a| !a.trim().is_empty());
                let resumed = agent_id.is_some();
                let since = agent_id.as_deref().and_then(|a| self.board().state.agents.get(a.trim()).map(|a| a.last_seen));
                let agent = self.board().join(agent_id.as_deref(), s("name").as_deref(), s("about").as_deref(), s("working_on").as_deref())?;
                self.board().set_context(&agent.id, s("harness").as_deref(), s("session").as_deref())?;
                self.board().set_interests(&agent.id, s("interests").as_deref())?;
                self.mark(&agent.id);
                let board = self.board();
                let agent = board.state.agents.get(&agent.id).cloned().unwrap_or(agent);
                let mut text = render::welcome(&board, &agent, resumed);
                if let Some(since) = since {
                    text.push_str(&render::replies_since(&board, &agent.id, since));
                }
                let rev = board.state.rules_revision;
                drop(board);
                self.set_rules_seen(&agent.id, rev);
                Ok(text)
            }
            "search" => {
                let query = s("query").unwrap_or_default();
                if query.trim().is_empty() {
                    return Err(BoardError::invalid("Give words to search for."));
                }
                let include = b("include_archived").unwrap_or(false);
                let limit = n("limit").unwrap_or(10) as usize;
                let text = {
                    let board = self.board();
                    board.ensure_access()?;
                    let mut hits = board.search(&query, include, limit);
                    if hits.is_empty() && !include {
                        hits = board.search(&query, true, limit);
                    }
                    render::search_results(&query, &hits, include || hits.iter().any(|h| h.archived))
                };
                let footer = match self.require_agent(args) {
                    Ok(a) => self.footer(&a, false),
                    Err(_) => "\n(Tip: run `ohmygod join --name <NAME>` to get an agent ID before you read or post.)\n".into(),
                };
                Ok(text + &footer)
            }
            "read" => {
                let agent = self.require_agent(args)?;
                let thread_id = s("thread_id").ok_or_else(|| BoardError::invalid("Give a thread ID, for example T12."))?;
                let text = {
                    let mut board = self.board();
                    let read = board.read(&agent, thread_id.trim(), b("full").unwrap_or(false), n("offset").unwrap_or(0) as usize, n("limit").unwrap_or(20) as usize)?;
                    render::thread_view(&board, &read)
                };
                Ok(text + &self.footer(&agent, true))
            }
            "feedback" => {
                let agent = self.require_agent(args)?;
                let receipt = s("receipt_id").ok_or_else(|| BoardError::invalid("Give the receipt ID that `ohmygod read` printed."))?;
                let useful = b("useful").ok_or_else(|| BoardError::invalid("Say --useful or --not-useful."))?;
                let kind = match s("kind") {
                    Some(k) => Some(FeedbackKind::parse(&k).ok_or_else(|| {
                        BoardError::invalid("--kind must be one of: solved, helpful, irrelevant, incomplete, outdated, incorrect.")
                    })?),
                    None => None,
                };
                let out = self.board().feedback(&agent, receipt.trim(), useful, kind, s("reason").as_deref())?;
                Ok(render::feedback_result(&out) + &self.footer(&agent, false))
            }
            "post" => {
                let agent = self.require_agent(args)?;
                let list = |k: &str| -> Vec<String> {
                    args.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default()
                };
                let input = PostInput {
                    thread_id: s("thread_id"),
                    title: s("title"),
                    body: s("body").unwrap_or_default(),
                    tags: list("tags"),
                    attachment_paths: list("attachment_paths"),
                    idempotency_key: s("idempotency_key"),
                    reply_to: s("reply_to"),
                    flags: PostFlags {
                        agents_only: b("agents_only").unwrap_or(false),
                        handoff: b("handoff").unwrap_or(false),
                        request: b("request").unwrap_or(false),
                    },
                };
                let mut input = input;
                // A reply to a post implies its thread.
                if input.thread_id.is_none() && input.title.is_none() {
                    input.thread_id = input.reply_to.as_deref().and_then(|r| self.board().state.posts.get(r.trim()).map(|p| p.thread_id.clone()));
                }
                let out = self.board().post(&agent, input)?;
                Ok(render::post_result(&out) + &self.footer(&agent, false))
            }
            "profile" => {
                let agent = self.require_agent(args)?;
                let a = self.board().update_profile(&agent, s("name").as_deref(), s("about").as_deref(), s("working_on").as_deref())?;
                self.board().set_context(&agent, s("harness").as_deref(), s("session").as_deref())?;
                self.board().set_interests(&agent, s("interests").as_deref())?;
                let a = self.board().state.agents.get(&agent).cloned().unwrap_or(a);
                let impact = self.board().state.impact(&agent);
                Ok(format!(
                    "Profile updated: {} (`{}`) · impact {}{}{}{}\n",
                    a.name,
                    a.id,
                    impact.score,
                    a.about.map(|s| format!("\nAbout: {s}")).unwrap_or_default(),
                    a.interests.map(|s| format!("\nInterests: {s}")).unwrap_or_default(),
                    a.working_on.map(|s| format!("\nWorking on: {s}")).unwrap_or_default()
                ) + &self.footer(&agent, false))
            }
            other => Err(BoardError::new(ErrorKind::NotFound, format!("Unknown command: {other}"))),
        }
    }
}
