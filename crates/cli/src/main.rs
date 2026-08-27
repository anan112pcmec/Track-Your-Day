//! `cli` — main logic. Titik masuk "track your day".
//!
//! Ini crate SATU-SATUNYA yang tahu semua implementasi konkret:
//! `database::MemoryStore`, `watcher_mutator::MutatorWatcher`,
//! `watcher_reactive::ReactiveWatcher`, `api::ApiState`.
//! Semua crate itu sendiri saling tidak kenal — cuma kenal trait dari
//! `separation`. Di sinilah mereka "dinikahkan" jadi satu aplikasi.
//!
//! Alurnya sesuai model CLI yang kamu mau: dijalankan SEKALI, lalu jalan
//! terus (foreground/daemon-style) melakukan watch end-to-end sampai
//! di-Ctrl+C.

use database::MemoryStore;
use separation::ActivityStore;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use watcher_mutator::MutatorWatcher;
use watcher_reactive::{LoggingReactor, ReactiveWatcher};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("track-your-day: starting...\n");

    // 1) DATABASE — konkret: MemoryStore, tapi dipegang sebagai trait object
    let store: Arc<dyn ActivityStore> = Arc::new(MemoryStore::new());
    println!("[cli] database ready (via separation::ActivityStore trait)");

    // 2) Channel penghubung antara watcher-mutator -> watcher-reactive.
    let (tx, rx) = broadcast::channel(64);

    // 3) WATCHER-MUTATOR — nulis data tiap 2 detik ke store, sambil
    let mutator = MutatorWatcher::new(store.clone(), tx.clone(), Duration::from_secs(2));
    let mutator_handle = tokio::spawn(mutator.run());
    println!("[cli] watcher-mutator spawned (menulis ke database)");

    // 4) WATCHER-REACTIVE — subscribe ke channel yang sama, bereaksi
    let reactive = ReactiveWatcher::new(rx, Box::new(LoggingReactor));
    let reactive_handle = tokio::spawn(reactive.run());
    println!("[cli] watcher-reactive spawned (bereaksi terhadap perubahan)\n");

    // 5) API — dibiarkan minimal, tapi dibuktikan ia juga baca lewat
    let api_state = api::ApiState::new(store.clone());

    tokio::time::sleep(Duration::from_millis(2500)).await;
    let count = api_state.health_check().await?;
    println!(
        "\n[cli] BUKTI: `api` membaca {} event dari `database`, yang ditulis oleh `watcher-mutator`,",
        count
    );
    println!("[cli]        dan sudah di-log balik oleh `watcher-reactive` di atas.");
    println!("[cli] Semua terhubung lewat kontrak di `separation` — tidak ada crate yang saling depend langsung.\n");

    println!("track-your-day berjalan. Tekan Ctrl+C untuk berhenti...");
    tokio::signal::ctrl_c().await?;

    mutator_handle.abort();
    reactive_handle.abort();
    println!("\ntrack-your-day: stopped.");
    Ok(())
}
