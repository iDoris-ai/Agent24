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
use crossterm::event::{Event as CtEvent, EventStream, KeyCode, KeyEventKind, KeyModifiers};
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
    /// FU-74: `self.base` is always the daemon on loopback (`agent24-cli`
    /// only ever constructs `Conn` from an `Endpoint` whose base is
    /// `http://127.0.0.1:{port}`), and every call through this client carries
    /// the bearer token. The default reqwest
    /// client reads `HTTP_PROXY`/`ALL_PROXY` without bypassing loopback and
    /// follows redirects — a stray proxy env var (or an impersonator that
    /// answers with a 3xx, since the daemon itself never redirects) would
    /// otherwise ship the bearer token off-box.
    fn client(&self) -> reqwest::Client {
        #[expect(
            clippy::expect_used,
            reason = "unwrap_or_default() here would silently rebuild the proxy-reading, \
                      redirect-following client FU-74 exists to rule out; fail closed instead"
        )]
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("building the loopback-only daemon HTTP client failed")
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
        let res = self
            .auth(
                self.client()
                    .post(format!("{}/api/v1/runs/{run_id}/cancel", self.base)),
            )
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if res.status().is_success() {
            Ok(())
        } else {
            Err(format!("cancel rejected: {}", res.status()))
        }
    }
}

/// Bounded so a stalled UI can't let WS events accumulate without limit
/// (review C6). On overflow the WS reader drops the frame; the resulting seq
/// gap — and the periodic reconcile — repair the view from REST truth.
const EVENT_CHANNEL_CAP: usize = 1024;
/// Safety-net reconcile cadence: even with no events, the view can never stay
/// stale (or miss an approval created in a subscription gap) longer than this.
const RECONCILE_EVERY: Duration = Duration::from_secs(15);

/// Messages the async tasks feed into the single-threaded UI loop.
enum Msg {
    Event(Event),
    /// The WS stream ended — the loop should reconcile and reconnect
    WsClosed,
}

/// Spawn the WS reader. It emits [`Msg::Event`] per frame and [`Msg::WsClosed`]
/// when the socket ends, then the loop re-arms it. Sends are non-blocking: on a
/// full channel the frame is dropped (the seq gap + periodic reconcile repair
/// it) so the reader never stalls or deadlocks against a busy UI.
fn spawn_ws(base: String, token: String, tx: mpsc::Sender<Msg>) {
    tokio::spawn(async move {
        // http(s)://host → ws(s)://host
        let ws_url = format!("{}/api/v1/events", base.replacen("http", "ws", 1));
        let request = match build_ws_request(&ws_url, &token) {
            Ok(req) => req,
            Err(_) => {
                // WsClosed drives reconnect — deliver it reliably (await), never
                // try_send, so a full channel can't strand the TUI on REST-only
                // polling (review C6).
                let _ = tx.send(Msg::WsClosed).await;
                return;
            }
        };
        if let Ok((mut socket, _)) = tokio_tungstenite::connect_async(request).await {
            while let Some(frame) = socket.next().await {
                match frame {
                    Ok(tungstenite::Message::Text(text)) => {
                        if let Ok(event) = serde_json::from_str::<Event>(&text) {
                            match tx.try_send(Msg::Event(event)) {
                                Err(mpsc::error::TrySendError::Closed(_)) => return, // UI gone
                                Err(mpsc::error::TrySendError::Full(_)) => {} // drop → gap repairs
                                Ok(()) => {}
                            }
                        }
                    }
                    Ok(tungstenite::Message::Close(_)) | Err(_) => break,
                    _ => {}
                }
            }
        }
        // Guaranteed delivery: the reconnect signal must not be dropped
        let _ = tx.send(Msg::WsClosed).await;
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

/// Owns the raw-mode / alternate-screen terminal and restores it on Drop —
/// covers the `?` early-return AND panic-unwind paths (review C6), so the
/// user's shell is never left in raw mode. Partial setup failure is unwound
/// before returning.
struct TerminalGuard {
    terminal: Tui,
}

impl TerminalGuard {
    fn new() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        // Any failure after raw mode is enabled fully unwinds ALL terminal
        // state (leave alt screen + show cursor + disable raw mode) before
        // erroring — a partial success must never leave a wrecked terminal.
        if let Err(err) = stdout
            .execute(EnterAlternateScreen)
            .and_then(|s| s.execute(cursor::Hide))
        {
            hard_restore();
            return Err(err);
        }
        match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(err) => {
                hard_restore();
                Err(err)
            }
        }
    }
}

/// Best-effort full terminal restore against a fresh stdout handle — used on
/// the partial-setup-failure path where no `Terminal` exists yet.
fn hard_restore() {
    let mut out = io::stdout();
    let _ = out.execute(LeaveAlternateScreen);
    let _ = out.execute(cursor::Show);
    let _ = disable_raw_mode();
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = self.terminal.backend_mut().execute(LeaveAlternateScreen);
        let _ = self.terminal.backend_mut().execute(cursor::Show);
        let _ = self.terminal.show_cursor();
    }
}

fn map_key(code: KeyCode, mods: KeyModifiers) -> Option<Key> {
    // Ctrl+C / Ctrl+D are the universal "get me out" gesture — they must QUIT,
    // never be seen as a plain 'c'/'d'. In raw mode crossterm delivers Ctrl+C
    // as Char('c') + CONTROL, so the modifier MUST be inspected before the
    // Char arms below (review #40): otherwise Ctrl+C would silently cancel the
    // selected run.
    if mods.contains(KeyModifiers::CONTROL) {
        return match code {
            KeyCode::Char('c') | KeyCode::Char('d') => Some(Key::Quit),
            _ => None, // ignore other Ctrl combos
        };
    }
    match code {
        KeyCode::Up => Some(Key::Up),
        KeyCode::Down => Some(Key::Down),
        KeyCode::Enter => Some(Key::Enter),
        KeyCode::Esc => Some(Key::Esc),
        KeyCode::Tab => Some(Key::Tab),
        KeyCode::Char('q') => Some(Key::Quit),
        KeyCode::Char('c') => Some(Key::Cancel),
        KeyCode::Char(c) => Some(Key::Char(c)),
        KeyCode::Backspace => Some(Key::Backspace),
        _ => None,
    }
}

/// Entry point for `agent24 tui`.
pub async fn run(conn: Conn) -> Result<(), String> {
    let mut guard = TerminalGuard::new().map_err(|e| e.to_string())?;
    // The guard's Drop restores the terminal on every exit path — normal
    // return, `?`, or panic unwind.
    run_loop(&mut guard.terminal, conn).await
}

async fn run_loop(terminal: &mut Tui, conn: Conn) -> Result<(), String> {
    let mut app = App::new();
    let (tx, mut rx) = mpsc::channel::<Msg>(EVENT_CHANNEL_CAP);

    // Subscribe to the WS stream BEFORE the initial reconcile: any
    // approval.required created during reconcile then sits buffered in the
    // channel and applies afterwards, instead of vanishing into the
    // no-replay subscription gap (review C6 blocker).
    spawn_ws(conn.base.clone(), conn.token.clone(), tx.clone());
    reconcile(&conn, &mut app).await;

    let mut keys = EventStream::new();
    let mut ticker = tokio::time::interval(RECONCILE_EVERY);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker.tick().await; // consume the immediate first tick
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
                        if let Some(mapped) = map_key(k.code, k.modifiers) {
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
            // Safety-net periodic reconcile — repairs anything a dropped frame
            // or subscription gap might have missed
            _ = ticker.tick() => {
                reconcile(&conn, &mut app).await;
                redraw = true;
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
            // Refresh so the run's new status shows even if the WS event races
            reconcile(conn, app).await;
        }
        Action::Quit | Action::None => {}
    }
}

/// REST reconcile. `needs_reconcile` is cleared ONLY when BOTH lists refresh —
/// a transient failure must not mark stale state clean (review C6), so the
/// loop keeps retrying until a full refresh lands.
async fn reconcile(conn: &Conn, app: &mut App) {
    let mut ok = true;
    match conn.list_runs().await {
        Ok(runs) => app.set_runs(runs),
        Err(_) => ok = false,
    }
    match conn.list_pending_approvals().await {
        Ok(approvals) => app.set_approvals(approvals),
        Err(_) => ok = false,
    }
    // set_runs/set_approvals each clear the flag; only a fully-successful pass
    // leaves it cleared.
    app.needs_reconcile = !ok;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn ctrl_c_and_ctrl_d_quit_not_cancel() {
        // Raw mode delivers Ctrl+C as Char('c') + CONTROL; it must map to Quit,
        // never Cancel (which would silently cancel the selected run) — review #40.
        assert_eq!(
            map_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Some(Key::Quit)
        );
        assert_eq!(
            map_key(KeyCode::Char('d'), KeyModifiers::CONTROL),
            Some(Key::Quit)
        );
        // a plain 'c' still cancels; plain 'q' quits
        assert_eq!(
            map_key(KeyCode::Char('c'), KeyModifiers::NONE),
            Some(Key::Cancel)
        );
        assert_eq!(
            map_key(KeyCode::Char('q'), KeyModifiers::NONE),
            Some(Key::Quit)
        );
        // other Ctrl combos are ignored, not passed through as Char
        assert_eq!(map_key(KeyCode::Char('a'), KeyModifiers::CONTROL), None);
    }

    // ── FU-74: `Conn::client()` must not honour HTTP_PROXY ─────────────────
    //
    // Same shape as `tests::cli_client_ignores_http_proxy` in `super::super`
    // (crate::main), agent24-models' `from_env_local_providers_ignore_http_proxy`,
    // and agent24-worker's `http_ml_worker_ignores_http_proxy`. See the CLI
    // client test's comment for why the target-port env var is safe to spell
    // SCREAMING_SNAKE_CASE here: `apps/agent24-cli` is outside what
    // `passthrough_list_matches_what_the_daemon_actually_reads` scans.

    /// A blocking stub on its own thread: counts connections, answers `reply`.
    fn thread_stub(reply: String) -> (u16, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let n = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let n2 = n.clone();
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { continue };
                n2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = s.set_read_timeout(Some(Duration::from_millis(300)));
                let mut buf = [0u8; 65536];
                let _ = s.read(&mut buf);
                let _ = s.write_all(reply.as_bytes());
            }
        });
        (port, n)
    }

    fn runs_ok_reply() -> String {
        let body = r#"{"runs":[]}"#;
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn target_port() -> u16 {
        std::env::var("AGENT24_TUI_TEST_TARGET_PORT")
            .expect("run_child must set AGENT24_TUI_TEST_TARGET_PORT")
            .parse()
            .expect("AGENT24_TUI_TEST_TARGET_PORT must be a u16")
    }

    /// Child: the production path — `Conn::client()` must be loopback-only,
    /// and the bearer token must not be handed to a proxy.
    #[tokio::test]
    #[ignore = "child process of tui_conn_client_ignores_http_proxy"]
    async fn proxy_child_conn_client() {
        let conn = Conn {
            base: format!("http://127.0.0.1:{}", target_port()),
            token: "SECRET-BEARER".to_owned(),
        };
        let _ = conn
            .auth(conn.client().get(format!("{}/api/v1/runs", conn.base)))
            .timeout(Duration::from_millis(500))
            .send()
            .await;
    }

    /// Child: positive control — the bare default client, same URL, same env.
    #[tokio::test]
    #[ignore = "child process of tui_conn_client_ignores_http_proxy"]
    async fn proxy_child_default_client() {
        let url = format!("http://127.0.0.1:{}/api/v1/runs", target_port());
        let raw_client = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(500))
            .build()
            .unwrap();
        let _ = raw_client
            .get(url)
            .timeout(Duration::from_millis(500))
            .send()
            .await;
    }

    fn run_child(test: &str, target: u16, proxy: u16) {
        let exe = std::env::current_exe().unwrap();
        let proxy_url = format!("http://127.0.0.1:{proxy}");
        let status = std::process::Command::new(exe)
            .args(["--exact", test, "--ignored", "--nocapture"])
            .env("AGENT24_TUI_TEST_TARGET_PORT", target.to_string())
            .env("HTTP_PROXY", &proxy_url)
            .env("http_proxy", &proxy_url)
            .env("ALL_PROXY", &proxy_url)
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn tui_conn_client_ignores_http_proxy() {
        let (tp, target) = thread_stub(runs_ok_reply());
        let (pp, proxy) = thread_stub(runs_ok_reply());
        run_child("tui::tests::proxy_child_conn_client", tp, pp);
        assert_eq!(
            proxy.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the proxy saw the TUI daemon client's request"
        );
        assert_eq!(target.load(std::sync::atomic::Ordering::SeqCst), 1);

        // Positive control: the default client under the same env goes through
        // the proxy — proves HTTP_PROXY was actually live for the child.
        let (tp2, target2) = thread_stub(runs_ok_reply());
        let (pp2, proxy2) = thread_stub(runs_ok_reply());
        run_child("tui::tests::proxy_child_default_client", tp2, pp2);
        assert_eq!(
            proxy2.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "measuring instrument: proxy env must take effect"
        );
        assert_eq!(target2.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}
