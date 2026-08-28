//! `separation` — lapisan domain / "batas pemisah" antar crate.
//!
//! Crate ini SENGAJA tidak boleh depend ke `database`, `watcher-mutator`,
//! `watcher-reactive`, atau `api`. Isinya cuma tipe data + trait (kontrak).
//! Efeknya: crate lain saling tidak kenal satu sama lain, mereka cuma kenal
//! trait di sini. `cli` yang nanti "menikahkan" implementasi konkretnya.
//!
//! Kalau besok mau ganti storage dari in-memory ke SQLite, atau ganti
//! reactor dari println ke notifikasi desktop, cukup ganti implementasi —
//! trait & tipe di file ini tidak perlu berubah.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Satu potongan aktivitas yang berhasil direkam oleh watcher.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityEvent {
    pub timestamp: DateTime<Utc>,
    pub kind: ActivityKind,
}

/// Jenis data mentah yang mau kamu rekam untuk "track your day".
/// Tinggal tambah varian di sini kalau mau nambah sinyal baru
/// (mis. AppSwitch, ClipboardActivity, dst).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ActivityKind {
    ActiveWindow { app: String, title: String },
    TypingSpeed { wpm: u32 },
    WindowId {id: usize},
    ProcessId {id: usize},
    Idle { seconds: u32 },
    Position {
        x: f32,
        y: f32,
        width: f32,
        height:f32,
    }
}

/// Kontrak penyimpanan. Diimplementasikan oleh crate `database`.
/// `watcher-mutator` cuma butuh trait ini — tidak tahu & tidak peduli
/// di baliknya SQLite, JSON file, atau in-memory.
#[async_trait]
pub trait ActivityStore: Send + Sync {
    async fn save(&self, event: ActivityEvent) -> anyhow::Result<()>;
    async fn recent(&self, limit: usize) -> anyhow::Result<Vec<ActivityEvent>>;
}

/// Kontrak "bereaksi" terhadap event baru. Diimplementasikan oleh
/// crate `watcher-reactive` (atau apapun yang mau bereaksi: logger,
/// notifikasi desktop, dsb).
#[async_trait]
pub trait ActivityReactor: Send + Sync {
    async fn on_event(&self, event: &ActivityEvent);
}
