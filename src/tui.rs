//! `dashcam tui`: a read-only terminal UI over the local timeline.
//!
//! Sessions on the left, the selected session's events on the right,
//! and optional per-model token/cost totals on `c`. It opens the same
//! store the CLI does and never writes.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{self, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Row, Table};

use dashcam::event::{Event, EventKind};
use dashcam::key;
use dashcam::store::Store;
use dashcam::usage::{self, PriceRow};

pub struct CostRow {
    pub model: String,
    pub turns: u64,
    pub prompt: u64,
    pub completion: u64,
    pub cost: f64,
}

/// Aggregate model turns into per-model token/cost rows.
pub fn aggregate(events: &[Event], prices: &[PriceRow]) -> Vec<CostRow> {
    let mut map: BTreeMap<String, CostRow> = BTreeMap::new();
    for ev in events {
        if ev.kind != EventKind::ModelTurn {
            continue;
        }
        let Ok(p) = serde_json::from_str::<serde_json::Value>(&ev.payload) else {
            continue;
        };
        let Some(u) = p.get("usage") else { continue };
        let model = u.get("model").and_then(|m| m.as_str()).unwrap_or("").to_string();
        if model.is_empty() {
            continue;
        }
        let prompt = u
            .get("prompt_tokens")
            .and_then(|v| v.as_u64())
            .or_else(|| p.get("prompt_tokens").and_then(|v| v.as_u64()))
            .unwrap_or(0);
        let completion = u
            .get("completion_tokens")
            .and_then(|v| v.as_u64())
            .or_else(|| p.get("completion_tokens").and_then(|v| v.as_u64()))
            .unwrap_or(0);
        let row = map.entry(model.clone()).or_insert_with(|| CostRow {
            model: model.clone(),
            turns: 0,
            prompt: 0,
            completion: 0,
            cost: 0.0,
        });
        row.turns += 1;
        row.prompt += prompt;
        row.completion += completion;
    }
    let mut rows: Vec<CostRow> = map.into_values().collect();
    for r in &mut rows {
        if let Some((pin, pout)) = usage::price_of(prices, &r.model) {
            r.cost = r.prompt as f64 / 1e6 * pin + r.completion as f64 / 1e6 * pout;
        }
    }
    rows
}

fn kind_label(kind: EventKind) -> &'static str {
    match kind {
        EventKind::NetworkHop => "hop",
        EventKind::ModelTurn => "model",
        EventKind::ToolCall => "tool",
        EventKind::Prompt => "prompt",
        EventKind::FileDiff => "diff",
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}\u{2026}", head)
}

struct App {
    store: Store,
    db_label: String,
    prices: Vec<PriceRow>,
    sessions: Vec<(i64, String, Option<String>, i64)>,
    sel: usize,
    events: Vec<Event>,
    focus_sessions: bool,
    ev_sel: usize,
    costs: Option<Vec<CostRow>>,
    quit: bool,
}

impl App {
    fn reload(&mut self) {
        self.sessions = self.store.sessions().unwrap_or_default();
        if self.sel >= self.sessions.len() {
            self.sel = self.sessions.len().saturating_sub(1);
        }
        self.load_events();
    }

    fn load_events(&mut self) {
        self.events = match self.sessions.get(self.sel) {
            Some((id, _, _, _)) => self.store.events(*id).unwrap_or_default(),
            None => Vec::new(),
        };
        self.ev_sel = 0;
    }

    fn toggle_costs(&mut self) {
        if self.costs.is_some() {
            self.costs = None;
            return;
        }
        let mut all: Vec<Event> = Vec::new();
        for (id, _, _, _) in &self.sessions {
            if let Ok(ev) = self.store.events(*id) {
                all.extend(ev);
            }
        }
        self.costs = Some(aggregate(&all, &self.prices));
    }

    fn next(&mut self, delta: isize) {
        if self.focus_sessions {
            let n = self.sessions.len();
            if n == 0 {
                return;
            }
            let target = (self.sel as isize + delta).clamp(0, n as isize - 1) as usize;
            if target != self.sel {
                self.sel = target;
                self.load_events();
            }
        } else {
            let n = self.events.len();
            if n == 0 {
                return;
            }
            self.ev_sel = (self.ev_sel as isize + delta).clamp(0, n as isize - 1) as usize;
        }
    }

    fn jump(&mut self, top: bool) {
        if self.focus_sessions {
            let n = self.sessions.len();
            if n == 0 {
                return;
            }
            let target = if top { 0 } else { n - 1 };
            if target != self.sel {
                self.sel = target;
                self.load_events();
            }
        } else {
            let n = self.events.len();
            if n == 0 {
                return;
            }
            self.ev_sel = if top { 0 } else { n - 1 };
        }
    }

    fn on_key(&mut self, k: KeyEvent) {
        if self.costs.is_some() {
            match k.code {
                KeyCode::Char('q') | KeyCode::Esc | KeyCode::Char('c') => self.costs = None,
                KeyCode::Char('C') if k.modifiers.contains(KeyModifiers::CONTROL) => self.quit = true,
                _ => {}
            }
            return;
        }
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => self.quit = true,
            KeyCode::Char('c') => self.toggle_costs(),
            KeyCode::Char('r') => self.reload(),
            KeyCode::Tab => self.focus_sessions = !self.focus_sessions,
            KeyCode::Down | KeyCode::Char('j') => self.next(1),
            KeyCode::Up | KeyCode::Char('k') => self.next(-1),
            KeyCode::PageDown => self.next(10),
            KeyCode::PageUp => self.next(-10),
            KeyCode::Home | KeyCode::Char('g') => self.jump(true),
            KeyCode::End | KeyCode::Char('G') => self.jump(false),
            _ => {}
        }
    }

    fn event_loop(&mut self, terminal: &mut ratatui::DefaultTerminal) -> Result<(), String> {
        while !self.quit {
            terminal
                .draw(|f| self.draw(f))
                .map_err(|e| e.to_string())?;
            if event::poll(Duration::from_millis(250)).map_err(|e| e.to_string())? {
                if let event::Event::Key(k) = event::read().map_err(|e| e.to_string())? {
                    if k.kind == KeyEventKind::Press {
                        self.on_key(k);
                    }
                }
            }
        }
        Ok(())
    }

    fn draw(&mut self, f: &mut ratatui::Frame) {
        let title = format!(
            " dashcam tui v{} | {} | {} sessions | j/k move | tab focus | c costs | r refresh | q quit ",
            env!("CARGO_PKG_VERSION"),
            self.db_label,
            self.sessions.len(),
        );
        let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(f.area());
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                title,
                Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD),
            ))),
            chunks[0],
        );
        let body = Layout::horizontal([Constraint::Percentage(34), Constraint::Percentage(66)]).split(chunks[1]);

        let items: Vec<ListItem> = self
            .sessions
            .iter()
            .map(|(id, started, note, hops)| {
                let text = format!(
                    " {:>4}  {:<19}  {:<24}  {:>4} hops",
                    id,
                    truncate(started, 19),
                    note.as_deref().map(|n| truncate(n, 24)).unwrap_or_else(|| "-".to_string()),
                    hops
                );
                ListItem::new(text)
            })
            .collect();
        let list = List::new(items)
            .block(Block::bordered().title("sessions").border_style(if self.focus_sessions {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::DarkGray)
            }))
            .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD));
        let mut st = ListState::default();
        if !self.sessions.is_empty() {
            st.select(Some(self.sel));
        }
        f.render_stateful_widget(list, body[0], &mut st);

        let sel_title = self
            .sessions
            .get(self.sel)
            .map(|(id, _, _, _)| format!("events - session {}", id))
            .unwrap_or_else(|| "events".to_string());
        let ev_items: Vec<ListItem> = self
            .events
            .iter()
            .map(|ev| {
                ListItem::new(format!(
                    " {:>5}  {:<6}  {}",
                    ev.seq,
                    kind_label(ev.kind),
                    truncate(&ev.summary, 96)
                ))
            })
            .collect();
        let ev_list = List::new(ev_items)
            .block(Block::bordered().title(sel_title).border_style(if self.focus_sessions {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::Cyan)
            }))
            .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD));
        let mut est = ListState::default();
        if !self.events.is_empty() {
            est.select(Some(self.ev_sel));
        }
        f.render_stateful_widget(ev_list, body[1], &mut est);

        if self.costs.is_some() {
            draw_costs(f, self.costs.as_deref().unwrap_or(&[]), !self.prices.is_empty());
        }
    }
}

pub fn run(db_path: PathBuf, prices: Vec<PriceRow>) -> Result<(), String> {
    let mut key_path = db_path.clone();
    key_path.set_file_name("key");
    let hex = key::load_or_create(&key_path).map_err(|e| e.to_string())?;
    let store = Store::open(&db_path, Some(&hex)).map_err(|e| e.to_string())?;
    let db_label = db_path.display().to_string();
    let mut app = App {
        store,
        db_label,
        prices,
        sessions: Vec::new(),
        sel: 0,
        events: Vec::new(),
        focus_sessions: true,
        ev_sel: 0,
        costs: None,
        quit: false,
    };
    app.reload();
    let mut terminal = ratatui::init();
    let res = app.event_loop(&mut terminal);
    ratatui::restore();
    res
}

fn costs_area(r: Rect) -> Rect {
    let v = Layout::vertical([
        Constraint::Percentage(20),
        Constraint::Percentage(60),
        Constraint::Percentage(20),
    ])
    .split(r);
    let h = Layout::horizontal([
        Constraint::Percentage(10),
        Constraint::Percentage(80),
        Constraint::Percentage(10),
    ])
    .split(v[1]);
    h[1]
}

fn draw_costs(f: &mut ratatui::Frame, rows: &[CostRow], has_prices: bool) {
    let area = costs_area(f.area());
    f.render_widget(Clear, area);
    let header = Row::new(vec!["model", "turns", "prompt", "completion", "cost"])
        .style(Style::default().add_modifier(Modifier::BOLD));
    let body: Vec<Row> = rows
        .iter()
        .map(|r| {
            Row::new(vec![
                truncate(&r.model, 28),
                r.turns.to_string(),
                r.prompt.to_string(),
                r.completion.to_string(),
                if r.cost > 0.0 { format!("${:.4}", r.cost) } else { "-".to_string() },
            ])
        })
        .collect();
    let totals_t: u64 = rows.iter().map(|r| r.turns).sum();
    let totals_p: u64 = rows.iter().map(|r| r.prompt).sum();
    let totals_c: u64 = rows.iter().map(|r| r.completion).sum();
    let total_cost: f64 = rows.iter().map(|r| r.cost).sum();
    let footer = Row::new(vec![
        "total".to_string(),
        totals_t.to_string(),
        totals_p.to_string(),
        totals_c.to_string(),
        if total_cost > 0.0 { format!("${:.4}", total_cost) } else { "-".to_string() },
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));
    let widths = [
        Constraint::Percentage(34),
        Constraint::Percentage(12),
        Constraint::Percentage(18),
        Constraint::Percentage(20),
        Constraint::Percentage(16),
    ];
    let title = if has_prices {
        "costs (all sessions)"
    } else {
        "costs (all sessions, no prices loaded)"
    };
    let t = Table::new(body, widths)
        .header(header)
        .footer(footer)
        .block(Block::bordered().title(title));
    f.render_widget(t, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: EventKind, payload: &str) -> Event {
        Event {
            id: 1,
            session_id: 1,
            seq: 1,
            ts: "t".into(),
            kind,
            summary: "s".into(),
            payload: payload.into(),
        }
    }

    #[test]
    fn aggregate_sums_tokens_and_costs() {
        let events = vec![
            ev(EventKind::ModelTurn, r#"{"usage":{"model":"m1","prompt_tokens":1000,"completion_tokens":500}}"#),
            ev(EventKind::ModelTurn, r#"{"usage":{"model":"m1","prompt_tokens":500,"completion_tokens":500}}"#),
            ev(EventKind::ToolCall, "{}"),
        ];
        let prices = usage::parse_prices("model,in,out\nm1,0.50,2.00\n");
        let rows = aggregate(&events, &prices);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].turns, 2);
        assert_eq!(rows[0].prompt, 1500);
        assert_eq!(rows[0].completion, 1000);
        assert!((rows[0].cost - 0.00275).abs() < 1e-9);
    }

    #[test]
    fn aggregate_skips_unparsed_and_no_model() {
        let events = vec![
            ev(EventKind::ModelTurn, "not json"),
            ev(EventKind::ModelTurn, r#"{"usage":{"prompt_tokens":10}}"#),
            ev(EventKind::Prompt, r#"{"usage":{"model":"m1"}}"#),
        ];
        assert!(aggregate(&events, &[]).is_empty());
    }

    #[test]
    fn truncate_respects_char_boundary() {
        assert_eq!(truncate("hello", 3), "he\u{2026}");
        assert_eq!(truncate("hi", 3), "hi");
        assert_eq!(truncate("abcdef", 10), "abcdef");
    }
}
