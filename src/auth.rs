//! Gmail OAuth 2.0 — loopback-redirect flow (RFC 8252) + token
//! persistence + refresh.
//!
//! Google deprecated the device-code grant for installed apps, so we
//! use the recommended "installed app on a machine with a browser"
//! flow: bind a TCP listener on `localhost:0`, open the browser to
//! `accounts.google.com/o/oauth2/v2/auth?...&redirect_uri=http://localhost:<port>`,
//! accept the redirect, exchange the auth code for an access + refresh
//! token at `oauth2.googleapis.com/token`.
//!
//! The client_id + client_secret come from `GMAIL_CLIENT_ID` /
//! `GMAIL_CLIENT_SECRET`. Google does NOT allow shared client IDs
//! for Gmail scopes — every user must create their own Google Cloud
//! project. See README.
//!
//! Persisted token lives at `~/.config/mnml-msg-gmail/token.json`,
//! mode 0600 on Unix.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

pub const SCOPES: &str =
    "https://www.googleapis.com/auth/gmail.modify https://www.googleapis.com/auth/gmail.send";

/// OAuth client identity read from env. Required.
#[derive(Debug, Clone)]
pub struct ClientCreds {
    pub client_id: String,
    pub client_secret: String,
}

impl ClientCreds {
    pub fn from_env() -> Result<Self> {
        let id = std::env::var("GMAIL_CLIENT_ID")
            .ok()
            .filter(|s| !s.is_empty());
        let secret = std::env::var("GMAIL_CLIENT_SECRET")
            .ok()
            .filter(|s| !s.is_empty());
        match (id, secret) {
            (Some(client_id), Some(client_secret)) => Ok(Self {
                client_id,
                client_secret,
            }),
            (None, _) => Err(anyhow!(
                "GMAIL_CLIENT_ID not set — create a Google Cloud project, enable the Gmail API, configure the OAuth consent screen, create OAuth credentials of type \"Desktop app\", then export the client ID. See README."
            )),
            (_, None) => Err(anyhow!(
                "GMAIL_CLIENT_SECRET not set — paired with GMAIL_CLIENT_ID from the same OAuth credentials. See README."
            )),
        }
    }
}

/// Persisted token. `access_token` rotates ~hourly; `refresh_token`
/// is long-lived (indefinitely in Production, ~7 days in Testing
/// mode).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds when `access_token` expires.
    pub expires_at: u64,
    /// `Bearer` typically.
    #[serde(default)]
    pub token_type: String,
    /// Space-separated scopes granted.
    #[serde(default)]
    pub scope: String,
}

impl Token {
    /// True if expired or expiring in the next 60s.
    pub fn is_expired(&self) -> bool {
        let now = now_unix();
        self.expires_at <= now + 60
    }
}

pub fn token_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("mnml-msg-gmail")
        .join("token.json")
}

pub fn load_token() -> Result<Option<Token>> {
    let p = token_path();
    if !p.exists() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(&p).with_context(|| format!("read token at {}", p.display()))?;
    let tok: Token =
        serde_json::from_str(&text).with_context(|| "parse token.json (re-run `auth`?)")?;
    Ok(Some(tok))
}

pub fn save_token(tok: &Token) -> Result<()> {
    let p = token_path();
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(tok)?;
    std::fs::write(&p, json).with_context(|| format!("write token to {}", p.display()))?;
    set_mode_0600(&p)?;
    Ok(())
}

pub fn delete_token() -> Result<bool> {
    let p = token_path();
    if !p.exists() {
        return Ok(false);
    }
    std::fs::remove_file(&p).with_context(|| format!("remove {}", p.display()))?;
    Ok(true)
}

#[cfg(unix)]
fn set_mode_0600(p: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(p)?.permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(p, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_mode_0600(_p: &std::path::Path) -> Result<()> {
    Ok(())
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ── Token-exchange response shapes ────────────────────────────────

#[derive(Debug, Deserialize)]
struct TokenExchangeResponse {
    access_token: String,
    /// Only present on the initial code → token exchange. The refresh
    /// endpoint does NOT echo it back.
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: u64,
    #[serde(default)]
    token_type: String,
    #[serde(default)]
    scope: String,
}

#[derive(Debug, Deserialize)]
struct GoogleErrorResponse {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

fn extract_google_error(status: reqwest::StatusCode, body: &str) -> String {
    if let Ok(e) = serde_json::from_str::<GoogleErrorResponse>(body) {
        let kind = e.error.unwrap_or_else(|| "unknown".into());
        let desc = e.error_description.unwrap_or_default();
        if !desc.is_empty() {
            return format!("google oauth: {kind}: {desc}");
        }
        return format!("google oauth: {kind}");
    }
    format!(
        "HTTP {status}: {}",
        body.chars().take(200).collect::<String>()
    )
}

// ── Loopback OAuth ────────────────────────────────────────────────

/// Run the full interactive loopback flow. Blocks on the browser
/// round-trip. Persists the token + returns it.
pub fn interactive_login(creds: &ClientCreds) -> Result<Token> {
    // Bind the loopback listener first — the redirect_uri has to
    // match exactly when we exchange the code.
    let listener =
        TcpListener::bind("127.0.0.1:0").context("bind 127.0.0.1:0 for OAuth redirect listener")?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://localhost:{port}");

    let auth_url = build_auth_url(&creds.client_id, &redirect_uri);

    println!("Opening browser for Google sign-in…");
    println!();
    println!("If the browser doesn't open, paste this URL manually:");
    println!("{auth_url}");
    println!();
    let _ = webbrowser::open(&auth_url);

    println!("Waiting for redirect on {redirect_uri} …");

    // Accept exactly one connection — the redirect carries `?code=…`.
    listener
        .set_nonblocking(false)
        .context("set listener blocking")?;
    let (mut stream, _peer) = listener.accept().context("accept redirect")?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .context("set stream read timeout")?;
    let (path, _) = read_http_request(&mut stream)?;
    let code = parse_code_from_path(&path)?;
    let _ = write_redirect_response(&mut stream);

    println!("Got auth code; exchanging for tokens…");
    let tok = exchange_code(creds, &code, &redirect_uri)?;
    save_token(&tok)?;
    println!("Saved token to {}", token_path().display());
    Ok(tok)
}

fn build_auth_url(client_id: &str, redirect_uri: &str) -> String {
    format!(
        "{AUTH_URL}?client_id={cid}&redirect_uri={ru}&response_type=code&scope={sc}&access_type=offline&prompt=consent",
        cid = urlencode(client_id),
        ru = urlencode(redirect_uri),
        sc = urlencode(SCOPES),
    )
}

/// Read the request line of an HTTP request (`GET /?code=… HTTP/1.1`)
/// and the headers. Returns the path (`/?code=…`) and the full raw
/// request. We don't care about the body.
fn read_http_request(stream: &mut TcpStream) -> Result<(String, String)> {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .context("read request line")?;
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        return Err(anyhow!("malformed HTTP request line: {request_line:?}"));
    }
    let path = parts[1].to_string();
    // Drain headers so the peer's write completes cleanly.
    let mut headers = String::new();
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).unwrap_or(0);
        if n == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        headers.push_str(&line);
    }
    Ok((path, headers))
}

fn parse_code_from_path(path: &str) -> Result<String> {
    // path looks like `/?code=4/0Adeu5BV…&scope=…`
    let q = path
        .find('?')
        .ok_or_else(|| anyhow!("no query string in redirect path: {path:?}"))?;
    let query = &path[q + 1..];
    let mut code = None;
    let mut err = None;
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            match k {
                "code" => code = Some(urldecode(v)),
                "error" => err = Some(urldecode(v)),
                _ => {}
            }
        }
    }
    if let Some(e) = err {
        return Err(anyhow!("oauth redirect carried error: {e}"));
    }
    code.ok_or_else(|| anyhow!("oauth redirect had no `code` param: {path:?}"))
}

fn write_redirect_response(stream: &mut TcpStream) -> Result<()> {
    let body = "<!doctype html><html><head><meta charset=\"utf-8\"><title>mnml-msg-gmail</title>\
                <style>body{font-family:system-ui,sans-serif;display:flex;align-items:center;\
                justify-content:center;height:100vh;margin:0;background:#0e0f12;color:#e8e8e8}\
                .box{padding:32px 48px;border-radius:8px;background:#1a1c20;text-align:center}\
                h1{margin:0 0 8px;font-size:18px;font-weight:500}p{margin:0;color:#888;\
                font-size:14px}</style></head><body><div class=\"box\"><h1>You're signed in.</h1>\
                <p>You can close this tab and return to the terminal.</p></div></body></html>";
    let payload = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(payload.as_bytes())?;
    let _ = stream.flush();
    Ok(())
}

/// Exchange the redirect's `?code=…` for access + refresh tokens.
fn exchange_code(creds: &ClientCreds, code: &str, redirect_uri: &str) -> Result<Token> {
    let client = build_client()?;
    let resp = client
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("client_id", &creds.client_id),
            ("client_secret", &creds.client_secret),
            ("redirect_uri", redirect_uri),
        ])
        .send()
        .with_context(|| format!("POST {TOKEN_URL}"))?;
    let status = resp.status();
    let body = resp.text().context("read token response body")?;
    if !status.is_success() {
        return Err(anyhow!(extract_google_error(status, &body)));
    }
    let parsed: TokenExchangeResponse =
        serde_json::from_str(&body).context("parse token exchange response")?;
    let refresh = parsed.refresh_token.ok_or_else(|| {
        anyhow!(
            "google returned no refresh_token — this usually means a previous consent is cached. Revoke at https://myaccount.google.com/permissions and re-run."
        )
    })?;
    Ok(Token {
        access_token: parsed.access_token,
        refresh_token: refresh,
        expires_at: now_unix() + parsed.expires_in,
        token_type: parsed.token_type,
        scope: parsed.scope,
    })
}

/// Use the refresh_token to get a new access_token. Refresh tokens
/// are NOT rotated by Google for installed-app credentials, so we
/// preserve the existing one.
pub fn refresh(creds: &ClientCreds, tok: &Token) -> Result<Token> {
    let client = build_client()?;
    let resp = client
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", tok.refresh_token.as_str()),
            ("client_id", &creds.client_id),
            ("client_secret", &creds.client_secret),
        ])
        .send()
        .with_context(|| format!("POST {TOKEN_URL} (refresh)"))?;
    let status = resp.status();
    let body = resp.text().context("read refresh response body")?;
    if !status.is_success() {
        return Err(anyhow!(extract_google_error(status, &body)));
    }
    let parsed: TokenExchangeResponse =
        serde_json::from_str(&body).context("parse refresh response")?;
    let new = Token {
        access_token: parsed.access_token,
        // Google does not echo refresh_token on refresh — keep ours.
        refresh_token: tok.refresh_token.clone(),
        expires_at: now_unix() + parsed.expires_in,
        token_type: if parsed.token_type.is_empty() {
            tok.token_type.clone()
        } else {
            parsed.token_type
        },
        scope: if parsed.scope.is_empty() {
            tok.scope.clone()
        } else {
            parsed.scope
        },
    };
    save_token(&new)?;
    Ok(new)
}

/// Ensure we have a fresh access token. Loads from disk + refreshes
/// if needed. Returns the in-memory token.
pub fn ensure_fresh(creds: &ClientCreds) -> Result<Token> {
    let tok = load_token()?
        .ok_or_else(|| anyhow!("no token on disk — run `mnml-msg-gmail auth` first"))?;
    if tok.is_expired() {
        refresh(creds, &tok)
    } else {
        Ok(tok)
    }
}

fn build_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("mnml-msg-gmail/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("build HTTP client")
}

// ── tiny URL helpers (no extra dep) ───────────────────────────────

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("00");
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_expired_when_past_window() {
        let t = Token {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: now_unix() - 10,
            token_type: "Bearer".into(),
            scope: SCOPES.into(),
        };
        assert!(t.is_expired());
    }

    #[test]
    fn token_fresh_with_5min_left() {
        let t = Token {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: now_unix() + 300,
            token_type: "Bearer".into(),
            scope: SCOPES.into(),
        };
        assert!(!t.is_expired());
    }

    #[test]
    fn token_expired_inside_60s_skew_window() {
        let t = Token {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: now_unix() + 30,
            token_type: "Bearer".into(),
            scope: SCOPES.into(),
        };
        // 30s left should be considered expired (skew is 60s).
        assert!(t.is_expired());
    }

    #[test]
    fn parses_code_from_redirect_path() {
        let code = parse_code_from_path("/?code=4%2F0Adeu5BV-test&scope=foo").unwrap();
        assert_eq!(code, "4/0Adeu5BV-test");
    }

    #[test]
    fn rejects_redirect_carrying_error() {
        let err = parse_code_from_path("/?error=access_denied").unwrap_err();
        assert!(err.to_string().contains("access_denied"));
    }

    #[test]
    fn rejects_redirect_without_code() {
        assert!(parse_code_from_path("/?state=xyz").is_err());
    }

    #[test]
    fn build_auth_url_has_required_params() {
        let url = build_auth_url("123.apps.googleusercontent.com", "http://localhost:51234");
        assert!(url.contains("client_id=123.apps.googleusercontent.com"));
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A51234"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("access_type=offline"));
        assert!(url.contains("prompt=consent"));
        // Scopes encoded — gmail.modify + gmail.send.
        assert!(url.contains("gmail.modify"));
        assert!(url.contains("gmail.send"));
    }

    #[test]
    fn google_error_envelope_extracted() {
        let body =
            r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#;
        let msg = extract_google_error(reqwest::StatusCode::BAD_REQUEST, body);
        assert!(msg.contains("invalid_grant"));
        assert!(msg.contains("revoked"));
    }

    #[test]
    fn token_roundtrip_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("token.json");
        let tok = Token {
            access_token: "ya29.test".into(),
            refresh_token: "1//refresh".into(),
            expires_at: 1_700_000_000,
            token_type: "Bearer".into(),
            scope: SCOPES.into(),
        };
        let json = serde_json::to_string_pretty(&tok).unwrap();
        std::fs::write(&p, json).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        let parsed: Token = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed.access_token, "ya29.test");
        assert_eq!(parsed.refresh_token, "1//refresh");
        assert_eq!(parsed.expires_at, 1_700_000_000);
    }
}
