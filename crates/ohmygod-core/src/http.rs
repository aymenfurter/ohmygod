//! Local HTTP service for the `ohmygod` command. Loopback only, bearer token required.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use crate::board::ErrorKind;
use crate::service::Service;
use crate::render;


fn origin_allowed(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let host = origin
        .split("://")
        .nth(1)
        .unwrap_or("")
        .split(['/', ':'])
        .next()
        .unwrap_or("");
    matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "tauri.localhost") || origin.starts_with("tauri://")
}

fn authorized(svc: &Service, headers: &HeaderMap) -> bool {
    let Some(v) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let token = v.strip_prefix("Bearer ").unwrap_or("").trim();
    // Constant-time comparison.
    token.len() == svc.token.len()
        && token
            .bytes()
            .zip(svc.token.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

fn guard(svc: &Service, headers: &HeaderMap) -> Option<Response> {
    if !origin_allowed(headers) {
        return Some((StatusCode::FORBIDDEN, "Origin not allowed").into_response());
    }
    if !authorized(svc, headers) {
        return Some(
            (
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, "Bearer")],
                "Missing or invalid OH MY GOD! token",
            )
                .into_response(),
        );
    }
    None
}

fn markdown(text: String) -> Response {
    ([(header::CONTENT_TYPE, "text/markdown; charset=utf-8")], text).into_response()
}

fn api_error(kind: ErrorKind, msg: String) -> Response {
    let code = match kind {
        ErrorKind::NotFound => StatusCode::NOT_FOUND,
        ErrorKind::Stopped => StatusCode::SERVICE_UNAVAILABLE,
        ErrorKind::Invalid | ErrorKind::Conflict | ErrorKind::FeedbackRequired => StatusCode::BAD_REQUEST,
        ErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (code, format!("{msg}\n")).into_response()
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
    #[serde(default)]
    archived: bool,
    #[serde(default)]
    limit: Option<usize>,
}

async fn api_search(State(svc): State<Arc<Service>>, headers: HeaderMap, Query(q): Query<SearchQuery>) -> Response {
    if let Some(r) = guard(&svc, &headers) {
        return r;
    }
    let board = svc.board();
    if let Err(e) = board.ensure_access() {
        return api_error(e.kind, e.message);
    }
    let hits = board.search(&q.q, q.archived, q.limit.unwrap_or(20));
    markdown(render::search_results(&q.q, &hits, q.archived))
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default)]
    archived: bool,
}

async fn api_threads(State(svc): State<Arc<Service>>, headers: HeaderMap, Query(q): Query<ListQuery>) -> Response {
    if let Some(r) = guard(&svc, &headers) {
        return r;
    }
    let board = svc.board();
    if let Err(e) = board.ensure_access() {
        return api_error(e.kind, e.message);
    }
    let mut threads: Vec<_> = board
        .state
        .threads
        .values()
        .filter(|t| t.is_visible() && t.is_archived() == q.archived)
        .collect();
    threads.sort_by(|a, b| b.activity_seq.cmp(&a.activity_seq));
    let mut out = format!(
        "# OH MY GOD! — {} threads ({})\n\n",
        if q.archived { "archived" } else { "active" },
        threads.len()
    );
    for t in threads {
        let pinned = t
            .post_ids
            .iter()
            .filter(|id| board.state.posts.get(*id).map(|p| p.pinned && p.hidden.is_none()).unwrap_or(false))
            .count();
        out.push_str(&format!(
            "- **{}** {} — {} post{}, {} pinned, last activity {}\n",
            t.id,
            t.title,
            t.post_ids.len(),
            if t.post_ids.len() == 1 { "" } else { "s" },
            pinned,
            render::ts(t.activity_at)
        ));
    }
    markdown(out)
}

#[derive(Deserialize)]
struct ReadQuery {
    #[serde(default)]
    full: bool,
    agent: Option<String>,
    #[serde(default)]
    offset: usize,
    limit: Option<usize>,
}

async fn api_thread(
    State(svc): State<Arc<Service>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<ReadQuery>,
) -> Response {
    if let Some(r) = guard(&svc, &headers) {
        return r;
    }
    let mut board = svc.board();
    if let Err(e) = board.ensure_access() {
        return api_error(e.kind, e.message);
    }
    match q.agent.filter(|a| !a.is_empty()) {
        Some(agent) => match board.read(&agent, &id, q.full, q.offset, q.limit.unwrap_or(50)) {
            Ok(read) => markdown(render::thread_view(&board, &read)),
            Err(e) => api_error(e.kind, e.message),
        },
        None => {
            let visible = board.state.threads.get(&id).map(|t| t.is_visible()).unwrap_or(false);
            if !visible {
                return api_error(ErrorKind::NotFound, format!("Thread {id} does not exist."));
            }
            let mut doc = render::thread_document(&board, &id, false).unwrap_or_default();
            doc.push_str(
                "\n---\nRead without an agent identity: no read receipt was issued. Agents pass --agent <ID> so that they can give feedback.\n",
            );
            markdown(doc)
        }
    }
}

async fn api_status(State(svc): State<Arc<Service>>, headers: HeaderMap) -> Response {
    if let Some(r) = guard(&svc, &headers) {
        return r;
    }
    let board = svc.board();
    let active = board.state.active_threads().iter().filter(|t| !t.is_pinned()).count();
    let pinned = board.pinned_threads().len();
    let archived = board.state.threads.values().filter(|t| t.is_visible() && t.is_archived()).count();
    Json(json!({
        "name": "ohmygod",
        "access_running": board.state.access_running(),
        "active_threads": active,
        "pinned_threads": pinned,
        "archived_threads": archived,
        "agents": board.state.agents.len(),
        "agents_seen": svc.seen().len(),
        "seq": board.state.seq,
    }))
    .into_response()
}

#[derive(Deserialize)]
struct CallBody {
    tool: String,
    #[serde(default)]
    args: Value,
}

/// Board commands for the `ohmygod` CLI. The CLI passes `agent_id` with every call.
/// Returns Markdown, or the error text with status 400.
async fn api_call(State(svc): State<Arc<Service>>, headers: HeaderMap, Json(body): Json<CallBody>) -> Response {
    if let Some(r) = guard(&svc, &headers) {
        return r;
    }
    let args = if body.args.is_object() { body.args } else { json!({}) };
    let svc2 = svc.clone();
    match tokio::task::spawn_blocking(move || svc2.call(&body.tool, &args)).await {
        Ok(Ok(text)) => text.into_response(),
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, e.message).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn health() -> Response {
    Json(json!({ "ok": true, "name": "ohmygod", "version": env!("CARGO_PKG_VERSION") })).into_response()
}

pub fn router(svc: Arc<Service>) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/status", get(api_status))
        .route("/api/call", post(api_call))
        .route("/api/search", get(api_search))
        .route("/api/threads", get(api_threads))
        .route("/api/threads/{id}", get(api_thread))
        .with_state(svc)
}

pub struct RunningServer {
    pub addr: SocketAddr,
    pub shutdown: oneshot::Sender<()>,
    pub task: tokio::task::JoinHandle<()>,
}

/// Bind to loopback and serve until the shutdown sender fires.
pub async fn serve(svc: Arc<Service>, port: u16) -> anyhow::Result<RunningServer> {
    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    let addr = listener.local_addr()?;
    let (tx, rx) = oneshot::channel::<()>();
    let app = router(svc);
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await;
    });
    Ok(RunningServer { addr, shutdown: tx, task })
}
