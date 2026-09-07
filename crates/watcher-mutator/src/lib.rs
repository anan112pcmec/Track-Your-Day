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

#[derive(Debug)]
pub struct RamUtilData {
    hard_capacity: Box<f32>,
    hard_speed: Box<u32>,
    hard_slots_used: Box<u16>,
    hard_form_factor: Box<&'static str>,

    soft_hardware_reserve: Box<f32>,
    soft_in_use: Box<f32>,
    soft_available: Box<f32>,
    soft_in_commited: Box<f32>,
    soft_available_commited: Box<f32>,
    soft_cached: Box<f32>,
    soft_page_pool: Box<f32>,
    soft_non_paged_pool: Box<f32>,
}

impl RamUtilData {
    pub fn new() -> Self {
        RamUtilData {
            hard_capacity: Box::new(0.0),
            hard_speed: Box::new(0),
            hard_slots_used: Box::new(0),
            hard_form_factor: Box::new("Unknown"),
            soft_hardware_reserve: Box::new(0.0),
            soft_in_use: Box::new(0.0),
            soft_available: Box::new(0.0),
            soft_in_commited: Box::new(0.0),
            soft_available_commited: Box::new(0.0),
            soft_cached: Box::new(0.0),
            soft_page_pool: Box::new(0.0),
            soft_non_paged_pool: Box::new(0.0),
        }
    }

    pub fn update(&mut self) {
        const BYTES_TO_GB: f32 = 1024.0 * 1024.0 * 1024.0;

        // ---------- hard_capacity ----------
        // GetPhysicallyInstalledSystemMemory baca dari SMBIOS = kapasitas
        // FISIK terpasang (beda dari GlobalMemoryStatusEx yang cuma ngasih
        // yang "kelihatan" oleh OS — bisa lebih kecil karena hardware reserved).
        let mut installed_kb: u64 = 0;
        unsafe {
            let _ = windows::Win32::System::SystemInformation::GetPhysicallyInstalledSystemMemory(&mut installed_kb);
        }
        *self.hard_capacity = (installed_kb as f32 * 1024.0) / BYTES_TO_GB;

        // ---------- soft_in_use, soft_available, soft_in_commited, soft_available_commited, soft_hardware_reserve ----------
        unsafe {
            let mut mem_status = windows::Win32::System::SystemInformation::MEMORYSTATUSEX::default();
            mem_status.dwLength = std::mem::size_of::<windows::Win32::System::SystemInformation::MEMORYSTATUSEX>() as u32;

            if windows::Win32::System::SystemInformation::GlobalMemoryStatusEx(&mut mem_status).is_ok() {
                let total_phys_gb = mem_status.ullTotalPhys as f32 / BYTES_TO_GB;
                let avail_phys_gb = mem_status.ullAvailPhys as f32 / BYTES_TO_GB;
                *self.soft_in_use = total_phys_gb - avail_phys_gb;
                *self.soft_available = avail_phys_gb;

                // "Hardware reserved" ala Task Manager = terpasang fisik - yang kelihatan OS.
                *self.soft_hardware_reserve = (*self.hard_capacity - total_phys_gb).max(0.0);

                // Commit charge: total page file "budget" vs yang masih tersisa.
                let total_commit_gb = mem_status.ullTotalPageFile as f32 / BYTES_TO_GB;
                let avail_commit_gb = mem_status.ullAvailPageFile as f32 / BYTES_TO_GB;
                *self.soft_in_commited = total_commit_gb - avail_commit_gb;
                *self.soft_available_commited = avail_commit_gb;
            }
        }

        // ---------- soft_cached, soft_page_pool, soft_non_paged_pool ----------
        // Sama kayak di CpuUtilData: GetPerformanceInfo juga punya angka-angka
        // memory ini sekalian (dalam satuan "pages", dikali PageSize jadi bytes).
        unsafe {
            let mut perf_info = windows::Win32::System::ProcessStatus::PERFORMANCE_INFORMATION::default();
            let size = std::mem::size_of::<windows::Win32::System::ProcessStatus::PERFORMANCE_INFORMATION>() as u32;
            if windows::Win32::System::ProcessStatus::GetPerformanceInfo(&mut perf_info, size).is_ok() {
                let page_size = perf_info.PageSize as f32;
                *self.soft_cached = (perf_info.SystemCache as f32 * page_size) / BYTES_TO_GB;
                *self.soft_page_pool = (perf_info.KernelPaged as f32 * page_size) / BYTES_TO_GB;
                *self.soft_non_paged_pool = (perf_info.KernelNonpaged as f32 * page_size) / BYTES_TO_GB;
            }
        }

        // ---------- hard_speed, hard_slots_used, hard_form_factor ----------
        // Paling ribet: gak ada API langsung buat ini, harus baca tabel
        // mentah SMBIOS (data yang sama yang dipakai BIOS/UEFI) dan parse
        // manual byte-per-byte struktur "Type 17 - Memory Device".
        unsafe {
            const RSMB_SIGNATURE: u32 = 0x52534D42; // ASCII "RSMB"
            let provider = windows::Win32::System::SystemInformation::FIRMWARE_TABLE_PROVIDER(RSMB_SIGNATURE);

            // Panggilan pertama: buffer `None`, cuma buat tau ukuran yang dibutuhkan.
            let needed_size = windows::Win32::System::SystemInformation::GetSystemFirmwareTable(
                provider,
                0,
                None,
            );

            if needed_size > 0 {
                let mut buffer: std::vec::Vec<u8> = std::vec![0u8; needed_size as usize];
                let actual_size = windows::Win32::System::SystemInformation::GetSystemFirmwareTable(
                    provider,
                    0,
                    Some(&mut buffer),
                );

                if actual_size > 0 {
                    // Header RawSMBIOSData: 4 byte info + 4 byte panjang tabel,
                    // baru setelah itu data SMBIOS mentahnya dimulai.
                    let table_data = &buffer[8..];

                    let mut offset = 0usize;
                    let mut slots_used = 0u16;
                    let mut speeds_found: std::vec::Vec<u16> = std::vec::Vec::new();
                    let mut form_factor: &'static str = "Unknown";

                    while offset + 4 <= table_data.len() {
                        let struct_type = table_data[offset];
                        let struct_length = table_data[offset + 1] as usize;

                        if struct_length < 4 || offset + struct_length > table_data.len() {
                            break;
                        }

                        if struct_type == 17 && struct_length >= 0x15 {
                            // Offset 0x0C-0x0D: Size. 0 = slot kosong, 0xFFFF = unknown.
                            let size_raw = u16::from_le_bytes([table_data[offset + 0x0C], table_data[offset + 0x0D]]);
                            if size_raw != 0 && size_raw != 0xFFFF {
                                slots_used += 1;

                                // Offset 0x0E: Form Factor (cuma dicatat dari slot pertama yang keisi).
                                if form_factor == "Unknown" {
                                    form_factor = match table_data[offset + 0x0E] {
                                        0x09 => "DIMM",
                                        0x0D => "SODIMM",
                                        0x08 => "SIMM",
                                        _ => "Other",
                                    };
                                }

                                // Offset 0x15-0x16: Speed (MHz), kalau struct-nya cukup panjang.
                                if struct_length >= 0x17 {
                                    let speed = u16::from_le_bytes([table_data[offset + 0x15], table_data[offset + 0x16]]);
                                    if speed > 0 {
                                        speeds_found.push(speed);
                                    }
                                }
                            }
                        }

                        // Lompat ke akhir bagian "formatted" struct, lalu lewatin
                        // deretan string yang diakhiri byte 0x00 0x00 (double-null).
                        let mut cursor = offset + struct_length;
                        while cursor + 1 < table_data.len() && !(table_data[cursor] == 0 && table_data[cursor + 1] == 0) {
                            cursor += 1;
                        }
                        offset = cursor + 2;
                    }
                    
                    

                    *self.hard_slots_used = slots_used;
                    *self.hard_form_factor = form_factor;
                    if !speeds_found.is_empty() {
                        *self.hard_speed = speeds_found.iter().map(|&s| s as u32).sum::<u32>() / speeds_found.len() as u32;
                    }
                }
            }
        }
    }

    pub fn to_snapshot(&self) -> separation::RamSnapshot {
        separation::RamSnapshot {
            hard_capacity: *self.hard_capacity,
            hard_speed: *self.hard_speed,
            hard_slots_used: *self.hard_slots_used,
            hard_form_factor: (*self.hard_form_factor).to_string(),
            soft_hardware_reserve: *self.soft_hardware_reserve,
            soft_in_use: *self.soft_in_use,
            soft_available: *self.soft_available,
            soft_in_commited: *self.soft_in_commited,
            soft_available_commited: *self.soft_available_commited,
            soft_cached: *self.soft_cached,
            soft_page_pool: *self.soft_page_pool,
            soft_non_paged_pool: *self.soft_non_paged_pool,
        }
    }
}

#[derive(Debug, Clone)]
pub enum TypeDisk {
    SSD(String),
    HDD(String),
}

impl TypeDisk {
    fn name(&self) -> String {
        match self {
            TypeDisk::SSD(name) => format!("SSD: {}", name),
            TypeDisk::HDD(name) => format!("HDD: {}", name),
        }
    }
}

#[derive(Debug)]
pub struct DiskUtilData {
    hard_capacity: u32,
    hard_formatted: u32,
    hard_system_disk: bool,
    hard_type: TypeDisk,
    hard_capacity_in_use: u32,
    

    soft_read_speed: f32,
    soft_write_speed: f32,
    soft_active_time: f32,
    soft_average_response_time: f32,

    // State internal buat ngitung delta antar tick — sama pola dengan
    // last_idle/last_kernel di CpuUtilData. Angka dari IOCTL_DISK_PERFORMANCE
    // itu KUMULATIF sejak counter diaktifkan, bukan angka sesaat.
    last_bytes_read: i64,
    last_bytes_written: i64,
    last_read_time: i64,
    last_write_time: i64,
    last_idle_time: i64,
    last_read_count: u32,
    last_write_count: u32,
    last_query_time: i64,
}

impl DiskUtilData {
    pub fn new() -> Self {
        DiskUtilData {
            hard_capacity: 0,
            hard_formatted: 0,
            hard_system_disk: true, // asumsi: disk yang dipantau = disk sistem
            hard_type: TypeDisk::HDD("Unknown".to_string()),
            hard_capacity_in_use: 0,

            soft_read_speed: 0.0,
            soft_write_speed: 0.0,
            soft_active_time: 0.0,
            soft_average_response_time: 0.0,

            last_bytes_read: 0,
            last_bytes_written: 0,
            last_read_time: 0,
            last_write_time: 0,
            last_idle_time: 0,
            last_read_count: 0,
            last_write_count: 0,
            last_query_time: 0,
        }
    }

    pub fn update(&mut self) {
        // PENTING: hardcode "\\.\PhysicalDrive0" — asumsi disk nomor 0
        // adalah disk sistem kamu. Kalau ada lebih dari 1 disk fisik dan
        // sistemnya bukan disk 0, ini bakal baca disk yang salah.
        let drive_path: std::vec::Vec<u16> = "\\\\.\\PhysicalDrive0\0".encode_utf16().collect();

        unsafe {
            let handle_result = windows::Win32::Storage::FileSystem::CreateFileW(
                windows::core::PCWSTR(drive_path.as_ptr()),
                0, // gak butuh akses baca/tulis data, cuma query metadata
                windows::Win32::Storage::FileSystem::FILE_SHARE_READ
                    | windows::Win32::Storage::FileSystem::FILE_SHARE_WRITE,
                None,
                windows::Win32::Storage::FileSystem::OPEN_EXISTING,
                windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            );

            if let Ok(handle) = handle_result {
                // ---------- hard_type: SSD vs HDD ----------
                // Trik resminya Disk Defragmenter Windows: cek "seek penalty".
                // Kalau IncursSeekPenalty = false → SSD, true → HDD.
                let query = windows::Win32::System::Ioctl::STORAGE_PROPERTY_QUERY {
                    PropertyId: windows::Win32::System::Ioctl::StorageDeviceSeekPenaltyProperty,
                    QueryType: windows::Win32::System::Ioctl::PropertyStandardQuery,
                    AdditionalParameters: [0],
                };
                let mut seek_penalty = windows::Win32::System::Ioctl::DEVICE_SEEK_PENALTY_DESCRIPTOR::default();
                let mut bytes_returned: u32 = 0;

                let seek_result = windows::Win32::System::IO::DeviceIoControl(
                    handle,
                    windows::Win32::System::Ioctl::IOCTL_STORAGE_QUERY_PROPERTY,
                    Some(&query as *const _ as *const std::ffi::c_void),
                    std::mem::size_of::<windows::Win32::System::Ioctl::STORAGE_PROPERTY_QUERY>() as u32,
                    Some(&mut seek_penalty as *mut _ as *mut std::ffi::c_void),
                    std::mem::size_of::<windows::Win32::System::Ioctl::DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32,
                    Some(&mut bytes_returned),
                    None,
                );

                if seek_result.is_ok() {
                    self.hard_type = if seek_penalty.IncursSeekPenalty {
                        TypeDisk::HDD("PhysicalDrive0".to_string())
                    } else {
                        TypeDisk::SSD("PhysicalDrive0".to_string())
                    };
                }

                // ---------- soft_read_speed, soft_write_speed, soft_active_time, soft_average_response_time ----------
                let mut perf = windows::Win32::System::Ioctl::DISK_PERFORMANCE::default();
                let mut perf_bytes_returned: u32 = 0;

                let perf_result = windows::Win32::System::IO::DeviceIoControl(
                    handle,
                    windows::Win32::System::Ioctl::IOCTL_DISK_PERFORMANCE,
                    None,
                    0,
                    Some(&mut perf as *mut _ as *mut std::ffi::c_void),
                    std::mem::size_of::<windows::Win32::System::Ioctl::DISK_PERFORMANCE>() as u32,
                    Some(&mut perf_bytes_returned),
                    None,
                );

                if perf_result.is_ok() {
                    let query_diff = perf.QueryTime - self.last_query_time;

                    if query_diff > 0 {
                        // QueryTime satuannya 100-nanodetik sejak boot (sama kayak FILETIME).
                        let elapsed_secs = query_diff as f64 / 10_000_000.0;

                        let bytes_read_diff = (perf.BytesRead - self.last_bytes_read).max(0) as f64;
                        let bytes_written_diff = (perf.BytesWritten - self.last_bytes_written).max(0) as f64;
                        self.soft_read_speed = (bytes_read_diff / elapsed_secs / (1024.0 * 1024.0)) as f32;
                        self.soft_write_speed = (bytes_written_diff / elapsed_secs / (1024.0 * 1024.0)) as f32;

                        // Sama logikanya kayak CPU: IdleTime dikurangin dari total,
                        // sisanya itu waktu disk beneran sibuk.
                        let idle_diff = (perf.IdleTime - self.last_idle_time).max(0) as f64;
                        let active_ratio = 1.0 - (idle_diff / query_diff as f64);
                        self.soft_active_time = (active_ratio * 100.0).clamp(0.0, 100.0) as f32;

                        let read_time_diff = (perf.ReadTime - self.last_read_time).max(0) as f64;
                        let write_time_diff = (perf.WriteTime - self.last_write_time).max(0) as f64;
                        let read_count_diff = perf.ReadCount.saturating_sub(self.last_read_count) as f64;
                        let write_count_diff = perf.WriteCount.saturating_sub(self.last_write_count) as f64;
                        let total_io_time = read_time_diff + write_time_diff;
                        let total_io_count = read_count_diff + write_count_diff;

                        if total_io_count > 0.0 {
                            // Konversi 100-nanodetik ke milidetik.
                            self.soft_average_response_time = ((total_io_time / total_io_count) / 10_000.0) as f32;
                        }
                    }

                    self.last_bytes_read = perf.BytesRead;
                    self.last_bytes_written = perf.BytesWritten;
                    self.last_read_time = perf.ReadTime;
                    self.last_write_time = perf.WriteTime;
                    self.last_idle_time = perf.IdleTime;
                    self.last_read_count = perf.ReadCount;
                    self.last_write_count = perf.WriteCount;
                    self.last_query_time = perf.QueryTime;
                }

                let _ = windows::Win32::Foundation::CloseHandle(handle);
            }
        }

        // ---------- hard_capacity, hard_formatted ----------
        // Sengaja beda sumber: hard_capacity dari ukuran RAW disk fisik
        // (IOCTL_DISK_GET_LENGTH_INFO), hard_formatted dari ukuran volume
        // SETELAH diformat filesystem (GetDiskFreeSpaceExW) — biasanya
        // hard_formatted sedikit lebih kecil karena overhead filesystem.
        unsafe {
            let drive_path: std::vec::Vec<u16> = "\\\\.\\PhysicalDrive0\0".encode_utf16().collect();
            let handle_result = windows::Win32::Storage::FileSystem::CreateFileW(
                windows::core::PCWSTR(drive_path.as_ptr()),
                0,
                windows::Win32::Storage::FileSystem::FILE_SHARE_READ
                    | windows::Win32::Storage::FileSystem::FILE_SHARE_WRITE,
                None,
                windows::Win32::Storage::FileSystem::OPEN_EXISTING,
                windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            );

            if let Ok(handle) = handle_result {
                let mut length_info = windows::Win32::System::Ioctl::GET_LENGTH_INFORMATION::default();
                let mut bytes_returned: u32 = 0;

                let length_result = windows::Win32::System::IO::DeviceIoControl(
                    handle,
                    windows::Win32::System::Ioctl::IOCTL_DISK_GET_LENGTH_INFO,
                    None,
                    0,
                    Some(&mut length_info as *mut _ as *mut std::ffi::c_void),
                    std::mem::size_of::<windows::Win32::System::Ioctl::GET_LENGTH_INFORMATION>() as u32,
                    Some(&mut bytes_returned),
                    None,
                );

                if length_result.is_ok() {
                    self.hard_capacity = (length_info.Length / (1024 * 1024)) as u32; // dalam MB
                }

                let _ = windows::Win32::Foundation::CloseHandle(handle);
            }

            let system_drive: std::vec::Vec<u16> = "C:\\\0".encode_utf16().collect();
            let mut total_bytes: u64 = 0;
            let mut free_bytes: u64 = 0; // <- tambahan
            if windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
                windows::core::PCWSTR(system_drive.as_ptr()),
                None,
                Some(&mut total_bytes),
                Some(&mut free_bytes), // <- diisi, sebelumnya None
            )
            .is_ok()
            {
                self.hard_formatted = (total_bytes / (1024 * 1024)) as u32; // dalam MB
                self.hard_capacity_in_use = ((total_bytes.saturating_sub(free_bytes)) / (1024 * 1024)) as u32; // <- tambahan
            }
        }
    }

    pub fn to_snapshot(&self) -> separation::DiskSnapshot {
        separation::DiskSnapshot {
            hard_capacity: self.hard_capacity,
            hard_formatted: self.hard_formatted,
            hard_system_disk: self.hard_system_disk,
            hard_type: self.hard_type.name(),
            hard_capacity_in_use: self.hard_capacity_in_use, // <- tambahan
            soft_read_speed: self.soft_read_speed,
            soft_write_speed: self.soft_write_speed,
            soft_active_time: self.soft_active_time,
            soft_average_response_time: self.soft_average_response_time,
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
    ram_util: RamUtilData,
    disk_util: DiskUtilData,
}

impl MutatorWatcher {
    pub fn new(
        store: Arc<dyn ActivityStore>,
        notify: broadcast::Sender<ActivityEvent>,
        interval: Duration,
        key_counter: KeyCounter,
        process_window: usize,
        cpu_util: CpuUtilData,
        ram_util: RamUtilData,
        disk_util: DiskUtilData
    ) -> Self {
        Self { store, notify, interval, key_counter, process_window, cpu_util, ram_util, disk_util }
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

            {
                self.ram_util.update();
                let event_ram = separation::ActivityEvent {
                    timestamp: Utc::now(),
                    kind: separation::ActivityKind::Ram(self.ram_util.to_snapshot()),
                };
                self.store.save(event_ram.clone()).await?;
                let _ = self.notify.send(event_ram);
            }

            {
                self.disk_util.update();
                let event_disk = separation::ActivityEvent {
                    timestamp: Utc::now(),
                    kind: separation::ActivityKind::Disk(self.disk_util.to_snapshot()),
                };
                self.store.save(event_disk.clone()).await?;
                let _ = self.notify.send(event_disk);
            }
        }
    }
}