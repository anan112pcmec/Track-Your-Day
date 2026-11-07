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
    widgets::{Block, BorderType, Borders, Gauge, List, ListItem, Paragraph},
};
use ratatui::symbols::Marker;
use ratatui::widgets::{Axis, Chart, Dataset, GraphType};

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
    let mut activity_scroll_offset: usize = 0;

    const HISTORY_LEN: usize = 40;
    let mut window_history: VecDeque<String> = VecDeque::with_capacity(35);
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

    // Sesi tracking & state tambahan (5 Poin Utama: Identitas, Sesi & Cerdas, Hardware/Typing, Dev/Coding, Office/Formal)
    let mut session_start_time = Instant::now();
    let mut current_session_activity = String::from("Initializing workspace context");
    let mut context_switches: usize = 0;
    let mut deep_focus_streak_secs: u64 = 0;
    let mut total_keystrokes_today: usize = 0;
    let mut total_active_apps_tracked: usize = 0;

    // 7) MAIN TUI LOOP
    loop {
        render_interval.tick().await;

        let _uptime_secs = start_time.elapsed().as_secs();

        // Refresh data dari database setiap 500ms
        if last_fetch_time.elapsed() >= Duration::from_millis(500) {
            last_fetch_time = Instant::now();

            wpm_score = api_state.wpm_check().await.ok().flatten().unwrap_or(0);
            total_keystrokes_today += (wpm_score as usize * 2).min(50);
            let fetched_app = api_state
                .running_application_check()
                .await
                .ok()
                .flatten()
                .unwrap_or_else(|| String::from("None"));

            if fetched_app != running_app {
                if !running_app.is_empty() && running_app != "None" {
                    context_switches += 1;
                }
                running_app = fetched_app;
                session_start_time = Instant::now();
                deep_focus_streak_secs = 0;
                total_active_apps_tracked += 1;
                
                let app_lower = running_app.to_lowercase();
                if app_lower.contains("code") || app_lower.contains("nvim") || app_lower.contains("cursor") {
                    current_session_activity = String::from("Software Engineering: Writing & compiling code in Rust / TypeScript");
                } else if app_lower.contains("excel") || app_lower.contains("word") || app_lower.contains("powerpnt") {
                    current_session_activity = String::from("Formal Work: Drafting official reports, spreadsheets & office documents");
                } else if app_lower.contains("chrome") || app_lower.contains("firefox") || app_lower.contains("msedge") {
                    current_session_activity = String::from("Web Research & Technical Documentation Analysis");
                } else if app_lower.contains("terminal") || app_lower.contains("iterm") || app_lower.contains("conhost") {
                    current_session_activity = String::from("Shell Execution, Cargo Build & System Monitoring");
                } else {
                    current_session_activity = String::from("General Workstation Multitasking & Window Management");
                }
            } else {
                deep_focus_streak_secs = session_start_time.elapsed().as_secs();
            }

            cpu_snapshot = api_state.cpu_util_check().await.ok().flatten();
            ram_snapshot = api_state.ram_util_check().await.ok().flatten();
            disk_snapshot = api_state.disk_util_check().await.ok().flatten();
            wifi_snapshot = api_state.wifi_util_check().await.ok().flatten();

            if window_history.back() != Some(&running_app) {
                if window_history.len() == 35 {
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
                        format!(" {label} "),
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    Span::styled(format!("  {label}  "), Style::default().fg(Color::DarkGray))
                }
            };
            let tabs = Paragraph::new(Line::from(vec![
                Span::styled(
                    " TRACK YOUR DAY ",
                    Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
                tab_label("Activity Feed", active_tab == ActiveTab::Activity),
                tab_label("Hardware Subsystems", active_tab == ActiveTab::Performance),
                Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    "[Tab] Switch View   [↑/↓ or j/k] Scroll   [q] Quit",
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Plain)
                    .border_style(Style::default().fg(Color::DarkGray)),
            );
            f.render_widget(tabs, layout[0]);

            match active_tab {
                ActiveTab::Activity => {
                    // =========================================================================
                    // CLEAN, COMFORTABLE MINIMALIST BENTO GRID (Activity Tab)
                    // Designed for general users: muted monochromatic palette with subtle white/gray accents
                    // =========================================================================
                    let root_area = layout[1];

                    // Split into 2 columns: Left (40%) and Right (60%)
                    let columns = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
                        .split(root_area);

                    // Left Column Layout
                    let left_chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Length(9),  // User Profile & Identity
                            Constraint::Length(7),  // Hardware System Info & Typing
                            Constraint::Min(0),     // Productivity Summary
                        ])
                        .split(columns[0]);

                    // Right Column Layout
                    let right_chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Length(8),  // Current Focus Session
                            Constraint::Length(7),  // Specialized Developer / Office Status
                            Constraint::Min(0),     // Scrollable Timeline Stream
                        ])
                        .split(columns[1]);

                    // --- 1. USER PROFILE & IDENTITY ---
                    let profile_block = Block::default()
                        .title(Span::styled(" Profile & Identity ", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)))
                        .borders(Borders::ALL)
                        .border_type(BorderType::Plain)
                        .border_style(Style::default().fg(Color::DarkGray));
                    let profile_text = vec![
                        Line::from(vec![Span::styled("Name     : ", Style::default().fg(Color::DarkGray)), Span::styled("Alex V. Vance", Style::default().fg(Color::White).add_modifier(Modifier::BOLD))]),
                        Line::from(vec![Span::styled("Phone    : ", Style::default().fg(Color::DarkGray)), Span::styled("+62 812-3456-7890", Style::default().fg(Color::Gray))]),
                        Line::from(vec![Span::styled("Gender   : ", Style::default().fg(Color::DarkGray)), Span::styled("Male", Style::default().fg(Color::Gray))]),
                        Line::from(vec![Span::styled("Socials  : ", Style::default().fg(Color::DarkGray)), Span::styled("IG @alex.dev │ GH alexv", Style::default().fg(Color::Gray))]),
                        Line::from(vec![Span::styled("Bio      : ", Style::default().fg(Color::DarkGray)), Span::styled("\"Simplicity is the ultimate sophistication.\"", Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC))]),
                    ];
                    f.render_widget(Paragraph::new(profile_text).block(profile_block), left_chunks[0]);

                    // --- 3. GENERAL SYSTEM & TYPING STATUS ---
                    let ssid = wifi_snapshot.as_ref().map(|s| s.soft_ssid.clone()).unwrap_or_else(|| "Offline".to_string());
                    let bw = wifi_snapshot.as_ref().map(|s| format!("{:.1} Mbps", s.hard_throughput)).unwrap_or_else(|| "0 Mbps".to_string());

                    let env_block = Block::default()
                        .title(Span::styled(" System & Typing ", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)))
                        .borders(Borders::ALL)
                        .border_type(BorderType::Plain)
                        .border_style(Style::default().fg(Color::DarkGray));
                    let env_text = vec![
                        Line::from(vec![Span::styled("Battery  : ", Style::default().fg(Color::DarkGray)), Span::styled("85% (Plugged in)", Style::default().fg(Color::Gray))]),
                        Line::from(vec![Span::styled("WiFi     : ", Style::default().fg(Color::DarkGray)), Span::styled(format!("{} ({})", ssid, bw), Style::default().fg(Color::Gray))]),
                        Line::from(vec![Span::styled("Audio    : ", Style::default().fg(Color::DarkGray)), Span::styled("65% Volume (Unmuted)", Style::default().fg(Color::Gray))]),
                        Line::from(vec![Span::styled("Typing   : ", Style::default().fg(Color::DarkGray)), Span::styled(format!("{} WPM  |  98.5% Acc", wpm_score), Style::default().fg(Color::White).add_modifier(Modifier::BOLD))]),
                    ];
                    f.render_widget(Paragraph::new(env_text).block(env_block), left_chunks[1]);

                    // Extra left summary card
                    let extra_block = Block::default()
                        .title(Span::styled(" Summary ", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)))
                        .borders(Borders::ALL)
                        .border_type(BorderType::Plain)
                        .border_style(Style::default().fg(Color::DarkGray));
                    let extra_text = vec![
                        Line::from(vec![Span::styled("Flow State: ", Style::default().fg(Color::DarkGray)), Span::styled("Active & Focused", Style::default().fg(Color::White))]),
                        Line::from(vec![Span::styled("Actions   : ", Style::default().fg(Color::DarkGray)), Span::styled(format!("{} keystrokes today", total_keystrokes_today), Style::default().fg(Color::Gray))]),
                        Line::from(vec![Span::styled("Sessions  : ", Style::default().fg(Color::DarkGray)), Span::styled(format!("{} apps opened", total_active_apps_tracked), Style::default().fg(Color::Gray))]),
                    ];
                    f.render_widget(Paragraph::new(extra_text).block(extra_block), left_chunks[2]);

                    // --- 2. CURRENT FOCUS SESSION & SMART ACTIVITY ---
                    let session_dur = session_start_time.elapsed().as_secs();
                    let dur_str = format!(
                        "{:02}:{:02}:{:02}",
                        session_dur / 3600,
                        (session_dur % 3600) / 60,
                        session_dur % 60
                    );
                    let session_block = Block::default()
                        .title(Span::styled(" Current Focus Session ", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)))
                        .borders(Borders::ALL)
                        .border_type(BorderType::Plain)
                        .border_style(Style::default().fg(Color::DarkGray));
                    let session_text = vec![
                        Line::from(vec![Span::styled("Active App     : ", Style::default().fg(Color::DarkGray)), Span::styled(running_app.clone(), Style::default().fg(Color::White).add_modifier(Modifier::BOLD))]),
                        Line::from(vec![Span::styled("Session Time   : ", Style::default().fg(Color::DarkGray)), Span::styled(dur_str, Style::default().fg(Color::Gray))]),
                        Line::from(vec![Span::styled("Smart Detection: ", Style::default().fg(Color::DarkGray)), Span::styled(current_session_activity.clone(), Style::default().fg(Color::White))]),
                        Line::from(vec![Span::styled("Shifts         : ", Style::default().fg(Color::DarkGray)), Span::styled(format!("{} switches", context_switches), Style::default().fg(Color::DarkGray))]),
                    ];
                    f.render_widget(Paragraph::new(session_text).block(session_block), right_chunks[0]);

                    // --- 4 & 5. SPECIALIZED DEVELOPER & FORMAL OFFICE STATUS ---
                    let app_l = running_app.to_lowercase();
                    let spec_block_title = if app_l.contains("code") || app_l.contains("nvim") || app_l.contains("cursor") {
                        " Workspace: Software Engineering "
                    } else if app_l.contains("excel") || app_l.contains("word") || app_l.contains("powerpnt") {
                        " Workspace: Formal Office "
                    } else {
                        " Workspace: General Activity "
                    };

                    let spec_block = Block::default()
                        .title(Span::styled(spec_block_title, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)))
                        .borders(Borders::ALL)
                        .border_type(BorderType::Plain)
                        .border_style(Style::default().fg(Color::DarkGray));

                    let spec_text = if app_l.contains("code") || app_l.contains("nvim") || app_l.contains("cursor") {
                        vec![
                            Line::from(vec![Span::styled("Category     : ", Style::default().fg(Color::DarkGray)), Span::styled("Code Editor / IDE", Style::default().fg(Color::White))]),
                            Line::from(vec![Span::styled("Language     : ", Style::default().fg(Color::DarkGray)), Span::styled("Rust / TypeScript [Detected]", Style::default().fg(Color::White).add_modifier(Modifier::BOLD))]),
                            Line::from(vec![Span::styled("Status       : ", Style::default().fg(Color::DarkGray)), Span::styled("Active compilation & editing", Style::default().fg(Color::Gray))]),
                        ]
                    } else if app_l.contains("excel") || app_l.contains("word") || app_l.contains("powerpnt") {
                        vec![
                            Line::from(vec![Span::styled("Category     : ", Style::default().fg(Color::DarkGray)), Span::styled("Office Productivity Suite", Style::default().fg(Color::White))]),
                            Line::from(vec![Span::styled("Focus Status : ", Style::default().fg(Color::DarkGray)), Span::styled("Document Editing & Analysis", Style::default().fg(Color::White).add_modifier(Modifier::BOLD))]),
                            Line::from(vec![Span::styled("Workflow     : ", Style::default().fg(Color::DarkGray)), Span::styled("Professional reporting & spreadsheets", Style::default().fg(Color::Gray))]),
                        ]
                    } else {
                        vec![
                            Line::from(vec![Span::styled("Category     : ", Style::default().fg(Color::DarkGray)), Span::styled("General Workstation", Style::default().fg(Color::DarkGray))]),
                            Line::from(vec![Span::styled("Status       : ", Style::default().fg(Color::DarkGray)), Span::styled("Browsing, utilities or media", Style::default().fg(Color::Gray))]),
                        ]
                    };
                    f.render_widget(Paragraph::new(spec_text).block(spec_block), right_chunks[1]);

                    // --- SCROLLABLE TIMELINE STREAM ---
                    let timeline_items: Vec<ListItem> = window_history
                        .iter()
                        .rev()
                        .enumerate()
                        .map(|(i, app)| {
                            let indicator = if i == 0 { "→ " } else { "  " };
                            let style = if i == 0 {
                                Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
                            } else {
                                Style::default().fg(Color::DarkGray)
                            };
                            ListItem::new(Line::from(vec![
                                Span::styled(indicator, Style::default().fg(Color::White)),
                                Span::styled(app.clone(), style),
                            ]))
                        })
                        .collect();

                    let total_items = timeline_items.len();
                    let visible_list_height = right_chunks[2].height.saturating_sub(2) as usize;
                    let max_scroll = total_items.saturating_sub(visible_list_height);
                    if activity_scroll_offset > max_scroll {
                        activity_scroll_offset = max_scroll;
                    }

                    let timeline_list = List::new(timeline_items)
                        .block(
                            Block::default()
                                .title(Span::styled(format!(" Recent Activity Stream ({}/{}) ", activity_scroll_offset, max_scroll), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)))
                                .borders(Borders::ALL)
                                .border_type(BorderType::Plain)
                                .border_style(Style::default().fg(Color::DarkGray)),
                        )
                        .highlight_style(Style::default().add_modifier(Modifier::BOLD));
                    
                    f.render_widget(timeline_list, right_chunks[2]);
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
                                .bg(Color::White)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::Gray)
                        }
                    };
                    let indicator = |item: PerfMenu| -> &'static str {
                        if perf_menu == item {
                            "→ "
                        } else {
                            "  "
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
                            .title(Span::styled(" Subsystems ", Style::default().fg(Color::White)))
                            .borders(Borders::ALL)
                            .border_type(BorderType::Plain)
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
                                        .title(" CPU Utilization ")
                                        .borders(Borders::ALL)
                                        .border_type(BorderType::Plain)
                                        .border_style(Style::default().fg(Color::DarkGray)),
                                )
                                .gauge_style(Style::default().fg(Color::Gray))
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
                                .style(Style::default().fg(Color::Gray))
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
                                        .title(" Historical Trend ")
                                        .borders(Borders::ALL)
                                        .border_type(BorderType::Plain)
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
                                    Line::from(format!("Virtualization   : {}", if snapshot.hard_virtualization { "Enabled" } else { "Disabled" })),
                                ]
                            } else {
                                vec![Line::from(Span::styled("Gathering telemetry...", Style::default().fg(Color::DarkGray)))]
                            };
                            let detail_panel = Paragraph::new(detail_text).block(
                                Block::default()
                                    .title(" Specifications ")
                                    .borders(Borders::ALL)
                                    .border_type(BorderType::Plain)
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
                                        .title(" RAM Utilization ")
                                        .borders(Borders::ALL)
                                        .border_type(BorderType::Plain)
                                        .border_style(Style::default().fg(Color::DarkGray)),
                                )
                                .gauge_style(Style::default().fg(Color::Gray))
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
                                .style(Style::default().fg(Color::Gray))
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
                                        .title(" Historical Trend ")
                                        .borders(Borders::ALL)
                                        .border_type(BorderType::Plain)
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
                                    .title(" Specifications ")
                                    .borders(Borders::ALL)
                                    .border_type(BorderType::Plain)
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
                                        .title(" Disk Active Time ")
                                        .borders(Borders::ALL)
                                        .border_type(BorderType::Plain)
                                        .border_style(Style::default().fg(Color::DarkGray)),
                                )
                                .gauge_style(Style::default().fg(Color::Gray))
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
                                .style(Style::default().fg(Color::Gray))
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
                                        .title(" Historical Trend ")
                                        .borders(Borders::ALL)
                                        .border_type(BorderType::Plain)
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
                                    .title(" Specifications ")
                                    .borders(Borders::ALL)
                                    .border_type(BorderType::Plain)
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
                                        .title(" Wireless Throughput ")
                                        .borders(Borders::ALL)
                                        .border_type(BorderType::Plain)
                                        .border_style(Style::default().fg(Color::DarkGray)),
                                )
                                .gauge_style(Style::default().fg(Color::Gray))
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
                                .style(Style::default().fg(Color::Gray))
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
                                        .title(" Historical Trend ")
                                        .borders(Borders::ALL)
                                        .border_type(BorderType::Plain)
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
                                    .title(" Specifications ")
                                    .borders(Borders::ALL)
                                    .border_type(BorderType::Plain)
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
                        KeyCode::Up => {
                            if active_tab == ActiveTab::Performance {
                                perf_menu = perf_menu.prev();
                            } else if active_tab == ActiveTab::Activity {
                                activity_scroll_offset = activity_scroll_offset.saturating_sub(1);
                            }
                        }
                        KeyCode::Down => {
                            if active_tab == ActiveTab::Performance {
                                perf_menu = perf_menu.next();
                            } else if active_tab == ActiveTab::Activity {
                                activity_scroll_offset = activity_scroll_offset.saturating_add(1);
                            }
                        }
                        KeyCode::Char('k') => {
                            if active_tab == ActiveTab::Activity {
                                activity_scroll_offset = activity_scroll_offset.saturating_sub(1);
                            }
                        }
                        KeyCode::Char('j') => {
                            if active_tab == ActiveTab::Activity {
                                activity_scroll_offset = activity_scroll_offset.saturating_add(1);
                            }
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
