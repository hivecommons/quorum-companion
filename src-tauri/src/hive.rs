//! The hive's companion API (hive docs/meetings.md, "Desktop companion").
//!
//! Every request carries the per-user companion token in `X-Hive-Companion-Token`; the hive
//! admits it on `/api/companion/*` only. TLS is required except for a hive on this machine.

use std::time::Duration;

use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use url::Url;

pub const TOKEN_HEADER: &str = "X-Hive-Companion-Token";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub enum HiveError {
    /// The hive wants chunk `expected` next on this channel.
    Gap { expected: u64 },
    /// The hive answered with an error status.
    Status { status: u16, message: String },
    /// The request did not complete.
    Network(String),
    /// The settings are not usable (bad URL, no token).
    Config(String),
}

impl std::fmt::Display for HiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HiveError::Gap { expected } => {
                write!(f, "chunk out of order; the hive expects {expected}")
            }
            HiveError::Status { status, message } => write!(f, "hive answered {status}: {message}"),
            HiveError::Network(m) => write!(f, "cannot reach the hive: {m}"),
            HiveError::Config(m) => write!(f, "{m}"),
        }
    }
}

/// GET /api/companion/info.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Info {
    pub consent_version: String,
    pub consent_text: String,
    pub sample_rate: u32,
    pub max_chunk_bytes: usize,
}

/// POST /api/companion/meetings.
#[derive(Debug, Serialize)]
pub struct StartRequest<'a> {
    pub title: &'a str,
    pub app: &'a str,
    pub host_name: &'a str,
    pub consent_version: &'a str,
    pub consent_attested: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Meeting {
    pub id: String,
    #[serde(default)]
    pub state: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Line {
    pub seq: i64,
    pub text: String,
    #[serde(default)]
    pub at: String,
}

/// GET /api/companion/meetings/{id}/replies.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Replies {
    #[serde(default)]
    pub lines: Vec<Line>,
    pub next_seq: i64,
    pub state: String,
    /// Play Betty's listening chime now (true once per chime; an older hive leaves it out).
    #[serde(default)]
    pub tone: bool,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    #[serde(default)]
    error: String,
    expected_seq: Option<u64>,
}

/// Checks a hive URL: https, or http only for a hive on this machine.
pub fn validate_base(raw: &str) -> Result<String, HiveError> {
    let url = Url::parse(raw.trim()).map_err(|e| HiveError::Config(format!("hive URL: {e}")))?;
    let host = url.host_str().unwrap_or_default().to_string();
    let local = matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]" | "::1");
    match url.scheme() {
        "https" => {}
        "http" if local => {}
        _ => return Err(HiveError::Config("the hive URL must use https".into())),
    }
    if host.is_empty() {
        return Err(HiveError::Config("the hive URL has no host".into()));
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

pub struct Hive {
    base: String,
    token: String,
    http: Client,
}

impl Hive {
    pub fn new(base: &str, token: &str) -> Result<Hive, HiveError> {
        if token.trim().is_empty() {
            return Err(HiveError::Config(
                "no companion token: create one in the hive dashboard".into(),
            ));
        }
        let base = validate_base(base)?;
        let http = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .https_only(!base.starts_with("http://"))
            .build()
            .map_err(|e| HiveError::Config(e.to_string()))?;
        Ok(Hive {
            base,
            token: token.trim().to_string(),
            http,
        })
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.http
            .request(method, format!("{}{}", self.base, path))
            .header(TOKEN_HEADER, &self.token)
    }

    fn send(builder: RequestBuilder) -> Result<Response, HiveError> {
        let resp = builder
            .send()
            .map_err(|e| HiveError::Network(e.to_string()))?;
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status();
        let body: ErrorBody = resp.json().unwrap_or(ErrorBody {
            error: String::new(),
            expected_seq: None,
        });
        if status == StatusCode::CONFLICT {
            if let Some(expected) = body.expected_seq {
                return Err(HiveError::Gap { expected });
            }
        }
        Err(HiveError::Status {
            status: status.as_u16(),
            message: body.error,
        })
    }

    fn json<T: for<'de> Deserialize<'de>>(resp: Response) -> Result<T, HiveError> {
        resp.json()
            .map_err(|e| HiveError::Network(format!("unreadable reply: {e}")))
    }

    pub fn info(&self) -> Result<Info, HiveError> {
        Self::json(Self::send(
            self.request(Method::GET, "/api/companion/info"),
        )?)
    }

    pub fn start(&self, req: &StartRequest) -> Result<Meeting, HiveError> {
        Self::json(Self::send(
            self.request(Method::POST, "/api/companion/meetings")
                .json(req),
        )?)
    }

    pub fn audio(&self, id: &str, channel: &str, seq: u64, pcm: Vec<u8>) -> Result<(), HiveError> {
        let path = format!("/api/companion/meetings/{id}/audio?channel={channel}&seq={seq}");
        Self::send(
            self.request(Method::POST, &path)
                .header("Content-Type", "application/octet-stream")
                .body(pcm),
        )?;
        Ok(())
    }

    pub fn replies(&self, id: &str, after: i64) -> Result<Replies, HiveError> {
        let path = format!(
            "/api/companion/meetings/{id}/replies?after={}",
            after.max(0)
        );
        Self::json(Self::send(self.request(Method::GET, &path))?)
    }

    pub fn end(&self, id: &str) -> Result<(), HiveError> {
        Self::send(self.request(Method::POST, &format!("/api/companion/meetings/{id}/end")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hive_urls_need_tls_unless_local() {
        assert_eq!(
            validate_base("https://hive.example.dev/").unwrap(),
            "https://hive.example.dev"
        );
        assert_eq!(
            validate_base(" https://h.example/x/ ").unwrap(),
            "https://h.example/x"
        );
        assert!(validate_base("http://localhost:3002").is_ok());
        assert!(validate_base("http://127.0.0.1:3002").is_ok());
        assert!(validate_base("http://hive.example.dev").is_err());
        assert!(validate_base("ftp://hive.example.dev").is_err());
        assert!(validate_base("not a url").is_err());
    }

    #[test]
    fn a_client_needs_a_token_and_a_valid_url() {
        assert!(matches!(
            Hive::new("https://h.example", " "),
            Err(HiveError::Config(_))
        ));
        assert!(matches!(
            Hive::new("http://h.example", "tok"),
            Err(HiveError::Config(_))
        ));
        assert!(Hive::new("https://h.example", "tok").is_ok());
    }

    #[test]
    fn errors_read_well() {
        assert_eq!(
            HiveError::Gap { expected: 3 }.to_string(),
            "chunk out of order; the hive expects 3"
        );
        assert_eq!(
            HiveError::Status {
                status: 403,
                message: "nope".into()
            }
            .to_string(),
            "hive answered 403: nope"
        );
    }

    #[test]
    fn replies_decode_with_defaults() {
        let r: Replies = serde_json::from_str(
            r#"{"next_seq":2,"state":"live","lines":[{"seq":2,"text":"hi"}]}"#,
        )
        .unwrap();
        assert_eq!(
            r.lines,
            vec![Line {
                seq: 2,
                text: "hi".into(),
                at: String::new()
            }]
        );
        assert!(!r.tone);
        let empty: Replies = serde_json::from_str(r#"{"next_seq":0,"state":"ended"}"#).unwrap();
        assert!(empty.lines.is_empty());
        let chime: Replies =
            serde_json::from_str(r#"{"next_seq":3,"state":"live","tone":true}"#).unwrap();
        assert!(chime.tone);
    }
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread::JoinHandle;

    /// Serves one canned `(status, body)` per connection on a loopback port, then returns the raw
    /// requests it saw (lowercased, so header names compare however the client cases them).
    fn serve(replies: Vec<(u16, &'static str)>) -> (String, JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for (status, body) in replies {
                let (mut conn, _) = listener.accept().unwrap();
                let mut raw = Vec::new();
                let mut buf = [0u8; 1024];
                let head_end = loop {
                    let n = conn.read(&mut buf).unwrap();
                    raw.extend_from_slice(&buf[..n]);
                    if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                    assert!(n > 0, "client closed before finishing its request head");
                };
                let head = String::from_utf8_lossy(&raw[..head_end]).to_lowercase();
                let want = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                while raw.len() < head_end + want {
                    let n = conn.read(&mut buf).unwrap();
                    assert!(n > 0, "client closed before finishing its request body");
                    raw.extend_from_slice(&buf[..n]);
                }
                seen.push(String::from_utf8_lossy(&raw).to_lowercase());
                let reply = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                conn.write_all(reply.as_bytes()).unwrap();
            }
            seen
        });
        (base, handle)
    }

    #[test]
    fn a_conflict_with_expected_seq_is_a_gap_and_the_request_is_well_formed() {
        let (base, server) = serve(vec![(409, r#"{"error":"gap","expected_seq":7}"#)]);
        let hive = Hive::new(&base, " tok ").unwrap();
        let err = hive.audio("m1", "mic", 3, vec![1, 2, 3, 4]).unwrap_err();
        assert!(matches!(err, HiveError::Gap { expected: 7 }), "{err:?}");
        let seen = server.join().unwrap();
        assert!(
            seen[0].starts_with("post /api/companion/meetings/m1/audio?channel=mic&seq=3 "),
            "{}",
            seen[0]
        );
        assert!(seen[0].contains("x-hive-companion-token: tok\r\n"), "{}", seen[0]);
        assert!(seen[0].contains("content-type: application/octet-stream"));
        assert!(seen[0].contains("content-length: 4"));
    }

    #[test]
    fn other_error_statuses_keep_the_hives_message() {
        let (base, server) = serve(vec![
            (403, r#"{"error":"nope"}"#),
            (409, r#"{"error":"ended"}"#),
            (500, "<html>boom</html>"),
        ]);
        let hive = Hive::new(&base, "tok").unwrap();
        let denied = hive.info().unwrap_err();
        assert!(
            matches!(&denied, HiveError::Status { status: 403, message } if message == "nope"),
            "{denied:?}"
        );
        // A 409 without expected_seq is an ordinary status error, not a gap.
        let ended = hive.end("m1").unwrap_err();
        assert!(
            matches!(&ended, HiveError::Status { status: 409, message } if message == "ended"),
            "{ended:?}"
        );
        // An unreadable error body still reports the status, with an empty message.
        let boom = hive.end("m1").unwrap_err();
        assert!(
            matches!(&boom, HiveError::Status { status: 500, message } if message.is_empty()),
            "{boom:?}"
        );
        server.join().unwrap();
    }

    #[test]
    fn replies_never_ask_before_the_start_and_decode_the_answer() {
        let (base, server) = serve(vec![(
            200,
            r#"{"next_seq":5,"state":"live","tone":true,"lines":[{"seq":4,"text":"hi"}]}"#,
        )]);
        let hive = Hive::new(&base, "tok").unwrap();
        let r = hive.replies("m1", -9).unwrap();
        assert_eq!((r.next_seq, r.tone, r.lines.len()), (5, true, 1));
        let seen = server.join().unwrap();
        assert!(
            seen[0].starts_with("get /api/companion/meetings/m1/replies?after=0 "),
            "{}",
            seen[0]
        );
    }

    #[test]
    fn an_unreadable_success_body_is_a_network_error() {
        let (base, server) = serve(vec![(200, "not json")]);
        let hive = Hive::new(&base, "tok").unwrap();
        assert!(matches!(hive.info().unwrap_err(), HiveError::Network(_)));
        server.join().unwrap();
    }

    #[test]
    fn an_unreachable_hive_is_a_network_error() {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let hive = Hive::new(&format!("http://127.0.0.1:{port}"), "tok").unwrap();
        assert!(matches!(hive.info().unwrap_err(), HiveError::Network(_)));
    }
}
