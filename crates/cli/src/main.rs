//! `cli` — main logic & TUI interface "track your day".

use anyhow::Result;
use database::MemoryStore;
use separation::{ActivityStore, CpuSnapshot, DiskSnapshot, RamSnapshot};
use std::collections::VecDeque;
use std::io::stdout;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use watcher_mutator::{CpuUtilData, DiskUtilData, MutatorWatcher, RamUtilData};
use watcher_reactive::{LoggingReactor, ReactiveWatcher};

use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    prelude::*,
    widgets::{Block, Gauge, Paragraph, Sparkline, Wrap},
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

/// Ambil isi VecDeque jadi Vec biasa buat dikasih ke Sparkline/Chart.
/// HARUS di-`let` ke variabel dulu sebelum dipakai widget — kalau
/// dipanggil inline di tengah argumen, hasilnya adalah temporary yang
/// mati duluan sebelum sempat dipakai (borrow checker bakal nolak).
fn wpm_data_slice(history: &VecDeque<u64>) -> Vec<u64> {
    history.iter().copied().collect()
}

#[derive(PartialEq, Clone, Copy)]
enum ActiveTab {
    Activity,
    Performance,
}

/// Menu kiri di tab Performance — sekarang beneran bisa dipindah pakai
/// panah atas/bawah, bukan cuma dekorasi statis.
#[derive(PartialEq, Clone, Copy)]
enum PerfMenu {
    Cpu,
    Memory,
    Disk,
    Wifi,
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
        DiskUtilData::new()
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
    let mut render_interval = tokio::time::interval(Duration::from_millis(100));

    let mut active_tab = ActiveTab::Activity;
    let mut perf_menu = PerfMenu::Cpu;
    let mut current_menu: u8 = 0; // 0=Cpu, 1=Memory, 2=Disk, 3=Wifi   <- PINDAH KE SINI

    const HISTORY_LEN: usize = 40;
    let mut wpm_history: VecDeque<u64> = VecDeque::with_capacity(HISTORY_LEN);
    let mut process_history: VecDeque<u64> = VecDeque::with_capacity(HISTORY_LEN);
    let mut uptime_history: VecDeque<u64> = VecDeque::with_capacity(HISTORY_LEN);
    let mut window_history: VecDeque<String> = VecDeque::with_capacity(5);
    let mut cpu_history: VecDeque<u64> = VecDeque::with_capacity(HISTORY_LEN);
    let mut ram_history: VecDeque<u64> = VecDeque::with_capacity(HISTORY_LEN);
    let mut disk_history: VecDeque<u64> = VecDeque::with_capacity(HISTORY_LEN);

    // 7) MAIN TUI LOOP
    loop {
        render_interval.tick().await;

        let uptime_secs = start_time.elapsed().as_secs();
        let wpm_score: u32 = api_state.wpm_check().await.ok().flatten().unwrap_or(0);
        let running_app: String = api_state
            .running_application_check()
            .await
            .ok()
            .flatten()
            .unwrap_or(String::from("None"));
        let current_process: usize = api_state
            .running_process_check()
            .await
            .ok()
            .flatten()
            .unwrap_or(0);
        // Ambil snapshot CPU & RAM terbaru dari database lewat api_state.
        let cpu_snapshot: Option<CpuSnapshot> = api_state.cpu_util_check().await.ok().flatten();
        let ram_snapshot: Option<RamSnapshot> = api_state.ram_util_check().await.ok().flatten();
        let disk_snapshot: Option<DiskSnapshot> = api_state.disk_util_check().await.ok().flatten();

        if wpm_history.len() == HISTORY_LEN {
            wpm_history.pop_front();
        }
        wpm_history.push_back(wpm_score as u64);

        if process_history.len() == HISTORY_LEN {
            process_history.pop_front();
        }
        process_history.push_back(current_process as u64);

        if uptime_history.len() == HISTORY_LEN {
            uptime_history.pop_front();
        }
        uptime_history.push_back(uptime_secs);

        if window_history.back() != Some(&running_app) {
            if window_history.len() == 5 {
                window_history.pop_front();
            }
            window_history.push_back(running_app.clone());
        }

        // History buat chart CPU usage.
        if cpu_history.len() == HISTORY_LEN {
            cpu_history.pop_front();
        }
        cpu_history.push_back(
            cpu_snapshot
                .as_ref()
                .map(|s| s.soft_utilization.round() as u64)
                .unwrap_or(0),
        );

        // History buat chart RAM usage — dihitung sebagai persen dari kapasitas.
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

        if disk_history.len() == HISTORY_LEN{
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
            }).unwrap_or(0),
        );

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
                    " track your day ",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" │ "),
                tab_label("Your Activity", active_tab == ActiveTab::Activity),
                tab_label("Performance", active_tab == ActiveTab::Performance),
                Span::raw(" │ "),
                Span::styled("[Tab] switch   [↑↓] pilih util   [q] exit", Style::default().fg(Color::DarkGray)),
            ]))
            .block(Block::bordered());
            f.render_widget(tabs, layout[0]);

            match active_tab {
                ActiveTab::Activity => {
                    let rows = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([Constraint::Length(3); 4])
                        .split(layout[1]);

                    let wpm_data = wpm_data_slice(&wpm_history);
                    let process_data = wpm_data_slice(&process_history);

                    let metric_row = |label: &str, value: String, color: Color, area: Rect| -> (Paragraph, std::rc::Rc<[Rect]>) {
                        let cols = Layout::default()
                            .direction(Direction::Horizontal)
                            .constraints([Constraint::Length(28), Constraint::Min(0)])
                            .split(area);
                        let text = Paragraph::new(Line::from(vec![
                            Span::styled(format!("{label:<16}"), Style::default().fg(Color::DarkGray)),
                            Span::styled(value, Style::default().fg(color).add_modifier(Modifier::BOLD)),
                        ]));
                        (text, cols)
                    };

                    let (text, cols) = metric_row("Typing Speed", format!("{wpm_score} WPM"), Color::Cyan, rows[0]);
                    f.render_widget(text, cols[0]);
                    f.render_widget(
                        Sparkline::default().data(&wpm_data).style(Style::default().fg(Color::Cyan)),
                        cols[1],
                    );

                    let (text, _) = metric_row("Active Window", running_app.clone(), Color::Magenta, rows[1]);
                    f.render_widget(text, rows[1]);

                    let (text, cols) = metric_row("Processes", format!("{current_process}"), Color::Yellow, rows[2]);
                    f.render_widget(text, cols[0]);
                    f.render_widget(
                        Sparkline::default().data(&process_data).style(Style::default().fg(Color::Yellow)),
                        cols[1],
                    );

                    let (text, _) = metric_row("Uptime", format!("{uptime_secs}s"), Color::Blue, rows[3]);
                    f.render_widget(text, rows[3]);
                }
                ActiveTab::Performance => {
                    let sections = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
                        .split(layout[1]);

                    // ---------------- KIRI (30%): daftar util yang bisa dipilih ----------------
                    let menu_style = |item: PerfMenu, enabled: bool| -> Style {
                        if perf_menu == item {
                            Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
                        } else if enabled {
                            Style::default().fg(Color::White)
                        } else {
                            Style::default().fg(Color::DarkGray)
                        }
                    };
                    let arrow = |item: PerfMenu| -> &'static str { if perf_menu == item { "▶ " } else { "  " } };

                    let menu_items = vec![
                        Line::from(Span::styled(format!("{}CPU        ", arrow(PerfMenu::Cpu)), menu_style(PerfMenu::Cpu, true))),
                        Line::from(Span::styled(format!("{}Memory     ", arrow(PerfMenu::Memory)), menu_style(PerfMenu::Memory, true))),
                        Line::from(Span::styled(format!("{}Disk       ", arrow(PerfMenu::Disk)), menu_style(PerfMenu::Disk, false))),
                        Line::from(Span::styled(format!("{}WiFi       ", arrow(PerfMenu::Wifi)), menu_style(PerfMenu::Wifi, false))),
                    ];
                    let menu = Paragraph::new(menu_items).block(Block::bordered().title(" Utilities "));
                    f.render_widget(menu, sections[0]);

                    // ---------------- KANAN (70%): detail sesuai menu yang dipilih ----------------
                    match perf_menu {
                        PerfMenu::Cpu => {
                            let detail_rows = Layout::default()
                                .direction(Direction::Vertical)
                                .constraints([Constraint::Length(3), Constraint::Min(10), Constraint::Min(0)])
                                .split(sections[1]);

                            let usage_percent = cpu_snapshot.as_ref().map(|s| s.soft_utilization).unwrap_or(0.0);
                            let usage_gauge = Gauge::default()
                                .block(Block::bordered().title(" CPU Usage "))
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
                                .title(Span::styled("Waktu", Style::default().fg(Color::DarkGray)))
                                .bounds([0.0, HISTORY_LEN as f64])
                                .labels(["awal", "sekarang"]);
                            let y_axis = Axis::default()
                                .title(Span::styled("Persen", Style::default().fg(Color::DarkGray)))
                                .bounds([0.0, 100.0])
                                .labels(["0", "50", "100"]);

                            let cpu_chart = Chart::new(vec![cpu_dataset])
                                .block(Block::bordered().title(" History "))
                                .x_axis(x_axis)
                                .y_axis(y_axis);
                            f.render_widget(cpu_chart, detail_rows[1]);

                            let detail_text: Vec<Line> = if let Some(snapshot) = &cpu_snapshot {
                                vec![
                                    Line::from(Span::styled("Software (live)", Style::default().add_modifier(Modifier::UNDERLINED))),
                                    Line::from(format!("Processes        : {}", snapshot.soft_processes)),
                                    Line::from(format!("Threads          : {}", snapshot.soft_threads)),
                                    Line::from(format!("Handles          : {}", snapshot.soft_handles)),
                                    Line::from(format!("Speed sekarang   : {:.0} MHz", snapshot.soft_speed_clock)),
                                    Line::from(format!("Uptime           : {}", snapshot.soft_uptime)),
                                    Line::from(""),
                                    Line::from(Span::styled("Hardware (statis)", Style::default().add_modifier(Modifier::UNDERLINED))),
                                    Line::from(format!("Base speed       : {:.0} MHz", snapshot.hard_base_speed)),
                                    Line::from(format!("Sockets          : {}", snapshot.hard_sockets)),
                                    Line::from(format!("Cores            : {}", snapshot.hard_cores)),
                                    Line::from(format!("Logical procs    : {}", snapshot.hard_logical_processors)),
                                    Line::from(format!(
                                        "Virtualization   : {}",
                                        if snapshot.hard_virtualization { "Enabled" } else { "Disabled" }
                                    )),
                                    Line::from(format!("L1 cache         : {:.0} KB", snapshot.hard_l1_cache)),
                                    Line::from(format!("L2 cache         : {:.0} KB", snapshot.hard_l2_cache)),
                                    Line::from(format!("L3 cache         : {:.0} KB", snapshot.hard_l3_cache)),
                                ]
                            } else {
                                vec![Line::from(Span::styled("Menunggu data CPU pertama...", Style::default().fg(Color::DarkGray)))]
                            };
                            let detail_panel = Paragraph::new(detail_text).block(Block::bordered().title(" Detail "));
                            f.render_widget(detail_panel, detail_rows[2]);
                        }

                        PerfMenu::Memory => {
                            let detail_rows = Layout::default()
                                .direction(Direction::Vertical)
                                .constraints([Constraint::Length(3), Constraint::Min(10), Constraint::Min(0)])
                                .split(sections[1]);

                            let usage_percent = ram_snapshot
                                .as_ref()
                                .map(|s| if s.hard_capacity > 0.0 { (s.soft_in_use / s.hard_capacity) * 100.0 } else { 0.0 })
                                .unwrap_or(0.0);
                            let usage_gauge = Gauge::default()
                                .block(Block::bordered().title(" RAM Usage "))
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
                                .title(Span::styled("Waktu", Style::default().fg(Color::DarkGray)))
                                .bounds([0.0, HISTORY_LEN as f64])
                                .labels(["awal", "sekarang"]);
                            let y_axis = Axis::default()
                                .title(Span::styled("Persen", Style::default().fg(Color::DarkGray)))
                                .bounds([0.0, 100.0])
                                .labels(["0", "50", "100"]);

                            let ram_chart = Chart::new(vec![ram_dataset])
                                .block(Block::bordered().title(" History "))
                                .x_axis(x_axis)
                                .y_axis(y_axis);
                            f.render_widget(ram_chart, detail_rows[1]);

                            let detail_text: Vec<Line> = if let Some(snapshot) = &ram_snapshot {
                                vec![
                                    Line::from(Span::styled("Software (live)", Style::default().add_modifier(Modifier::UNDERLINED))),
                                    Line::from(format!("In use           : {:.2} GB", snapshot.soft_in_use)),
                                    Line::from(format!("Available        : {:.2} GB", snapshot.soft_available)),
                                    Line::from(format!("Hardware reserved: {:.2} GB", snapshot.soft_hardware_reserve)),
                                    Line::from(format!("Committed in use : {:.2} GB", snapshot.soft_in_commited)),
                                    Line::from(format!("Committed avail  : {:.2} GB", snapshot.soft_available_commited)),
                                    Line::from(format!("Cached           : {:.2} GB", snapshot.soft_cached)),
                                    Line::from(format!("Paged pool       : {:.2} GB", snapshot.soft_page_pool)),
                                    Line::from(format!("Non-paged pool   : {:.2} GB", snapshot.soft_non_paged_pool)),
                                    Line::from(""),
                                    Line::from(Span::styled("Hardware (statis)", Style::default().add_modifier(Modifier::UNDERLINED))),
                                    Line::from(format!("Capacity         : {:.2} GB", snapshot.hard_capacity)),
                                    Line::from(format!("Speed            : {} MHz", snapshot.hard_speed)),
                                    Line::from(format!("Slots terpakai   : {}", snapshot.hard_slots_used)),
                                    Line::from(format!("Form factor      : {}", snapshot.hard_form_factor)),
                                ]
                            } else {
                                vec![Line::from(Span::styled("Menunggu data RAM pertama...", Style::default().fg(Color::DarkGray)))]
                            };
                            let detail_panel = Paragraph::new(detail_text).block(Block::bordered().title(" Detail "));
                            f.render_widget(detail_panel, detail_rows[2]);
                        }

                        PerfMenu::Disk => {
                            let detail_rows = Layout::default()
                                .direction(Direction::Vertical)
                                .constraints([Constraint::Length(3), Constraint::Min(10), Constraint::Min(0)])
                                .split(sections[1]);

                            let usage_percent = disk_snapshot.as_ref().map(|s| s.soft_active_time).unwrap_or(0.0);
                            let usage_gauge = Gauge::default()
                                .block(Block::bordered().title(" Disk Active Time "))
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
                                .title(Span::styled("Waktu", Style::default().fg(Color::DarkGray)))
                                .bounds([0.0, HISTORY_LEN as f64])
                                .labels(["awal", "sekarang"]);
                            let y_axis = Axis::default()
                                .title(Span::styled("Persen", Style::default().fg(Color::DarkGray)))
                                .bounds([0.0, 100.0])
                                .labels(["0", "50", "100"]);

                            let disk_chart = Chart::new(vec![disk_dataset])
                                .block(Block::bordered().title(" History "))
                                .x_axis(x_axis)
                                .y_axis(y_axis);
                            f.render_widget(disk_chart, detail_rows[1]);

                            let detail_text: Vec<Line> = if let Some(snapshot) = &disk_snapshot {
                                vec![
                                    Line::from(Span::styled("Software (live)", Style::default().add_modifier(Modifier::UNDERLINED))),
                                    Line::from(format!("Active time      : {:.1}%", snapshot.soft_active_time)),
                                    Line::from(format!("Read speed       : {:.2} MB/s", snapshot.soft_read_speed)),
                                    Line::from(format!("Write speed      : {:.2} MB/s", snapshot.soft_write_speed)),
                                    Line::from(format!("Avg response time: {:.2} ms", snapshot.soft_average_response_time)),
                                    Line::from(""),
                                    Line::from(Span::styled("Hardware (statis)", Style::default().add_modifier(Modifier::UNDERLINED))),
                                    Line::from(format!("Capacity         : {} GB", snapshot.hard_capacity)),
                                    Line::from(format!("Formatted        : {} GB", snapshot.hard_formatted)),
                                    Line::from(format!(
                                        "System disk      : {}",
                                        if snapshot.hard_system_disk { "Yes" } else { "No" }
                                    )),
                                    Line::from(format!("Type             : {}", snapshot.hard_type)),
                                ]
                            } else {
                                vec![Line::from(Span::styled("Menunggu data Disk pertama...", Style::default().fg(Color::DarkGray)))]
                            };
                            let detail_panel = Paragraph::new(detail_text).block(Block::bordered().title(" Detail "));
                            f.render_widget(detail_panel, detail_rows[2]);
                        }

                        PerfMenu::Wifi => {
                            let placeholder = Paragraph::new(Span::styled(
                                "belum diisi — nyusul besok",
                                Style::default().fg(Color::DarkGray),
                            ))
                            .block(Block::bordered().title(" Detail "));
                            f.render_widget(placeholder, sections[1]);
                        }
                    }
                }
            }
        })?;


        if event::poll(Duration::from_millis(10))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                    KeyCode::Tab => {
                        active_tab = match active_tab {
                            ActiveTab::Activity => ActiveTab::Performance,
                            ActiveTab::Performance => ActiveTab::Activity,
                        };
                    }
                    KeyCode::Up if active_tab == ActiveTab::Performance => {
                        if current_menu > 0 {
                            current_menu -= 1;
                        }
                    }
                    KeyCode::Down if active_tab == ActiveTab::Performance => {
                        if current_menu < 3 {
                            current_menu += 1;
                        }
                    }
                    _ => {}
                }

                // Update perf_menu secara otomatis berdasarkan nilai current_menu
               perf_menu = match current_menu {
                    0 => PerfMenu::Cpu,   // <- harusnya Cpu
                    1 => PerfMenu::Memory,
                    2 => PerfMenu::Disk,
                    _ => PerfMenu::Wifi,
                };
            }
        }
    }

    // 8) TEARDOWN TASKS
    mutator_handle.abort();
    reactive_handle.abort();

    Ok(())
}