//! `watcher-reactive` — watcher async yang REAKTIF terhadap perubahan data.
//!
//! Orientasi: ini "sisi mata/telinga" dari aplikasi. Berbeda dari
//! `watcher-mutator` yang aktif menulis, crate ini cuma MENDENGARKAN
//! (subscribe ke broadcast channel yang dikirim mutator) lalu menjalankan
//! reaksi lewat trait `ActivityReactor` — misalnya: cetak log, kirim
//! notifikasi desktop, update ringkasan real-time, dsb.
//!
//! Karena reaksinya lewat trait, kamu bisa punya banyak reactor sekaligus
//! (logger + notifier + summarizer) tanpa watcher ini tahu detailnya.

use async_trait::async_trait;
use separation::{ActivityEvent, ActivityReactor};
use tokio::sync::broadcast;

/// Reactor contoh paling sederhana: cuma nge-print ke stdout.
/// Ganti/implement `ActivityReactor` lain (mis. desktop notification)
/// tanpa menyentuh `ReactiveWatcher`.
pub struct LoggingReactor;

#[async_trait]
impl ActivityReactor for LoggingReactor {
    async fn on_event(&self, event: &ActivityEvent) {
        println!("[reactive] event baru terdeteksi: {:?}", event.kind);
    }
}

pub struct ReactiveWatcher {
    receiver: broadcast::Receiver<ActivityEvent>,
    reactor: Box<dyn ActivityReactor>,
}

impl ReactiveWatcher {
    pub fn new(receiver: broadcast::Receiver<ActivityEvent>, reactor: Box<dyn ActivityReactor>) -> Self {
        Self { receiver, reactor }
    }

    /// Jalankan loop reaktif. Dipanggil sebagai tokio task terpisah dari `cli`.
    pub async fn run(mut self) -> anyhow::Result<()> {
        loop {
            match self.receiver.recv().await {
                Ok(event) => self.reactor.on_event(&event).await,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
        Ok(())
    }
}
