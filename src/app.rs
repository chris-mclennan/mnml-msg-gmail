//! App state — per-tab lists + selection + the compose overlay +
//! the search input.

use crate::auth::{ClientCreds, Token};
use crate::config::{Config, Tab};
use crate::gmail::{self, Label, Message};
use anyhow::Result;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct TabSpec {
    pub kind: String,
    /// `search`-only: initial query.
    pub query: Option<String>,
    /// `labels`-only: when an entry is opened with Enter, the tab
    /// view switches to a transient label-filtered messages list
    /// scoped to this label id.
    pub label_filter: Option<String>,
}

impl TabSpec {
    pub fn resolve(t: &Tab) -> Result<Self> {
        match t.kind.as_str() {
            "inbox" | "sent" | "starred" | "labels" | "search" => Ok(Self {
                kind: t.kind.clone(),
                query: t.query.clone(),
                label_filter: None,
            }),
            other => anyhow::bail!("tab `{}`: unknown kind {other:?}", t.name),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Item {
    Message(Message),
    Label(Label),
}

impl Item {
    pub fn primary_label(&self) -> String {
        match self {
            Item::Message(m) => m
                .header("From")
                .map(parse_from_display)
                .unwrap_or_else(|| "(no sender)".to_string()),
            Item::Label(l) => l.name.clone(),
        }
    }
    pub fn secondary_label(&self) -> String {
        match self {
            Item::Message(m) => {
                let subject = m.header("Subject").unwrap_or("(no subject)");
                let subject_trunc = truncate(subject, 60);
                let age = m
                    .header("Date")
                    .map(short_date)
                    .unwrap_or_else(|| internal_date_age(&m.internal_date));
                format!("{subject_trunc} · {age}")
            }
            Item::Label(l) => {
                let kind = if l.label_type == "system" {
                    "system"
                } else {
                    "user"
                };
                if l.messages_unread > 0 {
                    format!(
                        "{} unread · {kind} · {} total",
                        l.messages_unread, l.messages_total
                    )
                } else {
                    format!("{kind} · {} total", l.messages_total)
                }
            }
        }
    }
}

/// `From: "Alice" <alice@example.com>` → `Alice`. Falls back to the
/// raw header.
fn parse_from_display(from: &str) -> String {
    let from = from.trim();
    if let Some(angle) = from.find('<') {
        let name = from[..angle].trim().trim_matches('"').trim();
        if !name.is_empty() {
            return name.to_string();
        }
        // No name part — `<addr@host>`. Return without angles.
        return from
            .trim_start_matches('<')
            .trim_end_matches('>')
            .to_string();
    }
    from.to_string()
}

/// Very loose age display from `internalDate` (ms since epoch as a
/// string).
fn internal_date_age(internal_date: &str) -> String {
    let Ok(ms) = internal_date.parse::<u64>() else {
        return "—".to_string();
    };
    let secs = ms / 1000;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(secs);
    let dt = now.saturating_sub(secs);
    age_string(dt)
}

fn age_string(seconds: u64) -> String {
    if seconds < 60 {
        return format!("{seconds}s");
    }
    if seconds < 3600 {
        return format!("{}m", seconds / 60);
    }
    if seconds < 86_400 {
        return format!("{}h", seconds / 3600);
    }
    if seconds < 86_400 * 14 {
        return format!("{}d", seconds / 86_400);
    }
    format!("{}w", seconds / (86_400 * 7))
}

/// `Thu, 1 Jan 2026 12:34:56 +0000` → `1 Jan`. Best-effort —
/// emails vary in date format wildly.
fn short_date(d: &str) -> String {
    // Strip the leading day-of-week if present.
    let s = d.split_once(", ").map(|(_, rest)| rest).unwrap_or(d).trim();
    // Take the first three whitespace-separated tokens →
    // `day month year` → drop year so we get `day month`.
    let toks: Vec<&str> = s.split_whitespace().collect();
    if toks.len() >= 2 {
        return format!("{} {}", toks[0], toks[1]);
    }
    s.to_string()
}

pub struct ItemsTab {
    pub items: Vec<Item>,
    pub selected: usize,
    pub last_loaded: Option<Instant>,
    pub last_error: Option<String>,
    pub loading: bool,
}

impl ItemsTab {
    fn empty() -> Self {
        ItemsTab {
            items: Vec::new(),
            selected: 0,
            last_loaded: None,
            last_error: None,
            loading: false,
        }
    }
}

pub struct TabState {
    pub name: String,
    pub spec: TabSpec,
    pub data: ItemsTab,
    /// `search`-only: editing the query.
    pub search_input: String,
    /// True while the user is actively typing in the search field.
    pub search_editing: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeField {
    To,
    Subject,
    Body,
}

pub struct Compose {
    pub to: String,
    pub subject: String,
    pub body: String,
    pub field: ComposeField,
    pub sending: bool,
}

impl Compose {
    pub fn new() -> Self {
        Self {
            to: String::new(),
            subject: String::new(),
            body: String::new(),
            field: ComposeField::To,
            sending: false,
        }
    }
    pub fn focused_mut(&mut self) -> &mut String {
        match self.field {
            ComposeField::To => &mut self.to,
            ComposeField::Subject => &mut self.subject,
            ComposeField::Body => &mut self.body,
        }
    }
    pub fn next_field(&mut self) {
        self.field = match self.field {
            ComposeField::To => ComposeField::Subject,
            ComposeField::Subject => ComposeField::Body,
            ComposeField::Body => ComposeField::Body,
        };
    }
    pub fn prev_field(&mut self) {
        self.field = match self.field {
            ComposeField::To => ComposeField::To,
            ComposeField::Subject => ComposeField::To,
            ComposeField::Body => ComposeField::Subject,
        };
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    Archive,
}

pub struct App {
    pub cfg: Config,
    pub creds: ClientCreds,
    pub token: Token,
    pub tabs: Vec<TabState>,
    pub active_tab: usize,
    pub status: String,
    /// Cached full body of the focused message (decoded plain text /
    /// HTML-stripped). Keyed by message id so we don't re-fetch on
    /// every render.
    pub detail_cache: Option<(String, String)>,
    /// Compose overlay state. None = not composing.
    pub compose: Option<Compose>,
    /// Pending y/n confirmation.
    pub confirm: Option<Confirm>,
}

impl App {
    pub fn new(cfg: Config, creds: ClientCreds, token: Token) -> Result<Self> {
        let mut tabs = Vec::with_capacity(cfg.tabs.len());
        for t in &cfg.tabs {
            let spec = TabSpec::resolve(t)?;
            let initial_search = if spec.kind == "search" {
                spec.query.clone().unwrap_or_default()
            } else {
                String::new()
            };
            tabs.push(TabState {
                name: t.name.clone(),
                data: ItemsTab::empty(),
                spec,
                search_input: initial_search,
                search_editing: false,
            });
        }
        let mut app = App {
            cfg,
            creds,
            token,
            tabs,
            active_tab: 0,
            status: String::new(),
            detail_cache: None,
            compose: None,
            confirm: None,
        };
        app.refresh_active();
        Ok(app)
    }

    pub fn active(&self) -> &TabState {
        &self.tabs[self.active_tab]
    }
    pub fn active_mut(&mut self) -> &mut TabState {
        &mut self.tabs[self.active_tab]
    }

    pub fn switch_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.active_tab = idx;
            self.detail_cache = None;
            if self.tabs[idx].data.items.is_empty() && self.tabs[idx].data.last_error.is_none() {
                self.refresh_active();
            }
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        let tab = self.active_mut();
        if tab.data.items.is_empty() {
            return;
        }
        let n = tab.data.items.len() as isize;
        let cur = tab.data.selected as isize;
        let next = (cur + delta).clamp(0, n - 1);
        tab.data.selected = next as usize;
        self.detail_cache = None;
    }

    pub fn refresh_active(&mut self) {
        let idx = self.active_tab;
        let spec = self.tabs[idx].spec.clone();
        let name = self.tabs[idx].name.clone();
        self.status = format!("loading {name}…");
        self.tabs[idx].data.loading = true;

        let result = self.load_for_spec(&spec);

        let t = &mut self.tabs[idx];
        t.data.loading = false;
        match result {
            Ok(items) => {
                let count = items.len();
                t.data.items = items;
                t.data.selected = t.data.selected.min(count.saturating_sub(1));
                t.data.last_loaded = Some(Instant::now());
                t.data.last_error = None;
                let kind_label = match spec.kind.as_str() {
                    "inbox" | "sent" | "starred" | "search" => "messages",
                    "labels" => "labels",
                    _ => "items",
                };
                self.status = format!("{name}: {count} {kind_label}");
            }
            Err(e) => {
                t.data.last_error = Some(e.to_string());
                self.status = format!("error: {e}");
            }
        }
        self.detail_cache = None;
    }

    fn load_for_spec(&mut self, spec: &TabSpec) -> Result<Vec<Item>> {
        match spec.kind.as_str() {
            "inbox" => self.load_messages(&["INBOX"], None),
            "sent" => self.load_messages(&["SENT"], None),
            "starred" => self.load_messages(&["STARRED"], None),
            "search" => {
                let q = self.tabs[self.active_tab].search_input.clone();
                if q.trim().is_empty() {
                    Ok(Vec::new())
                } else {
                    self.load_messages(&[], Some(&q))
                }
            }
            "labels" => {
                if let Some(label_id) = spec.label_filter.as_deref() {
                    self.load_messages(&[label_id], None)
                } else {
                    let creds = self.creds.clone();
                    let labels = gmail::with_fresh_token(&creds, &mut self.token, |tok| {
                        gmail::list_labels(tok)
                    })?;
                    Ok(labels.into_iter().map(Item::Label).collect())
                }
            }
            _ => unreachable!("validated in TabSpec::resolve"),
        }
    }

    fn load_messages(&mut self, label_ids: &[&str], query: Option<&str>) -> Result<Vec<Item>> {
        let creds = self.creds.clone();
        let refs = gmail::with_fresh_token(&creds, &mut self.token, |tok| {
            gmail::list_messages(tok, label_ids, query, gmail::LIST_CAP)
        })?;
        let mut out = Vec::with_capacity(refs.len());
        for r in &refs {
            match gmail::with_fresh_token(&creds, &mut self.token, |tok| {
                gmail::get_message_metadata(tok, &r.id)
            }) {
                Ok(m) => out.push(Item::Message(m)),
                Err(e) => {
                    // Surface the first error but continue — partial
                    // lists are still useful.
                    self.status = format!("warn: {e}");
                }
            }
        }
        Ok(out)
    }

    /// Tick — runs each frame. Refreshes the active tab whenever
    /// `refresh_interval_secs` has elapsed since the last load.
    pub fn tick(&mut self) -> bool {
        if self.compose.is_some() || self.confirm.is_some() {
            return false;
        }
        let idx = self.active_tab;
        let interval = self.cfg.refresh_interval_secs;
        if interval == 0 {
            return false;
        }
        // Don't auto-refresh search tabs with no query — they're
        // intentionally empty until the user types.
        if self.tabs[idx].spec.kind == "search" && self.tabs[idx].search_input.trim().is_empty() {
            return false;
        }
        let stale = match self.tabs[idx].data.last_loaded {
            Some(t) => t.elapsed().as_secs() >= interval,
            None => true,
        };
        if stale && !self.tabs[idx].data.loading {
            self.refresh_active();
            true
        } else {
            false
        }
    }

    pub fn focused_item(&self) -> Option<&Item> {
        let t = self.active();
        t.data.items.get(t.data.selected)
    }

    /// Ensure `self.detail_cache` matches the focused item. Lazily
    /// fetches the full body on demand.
    pub fn ensure_detail(&mut self) {
        let Some(item) = self.focused_item() else {
            self.detail_cache = None;
            return;
        };
        let id = match item {
            Item::Message(m) => m.id.clone(),
            Item::Label(_) => {
                self.detail_cache = None;
                return;
            }
        };
        if self.detail_cache.as_ref().map(|(k, _)| k.as_str()) == Some(id.as_str()) {
            return;
        }
        let creds = self.creds.clone();
        match gmail::with_fresh_token(&creds, &mut self.token, |tok| {
            gmail::get_message_full(tok, &id)
        }) {
            Ok(full) => {
                let (text, _is_html) = gmail::extract_body_text(&full);
                self.detail_cache = Some((id, text));
            }
            Err(e) => {
                self.status = format!("detail fetch failed: {e}");
                self.detail_cache = Some((id, format!("(failed to load body: {e})")));
            }
        }
    }

    /// `Enter` — open the focused message or scope into a label.
    pub fn open_focused(&mut self) {
        let Some(item) = self.focused_item() else {
            self.status = "no item under cursor".into();
            return;
        };
        match item {
            Item::Message(_) => {
                self.ensure_detail();
            }
            Item::Label(l) => {
                let id = l.id.clone();
                let name = l.name.clone();
                // Mutate the active tab's spec so the next refresh
                // is label-filtered.
                let idx = self.active_tab;
                self.tabs[idx].spec.label_filter = Some(id);
                self.tabs[idx].spec.kind = "inbox".to_string(); // load_messages branch
                self.tabs[idx].name = format!("label:{name}");
                self.refresh_active();
            }
        }
    }

    /// `o` — open the focused message in the Gmail web UI.
    pub fn open_console(&mut self) {
        let url = match self.focused_item() {
            Some(Item::Message(m)) => gmail::message_web_url(&m.id),
            Some(Item::Label(_)) | None => {
                self.status = "open: focus a message first".into();
                return;
            }
        };
        match webbrowser::open(&url) {
            Ok(()) => self.status = format!("opened {url}"),
            Err(e) => self.status = format!("open failed: {e}"),
        }
    }

    /// `y` — yank the Gmail URL for the focused message.
    pub fn yank(&mut self) {
        let payload = match self.focused_item() {
            Some(Item::Message(m)) => gmail::message_web_url(&m.id),
            Some(Item::Label(_)) | None => {
                self.status = "yank: focus a message first".into();
                return;
            }
        };
        let len = payload.chars().count();
        match crate::clipboard::copy(&payload) {
            Ok(()) => self.status = format!("copied URL ({len} chars)"),
            Err(e) => self.status = format!("copy failed: {e}"),
        }
    }

    /// `D` — request archive confirmation.
    pub fn request_archive(&mut self) {
        if !matches!(self.focused_item(), Some(Item::Message(_))) {
            self.status = "archive: focus a message first".into();
            return;
        }
        self.confirm = Some(Confirm::Archive);
        self.status = "archive this message? [y/n]".into();
    }

    /// Confirm response (`y` / `n`).
    pub fn confirm_response(&mut self, yes: bool) {
        let Some(c) = self.confirm.take() else {
            return;
        };
        if !yes {
            self.status = "cancelled".into();
            return;
        }
        match c {
            Confirm::Archive => self.do_archive(),
        }
    }

    fn do_archive(&mut self) {
        let Some(Item::Message(m)) = self.focused_item().cloned() else {
            return;
        };
        let creds = self.creds.clone();
        let id = m.id.clone();
        match gmail::with_fresh_token(&creds, &mut self.token, |tok| {
            gmail::modify_labels(tok, &id, &[], &["INBOX"])
        }) {
            Ok(()) => {
                self.status = format!("archived {id}");
                // Reflect the change locally.
                let idx = self.active_tab;
                let sel = self.tabs[idx].data.selected;
                if let Some(Item::Message(msg)) = self.tabs[idx].data.items.get_mut(sel) {
                    msg.label_ids.retain(|l| l != "INBOX");
                }
                // If we're on the inbox view, drop the entry.
                if self.tabs[idx].spec.kind == "inbox" && self.tabs[idx].spec.label_filter.is_none()
                {
                    self.tabs[idx].data.items.remove(sel);
                    if self.tabs[idx].data.selected >= self.tabs[idx].data.items.len()
                        && !self.tabs[idx].data.items.is_empty()
                    {
                        self.tabs[idx].data.selected = self.tabs[idx].data.items.len() - 1;
                    }
                    self.detail_cache = None;
                }
            }
            Err(e) => self.status = format!("archive failed: {e}"),
        }
    }

    /// `!` — toggle the STARRED label on the focused message.
    pub fn toggle_star(&mut self) {
        let Some(Item::Message(m)) = self.focused_item().cloned() else {
            self.status = "star: focus a message first".into();
            return;
        };
        let id = m.id.clone();
        let currently = m.is_starred();
        let (add, remove): (&[&str], &[&str]) = if currently {
            (&[], &["STARRED"])
        } else {
            (&["STARRED"], &[])
        };
        let creds = self.creds.clone();
        match gmail::with_fresh_token(&creds, &mut self.token, |tok| {
            gmail::modify_labels(tok, &id, add, remove)
        }) {
            Ok(()) => {
                let idx = self.active_tab;
                let sel = self.tabs[idx].data.selected;
                if let Some(Item::Message(msg)) = self.tabs[idx].data.items.get_mut(sel) {
                    if currently {
                        msg.label_ids.retain(|l| l != "STARRED");
                    } else if !msg.label_ids.iter().any(|l| l == "STARRED") {
                        msg.label_ids.push("STARRED".into());
                    }
                }
                self.status = if currently {
                    format!("unstarred {id}")
                } else {
                    format!("starred {id}")
                };
            }
            Err(e) => self.status = format!("star failed: {e}"),
        }
    }

    // ── Compose ──────────────────────────────────────────────────

    pub fn begin_compose(&mut self) {
        self.compose = Some(Compose::new());
        self.status = "compose: To · Tab → next field · Ctrl+Enter send · Esc cancel".into();
    }

    pub fn cancel_compose(&mut self) {
        self.compose = None;
        self.status = "compose cancelled".into();
    }

    pub fn send_compose(&mut self) {
        let Some(c) = self.compose.as_ref() else {
            return;
        };
        if c.to.trim().is_empty() {
            self.status = "compose: To is required".into();
            return;
        }
        let to = c.to.clone();
        let subject = c.subject.clone();
        let body = c.body.clone();
        if let Some(cmp) = self.compose.as_mut() {
            cmp.sending = true;
        }
        let creds = self.creds.clone();
        let result = gmail::with_fresh_token(&creds, &mut self.token, |tok| {
            gmail::send_message(tok, &to, &subject, &body)
        });
        match result {
            Ok(()) => {
                self.status = format!("sent to {to}");
                self.compose = None;
            }
            Err(e) => {
                self.status = format!("send failed: {e}");
                if let Some(cmp) = self.compose.as_mut() {
                    cmp.sending = false;
                }
            }
        }
    }

    // ── Search ───────────────────────────────────────────────────

    /// `/` — focus the search tab and start editing the query.
    /// Picks the first `search` tab; if none exists, status hint.
    pub fn begin_search(&mut self) {
        let pos = self.tabs.iter().position(|t| t.spec.kind == "search");
        match pos {
            Some(i) => {
                self.active_tab = i;
                self.tabs[i].search_editing = true;
                self.status = "search — type query, Enter to run, Esc to cancel".into();
            }
            None => self.status = "no `search` tab configured".into(),
        }
    }

    pub fn search_input_char(&mut self, c: char) {
        if let Some(t) = self.tabs.get_mut(self.active_tab)
            && t.search_editing
        {
            t.search_input.push(c);
        }
    }
    pub fn search_input_backspace(&mut self) {
        if let Some(t) = self.tabs.get_mut(self.active_tab)
            && t.search_editing
        {
            t.search_input.pop();
        }
    }
    pub fn search_submit(&mut self) {
        if let Some(t) = self.tabs.get_mut(self.active_tab) {
            t.search_editing = false;
        }
        self.refresh_active();
    }
    pub fn search_cancel(&mut self) {
        if let Some(t) = self.tabs.get_mut(self.active_tab) {
            t.search_editing = false;
        }
        self.status = "search cancelled".into();
    }
    pub fn is_search_editing(&self) -> bool {
        self.tabs
            .get(self.active_tab)
            .map(|t| t.search_editing)
            .unwrap_or(false)
    }
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_display_with_quoted_name() {
        assert_eq!(
            parse_from_display(r#""Alice Example" <alice@example.com>"#),
            "Alice Example"
        );
    }

    #[test]
    fn from_display_with_bare_name() {
        assert_eq!(parse_from_display("Alice <alice@example.com>"), "Alice");
    }

    #[test]
    fn from_display_no_name_falls_back_to_addr() {
        assert_eq!(
            parse_from_display("<alice@example.com>"),
            "alice@example.com"
        );
    }

    #[test]
    fn from_display_no_angles_returns_raw() {
        assert_eq!(parse_from_display("alice@example.com"), "alice@example.com");
    }

    #[test]
    fn short_date_strips_dow_and_year() {
        assert_eq!(short_date("Thu, 1 Jan 2026 12:34:56 +0000"), "1 Jan");
    }

    #[test]
    fn age_string_levels() {
        assert_eq!(age_string(10), "10s");
        assert_eq!(age_string(125), "2m");
        assert_eq!(age_string(7_200), "2h");
        assert_eq!(age_string(86_400 * 3), "3d");
        assert_eq!(age_string(86_400 * 30), "4w");
    }

    #[test]
    fn truncate_handles_unicode() {
        let s = "abcdef";
        assert_eq!(truncate(s, 10), "abcdef");
        assert_eq!(truncate(s, 3), "ab…");
    }
}
