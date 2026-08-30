//! `cli` — main logic & TUI interface "track your day".

use anyhow::Result;
use database::MemoryStore;
use separation::ActivityStore;
use std::collections::VecDeque;
use std::io::stdout;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use watcher_mutator::MutatorWatcher;
use watcher_reactive::{LoggingReactor, ReactiveWatcher};

use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    prelude::*,
    widgets::{Block, Paragraph, Sparkline, Wrap},
};

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

/// Ambil isi VecDeque jadi Vec biasa buat dikasih ke Sparkline.
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
        0
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

    const HISTORY_LEN: usize = 40;
    let mut wpm_history: VecDeque<u64> = VecDeque::with_capacity(HISTORY_LEN);
    let mut process_history: VecDeque<u64> = VecDeque::with_capacity(HISTORY_LEN);
    let mut uptime_history: VecDeque<u64> = VecDeque::with_capacity(HISTORY_LEN);
    let mut window_history: VecDeque<String> = VecDeque::with_capacity(5);

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
                Span::styled("[Tab] switch   [q] exit", Style::default().fg(Color::DarkGray)),
            ]))
            .block(Block::bordered());
            f.render_widget(tabs, layout[0]);

            match active_tab {
                ActiveTab::Activity => {
                    let rows = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([Constraint::Length(3); 4])
                        .split(layout[1]);

                    // Vec-nya di-`let` dulu supaya tetap hidup selagi dipakai sparkline.
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
                    let rows = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([Constraint::Percentage(25); 4])
                        .split(layout[1]);

                    let placeholder = |title: &str| {
                        Paragraph::new(Span::styled(
                            "belum diisi — nyusul besok",
                            Style::default().fg(Color::DarkGray),
                        ))
                        .block(Block::bordered().title(format!(" {title} ")))
                    };
                    f.render_widget(placeholder("CPU"), rows[0]);
                    f.render_widget(placeholder("Memory"), rows[1]);
                    f.render_widget(placeholder("Disk"), rows[2]);
                    f.render_widget(placeholder("WiFi / Network"), rows[3]);
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
                    _ => {}
                }
            }
        }
    }

    // 8) TEARDOWN TASKS
    mutator_handle.abort();
    reactive_handle.abort();

    Ok(())
}