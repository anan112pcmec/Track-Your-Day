//! `cli` — main logic & TUI interface "track your day".

use anyhow::Result;
use database::MemoryStore;
use separation::{ActivityStore, CpuSnapshot, DiskSnapshot, RamSnapshot, WiFiSnapshot};
use std::collections::VecDeque;
use std::io::stdout;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use watcher_mutator::{CpuUtilData, DiskUtilData, MutatorWatcher, RamUtilData, WiFiUtilData};
use watcher_reactive::{LoggingReactor, ReactiveWatcher};

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    prelude::*,
    widgets::{Block, Gauge, Paragraph},
};
use ratatui::symbols::Marker;
use ratatui::widgets::{Axis, Chart, Dataset, GraphType, List, ListItem};

/// Guard RAII untuk memastikan state terminal selalu dikembalikan ke normal saat exit / panic
struct TerminalGuard;

impl TerminalGuard {
    fn new() -> Result<Self> {
        enable_raw_mode()?;
        stdout().execute(EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = stdout().execute(LeaveAlternateScreen);
    }
}

#[derive(PartialEq, Clone, Copy)]
enum ActiveTab {
    Activity,
    Performance,
}

#[derive(PartialEq, Clone, Copy)]
enum PerfMenu {
    Cpu,
    Memory,
    Disk,
    Wifi,
}

impl PerfMenu {
    fn next(self) -> Self {
        match self {
            PerfMenu::Cpu => PerfMenu::Memory,
            PerfMenu::Memory => PerfMenu::Disk,
            PerfMenu::Disk => PerfMenu::Wifi,
            PerfMenu::Wifi => PerfMenu::Wifi,
        }
    }

    fn prev(self) -> Self {
        match self {
            PerfMenu::Cpu => PerfMenu::Cpu,
            PerfMenu::Memory => PerfMenu::Cpu,
            PerfMenu::Disk => PerfMenu::Memory,
            PerfMenu::Wifi => PerfMenu::Disk,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // 1) DATABASE — MemoryStore dipegang via ActivityStore trait
    let store: Arc<dyn ActivityStore> = Arc::new(MemoryStore::new());

    // 2) Broadcast channel
    let (tx, rx) = broadcast::channel(64);

    // 3) WATCHER-MUTATOR — spawn background task (menulis data tiap 2 detik)
    let mutator = MutatorWatcher::new(
        store.clone(),
        tx.clone(),
        Duration::from_secs(2),
        watcher_mutator::KeyCounter::start(),
        0,
        CpuUtilData::new(),
        RamUtilData::new(),
        DiskUtilData::new(),
        WiFiUtilData::new(),
    );
    let mutator_handle = tokio::spawn(mutator.run());

    // 4) WATCHER-REACTIVE — spawn background task
    let reactive = ReactiveWatcher::new(rx, Box::new(LoggingReactor { verbose: false }));
    let reactive_handle = tokio::spawn(reactive.run());

    // 5) API STATE
    let api_state = api::ApiState::new(store.clone());

    // 6) INITIALIZE RATATUI TERMINAL
    let _guard = TerminalGuard::new()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    // Timer & Ticker
    let start_time = Instant::now();
    let mut render_interval = tokio::time::interval(Duration::from_millis(50));
    let mut last_fetch_time = Instant::now() - Duration::from_secs(5); // fetch segera saat start

    let mut active_tab = ActiveTab::Activity;
    let mut perf_menu = PerfMenu::Cpu;

    const HISTORY_LEN: usize = 40;
    let mut window_history: VecDeque<String> = VecDeque::with_capacity(10);
    let mut cpu_history: VecDeque<u64> = VecDeque::with_capacity(HISTORY_LEN);
    let mut ram_history: VecDeque<u64> = VecDeque::with_capacity(HISTORY_LEN);
    let mut disk_history: VecDeque<u64> = VecDeque::with_capacity(HISTORY_LEN);
    let mut wifi_history: VecDeque<u64> = VecDeque::with_capacity(HISTORY_LEN);

    // Cache data metrik agar tidak query lock store setiap 50-100ms
    let mut wpm_score: u32 = 0;
    let mut running_app: String = String::from("None");
    let mut cpu_snapshot: Option<CpuSnapshot> = None;
    let mut ram_snapshot: Option<RamSnapshot> = None;
    let mut disk_snapshot: Option<DiskSnapshot> = None;
    let mut wifi_snapshot: Option<WiFiSnapshot> = None;

    // Sesi tracking & state tambahan untuk fitur social-activity
    let mut session_start_time = Instant::now();
    let mut current_session_activity = String::from("Architecture design & system implementation");

    // 7) MAIN TUI LOOP
    loop {
        render_interval.tick().await;

        let _uptime_secs = start_time.elapsed().as_secs();

        // Refresh data dari database setiap 500ms
        if last_fetch_time.elapsed() >= Duration::from_millis(500) {
            last_fetch_time = Instant::now();

            wpm_score = api_state.wpm_check().await.ok().flatten().unwrap_or(0);
            let fetched_app = api_state
                .running_application_check()
                .await
                .ok()
                .flatten()
                .unwrap_or_else(|| String::from("None"));

            if fetched_app != running_app {
                running_app = fetched_app;
                session_start_time = Instant::now(); // Reset durasi sesi saat aplikasi berganti
                // Deteksi cerdas aktivitas berdasarkan aplikasi
                if running_app.to_lowercase().contains("code") || running_app.to_lowercase().contains("nvim") {
                    current_session_activity = String::from("Writing Rust/TS code");
                } else if running_app.to_lowercase().contains("excel") || running_app.to_lowercase().contains("word") {
                    current_session_activity = String::from("Drafting reports & documentation");
                } else if running_app.to_lowercase().contains("chrome") || running_app.to_lowercase().contains("firefox") {
                    current_session_activity = String::from("Researching technical specs");
                } else {
                    current_session_activity = String::from("General multitasking");
                }
            }

            cpu_snapshot = api_state.cpu_util_check().await.ok().flatten();
            ram_snapshot = api_state.ram_util_check().await.ok().flatten();
            disk_snapshot = api_state.disk_util_check().await.ok().flatten();
            wifi_snapshot = api_state.wifi_util_check().await.ok().flatten();

            if window_history.back() != Some(&running_app) {
                if window_history.len() == 15 {
                    window_history.pop_front();
                }
                window_history.push_back(running_app.clone());
            }

            if cpu_history.len() == HISTORY_LEN {
                cpu_history.pop_front();
            }
            cpu_history.push_back(
                cpu_snapshot
                    .as_ref()
                    .map(|s| s.soft_utilization.round() as u64)
                    .unwrap_or(0),
            );

            if ram_history.len() == HISTORY_LEN {
                ram_history.pop_front();
            }
            ram_history.push_back(
                ram_snapshot
                    .as_ref()
                    .map(|s| {
                        if s.hard_capacity > 0.0 {
                            ((s.soft_in_use / s.hard_capacity) * 100.0).round() as u64
                        } else {
                            0
                        }
                    })
                    .unwrap_or(0),
            );

            if wifi_history.len() == HISTORY_LEN {
                wifi_history.pop_front();
            }
            wifi_history.push_back(
                wifi_snapshot
                    .as_ref()
                    .map(|s| s.hard_throughput.round() as u64)
                    .unwrap_or(0),
            );

            if disk_history.len() == HISTORY_LEN {
                disk_history.pop_front();
            }
            disk_history.push_back(
                disk_snapshot
                    .as_ref()
                    .map(|s| {
                        if s.hard_capacity > 0 {
                            ((s.hard_capacity_in_use / s.hard_capacity) * 100) as u64
                        } else {
                            0
                        }
                    })
                    .unwrap_or(0),
            );
        }

        terminal.draw(|f| {
            let layout = Layout::default()
                .direction(Direction::Vertical)
                .margin(1)
                .constraints([Constraint::Length(3), Constraint::Min(0)])
                .split(f.area());

            // ---------------- TAB BAR ----------------
            let tab_label = |label: &str, is_active: bool| -> Span {
                if is_active {
                    Span::styled(
                        format!(" {label} "),
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    Span::styled(format!(" {label} "), Style::default().fg(Color::DarkGray))
                }
            };
            let tabs = Paragraph::new(Line::from(vec![
                Span::styled(
                    " TRACK YOUR DAY ",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" │ "),
                tab_label("Activity", active_tab == ActiveTab::Activity),
                tab_label("Performance", active_tab == ActiveTab::Performance),
                Span::raw(" │ "),
                Span::styled(
                    "[Tab] Switch View    [q] Quit",
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
            .block(Block::bordered().border_style(Style::default().fg(Color::DarkGray)));
            f.render_widget(tabs, layout[0]);

            match active_tab {
                ActiveTab::Activity => {
                    // Professional 3-column Layout
                    let main_layout = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([
                            Constraint::Percentage(28),
                            Constraint::Percentage(44),
                            Constraint::Percentage(28),
                        ])
                        .split(layout[1]);

                    // --- 1. USER PROFILE & IDENTITY ---
                    let profile_chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Length(11),
                            Constraint::Min(0),
                        ])
                        .split(main_layout[0]);

                    let profile_block = Block::bordered()
                        .title(Span::styled(" PROFILE ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)))
                        .border_style(Style::default().fg(Color::DarkGray));
                    let profile_content = vec![
                        Line::from(vec![Span::styled("Name    : ", Style::default().fg(Color::DarkGray)), Span::styled("Alex Ratatui", Style::default().fg(Color::White).add_modifier(Modifier::BOLD))]),
                        Line::from(vec![Span::styled("Phone   : ", Style::default().fg(Color::DarkGray)), Span::styled("+62 812-3456-7890", Style::default().fg(Color::White))]),
                        Line::from(vec![Span::styled("Gender  : ", Style::default().fg(Color::DarkGray)), Span::styled("Male", Style::default().fg(Color::White))]),
                        Line::from(vec![Span::styled("IG      : ", Style::default().fg(Color::DarkGray)), Span::styled("@alex.dev", Style::default().fg(Color::Cyan))]),
                        Line::from(vec![Span::styled("GitHub  : ", Style::default().fg(Color::DarkGray)), Span::styled("github.com/alex", Style::default().fg(Color::Green))]),
                        Line::from(vec![Span::styled("Twitter : ", Style::default().fg(Color::DarkGray)), Span::styled("@alex_rust", Style::default().fg(Color::Blue))]),
                    ];
                    f.render_widget(Paragraph::new(profile_content).block(profile_block), profile_chunks[0]);

                    let bio_block = Block::bordered()
                        .title(Span::styled(" BIO & STATUS ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)))
                        .border_style(Style::default().fg(Color::DarkGray));
                    let bio_content = vec![
                        Line::from(Span::styled("\"Building robust systems & shipping clean Rust code daily.\"", Style::default().fg(Color::Yellow).add_modifier(Modifier::ITALIC))),
                        Line::from(""),
                        Line::from(vec![
                            Span::styled("STATE: ", Style::default().fg(Color::DarkGray)),
                            Span::styled("DEEP FOCUS", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                        ]),
                    ];
                    f.render_widget(Paragraph::new(bio_content).block(bio_block), profile_chunks[1]);

                    // --- 2. ACTIVE SESSION & WORK INSIGHTS ---
                    let session_chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Length(8),
                            Constraint::Length(7),
                            Constraint::Min(0),
                        ])
                        .split(main_layout[1]);

                    let session_duration = session_start_time.elapsed().as_secs();
                    let session_time_str = format!(
                        "{:02}:{:02}:{:02}",
                        session_duration / 3600,
                        (session_duration % 3600) / 60,
                        session_duration % 60
                    );

                    let current_session_block = Block::bordered()
                        .title(Span::styled(" CURRENT SESSION ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)))
                        .border_style(Style::default().fg(Color::DarkGray));
                    let current_session_content = vec![
                        Line::from(vec![Span::styled("App     : ", Style::default().fg(Color::DarkGray)), Span::styled(running_app.clone(), Style::default().fg(Color::White).add_modifier(Modifier::BOLD))]),
                        Line::from(vec![Span::styled("Duration: ", Style::default().fg(Color::DarkGray)), Span::styled(session_time_str, Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))]),
                        Line::from(vec![Span::styled("Activity: ", Style::default().fg(Color::DarkGray)), Span::styled(current_session_activity.clone(), Style::default().fg(Color::Cyan))]),
                    ];
                    f.render_widget(Paragraph::new(current_session_content).block(current_session_block), session_chunks[0]);

                    let special_status_block = Block::bordered()
                        .title(Span::styled(" CONTEXT & INSIGHT ", Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)))
                        .border_style(Style::default().fg(Color::DarkGray));
                    let special_content = if running_app.to_lowercase().contains("code") || running_app.to_lowercase().contains("nvim") {
                        vec![
                            Line::from(vec![Span::styled("Environment : ", Style::default().fg(Color::DarkGray)), Span::styled("IDE / Code Editor", Style::default().fg(Color::Green))]),
                            Line::from(vec![Span::styled("Primary Lang: ", Style::default().fg(Color::DarkGray)), Span::styled("Rust / TypeScript", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))]),
                            Line::from(vec![Span::styled("Task Status : ", Style::default().fg(Color::DarkGray)), Span::styled("Active compilation & test suite", Style::default().fg(Color::Yellow))]),
                        ]
                    } else if running_app.to_lowercase().contains("excel") || running_app.to_lowercase().contains("word") {
                        vec![
                            Line::from(vec![Span::styled("Environment : ", Style::default().fg(Color::DarkGray)), Span::styled("Office / Productivity Suite", Style::default().fg(Color::Blue))]),
                            Line::from(vec![Span::styled("Task Status : ", Style::default().fg(Color::DarkGray)), Span::styled("Data analysis & reporting", Style::default().fg(Color::White))]),
                        ]
                    } else {
                        vec![
                            Line::from(vec![Span::styled("Environment : ", Style::default().fg(Color::DarkGray)), Span::styled("General Workstation", Style::default().fg(Color::DarkGray))]),
                            Line::from(vec![Span::styled("Task Status : ", Style::default().fg(Color::DarkGray)), Span::styled("General browsing / utilities", Style::default().fg(Color::DarkGray))]),
                        ]
                    };
                    f.render_widget(Paragraph::new(special_content).block(special_status_block), session_chunks[1]);

                    let timeline_block = Block::bordered()
                        .title(Span::styled(" SESSION HISTORY ", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)))
                        .border_style(Style::default().fg(Color::DarkGray));
                    let timeline_items: Vec<ListItem> = window_history
                        .iter()
                        .rev()
                        .enumerate()
                        .map(|(idx, app)| {
                            let prefix = if idx == 0 { "> " } else { "  " };
                            let style = if idx == 0 {
                                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                            } else {
                                Style::default().fg(Color::DarkGray)
                            };
                            ListItem::new(Line::from(vec![
                                Span::styled(prefix, Style::default().fg(Color::Cyan)),
                                Span::styled(app.clone(), style),
                            ]))
                        })
                        .collect();
                    f.render_widget(List::new(timeline_items).block(timeline_block), session_chunks[2]);

                    // --- 3. METRICS & ENVIRONMENT ---
                    let metrics_chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Length(8),
                            Constraint::Min(0),
                        ])
                        .split(main_layout[2]);

                    let typing_block = Block::bordered()
                        .title(Span::styled(" TYPING METRICS ", Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)))
                        .border_style(Style::default().fg(Color::DarkGray));
                    let typing_content = vec![
                        Line::from(vec![Span::styled("Speed   : ", Style::default().fg(Color::DarkGray)), Span::styled(format!("{} WPM", wpm_score), Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD))]),
                        Line::from(vec![Span::styled("Accuracy: ", Style::default().fg(Color::DarkGray)), Span::styled("98.5% [Optimal]", Style::default().fg(Color::Green))]),
                        Line::from(vec![Span::styled("Raw CPM : ", Style::default().fg(Color::DarkGray)), Span::styled(format!("{} chars/min", wpm_score * 5), Style::default().fg(Color::White))]),
                    ];
                    f.render_widget(Paragraph::new(typing_content).block(typing_block), metrics_chunks[0]);

                    let sys_env_block = Block::bordered()
                        .title(Span::styled(" ENVIRONMENT ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)))
                        .border_style(Style::default().fg(Color::DarkGray));
                    let wifi_ssid = wifi_snapshot.as_ref().map(|s| s.soft_ssid.clone()).unwrap_or_else(|| "Not connected".to_string());
                    let wifi_tp = wifi_snapshot.as_ref().map(|s| format!("{:.1} Mbps", s.hard_throughput)).unwrap_or_else(|| "0 Mbps".to_string());
                    let sys_env_content = vec![
                        Line::from(vec![Span::styled("Battery : ", Style::default().fg(Color::DarkGray)), Span::styled("85% [Charging]", Style::default().fg(Color::Green))]),
                        Line::from(vec![Span::styled("Network : ", Style::default().fg(Color::DarkGray)), Span::styled(wifi_ssid, Style::default().fg(Color::Cyan))]),
                        Line::from(vec![Span::styled("Bandwidth: ", Style::default().fg(Color::DarkGray)), Span::styled(wifi_tp, Style::default().fg(Color::White))]),
                        Line::from(vec![Span::styled("Audio   : ", Style::default().fg(Color::DarkGray)), Span::styled("Active [65%]", Style::default().fg(Color::Yellow))]),
                    ];
                    f.render_widget(Paragraph::new(sys_env_content).block(sys_env_block), metrics_chunks[1]);
                }
                ActiveTab::Performance => {
                    let sections = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
                        .split(layout[1]);

                    // ---------------- KIRI (30%): daftar util yang bisa dipilih ----------------
                    let menu_style = |item: PerfMenu| -> Style {
                        if perf_menu == item {
                            Style::default()
                                .fg(Color::Black)
                                .bg(Color::Cyan)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::White)
                        }
                    };
                    let arrow = |item: PerfMenu| -> &'static str {
                        if perf_menu == item {
                            "> "
                        } else {
                            "  "
                        }
                    };

                    let menu_items = vec![
                        Line::from(Span::styled(
                            format!("{}CPU        ", arrow(PerfMenu::Cpu)),
                            menu_style(PerfMenu::Cpu),
                        )),
                        Line::from(Span::styled(
                            format!("{}Memory     ", arrow(PerfMenu::Memory)),
                            menu_style(PerfMenu::Memory),
                        )),
                        Line::from(Span::styled(
                            format!("{}Disk       ", arrow(PerfMenu::Disk)),
                            menu_style(PerfMenu::Disk),
                        )),
                        Line::from(Span::styled(
                            format!("{}WiFi       ", arrow(PerfMenu::Wifi)),
                            menu_style(PerfMenu::Wifi),
                        )),
                    ];
                    let menu = Paragraph::new(menu_items).block(Block::bordered().title(" Utilities ").border_style(Style::default().fg(Color::DarkGray)));
                    f.render_widget(menu, sections[0]);

                    // ---------------- KANAN (70%): detail sesuai menu yang dipilih ----------------
                    match perf_menu {
                        PerfMenu::Cpu => {
                            let detail_rows = Layout::default()
                                .direction(Direction::Vertical)
                                .constraints([
                                    Constraint::Length(3),
                                    Constraint::Min(10),
                                    Constraint::Min(0),
                                ])
                                .split(sections[1]);

                            let usage_percent =
                                cpu_snapshot.as_ref().map(|s| s.soft_utilization).unwrap_or(0.0);
                            let usage_gauge = Gauge::default()
                                .block(Block::bordered().title(" CPU Usage ").border_style(Style::default().fg(Color::DarkGray)))
                                .gauge_style(Style::default().fg(Color::Cyan))
                                .percent(usage_percent.clamp(0.0, 100.0).round() as u16)
                                .label(format!("{usage_percent:.1}%"));
                            f.render_widget(usage_gauge, detail_rows[0]);

                            let cpu_points: Vec<(f64, f64)> = cpu_history
                                .iter()
                                .enumerate()
                                .map(|(i, v)| (i as f64, *v as f64))
                                .collect();

                            let cpu_dataset = Dataset::default()
                                .name("CPU %")
                                .marker(Marker::Braille)
                                .graph_type(GraphType::Line)
                                .style(Style::default().fg(Color::Cyan))
                                .data(&cpu_points);

                            let x_axis = Axis::default()
                                .title(Span::styled("Time", Style::default().fg(Color::DarkGray)))
                                .bounds([0.0, HISTORY_LEN as f64])
                                .labels(["start", "now"]);
                            let y_axis = Axis::default()
                                .title(Span::styled("Percent", Style::default().fg(Color::DarkGray)))
                                .bounds([0.0, 100.0])
                                .labels(["0", "50", "100"]);

                            let cpu_chart = Chart::new(vec![cpu_dataset])
                                .block(Block::bordered().title(" History ").border_style(Style::default().fg(Color::DarkGray)))
                                .x_axis(x_axis)
                                .y_axis(y_axis);
                            f.render_widget(cpu_chart, detail_rows[1]);

                            let detail_text: Vec<Line> = if let Some(snapshot) = &cpu_snapshot {
                                vec![
                                    Line::from(Span::styled(
                                        "Software (Live)",
                                        Style::default().add_modifier(Modifier::UNDERLINED),
                                    )),
                                    Line::from(format!("Processes        : {}", snapshot.soft_processes)),
                                    Line::from(format!("Threads          : {}", snapshot.soft_threads)),
                                    Line::from(format!("Handles          : {}", snapshot.soft_handles)),
                                    Line::from(format!(
                                        "Current speed    : {:.0} MHz",
                                        snapshot.soft_speed_clock
                                    )),
                                    Line::from(format!("Uptime           : {}", snapshot.soft_uptime)),
                                    Line::from(""),
                                    Line::from(Span::styled(
                                        "Hardware (Static)",
                                        Style::default().add_modifier(Modifier::UNDERLINED),
                                    )),
                                    Line::from(format!(
                                        "Base speed       : {:.0} MHz",
                                        snapshot.hard_base_speed
                                    )),
                                    Line::from(format!("Sockets          : {}", snapshot.hard_sockets)),
                                    Line::from(format!("Cores            : {}", snapshot.hard_cores)),
                                    Line::from(format!(
                                        "Logical procs    : {}",
                                        snapshot.hard_logical_processors
                                    )),
                                    Line::from(format!(
                                        "Virtualization   : {}",
                                        if snapshot.hard_virtualization {
                                            "Enabled"
                                        } else {
                                            "Disabled"
                                        }
                                    )),
                                    Line::from(format!(
                                        "L1 cache         : {:.0} KB",
                                        snapshot.hard_l1_cache
                                    )),
                                    Line::from(format!(
                                        "L2 cache         : {:.0} KB",
                                        snapshot.hard_l2_cache
                                    )),
                                    Line::from(format!(
                                        "L3 cache         : {:.0} KB",
                                        snapshot.hard_l3_cache
                                    )),
                                ]
                            } else {
                                vec![Line::from(Span::styled(
                                    "Awaiting CPU metrics...",
                                    Style::default().fg(Color::DarkGray),
                                ))]
                            };
                            let detail_panel =
                                Paragraph::new(detail_text).block(Block::bordered().title(" Details ").border_style(Style::default().fg(Color::DarkGray)));
                            f.render_widget(detail_panel, detail_rows[2]);
                        }

                        PerfMenu::Memory => {
                            let detail_rows = Layout::default()
                                .direction(Direction::Vertical)
                                .constraints([
                                    Constraint::Length(3),
                                    Constraint::Min(10),
                                    Constraint::Min(0),
                                ])
                                .split(sections[1]);

                            let usage_percent = ram_snapshot
                                .as_ref()
                                .map(|s| {
                                    if s.hard_capacity > 0.0 {
                                        (s.soft_in_use / s.hard_capacity) * 100.0
                                    } else {
                                        0.0
                                    }
                                })
                                .unwrap_or(0.0);
                            let usage_gauge = Gauge::default()
                                .block(Block::bordered().title(" RAM Usage ").border_style(Style::default().fg(Color::DarkGray)))
                                .gauge_style(Style::default().fg(Color::Green))
                                .percent(usage_percent.clamp(0.0, 100.0).round() as u16)
                                .label(format!("{usage_percent:.1}%"));
                            f.render_widget(usage_gauge, detail_rows[0]);

                            let ram_points: Vec<(f64, f64)> = ram_history
                                .iter()
                                .enumerate()
                                .map(|(i, v)| (i as f64, *v as f64))
                                .collect();

                            let ram_dataset = Dataset::default()
                                .name("RAM %")
                                .marker(Marker::Braille)
                                .graph_type(GraphType::Line)
                                .style(Style::default().fg(Color::Green))
                                .data(&ram_points);

                            let x_axis = Axis::default()
                                .title(Span::styled("Time", Style::default().fg(Color::DarkGray)))
                                .bounds([0.0, HISTORY_LEN as f64])
                                .labels(["start", "now"]);
                            let y_axis = Axis::default()
                                .title(Span::styled("Percent", Style::default().fg(Color::DarkGray)))
                                .bounds([0.0, 100.0])
                                .labels(["0", "50", "100"]);

                            let ram_chart = Chart::new(vec![ram_dataset])
                                .block(Block::bordered().title(" History ").border_style(Style::default().fg(Color::DarkGray)))
                                .x_axis(x_axis)
                                .y_axis(y_axis);
                            f.render_widget(ram_chart, detail_rows[1]);

                            let detail_text: Vec<Line> = if let Some(snapshot) = &ram_snapshot {
                                vec![
                                    Line::from(Span::styled(
                                        "Software (Live)",
                                        Style::default().add_modifier(Modifier::UNDERLINED),
                                    )),
                                    Line::from(format!("In use           : {:.2} GB", snapshot.soft_in_use)),
                                    Line::from(format!(
                                        "Available        : {:.2} GB",
                                        snapshot.soft_available
                                    )),
                                    Line::from(format!(
                                        "Hardware reserved: {:.2} GB",
                                        snapshot.soft_hardware_reserve
                                    )),
                                    Line::from(format!(
                                        "Committed in use : {:.2} GB",
                                        snapshot.soft_in_commited
                                    )),
                                    Line::from(format!(
                                        "Committed avail  : {:.2} GB",
                                        snapshot.soft_available_commited
                                    )),
                                    Line::from(format!("Cached           : {:.2} GB", snapshot.soft_cached)),
                                    Line::from(format!(
                                        "Paged pool       : {:.2} GB",
                                        snapshot.soft_page_pool
                                    )),
                                    Line::from(format!(
                                        "Non-paged pool   : {:.2} GB",
                                        snapshot.soft_non_paged_pool
                                    )),
                                    Line::from(""),
                                    Line::from(Span::styled(
                                        "Hardware (Static)",
                                        Style::default().add_modifier(Modifier::UNDERLINED),
                                    )),
                                    Line::from(format!(
                                        "Capacity         : {:.2} GB",
                                        snapshot.hard_capacity
                                    )),
                                    Line::from(format!("Speed            : {} MHz", snapshot.hard_speed)),
                                    Line::from(format!(
                                        "Slots used       : {}",
                                        snapshot.hard_slots_used
                                    )),
                                    Line::from(format!(
                                        "Form factor      : {}",
                                        snapshot.hard_form_factor
                                    )),
                                ]
                            } else {
                                vec![Line::from(Span::styled(
                                    "Awaiting RAM metrics...",
                                    Style::default().fg(Color::DarkGray),
                                ))]
                            };
                            let detail_panel =
                                Paragraph::new(detail_text).block(Block::bordered().title(" Details ").border_style(Style::default().fg(Color::DarkGray)));
                            f.render_widget(detail_panel, detail_rows[2]);
                        }

                        PerfMenu::Disk => {
                            let detail_rows = Layout::default()
                                .direction(Direction::Vertical)
                                .constraints([
                                    Constraint::Length(3),
                                    Constraint::Min(10),
                                    Constraint::Min(0),
                                ])
                                .split(sections[1]);

                            let usage_percent =
                                disk_snapshot.as_ref().map(|s| s.soft_active_time).unwrap_or(0.0);
                            let usage_gauge = Gauge::default()
                                .block(Block::bordered().title(" Disk Active Time ").border_style(Style::default().fg(Color::DarkGray)))
                                .gauge_style(Style::default().fg(Color::Yellow))
                                .percent(usage_percent.clamp(0.0, 100.0).round() as u16)
                                .label(format!("{usage_percent:.1}%"));
                            f.render_widget(usage_gauge, detail_rows[0]);

                            let disk_points: Vec<(f64, f64)> = disk_history
                                .iter()
                                .enumerate()
                                .map(|(i, v)| (i as f64, *v as f64))
                                .collect();

                            let disk_dataset = Dataset::default()
                                .name("Active %")
                                .marker(Marker::Braille)
                                .graph_type(GraphType::Line)
                                .style(Style::default().fg(Color::Yellow))
                                .data(&disk_points);

                            let x_axis = Axis::default()
                                .title(Span::styled("Time", Style::default().fg(Color::DarkGray)))
                                .bounds([0.0, HISTORY_LEN as f64])
                                .labels(["start", "now"]);
                            let y_axis = Axis::default()
                                .title(Span::styled("Percent", Style::default().fg(Color::DarkGray)))
                                .bounds([0.0, 100.0])
                                .labels(["0", "50", "100"]);

                            let disk_chart = Chart::new(vec![disk_dataset])
                                .block(Block::bordered().title(" History ").border_style(Style::default().fg(Color::DarkGray)))
                                .x_axis(x_axis)
                                .y_axis(y_axis);
                            f.render_widget(disk_chart, detail_rows[1]);

                            let detail_text: Vec<Line> = if let Some(snapshot) = &disk_snapshot {
                                vec![
                                    Line::from(Span::styled(
                                        "Software (Live)",
                                        Style::default().add_modifier(Modifier::UNDERLINED),
                                    )),
                                    Line::from(format!(
                                        "Active time      : {:.1}%",
                                        snapshot.soft_active_time
                                    )),
                                    Line::from(format!(
                                        "Read speed       : {:.2} MB/s",
                                        snapshot.soft_read_speed
                                    )),
                                    Line::from(format!(
                                        "Write speed      : {:.2} MB/s",
                                        snapshot.soft_write_speed
                                    )),
                                    Line::from(format!(
                                        "Avg response time: {:.2} ms",
                                        snapshot.soft_average_response_time
                                    )),
                                    Line::from(""),
                                    Line::from(Span::styled(
                                        "Hardware (Static)",
                                        Style::default().add_modifier(Modifier::UNDERLINED),
                                    )),
                                    Line::from(format!("Capacity         : {} GB", snapshot.hard_capacity)),
                                    Line::from(format!(
                                        "Formatted        : {} GB",
                                        snapshot.hard_formatted
                                    )),
                                    Line::from(format!(
                                        "System disk      : {}",
                                        if snapshot.hard_system_disk {
                                            "Yes"
                                        } else {
                                            "No"
                                        }
                                    )),
                                    Line::from(format!("Type             : {}", snapshot.hard_type)),
                                ]
                            } else {
                                vec![Line::from(Span::styled(
                                    "Awaiting Disk metrics...",
                                    Style::default().fg(Color::DarkGray),
                                ))]
                            };
                            let detail_panel =
                                Paragraph::new(detail_text).block(Block::bordered().title(" Details ").border_style(Style::default().fg(Color::DarkGray)));
                            f.render_widget(detail_panel, detail_rows[2]);
                        }

                        PerfMenu::Wifi => {
                            let detail_rows = Layout::default()
                                .direction(Direction::Vertical)
                                .constraints([
                                    Constraint::Length(3),
                                    Constraint::Min(10),
                                    Constraint::Min(0),
                                ])
                                .split(sections[1]);

                            let throughput =
                                wifi_snapshot.as_ref().map(|s| s.hard_throughput).unwrap_or(0.0);
                            const THROUGHPUT_MAX: f32 = 100.0;
                            let gauge_percent =
                                ((throughput / THROUGHPUT_MAX) * 100.0).clamp(0.0, 100.0);
                            let usage_gauge = Gauge::default()
                                .block(Block::bordered().title(" WiFi Throughput ").border_style(Style::default().fg(Color::DarkGray)))
                                .gauge_style(Style::default().fg(Color::Magenta))
                                .percent(gauge_percent.round() as u16)
                                .label(format!("{throughput:.1} Mbps"));
                            f.render_widget(usage_gauge, detail_rows[0]);

                            let wifi_points: Vec<(f64, f64)> = wifi_history
                                .iter()
                                .enumerate()
                                .map(|(i, v)| (i as f64, *v as f64))
                                .collect();

                            let wifi_dataset = Dataset::default()
                                .name("Throughput (Mbps)")
                                .marker(Marker::Braille)
                                .graph_type(GraphType::Line)
                                .style(Style::default().fg(Color::Magenta))
                                .data(&wifi_points);

                            let x_axis = Axis::default()
                                .title(Span::styled("Time", Style::default().fg(Color::DarkGray)))
                                .bounds([0.0, HISTORY_LEN as f64])
                                .labels(["start", "now"]);
                            let wifi_y_max =
                                wifi_history.iter().map(|&v| v as f64).fold(10.0_f64, f64::max);
                            let y_axis = Axis::default()
                                .title(Span::styled("Mbps", Style::default().fg(Color::DarkGray)))
                                .bounds([0.0, wifi_y_max])
                                .labels([
                                    "0".to_string(),
                                    format!("{:.0}", wifi_y_max / 2.0),
                                    format!("{:.0}", wifi_y_max),
                                ]);

                            let wifi_chart = Chart::new(vec![wifi_dataset])
                                .block(Block::bordered().title(" History ").border_style(Style::default().fg(Color::DarkGray)))
                                .x_axis(x_axis)
                                .y_axis(y_axis);
                            f.render_widget(wifi_chart, detail_rows[1]);

                            let detail_text: Vec<Line> = if let Some(snapshot) = &wifi_snapshot {
                                vec![
                                    Line::from(Span::styled(
                                        "Software (Live)",
                                        Style::default().add_modifier(Modifier::UNDERLINED),
                                    )),
                                    Line::from(format!("SSID             : {}", snapshot.soft_ssid)),
                                    Line::from(format!(
                                        "Connection type  : {}",
                                        snapshot.soft_connection_type
                                    )),
                                    Line::from(format!(
                                        "Signal strength  : {}%",
                                        snapshot.soft_signal_strength
                                    )),
                                    Line::from(format!(
                                        "IPv4 address     : {}",
                                        snapshot.soft_ipv4_address
                                    )),
                                    Line::from(format!(
                                        "IPv6 address     : {}",
                                        snapshot.soft_ipv6_address
                                    )),
                                    Line::from(format!("Adapter          : {}", snapshot.soft_adapter_name)),
                                    Line::from(""),
                                    Line::from(Span::styled(
                                        "Hardware (Static)",
                                        Style::default().add_modifier(Modifier::UNDERLINED),
                                    )),
                                    Line::from(format!(
                                        "Interface        : {}",
                                        snapshot.hard_name_wireless_lan
                                    )),
                                    Line::from(format!(
                                        "Throughput       : {:.2} Mbps",
                                        snapshot.hard_throughput
                                    )),
                                    Line::from(format!(
                                        "Send (Tx)        : {:.2} Mbps",
                                        snapshot.hard_send
                                    )),
                                    Line::from(format!(
                                        "Receive (Rx)     : {:.2} Mbps",
                                        snapshot.hard_receive
                                    )),
                                ]
                            } else {
                                vec![Line::from(Span::styled(
                                    "Awaiting WiFi metrics...",
                                    Style::default().fg(Color::DarkGray),
                                ))]
                            };
                            let detail_panel =
                                Paragraph::new(detail_text).block(Block::bordered().title(" Details ").border_style(Style::default().fg(Color::DarkGray)));
                            f.render_widget(detail_panel, detail_rows[2]);
                        }
                    }
                }
            }
        })?;

        // Tangani event keyboard secara non-blocking
        while event::poll(Duration::from_millis(0))? {
            if let Event::Key(key) = event::read()? {
                // HANYA proses saat key ditekan (mengabaikan release & repeat untuk mencegah bug loncat 2x)
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') => return Ok(()),
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            return Ok(())
                        }
                        KeyCode::Tab => {
                            active_tab = match active_tab {
                                ActiveTab::Activity => ActiveTab::Performance,
                                ActiveTab::Performance => ActiveTab::Activity,
                            };
                        }
                        KeyCode::Up if active_tab == ActiveTab::Performance => {
                            perf_menu = perf_menu.prev();
                        }
                        KeyCode::Down if active_tab == ActiveTab::Performance => {
                            perf_menu = perf_menu.next();
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    // 8) TEARDOWN TASKS
    #[allow(unreachable_code)]
    {
        mutator_handle.abort();
        reactive_handle.abort();
        Ok(())
    }
}
