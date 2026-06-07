//! Keyboard chord → action mapping. v0.1.
//!
//! The router has three sub-modes:
//!   - normal tab nav
//!   - compose overlay editing
//!   - search input editing
//!
//! Confirm prompts ([y/n]) consume `y` and `n` directly.

use crate::app::{App, ComposeField};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub enum Action {
    Quit,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    OpenInBrowser,
    OpenSelection,
    Yank,
    Archive,
    ToggleStar,
    Refresh,
    SwitchTab(usize),
    NextTab,
    PrevTab,
    BeginCompose,
    BeginSearch,
    Confirm(bool),
}

/// Top-level router — returns either a generic `Action` or `None`
/// when the key was consumed by an overlay/sub-mode.
pub fn handle(key: KeyEvent, app: &mut App) -> Option<Action> {
    // 1. Compose overlay swallows almost everything.
    if app.compose.is_some() {
        compose_key(key, app);
        return None;
    }

    // 2. Search-editing sub-mode.
    if app.is_search_editing() {
        search_key(key, app);
        return None;
    }

    // 3. Confirm prompt.
    if app.confirm.is_some() {
        return match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => Some(Action::Confirm(true)),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Some(Action::Confirm(false)),
            _ => None,
        };
    }

    // 4. Normal mode.
    let m = key.modifiers;
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => Some(Action::Quit),
        KeyCode::Char('c') if m.contains(KeyModifiers::CONTROL) => Some(Action::Quit),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::Up),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::Down),
        KeyCode::PageUp => Some(Action::PageUp),
        KeyCode::PageDown => Some(Action::PageDown),
        KeyCode::Home | KeyCode::Char('g') => Some(Action::Home),
        KeyCode::End | KeyCode::Char('G') => Some(Action::End),
        KeyCode::Enter => Some(Action::OpenSelection),
        KeyCode::Char('o') => Some(Action::OpenInBrowser),
        KeyCode::Char('y') => Some(Action::Yank),
        KeyCode::Char('D') => Some(Action::Archive),
        KeyCode::Char('!') => Some(Action::ToggleStar),
        KeyCode::Char('c') => Some(Action::BeginCompose),
        KeyCode::Char('/') => Some(Action::BeginSearch),
        KeyCode::Char('r') => Some(Action::Refresh),
        KeyCode::Tab => Some(Action::NextTab),
        KeyCode::BackTab => Some(Action::PrevTab),
        KeyCode::Char(c @ '1'..='9') => Some(Action::SwitchTab((c as u8 - b'1') as usize)),
        _ => None,
    }
}

pub fn apply(action: Action, app: &mut App) -> bool {
    match action {
        Action::Quit => return true,
        Action::Up => app.move_selection(-1),
        Action::Down => app.move_selection(1),
        Action::PageUp => app.move_selection(-10),
        Action::PageDown => app.move_selection(10),
        Action::Home => app.move_selection(-(i32::MAX as isize)),
        Action::End => app.move_selection(i32::MAX as isize),
        Action::OpenSelection => app.open_focused(),
        Action::OpenInBrowser => app.open_console(),
        Action::Yank => app.yank(),
        Action::Archive => app.request_archive(),
        Action::ToggleStar => app.toggle_star(),
        Action::Refresh => app.refresh_active(),
        Action::BeginCompose => app.begin_compose(),
        Action::BeginSearch => app.begin_search(),
        Action::Confirm(yes) => app.confirm_response(yes),
        Action::NextTab => {
            let next = (app.active_tab + 1) % app.tabs.len();
            app.switch_tab(next);
        }
        Action::PrevTab => {
            let prev = if app.active_tab == 0 {
                app.tabs.len() - 1
            } else {
                app.active_tab - 1
            };
            app.switch_tab(prev);
        }
        Action::SwitchTab(i) => app.switch_tab(i),
    }
    false
}

// ── Compose overlay ──────────────────────────────────────────────

fn compose_key(key: KeyEvent, app: &mut App) {
    let m = key.modifiers;
    // Sending? Swallow input until the network call returns.
    if let Some(c) = &app.compose
        && c.sending
    {
        return;
    }

    // Global compose chords first.
    match key.code {
        KeyCode::Esc => {
            app.cancel_compose();
            return;
        }
        KeyCode::Enter if m.contains(KeyModifiers::CONTROL) => {
            app.send_compose();
            return;
        }
        KeyCode::Char('s') if m.contains(KeyModifiers::CONTROL) => {
            app.send_compose();
            return;
        }
        KeyCode::Tab => {
            if let Some(c) = app.compose.as_mut() {
                c.next_field();
            }
            return;
        }
        KeyCode::BackTab => {
            if let Some(c) = app.compose.as_mut() {
                c.prev_field();
            }
            return;
        }
        _ => {}
    }

    // Per-field editing.
    let Some(c) = app.compose.as_mut() else {
        return;
    };
    match key.code {
        KeyCode::Backspace => {
            c.focused_mut().pop();
        }
        KeyCode::Enter => {
            // Newline only inside the Body field.
            if c.field == ComposeField::Body {
                c.body.push('\n');
            } else {
                c.next_field();
            }
        }
        KeyCode::Char(ch) => {
            // Ignore control-modifier chords that aren't already
            // handled (e.g. Ctrl+C is treated as the OS-level
            // copy intent; just ignore here).
            if m.contains(KeyModifiers::CONTROL) {
                return;
            }
            c.focused_mut().push(ch);
        }
        _ => {}
    }
}

// ── Search editing ───────────────────────────────────────────────

fn search_key(key: KeyEvent, app: &mut App) {
    match key.code {
        KeyCode::Esc => app.search_cancel(),
        KeyCode::Enter => app.search_submit(),
        KeyCode::Backspace => app.search_input_backspace(),
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.search_input_char(c);
        }
        _ => {}
    }
}
