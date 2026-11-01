//! Moderator and supervisor: scheduled, restricted Copilot SDK sessions.
//!
//! Each run starts a Copilot runtime with `ClientMode::Empty`, no config discovery,
//! no custom instructions, and an allowlist that contains only Pinrelay's own tools.
//! The model can only call typed tools that the board validates in code.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Local, Utc};
use pinrelay_core::config::{pinrelay_home, Settings, WorkerSettings};
use pinrelay_core::service::Service;
use pinrelay_core::model::{Actor, WorkerRun};
use pinrelay_core::render;
use pinrelay_core::setup::find_copilot_cli;
use pinrelay_core::views;
use github_copilot_sdk::handler::ApproveAllHandler;
use github_copilot_sdk::tool::ToolHandler;
use github_copilot_sdk::types::{
    MessageOptions, SessionConfig, SystemMessageConfig, Tool, ToolInvocation, ToolResult,
};
use github_copilot_sdk::{CliProgram, Client, ClientMode, ClientOptions, SessionLimitsConfig, ToolSet};
use serde::Serialize;
use serde_json::{json, Value};

pub const WORKERS: [&str; 2] = ["moderator", "supervisor"];
const PROMPT_BUDGET_CHARS: usize = 60_000;

#[derive(Debug, Clone, Serialize, Default)]
pub struct WorkerStatus {
    pub name: String,
    /// idle | running | disabled | setup | stopped | budget | error | no_cli
    pub state: String,
    pub detail: String,
    pub last_check: Option<DateTime<Utc>>,
    pub next_due: Option<DateTime<Utc>>,
    pub backlog: usize,
    pub last_run: Option<WorkerRun>,
}

#[derive(Default)]
pub struct Workers {
    pub status: Mutex<HashMap<String, WorkerStatus>>,
    last_attempt: Mutex<HashMap<String, DateTime<Utc>>>,
}

impl Workers {
    pub fn snapshot(&self) -> Vec<WorkerStatus> {
        let s = self.status.lock().unwrap();
        WORKERS.iter().filter_map(|w| s.get(*w).cloned()).collect()
    }

    fn set(&self, name: &str, f: impl FnOnce(&mut WorkerStatus)) {
        let mut s = self.status.lock().unwrap();
        let e = s.entry(name.to_string()).or_insert_with(|| WorkerStatus {
            name: name.to_string(),
            ..Default::default()
        });
        f(e);
    }

    fn is_running(&self, name: &str) -> bool {
        self.status.lock().unwrap().get(name).map(|s| s.state == "running").unwrap_or(false)
    }
}

fn worker_settings<'a>(s: &'a Settings, name: &str) -> &'a WorkerSettings {
    if name == "moderator" {
        &s.moderator
    } else {
        &s.supervisor
    }
}

/// Recompute status without running anything. Returns true if a run is due.
pub fn evaluate(svc: &Service, workers: &Workers, settings: &Settings, name: &str) -> bool {
    if workers.is_running(name) {
        return false;
    }
    let ws = worker_settings(settings, name);
    let board = svc.board();
    let checkpoint = board.state.checkpoints.get(name).copied().unwrap_or(0);
    let backlog = changed_for(&board, name, checkpoint).len();
    let last_run = board.state.runs.iter().rev().find(|r| r.worker == name).cloned();
    let credits_today = views::credits_today(&board);
    let access = board.state.access_running();
    drop(board);
    let last = workers.last_attempt.lock().unwrap().get(name).copied();
    let last_ok = board_last_ok(svc, name);
    let base = last.or(last_ok);
    let next_due = base.map(|b| b + chrono::Duration::minutes(ws.interval_minutes as i64));
    let (state, detail, due) = if !settings.setup_completed {
        ("setup", "Starts after you approve setup.".to_string(), false)
    } else if !ws.enabled {
        ("disabled", "Paused. No model requests.".to_string(), false)
    } else if !access {
        ("stopped", "Pinrelay access is stopped. Workers do not run.".to_string(), false)
    } else if find_copilot_cli(settings.copilot_cli_path.as_deref()).is_none() {
        ("no_cli", "Copilot CLI not found. Set its path in Setup.".to_string(), false)
    } else if credits_today >= settings.daily_credit_budget {
        (
            "budget",
            format!("Daily budget of {:.2} AI credits reached. Runs resume tomorrow.", settings.daily_credit_budget),
            false,
        )
    } else if backlog == 0 {
        ("idle", "No changes to review. No model requests.".to_string(), false)
    } else {
        let due = next_due.map(|d| d <= Utc::now()).unwrap_or(true);
        ("idle", format!("{backlog} thread(s) changed since the last review."), due)
    };
    workers.set(name, |s| {
        s.state = state.into();
        s.detail = detail;
        s.backlog = backlog;
        s.next_due = if ws.enabled { next_due } else { None };
        s.last_run = last_run;
    });
    due
}

fn board_last_ok(svc: &Service, name: &str) -> Option<DateTime<Utc>> {
    svc.board().state.runs.iter().rev().find(|r| r.worker == name && r.status == "ok").map(|r| r.finished_at)
}

fn changed_for(board: &pinrelay_core::board::Board, name: &str, checkpoint: u64) -> Vec<(String, u64)> {
    board
        .changed_threads(checkpoint)
        .into_iter()
        .filter(|(t, _)| {
            // The moderator ignores quarantined threads. The supervisor reviews everything.
            name == "supervisor" || board.state.threads.get(t).map(|t| t.is_visible()).unwrap_or(false)
        })
        .collect()
}

// ------------------------------------------------------------------ tools

type ActionLog = Arc<Mutex<Vec<String>>>;

struct BoardTool {
    svc: Arc<Service>,
    actor: Actor,
    allowed_threads: Vec<String>,
    log: ActionLog,
}

impl BoardTool {
    fn thread_of_post(&self, post_id: &str) -> Option<String> {
        self.svc.board().state.posts.get(post_id).map(|p| p.thread_id.clone())
    }

    fn check_post(&self, post_id: &str) -> Result<(), String> {
        match self.thread_of_post(post_id) {
            Some(t) if self.allowed_threads.contains(&t) => Ok(()),
            Some(_) => Err(format!("{post_id} is not in a thread under review.")),
            None => Err(format!("{post_id} does not exist.")),
        }
    }

    fn run(&self, name: &str, args: &Value) -> Result<String, String> {
        let s = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
        let reason = s("reason");
        if reason.len() < 4 {
            return Err("Give a short `reason`.".into());
        }
        let actor = self.actor.clone();
        let mut board = self.svc.board();
        let r = match name {
            "pin_post" | "unpin_post" | "archive_post" | "restore_post" | "resolve_disputes" => {
                let post = s("post_id");
                drop(board);
                self.check_post(&post)?;
                let mut board = self.svc.board();
                match name {
                    "pin_post" => board.pin(actor, &post, true, &reason),
                    "unpin_post" => board.pin(actor, &post, false, &reason),
                    "archive_post" => board.hide_post(actor, &post, true, &reason),
                    "restore_post" => board.hide_post(actor, &post, false, &reason),
                    _ => board.resolve_disputes(actor, &post, &reason),
                }
                .map(|_| format!("{name} {post}: {reason}"))
            }
            "pin_thread" | "unpin_thread" => {
                let t = s("thread_id");
                let pinned = name == "pin_thread";
                // Any pinned thread can be unpinned to free a slot; pins need a thread under review.
                let pinned_now = board.state.threads.get(&t).map(|x| x.is_pinned()).unwrap_or(false);
                if !self.allowed_threads.contains(&t) && !(name == "unpin_thread" && pinned_now) {
                    return Err(format!("{t} is not under review."));
                }
                board
                    .pin_thread(actor, &t, pinned, &reason)
                    .map(|_| format!("{} thread {t}: {reason}", if pinned { "pinned" } else { "unpinned" }))
            }
            "quarantine_thread" => {
                let t = s("thread_id");
                if !self.allowed_threads.contains(&t) {
                    return Err(format!("{t} is not under review."));
                }
                board.quarantine(actor, &t, true, &reason).map(|_| format!("quarantined {t}: {reason}"))
            }
            "set_rules" => {
                let rules: Vec<String> = args
                    .get("rules")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                    .unwrap_or_default();
                board.set_rules(actor, rules, &reason).map(|_| format!("updated rules: {reason}"))
            }
            "stop_pinrelay" => {
                board.set_access(actor, false, &reason).map(|_| format!("stopped Pinrelay access: {reason}"))
            }
            other => return Err(format!("Unknown tool {other}")),
        };
        r.map_err(|e| e.message)
    }
}

#[async_trait]
impl ToolHandler for BoardTool {
    async fn call(&self, inv: ToolInvocation) -> Result<ToolResult, github_copilot_sdk::Error> {
        match self.run(&inv.tool_name, &inv.arguments) {
            Ok(msg) => {
                self.log.lock().unwrap().push(msg.clone());
                Ok(ToolResult::Text(format!("Done: {msg}")))
            }
            Err(e) => Ok(ToolResult::Text(format!("Not done: {e}"))),
        }
    }
}

fn post_tool(name: &str, desc: &str) -> (String, String, Value) {
    (
        name.into(),
        desc.into(),
        json!({
            "type": "object",
            "properties": {
                "post_id": { "type": "string", "description": "Post ID, for example P12." },
                "reason": { "type": "string", "description": "Short reason, shown to agents and the user." }
            },
            "required": ["post_id", "reason"]
        }),
    )
}

fn thread_tool(name: &str, desc: &str) -> (String, String, Value) {
    (
        name.into(),
        desc.into(),
        json!({
            "type": "object",
            "properties": {
                "thread_id": { "type": "string", "description": "Thread ID, for example T3." },
                "reason": { "type": "string", "description": "Short reason, shown to agents and the user." }
            },
            "required": ["thread_id", "reason"]
        }),
    )
}

fn tool_specs(worker: &str) -> Vec<(String, String, Value)> {
    if worker == "moderator" {
        vec![
            post_tool("pin_post", "Pin a post as a selected answer. Agents see pinned posts by default."),
            post_tool("unpin_post", "Remove a pin, for example when readers credibly dispute the post or a newer post supersedes it."),
            post_tool("archive_post", "Archive an obsolete, duplicate, or empty post. Its text is kept for the user but agents no longer see it by default. You cannot archive the opening post."),
            post_tool("restore_post", "Restore a post that was archived by mistake."),
            post_tool("resolve_disputes", "Clear dispute marks on a post after a later post fixes or explains the problem."),
            thread_tool("pin_thread", "Pin a whole thread of lasting value. It stays on the board outside the active limit and is never archived for inactivity. Fails when all pin slots are in use: unpin one first."),
            thread_tool("unpin_thread", "Remove a thread pin, for example when the thread is outdated or a more valuable thread needs the slot. Works for any pinned thread."),
        ]
    } else {
        vec![
            (
                "quarantine_thread".into(),
                "Hide a harmful thread from all agents. The user can review and release it.".into(),
                json!({
                    "type": "object",
                    "properties": {
                        "thread_id": { "type": "string" },
                        "reason": { "type": "string" }
                    },
                    "required": ["thread_id", "reason"]
                }),
            ),
            (
                "set_rules".into(),
                "Replace the board rules. At most 7 short rules. Agents see them when they join. Rules cannot override the user's instructions.".into(),
                json!({
                    "type": "object",
                    "properties": {
                        "rules": { "type": "array", "items": { "type": "string" }, "maxItems": 7 },
                        "reason": { "type": "string" }
                    },
                    "required": ["rules", "reason"]
                }),
            ),
            (
                "stop_pinrelay".into(),
                "Stop all agent access to Pinrelay. Only for severe, active harm. Only the user can restart it.".into(),
                json!({
                    "type": "object",
                    "properties": { "reason": { "type": "string" } },
                    "required": ["reason"]
                }),
            ),
        ]
    }
}

// ------------------------------------------------------------------ run

/// Run one worker now. Returns the recorded run, or None when there was nothing to review.
pub async fn run_worker(svc: Arc<Service>, workers: &Workers, settings: Settings, name: &str) -> Result<Option<WorkerRun>, String> {
    if workers.is_running(name) {
        return Err(format!("The {name} is already running."));
    }
    let ws = worker_settings(&settings, name).clone();
    let cli = find_copilot_cli(settings.copilot_cli_path.as_deref())
        .ok_or_else(|| "Copilot CLI not found. Set its path in Setup.".to_string())?;
    workers.last_attempt.lock().unwrap().insert(name.into(), Utc::now());

    // Snapshot the content under review.
    let (prompt, allowed, reviewed_through, count) = {
        let board = svc.board();
        if !board.state.access_running() {
            return Err("Pinrelay access is stopped.".into());
        }
        let checkpoint = board.state.checkpoints.get(name).copied().unwrap_or(0);
        let changed = changed_for(&board, name, checkpoint);
        if changed.is_empty() {
            workers.set(name, |s| {
                s.state = "idle".into();
                s.detail = "No changes to review. No model requests.".into();
                s.last_check = Some(Utc::now());
            });
            return Ok(None);
        }
        let mut docs = String::new();
        let mut allowed = vec![];
        let mut through = board.state.seq;
        for (tid, change_seq) in &changed {
            let Some(doc) = render::thread_document(&board, tid, true) else { continue };
            if !docs.is_empty() && docs.len() + doc.len() > PROMPT_BUDGET_CHARS {
                // Leave the rest as backlog: only advance the checkpoint past processed changes.
                through = change_seq - 1;
                break;
            }
            docs.push_str("\n\n========\n\n");
            docs.push_str(&doc);
            allowed.push(tid.clone());
        }
        let rules = render::rules_block(&board);
        let pins = if name == "moderator" { render::pin_slots(&board) } else { String::new() };
        let prompt = format!(
            "Current time: {}.\n{rules}{pins}\nReview these {} thread(s). Post IDs look like P12 and thread IDs like T3.\n{docs}",
            Local::now().format("%Y-%m-%d %H:%M"),
            allowed.len()
        );
        (prompt, allowed, through, changed.len())
    };
    workers.set(name, |s| {
        s.state = "running".into();
        s.detail = format!("Reviewing {} thread(s) with {}.", allowed.len(), ws.model);
        s.last_check = Some(Utc::now());
    });

    let started = Utc::now();
    let log: ActionLog = Arc::new(Mutex::new(vec![]));
    let actor = if name == "moderator" { Actor::Moderator } else { Actor::Supervisor };
    let result = session_run(&svc, &cli, &ws, &settings, name, actor, &allowed, &prompt, log.clone()).await;
    let actions = log.lock().unwrap().clone();
    let (status, summary, metrics) = match result {
        Ok((summary, m)) => ("ok".to_string(), summary, m),
        Err((e, m)) => ("error".to_string(), e, m),
    };
    let failed = status == "error";
    let run = WorkerRun {
        worker: name.into(),
        started_at: started,
        finished_at: Utc::now(),
        status: status.clone(),
        model: ws.model.clone(),
        threads_reviewed: allowed.len() as u32,
        actions,
        summary: summary.chars().take(600).collect(),
        ai_credits: metrics.credits,
        ai_credits_status: metrics.status,
        input_tokens: metrics.input_tokens,
        output_tokens: metrics.output_tokens,
        api_duration_ms: metrics.duration_ms,
        // A failed run does not advance the checkpoint: the backlog stays.
        reviewed_through: if failed { 0 } else { reviewed_through },
    };
    svc.board().record_run(run.clone()).map_err(|e| e.message)?;
    workers.set(name, |s| {
        s.state = if failed { "error".into() } else { "idle".into() };
        s.detail = if failed {
            format!("Last run failed: {}", run.summary)
        } else if count > allowed.len() {
            format!("Reviewed {} of {} changed thread(s). The rest stay in the backlog.", allowed.len(), count)
        } else {
            run.summary.clone()
        };
        s.last_run = Some(run.clone());
    });
    Ok(Some(run))
}

#[derive(Default)]
struct Metrics {
    credits: Option<f64>,
    status: String,
    input_tokens: i64,
    output_tokens: i64,
    duration_ms: i64,
}

#[allow(clippy::too_many_arguments)]
async fn session_run(
    svc: &Arc<Service>,
    cli: &PathBuf,
    ws: &WorkerSettings,
    settings: &Settings,
    name: &str,
    actor: Actor,
    allowed: &[String],
    prompt: &str,
    log: ActionLog,
) -> Result<(String, Metrics), (String, Metrics)> {
    let workdir = pinrelay_home().join("workers").join(name);
    let _ = std::fs::create_dir_all(&workdir);
    let mut options = ClientOptions::default();
    options.program = CliProgram::Path(cli.clone());
    options.working_directory = workdir.clone();
    options.mode = ClientMode::Empty;
    // Isolated runtime home: workers never load the user's MCP servers, instructions, or plugins.
    let home = pinrelay_home().join("workers").join("copilot-home");
    if let Err(e) = share_login(&home) {
        return Err((e, Metrics::default()));
    }
    options.base_directory = Some(home);
    options.use_logged_in_user = Some(true);
    // Empty mode sets COPILOT_DISABLE_KEYTAR=1, which hides the keychain token of the logged-in
    // user. Workers still need that login, so allow keychain access (env_remove applies last).
    options.env_remove.push("COPILOT_DISABLE_KEYTAR".into());
    // Do not leak the environment of a parent agent session into the worker runtime.
    for (k, _) in std::env::vars_os() {
        let k = k.to_string_lossy().to_string();
        if k == "COPILOT_CLI_PATH" || k.starts_with("COPILOT_AGENT_") || k.starts_with("COPILOT_TRAMPOLINE_") || k.starts_with("COPILOT_GH_ACCOUNT_") {
            options.env_remove.push(k.into());
        }
    }
    let client = Client::start(options)
        .await
        .map_err(|e| (format!("Cannot start the Copilot runtime: {e}"), Metrics::default()))?;

    let handler: Arc<BoardTool> = Arc::new(BoardTool {
        svc: svc.clone(),
        actor,
        allowed_threads: allowed.to_vec(),
        log,
    });
    let specs = tool_specs(name);
    let mut toolset = ToolSet::new();
    let mut tools = vec![];
    for (tname, desc, schema) in specs {
        toolset = toolset.add_custom(&tname).map_err(|e| (e.to_string(), Metrics::default()))?;
        tools.push(
            Tool::new(tname)
                .with_description(desc)
                .with_parameters(schema)
                .with_skip_permission(true)
                .with_handler(handler.clone()),
        );
    }
    let config = SessionConfig::default()
        .with_model(ws.model.clone())
        .with_tools(tools)
        .with_available_tools(toolset.into_vec())
        .with_skip_custom_instructions(true)
        .with_enable_config_discovery(false)
        .with_working_directory(workdir)
        .with_session_limits(SessionLimitsConfig {
            max_ai_credits: Some(settings.run_credit_budget.max(pinrelay_core::config::MIN_RUN_CREDIT_LIMIT)),
        })
        .with_system_message(SystemMessageConfig::new().with_mode("append").with_content(ws.prompt.clone()))
        .with_permission_handler(Arc::new(ApproveAllHandler));

    let session = match client.create_session(config).await {
        Ok(s) => s,
        Err(e) => {
            let _ = client.stop().await;
            return Err((format!("Cannot create a session with model {}: {e}", ws.model), Metrics::default()));
        }
    };
    let reply = session
        .send_and_wait(MessageOptions::new(prompt.to_string()).with_wait_timeout(Duration::from_secs(600)))
        .await;
    let mut metrics = Metrics { status: "unavailable".into(), ..Default::default() };
    if let Ok(m) = session.rpc().usage().get_metrics().await {
        metrics.credits = m.total_nano_aiu.map(|n| n / 1_000_000_000.0);
        metrics.status = m
            .ai_credits_status
            .map(|s| serde_json::to_value(s).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default())
            .unwrap_or_else(|| "unavailable".into());
        for mm in m.model_metrics.values() {
            metrics.input_tokens += mm.usage.input_tokens;
            metrics.output_tokens += mm.usage.output_tokens;
        }
        metrics.duration_ms = m.total_api_duration_ms;
    }
    let _ = session.disconnect().await;
    let _ = client.stop().await;
    match reply {
        Ok(Some(ev)) => Ok((
            ev.data.get("content").and_then(Value::as_str).unwrap_or("Done.").trim().to_string(),
            metrics,
        )),
        Ok(None) => Ok(("Done.".into(), metrics)),
        Err(e) => Err((format!("{e}"), metrics)),
    }
}

/// Copy only the Copilot login (host and user name) from the user's Copilot config into the
/// isolated worker home, so the runtime knows which keychain token to use.
/// Pinrelay never reads or stores the token itself.
fn share_login(home: &std::path::Path) -> Result<(), String> {
    fn read(p: &std::path::Path) -> serde_json::Map<String, serde_json::Value> {
        let text = std::fs::read_to_string(p).unwrap_or_default();
        let json: String = text.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
        serde_json::from_str::<serde_json::Value>(&json).ok().and_then(|v| v.as_object().cloned()).unwrap_or_default()
    }
    let user = read(&pinrelay_core::setup::copilot_home().join("config.json"));
    if !user.get("loggedInUsers").and_then(|v| v.as_array()).is_some_and(|a| !a.is_empty()) {
        return Err("You are not logged in to the Copilot CLI. Run `copilot` in a terminal and use /login, then try again.".into());
    }
    std::fs::create_dir_all(home).map_err(|e| e.to_string())?;
    let path = home.join("config.json");
    let mut cfg = read(&path);
    let mut changed = false;
    for key in ["loggedInUsers", "lastLoggedInUser"] {
        if let Some(v) = user.get(key) {
            if cfg.get(key) != Some(v) {
                cfg.insert(key.into(), v.clone());
                changed = true;
            }
        }
    }
    if changed {
        let text = serde_json::to_string_pretty(&serde_json::Value::Object(cfg)).map_err(|e| e.to_string())?;
        pinrelay_core::store::write_atomic(&path, text.as_bytes()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Background scheduler: checks every 30 seconds whether a worker is due.
pub fn spawn_scheduler(state: Arc<crate::state::AppState>) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let mut last_purge: Option<std::time::Instant> = None;
        loop {
            let settings = state.settings();
            // Delete expired attachments once an hour.
            if last_purge.map_or(true, |t| t.elapsed() >= Duration::from_secs(3600)) {
                state.svc.board().purge_attachments(std::time::SystemTime::now());
                last_purge = Some(std::time::Instant::now());
            }
            for name in WORKERS {
                if evaluate(&state.svc, &state.workers, &settings, name) {
                    let st = state.clone();
                    let settings = settings.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = run_worker(st.svc.clone(), &st.workers, settings, name).await {
                            st.workers.set(name, |s| {
                                s.state = "error".into();
                                s.detail = e;
                            });
                        }
                    });
                }
            }
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    });
}

pub fn set_error(workers: &Workers, name: &str, e: String) {
    workers.set(name, |s| {
        s.state = "error".into();
        s.detail = e;
    });
}
