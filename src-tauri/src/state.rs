use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use ohmygod_core::config::Settings;
use ohmygod_core::http::{serve, RunningServer};
use ohmygod_core::service::Service;
use serde::Serialize;

use crate::workers::Workers;

pub struct AppState {
    pub svc: Arc<Service>,
    pub settings: Mutex<Settings>,
    pub server: tokio::sync::Mutex<Option<RunningServer>>,
    pub server_error: Mutex<Option<String>>,
    pub workers: Workers,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServerStatus {
    pub listening: bool,
    pub port: u16,
    pub error: Option<String>,
    pub access_running: bool,
    /// Agents that ran a `ohmygod` command within the active window.
    pub agents_active: usize,
    pub agents: Vec<AgentCalls>,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentCalls {
    pub agent_id: String,
    pub last_seen: DateTime<Utc>,
    pub calls: u64,
}

/// An agent counts as active if it ran a command within this window.
pub const ACTIVE_WINDOW_MINUTES: i64 = 10;

impl AppState {
    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    pub async fn start_server(&self) {
        let mut guard = self.server.lock().await;
        if guard.is_some() {
            return;
        }
        if !self.svc.board().state.access_running() {
            return;
        }
        let port = self.settings().port;
        match serve(self.svc.clone(), port).await {
            Ok(s) => {
                *guard = Some(s);
                *self.server_error.lock().unwrap() = None;
            }
            Err(e) => {
                *self.server_error.lock().unwrap() = Some(format!(
                    "Cannot listen on 127.0.0.1:{port}: {e}. Another program may use this port."
                ));
            }
        }
    }

    pub async fn stop_server(&self) {
        let mut guard = self.server.lock().await;
        if let Some(s) = guard.take() {
            let _ = s.shutdown.send(());
            let _ = tokio::time::timeout(std::time::Duration::from_secs(3), s.task).await;
        }
    }

    pub async fn server_status(&self) -> ServerStatus {
        let listening = self.server.lock().await.is_some();
        let port = self.settings().port;
        let board = self.svc.board();
        let now = Utc::now();
        let agents: Vec<AgentCalls> = self
            .svc
            .seen()
            .into_iter()
            .map(|(agent_id, x)| AgentCalls { agent_id, last_seen: x.last_call, calls: x.calls })
            .collect();
        let active = agents.iter().filter(|c| (now - c.last_seen).num_minutes() < ACTIVE_WINDOW_MINUTES).count();
        ServerStatus {
            listening,
            port,
            error: self.server_error.lock().unwrap().clone(),
            access_running: board.state.access_running(),
            agents_active: active,
            agents,
            started_at: self.svc.started_at,
        }
    }
}
