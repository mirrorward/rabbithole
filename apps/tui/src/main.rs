//! `rabbit-tui` — the RabbitHole terminal client.
//!
//! Wave 2 v1: connect + authenticate, then a live lobby with a who-list
//! sidebar, an input line, and light/dark theming (Ctrl-T) driven by the
//! shared `rabbithole-core` theme tokens (server accent applied when the
//! server publishes a verified bundle).
//!
//! Keys: Enter send · Ctrl-T toggle light/dark · Ctrl-R retro theme ·
//! Ctrl-C / Esc quit. Lines starting with `/go <word>` teleport (a room
//! join or a printed target).

use std::io::Stdout;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use rabbithole_core::theme::{self, Mode, Palette, Rgb, ThemePack};
use rabbithole_core::Client;
use rabbithole_proto::chat::ChatMessage;
use rabbithole_proto::presence::{UserJoined, UserLeft};
use rabbithole_proto::welcome::KeywordKind;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::{Frame, Terminal};

#[derive(Parser)]
#[command(name = "rabbit-tui", version, about = "RabbitHole terminal client")]
struct Cli {
    /// host:port (QUIC) or ws:// URL.
    endpoint: String,
    #[arg(long)]
    fingerprint: Option<String>,
    #[arg(long)]
    server_name: Option<String>,
    #[arg(long)]
    user: Option<String>,
    #[arg(long)]
    password: Option<String>,
    #[arg(long)]
    guest: bool,
    #[arg(long)]
    name: Option<String>,
}

fn to_color(c: Rgb) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

struct App {
    lines: Vec<(String, String)>, // (from, text); from "" = system
    online: Vec<String>,
    input: String,
    pack: ThemePack,
    mode: Mode,
    server_theme: Option<rabbithole_proto::welcome::ThemeBundle>,
    server_name: String,
    should_quit: bool,
}

impl App {
    fn palette(&self) -> Palette {
        theme::resolve(self.pack, self.mode, self.server_theme.as_ref())
    }

    fn sys(&mut self, text: impl Into<String>) {
        self.lines.push((String::new(), text.into()));
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut client = Client::connect(
        &cli.endpoint,
        cli.server_name.as_deref(),
        cli.fingerprint.as_deref(),
        "rabbit-tui",
        env!("CARGO_PKG_VERSION"),
    )
    .await
    .context("connect failed")?;

    let ok = if cli.guest {
        client.auth_guest(cli.name.clone()).await?
    } else {
        let user = cli.user.clone().context("--user LOGIN (or --guest)")?;
        let password = cli
            .password
            .clone()
            .or_else(|| std::env::var("RABBIT_PASSWORD").ok())
            .context("--password or RABBIT_PASSWORD")?;
        client.auth_password(&user, &password).await?
    };
    let welcome = client.expect_welcome().await?;
    let server_theme = client.theme().await.ok().flatten();
    let online = client
        .who()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|u| u.screen_name)
        .collect();
    let history = client.chat_history("lobby", 50).await.unwrap_or_default();

    let mut app = App {
        lines: history.into_iter().map(|m| (m.from, m.text)).collect(),
        online,
        input: String::new(),
        pack: ThemePack::Clean,
        mode: Mode::Dark,
        server_theme,
        server_name: client.server.server_name.clone(),
        should_quit: false,
    };
    app.sys(format!("— signed in as {} —", ok.screen_name));
    if !welcome.motd.is_empty() {
        app.sys(welcome.motd.clone());
    }

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run(&mut terminal, &mut client, &mut app).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

async fn run(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    client: &mut Client,
    app: &mut App,
) -> Result<()> {
    // Feed crossterm input events over a channel from a blocking thread.
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

    terminal.draw(|f| draw(f, app))?;
    while !app.should_quit {
        tokio::select! {
            ev = rx.recv() => {
                let Some(Event::Key(key)) = ev else { continue };
                handle_key(client, app, key).await?;
            }
            push = client.next_push() => {
                match push? {
                    Some(frame) => apply_push(app, &frame),
                    None => {
                        app.sys("— disconnected —");
                        app.should_quit = true;
                    }
                }
            }
        }
        terminal.draw(|f| draw(f, app))?;
    }
    Ok(())
}

fn apply_push(app: &mut App, frame: &rabbithole_proto::Frame) {
    if let Some(Ok(m)) = frame.decode::<ChatMessage>() {
        if m.room == "lobby" {
            app.lines.push((m.from, m.text));
        }
    } else if let Some(Ok(j)) = frame.decode::<UserJoined>() {
        if !app.online.contains(&j.user.screen_name) {
            app.online.push(j.user.screen_name.clone());
        }
        app.sys(format!("* {} joined", j.user.screen_name));
    } else if let Some(Ok(l)) = frame.decode::<UserLeft>() {
        app.online.retain(|n| n != &l.screen_name);
        if !l.screen_name.is_empty() {
            app.sys(format!("* {} left", l.screen_name));
        }
    }
}

async fn handle_key(client: &mut Client, app: &mut App, key: KeyEvent) -> Result<()> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => app.should_quit = true,
        KeyCode::Char('c') if ctrl => app.should_quit = true,
        KeyCode::Char('t') if ctrl => {
            app.mode = match app.mode {
                Mode::Dark => Mode::Light,
                Mode::Light => Mode::Dark,
            };
        }
        KeyCode::Char('r') if ctrl => {
            app.pack = match app.pack {
                ThemePack::Retro => ThemePack::Clean,
                _ => ThemePack::Retro,
            };
        }
        KeyCode::Backspace => {
            app.input.pop();
        }
        KeyCode::Char(c) => app.input.push(c),
        KeyCode::Enter => {
            let text = std::mem::take(&mut app.input);
            let text = text.trim();
            if text.is_empty() {
                return Ok(());
            }
            if let Some(word) = text.strip_prefix("/go ") {
                match client.keyword_go(word.trim()).await {
                    Ok(t) => match t.kind {
                        KeywordKind::Room => {
                            let _ = client.room_join(&t.target).await;
                            app.sys(format!("→ room: {}", t.target));
                        }
                        KeywordKind::User => app.sys(format!("→ user: {}", t.target)),
                        KeywordKind::Url => app.sys(format!("→ url: {}", t.target)),
                        KeywordKind::Unknown => app.sys(format!("no keyword: {}", t.target)),
                        _ => {}
                    },
                    Err(e) => app.sys(format!("keyword failed: {e}")),
                }
            } else if let Err(e) = client.chat_send("lobby", text).await {
                app.sys(format!("send failed: {e}"));
            }
        }
        _ => {}
    }
    Ok(())
}

fn draw(f: &mut Frame, app: &App) {
    let pal = app.palette();
    let bg = Style::default()
        .bg(to_color(pal.background))
        .fg(to_color(pal.text));
    f.render_widget(Block::default().style(bg), f.area());

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(20), Constraint::Length(22)])
        .split(f.area());
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(cols[0]);

    let accent = Style::default().fg(to_color(pal.accent));
    let muted = Style::default().fg(to_color(pal.muted));

    // Chat log (tail to fit).
    let log: Vec<ListItem> = app
        .lines
        .iter()
        .map(|(from, text)| {
            if from.is_empty() {
                ListItem::new(Line::from(Span::styled(text.clone(), muted)))
            } else {
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{from}: "), accent),
                    Span::styled(text.clone(), Style::default().fg(to_color(pal.text))),
                ]))
            }
        })
        .collect();
    let title = format!(" {} — lobby ", app.server_name);
    f.render_widget(
        List::new(log).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(accent),
        ),
        rows[0],
    );

    // Input.
    f.render_widget(
        Paragraph::new(format!("> {}", app.input)).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" say (Enter) · Ctrl-T light/dark · Ctrl-R retro · Esc quit ")
                .border_style(muted),
        ),
        rows[1],
    );

    // Who sidebar.
    let who: Vec<ListItem> = app
        .online
        .iter()
        .map(|n| ListItem::new(Line::from(n.clone())))
        .collect();
    f.render_widget(
        List::new(who).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" online {} ", app.online.len()))
                .border_style(accent),
        ),
        cols[1],
    );
}
