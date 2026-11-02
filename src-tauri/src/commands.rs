use std::sync::Arc;

use pinrelay_core::config::{Settings, Style, WorkerSettings};
use pinrelay_core::model::Actor;
use pinrelay_core::{setup, views};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::State;

use crate::state::AppState;
use crate::workers::{self, WORKERS};

type S<'a> = State<'a, Arc<AppState>>;
type R<T> = Result<T, String>;

#[derive(Serialize)]
pub struct Snapshot {
    overview: views::Overview,
    server: crate::state::ServerStatus,
    workers: Vec<workers::WorkerStatus>,
    setup_completed: bool,
    style: Style,
    data_dir: String,
    instructions: setup::InstructionsState,
}

#[tauri::command]
pub async fn snapshot(state: S<'_>) -> R<Snapshot> {
    let settings = state.settings();
    for w in WORKERS {
        workers::evaluate(&state.svc, &state.workers, &settings, w);
    }
    let overview = views::overview(&state.svc.board());
    let instructions = tauri::async_runtime::spawn_blocking(setup::instructions_state).await.map_err(|e| e.to_string())?;
    Ok(Snapshot {
        instructions,
        overview,
        server: state.server_status().await,
        workers: state.workers.snapshot(),
        setup_completed: settings.setup_completed,
        style: settings.style,
        data_dir: pinrelay_core::config::pinrelay_home().display().to_string(),
    })
}

#[tauri::command]
pub async fn list_threads(state: S<'_>, filter: String) -> R<Vec<views::ThreadSummary>> {
    Ok(views::threads(&state.svc.board(), &filter))
}

#[tauri::command]
pub async fn search_threads(state: S<'_>, query: String, archived: bool) -> R<Vec<views::ThreadSummary>> {
    let board = state.svc.board();
    Ok(board
        .search(&query, archived, 50)
        .into_iter()
        .filter_map(|h| board.state.threads.get(&h.thread_id).map(|t| views::summary(&board, t)))
        .collect())
}

#[tauri::command]
pub async fn get_thread(state: S<'_>, id: String) -> R<views::ThreadDetail> {
    views::thread_detail(&state.svc.board(), &id).ok_or_else(|| format!("Thread {id} does not exist."))
}

#[tauri::command]
pub async fn list_agents(state: S<'_>) -> R<Vec<views::AgentView>> {
    Ok(views::agents(&state.svc.board()))
}

#[tauri::command]
pub async fn social_graph(state: S<'_>) -> R<views::SocialView> {
    Ok(views::social(&state.svc.board()))
}

#[tauri::command]
pub async fn get_activity(state: S<'_>, include_reads: bool) -> R<Vec<pinrelay_core::model::ActivityItem>> {
    Ok(views::activity(&state.svc.board(), include_reads, 300))
}

#[tauri::command]
pub async fn list_runs(state: S<'_>) -> R<Vec<pinrelay_core::model::WorkerRun>> {
    let board = state.svc.board();
    Ok(board.state.runs.iter().rev().take(100).cloned().collect())
}

// ------------------------------------------------------------------ user moderation

#[tauri::command]
pub async fn set_pin(state: S<'_>, post_id: String, pinned: bool) -> R<()> {
    state
        .svc
        .board()
        .pin(Actor::User, &post_id, pinned, if pinned { "Pinned by the user" } else { "Unpinned by the user" })
        .map_err(|e| e.message)
}

#[tauri::command]
pub async fn set_post_hidden(state: S<'_>, post_id: String, hidden: bool, reason: String) -> R<()> {
    state.svc.board().hide_post(Actor::User, &post_id, hidden, &reason).map_err(|e| e.message)
}

#[tauri::command]
pub async fn resolve_disputes(state: S<'_>, post_id: String) -> R<()> {
    state
        .svc
        .board()
        .resolve_disputes(Actor::User, &post_id, "Resolved by the user")
        .map_err(|e| e.message)
}

#[tauri::command]
pub async fn set_archived(state: S<'_>, thread_id: String, archived: bool) -> R<()> {
    state.svc.board().set_archived(&thread_id, archived).map_err(|e| e.message)
}

#[tauri::command]
pub async fn set_thread_pinned(state: S<'_>, thread_id: String, pinned: bool) -> R<()> {
    let reason = if pinned { "Pinned by the user." } else { "Unpinned by the user." };
    state.svc.board().pin_thread(Actor::User, &thread_id, pinned, reason).map_err(|e| e.message)
}

#[tauri::command]
pub async fn set_quarantine(state: S<'_>, thread_id: String, quarantined: bool, reason: String) -> R<()> {
    state
        .svc
        .board()
        .quarantine(Actor::User, &thread_id, quarantined, &reason)
        .map_err(|e| e.message)
}

#[tauri::command]
pub async fn set_rules(state: S<'_>, rules: Vec<String>) -> R<()> {
    state.svc.board().set_rules(Actor::User, rules, "Edited by the user").map_err(|e| e.message)
}

#[tauri::command]
pub async fn set_access(state: S<'_>, running: bool, reason: String) -> R<()> {
    let reason = if reason.trim().is_empty() { "Stopped by the user".to_string() } else { reason };
    state.svc.board().set_access(Actor::User, running, &reason).map_err(|e| e.message)?;
    if running {
        state.start_server().await;
    } else {
        state.stop_server().await;
    }
    Ok(())
}

#[tauri::command]
pub async fn retry_server(state: S<'_>) -> R<()> {
    state.start_server().await;
    match state.server_error.lock().unwrap().clone() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

// ------------------------------------------------------------------ settings + workers

#[tauri::command]
pub async fn get_settings(state: S<'_>) -> R<Settings> {
    Ok(state.settings())
}

#[derive(Deserialize)]
pub struct SettingsPatch {
    style: Option<Style>,
    moderator: Option<WorkerSettings>,
    supervisor: Option<WorkerSettings>,
    daily_credit_budget: Option<f64>,
    run_credit_budget: Option<f64>,
    copilot_cli_path: Option<String>,
    attachments: Option<pinrelay_core::model::AttachmentPolicy>,
    active_threads: Option<usize>,
    pinned_threads: Option<usize>,
}

fn checked_worker(mut v: WorkerSettings) -> R<WorkerSettings> {
    if !(5..=1440).contains(&v.interval_minutes) {
        return Err("The interval must be between 5 minutes and 1 day.".into());
    }
    if v.model.trim().is_empty() || v.prompt.trim().len() < 20 {
        return Err("Set a model and a prompt.".into());
    }
    v.model = v.model.trim().to_string();
    Ok(v)
}

#[tauri::command]
pub async fn save_settings(state: S<'_>, patch: SettingsPatch) -> R<Settings> {
    let mut guard = state.settings.lock().unwrap();
    let s: &mut Settings = &mut guard;
    if let Some(v) = patch.style {
        s.style = v;
    }
    for (src, dst) in [(patch.moderator, &mut s.moderator), (patch.supervisor, &mut s.supervisor)] {
        if let Some(v) = src {
            *dst = checked_worker(v)?;
        }
    }
    if let Some(v) = patch.daily_credit_budget {
        s.daily_credit_budget = v.max(0.0);
    }
    if let Some(v) = patch.run_credit_budget {
        s.run_credit_budget = v.max(pinrelay_core::config::MIN_RUN_CREDIT_LIMIT);
    }
    if let Some(v) = patch.copilot_cli_path {
        s.copilot_cli_path = Some(v.trim().to_string()).filter(|v| !v.is_empty());
    }
    if let Some(v) = patch.active_threads {
        let mut board = state.svc.board();
        board.set_capacity(v).map_err(|e| e.message)?;
        s.active_threads = board.capacity;
    }
    if let Some(v) = patch.pinned_threads {
        let mut board = state.svc.board();
        board.set_pin_capacity(v).map_err(|e| e.message)?;
        s.pinned_threads = board.pin_capacity;
    }
    if let Some(v) = patch.attachments {
        s.attachments = v.clamped();
        let mut board = state.svc.board();
        board.attachments = s.attachments;
        board.purge_attachments(std::time::SystemTime::now());
    }
    s.save().map_err(|e| e.to_string())?;
    Ok(s.clone())
}

#[tauri::command]
pub fn reset_prompt(worker: String) -> String {
    if worker == "moderator" {
        pinrelay_core::config::MODERATOR_PROMPT.into()
    } else {
        pinrelay_core::config::SUPERVISOR_PROMPT.into()
    }
}

#[tauri::command]
pub async fn run_worker(state: S<'_>, name: String) -> R<Value> {
    if !WORKERS.contains(&name.as_str()) {
        return Err("Unknown worker".into());
    }
    let st = state.inner().clone();
    let settings = st.settings();
    if !settings.setup_completed {
        return Err("Approve setup first.".into());
    }
    match workers::run_worker(st.svc.clone(), &st.workers, settings, &name).await {
        Ok(Some(run)) => Ok(json!(run)),
        Ok(None) => Ok(json!({ "skipped": true })),
        Err(e) => {
            workers::set_error(&st.workers, &name, e.clone());
            Err(e)
        }
    }
}

#[tauri::command]
pub async fn list_models(state: S<'_>) -> R<Vec<Value>> {
    let settings = state.settings();
    let cli = setup::find_copilot_cli(settings.copilot_cli_path.as_deref()).ok_or("Copilot CLI not found.")?;
    let mut options = github_copilot_sdk::ClientOptions::default();
    options.program = github_copilot_sdk::CliProgram::Path(cli);
    options.working_directory = pinrelay_core::config::pinrelay_home();
    options.mode = github_copilot_sdk::ClientMode::Empty;
    options.base_directory = Some(pinrelay_core::config::pinrelay_home().join("workers").join("copilot-home"));
    options.use_logged_in_user = Some(true);
    let client = github_copilot_sdk::Client::start(options)
    .await
    .map_err(|e| e.to_string())?;
    let models = client.list_models().await.map_err(|e| e.to_string());
    let _ = client.stop().await;
    Ok(models?
        .into_iter()
        .map(|m| json!({ "id": m.id, "name": m.name }))
        .collect())
}

// ------------------------------------------------------------------ setup

// Setup commands spawn shells and read files. Run them off the main thread so the UI stays responsive.

#[tauri::command]
pub async fn setup_detect(state: S<'_>) -> R<Vec<setup::Harness>> {
    let s = state.settings();
    tauri::async_runtime::spawn_blocking(move || setup::detect(s.copilot_cli_path.as_deref()))
        .await
        .map_err(|e| e.to_string())
}

/// The .app bundle that contains this binary, or the binary itself in development.
fn app_path() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.ancestors().find(|p| p.extension().is_some_and(|e| e == "app")).map(|p| p.to_path_buf()).unwrap_or(exe))
}

#[tauri::command]
pub fn launch_at_login() -> bool {
    setup::launch_agent_path().exists()
}

fn known_harnesses(h: Vec<String>) -> Vec<String> {
    setup::HARNESSES.iter().map(|(id, _)| id.to_string()).filter(|id| h.contains(id)).collect()
}

#[tauri::command]
pub async fn setup_plan(
    state: S<'_>,
    uninstall: bool,
    style: Option<Style>,
    uses: Option<Vec<String>>,
    login: Option<bool>,
    harnesses: Option<Vec<String>>,
    visits: Option<bool>,
    reputation: Option<bool>,
    features: Option<pinrelay_core::model::Features>,
) -> R<setup::Plan> {
    let s = state.settings();
    let login = if login.unwrap_or_else(launch_at_login) { app_path() } else { None };
    let cli = std::env::current_exe().ok();
    let harnesses = known_harnesses(harnesses.unwrap_or(s.harnesses.clone()));
    tauri::async_runtime::spawn_blocking(move || {
        let uses = pinrelay_core::instructions::normalize_uses(&uses.unwrap_or(s.uses.clone()));
        Ok::<_, String>(setup::plan(&setup::PlanOptions {
            style: style.unwrap_or(s.style),
            uses: &uses,
            visits: visits.unwrap_or(s.visits),
            reputation: reputation.unwrap_or(s.reputation),
            features: features.unwrap_or(s.features),
            port: s.port,
            uninstall,
            login: login.as_deref(),
            harnesses: &harnesses,
            cli: cli.as_deref(),
        }))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Deserialize)]
pub struct Approval {
    id: String,
    before_sha: Option<String>,
}

#[derive(Deserialize)]
pub struct ApplyRequest {
    approvals: Vec<Approval>,
    uninstall: bool,
    style: Style,
    #[serde(default)]
    uses: Vec<String>,
    moderator: WorkerSettings,
    supervisor: WorkerSettings,
    #[serde(default)]
    login: Option<bool>,
    #[serde(default)]
    harnesses: Option<Vec<String>>,
    #[serde(default)]
    active_threads: Option<usize>,
    #[serde(default)]
    pinned_threads: Option<usize>,
    #[serde(default)]
    visits: Option<bool>,
    #[serde(default)]
    reputation: Option<bool>,
    #[serde(default)]
    features: Option<pinrelay_core::model::Features>,
}

#[tauri::command]
pub async fn setup_apply(state: S<'_>, req: ApplyRequest) -> R<Vec<String>> {
    let port = state.settings().port;
    let approved: Vec<(String, Option<String>)> = req.approvals.into_iter().map(|a| (a.id, a.before_sha)).collect();
    let (style, uninstall) = (req.style, req.uninstall);
    let (moderator, supervisor) = (checked_worker(req.moderator)?, checked_worker(req.supervisor)?);
    let uses = pinrelay_core::instructions::normalize_uses(&req.uses);
    let uses2 = uses.clone();
    let login = if req.login.unwrap_or_else(launch_at_login) { app_path() } else { None };
    let cli = std::env::current_exe().ok();
    let harnesses = known_harnesses(req.harnesses.unwrap_or_else(|| state.settings().harnesses));
    let harnesses2 = harnesses.clone();
    let visits = req.visits.unwrap_or_else(|| state.settings().visits);
    let reputation = req.reputation.unwrap_or_else(|| state.settings().reputation);
    let features = req.features.unwrap_or_else(|| state.settings().features);
    let written = tauri::async_runtime::spawn_blocking(move || {
        let o = setup::PlanOptions {
            style,
            uses: &uses2,
            visits,
            reputation,
            features,
            port,
            uninstall,
            login: login.as_deref(),
            harnesses: &harnesses2,
            cli: cli.as_deref(),
        };
        setup::apply(&approved, &o).map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())??;
    let mut s = state.settings.lock().unwrap();
    s.style = style;
    s.uses = uses;
    if !uninstall {
        s.harnesses = harnesses;
        s.visits = visits;
        s.reputation = reputation;
        s.features = features;
        {
            let mut board = state.svc.board();
            board.reputation = reputation;
            board.features = features;
        }
        if let Some(n) = req.active_threads {
            let mut board = state.svc.board();
            board.set_capacity(n).map_err(|e| e.message)?;
            s.active_threads = board.capacity;
        }
        if let Some(n) = req.pinned_threads {
            let mut board = state.svc.board();
            board.set_pin_capacity(n).map_err(|e| e.message)?;
            s.pinned_threads = board.pin_capacity;
        }
    }
    if uninstall {
        s.setup_completed = false;
    } else {
        s.setup_completed = true;
        s.moderator = moderator;
        s.supervisor = supervisor;
    }
    s.save().map_err(|e| e.to_string())?;
    Ok(written)
}

#[tauri::command]
pub fn use_cases() -> Vec<pinrelay_core::instructions::UseCase> {
    pinrelay_core::instructions::USE_CASES.to_vec()
}

#[tauri::command]
pub fn open_path(path: String) -> R<()> {
    let home = pinrelay_core::config::pinrelay_home();
    let p = std::path::PathBuf::from(&path);
    // Only open files inside the Pinrelay data directory (attachments, backups) or the Copilot config dir.
    if !(p.starts_with(&home) || p.starts_with(setup::copilot_home())) {
        return Err("Pinrelay opens only files in its data directory.".into());
    }
    let target = if p.is_file() { p.parent().map(|d| d.to_path_buf()).unwrap_or(p.clone()) } else { p.clone() };
    let mut cmd = std::process::Command::new(if cfg!(target_os = "macos") { "open" } else { "xdg-open" });
    if cfg!(target_os = "macos") && p.is_file() {
        cmd.arg("-R").arg(&p);
    } else {
        cmd.arg(&target);
    }
    cmd.spawn().map_err(|e| e.to_string())?;
    Ok(())
}
