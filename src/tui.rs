use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, List, ListItem},
    Terminal,
};
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use std::{
    collections::HashMap,
    io,
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};
use tokio::sync::Mutex as TokioMutex;
use chrono::Local;
use std::fs::OpenOptions;
use std::io::Write;

use crate::executor::ActiveTrade;
use crate::config::Config;

const MAX_LOG_LINES: usize = 200;

pub struct TuiState {
    pub trade_logs: StdMutex<Vec<String>>,
    pub scanner_logs: StdMutex<Vec<String>>,
    pub active_trades: Arc<TokioMutex<HashMap<String, ActiveTrade>>>,
    pub balance: StdMutex<f64>,
    pub config: Config,
}

impl TuiState {
    pub fn new(active_trades: Arc<TokioMutex<HashMap<String, ActiveTrade>>>, config: Config) -> Self {
        Self {
            trade_logs: StdMutex::new(Vec::new()),
            scanner_logs: StdMutex::new(Vec::new()),
            active_trades,
            balance: StdMutex::new(if config.paper_trade { 10.0 } else { 0.0 }),
            config,
        }
    }

    pub fn log_trade(&self, msg: &str) {
        let time = Local::now().format("%H:%M:%S").to_string();
        let log_entry = format!("[{}] {}", time, msg);

        if let Ok(mut logs) = self.trade_logs.lock() {
            logs.push(log_entry.clone());
            if logs.len() > MAX_LOG_LINES {
                logs.remove(0);
            }
        }

        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open("logs/trades.log") 
        {
            let _ = writeln!(file, "{}", log_entry);
        }
    }

    pub fn log_scanner(&self, msg: &str) {
        let time = Local::now().format("%H:%M:%S").to_string();
        let log_entry = format!("[{}] {}", time, msg);
        
        // Write to memory for TUI
        if let Ok(mut logs) = self.scanner_logs.lock() {
            logs.push(log_entry.clone());
            if logs.len() > MAX_LOG_LINES {
                logs.remove(0);
            }
        }

        // Write to file for remote monitoring
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open("logs/scanner.log") 
        {
            let _ = writeln!(file, "{}", log_entry);
        }
    }
}

pub async fn run_tui(state: Arc<TuiState>) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let tick_rate = Duration::from_millis(250);
    let mut last_tick = Instant::now();
    let mut scroll_offset: usize = 0;

    loop {
        terminal.draw(|f| {
            let size = f.size();

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(5),
                    Constraint::Length(12),
                ].as_ref())
                .split(size);

            draw_header(f, &state, chunks[0]);
            draw_active_trades(f, &state, chunks[1]);

            let log_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(40), Constraint::Percentage(60)].as_ref())
                .split(chunks[2]);

            draw_trade_logs(f, &state, log_chunks[0], scroll_offset);
            draw_scanner_logs(f, &state, log_chunks[1], scroll_offset);
        })?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));

        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if let KeyCode::Char('q') | KeyCode::Esc = key.code {
                    break;
                }
                match key.code {
                    KeyCode::Up => scroll_offset = scroll_offset.saturating_add(1),
                    KeyCode::Down => scroll_offset = scroll_offset.saturating_sub(1),
                    KeyCode::PageUp => scroll_offset = scroll_offset.saturating_add(10),
                    KeyCode::PageDown => scroll_offset = scroll_offset.saturating_sub(10),
                    _ => {}
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            last_tick = Instant::now();
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    std::process::exit(0);
}

fn draw_header(f: &mut ratatui::Frame, state: &TuiState, area: Rect) {
    let mode = if state.config.paper_trade { "PAPER TRADE" } else { "LIVE TRADE" };
    let balance = *state.balance.lock().unwrap_or_else(|e| e.into_inner());
    let active_count = if let Ok(guard) = state.active_trades.try_lock() {
        guard.len()
    } else {
        0
    };

    let pnl_str = format!("Balance: {:.3} SOL | Active: {}/{} | Mode: {}", 
        balance, active_count, state.config.max_active_trades, mode);

    let p = Paragraph::new(pnl_str)
        .block(Block::default().title(" 💰 Portfolio Overview ").borders(Borders::ALL).border_style(Style::default().fg(Color::Cyan)))
        .style(Style::default().fg(Color::White).add_modifier(Modifier::BOLD));
    
    f.render_widget(p, area);
}

fn draw_active_trades(f: &mut ratatui::Frame, state: &TuiState, area: Rect) {
    let mut items = Vec::new();

    if let Ok(guard) = state.active_trades.try_lock() {
        if guard.is_empty() {
            items.push(ListItem::new("- No active trades -").style(Style::default().fg(Color::DarkGray)));
        } else {
            for (address, trade) in guard.iter() {
                let pnl_color = if trade.pnl_pct >= 0.0 { Color::Green } else { Color::Red };
                let pnl_str = if trade.pnl_pct >= 0.0 {
                    format!("+{:.2}%", trade.pnl_pct)
                } else {
                    format!("{:.2}%", trade.pnl_pct)
                };

                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
                let age = now.saturating_sub(trade.start_time);
                
                let status_color = if trade.status.contains("STOP") {
                    Color::Red
                } else if trade.status.contains("Monitoring") {
                    Color::Yellow
                } else {
                    Color::Cyan
                };

                // Line 1: Trade Info
                let info_line = Line::from(vec![
                    Span::styled(format!("🚀 ${:<8} ", trade.symbol), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                    Span::raw(format!("| Spent: {:.3} SOL | Entry: ${:.6} | Current: ${:.6} | PnL: ", trade.spent_sol, trade.entry_price, trade.current_price)),
                    Span::styled(pnl_str, Style::default().fg(pnl_color).add_modifier(Modifier::BOLD)),
                    Span::raw(format!(" | Age: {}s | Status: ", age)),
                    Span::styled(trade.status.clone(), Style::default().fg(status_color)),
                ]);

                // Line 2: Chart URL
                let url_line = Line::from(vec![
                    Span::raw("   🔗 Chart: "),
                    Span::styled(format!("https://birdeye.so/token/{}?chain=solana", address), Style::default().fg(Color::Blue).add_modifier(Modifier::UNDERLINED)),
                ]);

                items.push(ListItem::new(vec![info_line, url_line, Line::from("")])); // Extra empty line for spacing
            }
        }
    } else {
        items.push(ListItem::new("Updating...").style(Style::default().fg(Color::DarkGray)));
    }

    let list = List::new(items)
        .block(Block::default().title(" 📈 Active Trades Monitor (Real-time) ").borders(Borders::ALL).border_style(Style::default().fg(Color::Magenta)));

    f.render_widget(list, area);
}

fn draw_trade_logs(f: &mut ratatui::Frame, state: &TuiState, area: Rect, scroll_offset: usize) {
    let mut items = Vec::new();
    if let Ok(logs) = state.trade_logs.lock() {
        let height = area.height.saturating_sub(2) as usize;
        let max_scroll = logs.len().saturating_sub(height);
        let current_scroll = scroll_offset.min(max_scroll);
        let start = logs.len().saturating_sub(height + current_scroll);
        let end = (start + height).min(logs.len());
        for log in &logs[start..end] {
            items.push(ListItem::new(log.clone()).style(Style::default().fg(Color::Green)));
        }
    }

    let title = if scroll_offset > 0 { " 💰 Trade Activity (Buy/Sell) [SCROLLED] " } else { " 💰 Trade Activity (Buy/Sell) " };
    let list = List::new(items)
        .block(Block::default().title(title).borders(Borders::ALL).border_style(Style::default().fg(Color::Green)));
    f.render_widget(list, area);
}

fn draw_scanner_logs(f: &mut ratatui::Frame, state: &TuiState, area: Rect, scroll_offset: usize) {
    let mut items = Vec::new();
    if let Ok(logs) = state.scanner_logs.lock() {
        let height = area.height.saturating_sub(2) as usize;
        let max_scroll = logs.len().saturating_sub(height);
        let current_scroll = scroll_offset.min(max_scroll);
        let start = logs.len().saturating_sub(height + current_scroll);
        let end = (start + height).min(logs.len());
        for log in &logs[start..end] {
            items.push(ListItem::new(log.clone()).style(Style::default().fg(Color::White)));
        }
    }

    let title = if scroll_offset > 0 { " 🔍 Scanner & System Activity [SCROLLED] " } else { " 🔍 Scanner & System Activity " };
    let list = List::new(items)
        .block(Block::default().title(title).borders(Borders::ALL).border_style(Style::default().fg(Color::Blue)));
    f.render_widget(list, area);
}
