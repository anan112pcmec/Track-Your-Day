//! `watcher-mutator` — watcher async yang MELAKUKAN PERUBAHAN pada data.
//!
//! Orientasi: ini "sisi tangan" dari aplikasi. Loop-nya baca kondisi OS
//! (window aktif, kecepatan mengetik, idle time, dst) lalu MENULIS
//! (mutasi) hasilnya ke `ActivityStore`. Di starter ini isi capture-nya
//! masih placeholder — nanti diganti dengan pemanggilan API OS asli
//! (mis. `active-win` di Windows/macOS/Linux, hook keyboard untuk WPM).
//!
//! Watcher ini TIDAK tahu implementasi database-nya (SQLite/in-memory),
//! dan TIDAK tahu siapa yang bereaksi terhadap event yang ia hasilkan.
//! Ia hanya kenal trait `ActivityStore` dan (opsional) sebuah broadcast
//! channel untuk memberi tahu pihak lain bahwa ada data baru.

use chrono::Utc;
use separation::{ActivityEvent, ActivityKind, ActivityStore};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

pub struct MutatorWatcher {
    store: Arc<dyn ActivityStore>,
    notify: broadcast::Sender<ActivityEvent>,
    interval: Duration,
}

impl MutatorWatcher {
    pub fn new(
        store: Arc<dyn ActivityStore>,
        notify: broadcast::Sender<ActivityEvent>,
        interval: Duration,
    ) -> Self {
        Self { store, notify, interval }
    }

    /// Jalankan loop watcher. Dipanggil sebagai tokio task terpisah dari `cli`.
    pub async fn run(self) -> anyhow::Result<()> {
        let mut tick = tokio::time::interval(self.interval);
        loop {
            tick.tick().await;

            // TODO: ganti placeholder ini dengan capture asli:
            // - active window (app + title)
            // - hitung WPM dari event keyboard
            // - deteksi idle
            let event = ActivityEvent {
                timestamp: Utc::now(),
                kind: ActivityKind::ActiveWindow {
                    app: "vscode".into(),
                    title: "track-your-day".into(),
                },
            };

            self.store.save(event.clone()).await?;
            // Broadcast tidak wajib berhasil (mungkin belum ada subscriber).
            let _ = self.notify.send(event);
        }
    }
}
