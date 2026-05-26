use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};

use super::Permissions;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub client_id: String,
    pub display_name: String,
    pub peer_addr: String,
    pub permissions: Permissions,
    pub connected_at: DateTime<Utc>,
    pub stats: SessionStats,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionStats {
    pub fps: f32,
    pub latency_ms: u32,
    pub bitrate_kbps: u32,
    pub bytes_sent: u64,
}

pub struct SessionManager {
    sessions: DashMap<String, Session>,
    /// HWND currently being shared (0 = not sharing)
    active_hwnd: isize,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: DashMap::new(),
            active_hwnd: 0,
        }
    }

    pub fn add_session(&self, session: Session) {
        self.sessions.insert(session.id.clone(), session);
    }

    pub fn remove_session(&self, id: &str) -> Option<Session> {
        self.sessions.remove(id).map(|(_, s)| s)
    }

    pub fn get_sessions(&self) -> Vec<Session> {
        self.sessions.iter().map(|e| e.value().clone()).collect()
    }

    pub fn update_stats(&self, session_id: &str, stats: SessionStats) {
        if let Some(mut s) = self.sessions.get_mut(session_id) {
            s.stats = stats;
        }
    }

    pub fn set_active_hwnd(&mut self, hwnd: isize) {
        self.active_hwnd = hwnd;
    }

    pub fn active_hwnd(&self) -> isize {
        self.active_hwnd
    }

    pub fn is_sharing(&self) -> bool {
        self.active_hwnd != 0
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }
}
