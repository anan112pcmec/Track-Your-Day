//! `cli` — main logic & TUI interface "track your day".

use anyhow::Result;
use database::MemoryStore;
use separation::ActivityStore;
use std::io::stdout;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use watcher_mutator::MutatorWatcher;
use watcher_reactive::{LoggingReactor, ReactiveWatcher};

// --- IMPORT RATATUI & CROSSTERM ---
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph, Wrap},
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

#[tokio::main]
async fn main() -> Result<()> {
    // 1) DATABASE — MemoryStore dipegang via ActivityStore trait
    let store: Arc<dyn ActivityStore> = Arc::new(MemoryStore::new());

    // 2) Broadcast channel
    let (tx, rx) = broadcast::channel(64);

    // 3) WATCHER-MUTATOR — spawn background task (menulis data tiap 2 detik)
    let mutator = MutatorWatcher::new(store.clone(), tx.clone(), Duration::from_secs(2),  watcher_mutator::KeyCounter::start());
    let mutator_handle = tokio::spawn(mutator.run());

    // 4) WATCHER-REACTIVE — spawn background task
    let reactive = ReactiveWatcher::new(rx, Box::new(LoggingReactor {verbose: false}));
    let reactive_handle = tokio::spawn(reactive.run());

    // 5) API STATE
    let api_state = api::ApiState::new(store.clone());

    // 6) INITIALIZE RATATUI TERMINAL
    let _guard = TerminalGuard::new()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    // Timer & Ticker
    let start_time = Instant::now();
    let mut render_interval = tokio::time::interval(Duration::from_millis(100));

    // 7) MAIN TUI LOOP
    loop {
        // Redraw UI setiap interval tick
        render_interval.tick().await;

        // Ambil data live dari api_state
        let event_count = api_state.health_check().await.unwrap_or(0);
        let wpm_score: u32 = api_state.wpm_check().await.ok().flatten().unwrap_or(0);
        let running_app: String = api_state.running_application_check().await.ok().flatten().unwrap_or(String::from("None"));
        let uptime_secs = start_time.elapsed().as_secs();

        // Render Frame Ratatui
        terminal.draw(|f| {
            // Layout Utama (Vertical): Header Nav (10%), First Section / Upper Body (50%), Second Section / Lower Body (40%)
            let outer_layer = Layout::default()
                .direction(Direction::Vertical)
                .margin(1)
                .constraints([
                    Constraint::Percentage(10), // Navigasi Header
                    Constraint::Percentage(50), // Konten Atas (Kotak 1 & 2)
                    Constraint::Percentage(40), // Konten Bawah (Kotak 3 & 4)
                ])
                .split(f.area());

            // First Section (Horizontal 30% : 70%) - Dibagi dari outer_layer[1]
            let first_section = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(30),
                    Constraint::Percentage(70),
                ])
                .split(outer_layer[1]);

            // Second Section (Horizontal 50% : 50%) - Dibagi dari outer_layer[2]
            let second_section = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(50),
                    Constraint::Percentage(50),
                ])
                .split(outer_layer[2]);

            // Styling dasar border
            let block_style = Block::bordered();

            // ---------------- HEADER / NAV (10%) ----------------
            let header_nav = Paragraph::new(
                Line::from(vec![
                    Span::styled(" TRACK YOUR DAY ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                    Span::raw(" | "),
                    Span::styled(" [Home] ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                    Span::styled(" [Dashboard] ", Style::default().fg(Color::DarkGray)),
                    Span::styled(" [Settings] ", Style::default().fg(Color::DarkGray)),
                    Span::styled(" [Press 'q' to Exit] ", Style::default().fg(Color::Red)),
                ])
            )
            .block(block_style.clone().title(" Navigation "));
            f.render_widget(header_nav, outer_layer[0]);

            // ---------------- KOTAK 1 (Kiri Atas - 30%): Live Metrics ----------------
            let stats_text = vec![
                Line::from(vec![
                    Span::raw("Mutator Task : "),
                    Span::styled("RUNNING (2s)", Style::default().fg(Color::Green)),
                ]),
                Line::from(vec![
                    Span::raw("Reactive Task: "),
                    Span::styled("ACTIVE", Style::default().fg(Color::Green)),
                ]),
                Line::from(vec![
                    Span::raw("Events Read  : "),
                    Span::styled(
                        format!("{} rec", event_count),
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(vec![
                    Span::raw("Uptime       : "),
                    Span::styled(format!("{}s", uptime_secs), Style::default().fg(Color::Blue)),
                ]),
            ];
            let p1 = Paragraph::new(stats_text)
                .block(block_style.clone().title(" Live Metrics "));
            f.render_widget(p1, first_section[0]);

            // ---------------- KOTAK 2 (Kanan Atas - 70%): Activity Overview ----------------
            let overview_text = vec![
                Line::from(Span::styled("Your Live Activity Performance", Style::default().add_modifier(Modifier::UNDERLINED))),
                Line::from(vec![
                    Span::raw("Typing Speed (WPM)  : "),
                    Span::styled(
                        format!("{} character", wpm_score), 
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(vec![
                    Span::raw("Active Window       : "),
                    Span::styled(
                        format!("{}", running_app), 
                        Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
                    ),
                ]),
            ];
            let p2 = Paragraph::new(overview_text)
                .wrap(Wrap { trim: true })
                .block(block_style.clone().title(" Activity Overview "));
            f.render_widget(p2, first_section[1]);

            // ---------------- KOTAK 3 (Kiri Bawah - 50%): System Logs / Status ----------------
            let p3 = Paragraph::new(vec![
                Line::from(Span::styled("System Status: Optimal", Style::default().fg(Color::Green))),
                Line::from(format!("Uptime total: {} seconds elapsed.", uptime_secs)),
                Line::from(Span::styled("Worker threads operational.", Style::default().fg(Color::DarkGray))),
            ])
            .wrap(Wrap { trim: true })
            .block(block_style.clone().title(" System Health "));
            f.render_widget(p3, second_section[0]);

            // ---------------- KOTAK 4 (Kanan Bawah - 50%): Quick Help / Shortcuts ----------------
            let p4 = Paragraph::new(vec![
                Line::from(Span::styled("Keyboard Shortcuts:", Style::default().add_modifier(Modifier::BOLD))),
                Line::from(" • [q] : Keluar dari aplikasi"),
                Line::from(" • [Ctrl+C] : Force stop proses"),
            ])
            .wrap(Wrap { trim: true })
            .block(block_style.clone().title(" Information "));
            f.render_widget(p4, second_section[1]);
        })?;

        // Handle Input Keyboard (Non-blocking poll)
        if event::poll(Duration::from_millis(10))? {
            if let Event::Key(key) = event::read()? {
                if key.code == KeyCode::Char('q')
                    || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
                {
                    break;
                }
            }
        }
    }

    // 8) TEARDOWN TASKS
    mutator_handle.abort();
    reactive_handle.abort();

    Ok(())
}