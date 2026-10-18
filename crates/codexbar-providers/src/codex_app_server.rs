//! Short sessions with the Codex CLI's app-server (`codex app-server`), the JSON-RPC interface Codex's own editor
//! extensions use (#78). Through it Codex itself signs a Codex home in to ChatGPT, signs it out and renews its tokens,
//! and goes on writing that home's `auth.json`. CodexBar never holds a Codex client id and never writes the file.
//!
//! Each session starts one app-server for one home (`CODEX_HOME`), asks one thing and ends it. Messages are JSON, one
//! per line, without the `jsonrpc` field. Errors never carry tokens: only Codex's short error text is kept.

use std::io::{BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// How long Codex gets to start and answer `initialize`.
const START_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a renewal or sign-out may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How often a wait checks for cancellation.
const POLL: Duration = Duration::from_millis(200);
/// How long a completion may still arrive after Codex answered a cancel: Codex answers the cancel before the
/// sign-in task it stops publishes its own completion.
const AFTER_CANCEL: Duration = Duration::from_secs(2);
/// Longest error text kept from Codex.
const MAX_ERROR: usize = 200;

/// Why a session with Codex failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppServerError {
    /// No `codex` on PATH.
    NotInstalled,
    /// Codex couldn't be started or stopped answering.
    Failed,
    TimedOut,
    Cancelled,
    /// Codex answered with an error, or the sign-in didn't complete. Codex's own short message.
    Refused(String),
}

impl std::fmt::Display for AppServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInstalled => f.write_str("The Codex CLI isn't installed, or `codex` isn't on PATH."),
            Self::Failed => f.write_str("The Codex CLI stopped responding."),
            Self::TimedOut => f.write_str("The Codex CLI didn't answer in time."),
            Self::Cancelled => f.write_str("Sign-in was cancelled."),
            Self::Refused(message) => write!(f, "Codex: {message}"),
        }
    }
}

impl std::error::Error for AppServerError {}

/// Sends one message to the app-server.
pub trait Transport: Send {
    fn send(&mut self, message: &Value) -> Result<(), AppServerError>;
}

/// One app-server: what goes to it, what comes back, and the process (none in tests).
pub struct AppServer {
    // Declared first so it drops first: closing stdin asks Codex to exit before the process is stopped.
    transport: Box<dyn Transport>,
    messages: Receiver<Value>,
    /// Notifications read while waiting for a response, kept for a later wait.
    pending: Vec<Value>,
    next_id: u64,
    _process: Option<Process>,
}

impl AppServer {
    /// Starts `codex app-server` for the Codex home `home` and completes the handshake.
    pub fn start(home: &Path) -> Result<Self, AppServerError> {
        let program = codex_program().ok_or(AppServerError::NotInstalled)?;
        let mut command = crate::platform::command(program);
        command
            .arg("app-server")
            .env("CODEX_HOME", home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut process = crate::contained::spawn(&mut command).map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => AppServerError::NotInstalled,
            _ => AppServerError::Failed,
        })?;
        let stdin = process.child.stdin.take().ok_or(AppServerError::Failed)?;
        let stdout = process.child.stdout.take().ok_or(AppServerError::Failed)?;
        let (sender, messages) = channel();
        std::thread::spawn(move || read_messages(BufReader::new(stdout), &sender));
        let mut server = Self::over(Box::new(Pipe(stdin)), messages);
        server._process = Some(Process(process));
        server.initialize()?;
        Ok(server)
    }

    /// An app-server reached through `transport`, its messages arriving on `messages`. The handshake is the caller's.
    pub fn over(transport: Box<dyn Transport>, messages: Receiver<Value>) -> Self {
        Self {
            transport,
            messages,
            pending: Vec::new(),
            next_id: 1,
            _process: None,
        }
    }

    /// The handshake every connection starts with.
    pub fn initialize(&mut self) -> Result<(), AppServerError> {
        let client = json!({
            "clientInfo": {"name": "codexbar", "title": "CodexBar", "version": env!("CARGO_PKG_VERSION")},
        });
        self.request("initialize", client, START_TIMEOUT)?;
        self.transport.send(&json!({"method": "initialized"}))
    }

    /// Asks Codex to renew this home's ChatGPT tokens now. Codex writes the renewed sign-in to the home itself.
    pub fn renew(&mut self) -> Result<(), AppServerError> {
        let result = self.request("account/read", json!({"refreshToken": true}), REQUEST_TIMEOUT)?;
        match result.get("account") {
            Some(account) if !account.is_null() => Ok(()),
            _ => Err(AppServerError::Refused("This Codex home isn't signed in.".to_owned())),
        }
    }

    /// Signs this home out; Codex deletes its sign-in.
    pub fn sign_out(&mut self) -> Result<(), AppServerError> {
        self.request("account/logout", json!({}), REQUEST_TIMEOUT).map(|_| ())
    }

    /// Starts a ChatGPT browser sign-in. Codex listens for the browser's redirect; the caller opens the URL.
    pub fn begin_sign_in(mut self) -> Result<SignIn, AppServerError> {
        let result = self.request("account/login/start", json!({"type": "chatgpt"}), REQUEST_TIMEOUT)?;
        let field = |name: &str| {
            result
                .get(name)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        match (field("loginId"), field("authUrl")) {
            (Some(login_id), Some(url)) => Ok(SignIn {
                server: self,
                login_id,
                url,
            }),
            _ => Err(AppServerError::Failed),
        }
    }

    fn request(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value, AppServerError> {
        let id = self.next_id;
        self.next_id += 1;
        self.transport
            .send(&json!({"method": method, "id": id, "params": params}))?;
        let deadline = Instant::now() + timeout;
        loop {
            let message = self.next_message(deadline)?;
            if message.get("id").and_then(Value::as_u64) == Some(id) && message.get("method").is_none() {
                return match message.get("error") {
                    Some(error) => Err(refused(error.get("message"))),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
            }
            // A request from Codex (an approval prompt) or another response isn't for this session.
            self.keep(message);
        }
    }

    /// Waits for the notification `method` whose params satisfy `matches`, until `deadline` or cancellation.
    fn wait_for(
        &mut self,
        method: &str,
        matches: impl Fn(&Value) -> bool,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<Value, AppServerError> {
        let is_it = |message: &Value| {
            message.get("method").and_then(Value::as_str) == Some(method)
                && matches(message.get("params").unwrap_or(&Value::Null))
        };
        if let Some(ix) = self.pending.iter().position(is_it) {
            return Ok(self.pending.remove(ix));
        }
        loop {
            // What has already arrived counts before cancellation: a sign-in that completed just as the dialog
            // closed still completes.
            while let Ok(message) = self.messages.try_recv() {
                if is_it(&message) {
                    return Ok(message);
                }
                self.keep(message);
            }
            if cancel.load(Ordering::SeqCst) {
                return Err(AppServerError::Cancelled);
            }
            let step = (Instant::now() + POLL).min(deadline);
            match self.next_message(step) {
                Ok(message) if is_it(&message) => return Ok(message),
                Ok(message) => self.keep(message),
                Err(AppServerError::TimedOut) if Instant::now() < deadline => {}
                Err(err) => return Err(err),
            }
        }
    }

    /// Keeps a notification for a later wait; anything else isn't for this session.
    fn keep(&mut self, message: Value) {
        if message.get("method").is_some() && message.get("id").is_none() {
            self.pending.push(message);
        }
    }

    fn next_message(&mut self, deadline: Instant) -> Result<Value, AppServerError> {
        let wait = deadline.saturating_duration_since(Instant::now());
        match self.messages.recv_timeout(wait) {
            Ok(message) => Ok(message),
            Err(RecvTimeoutError::Timeout) => Err(AppServerError::TimedOut),
            Err(RecvTimeoutError::Disconnected) => Err(AppServerError::Failed),
        }
    }
}

/// A browser sign-in Codex is waiting on.
pub struct SignIn {
    server: AppServer,
    login_id: String,
    url: String,
}

impl SignIn {
    /// The page to open in the user's browser.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Waits until the browser sign-in completes, fails, times out or `cancel` is set. A cancelled or timed-out
    /// sign-in is cancelled in Codex too, so its redirect listener closes.
    pub fn finish(mut self, timeout: Duration, cancel: &AtomicBool) -> Result<(), AppServerError> {
        let login_id = self.login_id.clone();
        let this_one = |params: &Value| params.get("loginId").and_then(Value::as_str) == Some(login_id.as_str());
        let deadline = Instant::now() + timeout;
        let completed = self
            .server
            .wait_for("account/login/completed", this_one, deadline, cancel)
            .or_else(|err| match err {
                AppServerError::Cancelled | AppServerError::TimedOut => {
                    let _ = self
                        .server
                        .request("account/login/cancel", json!({"loginId": login_id}), POLL * 10);
                    // The sign-in may have completed before Codex saw the cancel; then it stands.
                    self.server
                        .wait_for(
                            "account/login/completed",
                            this_one,
                            Instant::now() + AFTER_CANCEL,
                            &AtomicBool::new(false),
                        )
                        .map_err(|_| err)
                }
                err => Err(err),
            });
        match completed {
            Ok(message) => {
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                if params.get("success").and_then(Value::as_bool) == Some(true) {
                    Ok(())
                } else {
                    Err(match params.get("error") {
                        Some(error) if error.is_string() => refused(Some(error)),
                        _ => AppServerError::Refused("The sign-in didn't complete.".to_owned()),
                    })
                }
            }
            Err(err) => Err(err),
        }
    }
}

fn refused(message: Option<&Value>) -> AppServerError {
    let text = message
        .and_then(Value::as_str)
        .unwrap_or("The request was refused.")
        .trim();
    let mut text: String = text.chars().take(MAX_ERROR).collect();
    if text.is_empty() {
        text = "The request was refused.".to_owned();
    }
    AppServerError::Refused(text)
}

/// Reads JSON lines until the app-server exits. Lines that aren't JSON (none are expected) are skipped.
fn read_messages(reader: impl std::io::BufRead, sender: &Sender<Value>) {
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if let Ok(message) = serde_json::from_str::<Value>(&line)
            && sender.send(message).is_err()
        {
            break;
        }
    }
}

struct Pipe(ChildStdin);

impl Transport for Pipe {
    fn send(&mut self, message: &Value) -> Result<(), AppServerError> {
        let mut line = message.to_string();
        line.push('\n');
        self.0
            .write_all(line.as_bytes())
            .and_then(|()| self.0.flush())
            .map_err(|_| AppServerError::Failed)
    }
}

/// The app-server process. `codex` from npm is a `.cmd` that starts Node, which starts `codex.exe`; they all run in
/// one kill-on-close job, so none of them outlives the session or CodexBar. Codex exits by itself when its stdin
/// closes; the job is the backstop.
struct Process(crate::contained::Contained);

impl Drop for Process {
    fn drop(&mut self) {
        // Stdin is closed by now; give Codex a moment to exit on its own.
        let child = &mut self.0.child;
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// The Codex CLI: `codex.exe`, else `codex.cmd` (the npm install) on Windows, from PATH or an install folder.
fn codex_program() -> Option<PathBuf> {
    crate::platform::find_program("codex")
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// The replies a scripted app-server sends for one message.
    type Answer = Box<dyn Fn(&Value) -> Vec<Value> + Send>;

    /// A scripted app-server: each request gets the replies `answer` returns for it.
    struct Fake {
        replies: Sender<Value>,
        answer: Answer,
        seen: Arc<Mutex<Vec<Value>>>,
    }

    impl Transport for Fake {
        fn send(&mut self, message: &Value) -> Result<(), AppServerError> {
            self.seen.lock().unwrap().push(message.clone());
            for reply in (self.answer)(message) {
                let _ = self.replies.send(reply);
            }
            Ok(())
        }
    }

    fn server(answer: impl Fn(&Value) -> Vec<Value> + Send + 'static) -> (AppServer, Arc<Mutex<Vec<Value>>>) {
        let (sender, receiver) = channel();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let fake = Fake {
            replies: sender,
            answer: Box::new(answer),
            seen: seen.clone(),
        };
        (AppServer::over(Box::new(fake), receiver), seen)
    }

    fn method(message: &Value) -> &str {
        message.get("method").and_then(Value::as_str).unwrap_or("")
    }

    fn reply(message: &Value, result: Value) -> Value {
        json!({"id": message["id"], "result": result})
    }

    #[test]
    fn initialize_then_renew_asks_codex_to_refresh() {
        let (mut server, seen) = server(|message| match method(message) {
            "initialize" => vec![reply(message, json!({"userAgent": "codex"}))],
            "account/read" => vec![
                json!({"method": "account/updated", "params": {}}),
                reply(
                    message,
                    json!({"account": {"type": "chatgpt", "email": "a@example.com"}}),
                ),
            ],
            _ => vec![],
        });
        server.initialize().unwrap();
        server.renew().unwrap();
        let seen = seen.lock().unwrap();
        let methods: Vec<&str> = seen.iter().map(method).collect();
        assert_eq!(methods, ["initialize", "initialized", "account/read"]);
        assert_eq!(seen[2]["params"], json!({"refreshToken": true}));
        assert_eq!(seen[0]["params"]["clientInfo"]["name"], "codexbar");
    }

    #[test]
    fn renew_without_a_sign_in_or_with_an_error_fails() {
        let (mut signed_out, _) = server(|message| vec![reply(message, json!({"account": null}))]);
        assert!(matches!(signed_out.renew(), Err(AppServerError::Refused(_))));
        let (mut refused_server, _) = server(|message| {
            vec![json!({"id": message["id"], "error": {"code": -32000, "message": "refresh token expired"}})]
        });
        assert_eq!(
            refused_server.renew(),
            Err(AppServerError::Refused("refresh token expired".to_owned()))
        );
    }

    #[test]
    fn a_server_that_stops_or_stays_silent_fails() {
        let (sender, receiver) = channel::<Value>();
        drop(sender);
        let (fake, _) = server(|_| vec![]);
        let mut gone = AppServer::over(fake.transport, receiver);
        assert_eq!(gone.sign_out(), Err(AppServerError::Failed));
        let (mut silent, _) = server(|_| vec![]);
        let started = Instant::now();
        assert_eq!(
            silent.request("account/logout", json!({}), Duration::from_millis(50)),
            Err(AppServerError::TimedOut)
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    fn sign_in_server(completion: Option<Value>) -> (AppServer, Arc<Mutex<Vec<Value>>>) {
        server(move |message| match method(message) {
            "account/login/start" => {
                let mut replies = vec![reply(
                    message,
                    json!({"type": "chatgpt", "loginId": "login-1", "authUrl": "https://auth.example/start"}),
                )];
                // Another sign-in's completion first: it must not end this one.
                replies.push(json!({"method": "account/login/completed",
                    "params": {"loginId": "other", "success": true, "error": null}}));
                replies.extend(completion.clone());
                replies
            }
            "account/login/cancel" => vec![reply(message, json!({}))],
            _ => vec![],
        })
    }

    #[test]
    fn a_browser_sign_in_returns_the_url_and_waits_for_its_own_completion() {
        let done = json!({"method": "account/login/completed",
            "params": {"loginId": "login-1", "success": true, "error": null}});
        let (server, _) = sign_in_server(Some(done));
        let sign_in = server.begin_sign_in().unwrap();
        assert_eq!(sign_in.url(), "https://auth.example/start");
        assert_eq!(sign_in.finish(Duration::from_secs(5), &AtomicBool::new(false)), Ok(()));
    }

    #[test]
    fn a_failed_browser_sign_in_reports_codex_s_reason() {
        let failed = json!({"method": "account/login/completed",
            "params": {"loginId": "login-1", "success": false, "error": "Login was cancelled in the browser"}});
        let (server, _) = sign_in_server(Some(failed));
        let result = server
            .begin_sign_in()
            .unwrap()
            .finish(Duration::from_secs(5), &AtomicBool::new(false));
        assert_eq!(
            result,
            Err(AppServerError::Refused("Login was cancelled in the browser".to_owned()))
        );
    }

    #[test]
    fn cancelling_or_timing_out_a_sign_in_cancels_it_in_codex() {
        let (server, seen) = sign_in_server(None);
        let cancel = AtomicBool::new(true);
        let result = server.begin_sign_in().unwrap().finish(Duration::from_secs(5), &cancel);
        assert_eq!(result, Err(AppServerError::Cancelled));
        let cancelled = seen.lock().unwrap().last().cloned().unwrap();
        assert_eq!(method(&cancelled), "account/login/cancel");
        assert_eq!(cancelled["params"], json!({"loginId": "login-1"}));

        let (server, seen) = sign_in_server(None);
        let result = server
            .begin_sign_in()
            .unwrap()
            .finish(Duration::from_millis(100), &AtomicBool::new(false));
        assert_eq!(result, Err(AppServerError::TimedOut));
        assert_eq!(method(seen.lock().unwrap().last().unwrap()), "account/login/cancel");
    }

    #[test]
    fn a_sign_in_that_completed_as_it_was_cancelled_still_completes() {
        // The completion is already waiting when the dialog closes.
        let done = json!({"method": "account/login/completed",
            "params": {"loginId": "login-1", "success": true, "error": null}});
        let (queued, _) = sign_in_server(Some(done.clone()));
        let result = queued
            .begin_sign_in()
            .unwrap()
            .finish(Duration::from_secs(5), &AtomicBool::new(true));
        assert_eq!(result, Ok(()));

        // It arrives while Codex answers the cancel.
        let (answered, _) = server(move |message| match method(message) {
            "account/login/start" => vec![reply(
                message,
                json!({"type": "chatgpt", "loginId": "login-1", "authUrl": "https://auth.example/start"}),
            )],
            "account/login/cancel" => vec![done.clone(), reply(message, json!({"status": "notFound"}))],
            _ => vec![],
        });
        let result = answered
            .begin_sign_in()
            .unwrap()
            .finish(Duration::from_secs(5), &AtomicBool::new(true));
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn a_completion_published_after_the_cancel_answer_still_completes() {
        // Codex answers the cancel first; the sign-in task it stopped publishes its completion a moment later.
        let (sender, receiver) = channel();
        let late = sender.clone();
        let fake = Fake {
            replies: sender,
            answer: Box::new(move |message| match method(message) {
                "account/login/start" => vec![reply(
                    message,
                    json!({"type": "chatgpt", "loginId": "login-1", "authUrl": "https://auth.example/start"}),
                )],
                "account/login/cancel" => {
                    let late = late.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(300));
                        let _ = late.send(json!({"method": "account/login/completed",
                            "params": {"loginId": "login-1", "success": true, "error": null}}));
                    });
                    vec![reply(message, json!({"status": "notFound"}))]
                }
                _ => vec![],
            }),
            seen: Arc::default(),
        };
        let server = AppServer::over(Box::new(fake), receiver);
        let result = server
            .begin_sign_in()
            .unwrap()
            .finish(Duration::from_secs(5), &AtomicBool::new(true));
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn errors_are_short_and_never_echo_more_than_codex_s_message() {
        let long = "x".repeat(500);
        let error = refused(Some(&json!(long)));
        assert!(matches!(&error, AppServerError::Refused(text) if text.len() == MAX_ERROR));
        assert_eq!(
            refused(Some(&json!("  "))),
            AppServerError::Refused("The request was refused.".to_owned())
        );
        assert_eq!(
            refused(None),
            AppServerError::Refused("The request was refused.".to_owned())
        );
    }

    /// Starts the real Codex CLI against an empty home: the handshake, a renewal that finds no sign-in, a sign-out, and
    /// the process ending with the session. Needs `codex` on PATH; run with `--ignored`.
    #[test]
    #[ignore = "starts the installed Codex CLI"]
    fn the_installed_codex_cli_answers_for_an_empty_home() {
        let home = std::env::temp_dir().join(format!("codexbar-live-codex-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("config.toml"),
            "cli_auth_credentials_store = \"file\"
",
        )
        .unwrap();
        let started = Instant::now();
        {
            let mut server = AppServer::start(&home).expect("codex app-server starts");
            assert!(matches!(server.renew(), Err(AppServerError::Refused(_))));
            assert_eq!(server.sign_out(), Ok(()));
        }
        assert!(started.elapsed() < Duration::from_secs(60));
        assert!(!home.join("auth.json").exists());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn read_messages_skips_lines_that_are_not_json() {
        let (sender, receiver) = channel();
        read_messages(std::io::Cursor::new("not json\n{\"id\":1}\n"), &sender);
        assert_eq!(receiver.try_recv().unwrap(), json!({"id": 1}));
        assert!(receiver.try_recv().is_err());
    }
}
