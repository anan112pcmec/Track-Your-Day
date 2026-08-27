//! `database` — implementasi nyata dari trait `separation::ActivityStore`.
//!
//! Sekarang isinya in-memory (Vec di belakang Mutex) supaya starter ini
//! ringan & langsung jalan tanpa setup file/DB apapun. Nanti kalau mau
//! ganti ke SQLite/sled/rocksdb, cukup ganti isi `MemoryStore` (atau bikin
//! struct baru) — selama tetap implement `ActivityStore`, `cli` dan
//! `watcher-mutator` tidak perlu diubah sama sekali.

use anyhow::Result;
use async_trait::async_trait;
use separation::{ActivityEvent, ActivityStore};
use tokio::sync::Mutex;

pub struct MemoryStore {
    events: Mutex<Vec<ActivityEvent>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self { events: Mutex::new(Vec::new()) }
    }
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ActivityStore for MemoryStore {
    async fn save(&self, event: ActivityEvent) -> Result<()> {
        self.events.lock().await.push(event);
        Ok(())
    }

    async fn recent(&self, limit: usize) -> Result<Vec<ActivityEvent>> {
        let events = self.events.lock().await;
        let start = events.len().saturating_sub(limit);
        Ok(events[start..].to_vec())
    }
}
