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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RamSnapshot {
    pub hard_capacity: f32,
    pub hard_speed: u32,
    pub hard_slots_used: u16,
    pub hard_form_factor: String,
    pub soft_hardware_reserve: f32,
    pub soft_in_use: f32,
    pub soft_available: f32,
    pub soft_in_commited: f32,
    pub soft_available_commited: f32,
    pub soft_cached: f32,
    pub soft_page_pool: f32,
    pub soft_non_paged_pool: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskSnapshot {
    pub hard_capacity: u32,
    pub hard_formatted: u32,
    pub hard_system_disk: bool,
    pub hard_type: String,
    pub hard_capacity_in_use: u32,
    pub soft_read_speed: f32,
    pub soft_write_speed: f32,
    pub soft_active_time: f32,
    pub soft_average_response_time: f32,
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
    Ram(RamSnapshot),
    Disk(DiskSnapshot),
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