//! `rabbit-tui` — the RabbitHole terminal client.
//!
//! Wave 2 v1: connect + authenticate, then a live lobby with a who-list
//! sidebar, an input line, and light/dark theming (Ctrl-T) driven by the
//! shared `rabbithole-core` theme tokens (server accent applied when the
//! server publishes a verified bundle).
//!
//! Beyond the lobby there are two full-screen views:
//!
//! - **Radio** (Ctrl-N): the station list fed by the typed RADIO family
//!   (see `radio` for the reducer), plus the playback **handoff**:
//!   Enter/`p` derives a copyable `<base>/<station>` stream URL from a
//!   session-local "radio base" (`b` to edit — this crate has no settings
//!   persistence yet, so the base lives for the session only) and `o`
//!   launches `$RABBIT_PLAYER <url>` detached. The TUI hands playback off to
//!   an external player and never decodes audio (see `handoff`).
//! - **Server browser** (Ctrl-B): a Looking Glass tracker's status port over
//!   one-shot TCP text lines (`INDEX`/`CATEGORIES`/`HEALTH` — see `browser`).
//!   Address defaults from `$RABBIT_TRACKER`; `r` refreshes, `c` cycles the
//!   category filter, `h` fetches a health sparkline. Uptime is always
//!   labelled tracker-observed (verifiable, not authoritative).
//!
//! Keys: Enter send · Ctrl-T light/dark · Ctrl-R retro theme · Ctrl-N radio ·
//! Ctrl-B servers · Ctrl-C quit (Esc backs out of a view, quits the lobby).
//! Lines starting with `/go <word>` teleport (a room join or a printed
//! target).
//!
//! A one-line status bar under every view surfaces radio now-playing plus
//! transient action/error messages — player spawn failures land there, they
//! never crash the TUI.

#![forbid(unsafe_code)]

mod browser;
mod handoff;
mod radio;

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
use rabbithole_proto::radio::{RadioNowPlaying, RadioOff};
use rabbithole_proto::session::ServerNotice;
use rabbithole_proto::welcome::KeywordKind;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::{Frame, Terminal};
use tokio::sync::mpsc::UnboundedSender;

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

/// Which full-screen surface owns the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Lobby,
    Radio,
    Browser,
}

struct App {
    lines: Vec<(String, String)>, // (from, text); from "" = system
    online: Vec<String>,
    input: String,
    pack: ThemePack,
    mode: Mode,
    server_theme: Option<rabbithole_proto::welcome::ThemeBundle>,
    server_name: String,
    radio: radio::RadioState,
    view: View,
    /// Transient status-bar message (action results, handoff errors).
    status: Option<String>,
    /// Radio view: selected station index (clamped against the list).
    radio_selected: usize,
    /// Radio view: the stream delivery base (`http://host:8000`).
    /// **Session-local** — the TUI has no settings persistence yet, so this
    /// is typed per session (`b`) and forgotten on exit.
    radio_base: String,
    /// Radio view: the last derived (copyable) stream URL.
    radio_url: Option<String>,
    /// Radio view: the base-input buffer while editing (`b` … Enter/Esc).
    base_edit: Option<String>,
    /// Server-browser view state (see `browser`).
    browser: browser::BrowserState,
    should_quit: bool,
}

impl App {
    fn palette(&self) -> Palette {
        theme::resolve(self.pack, self.mode, self.server_theme.as_ref())
    }

    fn sys(&mut self, text: impl Into<String>) {
        self.lines.push((String::new(), text.into()));
    }

    fn status(&mut self, text: impl Into<String>) {
        self.status = Some(text.into());
    }

    /// Toggle into `view` (or back to the lobby when already there),
    /// dropping any half-finished edit buffers and stale status text.
    fn switch_view(&mut self, view: View) {
        self.status = None;
        self.base_edit = None;
        self.view = if self.view == view { View::Lobby } else { view };
        if self.view == View::Browser && self.browser.addr.is_none() {
            self.browser.editing_addr = true;
        }
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
        radio: radio::RadioState::default(),
        view: View::Lobby,
        status: None,
        radio_selected: 0,
        radio_base: String::new(),
        radio_url: None,
        base_edit: None,
        browser: browser::BrowserState::new(std::env::var(browser::TRACKER_ENV).ok()),
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

    // Completed tracker fetches flow back in from spawned tasks; the sender
    // stays alive here so the receive arm simply idles when nothing is
    // in flight.
    let (fetch_tx, mut fetch_rx) = tokio::sync::mpsc::unbounded_channel::<browser::Fetched>();

    terminal.draw(|f| draw(f, app))?;
    while !app.should_quit {
        tokio::select! {
            ev = rx.recv() => {
                let Some(Event::Key(key)) = ev else { continue };
                handle_key(client, app, &fetch_tx, key).await?;
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
            fetched = fetch_rx.recv() => {
                if let Some(fetched) = fetched {
                    app.browser.apply(fetched);
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
    } else if let Some(Ok(np)) = frame.decode::<RadioNowPlaying>() {
        // Typed RADIO now-playing / sign-off update the reducer silently.
        app.radio.apply_radio_status(np.into());
    } else if let Some(Ok(off)) = frame.decode::<RadioOff>() {
        app.radio.clear_station(&off.station);
    } else if let Some(Ok(n)) = frame.decode::<ServerNotice>() {
        // Everything else is an operator notice for the chat log.
        app.sys(format!("! {}: {}", n.from, n.text));
    }
}

async fn handle_key(
    client: &mut Client,
    app: &mut App,
    fetch_tx: &UnboundedSender<browser::Fetched>,
    key: KeyEvent,
) -> Result<()> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    // Global chords first: quit, theming, view toggles.
    match key.code {
        KeyCode::Char('c') if ctrl => {
            app.should_quit = true;
            return Ok(());
        }
        KeyCode::Char('t') if ctrl => {
            app.mode = match app.mode {
                Mode::Dark => Mode::Light,
                Mode::Light => Mode::Dark,
            };
            return Ok(());
        }
        KeyCode::Char('r') if ctrl => {
            app.pack = match app.pack {
                ThemePack::Retro => ThemePack::Clean,
                _ => ThemePack::Retro,
            };
            return Ok(());
        }
        KeyCode::Char('n') if ctrl => {
            app.switch_view(View::Radio);
            return Ok(());
        }
        KeyCode::Char('b') if ctrl => {
            app.switch_view(View::Browser);
            return Ok(());
        }
        _ => {}
    }
    match app.view {
        View::Lobby => handle_lobby_key(client, app, key, ctrl).await?,
        View::Radio => handle_radio_key(app, key, ctrl),
        View::Browser => handle_browser_key(app, fetch_tx, key, ctrl),
    }
    Ok(())
}

async fn handle_lobby_key(
    client: &mut Client,
    app: &mut App,
    key: KeyEvent,
    ctrl: bool,
) -> Result<()> {
    match key.code {
        KeyCode::Esc => app.should_quit = true,
        KeyCode::Backspace => {
            app.input.pop();
        }
        KeyCode::Char(c) if !ctrl => app.input.push(c),
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

// ---------------------------------------------------------------------------
// Radio view (Slice: player handoff — the TUI never decodes audio).
// ---------------------------------------------------------------------------

fn handle_radio_key(app: &mut App, key: KeyEvent, ctrl: bool) {
    // While the base is being edited, the input line owns the keyboard.
    if let Some(buf) = &mut app.base_edit {
        match key.code {
            KeyCode::Enter => {
                let base = buf.trim().to_string();
                app.base_edit = None;
                if base.is_empty() {
                    app.radio_base.clear();
                    app.status("radio base cleared");
                } else if handoff::base_is_valid(&base) {
                    app.radio_base = base;
                    app.status("radio base set (session-only — no config file yet)");
                } else {
                    app.radio_base = base;
                    app.status("saved, but base must be http:// or https:// with a host");
                }
            }
            KeyCode::Esc => app.base_edit = None,
            KeyCode::Backspace => {
                buf.pop();
            }
            KeyCode::Char(c) if !ctrl => buf.push(c),
            _ => {}
        }
        return;
    }
    match key.code {
        KeyCode::Esc => app.view = View::Lobby,
        KeyCode::Up | KeyCode::Char('k') => {
            app.radio_selected = app.radio_selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let last = app.radio.stations().len().saturating_sub(1);
            app.radio_selected = (app.radio_selected + 1).min(last);
        }
        KeyCode::Char('b') => app.base_edit = Some(app.radio_base.clone()),
        KeyCode::Enter | KeyCode::Char('p') => {
            derive_stream_url(app);
        }
        KeyCode::Char('o') => {
            if derive_stream_url(app) {
                launch_player(app);
            }
        }
        _ => {}
    }
}

/// Derive `<base>/<station>` for the selected station into `radio_url`
/// (the copyable line in the handoff pane). Returns whether a URL exists.
fn derive_stream_url(app: &mut App) -> bool {
    let stations = app.radio.stations();
    if stations.is_empty() {
        app.status("no stations on the air");
        return false;
    }
    let sel = app.radio_selected.min(stations.len() - 1);
    let slug = stations[sel].station.clone();
    if app.radio_base.trim().is_empty() {
        app.status("set the radio base first: b, then e.g. http://host:8000");
        return false;
    }
    match handoff::stream_url(&app.radio_base, &slug) {
        Some(url) => {
            app.radio_url = Some(url);
            app.status(format!(
                "stream url ready for '{slug}' — copy it, or o to hand off to ${}",
                handoff::PLAYER_ENV
            ));
            true
        }
        None => {
            app.status("invalid radio base — must be http:// or https:// with a host");
            false
        }
    }
}

/// Hand the derived URL to `$RABBIT_PLAYER` (detached spawn). Every failure
/// is a status-line message; nothing here can take the TUI down.
fn launch_player(app: &mut App) {
    let Some(url) = app.radio_url.clone() else {
        return;
    };
    match std::env::var(handoff::PLAYER_ENV) {
        Ok(spec) => match handoff::player_command(&spec, &url) {
            Some((program, args)) => match handoff::spawn_player(&program, &args) {
                Ok(()) => app.status(format!("handed off to {program} — {url}")),
                Err(err) => app.status(err),
            },
            None => app.status(format!("${} is empty — url: {url}", handoff::PLAYER_ENV)),
        },
        Err(_) => app.status(format!(
            "${} unset — copy the url: {url}",
            handoff::PLAYER_ENV
        )),
    }
}

// ---------------------------------------------------------------------------
// Server-browser view (Slice: Looking Glass status port).
// ---------------------------------------------------------------------------

fn handle_browser_key(
    app: &mut App,
    fetch_tx: &UnboundedSender<browser::Fetched>,
    key: KeyEvent,
    ctrl: bool,
) {
    if app.browser.editing_addr {
        match key.code {
            KeyCode::Enter => match browser::normalize_tracker_addr(&app.browser.addr_input) {
                Some(addr) => {
                    app.browser.addr = Some(addr);
                    app.browser.editing_addr = false;
                    start_index_fetch(app, fetch_tx);
                }
                None => app.status(format!(
                    "enter a tracker host[:port] (or set ${})",
                    browser::TRACKER_ENV
                )),
            },
            KeyCode::Esc => {
                if app.browser.addr.is_some() {
                    app.browser.editing_addr = false;
                } else {
                    app.view = View::Lobby;
                }
            }
            KeyCode::Backspace => {
                app.browser.addr_input.pop();
            }
            KeyCode::Char(c) if !ctrl => app.browser.addr_input.push(c),
            _ => {}
        }
        return;
    }
    match key.code {
        KeyCode::Esc => app.view = View::Lobby,
        KeyCode::Char('a') => app.browser.editing_addr = true,
        KeyCode::Char('r') => start_index_fetch(app, fetch_tx),
        KeyCode::Char('c') => {
            if app.browser.categories.is_empty() && app.browser.filter.is_none() {
                app.status("no categories advertised by this tracker (r to refresh)");
            } else {
                app.browser.cycle_filter();
                start_index_fetch(app, fetch_tx);
            }
        }
        KeyCode::Up | KeyCode::Char('k') => app.browser.move_selection(false),
        KeyCode::Down | KeyCode::Char('j') => app.browser.move_selection(true),
        KeyCode::Char('h') => start_health_fetch(app, fetch_tx),
        _ => {}
    }
}

/// Spawn one `INDEX` (+ best-effort `CATEGORIES`) exchange; the reply comes
/// back through the fetch channel tagged with a sequence number.
fn start_index_fetch(app: &mut App, fetch_tx: &UnboundedSender<browser::Fetched>) {
    let Some(addr) = app.browser.addr.clone() else {
        app.status("no tracker address yet — press a");
        return;
    };
    let seq = app.browser.begin();
    let filter = app.browser.filter.clone();
    let tx = fetch_tx.clone();
    tokio::spawn(async move {
        let command = match &filter {
            Some(category) => format!("INDEX cat={category}"),
            None => "INDEX".to_string(),
        };
        let outcome = match browser::query(&addr, &command).await {
            Ok(text) => match browser::parse_index(&text) {
                Ok(rows) => {
                    // Best-effort: a failed CATEGORIES fetch keeps the old
                    // filter list rather than failing the whole refresh.
                    let categories = match browser::query(&addr, "CATEGORIES").await {
                        Ok(text) => browser::parse_categories(&text).ok(),
                        Err(_) => None,
                    };
                    Ok((rows, categories))
                }
                Err(err) => Err(err),
            },
            Err(err) => Err(err),
        };
        let _ = tx.send(browser::Fetched {
            seq,
            outcome: browser::Outcome::Index(outcome),
        });
    });
}

/// Spawn a `HEALTH <ip:port>` exchange for the selected row.
fn start_health_fetch(app: &mut App, fetch_tx: &UnboundedSender<browser::Fetched>) {
    let Some(addr) = app.browser.addr.clone() else {
        app.status("no tracker address yet — press a");
        return;
    };
    let Some(row) = app.browser.selected_row() else {
        app.status("no server selected");
        return;
    };
    let target = row.addr.clone();
    let seq = app.browser.begin();
    let tx = fetch_tx.clone();
    tokio::spawn(async move {
        let outcome = match browser::query(&addr, &format!("HEALTH {target}")).await {
            Ok(text) => browser::parse_health(&text),
            Err(err) => Err(err),
        };
        let _ = tx.send(browser::Fetched {
            seq,
            outcome: browser::Outcome::Health(outcome),
        });
    });
}

// ---------------------------------------------------------------------------
// Rendering.
// ---------------------------------------------------------------------------

fn draw(f: &mut Frame, app: &App) {
    let pal = app.palette();
    let bg = Style::default()
        .bg(to_color(pal.background))
        .fg(to_color(pal.text));
    f.render_widget(Block::default().style(bg), f.area());

    let bands = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(6), Constraint::Length(1)])
        .split(f.area());

    let accent = Style::default().fg(to_color(pal.accent));
    let muted = Style::default().fg(to_color(pal.muted));
    let text = Style::default().fg(to_color(pal.text));

    match app.view {
        View::Lobby => draw_lobby(f, app, bands[0], accent, muted, text),
        View::Radio => draw_radio(f, app, bands[0], accent, muted),
        View::Browser => draw_browser(f, app, bands[0], accent, muted, text),
    }
    draw_status_bar(f, app, bands[1], accent, muted);
}

fn draw_lobby(f: &mut Frame, app: &App, area: Rect, accent: Style, muted: Style, text: Style) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(20), Constraint::Length(22)])
        .split(area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(cols[0]);

    // Chat log (tail to fit).
    let log: Vec<ListItem> = app
        .lines
        .iter()
        .map(|(from, line)| {
            if from.is_empty() {
                ListItem::new(Line::from(Span::styled(line.clone(), muted)))
            } else {
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{from}: "), accent),
                    Span::styled(line.clone(), text),
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
                .title(" say (Enter) · Ctrl-N radio · Ctrl-B servers · Ctrl-T/R theme · Esc quit ")
                .border_style(muted),
        ),
        rows[1],
    );

    // Sidebar: the who-list.
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

fn draw_radio(f: &mut Frame, app: &App, area: Rect, accent: Style, muted: Style) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(5)])
        .split(area);

    // Station list: two lines per station, selection reversed on the header.
    let inner_width = rows[0].width.saturating_sub(2) as usize;
    let stations = app.radio.stations();
    let sel = app.radio_selected.min(stations.len().saturating_sub(1));
    let items: Vec<ListItem> = radio::panel_lines(&app.radio, inner_width)
        .into_iter()
        .enumerate()
        .map(|(i, l)| {
            let mut style = if l.live { accent } else { muted };
            if !stations.is_empty() && i / 2 == sel && i % 2 == 0 {
                style = style.add_modifier(Modifier::REVERSED);
            }
            ListItem::new(Line::from(Span::styled(l.text, style)))
        })
        .collect();
    f.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" radio — ↑/↓ pick · Enter/p url · o play · b base · Esc back ")
                .border_style(accent),
        ),
        rows[0],
    );

    // Handoff pane: base + derived URL + the no-decode note.
    let base_line = match &app.base_edit {
        Some(buf) => Line::from(Span::styled(format!("base> {buf}▌"), accent)),
        None if app.radio_base.is_empty() => Line::from(Span::styled(
            "base: (unset — press b · session-only, the TUI has no config file yet)",
            muted,
        )),
        None => Line::from(vec![
            Span::styled("base: ", muted),
            Span::styled(app.radio_base.clone(), accent),
            Span::styled("  (session-only)", muted),
        ]),
    };
    let url_line = match &app.radio_url {
        Some(url) => Line::from(vec![
            Span::styled("url:  ", muted),
            Span::styled(url.clone(), accent),
        ]),
        None => Line::from(Span::styled(
            "url:  (press Enter/p on a station to derive <base>/<station>)",
            muted,
        )),
    };
    let note = Line::from(Span::styled(
        format!(
            "note: plays via ${} — the TUI hands off and never decodes audio",
            handoff::PLAYER_ENV
        ),
        muted,
    ));
    let edit_hint = if app.base_edit.is_some() {
        " handoff — editing base: Enter save · Esc cancel "
    } else {
        " handoff (external player) "
    };
    f.render_widget(
        Paragraph::new(vec![base_line, url_line, note]).block(
            Block::default()
                .borders(Borders::ALL)
                .title(edit_hint)
                .border_style(muted),
        ),
        rows[1],
    );
}

fn draw_browser(f: &mut Frame, app: &App, area: Rect, accent: Style, muted: Style, text: Style) {
    let b = &app.browser;
    let mut constraints = vec![Constraint::Length(3), Constraint::Min(4)];
    if b.health.is_some() {
        constraints.push(Constraint::Length(6));
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    // Tracker/filter bar.
    let bar = if b.editing_addr {
        Line::from(Span::styled(format!("tracker> {}▌", b.addr_input), accent))
    } else {
        let filter = match &b.filter {
            Some(name) => {
                let count = b
                    .categories
                    .iter()
                    .find(|c| &c.name == name)
                    .map(|c| c.count)
                    .unwrap_or(0);
                format!("{name} ({count} live)")
            }
            None => format!("all ({} cats)", b.categories.len()),
        };
        Line::from(vec![
            Span::styled("tracker: ", muted),
            Span::styled(b.addr.clone().unwrap_or_else(|| "—".into()), accent),
            Span::styled(format!("  · filter: {filter}"), muted),
        ])
    };
    let bar_title = if b.editing_addr {
        " tracker address — Enter connect · Esc cancel "
    } else {
        " server browser — a addr · r refresh · c filter · h health · Esc back "
    };
    f.render_widget(
        Paragraph::new(bar).block(
            Block::default()
                .borders(Borders::ALL)
                .title(bar_title)
                .border_style(muted),
        ),
        rows[0],
    );

    // The table: header, optional error banner, rows as served.
    let mut items: Vec<ListItem> = Vec::new();
    if let Some(err) = &b.error {
        items.push(ListItem::new(Line::from(Span::styled(
            format!("⚠ {err} — press r to retry"),
            accent.add_modifier(Modifier::BOLD),
        ))));
    }
    items.push(ListItem::new(Line::from(Span::styled(
        browser::table_header(),
        muted.add_modifier(Modifier::BOLD),
    ))));
    if b.rows.is_empty() {
        let placeholder = if b.addr.is_none() {
            "(no tracker yet — press a to enter one, or set $RABBIT_TRACKER)"
        } else if b.loading {
            "(fetching…)"
        } else if b.error.is_none() {
            "(no servers listed — r to refresh)"
        } else {
            ""
        };
        if !placeholder.is_empty() {
            items.push(ListItem::new(Line::from(Span::styled(placeholder, muted))));
        }
    } else {
        let sel = b.selected.min(b.rows.len() - 1);
        for (i, row) in b.rows.iter().enumerate() {
            let style = if i == sel {
                text.add_modifier(Modifier::REVERSED)
            } else {
                text
            };
            items.push(ListItem::new(Line::from(Span::styled(
                browser::format_row(row),
                style,
            ))));
        }
        if let Some(row) = b.selected_row() {
            items.push(ListItem::new(Line::from(Span::styled(
                browser::selection_detail(row),
                muted,
            ))));
        }
    }
    let table_title = format!(
        " servers {}{} — sorted by tracker · uptime is tracker-observed ",
        b.rows.len(),
        if b.loading { " · fetching…" } else { "" }
    );
    f.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .title(table_title)
                .border_style(accent),
        ),
        rows[1],
    );

    // Health detail pane (only after `h`).
    if let Some(h) = &b.health {
        let lines = vec![
            Line::from(vec![
                Span::styled(h.addr.clone(), accent),
                Span::styled(
                    format!(
                        " · live {} · uptime {}% (as observed by this tracker) · flaps {}",
                        if h.live { "yes" } else { "no" },
                        h.uptime_pct,
                        h.flaps
                    ),
                    text,
                ),
            ]),
            Line::from(Span::styled(
                format!(
                    "first seen {}s ago · last seen {}s ago",
                    h.first_seen_secs, h.last_seen_secs
                ),
                muted,
            )),
            Line::from(Span::styled(h.sparkline.clone(), accent)),
            Line::from(Span::styled(
                "# full · + partial · . silent — 15-min buckets, oldest first",
                muted,
            )),
        ];
        f.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" health — tracker-observed, not authoritative ")
                    .border_style(muted),
            ),
            rows[2],
        );
    }
}

/// Status bar: a transient action/error message when present, else the
/// radio now-playing segment — always beside the server name.
fn draw_status_bar(f: &mut Frame, app: &App, area: Rect, accent: Style, muted: Style) {
    let width = area.width as usize;
    let (seg, seg_style) = match &app.status {
        Some(msg) => (msg.clone(), accent),
        None => match radio::status_segment(&app.radio, width.saturating_sub(3)) {
            Some(seg) => (seg, accent),
            None => ("♪ off the air".to_string(), muted),
        },
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {seg} "), seg_style),
            Span::styled(format!("· {}", app.server_name), muted),
        ])),
        area,
    );
}
