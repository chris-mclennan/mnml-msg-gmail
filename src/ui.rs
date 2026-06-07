//! ratatui rendering + the main event loop.

use crate::app::{App, ComposeField, Item, TabState};
use crate::keys;
use anyhow::Result;
use crossterm::{
    event::{self, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Tabs, Wrap},
};
use std::io::Stdout;
use std::time::Duration;

pub fn run(app: &mut App) -> Result<()> {
    let mut stdout = std::io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = event_loop(&mut terminal, app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    res
}

fn event_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|f| draw(f, app))?;
        app.tick();
        if event::poll(Duration::from_millis(250))?
            && let Event::Key(key) = event::read()?
            && key.kind == event::KeyEventKind::Press
            && let Some(action) = keys::handle(key, app)
        {
            let quit = keys::apply(action, app);
            if quit {
                break;
            }
        }
    }
    Ok(())
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let size = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(size);
    draw_tabs(f, chunks[0], app);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(chunks[1]);
    draw_list(f, body[0], app.active());
    // Lazy-fetch the focused message body before rendering detail.
    if app.compose.is_none() {
        app.ensure_detail();
    }
    draw_detail(f, body[1], app);
    draw_status(f, chunks[2], app);

    if app.compose.is_some() {
        draw_compose(f, app);
    }
}

fn draw_tabs(f: &mut Frame, area: Rect, app: &App) {
    let labels: Vec<Line> = app
        .tabs
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let badge = if t.data.loading {
                " (…)".to_string()
            } else if t.data.last_error.is_some() {
                " (err)".to_string()
            } else if t.spec.kind == "search" && t.search_input.trim().is_empty() {
                "".to_string()
            } else {
                format!(" ({})", t.data.items.len())
            };
            Line::from(format!("{}.{}{}", i + 1, t.name, badge))
        })
        .collect();
    let tabs = Tabs::new(labels)
        .block(Block::default().borders(Borders::ALL).title(" gmail "))
        .select(app.active_tab)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(tabs, area);
}

fn draw_list(f: &mut Frame, area: Rect, tab: &TabState) {
    // Search-edit overlay row: show the live query.
    if tab.spec.kind == "search" {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(1)])
            .split(area);
        draw_search_input(f, chunks[0], tab);
        draw_items(f, chunks[1], tab);
    } else {
        draw_items(f, area, tab);
    }
}

fn draw_search_input(f: &mut Frame, area: Rect, tab: &TabState) {
    let title = if tab.search_editing {
        " search (editing) "
    } else {
        " search "
    };
    let cursor = if tab.search_editing { "_" } else { "" };
    let line = Line::from(vec![
        Span::styled(" q: ", Style::default().fg(Color::DarkGray)),
        Span::styled(tab.search_input.clone(), Style::default().fg(Color::White)),
        Span::styled(cursor, Style::default().fg(Color::Cyan)),
    ]);
    let p = Paragraph::new(line).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn draw_items(f: &mut Frame, area: Rect, tab: &TabState) {
    if let Some(err) = &tab.data.last_error {
        let p = Paragraph::new(format!("error: {err}"))
            .style(Style::default().fg(Color::Red))
            .block(Block::default().borders(Borders::ALL).title(" items "));
        f.render_widget(p, area);
        return;
    }
    if tab.data.items.is_empty() {
        let msg = if tab.data.loading {
            "(loading…)"
        } else if tab.spec.kind == "search" && tab.search_input.trim().is_empty() {
            "(type a query, e.g. `from:alice has:attachment newer_than:7d`)"
        } else {
            "(none)"
        };
        let p = Paragraph::new(msg)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL).title(" items "));
        f.render_widget(p, area);
        return;
    }
    let body_rows = area.height.saturating_sub(2) as usize;
    let total = tab.data.items.len();
    let selected = tab.data.selected;
    let start = if total <= body_rows {
        0
    } else {
        let lo = selected.saturating_sub(body_rows / 2);
        lo.min(total - body_rows)
    };

    let lines: Vec<Line> = tab.data.items[start..]
        .iter()
        .take(body_rows)
        .enumerate()
        .map(|(i, item)| {
            let abs = start + i;
            let cursor = if abs == selected { "▸ " } else { "  " };
            let primary = truncate(&item.primary_label(), 22);
            let secondary = item.secondary_label();
            let line = format!("{cursor}{:<22}  {secondary}", primary);
            let style = if abs == selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                state_style_for(item)
            };
            Line::from(Span::styled(line, style))
        })
        .collect();

    let title = match tab.spec.kind.as_str() {
        "inbox" => format!(" inbox ({total}) "),
        "sent" => format!(" sent ({total}) "),
        "starred" => format!(" starred ({total}) "),
        "labels" => format!(" labels ({total}) "),
        "search" => format!(" results ({total}) "),
        _ => format!(" items ({total}) "),
    };
    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn state_style_for(item: &Item) -> Style {
    match item {
        Item::Message(m) => {
            let mut s = Style::default().fg(Color::Gray);
            if m.is_unread() {
                s = s.add_modifier(Modifier::BOLD).fg(Color::White);
            }
            if m.is_starred() {
                s = s.fg(Color::Yellow);
            }
            s
        }
        Item::Label(l) => {
            if l.messages_unread > 0 {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            }
        }
    }
}

fn draw_detail(f: &mut Frame, area: Rect, app: &App) {
    let title = " detail ";
    let Some(item) = app.focused_item() else {
        let p = Paragraph::new("(no item selected)")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL).title(title));
        f.render_widget(p, area);
        return;
    };
    let mut lines: Vec<Line> = Vec::new();
    let kv = |k: &str, v: String| -> Line<'static> {
        Line::from(vec![
            Span::styled(format!(" {k:<10}"), Style::default().fg(Color::DarkGray)),
            Span::styled(v, Style::default().fg(Color::White)),
        ])
    };

    match item {
        Item::Message(m) => {
            if let Some(s) = m.header("Subject") {
                lines.push(kv("Subject", s.to_string()));
            }
            if let Some(s) = m.header("From") {
                lines.push(kv("From", s.to_string()));
            }
            if let Some(s) = m.header("To") {
                lines.push(kv("To", s.to_string()));
            }
            if let Some(s) = m.header("Cc") {
                lines.push(kv("Cc", s.to_string()));
            }
            if let Some(s) = m.header("Date") {
                lines.push(kv("Date", s.to_string()));
            }
            if !m.label_ids.is_empty() {
                lines.push(kv("Labels", m.label_ids.join(", ")));
            }
            lines.push(kv("ID", m.id.clone()));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                " Body",
                Style::default().fg(Color::DarkGray),
            )));
            let body_src = app
                .detail_cache
                .as_ref()
                .filter(|(k, _)| k == &m.id)
                .map(|(_, body)| body.clone())
                .unwrap_or_else(|| m.snippet.clone());
            for ln in body_src.lines().take(200) {
                lines.push(Line::from(Span::styled(
                    format!(" {ln}"),
                    Style::default().fg(Color::Gray),
                )));
            }
        }
        Item::Label(l) => {
            lines.push(kv("Name", l.name.clone()));
            lines.push(kv("ID", l.id.clone()));
            lines.push(kv("Type", l.label_type.clone()));
            lines.push(kv("Unread", l.messages_unread.to_string()));
            lines.push(kv("Total", l.messages_total.to_string()));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                " Press Enter to view messages in this label.",
                Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
            )));
        }
    }

    let p = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let hint = if app.compose.is_some() {
        " Tab next · BackTab prev · Ctrl+Enter send · Esc cancel "
    } else if app.is_search_editing() {
        " type query · Enter run · Esc cancel "
    } else if app.confirm.is_some() {
        " y to confirm · n / Esc to cancel "
    } else {
        " 1-9 tab · ↑↓/jk move · Enter open · / search · c compose · r refresh · y URL · D archive · ! star · q quit "
    };
    let line = Line::from(vec![
        Span::styled(
            format!(" {} ", app.status),
            Style::default().fg(Color::White),
        ),
        Span::styled(
            hint,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn draw_compose(f: &mut Frame, app: &App) {
    let Some(c) = &app.compose else { return };
    let area = f.area();
    // Overlay 60% wide, 70% tall, centered.
    let w = (area.width as u32 * 60 / 100) as u16;
    let h = (area.height as u32 * 70 / 100) as u16;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let r = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, r);

    let inner = Block::default().borders(Borders::ALL).title(if c.sending {
        " compose — sending… "
    } else {
        " compose "
    });
    let inner_area = inner.inner(r);
    f.render_widget(inner, r);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // To
            Constraint::Length(3), // Subject
            Constraint::Min(3),    // Body
        ])
        .split(inner_area);

    let field_block = |title: &'static str, focused: bool| {
        let style = if focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(style)
    };

    let to_focused = c.field == ComposeField::To;
    let subject_focused = c.field == ComposeField::Subject;
    let body_focused = c.field == ComposeField::Body;

    let to_value = format!("{}{}", c.to, if to_focused { "_" } else { "" });
    let to = Paragraph::new(to_value).block(field_block(" To ", to_focused));
    f.render_widget(to, rows[0]);

    let subj_value = format!("{}{}", c.subject, if subject_focused { "_" } else { "" });
    let subj = Paragraph::new(subj_value).block(field_block(" Subject ", subject_focused));
    f.render_widget(subj, rows[1]);

    let body_value = if body_focused {
        format!("{}_", c.body)
    } else {
        c.body.clone()
    };
    let body = Paragraph::new(body_value)
        .block(field_block(" Body ", body_focused))
        .wrap(Wrap { trim: false });
    f.render_widget(body, rows[2]);
}

fn truncate(s: &str, max: usize) -> String {
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
    fn truncate_short_strings_unchanged() {
        assert_eq!(truncate("short", 10), "short");
    }
}
