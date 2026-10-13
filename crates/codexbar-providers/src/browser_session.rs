//! A disposable browser profile for a website sign-in CodexBar can't do through an OAuth grant or a provider CLI
//! (Gemini Apps, #106). Microsoft Edge or Google Chrome runs on a fresh, empty profile folder of its own, with the
//! DevTools protocol on a local port, and opens the provider's page. The user signs in there themselves; CodexBar
//! only asks the browser for its cookies (`Storage.getCookies`), never for page contents, passwords or scripts. When
//! the attempt ends, the browser is closed and the folder deleted, so nothing of the session stays on disk.
//!
//! Limits, recorded for the security review: while the attempt runs, the DevTools port accepts local connections, so
//! another program running as the same user could read that profile's cookies too. The browser runs in a kill-on-close
//! Windows job, so it can't outlive the attempt or CodexBar.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Why a browser sign-in couldn't run.
#[derive(Debug)]
pub enum BrowserError {
    /// Neither Microsoft Edge nor Google Chrome is installed where Windows puts them.
    NoBrowser,
    /// The browser didn't start, or its DevTools didn't answer in time.
    NotStarted,
    /// The DevTools connection failed or answered in a shape CodexBar doesn't know.
    Protocol,
}

impl std::fmt::Display for BrowserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NoBrowser => "Microsoft Edge or Google Chrome is needed for this sign-in.",
            Self::NotStarted => "The sign-in browser didn't start.",
            Self::Protocol => "CodexBar lost its connection to the sign-in browser.",
        })
    }
}

impl std::error::Error for BrowserError {}

/// Where Edge and Chrome install, in the order they're tried.
pub fn candidates() -> Vec<PathBuf> {
    let env = |name: &str| std::env::var_os(name).map(PathBuf::from);
    let mut paths = Vec::new();
    for base in [env("ProgramFiles(x86)"), env("ProgramFiles"), env("LOCALAPPDATA")]
        .into_iter()
        .flatten()
    {
        paths.push(base.join(r"Microsoft\Edge\Application\msedge.exe"));
    }
    for base in [env("ProgramFiles"), env("ProgramFiles(x86)"), env("LOCALAPPDATA")]
        .into_iter()
        .flatten()
    {
        paths.push(base.join(r"Google\Chrome\Application\chrome.exe"));
    }
    paths
}

/// The browser to use: the first candidate that exists.
pub fn find_browser() -> Option<PathBuf> {
    candidates().into_iter().find(|path| path.is_file())
}

/// The port and browser endpoint path the browser writes to `DevToolsActivePort` in its profile folder.
pub fn parse_active_port(text: &str) -> Option<(u16, String)> {
    let mut lines = text.lines();
    let port: u16 = lines.next()?.trim().parse().ok().filter(|p| *p > 0)?;
    let path = lines.next()?.trim();
    (path.starts_with("/devtools/browser/") && path.chars().all(|c| c.is_ascii_graphic()))
        .then(|| (port, path.to_owned()))
}

/// A client-to-server WebSocket text frame (RFC 6455 section 5): FIN, text opcode, masked with `mask`.
pub fn encode_frame(text: &str, mask: [u8; 4]) -> Vec<u8> {
    let payload = text.as_bytes();
    let mut frame = vec![0x81];
    match payload.len() {
        len @ 0..=125 => frame.push(0x80 | len as u8),
        len @ 126..=0xFFFF => {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(len as u16).to_be_bytes());
        }
        len => {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(len as u64).to_be_bytes());
        }
    }
    frame.extend_from_slice(&mask);
    frame.extend(payload.iter().enumerate().map(|(ix, byte)| byte ^ mask[ix % 4]));
    frame
}

/// One frame read from the server: its opcode and unmasked payload.
pub fn read_frame(stream: &mut impl Read) -> std::io::Result<(u8, Vec<u8>)> {
    let mut head = [0u8; 2];
    stream.read_exact(&mut head)?;
    let opcode = head[0] & 0x0F;
    let masked = head[1] & 0x80 != 0;
    let len = match head[1] & 0x7F {
        126 => {
            let mut bytes = [0u8; 2];
            stream.read_exact(&mut bytes)?;
            u16::from_be_bytes(bytes) as u64
        }
        127 => {
            let mut bytes = [0u8; 8];
            stream.read_exact(&mut bytes)?;
            u64::from_be_bytes(bytes)
        }
        len => len as u64,
    };
    // Cookie lists are small; anything this large isn't a DevTools reply CodexBar asked for.
    if len > 16 * 1024 * 1024 {
        return Err(std::io::Error::other("frame too large"));
    }
    let mask = if masked {
        let mut mask = [0u8; 4];
        stream.read_exact(&mut mask)?;
        Some(mask)
    } else {
        None
    };
    let mut payload = vec![0u8; len as usize];
    stream.read_exact(&mut payload)?;
    if let Some(mask) = mask {
        for (ix, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[ix % 4];
        }
    }
    Ok((opcode, payload))
}

/// Random bytes for the WebSocket handshake; profile names get their own, checked, source.
fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    // A failed source only weakens the WebSocket key and mask, which carry no secret on a local connection.
    let _ = getrandom::fill(&mut bytes);
    bytes
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for (ix, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if ix <= chunk.len() {
                out.push(ALPHABET[(n >> shift & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A DevTools connection to the browser endpoint.
pub struct DevTools {
    stream: TcpStream,
    next_id: u64,
}

impl DevTools {
    /// Connects to `ws://127.0.0.1:{port}{path}`; only the loopback address is ever used.
    pub fn connect(port: u16, path: &str) -> Result<Self, BrowserError> {
        let mut stream =
            TcpStream::connect_timeout(&(std::net::Ipv4Addr::LOCALHOST, port).into(), Duration::from_secs(5))
                .map_err(|_| BrowserError::Protocol)?;
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .map_err(|_| BrowserError::Protocol)?;
        let key = base64(&random_bytes::<16>());
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|_| BrowserError::Protocol)?;
        // Read the handshake reply up to its blank line, one byte at a time so no frame is consumed with it.
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).map_err(|_| BrowserError::Protocol)?;
            head.push(byte[0]);
            if head.len() > 8192 {
                return Err(BrowserError::Protocol);
            }
        }
        if !head.starts_with(b"HTTP/1.1 101") {
            return Err(BrowserError::Protocol);
        }
        Ok(Self { stream, next_id: 1 })
    }

    /// Sends a DevTools command and returns its `result`, skipping events.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, BrowserError> {
        let id = self.next_id;
        self.next_id += 1;
        let message = json!({ "id": id, "method": method, "params": params }).to_string();
        self.stream
            .write_all(&encode_frame(&message, random_bytes::<4>()))
            .map_err(|_| BrowserError::Protocol)?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let (opcode, payload) = read_frame(&mut self.stream).map_err(|_| BrowserError::Protocol)?;
            match opcode {
                0x1 => {
                    let value: Value = serde_json::from_slice(&payload).map_err(|_| BrowserError::Protocol)?;
                    if value.get("id").and_then(Value::as_u64) == Some(id) {
                        return value.get("result").cloned().ok_or(BrowserError::Protocol);
                    }
                }
                0x8 => return Err(BrowserError::Protocol),
                _ => {}
            }
        }
        Err(BrowserError::Protocol)
    }
}

/// A browser running on a disposable profile. Dropping it ends the browser and deletes the profile.
pub struct DisposableBrowser {
    profile: PathBuf,
    process: Option<crate::contained::Contained>,
    devtools: Option<DevTools>,
}

impl DisposableBrowser {
    /// Starts `browser` on a new profile folder inside `parent` and opens `url` in it.
    pub fn open(browser: &Path, parent: &Path, url: &str) -> Result<Self, BrowserError> {
        // A name nobody can predict, in a folder that must not exist yet: a sign-in never starts in a profile another
        // attempt left or is still using.
        let mut name = [0u8; 9];
        getrandom::fill(&mut name).map_err(|_| BrowserError::NotStarted)?;
        let profile = parent.join(format!("browser-{}", base64(&name).replace(['+', '/'], "-")));
        std::fs::create_dir_all(parent).map_err(|_| BrowserError::NotStarted)?;
        std::fs::create_dir(&profile).map_err(|_| BrowserError::NotStarted)?;
        let mut session = Self {
            profile,
            process: None,
            devtools: None,
        };
        let mut command = std::process::Command::new(browser);
        command.arg(format!("--user-data-dir={}", session.profile.display()));
        command.args([
            "--remote-debugging-port=0",
            "--remote-allow-origins=http://127.0.0.1",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-sync",
            "--new-window",
            url,
        ]);
        session.process = Some(crate::contained::spawn(&mut command).map_err(|_| BrowserError::NotStarted)?);
        let port_file = session.profile.join("DevToolsActivePort");
        let deadline = Instant::now() + Duration::from_secs(30);
        let (port, path) = loop {
            if let Some(found) = std::fs::read_to_string(&port_file)
                .ok()
                .as_deref()
                .and_then(parse_active_port)
            {
                break found;
            }
            if Instant::now() > deadline {
                return Err(BrowserError::NotStarted);
            }
            std::thread::sleep(Duration::from_millis(200));
        };
        session.devtools = Some(DevTools::connect(port, &path)?);
        Ok(session)
    }

    /// The browser's cookies for `urls`, as DevTools reports them.
    pub fn cookies(&mut self) -> Result<Value, BrowserError> {
        let devtools = self.devtools.as_mut().ok_or(BrowserError::Protocol)?;
        devtools.call("Storage.getCookies", json!({}))
    }

    /// True while the browser process runs.
    pub fn running(&mut self) -> bool {
        self.process
            .as_mut()
            .is_some_and(|process| matches!(process.child.try_wait(), Ok(None)))
    }
}

impl Drop for DisposableBrowser {
    fn drop(&mut self) {
        if let Some(devtools) = self.devtools.as_mut() {
            let _ = devtools.call("Browser.close", json!({}));
        }
        if let Some(mut process) = self.process.take() {
            let _ = process.child.kill();
            let _ = process.child.wait();
            // Dropping it closes the job, which ends anything the browser started.
        }
        // The browser can hold files briefly after it exits.
        for _ in 0..20 {
            match std::fs::remove_dir_all(&self.profile) {
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                    std::thread::sleep(Duration::from_millis(250))
                }
                _ => break,
            }
        }
    }
}

/// Deletes disposable profiles an interrupted attempt left in `parent`. Only a missing `parent` is nothing to do; a
/// folder that can't be listed is an error, never a clean result, since a profile holds a Google session.
pub fn clean_up(parent: &Path) -> std::io::Result<usize> {
    let mut removed = 0;
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(err),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with("browser-") && entry.path().is_dir() {
            std::fs::remove_dir_all(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_devtools_port_file_gives_the_port_and_browser_path() {
        assert_eq!(
            parse_active_port("53211\n/devtools/browser/0b1c2d3e-aaaa\n"),
            Some((53211, "/devtools/browser/0b1c2d3e-aaaa".to_owned()))
        );
        assert_eq!(parse_active_port("0\n/devtools/browser/x"), None);
        assert_eq!(parse_active_port("53211\n/devtools/page/x"), None);
        assert_eq!(parse_active_port("53211"), None);
    }

    #[test]
    fn frames_are_masked_and_read_back() {
        let frame = encode_frame("hello", [1, 2, 3, 4]);
        assert_eq!(frame[0], 0x81);
        assert_eq!(frame[1], 0x80 | 5);
        assert_ne!(&frame[6..], b"hello", "the payload is masked");
        let (opcode, payload) = read_frame(&mut frame.as_slice()).unwrap();
        assert_eq!((opcode, payload.as_slice()), (1, b"hello".as_slice()));
        let long = "x".repeat(70_000);
        let (_, payload) = read_frame(&mut encode_frame(&long, [9, 8, 7, 6]).as_slice()).unwrap();
        assert_eq!(payload.len(), 70_000);
        let medium = "y".repeat(300);
        let frame = encode_frame(&medium, [0, 0, 0, 0]);
        assert_eq!(frame[1], 0x80 | 126);
    }

    #[test]
    fn base64_matches_rfc_4648() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn edge_comes_before_chrome() {
        let paths = candidates();
        let edge = paths.iter().position(|p| p.ends_with("msedge.exe"));
        let chrome = paths.iter().position(|p| p.ends_with("chrome.exe"));
        assert!(edge < chrome);
    }

    #[test]
    fn leftover_profiles_are_cleaned_up_and_nothing_else() {
        let parent = std::env::temp_dir().join(format!("codexbar-browser-{}", std::process::id()));
        std::fs::create_dir_all(parent.join("browser-abc").join("Default")).unwrap();
        std::fs::create_dir_all(parent.join("keep")).unwrap();
        assert_eq!(clean_up(&parent).unwrap(), 1);
        assert!(parent.join("keep").is_dir());
        // A folder that isn't there is nothing to do; one that can't be listed is an error, never a clean result.
        assert_eq!(clean_up(&parent.join("missing")).unwrap(), 0);
        std::fs::write(parent.join("not-a-folder"), "").unwrap();
        assert!(clean_up(&parent.join("not-a-folder")).is_err());
        std::fs::remove_dir_all(&parent).unwrap();
    }
}
