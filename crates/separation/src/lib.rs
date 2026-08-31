use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Satu potongan aktivitas yang berhasil direkam oleh watcher.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityEvent {
    pub timestamp: DateTime<Utc>,
    pub kind: ActivityKind,
}

/// Snapshot data CPU dalam satu waktu. Dipisah jadi struct sendiri
/// (bukan field langsung di `ActivityKind::Cpu`) biar gak numpuk 13 field
/// mentah di satu variant enum, dan gampang dipakai ulang di tempat lain
/// (mis. buat panel "Performance" di TUI) tanpa harus destructure enum.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpuSnapshot {
    // ---------- hardware (statis) ----------
    pub hard_base_speed: f32,
    pub hard_sockets: u8,
    pub hard_cores: u16,
    pub hard_logical_processors: u8,
    pub hard_virtualization: bool,
    pub hard_l1_cache: f32,
    pub hard_l2_cache: f32,
    pub hard_l3_cache: f32,

    // ---------- software (live) ----------
    pub soft_utilization: f32,
    pub soft_speed_clock: f32,
    pub soft_processes: u32,
    pub soft_threads: u32,
    pub soft_handles: u64,
    pub soft_uptime: String,
}

/// Jenis data mentah yang mau kamu rekam untuk "track your day".
/// Tinggal tambah varian di sini kalau mau nambah sinyal baru
/// (mis. AppSwitch, ClipboardActivity, dst).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ActivityKind {
    ActiveWindow { app: String, title: String },
    TypingSpeed { wpm: u32 },
    TotalProcess { process: usize },
    Cpu(CpuSnapshot),
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