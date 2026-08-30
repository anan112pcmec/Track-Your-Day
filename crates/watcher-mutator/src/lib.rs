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
use rdev::{listen, EventType};
use separation::{ActivityEvent, ActivityKind, ActivityStore};
use sysinfo::{Process, System};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use active_win_pos_rs::get_active_window;

pub struct KeyCounter {
    count: Arc<AtomicUsize>
}

impl KeyCounter {
    pub fn start() -> Self {
        let count: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
        let counter_for_thread = count.clone();


        std::thread::spawn(move || {
            let callback = move | event: rdev::Event| {
                if let EventType::KeyPress(_) = event.event_type {
                    counter_for_thread.fetch_add(1, Ordering::Relaxed);
                }
            };

            if let Err(err) = listen(callback) {
                eprintln!("[watcher-mutator] gagal pasang keyboard hook: {err:?}");
            }

        });

        
        Self {count}
    }

    pub fn take_and_reset(&self) -> usize {
        self.count.swap(0, Ordering::Relaxed)
    }
}


pub struct MutatorWatcher {
    store: Arc<dyn ActivityStore>,
    notify: broadcast::Sender<ActivityEvent>,
    interval: Duration,
    key_counter: KeyCounter,
    process_window: usize,
}

impl MutatorWatcher {
    pub fn new(
        store: Arc<dyn ActivityStore>,
        notify: broadcast::Sender<ActivityEvent>,
        interval: Duration,
        key_counter: KeyCounter,
        process_window: usize,
    ) -> Self {
        Self { store, notify, interval, key_counter, process_window }
    }

    /// Jalankan loop watcher. Dipanggil sebagai tokio task terpisah dari `cli`.
    pub async fn run(self) -> anyhow::Result<()> {
        let mut tick = tokio::time::interval(self.interval);
        loop {
            tick.tick().await;

           {
                let keystrokes = self.key_counter.take_and_reset();
                let elapsed_minutes = self.interval.as_secs_f64() / 60.0;
                let wpm = ((keystrokes as f64 / 5.0) / elapsed_minutes).round() as u32;

                // TODO: ganti placeholder ini dengan capture asli:
                // - active window (app + title)
                // - hitung WPM dari event keyboard
                // - deteksi idle

                let eventwpm = ActivityEvent {
                    timestamp: Utc::now(),
                    kind: ActivityKind::TypingSpeed { wpm }
                };

                self.store.save(eventwpm.clone()).await?;
                let _ = self.notify.send(eventwpm);
           }

            {
                let eventwindow = match get_active_window(){
                    Ok(window) => ActivityEvent { timestamp: Utc::now(), 
                        kind: ActivityKind::ActiveWindow { app: window.process_name, title: window.title } 
                    },
                        
                    Err(_) => {
                        eprintln!("[watcher-mutator] gagal baca active window");
                        ActivityEvent { timestamp: Utc::now(), kind: ActivityKind::ActiveWindow { app: "unknown".into(), title: "unknown".into() } }
                    }
                };
                self.store.save(eventwindow.clone()).await?;

                // Broadcast tidak wajib berhasil (mungkin belum ada subscriber).
                let _ = self.notify.send(eventwindow);
            }

            {
                fn total_process_count() -> usize {
                    let mut sys = sysinfo::System::new_all();
                    sys.refresh_all();
                    sys.processes().len()
                };
                let eventwindow: separation::ActivityEvent = separation::ActivityEvent{
                    timestamp: Utc::now(),
                    kind: separation::ActivityKind::TotalProcess { process:  total_process_count() }
                };
                self.store.save(eventwindow.clone()).await?;
            }
        }
    }
}
