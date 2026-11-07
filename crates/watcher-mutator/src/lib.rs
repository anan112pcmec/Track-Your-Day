use chrono::Utc;
use rdev::{listen, EventType};
use separation::{ActivityEvent, ActivityKind, ActivityStore, PowerSnapshot, AudioSnapshot};
use std::ffi::c_void;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use active_win_pos_rs::get_active_window;
use windows::Win32::NetworkManagement::IpHelper::{
    GetAdaptersAddresses, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER,
    GAA_FLAG_SKIP_MULTICAST, IP_ADAPTER_ADDRESSES_LH,
};
use windows::Win32::NetworkManagement::WiFi::{
    dot11_phy_type_dmg, dot11_phy_type_eht, dot11_phy_type_erp, dot11_phy_type_he,
    dot11_phy_type_hrdsss, dot11_phy_type_ht, dot11_phy_type_ofdm, dot11_phy_type_vht,
    wlan_interface_state_connected, wlan_intf_opcode_current_connection, WlanCloseHandle,
    WlanEnumInterfaces, WlanFreeMemory, WlanOpenHandle, WlanQueryInterface,
    WLAN_CONNECTION_ATTRIBUTES, WLAN_INTERFACE_INFO_LIST,
};
use windows::Win32::Networking::WinSock::{AF_INET, AF_INET6, AF_UNSPEC};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows::Win32::System::SystemInformation::GetTickCount;

pub struct KeyCounter {
    count: Arc<AtomicUsize>,
}

impl KeyCounter {
    pub fn start() -> Self {
        let count: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
        let counter_for_thread = count.clone();

        std::thread::spawn(move || {
            let callback = move |event: rdev::Event| {
                if let EventType::KeyPress(_) = event.event_type {
                    counter_for_thread.fetch_add(1, Ordering::Relaxed);
                }
            };

            if let Err(err) = listen(callback) {
                eprintln!("[watcher-mutator] gagal pasang keyboard hook: {err:?}");
            }
        });

        Self { count }
    }

    pub fn take_and_reset(&self) -> usize {
        self.count.swap(0, Ordering::Relaxed)
    }
}

#[derive(Debug)]
pub struct CpuUtilData {
    hard_base_speed: f32,
    hard_sockets: u8,
    hard_cores: u16,
    hard_logical_processors: u8,
    hard_virtualization: bool,
    hard_l1_cache: f32,
    hard_l2_cache: f32,
    hard_l3_cache: f32,

    soft_utilization: f32,
    soft_speed_clock: f32,
    soft_processes: u32,
    soft_threads: u32,
    soft_handles: u64,
    soft_uptime: String,

    last_idle: u64,
    last_kernel: u64,
    last_user: u64,
}

impl CpuUtilData {
    pub fn new() -> Self {
        let mut data = CpuUtilData {
            hard_base_speed: 0.0,
            hard_sockets: 0,
            hard_cores: 0,
            hard_logical_processors: 0,
            hard_virtualization: false,
            hard_l1_cache: 0.0,
            hard_l2_cache: 0.0,
            hard_l3_cache: 0.0,

            soft_utilization: 0.0,
            soft_speed_clock: 0.0,
            soft_processes: 0,
            soft_threads: 0,
            soft_handles: 0,
            soft_uptime: String::new(),

            last_idle: 0,
            last_kernel: 0,
            last_user: 0,
        };
        data.init_hardware_specs();
        data
    }

    fn init_hardware_specs(&mut self) {
        unsafe {
            let mut sys_info = windows::Win32::System::SystemInformation::SYSTEM_INFO::default();
            windows::Win32::System::SystemInformation::GetSystemInfo(&mut sys_info);
            self.hard_logical_processors = sys_info.dwNumberOfProcessors as u8;
        }

        unsafe {
            self.hard_virtualization = windows::Win32::System::Threading::IsProcessorFeaturePresent(
                windows::Win32::System::Threading::PF_VIRT_FIRMWARE_ENABLED,
            )
            .as_bool();
        }

        unsafe {
            let mut buf_len: u32 = 0;
            let _ = windows::Win32::System::SystemInformation::GetLogicalProcessorInformation(
                None,
                &mut buf_len,
            );

            if buf_len > 0 {
                let count = buf_len as usize
                    / std::mem::size_of::<
                        windows::Win32::System::SystemInformation::SYSTEM_LOGICAL_PROCESSOR_INFORMATION,
                    >();
                let mut buffer: std::vec::Vec<
                    windows::Win32::System::SystemInformation::SYSTEM_LOGICAL_PROCESSOR_INFORMATION,
                > = vec![
                    windows::Win32::System::SystemInformation::SYSTEM_LOGICAL_PROCESSOR_INFORMATION::default();
                    count
                ];

                if windows::Win32::System::SystemInformation::GetLogicalProcessorInformation(
                    Some(buffer.as_mut_ptr()),
                    &mut buf_len,
                )
                .is_ok()
                {
                    let mut sockets = 0u8;
                    let mut cores = 0u16;
                    let (mut l1, mut l2, mut l3) = (0.0f32, 0.0f32, 0.0f32);

                    for entry in &buffer {
                        match entry.Relationship {
                            windows::Win32::System::SystemInformation::RelationProcessorPackage => {
                                sockets += 1;
                            }
                            windows::Win32::System::SystemInformation::RelationProcessorCore => {
                                cores += 1;
                            }
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

                    self.hard_sockets = sockets;
                    self.hard_cores = cores;
                    self.hard_l1_cache = l1;
                    self.hard_l2_cache = l2;
                    self.hard_l3_cache = l3;
                }
            }
        }
    }

    pub fn update(&mut self) {
        unsafe {
            let mut perf_info =
                windows::Win32::System::ProcessStatus::PERFORMANCE_INFORMATION::default();
            let size = std::mem::size_of::<
                windows::Win32::System::ProcessStatus::PERFORMANCE_INFORMATION,
            >() as u32;
            if windows::Win32::System::ProcessStatus::GetPerformanceInfo(&mut perf_info, size).is_ok()
            {
                self.soft_processes = perf_info.ProcessCount;
                self.soft_threads = perf_info.ThreadCount;
                self.soft_handles = perf_info.HandleCount as u64;
            }
        }

        unsafe {
            let uptime_ms = windows::Win32::System::SystemInformation::GetTickCount64();
            let total_secs = uptime_ms / 1000;
            let h = total_secs / 3600;
            let m = (total_secs % 3600) / 60;
            let s = total_secs % 60;
            self.soft_uptime = format!("{h:02}:{m:02}:{s:02}");
        }

        unsafe {
            let logical_count = std::cmp::max(self.hard_logical_processors as usize, 1);
            let mut infos: std::vec::Vec<windows::Win32::System::Power::PROCESSOR_POWER_INFORMATION> =
                vec![windows::Win32::System::Power::PROCESSOR_POWER_INFORMATION::default(); logical_count];
            let buf_size = (std::mem::size_of::<
                windows::Win32::System::Power::PROCESSOR_POWER_INFORMATION,
            >() * infos.len()) as u32;

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
                self.soft_speed_clock = avg_current;
                self.hard_base_speed = infos[0].MaxMhz as f32;
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
            )
            .is_ok()
            {
                let idle = filetime_to_u64(&idle_time);
                let kernel = filetime_to_u64(&kernel_time);
                let user = filetime_to_u64(&user_time);

                let idle_diff = idle.saturating_sub(self.last_idle);
                let kernel_diff = kernel.saturating_sub(self.last_kernel);
                let user_diff = user.saturating_sub(self.last_user);
                let total_diff = kernel_diff + user_diff;

                if total_diff > 0 {
                    let busy_diff = total_diff.saturating_sub(idle_diff);
                    self.soft_utilization = (busy_diff as f64 / total_diff as f64 * 100.0) as f32;
                }

                self.last_idle = idle;
                self.last_kernel = kernel;
                self.last_user = user;
            }
        }
    }

    pub fn to_snapshot(&self) -> separation::CpuSnapshot {
        separation::CpuSnapshot {
            hard_base_speed: self.hard_base_speed,
            hard_sockets: self.hard_sockets,
            hard_cores: self.hard_cores,
            hard_logical_processors: self.hard_logical_processors,
            hard_virtualization: self.hard_virtualization,
            hard_l1_cache: self.hard_l1_cache,
            hard_l2_cache: self.hard_l2_cache,
            hard_l3_cache: self.hard_l3_cache,
            soft_utilization: self.soft_utilization,
            soft_speed_clock: self.soft_speed_clock,
            soft_processes: self.soft_processes,
            soft_threads: self.soft_threads,
            soft_handles: self.soft_handles,
            soft_uptime: self.soft_uptime.clone(),
        }
    }
}

#[derive(Debug)]
pub struct RamUtilData {
    hard_capacity: f32,
    hard_speed: u32,
    hard_slots_used: u16,
    hard_form_factor: &'static str,
    soft_hardware_reserve: f32,
    soft_in_use: f32,
    soft_available: f32,
    soft_in_commited: f32,
    soft_available_commited: f32,
    soft_cached: f32,
    soft_page_pool: f32,
    soft_non_paged_pool: f32,
}

impl RamUtilData {
    pub fn new() -> Self {
        let mut data = RamUtilData {
            hard_capacity: 0.0,
            hard_speed: 0,
            hard_slots_used: 0,
            hard_form_factor: "Unknown",
            soft_hardware_reserve: 0.0,
            soft_in_use: 0.0,
            soft_available: 0.0,
            soft_in_commited: 0.0,
            soft_available_commited: 0.0,
            soft_cached: 0.0,
            soft_page_pool: 0.0,
            soft_non_paged_pool: 0.0,
        };
        data.init_hardware_specs();
        data
    }

    fn init_hardware_specs(&mut self) {
        const BYTES_TO_GB: f32 = 1024.0 * 1024.0 * 1024.0;
        let mut installed_kb: u64 = 0;
        unsafe {
            let _ = windows::Win32::System::SystemInformation::GetPhysicallyInstalledSystemMemory(
                &mut installed_kb,
            );
        }
        self.hard_capacity = (installed_kb as f32 * 1024.0) / BYTES_TO_GB;

        unsafe {
            const RSMB_SIGNATURE: u32 = 0x52534D42;
            let provider =
                windows::Win32::System::SystemInformation::FIRMWARE_TABLE_PROVIDER(RSMB_SIGNATURE);
            let needed_size = windows::Win32::System::SystemInformation::GetSystemFirmwareTable(
                provider, 0, None,
            );

            if needed_size > 0 {
                let mut buffer: std::vec::Vec<u8> = vec![0u8; needed_size as usize];
                let actual_size = windows::Win32::System::SystemInformation::GetSystemFirmwareTable(
                    provider, 0, Some(&mut buffer),
                );

                if actual_size > 0 {
                    let table_data = &buffer[8..];
                    let mut offset = 0usize;
                    let mut slots_used = 0u16;
                    let mut speeds_found: std::vec::Vec<u16> = Vec::new();
                    let mut form_factor: &'static str = "Unknown";

                    while offset + 4 <= table_data.len() {
                        let struct_type = table_data[offset];
                        let struct_length = table_data[offset + 1] as usize;

                        if struct_length < 4 || offset + struct_length > table_data.len() {
                            break;
                        }

                        if struct_type == 17 && struct_length >= 0x15 {
                            let size_raw = u16::from_le_bytes([
                                table_data[offset + 0x0C],
                                table_data[offset + 0x0D],
                            ]);
                            if size_raw != 0 && size_raw != 0xFFFF {
                                slots_used += 1;
                                if form_factor == "Unknown" {
                                    form_factor = match table_data[offset + 0x0E] {
                                        0x09 => "DIMM",
                                        0x0D => "SODIMM",
                                        0x08 => "SIMM",
                                        _ => "Other",
                                    };
                                }
                                if struct_length >= 0x17 {
                                    let speed = u16::from_le_bytes([
                                        table_data[offset + 0x15],
                                        table_data[offset + 0x16],
                                    ]);
                                    if speed > 0 {
                                        speeds_found.push(speed);
                                    }
                                }
                            }
                        }

                        let mut cursor = offset + struct_length;
                        while cursor + 1 < table_data.len()
                            && !(table_data[cursor] == 0 && table_data[cursor + 1] == 0)
                        {
                            cursor += 1;
                        }
                        offset = cursor + 2;
                    }

                    self.hard_slots_used = slots_used;
                    self.hard_form_factor = form_factor;
                    if !speeds_found.is_empty() {
                        self.hard_speed = speeds_found.iter().map(|&s| s as u32).sum::<u32>()
                            / speeds_found.len() as u32;
                    }
                }
            }
        }
    }

    pub fn update(&mut self) {
        const BYTES_TO_GB: f32 = 1024.0 * 1024.0 * 1024.0;
        unsafe {
            let mut mem_status =
                windows::Win32::System::SystemInformation::MEMORYSTATUSEX::default();
            mem_status.dwLength =
                std::mem::size_of::<windows::Win32::System::SystemInformation::MEMORYSTATUSEX>()
                    as u32;

            if windows::Win32::System::SystemInformation::GlobalMemoryStatusEx(&mut mem_status).is_ok()
            {
                let total_phys_gb = mem_status.ullTotalPhys as f32 / BYTES_TO_GB;
                let avail_phys_gb = mem_status.ullAvailPhys as f32 / BYTES_TO_GB;
                self.soft_in_use = total_phys_gb - avail_phys_gb;
                self.soft_available = avail_phys_gb;
                self.soft_hardware_reserve = (self.hard_capacity - total_phys_gb).max(0.0);

                let total_commit_gb = mem_status.ullTotalPageFile as f32 / BYTES_TO_GB;
                let avail_commit_gb = mem_status.ullAvailPageFile as f32 / BYTES_TO_GB;
                self.soft_in_commited = total_commit_gb - avail_commit_gb;
                self.soft_available_commited = avail_commit_gb;
            }
        }

        unsafe {
            let mut perf_info =
                windows::Win32::System::ProcessStatus::PERFORMANCE_INFORMATION::default();
            let size = std::mem::size_of::<
                windows::Win32::System::ProcessStatus::PERFORMANCE_INFORMATION,
            >() as u32;
            if windows::Win32::System::ProcessStatus::GetPerformanceInfo(&mut perf_info, size).is_ok()
            {
                let page_size = perf_info.PageSize as f32;
                self.soft_cached = (perf_info.SystemCache as f32 * page_size) / BYTES_TO_GB;
                self.soft_page_pool = (perf_info.KernelPaged as f32 * page_size) / BYTES_TO_GB;
                self.soft_non_paged_pool =
                    (perf_info.KernelNonpaged as f32 * page_size) / BYTES_TO_GB;
            }
        }
    }

    pub fn to_snapshot(&self) -> separation::RamSnapshot {
        separation::RamSnapshot {
            hard_capacity: self.hard_capacity,
            hard_speed: self.hard_speed,
            hard_slots_used: self.hard_slots_used,
            hard_form_factor: self.hard_form_factor.to_string(),
            soft_hardware_reserve: self.soft_hardware_reserve,
            soft_in_use: self.soft_in_use,
            soft_available: self.soft_available,
            soft_in_commited: self.soft_in_commited,
            soft_available_commited: self.soft_available_commited,
            soft_cached: self.soft_cached,
            soft_page_pool: self.soft_page_pool,
            soft_non_paged_pool: self.soft_non_paged_pool,
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
            hard_system_disk: true,
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
        let drive_path: std::vec::Vec<u16> = "\\\\.\\PhysicalDrive0\0".encode_utf16().collect();
        unsafe {
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
                let query = windows::Win32::System::Ioctl::STORAGE_PROPERTY_QUERY {
                    PropertyId: windows::Win32::System::Ioctl::StorageDeviceSeekPenaltyProperty,
                    QueryType: windows::Win32::System::Ioctl::PropertyStandardQuery,
                    AdditionalParameters: [0],
                };
                let mut seek_penalty =
                    windows::Win32::System::Ioctl::DEVICE_SEEK_PENALTY_DESCRIPTOR::default();
                let mut bytes_returned: u32 = 0;

                let seek_result = windows::Win32::System::IO::DeviceIoControl(
                    handle,
                    windows::Win32::System::Ioctl::IOCTL_STORAGE_QUERY_PROPERTY,
                    Some(&query as *const _ as *const std::ffi::c_void),
                    std::mem::size_of::<windows::Win32::System::Ioctl::STORAGE_PROPERTY_QUERY>()
                        as u32,
                    Some(&mut seek_penalty as *mut _ as *mut std::ffi::c_void),
                    std::mem::size_of::<
                        windows::Win32::System::Ioctl::DEVICE_SEEK_PENALTY_DESCRIPTOR,
                    >() as u32,
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

                let mut perf = windows::Win32::System::Ioctl::DISK_PERFORMANCE::default();
                let mut perf_bytes_returned: u32 = 0;
                let perf_result = windows::Win32::System::IO::DeviceIoControl(
                    handle,
                    windows::Win32::System::Ioctl::IOCTL_DISK_PERFORMANCE,
                    None, 0,
                    Some(&mut perf as *mut _ as *mut std::ffi::c_void),
                    std::mem::size_of::<windows::Win32::System::Ioctl::DISK_PERFORMANCE>() as u32,
                    Some(&mut perf_bytes_returned),
                    None,
                );

                if perf_result.is_ok() {
                    let query_diff = perf.QueryTime - self.last_query_time;
                    if query_diff > 0 {
                        let elapsed_secs = query_diff as f64 / 10_000_000.0;
                        let bytes_read_diff = (perf.BytesRead - self.last_bytes_read).max(0) as f64;
                        let bytes_written_diff =
                            (perf.BytesWritten - self.last_bytes_written).max(0) as f64;
                        self.soft_read_speed =
                            (bytes_read_diff / elapsed_secs / (1024.0 * 1024.0)) as f32;
                        self.soft_write_speed =
                            (bytes_written_diff / elapsed_secs / (1024.0 * 1024.0)) as f32;

                        let idle_diff = (perf.IdleTime - self.last_idle_time).max(0) as f64;
                        let active_ratio = 1.0 - (idle_diff / query_diff as f64);
                        self.soft_active_time = (active_ratio * 100.0).clamp(0.0, 100.0) as f32;

                        let read_time_diff = (perf.ReadTime - self.last_read_time).max(0) as f64;
                        let write_time_diff = (perf.WriteTime - self.last_write_time).max(0) as f64;
                        let read_count_diff =
                            perf.ReadCount.saturating_sub(self.last_read_count) as f64;
                        let write_count_diff =
                            perf.WriteCount.saturating_sub(self.last_write_count) as f64;
                        let total_io_time = read_time_diff + write_time_diff;
                        let total_io_count = read_count_diff + write_count_diff;

                        if total_io_count > 0.0 {
                            self.soft_average_response_time =
                                ((total_io_time / total_io_count) / 10_000.0) as f32;
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

        unsafe {
            let system_drive: std::vec::Vec<u16> = "C:\\\0".encode_utf16().collect();
            let mut total_bytes: u64 = 0;
            let mut free_bytes: u64 = 0;
            if windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
                windows::core::PCWSTR(system_drive.as_ptr()),
                None, Some(&mut total_bytes), Some(&mut free_bytes),
            )
            .is_ok()
            {
                self.hard_formatted = (total_bytes / (1024 * 1024)) as u32;
                self.hard_capacity_in_use =
                    ((total_bytes.saturating_sub(free_bytes)) / (1024 * 1024)) as u32;
            }
        }
    }

    pub fn to_snapshot(&self) -> separation::DiskSnapshot {
        separation::DiskSnapshot {
            hard_capacity: self.hard_capacity,
            hard_formatted: self.hard_formatted,
            hard_system_disk: self.hard_system_disk,
            hard_type: self.hard_type.name(),
            hard_capacity_in_use: self.hard_capacity_in_use,
            soft_read_speed: self.soft_read_speed,
            soft_write_speed: self.soft_write_speed,
            soft_active_time: self.soft_active_time,
            soft_average_response_time: self.soft_average_response_time,
        }
    }
}

#[derive(Debug)]
pub struct WiFiUtilData {
    hard_name_wireless_lan: String,
    hard_throughput: f32,
    hard_send: f32,
    hard_receive: f32,
    soft_adapter_name: String,
    soft_ssid: String,
    soft_connection_type: String,
    soft_ipv4_address: String,
    soft_ipv6_address: String,
    soft_signal_strength: u32,
    last_bytes_received: u64,
    last_bytes_sent: u64,
    last_tick_time: Instant,
}

impl WiFiUtilData {
    pub fn new() -> Self {
        WiFiUtilData {
            hard_name_wireless_lan: "Unknown".to_string(),
            hard_throughput: 0.0,
            hard_send: 0.0,
            hard_receive: 0.0,
            soft_adapter_name: "Unknown".to_string(),
            soft_ssid: "Not connected".to_string(),
            soft_connection_type: "-".to_string(),
            soft_ipv4_address: "-".to_string(),
            soft_ipv6_address: "-".to_string(),
            soft_signal_strength: 0,
            last_bytes_received: 0,
            last_bytes_sent: 0,
            last_tick_time: Instant::now(),
        }
    }

    pub fn update(&mut self) {
        unsafe {
            let mut negotiated_version: u32 = 0;
            let mut client_handle = windows::Win32::Foundation::HANDLE::default();
            let open_result = WlanOpenHandle(2, None, &mut negotiated_version, &mut client_handle);
            if open_result != 0 {
                return;
            }

            let mut interface_list_ptr: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
            let enum_result = WlanEnumInterfaces(client_handle, None, &mut interface_list_ptr);
            if enum_result != 0 || interface_list_ptr.is_null() {
                let _ = WlanCloseHandle(client_handle, None);
                return;
            }

            let interface_list = &*interface_list_ptr;
            if interface_list.dwNumberOfItems == 0 {
                WlanFreeMemory(interface_list_ptr as *const c_void);
                let _ = WlanCloseHandle(client_handle, None);
                return;
            }

            let first_interface = &interface_list.InterfaceInfo[0];
            let interface_guid = first_interface.InterfaceGuid;

            let desc_len = first_interface
                .strInterfaceDescription
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(first_interface.strInterfaceDescription.len());
            let adapter_description =
                String::from_utf16_lossy(&first_interface.strInterfaceDescription[..desc_len]);

            self.hard_name_wireless_lan = adapter_description.clone();
            self.soft_adapter_name = adapter_description;

            if first_interface.isState == wlan_interface_state_connected {
                let mut data_size: u32 = 0;
                let mut data_ptr: *mut c_void = std::ptr::null_mut();
                let query_result = WlanQueryInterface(
                    client_handle, &interface_guid, wlan_intf_opcode_current_connection,
                    None, &mut data_size, &mut data_ptr, None,
                );

                if query_result == 0 && !data_ptr.is_null() {
                    let conn_attrs = &*(data_ptr as *const WLAN_CONNECTION_ATTRIBUTES);
                    let assoc = &conn_attrs.wlanAssociationAttributes;
                    let ssid_len = assoc.dot11Ssid.uSSIDLength as usize;
                    let ssid_bytes = &assoc.dot11Ssid.ucSSID[..ssid_len.min(32)];
                    self.soft_ssid = String::from_utf8_lossy(ssid_bytes).to_string();
                    self.soft_signal_strength = assoc.wlanSignalQuality;
                    self.soft_connection_type = match assoc.dot11PhyType {
                        dot11_phy_type_hrdsss => "802.11b".to_string(),
                        dot11_phy_type_ofdm => "802.11a".to_string(),
                        dot11_phy_type_erp => "802.11g".to_string(),
                        dot11_phy_type_ht => "802.11n".to_string(),
                        dot11_phy_type_vht => "802.11ac".to_string(),
                        dot11_phy_type_he => "802.11ax".to_string(),
                        dot11_phy_type_eht => "802.11be".to_string(),
                        dot11_phy_type_dmg => "802.11ad".to_string(),
                        _ => "Unknown".to_string(),
                    };
                } else {
                    self.reset_connection_fields();
                }
            } else {
                self.reset_connection_fields();
            }

            WlanFreeMemory(interface_list_ptr as *const c_void);
            let _ = WlanCloseHandle(client_handle, None);
            self.fetch_network_statistics_and_ip(&interface_guid);
        }
    }

    fn reset_connection_fields(&mut self) {
        self.soft_ssid = "Not connected".to_string();
        self.soft_connection_type = "-".to_string();
        self.soft_signal_strength = 0;
        self.hard_receive = 0.0;
        self.hard_send = 0.0;
        self.hard_throughput = 0.0;
    }

    unsafe fn fetch_network_statistics_and_ip(&mut self, interface_guid: &windows::core::GUID) {
        let target_guid_str = format!("{{{:?}}}", interface_guid).to_uppercase();
        let mut buffer_size: u32 = 15_000;
        let mut buffer: Vec<u8>;
        let mut result;
        loop {
            buffer = vec![0u8; buffer_size as usize];
            let addresses_ptr = buffer.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH;
            result = GetAdaptersAddresses(
                AF_UNSPEC.0 as u32,
                GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER,
                None, Some(addresses_ptr), &mut buffer_size,
            );
            if result == 111 { continue; }
            break;
        }

        if result != 0 {
            self.soft_ipv4_address = "-".to_string();
            self.soft_ipv6_address = "-".to_string();
            return;
        }

        let elapsed_secs = self.last_tick_time.elapsed().as_secs_f64();
        self.last_tick_time = Instant::now();
        if elapsed_secs <= 0.0 { return; }

        let mut current = buffer.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        let mut found_ipv4 = false;
        let mut found_ipv6 = false;

        while !current.is_null() {
            let adapter = &*current;
            let adapter_name = if adapter.AdapterName.0.is_null() {
                String::new()
            } else {
                std::ffi::CStr::from_ptr(adapter.AdapterName.0 as *const i8)
                    .to_string_lossy()
                    .to_uppercase()
            };

            if adapter_name == target_guid_str {
                let stats = adapter.Anonymous1.Anonymous.Stats;
                let current_bytes_received = stats.ipReceiveBytes;
                let current_bytes_sent = stats.ipTransmitBytes;

                if self.last_bytes_received > 0 && current_bytes_received >= self.last_bytes_received {
                    let rx_diff = (current_bytes_received - self.last_bytes_received) as f64;
                    self.hard_receive = ((rx_diff * 8.0) / elapsed_secs / 1_000_000.0) as f32;
                } else {
                    self.hard_receive = 0.0;
                }

                if self.last_bytes_sent > 0 && current_bytes_sent >= self.last_bytes_sent {
                    let tx_diff = (current_bytes_sent - self.last_bytes_sent) as f64;
                    self.hard_send = ((tx_diff * 8.0) / elapsed_secs / 1_000_000.0) as f32;
                } else {
                    self.hard_send = 0.0;
                }

                self.last_bytes_received = current_bytes_received;
                self.last_bytes_sent = current_bytes_sent;
                self.hard_throughput = self.hard_receive + self.hard_send;

                let mut unicast = adapter.FirstUnicastAddress;
                while !unicast.is_null() {
                    let ua = &*unicast;
                    let sockaddr = ua.Address.lpSockaddr;
                    if !sockaddr.is_null() {
                        let family = (*sockaddr).sa_family;
                        if family == AF_INET && !found_ipv4 {
                            let sockaddr_in =
                                &*(sockaddr as *const windows::Win32::Networking::WinSock::SOCKADDR_IN);
                            let ip_bytes = sockaddr_in.sin_addr.S_un.S_addr.to_le_bytes();
                            let ip = Ipv4Addr::new(ip_bytes[0], ip_bytes[1], ip_bytes[2], ip_bytes[3]);
                            self.soft_ipv4_address = ip.to_string();
                            found_ipv4 = true;
                        } else if family == AF_INET6 && !found_ipv6 {
                            let sockaddr_in6 = &*(sockaddr
                                as *const windows::Win32::Networking::WinSock::SOCKADDR_IN6);
                            let ip = Ipv6Addr::from(sockaddr_in6.sin6_addr.u.Byte);
                            self.soft_ipv6_address = ip.to_string();
                            found_ipv6 = true;
                        }
                    }
                    unicast = ua.Next;
                }
                break;
            }
            current = adapter.Next;
        }

        if !found_ipv4 { self.soft_ipv4_address = "-".to_string(); }
        if !found_ipv6 { self.soft_ipv6_address = "-".to_string(); }
    }

    pub fn to_snapshot(&self) -> separation::WiFiSnapshot {
        separation::WiFiSnapshot {
            hard_name_wireless_lan: self.hard_name_wireless_lan.clone(),
            hard_throughput: self.hard_throughput,
            hard_send: self.hard_send,
            hard_receive: self.hard_receive,
            soft_adapter_name: self.soft_adapter_name.clone(),
            soft_ssid: self.soft_ssid.clone(),
            soft_connection_type: self.soft_connection_type.clone(),
            soft_ipv4_address: self.soft_ipv4_address.clone(),
            soft_ipv6_address: self.soft_ipv6_address.clone(),
            soft_signal_strength: self.soft_signal_strength,
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
    wifi_util: WiFiUtilData,
    last_clipboard_seq: u32,
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
        disk_util: DiskUtilData,
        wifi_util: WiFiUtilData,
    ) -> Self {
        Self {
            store,
            notify,
            interval,
            key_counter,
            process_window,
            cpu_util,
            ram_util,
            disk_util,
            wifi_util,
            last_clipboard_seq: 0,
        }
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
        let mut tick = tokio::time::interval(self.interval);
        loop {
            tick.tick().await;

            // 1) Typing Speed (WPM)
            {
                let keystrokes = self.key_counter.take_and_reset();
                let elapsed_minutes = self.interval.as_secs_f64() / 60.0;
                let wpm = ((keystrokes as f64 / 5.0) / elapsed_minutes).round() as u32;

                let event_wpm = ActivityEvent {
                    timestamp: Utc::now(),
                    kind: ActivityKind::TypingSpeed { wpm },
                };
                let _ = self.store.save(event_wpm.clone()).await;
                let _ = self.notify.send(event_wpm);
            }

            // 2) Active Window
            {
                let event_window = match get_active_window() {
                    Ok(window) => ActivityEvent {
                        timestamp: Utc::now(),
                        kind: ActivityKind::ActiveWindow {
                            app: window.process_name,
                            title: window.title,
                        },
                    },
                    Err(_) => ActivityEvent {
                        timestamp: Utc::now(),
                        kind: ActivityKind::ActiveWindow {
                            app: "unknown".into(),
                            title: "unknown".into(),
                        },
                    },
                };
                let _ = self.store.save(event_window.clone()).await;
                let _ = self.notify.send(event_window);
            }

            // 3) CPU & Total Process
            {
                self.cpu_util.update();
                let event_cpu = ActivityEvent {
                    timestamp: Utc::now(),
                    kind: ActivityKind::Cpu(self.cpu_util.to_snapshot()),
                };
                let _ = self.store.save(event_cpu.clone()).await;
                let _ = self.notify.send(event_cpu);

                let total_processes = self.cpu_util.soft_processes as usize;
                let event_process = ActivityEvent {
                    timestamp: Utc::now(),
                    kind: ActivityKind::TotalProcess { process: total_processes },
                };
                let _ = self.store.save(event_process.clone()).await;
                let _ = self.notify.send(event_process);
            }

            // 4) RAM Util
            {
                self.ram_util.update();
                let event_ram = ActivityEvent {
                    timestamp: Utc::now(),
                    kind: ActivityKind::Ram(self.ram_util.to_snapshot()),
                };
                let _ = self.store.save(event_ram.clone()).await;
                let _ = self.notify.send(event_ram);
            }

            // 5) Disk Util
            {
                self.disk_util.update();
                let event_disk = ActivityEvent {
                    timestamp: Utc::now(),
                    kind: ActivityKind::Disk(self.disk_util.to_snapshot()),
                };
                let _ = self.store.save(event_disk.clone()).await;
                let _ = self.notify.send(event_disk);
            }

            // 6) WiFi Util
            {
                self.wifi_util.update();
                let event_wifi = ActivityEvent {
                    timestamp: Utc::now(),
                    kind: ActivityKind::WiFi(self.wifi_util.to_snapshot()),
                };
                let _ = self.store.save(event_wifi.clone()).await;
                let _ = self.notify.send(event_wifi);
            }

            // 7) Feature 1: User Idle Time (GetLastInputInfo)
            {
                let mut lii = LASTINPUTINFO {
                    cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
                    dwTime: 0,
                };
                let idle_secs = unsafe {
                    if GetLastInputInfo(&mut lii).as_bool() {
                        let tick_count = GetTickCount();
                        let elapsed_ms = tick_count.saturating_sub(lii.dwTime);
                        (elapsed_ms / 1000) as u64
                    } else {
                        0
                    }
                };
                let event_idle = ActivityEvent {
                    timestamp: Utc::now(),
                    kind: ActivityKind::IdleTime { idle_secs },
                };
                let _ = self.store.save(event_idle.clone()).await;
                let _ = self.notify.send(event_idle);
            }

            // 8) Feature 2: Power & Battery Details (GetSystemPowerStatus)
            {
                let mut sps = SYSTEM_POWER_STATUS::default();
                unsafe {
                    let _ = GetSystemPowerStatus(&mut sps);
                }
                let power_snap = PowerSnapshot {
                    ac_line_status: sps.ACLineStatus,
                    battery_flag: sps.BatteryFlag,
                    battery_life_percent: sps.BatteryLifePercent,
                    battery_life_time: sps.BatteryLifeTime,
                    battery_saver_active: sps.SystemStatusFlag != 0,
                };
                let event_power = ActivityEvent {
                    timestamp: Utc::now(),
                    kind: ActivityKind::Power(power_snap),
                };
                let _ = self.store.save(event_power.clone()).await;
                let _ = self.notify.send(event_power);
            }

            // 9) Feature 3: Audio Status (Placeholder / Default Windows State)
            {
                let audio_snap = AudioSnapshot {
                    master_volume: 65.0,
                    is_muted: false,
                };
                let event_audio = ActivityEvent {
                    timestamp: Utc::now(),
                    kind: ActivityKind::Audio(audio_snap),
                };
                let _ = self.store.save(event_audio.clone()).await;
                let _ = self.notify.send(event_audio);
            }

            // 10) Feature 6: Clipboard Activity Sequence Check
            {
                unsafe {
                    let seq = windows::Win32::System::DataExchange::GetClipboardSequenceNumber();
                    if self.last_clipboard_seq == 0 {
                        self.last_clipboard_seq = seq;
                    } else if seq != self.last_clipboard_seq {
                        self.last_clipboard_seq = seq;
                        let event_clip = ActivityEvent {
                            timestamp: Utc::now(),
                            kind: ActivityKind::ClipboardActivity { updates: 1 },
                        };
                        let _ = self.store.save(event_clip.clone()).await;
                        let _ = self.notify.send(event_clip);
                    }
                }
            }
        }
    }
}
