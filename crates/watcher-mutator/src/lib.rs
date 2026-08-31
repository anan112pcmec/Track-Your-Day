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

#[derive(Debug)]
pub struct CpuUtilData {
    hard_base_speed: Box<f32>,
    hard_sockets: Box<u8>,
    hard_cores: Box<u16>,
    hard_logical_processors: Box<u8>,
    hard_virtualization: Box<bool>,
    hard_l1_cache: Box<f32>,
    hard_l2_cache: Box<f32>,
    hard_l3_cache: Box<f32>,

    soft_utilization: Box<f32>,
    soft_speed_clock: Box<f32>,
    soft_processes: Box<u32>,
    soft_threads: Box<u32>,
    soft_handles: Box<u64>,
    soft_uptime: Box<String>,

    // State internal buat ngitung delta CPU usage antar tick.
    // BUKAN bagian dari CpuSnapshot — cuma dipakai di dalam update().
    last_idle: u64,
    last_kernel: u64,
    last_user: u64,
}


impl CpuUtilData {
    pub fn new() -> Self {
        CpuUtilData {
            hard_base_speed: Box::new(0.0),
            hard_sockets: Box::new(0),
            hard_cores: Box::new(0),
            hard_logical_processors: Box::new(0),
            hard_virtualization: Box::new(false),
            hard_l1_cache: Box::new(0.0),
            hard_l2_cache: Box::new(0.0),
            hard_l3_cache: Box::new(0.0),

            soft_utilization: Box::new(0.0),
            soft_speed_clock: Box::new(0.0),
            soft_processes: Box::new(0),
            soft_threads: Box::new(0),
            soft_handles: Box::new(0),
            soft_uptime: Box::new(String::new()),

             last_idle: 0,
            last_kernel: 0,
            last_user: 0,
        }
    }


    pub fn update(&mut self) {
        // ---------- soft_processes, soft_threads, soft_handles ----------
        unsafe {
            let mut perf_info = windows::Win32::System::ProcessStatus::PERFORMANCE_INFORMATION::default();
            let size = std::mem::size_of::<windows::Win32::System::ProcessStatus::PERFORMANCE_INFORMATION>() as u32;
            if windows::Win32::System::ProcessStatus::GetPerformanceInfo(&mut perf_info, size).is_ok() {
                *self.soft_processes = perf_info.ProcessCount;
                *self.soft_threads = perf_info.ThreadCount;
                *self.soft_handles = perf_info.HandleCount as u64;
            }
        }

        // ---------- soft_uptime ----------
        unsafe {
            let uptime_ms = windows::Win32::System::SystemInformation::GetTickCount64();
            let total_secs = uptime_ms / 1000;
            let h = total_secs / 3600;
            let m = (total_secs % 3600) / 60;
            let s = total_secs % 60;
            *self.soft_uptime = std::format!("{h:02}:{m:02}:{s:02}");
        }

        // ---------- soft_speed_clock & hard_base_speed ----------
        unsafe {
            let logical_count = std::cmp::max(*self.hard_logical_processors as usize, 1);
            let mut infos: std::vec::Vec<windows::Win32::System::Power::PROCESSOR_POWER_INFORMATION> =
                std::vec![windows::Win32::System::Power::PROCESSOR_POWER_INFORMATION::default(); logical_count];
            let buf_size = (std::mem::size_of::<windows::Win32::System::Power::PROCESSOR_POWER_INFORMATION>() * infos.len()) as u32;

            let result = windows::Win32::System::Power::CallNtPowerInformation(
                windows::Win32::System::Power::ProcessorInformation,
                None,
                0,
                Some(infos.as_mut_ptr() as *mut _),
                buf_size,
            );

            if result.is_ok() && !infos.is_empty() {
                let avg_current: f32 =
                    infos.iter().map(|i| i.CurrentMhz as f32).sum::<f32>() / infos.len() as f32;
                *self.soft_speed_clock = avg_current;
                *self.hard_base_speed = infos[0].MaxMhz as f32;
            }
        }

        unsafe {
    let mut idle_time = windows::Win32::Foundation::FILETIME::default();
    let mut kernel_time = windows::Win32::Foundation::FILETIME::default();
    let mut user_time = windows::Win32::Foundation::FILETIME::default();

    let filetime_to_u64 = |ft: &windows::Win32::Foundation::FILETIME| -> u64 {
        ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
    };

    if windows::Win32::System::Threading::GetSystemTimes(
        Some(&mut idle_time),
        Some(&mut kernel_time),
        Some(&mut user_time),
    ).is_ok() {
        let idle = filetime_to_u64(&idle_time);
        let kernel = filetime_to_u64(&kernel_time);
        let user = filetime_to_u64(&user_time);

        // PENTING: `kernel_time` dari Windows itu SUDAH TERMASUK idle_time
        // di dalamnya (bukan waktu kernel murni) — makanya idle harus
        // dikurangin dari total, bukan dijumlahin.
        let idle_diff = idle.saturating_sub(self.last_idle);
        let kernel_diff = kernel.saturating_sub(self.last_kernel);
        let user_diff = user.saturating_sub(self.last_user);
        let total_diff = kernel_diff + user_diff;

        if total_diff > 0 {
            let busy_diff = total_diff.saturating_sub(idle_diff);
            *self.soft_utilization = (busy_diff as f64 / total_diff as f64 * 100.0) as f32;
        }

        self.last_idle = idle;
        self.last_kernel = kernel;
        self.last_user = user;
    }
}

        // ---------- hard_sockets, hard_cores, hard_l1/l2/l3_cache ----------
        unsafe {
            let mut buf_len: u32 = 0;
            let _ = windows::Win32::System::SystemInformation::GetLogicalProcessorInformation(None, &mut buf_len);

            if buf_len > 0 {
                let count = buf_len as usize
                    / std::mem::size_of::<windows::Win32::System::SystemInformation::SYSTEM_LOGICAL_PROCESSOR_INFORMATION>();
                let mut buffer: std::vec::Vec<windows::Win32::System::SystemInformation::SYSTEM_LOGICAL_PROCESSOR_INFORMATION> =
                    std::vec![windows::Win32::System::SystemInformation::SYSTEM_LOGICAL_PROCESSOR_INFORMATION::default(); count];

                if windows::Win32::System::SystemInformation::GetLogicalProcessorInformation(Some(buffer.as_mut_ptr()), &mut buf_len).is_ok() {
                    let mut sockets = 0u8;
                    let mut cores = 0u16;
                    let (mut l1, mut l2, mut l3) = (0.0f32, 0.0f32, 0.0f32);

                    for entry in &buffer {
                        match entry.Relationship {
                            windows::Win32::System::SystemInformation::RelationProcessorPackage => sockets += 1,
                            windows::Win32::System::SystemInformation::RelationProcessorCore => cores += 1,
                            windows::Win32::System::SystemInformation::RelationCache => {
                                let cache = entry.Anonymous.Cache;
                                let size_kb = cache.Size as f32 / 1024.0;
                                match cache.Level {
                                    1 => l1 += size_kb,
                                    2 => l2 += size_kb,
                                    3 => l3 += size_kb,
                                    _ => {}
                                }
                            }
                            _ => {}
                        }
                    }

                    *self.hard_sockets = sockets;
                    *self.hard_cores = cores;
                    *self.hard_l1_cache = l1;
                    *self.hard_l2_cache = l2;
                    *self.hard_l3_cache = l3;
                }
            }
        }


        // ---------- hard_virtualization ----------
       unsafe {
            *self.hard_virtualization = windows::Win32::System::Threading::IsProcessorFeaturePresent(
                windows::Win32::System::Threading::PF_VIRT_FIRMWARE_ENABLED,
            ).as_bool();
        }
    }

    pub fn to_snapshot(&self) -> separation::CpuSnapshot {
        separation::CpuSnapshot {
            hard_base_speed: *self.hard_base_speed,
            hard_sockets: *self.hard_sockets,
            hard_cores: *self.hard_cores,
            hard_logical_processors: *self.hard_logical_processors,
            hard_virtualization: *self.hard_virtualization,
            hard_l1_cache: *self.hard_l1_cache,
            hard_l2_cache: *self.hard_l2_cache,
            hard_l3_cache: *self.hard_l3_cache,
            soft_utilization: *self.soft_utilization,
            soft_speed_clock: *self.soft_speed_clock,
            soft_processes: *self.soft_processes,
            soft_threads: *self.soft_threads,
            soft_handles: *self.soft_handles,
            soft_uptime: (*self.soft_uptime).clone(),
        }
    }
}



pub struct MutatorWatcher {
    store: Arc<dyn ActivityStore>,
    notify: broadcast::Sender<ActivityEvent>,
    interval: Duration,
    key_counter: KeyCounter,
    process_window: usize,
    cpu_util: CpuUtilData,
}

impl MutatorWatcher {
    pub fn new(
        store: Arc<dyn ActivityStore>,
        notify: broadcast::Sender<ActivityEvent>,
        interval: Duration,
        key_counter: KeyCounter,
        process_window: usize,
        cpu_util: CpuUtilData,
    ) -> Self {
        Self { store, notify, interval, key_counter, process_window, cpu_util }
    }

    /// Jalankan loop watcher. Dipanggil sebagai tokio task terpisah dari `cli`.
    pub async fn run(mut self) -> anyhow::Result<()> {
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

            {
                self.cpu_util.update();
                let event_cpu = separation::ActivityEvent {
                    timestamp: Utc::now(),
                    kind: separation::ActivityKind::Cpu(self.cpu_util.to_snapshot()),
                };
                self.store.save(event_cpu.clone()).await?;
                let _ = self.notify.send(event_cpu);
            }
        }
    }
}