//! Gmail HTTP API client — blocking `reqwest` + `serde_json`. No
//! SDK dep. Hits the `gmail.googleapis.com/gmail/v1/users/me/...`
//! endpoints.
//!
//! Auth: bearer `Authorization: Bearer <access_token>` per request.
//! Token plumbing lives in `auth.rs`; this module just consumes it.

use crate::auth::{ClientCreds, Token, ensure_fresh, refresh};
use anyhow::{Context, Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::blocking::Client;
use serde::Deserialize;
use std::time::Duration;

const API_BASE: &str = "https://gmail.googleapis.com/gmail/v1/users/me";

/// Hard cap on items rendered per list tab.
pub const LIST_CAP: usize = 50;

fn build_client() -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("mnml-msg-gmail/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("build HTTP client")
}

/// Parse Google's `{"error": {...}}` envelope. Falls back to the
/// raw status line.
pub fn extract_gmail_error(status: reqwest::StatusCode, body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body)
        && let Some(err) = v.get("error")
    {
        let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
        let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("");
        if code != 0 || !msg.is_empty() {
            return format!("gmail: {code}: {msg}");
        }
    }
    format!(
        "HTTP {status}: {}",
        body.chars().take(200).collect::<String>()
    )
}

// ── Profile ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct Profile {
    #[serde(rename = "emailAddress", default)]
    pub email_address: String,
    #[serde(rename = "messagesTotal", default)]
    pub messages_total: u64,
    #[serde(rename = "threadsTotal", default)]
    pub threads_total: u64,
}

pub fn get_profile(tok: &Token) -> Result<Profile> {
    let client = build_client()?;
    let url = format!("{API_BASE}/profile");
    let resp = client
        .get(&url)
        .bearer_auth(&tok.access_token)
        .send()
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().context("read profile body")?;
    if !status.is_success() {
        return Err(anyhow!(extract_gmail_error(status, &body)));
    }
    serde_json::from_str(&body).context("parse profile JSON")
}

// ── Messages list ─────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct MessageRef {
    pub id: String,
    #[allow(dead_code)]
    #[serde(rename = "threadId", default)]
    pub thread_id: String,
}

#[derive(Debug, Deserialize)]
struct MessagesListResponse {
    #[serde(default)]
    messages: Vec<MessageRef>,
    #[serde(rename = "resultSizeEstimate", default)]
    _result_size_estimate: u64,
}

/// `GET /messages?labelIds=...&maxResults=...` — returns id+threadId
/// only. Caller fetches metadata per id.
pub fn list_messages(
    tok: &Token,
    label_ids: &[&str],
    query: Option<&str>,
    max_results: usize,
) -> Result<Vec<MessageRef>> {
    let client = build_client()?;
    let mut url = format!("{API_BASE}/messages?maxResults={max_results}");
    for label in label_ids {
        url.push_str("&labelIds=");
        url.push_str(&urlencode(label));
    }
    if let Some(q) = query
        && !q.is_empty()
    {
        url.push_str("&q=");
        url.push_str(&urlencode(q));
    }
    let resp = client
        .get(&url)
        .bearer_auth(&tok.access_token)
        .send()
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().context("read messages list body")?;
    if !status.is_success() {
        return Err(anyhow!(extract_gmail_error(status, &body)));
    }
    let parsed: MessagesListResponse =
        serde_json::from_str(&body).context("parse messages list JSON")?;
    Ok(parsed.messages)
}

// ── Messages get (metadata + full) ────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct Message {
    pub id: String,
    #[allow(dead_code)]
    #[serde(rename = "threadId", default)]
    pub thread_id: String,
    #[serde(rename = "labelIds", default)]
    pub label_ids: Vec<String>,
    #[serde(default)]
    pub snippet: String,
    #[serde(default)]
    pub payload: Option<Payload>,
    /// Internal date, ms since epoch (as a string in the API).
    #[serde(rename = "internalDate", default)]
    pub internal_date: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Payload {
    #[serde(default, rename = "mimeType")]
    pub mime_type: String,
    #[serde(default)]
    pub headers: Vec<Header>,
    #[serde(default)]
    pub body: Option<Body>,
    #[serde(default)]
    pub parts: Vec<Payload>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Header {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Body {
    #[allow(dead_code)]
    #[serde(default)]
    pub size: u64,
    /// urlsafe-base64-encoded body. Empty when attachment-only or
    /// when the body is in a `parts[]` sibling.
    #[serde(default)]
    pub data: String,
}

impl Message {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.payload.as_ref().and_then(|p| {
            p.headers
                .iter()
                .find(|h| h.name.eq_ignore_ascii_case(name))
                .map(|h| h.value.as_str())
        })
    }

    pub fn is_unread(&self) -> bool {
        self.label_ids.iter().any(|l| l == "UNREAD")
    }
    pub fn is_starred(&self) -> bool {
        self.label_ids.iter().any(|l| l == "STARRED")
    }
}

/// `GET /messages/{id}?format=metadata&metadataHeaders=...` — small
/// fetch for the list rendering.
pub fn get_message_metadata(tok: &Token, id: &str) -> Result<Message> {
    let client = build_client()?;
    let url = format!(
        "{API_BASE}/messages/{id}?format=metadata\
         &metadataHeaders=From&metadataHeaders=To&metadataHeaders=Subject\
         &metadataHeaders=Date&metadataHeaders=Cc"
    );
    let resp = client
        .get(&url)
        .bearer_auth(&tok.access_token)
        .send()
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().context("read message metadata body")?;
    if !status.is_success() {
        return Err(anyhow!(extract_gmail_error(status, &body)));
    }
    serde_json::from_str(&body).context("parse message metadata JSON")
}

/// `GET /messages/{id}?format=full` — full payload + body parts.
pub fn get_message_full(tok: &Token, id: &str) -> Result<Message> {
    let client = build_client()?;
    let url = format!("{API_BASE}/messages/{id}?format=full");
    let resp = client
        .get(&url)
        .bearer_auth(&tok.access_token)
        .send()
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().context("read message full body")?;
    if !status.is_success() {
        return Err(anyhow!(extract_gmail_error(status, &body)));
    }
    serde_json::from_str(&body).context("parse message full JSON")
}

/// Walk the payload looking for `text/plain`, falling back to
/// `text/html` (stripped). Returns decoded UTF-8 string + a flag
/// indicating whether we landed on HTML.
pub fn extract_body_text(msg: &Message) -> (String, bool) {
    let Some(payload) = msg.payload.as_ref() else {
        return (msg.snippet.clone(), false);
    };
    if let Some((text, is_html)) = walk_for_text(payload) {
        let decoded = decode_urlsafe_base64(&text).unwrap_or_default();
        let s = String::from_utf8_lossy(&decoded).into_owned();
        if is_html {
            (strip_html(&s), true)
        } else {
            (s, false)
        }
    } else {
        (msg.snippet.clone(), false)
    }
}

fn walk_for_text(p: &Payload) -> Option<(String, bool)> {
    // Prefer text/plain, fall back to text/html. Walk parts depth-first.
    fn find(p: &Payload, mime_target: &str) -> Option<String> {
        if p.mime_type == mime_target
            && let Some(b) = p.body.as_ref()
            && !b.data.is_empty()
        {
            return Some(b.data.clone());
        }
        for child in &p.parts {
            if let Some(s) = find(child, mime_target) {
                return Some(s);
            }
        }
        None
    }
    if let Some(plain) = find(p, "text/plain") {
        return Some((plain, false));
    }
    if let Some(html) = find(p, "text/html") {
        return Some((html, true));
    }
    None
}

// ── base64 (urlsafe, no padding) ──────────────────────────────────

/// Decode Gmail's urlsafe-base64 body data. Gmail uses NO-padding by
/// default but tolerates padded too.
pub fn decode_urlsafe_base64(input: &str) -> Result<Vec<u8>> {
    // Trim whitespace + any explicit padding the API may emit.
    let cleaned: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    let stripped = cleaned.trim_end_matches('=');
    URL_SAFE_NO_PAD
        .decode(stripped)
        .map_err(|e| anyhow!("urlsafe-base64 decode: {e}"))
}

/// Encode a raw RFC 822 message for `messages.send` — Gmail requires
/// urlsafe-base64 (no padding).
pub fn encode_urlsafe_base64(input: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(input)
}

// ── HTML strip (no parser, regex-free) ────────────────────────────

/// Strip HTML tags + decode the common entities. Best-effort —
/// matches what e.g. `mnml-msg-teams` would do for a plain-text
/// fallback.
pub fn strip_html(html: &str) -> String {
    // 1. Drop <script> and <style> sections including their content.
    let html = drop_section(html, "<script", "</script>");
    let html = drop_section(&html, "<style", "</style>");
    // 2. Replace block-level tags with newlines before stripping
    //    so paragraphs don't smush together.
    let mut out = String::with_capacity(html.len());
    let mut chars = html.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '<' {
            // Read up to '>' — discard, but if it's a block tag,
            // emit a newline.
            let mut tag = String::new();
            for tc in chars.by_ref() {
                if tc == '>' {
                    break;
                }
                tag.push(tc);
            }
            let lower = tag.to_ascii_lowercase();
            let is_block = ["br", "/p", "/div", "/li", "/tr", "/h1", "/h2", "/h3", "/h4"]
                .iter()
                .any(|t| lower.starts_with(t));
            if is_block {
                out.push('\n');
            }
        } else {
            out.push(c);
        }
    }
    // 3. Entity decode.
    let out = decode_entities(&out);
    // 4. Collapse 3+ newlines → 2 to keep paragraph breaks.
    let mut collapsed = String::with_capacity(out.len());
    let mut blank_run = 0;
    for line in out.lines() {
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run <= 1 {
                collapsed.push('\n');
            }
        } else {
            blank_run = 0;
            collapsed.push_str(line.trim_end());
            collapsed.push('\n');
        }
    }
    collapsed.trim().to_string()
}

fn drop_section(haystack: &str, open: &str, close: &str) -> String {
    let mut out = String::with_capacity(haystack.len());
    let lower = haystack.to_ascii_lowercase();
    let mut i = 0;
    while i < haystack.len() {
        if let Some(start_rel) = lower[i..].find(open) {
            let start = i + start_rel;
            out.push_str(&haystack[i..start]);
            if let Some(end_rel) = lower[start..].find(close) {
                let end = start + end_rel + close.len();
                i = end;
            } else {
                // No close — drop the rest.
                return out;
            }
        } else {
            out.push_str(&haystack[i..]);
            break;
        }
    }
    out
}

fn decode_entities(s: &str) -> String {
    // The handful that matter for email bodies.
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            let mut entity = String::new();
            while let Some(&next) = chars.peek() {
                if next == ';' {
                    chars.next();
                    break;
                }
                if entity.len() > 8 {
                    break;
                }
                entity.push(next);
                chars.next();
            }
            match entity.as_str() {
                "amp" => out.push('&'),
                "lt" => out.push('<'),
                "gt" => out.push('>'),
                "quot" => out.push('"'),
                "apos" => out.push('\''),
                "nbsp" => out.push(' '),
                s if s.starts_with("#x") || s.starts_with("#X") => {
                    if let Ok(n) = u32::from_str_radix(&s[2..], 16)
                        && let Some(ch) = char::from_u32(n)
                    {
                        out.push(ch);
                    }
                }
                s if s.starts_with('#') => {
                    if let Ok(n) = s[1..].parse::<u32>()
                        && let Some(ch) = char::from_u32(n)
                    {
                        out.push(ch);
                    }
                }
                _ => {
                    out.push('&');
                    out.push_str(&entity);
                    if !entity.is_empty() {
                        out.push(';');
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ── Labels ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct Label {
    pub id: String,
    pub name: String,
    /// `system` or `user`.
    #[serde(default, rename = "type")]
    pub label_type: String,
    #[serde(default, rename = "messagesUnread")]
    pub messages_unread: u64,
    #[serde(default, rename = "messagesTotal")]
    pub messages_total: u64,
}

#[derive(Debug, Deserialize)]
struct LabelsListResponse {
    #[serde(default)]
    labels: Vec<Label>,
}

pub fn list_labels(tok: &Token) -> Result<Vec<Label>> {
    let client = build_client()?;
    let url = format!("{API_BASE}/labels");
    let resp = client
        .get(&url)
        .bearer_auth(&tok.access_token)
        .send()
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().context("read labels body")?;
    if !status.is_success() {
        return Err(anyhow!(extract_gmail_error(status, &body)));
    }
    let parsed: LabelsListResponse = serde_json::from_str(&body).context("parse labels JSON")?;
    let mut labels = parsed.labels;
    // Surface unread first, then alpha.
    labels.sort_by(|a, b| {
        b.messages_unread
            .cmp(&a.messages_unread)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(labels)
}

/// Label-detail fetch (used after listing — `messages_unread` is
/// already present on list, so this is rarely needed for v0.1).
#[allow(dead_code)]
pub fn get_label(tok: &Token, id: &str) -> Result<Label> {
    let client = build_client()?;
    let url = format!("{API_BASE}/labels/{id}");
    let resp = client
        .get(&url)
        .bearer_auth(&tok.access_token)
        .send()
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().context("read label body")?;
    if !status.is_success() {
        return Err(anyhow!(extract_gmail_error(status, &body)));
    }
    serde_json::from_str(&body).context("parse label JSON")
}

// ── Modify (archive, star) ────────────────────────────────────────

/// `POST /messages/{id}/modify` — add/remove label IDs.
pub fn modify_labels(tok: &Token, id: &str, add: &[&str], remove: &[&str]) -> Result<()> {
    let client = build_client()?;
    let url = format!("{API_BASE}/messages/{id}/modify");
    let payload = serde_json::json!({
        "addLabelIds": add,
        "removeLabelIds": remove,
    });
    let resp = client
        .post(&url)
        .bearer_auth(&tok.access_token)
        .header("Content-Type", "application/json")
        .body(payload.to_string())
        .send()
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    let body = resp.text().context("read modify body")?;
    if !status.is_success() {
        return Err(anyhow!(extract_gmail_error(status, &body)));
    }
    Ok(())
}

// ── Send ──────────────────────────────────────────────────────────

/// Build a minimal RFC 822 message + send via
/// `POST /messages/send` with `{"raw": "<urlsafe-base64>"}`.
pub fn send_message(tok: &Token, to: &str, subject: &str, body: &str) -> Result<()> {
    let rfc822 = build_rfc822(to, subject, body);
    let raw = encode_urlsafe_base64(rfc822.as_bytes());
    let client = build_client()?;
    let url = format!("{API_BASE}/messages/send");
    let payload = serde_json::json!({ "raw": raw });
    let resp = client
        .post(&url)
        .bearer_auth(&tok.access_token)
        .header("Content-Type", "application/json")
        .body(payload.to_string())
        .send()
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    let body = resp.text().context("read send body")?;
    if !status.is_success() {
        return Err(anyhow!(extract_gmail_error(status, &body)));
    }
    Ok(())
}

/// Build a minimal RFC 822 message. `From` is omitted — Gmail
/// stamps it server-side from the authenticated user.
pub fn build_rfc822(to: &str, subject: &str, body: &str) -> String {
    let mut s = String::new();
    s.push_str("To: ");
    s.push_str(to);
    s.push_str("\r\n");
    s.push_str("Subject: ");
    s.push_str(subject);
    s.push_str("\r\n");
    s.push_str("Content-Type: text/plain; charset=\"UTF-8\"\r\n");
    s.push_str("MIME-Version: 1.0\r\n");
    s.push_str("\r\n");
    s.push_str(body);
    s
}

// ── Token plumbing helpers ────────────────────────────────────────

/// Get a fresh token, refreshing once if Gmail returned 401.
/// Convenience for the App layer.
pub fn with_fresh_token<F, T>(creds: &ClientCreds, tok: &mut Token, mut f: F) -> Result<T>
where
    F: FnMut(&Token) -> Result<T>,
{
    match f(tok) {
        Ok(v) => Ok(v),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("gmail: 401") || msg.contains("UNAUTHENTICATED") {
                let fresh = refresh(creds, tok)?;
                *tok = fresh;
                f(tok)
            } else {
                Err(e)
            }
        }
    }
}

/// One-shot ensure — re-exported for `--check`.
#[allow(dead_code)]
pub fn touch(creds: &ClientCreds) -> Result<Token> {
    ensure_fresh(creds)
}

// ── URL helper ────────────────────────────────────────────────────

pub fn message_web_url(id: &str) -> String {
    format!("https://mail.google.com/mail/u/0/#inbox/{id}")
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_message_list_json() {
        let json = r#"{
            "messages":[
                {"id":"abc","threadId":"thr1"},
                {"id":"def","threadId":"thr2"}
            ],
            "resultSizeEstimate": 2
        }"#;
        let parsed: MessagesListResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.messages.len(), 2);
        assert_eq!(parsed.messages[0].id, "abc");
    }

    #[test]
    fn parses_message_metadata_json() {
        let json = r#"{
            "id":"abc",
            "threadId":"thr1",
            "labelIds":["INBOX","UNREAD"],
            "snippet":"hi there",
            "internalDate":"1700000000000",
            "payload":{
                "mimeType":"text/plain",
                "headers":[
                    {"name":"From","value":"alice@example.com"},
                    {"name":"Subject","value":"hello"},
                    {"name":"Date","value":"Thu, 1 Jan 2026 12:00:00 +0000"}
                ]
            }
        }"#;
        let msg: Message = serde_json::from_str(json).unwrap();
        assert_eq!(msg.id, "abc");
        assert!(msg.is_unread());
        assert!(!msg.is_starred());
        assert_eq!(msg.header("From"), Some("alice@example.com"));
        assert_eq!(msg.header("subject"), Some("hello"));
    }

    #[test]
    fn parses_labels_json() {
        let json = r#"{
            "labels":[
                {"id":"INBOX","name":"INBOX","type":"system","messagesUnread":5,"messagesTotal":100},
                {"id":"Label_42","name":"Receipts","type":"user","messagesUnread":0,"messagesTotal":12}
            ]
        }"#;
        let parsed: LabelsListResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.labels.len(), 2);
        assert_eq!(parsed.labels[0].name, "INBOX");
    }

    #[test]
    fn parses_profile_json() {
        let json = r#"{"emailAddress":"me@example.com","messagesTotal":1234,"threadsTotal":567,"historyId":"42"}"#;
        let p: Profile = serde_json::from_str(json).unwrap();
        assert_eq!(p.email_address, "me@example.com");
        assert_eq!(p.messages_total, 1234);
    }

    #[test]
    fn parses_full_message_with_parts() {
        let json = r#"{
            "id":"abc",
            "threadId":"thr",
            "labelIds":["INBOX"],
            "snippet":"hi",
            "internalDate":"1700000000000",
            "payload":{
                "mimeType":"multipart/alternative",
                "headers":[{"name":"Subject","value":"yo"}],
                "parts":[
                    {"mimeType":"text/plain","body":{"size":5,"data":"aGVsbG8"}},
                    {"mimeType":"text/html","body":{"size":12,"data":"PGI-aGk8L2I-"}}
                ]
            }
        }"#;
        let msg: Message = serde_json::from_str(json).unwrap();
        let (body, is_html) = extract_body_text(&msg);
        assert!(!is_html, "should prefer text/plain over text/html");
        assert_eq!(body, "hello");
    }

    #[test]
    fn urlsafe_base64_roundtrip() {
        let original = b"hello world\n<b>bold</b> & special chars: \x00\xff";
        let encoded = encode_urlsafe_base64(original);
        // urlsafe alphabet: no + or /
        assert!(!encoded.contains('+'));
        assert!(!encoded.contains('/'));
        assert!(!encoded.contains('='));
        let decoded = decode_urlsafe_base64(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn urlsafe_base64_decodes_with_padding() {
        // Some Gmail payloads carry padding; ours strips it.
        let padded = "aGVsbG8="; // "hello"
        let decoded = decode_urlsafe_base64(padded).unwrap();
        assert_eq!(decoded, b"hello");
    }

    #[test]
    fn html_strip_removes_tags_and_keeps_text() {
        let html = "<p>Hello <b>world</b></p><p>Line two</p>";
        let stripped = strip_html(html);
        assert!(stripped.contains("Hello"));
        assert!(stripped.contains("world"));
        assert!(stripped.contains("Line two"));
        assert!(!stripped.contains('<'));
        assert!(!stripped.contains('>'));
    }

    #[test]
    fn html_strip_decodes_entities() {
        let html = "A &amp; B &lt;foo&gt; &#65;&#x42;";
        let stripped = strip_html(html);
        assert!(stripped.contains("A & B"));
        assert!(stripped.contains("<foo>"));
        assert!(stripped.contains("AB"));
    }

    #[test]
    fn html_strip_drops_script_and_style() {
        let html = "before<script>evil()</script>middle<style>body{color:red}</style>after";
        let stripped = strip_html(html);
        assert!(!stripped.contains("evil"));
        assert!(!stripped.contains("color:red"));
        assert!(stripped.contains("before"));
        assert!(stripped.contains("after"));
    }

    #[test]
    fn gmail_error_envelope_extracted() {
        let body = r#"{"error":{"code":403,"message":"Forbidden","errors":[]}}"#;
        let msg = extract_gmail_error(reqwest::StatusCode::FORBIDDEN, body);
        assert_eq!(msg, "gmail: 403: Forbidden");
    }

    #[test]
    fn builds_rfc822_with_headers() {
        let m = build_rfc822("alice@example.com", "hello", "body text\nline two");
        assert!(m.contains("To: alice@example.com\r\n"));
        assert!(m.contains("Subject: hello\r\n"));
        assert!(m.contains("Content-Type: text/plain"));
        assert!(m.contains("MIME-Version: 1.0\r\n"));
        // Header / body separator.
        assert!(m.contains("\r\n\r\nbody text"));
    }

    #[test]
    fn message_web_url_includes_id() {
        let url = message_web_url("18f8a8c2c1234567");
        assert!(url.starts_with("https://mail.google.com/"));
        assert!(url.ends_with("18f8a8c2c1234567"));
    }
}
