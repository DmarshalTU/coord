//! `coord top` — live ratatui dashboard of agents and tasks.

use std::io::stdout;
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::{DateTime, Utc};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Row, Table, TableState},
    Terminal,
};
use serde_json::{json, Value};

use super::client::Client;
use super::format::{human_age, parse_age};

/// An agent stops appearing in the default `top` view this many seconds
/// after its last heartbeat. Toggle with `A` to override.
pub const STALE_AGENT_AFTER_SECS: f64 = 60.0;
/// Idle-but-not-stale: rendered yellow, still visible.
pub const IDLE_AGENT_AFTER_SECS: f64 = 15.0;

pub fn run(client: &Client) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout_handle = stdout();
    execute!(stdout_handle, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout_handle);
    let mut terminal = Terminal::new(backend)?;
    let result = top_loop(&mut terminal, client);
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

/// One-shot render to stdout using ratatui's TestBackend. Used by
/// snapshot tests and the `--once` flag.
pub fn render_once(client: &Client) -> Result<()> {
    use ratatui::backend::TestBackend;
    let mut state = TopState::default();
    refresh(client, &mut state);
    let visible_tasks = filter_tasks(&state.tasks, state.filter);
    if !visible_tasks.is_empty() {
        state.table_state.select(Some(0));
    }
    let backend = TestBackend::new(140, 36);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|f| draw(f, &state, &visible_tasks))?;
    let buf = terminal.backend().buffer().clone();
    for y in 0..buf.area.height {
        let mut line = String::new();
        for x in 0..buf.area.width {
            line.push_str(buf[(x, y)].symbol());
        }
        println!("{}", line.trim_end());
    }
    Ok(())
}

struct TopState {
    agents: Vec<Value>,
    tasks: Vec<Value>,
    filter: Filter,
    /// Hide agents whose last_seen is older than `STALE_AGENT_AFTER_SECS`.
    /// Toggle with `A`.
    hide_stale_agents: bool,
    show_detail: bool,
    paused: bool,
    /// Currently-highlighted task row, relative to the post-filter slice.
    selected: usize,
    table_state: TableState,
    last_err: Option<String>,
    status: Option<(String, Instant)>,
    sent_test_count: u64,
    last_refresh: Option<Instant>,
}

impl Default for TopState {
    fn default() -> Self {
        Self {
            agents: vec![],
            tasks: vec![],
            filter: Filter::default(),
            hide_stale_agents: true,
            show_detail: true,
            paused: false,
            selected: 0,
            table_state: TableState::default(),
            last_err: None,
            status: None,
            sent_test_count: 0,
            last_refresh: None,
        }
    }
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Filter {
    /// pending + claimed only (plus announcements as context)
    #[default]
    Active,
    /// pending + claimed + failed (i.e. needs human attention)
    Attention,
    /// everything
    All,
}

impl Filter {
    fn label(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Attention => "attention",
            Self::All => "all",
        }
    }
    fn cycle(self) -> Self {
        match self {
            Self::Active => Self::Attention,
            Self::Attention => Self::All,
            Self::All => Self::Active,
        }
    }
    fn matches(&self, state: &str, kind: &str) -> bool {
        // Announcements (ack / knowledge / decision) are completed-on-
        // creation but still useful context for the viewer of the ledger.
        // Surface them in non-All filters too.
        let is_announcement = matches!(kind, "ack" | "knowledge" | "decision");
        match self {
            Self::All => true,
            Self::Active => matches!(state, "pending" | "claimed") || is_announcement,
            Self::Attention => matches!(state, "pending" | "claimed" | "failed") || is_announcement,
        }
    }
}

fn top_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    client: &Client,
) -> Result<()> {
    let tick = Duration::from_millis(500);
    let mut last_tick = Instant::now();
    let mut state = TopState::default();
    refresh(client, &mut state);

    loop {
        if !state.paused && last_tick.elapsed() >= tick {
            refresh(client, &mut state);
            last_tick = Instant::now();
        }

        if let Some((_, t)) = state.status {
            if t.elapsed() > Duration::from_secs(3) {
                state.status = None;
            }
        }

        let visible_tasks = filter_tasks(&state.tasks, state.filter);
        if state.selected >= visible_tasks.len() && !visible_tasks.is_empty() {
            state.selected = visible_tasks.len() - 1;
        }
        state.table_state.select(if visible_tasks.is_empty() {
            None
        } else {
            Some(state.selected)
        });

        terminal.draw(|f| draw(f, &state, &visible_tasks))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                match k.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('j') | KeyCode::Down if !visible_tasks.is_empty() => {
                        state.selected = (state.selected + 1).min(visible_tasks.len() - 1);
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        state.selected = state.selected.saturating_sub(1);
                    }
                    KeyCode::Char('g') => state.selected = 0,
                    KeyCode::Char('G') if !visible_tasks.is_empty() => {
                        state.selected = visible_tasks.len() - 1;
                    }
                    KeyCode::Char('f') => state.filter = state.filter.cycle(),
                    KeyCode::Char('A') => {
                        state.hide_stale_agents = !state.hide_stale_agents;
                        let msg = if state.hide_stale_agents {
                            format!("hiding agents idle > {STALE_AGENT_AFTER_SECS:.0}s")
                        } else {
                            "showing all agents".into()
                        };
                        set_status(&mut state, msg);
                    }
                    KeyCode::Char('d') => state.show_detail = !state.show_detail,
                    KeyCode::Char('p') => {
                        state.paused = !state.paused;
                        let msg = if state.paused { "paused" } else { "resumed" };
                        set_status(&mut state, msg.into());
                    }
                    KeyCode::Char('r') => {
                        refresh(client, &mut state);
                        last_tick = Instant::now();
                        set_status(&mut state, "refreshed".into());
                    }
                    KeyCode::Char('s') => {
                        state.sent_test_count += 1;
                        let name = format!("top-test-{}", state.sent_test_count);
                        match client.call("tasks/send", json!({ "name": name, "payload": {} })) {
                            Ok(t) => {
                                let id = t.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                                let short: String = id.chars().take(8).collect();
                                set_status(&mut state, format!("sent {short}"));
                            }
                            Err(e) => set_status(&mut state, format!("send failed: {e}")),
                        }
                        refresh(client, &mut state);
                    }
                    KeyCode::Char('c') => {
                        if let Some(task) = visible_tasks.get(state.selected) {
                            if let Some(id) = task.get("id").and_then(|v| v.as_str()) {
                                match client.call("tasks/cancel", json!({ "id": id })) {
                                    Ok(_) => {
                                        let short: String = id.chars().take(8).collect();
                                        set_status(&mut state, format!("cancelled {short}"));
                                    }
                                    Err(e) => set_status(&mut state, format!("cancel failed: {e}")),
                                }
                                refresh(client, &mut state);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn set_status(state: &mut TopState, msg: String) {
    state.status = Some((msg, Instant::now()));
}

fn refresh(client: &Client, state: &mut TopState) {
    let mut new_err: Option<String> = None;
    match client.call("agents/list", json!({})) {
        Ok(v) => state.agents = v.as_array().cloned().unwrap_or_default(),
        Err(e) => new_err = Some(e.to_string()),
    }
    match client.call("tasks/list", json!({ "limit": 200 })) {
        Ok(v) => state.tasks = v.as_array().cloned().unwrap_or_default(),
        Err(e) => new_err = Some(e.to_string()),
    }
    state.last_err = new_err;
    if state.last_err.is_none() {
        state.last_refresh = Some(Instant::now());
    }
}

fn filter_tasks(tasks: &[Value], filter: Filter) -> Vec<Value> {
    tasks
        .iter()
        .filter(|t| {
            let s = t.get("state").and_then(|v| v.as_str()).unwrap_or("");
            let k = t.get("kind").and_then(|v| v.as_str()).unwrap_or("task");
            filter.matches(s, k)
        })
        .cloned()
        .collect()
}

fn count_state(tasks: &[Value], state: &str) -> usize {
    tasks
        .iter()
        .filter(|t| t.get("state").and_then(|s| s.as_str()) == Some(state))
        .count()
}

fn draw(f: &mut ratatui::Frame<'_>, state: &TopState, visible_tasks: &[Value]) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(f.area());

    let now = Utc::now();
    let (active_n, idle_n, stale_n) = partition_agents(&state.agents, now);
    let visible_agents: Vec<&Value> = state
        .agents
        .iter()
        .filter(|a| {
            if !state.hide_stale_agents {
                return true;
            }
            let ls = a.get("last_seen").and_then(|v| v.as_str()).unwrap_or("");
            parse_age(ls, now)
                .map(|s| s <= STALE_AGENT_AFTER_SECS)
                .unwrap_or(true)
        })
        .collect();

    // Header (3 lines).
    let header_l1 = Line::from(vec![
        Span::styled(
            "coord top",
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Magenta),
        ),
        Span::raw("  •  "),
        Span::styled(
            format!("{active_n}"),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" active  "),
        Span::styled(format!("{idle_n}"), Style::default().fg(Color::Yellow)),
        Span::raw(" idle  "),
        Span::styled(format!("{stale_n}"), Style::default().fg(Color::DarkGray)),
        Span::raw(format!(" stale  ({} agents total)", state.agents.len())),
    ]);
    let header_l2 = Line::from(vec![
        Span::raw("tasks: "),
        Span::styled(
            format!("{}", visible_tasks.len()),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" visible / {} total  ", state.tasks.len())),
        bracket_count(
            "pending",
            count_state(&state.tasks, "pending"),
            Color::Yellow,
        ),
        Span::raw(" "),
        bracket_count("claimed", count_state(&state.tasks, "claimed"), Color::Cyan),
        Span::raw(" "),
        bracket_count(
            "completed",
            count_state(&state.tasks, "completed"),
            Color::Green,
        ),
        Span::raw(" "),
        bracket_count("failed", count_state(&state.tasks, "failed"), Color::Red),
        Span::raw(" "),
        bracket_count(
            "cancelled",
            count_state(&state.tasks, "cancelled"),
            Color::DarkGray,
        ),
    ]);

    let refresh_age = state
        .last_refresh
        .map(|t| format!("{:.1}s ago", t.elapsed().as_secs_f64()))
        .unwrap_or_else(|| "never".into());
    let pause_marker = if state.paused {
        Span::styled(
            "  PAUSED",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw("")
    };
    let header_l3 = Line::from(vec![
        Span::raw(format!("filter: {}  •  ", state.filter.label())),
        Span::raw(format!(
            "agents-hidden: {}  •  detail: {}  •  refreshed {}",
            if state.hide_stale_agents {
                "stale"
            } else {
                "off"
            },
            if state.show_detail { "on" } else { "off" },
            refresh_age,
        )),
        pause_marker,
    ]);

    let header = Paragraph::new(vec![header_l1, header_l2, header_l3])
        .block(Block::default().borders(Borders::ALL).title("coord"));
    f.render_widget(header, chunks[0]);

    // Body: agents | tasks (| detail). Detail pane only shows on wide
    // terminals so the tasks table doesn't get squashed.
    let term_w = chunks[1].width;
    let show_detail_effective = state.show_detail && term_w >= 116;
    let constraints: Vec<Constraint> = if show_detail_effective {
        vec![
            Constraint::Length(36),
            Constraint::Min(42),
            Constraint::Length(38),
        ]
    } else {
        vec![Constraint::Length(36), Constraint::Min(42)]
    };
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(chunks[1]);

    // Agents pane.
    let agents_title = format!(
        "agents ({}{})",
        visible_agents.len(),
        if state.hide_stale_agents && stale_n > 0 {
            format!(" • +{stale_n} hidden")
        } else {
            String::new()
        },
    );
    let agents_pane = Block::default().borders(Borders::ALL).title(agents_title);
    if visible_agents.is_empty() {
        let hint = if state.hide_stale_agents && stale_n > 0 {
            format!("all {stale_n} agents are stale\npress A to show them")
        } else {
            "no agents have heartbeated yet".to_string()
        };
        let p = Paragraph::new(hint)
            .style(Style::default().fg(Color::DarkGray))
            .block(agents_pane);
        f.render_widget(p, panes[0]);
    } else {
        let rows: Vec<Row> = visible_agents
            .iter()
            .map(|a| {
                let last_seen_raw = a.get("last_seen").and_then(|v| v.as_str()).unwrap_or("");
                let beat_age = parse_age(last_seen_raw, now);
                let style = match beat_age {
                    Some(s) if s <= IDLE_AGENT_AFTER_SECS => Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                    Some(s) if s <= STALE_AGENT_AFTER_SECS => Style::default().fg(Color::Yellow),
                    _ => Style::default().fg(Color::DarkGray),
                };
                let dot = match beat_age {
                    Some(s) if s <= STALE_AGENT_AFTER_SECS => "●",
                    _ => "○",
                };
                // UPTIME — grows monotonically from first heartbeat. Falls
                // back to last_seen if first_seen is missing (e.g. agent
                // existed before the migration ran).
                let first_seen_raw = a
                    .get("first_seen")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty() && !s.starts_with("1970"))
                    .unwrap_or(last_seen_raw);
                let uptime_str = parse_age(first_seen_raw, now)
                    .map(human_age)
                    .unwrap_or_else(|| "-".into());
                let id = a.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let doing = describe_doing(a, &state.tasks);
                Row::new(vec![dot.to_string(), id.to_string(), uptime_str, doing]).style(style)
            })
            .collect();
        let table = Table::new(
            rows,
            [
                Constraint::Length(2),
                Constraint::Length(15),
                Constraint::Length(7),
                Constraint::Min(8),
            ],
        )
        .header(
            Row::new(vec![" ", "ID", "UPTIME", "DOING"])
                .style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .block(agents_pane);
        f.render_widget(table, panes[0]);
    }

    // Tasks pane.
    let task_rows: Vec<Row> = visible_tasks
        .iter()
        .map(|t| {
            let state_str = t.get("state").and_then(|v| v.as_str()).unwrap_or("");
            let kind_str = t.get("kind").and_then(|v| v.as_str()).unwrap_or("task");
            let priority_str = t
                .get("priority")
                .and_then(|v| v.as_str())
                .unwrap_or("normal");
            let style = row_style(state_str, priority_str);
            let age_str = t
                .get("created_at")
                .and_then(|v| v.as_str())
                .and_then(|ts| parse_age(ts, now))
                .map(human_age)
                .unwrap_or_else(|| "-".into());
            Row::new(vec![
                t.get("id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.chars().take(8).collect::<String>())
                    .unwrap_or_default(),
                age_str,
                priority_glyph(priority_str).to_string(),
                kind_glyph(kind_str).to_string(),
                state_str.to_string(),
                t.get("claimed_by")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-")
                    .to_string(),
                t.get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ])
            .style(style)
        })
        .collect();

    let tasks_title = format!("tasks ({}/{})", visible_tasks.len(), state.tasks.len());
    let tasks_table = Table::new(
        task_rows,
        [
            Constraint::Length(8),
            Constraint::Length(6),
            Constraint::Length(10),
            Constraint::Length(13),
            Constraint::Length(9),
            Constraint::Length(16),
            Constraint::Min(18),
        ],
    )
    .header(
        Row::new(vec![
            "ID",
            "AGE",
            "PRIO",
            "KIND",
            "STATE",
            "CLAIMED_BY",
            "NAME",
        ])
        .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .block(Block::default().borders(Borders::ALL).title(tasks_title))
    .row_highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("▶ ");
    let mut ts = state.table_state.clone();
    f.render_stateful_widget(tasks_table, panes[1], &mut ts);

    // Detail pane.
    if show_detail_effective {
        let detail_lines = build_detail(visible_tasks.get(state.selected), now);
        let detail = Paragraph::new(detail_lines)
            .wrap(ratatui::widgets::Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title("detail"));
        f.render_widget(detail, panes[2]);
    }

    // Footer.
    let footer_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(chunks[2]);

    let status_line = if let Some(e) = state.last_err.as_deref() {
        Line::from(vec![
            Span::styled(
                "[error] ",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::raw(e.to_string()),
        ])
    } else if let Some((msg, _)) = state.status.as_ref() {
        Line::from(vec![
            Span::styled(
                "[ok] ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(msg.clone()),
        ])
    } else {
        Line::from(Span::styled("ready", Style::default().fg(Color::DarkGray)))
    };
    f.render_widget(Paragraph::new(status_line), footer_layout[0]);

    let hints = Line::from(vec![
        key_hint("q", "quit"),
        key_hint("↑↓", "select"),
        key_hint("d", "detail"),
        key_hint("A", "stale"),
        key_hint("p", "pause"),
        key_hint("r", "refresh"),
        key_hint("f", "filter"),
        key_hint("s", "send"),
        key_hint("c", "cancel"),
    ]);
    f.render_widget(Paragraph::new(hints), footer_layout[1]);
}

fn bracket_count(label: &str, n: usize, color: Color) -> Span<'static> {
    Span::styled(
        format!("{label}={n}"),
        if n > 0 {
            Style::default().fg(color)
        } else {
            Style::default().fg(Color::DarkGray)
        },
    )
}

/// One-liner for the agents pane's DOING column.
fn describe_doing(agent: &Value, tasks: &[Value]) -> String {
    if let Some(tid) = agent.get("current_task").and_then(|v| v.as_str()) {
        for t in tasks {
            if t.get("id").and_then(|v| v.as_str()) == Some(tid) {
                let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let glyph = match t.get("kind").and_then(|v| v.as_str()).unwrap_or("task") {
                    "bug" => "🐛",
                    "feature" => "✨",
                    _ => "→",
                };
                return format!("{glyph} {name}");
            }
        }
        let short: String = tid.chars().take(8).collect();
        return format!("→ {short}");
    }
    "waiting…".into()
}

fn partition_agents(agents: &[Value], now: DateTime<Utc>) -> (usize, usize, usize) {
    let mut active = 0;
    let mut idle = 0;
    let mut stale = 0;
    for a in agents {
        let ls = a.get("last_seen").and_then(|v| v.as_str()).unwrap_or("");
        match parse_age(ls, now) {
            Some(s) if s <= IDLE_AGENT_AFTER_SECS => active += 1,
            Some(s) if s <= STALE_AGENT_AFTER_SECS => idle += 1,
            _ => stale += 1,
        }
    }
    (active, idle, stale)
}

fn row_style(state: &str, priority: &str) -> Style {
    match (state, priority) {
        ("completed", _) => Style::default().fg(Color::Green),
        ("cancelled", _) => Style::default().fg(Color::DarkGray),
        ("failed", _) => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        (_, "urgent") => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        (_, "high") => Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
        ("pending", _) => Style::default().fg(Color::Yellow),
        ("claimed", _) => Style::default().fg(Color::Cyan),
        _ => Style::default(),
    }
}

fn priority_glyph(p: &str) -> &'static str {
    match p {
        "urgent" => "‼ urgent",
        "high" => "▲ high",
        "low" => "▽ low",
        _ => "  normal",
    }
}

fn kind_glyph(k: &str) -> String {
    match k {
        "bug" => "[B] bug".into(),
        "feature" => "[F] feature".into(),
        "decision" => "[D] decision".into(),
        "ack" => "[A] ack".into(),
        "knowledge" => "[K] knowledge".into(),
        "build" => "[X] build".into(),
        "task" => "[ ] task".into(),
        other => format!("[ ] {other}"),
    }
}

fn build_detail(task: Option<&Value>, now: DateTime<Utc>) -> Vec<Line<'static>> {
    let Some(t) = task else {
        return vec![Line::from(Span::styled(
            "no task selected",
            Style::default().fg(Color::DarkGray),
        ))];
    };

    let id = t.get("id").and_then(|v| v.as_str()).unwrap_or("?");
    let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let kind = t.get("kind").and_then(|v| v.as_str()).unwrap_or("task");
    let priority = t
        .get("priority")
        .and_then(|v| v.as_str())
        .unwrap_or("normal");
    let state = t.get("state").and_then(|v| v.as_str()).unwrap_or("");
    let claimed_by = t.get("claimed_by").and_then(|v| v.as_str()).unwrap_or("-");
    let created = t.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
    let updated = t.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");
    let created_age = parse_age(created, now)
        .map(human_age)
        .unwrap_or_else(|| "-".into());
    let updated_age = parse_age(updated, now)
        .map(human_age)
        .unwrap_or_else(|| "-".into());

    let kv = |k: &str, v: String, color: Color| -> Line<'static> {
        Line::from(vec![
            Span::styled(format!("{k:<10}"), Style::default().fg(Color::DarkGray)),
            Span::styled(v, Style::default().fg(color)),
        ])
    };

    let state_color = match state {
        "pending" => Color::Yellow,
        "claimed" => Color::Cyan,
        "completed" => Color::Green,
        "failed" => Color::Red,
        "cancelled" => Color::DarkGray,
        _ => Color::White,
    };
    let prio_color = match priority {
        "urgent" => Color::Red,
        "high" => Color::Magenta,
        "low" => Color::DarkGray,
        _ => Color::White,
    };

    let mut lines = vec![
        Line::from(Span::styled(
            name.to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
        kv("id", id.to_string(), Color::White),
        kv("kind", kind_glyph(kind), Color::White),
        kv("priority", priority.to_string(), prio_color),
        kv("state", state.to_string(), state_color),
        kv("claimed", claimed_by.to_string(), Color::White),
        kv(
            "age",
            format!("{created_age} (updated {updated_age})"),
            Color::DarkGray,
        ),
    ];

    if let Some(p) = t.get("payload") {
        if !p.is_null() {
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                "payload".to_string(),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )));
            for l in pretty_json_lines(p, 12) {
                lines.push(Line::raw(l));
            }
        }
    }
    if let Some(r) = t.get("result") {
        if !r.is_null() {
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                "result".to_string(),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )));
            for l in pretty_json_lines(r, 8) {
                lines.push(Line::raw(l));
            }
        }
    }

    lines
}

/// Pretty-print a JSON value, capped at `max_lines` so long payloads
/// don't blow out the detail pane.
fn pretty_json_lines(v: &Value, max_lines: usize) -> Vec<String> {
    let pretty = serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string());
    let mut out: Vec<String> = pretty.lines().take(max_lines).map(String::from).collect();
    if pretty.lines().count() > max_lines {
        out.push(format!(
            "… +{} more lines",
            pretty.lines().count() - max_lines
        ));
    }
    out
}

fn key_hint(key: &str, label: &str) -> Span<'static> {
    Span::raw(format!("[{key}] {label}  "))
}
