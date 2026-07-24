//! `agent24 tui` — a thin ops client over the v1 protocol (ADR-026 decision
//! #11: the TUI is an operator surface, not a second agent runtime). It renders
//! runs / event stream / approval queue, drives approvals through REST, and
//! stays converged via a WS stream with REST reconciliation on any seq gap or
//! disconnect.

pub mod app;
mod ui;

use std::io::{self, Stdout};
use std::time::Duration;

use agent24_protocol::{Approval, Decision, Event, Run};
use crossterm::event::{Event as CtEvent, EventStream, KeyCode, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::{ExecutableCommand, cursor};
use futures_util::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite;

use app::{Action, App, Key};

/// Connection facts for the daemon under management.
pub struct Conn {
    pub base: String,
    pub token: String,
}

impl Conn {
    fn client(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .build()
            .unwrap_or_default()
    }

    fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.token.is_empty() {
            rb
        } else {
            rb.bearer_auth(&self.token)
        }
    }

    async fn list_runs(&self) -> Result<Vec<Run>, String> {
        let res = self
            .auth(self.client().get(format!("{}/api/v1/runs", self.base)))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let body: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
        serde_json::from_value(body["runs"].clone()).map_err(|e| e.to_string())
    }

    async fn list_pending_approvals(&self) -> Result<Vec<Approval>, String> {
        let res = self
            .auth(
                self.client()
                    .get(format!("{}/api/v1/approvals?status=pending", self.base)),
            )
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let body: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
        serde_json::from_value(body["approvals"].clone()).map_err(|e| e.to_string())
    }

    async fn decide(&self, approval_id: &str, decision: &Decision) -> Result<(), String> {
        let res = self
            .auth(
                self.client()
                    .post(format!("{}/api/v1/approvals/{approval_id}", self.base)),
            )
            .json(decision)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        // 409 (already resolved) is not fatal — the next reconcile drops it
        if res.status().is_success() || res.status().as_u16() == 409 {
            Ok(())
        } else {
            Err(format!("decision rejected: {}", res.status()))
        }
    }

    async fn cancel_run(&self, run_id: &str) -> Result<(), String> {
        self.auth(
            self.client()
                .post(format!("{}/api/v1/runs/{run_id}/cancel", self.base)),
        )
        .send()
        .await
        .map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// Messages the async tasks feed into the single-threaded UI loop.
enum Msg {
    Event(Event),
    /// The WS stream ended — the loop should reconcile and reconnect
    WsClosed,
}

/// Spawn the WS reader. It emits [`Msg::Event`] per frame and [`Msg::WsClosed`]
/// when the socket ends, then the loop re-arms it.
fn spawn_ws(base: String, token: String, tx: mpsc::UnboundedSender<Msg>) {
    tokio::spawn(async move {
        // http(s)://host → ws(s)://host
        let ws_url = format!("{}/api/v1/events", base.replacen("http", "ws", 1));
        let request = match build_ws_request(&ws_url, &token) {
            Ok(req) => req,
            Err(_) => {
                let _ = tx.send(Msg::WsClosed);
                return;
            }
        };
        match tokio_tungstenite::connect_async(request).await {
            Ok((mut socket, _)) => {
                while let Some(frame) = socket.next().await {
                    match frame {
                        Ok(tungstenite::Message::Text(text)) => {
                            if let Ok(event) = serde_json::from_str::<Event>(&text)
                                && tx.send(Msg::Event(event)).is_err()
                            {
                                return; // UI gone
                            }
                        }
                        Ok(tungstenite::Message::Close(_)) | Err(_) => break,
                        _ => {}
                    }
                }
            }
            Err(_) => { /* fall through to reconnect */ }
        }
        let _ = tx.send(Msg::WsClosed);
    });
}

fn build_ws_request(
    ws_url: &str,
    token: &str,
) -> Result<tungstenite::handshake::client::Request, String> {
    use tungstenite::client::IntoClientRequest;
    let mut request = ws_url.into_client_request().map_err(|e| e.to_string())?;
    if !token.is_empty() {
        let value = format!("Bearer {token}")
            .parse()
            .map_err(|_| "bad token header".to_owned())?;
        request.headers_mut().insert("Authorization", value);
    }
    Ok(request)
}

type Tui = Terminal<CrosstermBackend<Stdout>>;

fn setup_terminal() -> io::Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    stdout.execute(cursor::Hide)?;
    Terminal::new(CrosstermBackend::new(stdout))
}

fn restore_terminal(terminal: &mut Tui) {
    let _ = disable_raw_mode();
    let _ = terminal.backend_mut().execute(LeaveAlternateScreen);
    let _ = terminal.backend_mut().execute(cursor::Show);
    let _ = terminal.show_cursor();
}

fn map_key(code: KeyCode) -> Option<Key> {
    match code {
        KeyCode::Up => Some(Key::Up),
        KeyCode::Down => Some(Key::Down),
        KeyCode::Enter => Some(Key::Enter),
        KeyCode::Esc => Some(Key::Esc),
        KeyCode::Tab => Some(Key::Tab),
        KeyCode::Char('q') => Some(Key::Quit),
        KeyCode::Char('c') => Some(Key::Cancel),
        KeyCode::Char(c) => Some(Key::Char(c)),
        _ => None,
    }
}

/// Entry point for `agent24 tui`.
pub async fn run(conn: Conn) -> Result<(), String> {
    let mut terminal = setup_terminal().map_err(|e| e.to_string())?;
    let result = run_loop(&mut terminal, conn).await;
    restore_terminal(&mut terminal);
    result
}

async fn run_loop(terminal: &mut Tui, conn: Conn) -> Result<(), String> {
    let mut app = App::new();
    let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();

    // Initial REST reconcile so the UI is populated before the first frame.
    reconcile(&conn, &mut app).await;
    spawn_ws(conn.base.clone(), conn.token.clone(), tx.clone());

    let mut keys = EventStream::new();
    let mut redraw = true;

    loop {
        if redraw {
            terminal
                .draw(|f| ui::draw(f, &app))
                .map_err(|e| e.to_string())?;
            redraw = false;
        }
        if app.should_quit {
            return Ok(());
        }
        if app.needs_reconcile {
            reconcile(&conn, &mut app).await;
            redraw = true;
        }

        tokio::select! {
            // WS / reconnect messages
            msg = rx.recv() => {
                match msg {
                    Some(Msg::Event(event)) => { app.apply_event(&event); redraw = true; }
                    Some(Msg::WsClosed) => {
                        // Reconcile now and reconnect after a short backoff
                        reconcile(&conn, &mut app).await;
                        redraw = true;
                        let (base, token, txc) = (conn.base.clone(), conn.token.clone(), tx.clone());
                        tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            spawn_ws(base, token, txc);
                        });
                    }
                    None => return Ok(()),
                }
            }
            // Keyboard
            key = keys.next() => {
                match key {
                    Some(Ok(CtEvent::Key(k))) if k.kind == KeyEventKind::Press => {
                        if let Some(mapped) = map_key(k.code) {
                            let action = app.on_key(mapped);
                            redraw = true;
                            perform(&conn, &mut app, action).await;
                        }
                    }
                    Some(Ok(CtEvent::Resize(_, _))) => redraw = true,
                    Some(Err(_)) | None => return Ok(()),
                    _ => {}
                }
            }
        }
    }
}

async fn perform(conn: &Conn, app: &mut App, action: Action) {
    match action {
        Action::Decide {
            approval_id,
            decision,
        } => {
            if conn.decide(&approval_id, &decision).await.is_ok() {
                // Reconcile so the resolved approval leaves the queue promptly
                reconcile(conn, app).await;
            }
        }
        Action::CancelRun { run_id } => {
            let _ = conn.cancel_run(&run_id).await;
        }
        Action::Quit | Action::None => {}
    }
}

async fn reconcile(conn: &Conn, app: &mut App) {
    if let Ok(runs) = conn.list_runs().await {
        app.set_runs(runs);
    }
    if let Ok(approvals) = conn.list_pending_approvals().await {
        app.set_approvals(approvals);
    }
    app.needs_reconcile = false;
}
