//! The Unix-only sysop console implementation (talks to the burrow's local
//! ctl socket). `main.rs` compiles this module only on Unix.

use std::io::Stdout;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::event::{Event, KeyCode, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::{Frame, Terminal};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Parser)]
#[command(name = "burrow-tui", version, about = "Burrow sysop console")]
struct Cli {
    /// Burrow data directory (contains ctl.sock).
    #[arg(long, default_value = "./burrow-data")]
    data_dir: std::path::PathBuf,
    /// Refresh interval, seconds.
    #[arg(long, default_value_t = 3)]
    interval: u64,
}

async fn ctl(path: &std::path::Path, req: Value) -> Result<Value> {
    let stream = UnixStream::connect(path)
        .await
        .with_context(|| format!("is burrow running? (no socket at {})", path.display()))?;
    let (read, mut write) = stream.into_split();
    write.write_all(format!("{req}\n").as_bytes()).await?;
    let mut lines = BufReader::new(read).lines();
    let line = lines.next_line().await?.context("no response")?;
    Ok(serde_json::from_str(&line)?)
}

#[derive(Default)]
struct State {
    status: Value,
    who: Vec<Value>,
    error: Option<String>,
}

async fn refresh(sock: &std::path::Path) -> State {
    let mut state = State::default();
    match ctl(sock, json!({"cmd": "status"})).await {
        Ok(r) if r["ok"] == true => state.status = r["data"].clone(),
        Ok(r) => state.error = Some(r["error"].as_str().unwrap_or("error").to_string()),
        Err(e) => state.error = Some(e.to_string()),
    }
    if let Ok(r) = ctl(sock, json!({"cmd": "who"})).await {
        if let Some(arr) = r["data"].as_array() {
            state.who = arr.clone();
        }
    }
    state
}

#[tokio::main]
pub async fn run() -> Result<()> {
    let cli = Cli::parse();
    let sock = cli.data_dir.join("ctl.sock");

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;

    let result = run_loop(&mut terminal, &sock, cli.interval).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    sock: &std::path::Path,
    interval: u64,
) -> Result<()> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    std::thread::spawn(move || loop {
        if crossterm::event::poll(Duration::from_millis(200)).unwrap_or(false) {
            if let Ok(ev) = crossterm::event::read() {
                if tx.send(ev).is_err() {
                    break;
                }
            }
        }
    });

    let mut state = refresh(sock).await;
    terminal.draw(|f| draw(f, &state))?;
    let mut tick = tokio::time::interval(Duration::from_secs(interval.max(1)));

    loop {
        tokio::select! {
            _ = tick.tick() => {
                state = refresh(sock).await;
            }
            ev = rx.recv() => {
                let Some(Event::Key(key)) = ev else { continue };
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('c') if ctrl => break,
                    KeyCode::Char('r') => state = refresh(sock).await,
                    _ => {}
                }
            }
        }
        terminal.draw(|f| draw(f, &state))?;
    }
    Ok(())
}

fn draw(f: &mut Frame, state: &State) {
    let accent = Style::default().fg(Color::Rgb(0x6c, 0x9c, 0xff));
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(8),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(f.area());

    // Status panel.
    let s = &state.status;
    let status_lines = vec![
        Line::from(format!(
            "name:        {}",
            s["name"].as_str().unwrap_or("?")
        )),
        Line::from(format!(
            "version:     {}",
            s["version"].as_str().unwrap_or("?")
        )),
        Line::from(format!(
            "online:      {}",
            s["online"].as_u64().unwrap_or(0)
        )),
        Line::from(format!(
            "quic:        {}",
            s["quic_addr"].as_str().unwrap_or("?")
        )),
        Line::from(format!(
            "websocket:   {}",
            s["ws_addr"].as_str().unwrap_or("?")
        )),
        Line::from(format!(
            "fingerprint: {}",
            s["fingerprint"].as_str().unwrap_or("?")
        )),
    ];
    let title = match &state.error {
        Some(e) => format!(" burrow — ERROR: {e} "),
        None => " burrow — status ".to_string(),
    };
    f.render_widget(
        Paragraph::new(status_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(accent),
        ),
        rows[0],
    );

    // Who's online.
    let who: Vec<ListItem> = state
        .who
        .iter()
        .map(|u| {
            ListItem::new(Line::from(format!(
                "{:24} {:10} {:>5}s  {}",
                u["screen_name"].as_str().unwrap_or("?"),
                u["transport"].as_str().unwrap_or("?"),
                u["connected_secs"].as_u64().unwrap_or(0),
                u["role"].as_str().unwrap_or(""),
            )))
        })
        .collect();
    f.render_widget(
        List::new(who).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" connections ({}) ", state.who.len()))
                .border_style(accent),
        ),
        rows[1],
    );

    f.render_widget(
        Paragraph::new("r refresh · q/Esc quit")
            .style(Style::default().add_modifier(Modifier::DIM)),
        rows[2],
    );
}
