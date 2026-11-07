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
    widgets::{Block, BorderType, Borders, Gauge, Paragraph},
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
    let mut window_history: VecDeque<String> = VecDeque::with_capacity(12);
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

    // Sesi tracking & state tambahan
    let mut session_start_time = Instant::now();
    let mut current_session_activity = String::from("Initializing workspace context");

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
                session_start_time = Instant::now();
                
                let app_lower = running_app.to_lowercase();
                if app_lower.contains("code") || app_lower.contains("nvim") || app_lower.contains("cursor") {
                    current_session_activity = String::from("Writing & compiling code (Rust / TS)");
                } else if app_lower.contains("excel") || app_lower.contains("word") || app_lower.contains("powerpnt") {
                    current_session_activity = String::from("Drafting reports & spreadsheet analytics");
                } else if app_lower.contains("chrome") || app_lower.contains("firefox") || app_lower.contains("msedge") {
                    current_session_activity = String::from("Web research & technical documentation");
                } else if app_lower.contains("terminal") || app_lower.contains("iterm") || app_lower.contains("conhost") {
                    current_session_activity = String::from("Shell execution & system monitoring");
                } else {
                    current_session_activity = String::from("General workstation multitasking");
                }
            }

            cpu_snapshot = api_state.cpu_util_check().await.ok().flatten();
            ram_snapshot = api_state.ram_util_check().await.ok().flatten();
            disk_snapshot = api_state.disk_util_check().await.ok().flatten();
            wifi_snapshot = api_state.wifi_util_check().await.ok().flatten();

            if window_history.back() != Some(&running_app) {
                if window_history.len() == 12 {
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
            let size = f.area();
            let layout = Layout::default()
                .direction(Direction::Vertical)
                .margin(1)
                .constraints([Constraint::Length(3), Constraint::Min(0)])
                .split(size);

            // ---------------- HEADER / TOPBAR ----------------
            let tab_label = |label: &str, is_active: bool| -> Span {
                if is_active {
                    Span::styled(
                        format!(" [{label}] "),
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    Span::styled(format!("  {label}  "), Style::default().fg(Color::DarkGray))
                }
            };
            let tabs = Paragraph::new(Line::from(vec![
                Span::styled(
                    " ❖ ACTIVITY PULSE ",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
                Span::styled("│", Style::default().fg(Color::DarkGray)),
                tab_label("Activity Feed", active_tab == ActiveTab::Activity),
                tab_label("System Metrics", active_tab == ActiveTab::Performance),
                Span::styled("│", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    " [Tab] Toggle View   [q] Quit ",
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::DarkGray)),
            );
            f.render_widget(tabs, layout[0]);

            match active_tab {
                ActiveTab::Activity => {
                    // Professional 3-Column Asymmetric Dashboard Layout
                    let cols = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([
                            Constraint::Percentage(30),
                            Constraint::Percentage(45),
                            Constraint::Percentage(25),
                        ])
                        .split(layout[1]);

                    // --- COLUMN 1: IDENTITY & PROFILE ---
                    let left_chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Length(10), // Profile card
                            Constraint::Min(0),    // Bio & Philosophy
                        ])
                        .split(cols[0]);

                    let profile_block = Block::default()
                        .title(Span::styled(" [👤] OPERATOR PROFILE ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)))
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(Color::DarkGray));

                    let profile_text = vec![
                        Line::from(vec![Span::styled("◇ Name    ", Style::default().fg(Color::DarkGray)), Span::styled("Alex V. Vance", Style::default().fg(Color::White).add_modifier(Modifier::BOLD))]),
                        Line::from(vec![Span::styled("◇ Role    ", Style::default().fg(Color::DarkGray)), Span::styled("Principal Engineer", Style::default().fg(Color::Cyan))]),
                        Line::from(vec![Span::styled("◇ Contact ", Style::default().fg(Color::DarkGray)), Span::styled("+62 812-3456-7890", Style::default().fg(Color::White))]),
                        Line::from(vec![Span::styled("◇ GitHub  ", Style::default().fg(Color::DarkGray)), Span::styled("github.com/alexv", Style::default().fg(Color::Green))]),
                        Line::from(vec![Span::styled("◇ Network ", Style::default().fg(Color::DarkGray)), Span::styled("IG @alex.dev", Style::default().fg(Color::Magenta))]),
                    ];
                    f.render_widget(Paragraph::new(profile_text).block(profile_block), left_chunks[0]);

                    let bio_block = Block::default()
                        .title(Span::styled(" [★] MANIFESTO ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)))
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(Color::DarkGray));

                    let bio_text = vec![
                        Line::from(Span::styled("“Low latency, high throughput, zero friction engineering.”", Style::default().fg(Color::Yellow).add_modifier(Modifier::ITALIC))),
                        Line::from(""),
                        Line::from(vec![
                            Span::styled("STATUS  ", Style::default().fg(Color::DarkGray)),
                            Span::styled("[● ACTIVE / FOCUSED]", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                        ]),
                    ];
                    f.render_widget(Paragraph::new(bio_text).block(bio_block), left_chunks[1]);

                    // --- COLUMN 2: LIVE SESSION, CONTEXT & TIMELINE ---
                    let mid_chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Length(7),  // Current Focus Session
                            Constraint::Length(6),  // Developer / Office Insight
                            Constraint::Min(0),     // Timeline Stream
                        ])
                        .split(cols[1]);

                    let session_dur = session_start_time.elapsed().as_secs();
                    let dur_str = format!(
                        "{:02}:{:02}:{:02}",
                        session_dur / 3600,
                        (session_dur % 3600) / 60,
                        session_dur % 60
                    );

                    let session_block = Block::default()
                        .title(Span::styled(" [⏱] CURRENT FOCUS SESSION ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)))
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(Color::DarkGray));

                    let session_text = vec![
                        Line::from(vec![Span::styled("Target App : ", Style::default().fg(Color::DarkGray)), Span::styled(running_app.clone(), Style::default().fg(Color::White).add_modifier(Modifier::BOLD))]),
                        Line::from(vec![Span::styled("Duration   : ", Style::default().fg(Color::DarkGray)), Span::styled(dur_str, Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))]),
                        Line::from(vec![Span::styled("Activity   : ", Style::default().fg(Color::DarkGray)), Span::styled(current_session_activity.clone(), Style::default().fg(Color::Cyan))]),
                    ];
                    f.render_widget(Paragraph::new(session_text).block(session_block), mid_chunks[0]);

                    let insight_block = Block::default()
                        .title(Span::styled(" [⚡] WORKLOAD CONTEXT ", Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)))
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(Color::DarkGray));

                    let app_l = running_app.to_lowercase();
                    let insight_text = if app_l.contains("code") || app_l.contains("nvim") || app_l.contains("cursor") {
                        vec![
                            Line::from(vec![Span::styled("Domain   : ", Style::default().fg(Color::DarkGray)), Span::styled("Software Architecture [IDE]", Style::default().fg(Color::Green))]),
                            Line::from(vec![Span::styled("Language : ", Style::default().fg(Color::DarkGray)), Span::styled("Rust / TypeScript [ACTIVE]", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))]),
                        ]
                    } else if app_l.contains("excel") || app_l.contains("word") || app_l.contains("powerpnt") {
                        vec![
                            Line::from(vec![Span::styled("Domain   : ", Style::default().fg(Color::DarkGray)), Span::styled("Corporate Productivity [OFFICE]", Style::default().fg(Color::Blue))]),
                            Line::from(vec![Span::styled("Workflow : ", Style::default().fg(Color::DarkGray)), Span::styled("Documentation & Analysis", Style::default().fg(Color::White))]),
                        ]
                    } else {
                        vec![
                            Line::from(vec![Span::styled("Domain   : ", Style::default().fg(Color::DarkGray)), Span::styled("General Workstation [IDLE]", Style::default().fg(Color::DarkGray))]),
                            Line::from(vec![Span::styled("Workflow : ", Style::default().fg(Color::DarkGray)), Span::styled("Browsing & Multi-utility switching", Style::default().fg(Color::DarkGray))]),
                        ]
                    };
                    f.render_widget(Paragraph::new(insight_text).block(insight_block), mid_chunks[1]);

                    let timeline_block = Block::default()
                        .title(Span::styled(" [≡] SESSION TIMELINE STREAM ", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)))
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(Color::DarkGray));

                    let timeline_items: Vec<ListItem> = window_history
                        .iter()
                        .rev()
                        .enumerate()
                        .map(|(i, app)| {
                            let indicator = if i == 0 { "[▶] " } else { "[-] " };
                            let style = if i == 0 {
                                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                            } else {
                                Style::default().fg(Color::DarkGray)
                            };
                            ListItem::new(Line::from(vec![
                                Span::styled(indicator, Style::default().fg(Color::Cyan)),
                                Span::styled(app.clone(), style),
                            ]))
                        })
                        .collect();
                    f.render_widget(List::new(timeline_items).block(timeline_block), mid_chunks[2]);

                    // --- COLUMN 3: TELEMETRY & ENVIRONMENT ---
                    let right_chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Length(7),  // Typing Telemetry
                            Constraint::Min(0),     // System Environment
                        ])
                        .split(cols[2]);

                    let typing_block = Block::default()
                        .title(Span::styled(" [⌨] TYPING DYNAMICS ", Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)))
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(Color::DarkGray));

                    let typing_text = vec![
                        Line::from(vec![Span::styled("Speed    ", Style::default().fg(Color::DarkGray)), Span::styled(format!("{} WPM", wpm_score), Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD))]),
                        Line::from(vec![Span::styled("Accuracy ", Style::default().fg(Color::DarkGray)), Span::styled("98.5% [OPTIMAL]", Style::default().fg(Color::Green))]),
                        Line::from(vec![Span::styled("Raw Rate ", Style::default().fg(Color::DarkGray)), Span::styled(format!("{} CPM", wpm_score * 5), Style::default().fg(Color::White))]),
                    ];
                    f.render_widget(Paragraph::new(typing_text).block(typing_block), right_chunks[0]);

                    let env_block = Block::default()
                        .title(Span::styled(" [Ω] ENVIRONMENT ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)))
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(Color::DarkGray));

                    let ssid = wifi_snapshot.as_ref().map(|s| s.soft_ssid.clone()).unwrap_or_else(|| "Offline".to_string());
                    let bw = wifi_snapshot.as_ref().map(|s| format!("{:.1} Mbps", s.hard_throughput)).unwrap_or_else(|| "0 Mbps".to_string());
                    
                    let env_text = vec![
                        Line::from(vec![Span::styled("Battery  ", Style::default().fg(Color::DarkGray)), Span::styled("85% [⚡ AC POWER]", Style::default().fg(Color::Green))]),
                        Line::from(vec![Span::styled("Network  ", Style::default().fg(Color::DarkGray)), Span::styled(ssid, Style::default().fg(Color::Cyan))]),
                        Line::from(vec![Span::styled("Throughput", Style::default().fg(Color::DarkGray)), Span::styled(bw, Style::default().fg(Color::White))]),
                        Line::from(vec![Span::styled("Audio    ", Style::default().fg(Color::DarkGray)), Span::styled("Active [🔊 65%]", Style::default().fg(Color::Yellow))]),
                    ];
                    f.render_widget(Paragraph::new(env_text).block(env_block), right_chunks[1]);
                }
                ActiveTab::Performance => {
                    let sections = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([Constraint::Percentage(25), Constraint::Percentage(75)])
                        .split(layout[1]);

                    // ---------------- KIRI: MENU UTILITAS ----------------
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
                    let indicator = |item: PerfMenu| -> &'static str {
                        if perf_menu == item {
                            "[▶] "
                        } else {
                            "[ ] "
                        }
                    };

                    let menu_items = vec![
                        Line::from(Span::styled(
                            format!("{}CPU Subsystem ", indicator(PerfMenu::Cpu)),
                            menu_style(PerfMenu::Cpu),
                        )),
                        Line::from(Span::styled(
                            format!("{}Memory (RAM)  ", indicator(PerfMenu::Memory)),
                            menu_style(PerfMenu::Memory),
                        )),
                        Line::from(Span::styled(
                            format!("{}Storage I/O   ", indicator(PerfMenu::Disk)),
                            menu_style(PerfMenu::Disk),
                        )),
                        Line::from(Span::styled(
                            format!("{}Wireless LAN  ", indicator(PerfMenu::Wifi)),
                            menu_style(PerfMenu::Wifi),
                        )),
                    ];
                    let menu = Paragraph::new(menu_items).block(
                        Block::default()
                            .title(Span::styled(" [⚙] Subsystems ", Style::default().fg(Color::Cyan)))
                            .borders(Borders::ALL)
                            .border_type(BorderType::Rounded)
                            .border_style(Style::default().fg(Color::DarkGray)),
                    );
                    f.render_widget(menu, sections[0]);

                    // ---------------- KANAN: DETAIL & GRAFIK ----------------
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
                                .block(
                                    Block::default()
                                        .title(" [#] CPU Utilization ")
                                        .borders(Borders::ALL)
                                        .border_type(BorderType::Rounded)
                                        .border_style(Style::default().fg(Color::DarkGray)),
                                )
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
                                .title(Span::styled("Timeline", Style::default().fg(Color::DarkGray)))
                                .bounds([0.0, HISTORY_LEN as f64])
                                .labels(["past", "now"]);
                            let y_axis = Axis::default()
                                .title(Span::styled("Load", Style::default().fg(Color::DarkGray)))
                                .bounds([0.0, 100.0])
                                .labels(["0%", "50%", "100%"]);

                            let cpu_chart = Chart::new(vec![cpu_dataset])
                                .block(
                                    Block::default()
                                        .title(" [~] Historical Trend ")
                                        .borders(Borders::ALL)
                                        .border_type(BorderType::Rounded)
                                        .border_style(Style::default().fg(Color::DarkGray)),
                                )
                                .x_axis(x_axis)
                                .y_axis(y_axis);
                            f.render_widget(cpu_chart, detail_rows[1]);

                            let detail_text: Vec<Line> = if let Some(snapshot) = &cpu_snapshot {
                                vec![
                                    Line::from(Span::styled(
                                        "Software Telemetry",
                                        Style::default().add_modifier(Modifier::UNDERLINED),
                                    )),
                                    Line::from(format!("Active Processes : {}", snapshot.soft_processes)),
                                    Line::from(format!("Active Threads   : {}", snapshot.soft_threads)),
                                    Line::from(format!("Clock Speed      : {:.0} MHz", snapshot.soft_speed_clock)),
                                    Line::from(""),
                                    Line::from(Span::styled(
                                        "Hardware Specs",
                                        Style::default().add_modifier(Modifier::UNDERLINED),
                                    )),
                                    Line::from(format!("Base Clock       : {:.0} MHz", snapshot.hard_base_speed)),
                                    Line::from(format!("Cores / Threads  : {} / {}", snapshot.hard_cores, snapshot.hard_logical_processors)),
                                    Line::from(format!("Virtualization   : {}", if snapshot.hard_virtualization { "[ENABLED]" } else { "[DISABLED]" })),
                                ]
                            } else {
                                vec![Line::from(Span::styled("Gathering telemetry...", Style::default().fg(Color::DarkGray)))]
                            };
                            let detail_panel = Paragraph::new(detail_text).block(
                                Block::default()
                                    .title(" [i] Specifications ")
                                    .borders(Borders::ALL)
                                    .border_type(BorderType::Rounded)
                                    .border_style(Style::default().fg(Color::DarkGray)),
                            );
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
                                .block(
                                    Block::default()
                                        .title(" [#] RAM Utilization ")
                                        .borders(Borders::ALL)
                                        .border_type(BorderType::Rounded)
                                        .border_style(Style::default().fg(Color::DarkGray)),
                                )
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
                                .title(Span::styled("Timeline", Style::default().fg(Color::DarkGray)))
                                .bounds([0.0, HISTORY_LEN as f64])
                                .labels(["past", "now"]);
                            let y_axis = Axis::default()
                                .title(Span::styled("Load", Style::default().fg(Color::DarkGray)))
                                .bounds([0.0, 100.0])
                                .labels(["0%", "50%", "100%"]);

                            let ram_chart = Chart::new(vec![ram_dataset])
                                .block(
                                    Block::default()
                                        .title(" [~] Historical Trend ")
                                        .borders(Borders::ALL)
                                        .border_type(BorderType::Rounded)
                                        .border_style(Style::default().fg(Color::DarkGray)),
                                )
                                .x_axis(x_axis)
                                .y_axis(y_axis);
                            f.render_widget(ram_chart, detail_rows[1]);

                            let detail_text: Vec<Line> = if let Some(snapshot) = &ram_snapshot {
                                vec![
                                    Line::from(Span::styled(
                                        "Software Telemetry",
                                        Style::default().add_modifier(Modifier::UNDERLINED),
                                    )),
                                    Line::from(format!("In Use           : {:.2} GB", snapshot.soft_in_use)),
                                    Line::from(format!("Available        : {:.2} GB", snapshot.soft_available)),
                                    Line::from(format!("Committed        : {:.2} GB", snapshot.soft_in_commited)),
                                    Line::from(""),
                                    Line::from(Span::styled(
                                        "Hardware Specs",
                                        Style::default().add_modifier(Modifier::UNDERLINED),
                                    )),
                                    Line::from(format!("Total Capacity   : {:.2} GB", snapshot.hard_capacity)),
                                    Line::from(format!("Memory Speed     : {} MHz", snapshot.hard_speed)),
                                    Line::from(format!("Slots Used       : {}", snapshot.hard_slots_used)),
                                ]
                            } else {
                                vec![Line::from(Span::styled("Gathering telemetry...", Style::default().fg(Color::DarkGray)))]
                            };
                            let detail_panel = Paragraph::new(detail_text).block(
                                Block::default()
                                    .title(" [i] Specifications ")
                                    .borders(Borders::ALL)
                                    .border_type(BorderType::Rounded)
                                    .border_style(Style::default().fg(Color::DarkGray)),
                            );
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
                                .block(
                                    Block::default()
                                        .title(" [#] Disk Active Time ")
                                        .borders(Borders::ALL)
                                        .border_type(BorderType::Rounded)
                                        .border_style(Style::default().fg(Color::DarkGray)),
                                )
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
                                .title(Span::styled("Timeline", Style::default().fg(Color::DarkGray)))
                                .bounds([0.0, HISTORY_LEN as f64])
                                .labels(["past", "now"]);
                            let y_axis = Axis::default()
                                .title(Span::styled("Load", Style::default().fg(Color::DarkGray)))
                                .bounds([0.0, 100.0])
                                .labels(["0%", "50%", "100%"]);

                            let disk_chart = Chart::new(vec![disk_dataset])
                                .block(
                                    Block::default()
                                        .title(" [~] Historical Trend ")
                                        .borders(Borders::ALL)
                                        .border_type(BorderType::Rounded)
                                        .border_style(Style::default().fg(Color::DarkGray)),
                                )
                                .x_axis(x_axis)
                                .y_axis(y_axis);
                            f.render_widget(disk_chart, detail_rows[1]);

                            let detail_text: Vec<Line> = if let Some(snapshot) = &disk_snapshot {
                                vec![
                                    Line::from(Span::styled(
                                        "Software Telemetry",
                                        Style::default().add_modifier(Modifier::UNDERLINED),
                                    )),
                                    Line::from(format!("Read Speed       : {:.2} MB/s", snapshot.soft_read_speed)),
                                    Line::from(format!("Write Speed      : {:.2} MB/s", snapshot.soft_write_speed)),
                                    Line::from(format!("Avg Response     : {:.2} ms", snapshot.soft_average_response_time)),
                                    Line::from(""),
                                    Line::from(Span::styled(
                                        "Hardware Specs",
                                        Style::default().add_modifier(Modifier::UNDERLINED),
                                    )),
                                    Line::from(format!("Capacity         : {} GB", snapshot.hard_capacity)),
                                    Line::from(format!("Disk Type        : {}", snapshot.hard_type)),
                                ]
                            } else {
                                vec![Line::from(Span::styled("Gathering telemetry...", Style::default().fg(Color::DarkGray)))]
                            };
                            let detail_panel = Paragraph::new(detail_text).block(
                                Block::default()
                                    .title(" [i] Specifications ")
                                    .borders(Borders::ALL)
                                    .border_type(BorderType::Rounded)
                                    .border_style(Style::default().fg(Color::DarkGray)),
                            );
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
                            let gauge_percent = ((throughput / 100.0) * 100.0).clamp(0.0, 100.0);
                            let usage_gauge = Gauge::default()
                                .block(
                                    Block::default()
                                        .title(" [#] Wireless Throughput ")
                                        .borders(Borders::ALL)
                                        .border_type(BorderType::Rounded)
                                        .border_style(Style::default().fg(Color::DarkGray)),
                                )
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
                                .name("Mbps")
                                .marker(Marker::Braille)
                                .graph_type(GraphType::Line)
                                .style(Style::default().fg(Color::Magenta))
                                .data(&wifi_points);

                            let x_axis = Axis::default()
                                .title(Span::styled("Timeline", Style::default().fg(Color::DarkGray)))
                                .bounds([0.0, HISTORY_LEN as f64])
                                .labels(["past", "now"]);
                            let y_axis = Axis::default()
                                .title(Span::styled("Throughput", Style::default().fg(Color::DarkGray)))
                                .bounds([0.0, 100.0])
                                .labels(["0", "50", "100"]);

                            let wifi_chart = Chart::new(vec![wifi_dataset])
                                .block(
                                    Block::default()
                                        .title(" [~] Historical Trend ")
                                        .borders(Borders::ALL)
                                        .border_type(BorderType::Rounded)
                                        .border_style(Style::default().fg(Color::DarkGray)),
                                )
                                .x_axis(x_axis)
                                .y_axis(y_axis);
                            f.render_widget(wifi_chart, detail_rows[1]);

                            let detail_text: Vec<Line> = if let Some(snapshot) = &wifi_snapshot {
                                vec![
                                    Line::from(Span::styled(
                                        "Software Telemetry",
                                        Style::default().add_modifier(Modifier::UNDERLINED),
                                    )),
                                    Line::from(format!("SSID             : {}", snapshot.soft_ssid)),
                                    Line::from(format!("IPv4 Address     : {}", snapshot.soft_ipv4_address)),
                                    Line::from(format!("Signal Strength  : {}%", snapshot.soft_signal_strength)),
                                    Line::from(""),
                                    Line::from(Span::styled(
                                        "Hardware Specs",
                                        Style::default().add_modifier(Modifier::UNDERLINED),
                                    )),
                                    Line::from(format!("Adapter Interface: {}", snapshot.hard_name_wireless_lan)),
                                    Line::from(format!("Tx / Rx Speed    : {:.2} / {:.2} Mbps", snapshot.hard_send, snapshot.hard_receive)),
                                ]
                            } else {
                                vec![Line::from(Span::styled("Gathering telemetry...", Style::default().fg(Color::DarkGray)))]
                            };
                            let detail_panel = Paragraph::new(detail_text).block(
                                Block::default()
                                    .title(" [i] Specifications ")
                                    .borders(Borders::ALL)
                                    .border_type(BorderType::Rounded)
                                    .border_style(Style::default().fg(Color::DarkGray)),
                            );
                            f.render_widget(detail_panel, detail_rows[2]);
                        }
                    }
                }
            }
        })?;

        // Tangani event keyboard secara non-blocking
        while event::poll(Duration::from_millis(0))? {
            if let Event::Key(key) = event::read()? {
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
