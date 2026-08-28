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
            // Layout Utama (Vertical): Header, Body, Footer
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3), // Header
                    Constraint::Min(6),    // Main Content
                    Constraint::Length(3), // Footer
                ])
                .split(f.area());

            // ---------------- HEADER ----------------
            let header = Paragraph::new(" TRACK YOUR DAY —")
                .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
                .block(Block::default().borders(Borders::ALL).title(" App "));
            f.render_widget(header, chunks[0]);

            // ---------------- MAIN BODY (Horizontal Split) ----------------
            let body_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(50), // Kolom Kiri: Statistics
                    Constraint::Percentage(50), // Kolom Kanan: System Overview
                ])
                .split(chunks[1]);

            // Panel Kiri: Real-time Stats
            let stats_text = vec![
                Line::from(vec![
                    Span::raw("Mutator Task   : "),
                    Span::styled("RUNNING (Interval 2s)", Style::default().fg(Color::Green)),
                ]),
                Line::from(vec![
                    Span::raw("Reactive Task  : "),
                    Span::styled("ACTIVE", Style::default().fg(Color::Green)),
                ]),
                Line::from(vec![
                    Span::raw("Events Read    : "),
                    Span::styled(
                        format!("{} records", event_count),
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(vec![
                    Span::raw("Uptime         : "),
                    Span::styled(format!("{} detik", uptime_secs), Style::default().fg(Color::Blue)),
                ]),
            ];
            let stats_panel = Paragraph::new(stats_text)
                .block(Block::default().borders(Borders::ALL).title(" Live Metrics "));
            f.render_widget(stats_panel, body_chunks[0]);

            // Panel Kanan: Architecture 
            let overview_text = vec![
                Line::from(Span::styled("Your Activity", Style::default().add_modifier(Modifier::UNDERLINED))),
                Line::from(vec![
                    Span::raw("WPM : "),
                    Span::styled(
                        format!("{} character", wpm_score), 
                        Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(vec![
                    Span::raw("Running Application : "),
                    Span::styled(
                        format!("{} Recently", running_app), 
                        Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(vec![
                    Span::raw("Running Thread : "),
                    Span::styled(
                        format!("{} thread", ), style)
                ]),
            ];
            let overview_panel = Paragraph::new(overview_text)
                .wrap(Wrap { trim: true })
                .block(Block::default().borders(Borders::ALL).title(" Your Activity "));
            f.render_widget(overview_panel, body_chunks[1]);

            // ---------------- FOOTER ----------------
            let footer = Paragraph::new(" Tekan 'q' atau Ctrl+C untuk keluar dari aplikasi")
                .style(Style::default().fg(Color::DarkGray))
                .block(Block::default().borders(Borders::ALL));
            f.render_widget(footer, chunks[2]);
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