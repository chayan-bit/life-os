//! Test-only hermetic HTTP mock server. `lifeos-cli` has no dependency
//! injection seam for its transport (it always talks real HTTP via
//! `reqwest`), and the workspace has no HTTP-mocking crate yet - so instead
//! of adding one for a single thin client, this binds a real ephemeral
//! localhost socket and speaks just enough HTTP/1.1 to be a convincing
//! stand-in for `lifeos-api`. That keeps `Client`/`commands::*` tests
//! hermetic (no real `lifeos-api`, no network) without touching production
//! code shape at all.
#![cfg(test)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::config::Resolved;

/// A one-shot-per-connection mock server: every connection it accepts gets
/// the same canned `status_line`/`body` response, and the raw request text
/// it received is forwarded to `last_request()` so tests can assert on the
/// method, path, headers, and body the client actually sent.
pub(crate) struct MockServer {
    pub(crate) base_url: String,
    requests: mpsc::Receiver<String>,
}

impl MockServer {
    pub(crate) fn start(status_line: &'static str, body: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let port = listener.local_addr().expect("mock server local addr").port();
        let (tx, rx) = mpsc::channel();

        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut buf = [0u8; 16384];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                let response = format!(
                    "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
                if tx.send(request).is_err() {
                    break;
                }
            }
        });

        Self { base_url: format!("http://127.0.0.1:{port}"), requests: rx }
    }

    /// Blocks (briefly) for the next raw request the server captured.
    pub(crate) fn last_request(&self) -> String {
        self.requests
            .recv_timeout(Duration::from_secs(2))
            .expect("mock server never received a request")
    }
}

/// A `Resolved` pointed at `base_url` with no auth/workspace set - the
/// baseline most tests start from before overriding a field.
pub(crate) fn settings(base_url: &str) -> Resolved {
    Resolved { api_url: base_url.to_string(), token: None, workspace: None }
}

/// Reserves a real TCP port and then releases it immediately, so
/// `http://127.0.0.1:<port>` deterministically refuses connections -
/// for exercising `CliError::Connection` without relying on a hardcoded
/// port that might be in use.
pub(crate) fn unreachable_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind probe port");
    let port = listener.local_addr().expect("probe local addr").port();
    drop(listener);
    format!("http://127.0.0.1:{port}")
}
