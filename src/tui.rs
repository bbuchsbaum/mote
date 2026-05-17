//! `mote ui`: read-only TUI for oversight of the op log.
//!
//! Like `mote watch`, this only ever calls `reducer::replay_store`. There is
//! no write path; the TUI is a passive viewer.

use std::cmp::Ordering;
use std::io;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use jiff::Timestamp;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use ratatui::Frame;
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap};
use ratatui::{Terminal, prelude::Stylize};

use crate::errors::{MoteError, MoteResult};
use crate::ids;
use crate::reducer;
use crate::repo::Store;
use crate::state::{Bead, State};

pub fn run(store: &Store, actor: Option<&str>) -> MoteResult<i32> {
    enable_raw_mode().map_err(io_err)?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).map_err(io_err)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).map_err(io_err)?;

    let outcome = event_loop(&mut terminal, store, actor);

    // Always restore the terminal, even on error.
    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();

    outcome
}

fn io_err(e: io::Error) -> MoteError {
    MoteError::Io(e)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Overview,
    Beads,
    Discussion,
    Activity,
}

const TABS: [Tab; 4] = [Tab::Overview, Tab::Beads, Tab::Discussion, Tab::Activity];
const TAB_TITLES: [&str; 4] = ["Overview", "Beads", "Discussion", "Activity"];

impl Tab {
    fn index(self) -> usize {
        TABS.iter().position(|t| *t == self).unwrap_or(0)
    }
}

struct App {
    actor: Option<String>,
    tab: Tab,
    state: Option<State>,
    last_refresh_ts: Option<String>,
    error: Option<String>,
    show_help: bool,
    store_root: String,

    // Per-tab selections.
    beads_state: ListState,
    topics_state: ListState,
    activity_state: ListState,
    discussion_scroll: u16,
    discussion_scroll_max: u16,
    discussion_page_rows: u16,

    // Derived caches refreshed on each replay.
    bead_ids: Vec<String>,
    topic_names: Vec<String>,
    activity: Vec<ActivityEntry>,
}

#[derive(Clone)]
struct ActivityEntry {
    op_id: String,
    kind: String,
    actor: String,
    ts: String,
    entity: Option<String>,
    accepted: bool,
    reason: Option<String>,
}

impl App {
    fn new(actor: Option<&str>, store: &Store) -> Self {
        Self {
            actor: actor.map(String::from),
            tab: Tab::Overview,
            state: None,
            last_refresh_ts: None,
            error: None,
            show_help: false,
            store_root: store.root().display().to_string(),
            beads_state: ListState::default(),
            topics_state: ListState::default(),
            activity_state: ListState::default(),
            discussion_scroll: 0,
            discussion_scroll_max: 0,
            discussion_page_rows: 1,
            bead_ids: Vec::new(),
            topic_names: Vec::new(),
            activity: Vec::new(),
        }
    }

    fn refresh(&mut self, store: &Store) {
        match reducer::replay_store(store) {
            Ok(state) => {
                self.bead_ids = state
                    .beads
                    .values()
                    .filter(|b| !b.is_deleted())
                    .map(|b| b.id.clone())
                    .collect();
                self.bead_ids.sort();

                self.topic_names = state.board_topics.keys().cloned().collect();
                self.topic_names.sort();

                let mut acts: Vec<ActivityEntry> = Vec::new();
                for (entity, entries) in &state.history {
                    for e in entries {
                        acts.push(ActivityEntry {
                            op_id: e.op_id.clone(),
                            kind: e.kind.clone(),
                            actor: e.actor.clone(),
                            ts: e.ts.clone(),
                            entity: Some(entity.clone()),
                            accepted: e.accepted,
                            reason: e.reason.clone(),
                        });
                    }
                }
                for e in &state.orphan_history {
                    acts.push(ActivityEntry {
                        op_id: e.op_id.clone(),
                        kind: e.kind.clone(),
                        actor: e.actor.clone(),
                        ts: e.ts.clone(),
                        entity: None,
                        accepted: e.accepted,
                        reason: e.reason.clone(),
                    });
                }
                acts.sort_by(|a, b| b.op_id.cmp(&a.op_id));
                acts.truncate(500);
                self.activity = acts;

                clamp_selection(&mut self.beads_state, self.bead_ids.len());
                clamp_selection(&mut self.topics_state, self.topic_names.len());
                clamp_selection(&mut self.activity_state, self.activity.len());
                if self.beads_state.selected().is_none() && !self.bead_ids.is_empty() {
                    self.beads_state.select(Some(0));
                }
                if self.topics_state.selected().is_none() && !self.topic_names.is_empty() {
                    self.topics_state.select(Some(0));
                }
                if self.activity_state.selected().is_none() && !self.activity.is_empty() {
                    self.activity_state.select(Some(0));
                }

                self.state = Some(state);
                self.last_refresh_ts = Some(ids::format_rfc3339(Timestamp::now()));
                self.error = None;
            }
            Err(e) => {
                self.error = Some(format!("replay failed: {e}"));
            }
        }
    }

    fn next_tab(&mut self) {
        let i = (self.tab.index() + 1) % TABS.len();
        self.tab = TABS[i];
    }

    fn prev_tab(&mut self) {
        let i = (self.tab.index() + TABS.len() - 1) % TABS.len();
        self.tab = TABS[i];
    }

    fn move_down(&mut self) {
        let reset_discussion_scroll = self.tab == Tab::Discussion;
        let old_selection = self.topics_state.selected();
        let (state, len) = self.current_list_mut();
        if len == 0 {
            return;
        }
        let next = match state.selected() {
            Some(i) if i + 1 < len => i + 1,
            Some(_) => len - 1,
            None => 0,
        };
        state.select(Some(next));
        if reset_discussion_scroll && self.topics_state.selected() != old_selection {
            self.discussion_scroll = 0;
        }
    }

    fn move_up(&mut self) {
        let reset_discussion_scroll = self.tab == Tab::Discussion;
        let old_selection = self.topics_state.selected();
        let (state, _) = self.current_list_mut();
        let next = match state.selected() {
            Some(i) if i > 0 => i - 1,
            _ => 0,
        };
        state.select(Some(next));
        if reset_discussion_scroll && self.topics_state.selected() != old_selection {
            self.discussion_scroll = 0;
        }
    }

    fn page_down(&mut self) {
        if self.tab == Tab::Discussion {
            self.scroll_discussion_down();
            return;
        }
        let (state, len) = self.current_list_mut();
        if len == 0 {
            return;
        }
        let next = state.selected().map(|i| i + 10).unwrap_or(0).min(len - 1);
        state.select(Some(next));
    }

    fn page_up(&mut self) {
        if self.tab == Tab::Discussion {
            self.scroll_discussion_up();
            return;
        }
        let (state, _) = self.current_list_mut();
        let next = state.selected().map(|i| i.saturating_sub(10)).unwrap_or(0);
        state.select(Some(next));
    }

    fn home(&mut self) {
        let reset_discussion_scroll = self.tab == Tab::Discussion;
        let old_selection = self.topics_state.selected();
        let (state, len) = self.current_list_mut();
        if len > 0 {
            state.select(Some(0));
        }
        if reset_discussion_scroll && self.topics_state.selected() != old_selection {
            self.discussion_scroll = 0;
        }
    }

    fn end(&mut self) {
        let reset_discussion_scroll = self.tab == Tab::Discussion;
        let old_selection = self.topics_state.selected();
        let (state, len) = self.current_list_mut();
        if len > 0 {
            state.select(Some(len - 1));
        }
        if reset_discussion_scroll && self.topics_state.selected() != old_selection {
            self.discussion_scroll = 0;
        }
    }

    fn scroll_discussion_down(&mut self) {
        let step = self.discussion_page_rows.max(1);
        self.discussion_scroll = self
            .discussion_scroll
            .saturating_add(step)
            .min(self.discussion_scroll_max);
    }

    fn scroll_discussion_up(&mut self) {
        let step = self.discussion_page_rows.max(1);
        self.discussion_scroll = self.discussion_scroll.saturating_sub(step);
    }

    fn scroll_discussion_top(&mut self) {
        self.discussion_scroll = 0;
    }

    fn scroll_discussion_bottom(&mut self) {
        self.discussion_scroll = self.discussion_scroll_max;
    }

    fn current_list_mut(&mut self) -> (&mut ListState, usize) {
        match self.tab {
            Tab::Beads => (&mut self.beads_state, self.bead_ids.len()),
            Tab::Discussion => (&mut self.topics_state, self.topic_names.len()),
            Tab::Activity => (&mut self.activity_state, self.activity.len()),
            Tab::Overview => (&mut self.beads_state, 0),
        }
    }
}

fn clamp_selection(state: &mut ListState, len: usize) {
    if len == 0 {
        state.select(None);
    } else if let Some(i) = state.selected() {
        if i >= len {
            state.select(Some(len - 1));
        }
    }
}

fn event_loop<B: Backend>(
    terminal: &mut Terminal<B>,
    store: &Store,
    actor: Option<&str>,
) -> MoteResult<i32> {
    let (tx, rx) = channel::<()>();
    let tx_fs = tx.clone();
    let mut watcher: RecommendedWatcher =
        notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res {
                if matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                ) {
                    let _ = tx_fs.send(());
                }
            }
        })
        .map_err(|e| MoteError::Other(format!("ui: install watcher: {e}")))?;
    watcher
        .watch(&store.ops_dir(), RecursiveMode::NonRecursive)
        .map_err(|e| MoteError::Other(format!("ui: subscribe ops_dir: {e}")))?;

    let mut app = App::new(actor, store);
    app.refresh(store);
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|f| render(f, &mut app)).map_err(io_err)?;

        let mut needs_refresh = drain(&rx);
        let timeout = Duration::from_millis(200);
        if event::poll(timeout).map_err(io_err)? {
            match event::read().map_err(io_err)? {
                Event::Key(key) => {
                    if key.kind == KeyEventKind::Release {
                        continue;
                    }
                    if app.show_help {
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Esc | KeyCode::Char('?') => {
                                app.show_help = false;
                            }
                            _ => {}
                        }
                        continue;
                    }
                    match (key.code, key.modifiers) {
                        (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => return Ok(0),
                        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return Ok(0),
                        (KeyCode::Tab, _) => app.next_tab(),
                        (KeyCode::BackTab, _) => app.prev_tab(),
                        (KeyCode::Char('1'), _) => app.tab = Tab::Overview,
                        (KeyCode::Char('2'), _) => app.tab = Tab::Beads,
                        (KeyCode::Char('3'), _) => app.tab = Tab::Discussion,
                        (KeyCode::Char('4'), _) => app.tab = Tab::Activity,
                        (KeyCode::Down, _) | (KeyCode::Char('j'), _) => app.move_down(),
                        (KeyCode::Up, _) | (KeyCode::Char('k'), _) => app.move_up(),
                        (KeyCode::PageDown, _) => app.page_down(),
                        (KeyCode::PageUp, _) => app.page_up(),
                        (KeyCode::Home, _) if app.tab == Tab::Discussion => {
                            app.scroll_discussion_top()
                        }
                        (KeyCode::End, _) if app.tab == Tab::Discussion => {
                            app.scroll_discussion_bottom()
                        }
                        (KeyCode::Home, _) | (KeyCode::Char('g'), _) => app.home(),
                        (KeyCode::End, _) | (KeyCode::Char('G'), _) => app.end(),
                        (KeyCode::Char('r'), _) => needs_refresh = true,
                        (KeyCode::Char('?'), _) | (KeyCode::Char('h'), _) => {
                            app.show_help = true;
                        }
                        _ => {}
                    }
                }
                Event::Resize(_, _) => {}
                _ => {}
            }
        }

        if last_tick.elapsed() >= Duration::from_secs(2) {
            needs_refresh = true;
        }
        if needs_refresh {
            app.refresh(store);
            last_tick = Instant::now();
        }
    }
}

fn drain(rx: &Receiver<()>) -> bool {
    let mut any = false;
    while rx.try_recv().is_ok() {
        any = true;
    }
    any
}

fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header / tabs
            Constraint::Min(1),    // body
            Constraint::Length(1), // status bar
        ])
        .split(area);

    render_header(f, app, chunks[0]);
    render_body(f, app, chunks[1]);
    render_footer(f, app, chunks[2]);

    if app.show_help {
        render_help(f, area);
    }
}

fn render_header(f: &mut Frame, app: &App, area: Rect) {
    let titles: Vec<Line> = TAB_TITLES
        .iter()
        .enumerate()
        .map(|(i, t)| Line::from(format!(" {}. {} ", i + 1, t)))
        .collect();
    let actor_label = app
        .actor
        .as_deref()
        .map(|a| format!("actor: {a}"))
        .unwrap_or_else(|| "actor: -".into());
    let title = format!(" mote ui — {}  |  {}  ", app.store_root, actor_label);
    let tabs = Tabs::new(titles)
        .block(Block::default().borders(Borders::ALL).title(title))
        .select(app.tab.index())
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(tabs, area);
}

fn render_footer(f: &mut Frame, app: &App, area: Rect) {
    let mut spans: Vec<Span> = Vec::new();
    let scroll_hint = if app.tab == Tab::Discussion {
        " · PgUp/PgDn scroll posts"
    } else {
        " · PgUp/PgDn page"
    };
    spans.push(Span::raw(format!(
        "  q quit · Tab next · 1-4 jump · j/k move{scroll_hint} · r refresh · ? help"
    )));
    if let Some(err) = &app.error {
        spans.push(Span::raw("  |  "));
        spans.push(Span::styled(
            err.clone(),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
    } else if let Some(ts) = &app.last_refresh_ts {
        spans.push(Span::raw("  |  last refresh "));
        spans.push(Span::styled(
            ts.clone(),
            Style::default().fg(Color::DarkGray),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_body(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(state) = app.state.clone() else {
        let msg = app.error.clone().unwrap_or_else(|| "loading…".into());
        f.render_widget(Paragraph::new(msg).alignment(Alignment::Center), area);
        return;
    };
    match app.tab {
        Tab::Overview => render_overview(f, app, &state, area),
        Tab::Beads => render_beads(f, app, &state, area),
        Tab::Discussion => render_discussion(f, app, &state, area),
        Tab::Activity => render_activity(f, app, &state, area),
    }
}

fn render_overview(f: &mut Frame, app: &App, state: &State, area: Rect) {
    use std::collections::BTreeMap;
    let now = ids::format_rfc3339(Timestamp::now());

    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for b in state.live_beads() {
        *counts.entry(b.status.as_str()).or_insert(0) += 1;
    }
    let total_beads: usize = counts.values().sum();
    let active_claims: Vec<&Bead> = state
        .live_beads()
        .filter(|b| b.claim.as_ref().is_some_and(|c| c.is_live(&now)))
        .collect();
    let active_reservations: Vec<_> = state
        .reservations
        .values()
        .filter(|r| r.is_active(&now))
        .collect();
    let inbox = app
        .actor
        .as_deref()
        .map(|a| state.inbox_for(a).len())
        .unwrap_or(0);
    let unread = app
        .actor
        .as_deref()
        .map(|a| state.unread_board_posts_for(a, None).len())
        .unwrap_or(0);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6),
            Constraint::Min(3),
            Constraint::Min(3),
        ])
        .split(area);

    let status_line: Vec<Span> = if counts.is_empty() {
        vec![Span::styled(
            "(no beads)",
            Style::default().fg(Color::DarkGray),
        )]
    } else {
        counts
            .iter()
            .flat_map(|(k, v)| {
                vec![
                    Span::styled(format!(" {k}"), Style::default().fg(status_color(k))),
                    Span::raw(format!("={v}  ")),
                ]
            })
            .collect()
    };

    let summary_lines = vec![
        Line::from(vec![
            Span::raw("beads:        "),
            Span::styled(total_beads.to_string(), Style::default().bold()),
        ]),
        Line::from(status_line),
        Line::from(vec![
            Span::raw("claims:       "),
            Span::styled(active_claims.len().to_string(), Style::default().bold()),
            Span::raw("    reservations: "),
            Span::styled(
                active_reservations.len().to_string(),
                Style::default().bold(),
            ),
        ]),
        Line::from(vec![
            Span::raw("inbox:        "),
            Span::styled(inbox.to_string(), Style::default().bold()),
            Span::raw("    discussion unread: "),
            Span::styled(unread.to_string(), Style::default().bold()),
        ]),
    ];
    f.render_widget(
        Paragraph::new(summary_lines)
            .block(Block::default().borders(Borders::ALL).title("Summary")),
        chunks[0],
    );

    let claim_items: Vec<ListItem> = if active_claims.is_empty() {
        vec![ListItem::new(Span::styled(
            "(none)",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        active_claims
            .iter()
            .map(|b| {
                let holder = b
                    .claim
                    .as_ref()
                    .map(|c| c.claimed_by.as_str())
                    .unwrap_or("?");
                let lease = b
                    .claim
                    .as_ref()
                    .map(|c| c.lease_until_ts.as_str())
                    .unwrap_or("");
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{:>16}", b.id), Style::default().fg(Color::Cyan)),
                    Span::raw("  "),
                    Span::styled(
                        format!("{:>7}", b.status.as_str()),
                        Style::default().fg(status_color(b.status.as_str())),
                    ),
                    Span::raw("  by "),
                    Span::styled(holder.to_string(), Style::default().fg(Color::Yellow)),
                    Span::raw("  until "),
                    Span::styled(lease.to_string(), Style::default().fg(Color::DarkGray)),
                    Span::raw("  "),
                    Span::raw(truncate(&b.title, 40)),
                ]))
            })
            .collect()
    };
    f.render_widget(
        List::new(claim_items).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Active claims"),
        ),
        chunks[1],
    );

    let rv_items: Vec<ListItem> = if active_reservations.is_empty() {
        vec![ListItem::new(Span::styled(
            "(none)",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        active_reservations
            .iter()
            .map(|r| {
                let paths = r.live_paths().join(", ");
                ListItem::new(Line::from(vec![
                    Span::styled(r.reservation_id.clone(), Style::default().fg(Color::Cyan)),
                    Span::raw("  by "),
                    Span::styled(r.actor.clone(), Style::default().fg(Color::Yellow)),
                    Span::raw("  on "),
                    Span::raw(r.entity.clone()),
                    Span::raw("  "),
                    Span::raw(truncate(&paths, 60)),
                ]))
            })
            .collect()
    };
    f.render_widget(
        List::new(rv_items).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Active reservations"),
        ),
        chunks[2],
    );
}

fn status_color(s: &str) -> Color {
    match s {
        "open" => Color::Green,
        "doing" => Color::Yellow,
        "blocked" => Color::Red,
        "review" => Color::Magenta,
        "closed" => Color::DarkGray,
        _ => Color::White,
    }
}

fn render_beads(f: &mut Frame, app: &mut App, state: &State, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Min(20)])
        .split(area);

    let items: Vec<ListItem> = app
        .bead_ids
        .iter()
        .filter_map(|id| state.beads.get(id))
        .map(|b| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:>7}", b.status.as_str()),
                    Style::default().fg(status_color(b.status.as_str())),
                ),
                Span::raw(" "),
                Span::styled(
                    format!("p{}", b.priority),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(" "),
                Span::styled(b.id.clone(), Style::default().fg(Color::Cyan)),
                Span::raw("  "),
                Span::raw(truncate(&b.title, 40)),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("Beads ({})", app.bead_ids.len())),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, chunks[0], &mut app.beads_state);

    let detail = match app.beads_state.selected().and_then(|i| app.bead_ids.get(i)) {
        Some(id) => state.beads.get(id).map(|b| bead_detail_lines(state, b)),
        None => None,
    };
    let lines = detail.unwrap_or_else(|| vec![Line::from("(no selection)")]);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title("Detail")),
        chunks[1],
    );
}

fn bead_detail_lines(state: &State, b: &Bead) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(b.id.clone(), Style::default().fg(Color::Cyan).bold()),
        Span::raw("  "),
        Span::styled(
            format!("[{}]", b.status.as_str()),
            Style::default().fg(status_color(b.status.as_str())),
        ),
        Span::raw(format!(" p{}", b.priority)),
    ]));
    lines.push(Line::from(b.title.clone()));
    if let Some(a) = &b.assignee {
        lines.push(Line::from(format!("assignee: {a}")));
    }
    if !b.tags.is_empty() {
        let tags = b.tags.iter().cloned().collect::<Vec<_>>().join(", ");
        lines.push(Line::from(format!("tags: {tags}")));
    }
    if !b.deps.is_empty() {
        let deps = b
            .deps
            .iter()
            .map(|(p, k)| format!("{p}({k})"))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(Line::from(format!("deps: {deps}")));
    }
    if let Some(c) = &b.claim {
        lines.push(Line::from(vec![
            Span::raw("claim: "),
            Span::styled(c.claimed_by.clone(), Style::default().fg(Color::Yellow)),
            Span::raw(" until "),
            Span::styled(
                c.lease_until_ts.clone(),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }
    if !b.body.is_empty() {
        lines.push(Line::from(""));
        for body_line in b.body.lines() {
            lines.push(Line::from(body_line.to_string()));
        }
    }
    if !b.notes.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Notes:",
            Style::default().add_modifier(Modifier::UNDERLINED),
        )));
        for n in b.notes.iter().rev().take(15) {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("[{}]", n.note_kind),
                    Style::default().fg(Color::Magenta),
                ),
                Span::raw(" "),
                Span::styled(n.actor.clone(), Style::default().fg(Color::Yellow)),
                Span::raw(" "),
                Span::styled(n.ts.clone(), Style::default().fg(Color::DarkGray)),
            ]));
            for body_line in n.text.lines() {
                lines.push(Line::from(format!("  {body_line}")));
            }
        }
    }
    // Reservations on this bead
    let rvs: Vec<_> = state
        .reservations
        .values()
        .filter(|r| r.entity == b.id && !r.live_paths().is_empty())
        .collect();
    if !rvs.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Reservations:",
            Style::default().add_modifier(Modifier::UNDERLINED),
        )));
        for r in rvs {
            lines.push(Line::from(format!(
                "  {} by {} — {}",
                r.reservation_id,
                r.actor,
                r.live_paths().join(", ")
            )));
        }
    }
    lines
}

fn render_discussion(f: &mut Frame, app: &mut App, state: &State, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Min(20)])
        .split(area);

    let mut topics: Vec<&crate::state::BoardTopicRecord> = state.board_topics.values().collect();
    topics.sort_by(
        |a, b| match b.last_activity_op_id.cmp(&a.last_activity_op_id) {
            Ordering::Equal => a.topic.cmp(&b.topic),
            other => other,
        },
    );

    let items: Vec<ListItem> = topics
        .iter()
        .map(|t| {
            let mark = if t.explicit { "★" } else { " " };
            ListItem::new(Line::from(vec![
                Span::styled(mark.to_string(), Style::default().fg(Color::Yellow)),
                Span::raw(" "),
                Span::styled(t.topic.clone(), Style::default().fg(Color::Cyan)),
                Span::raw("  "),
                Span::styled(
                    format!("posts={} sticky={}", t.post_count, t.sticky_count),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("Topics ({})", topics.len())),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, chunks[0], &mut app.topics_state);

    let selected_topic = app
        .topics_state
        .selected()
        .and_then(|i| topics.get(i))
        .map(|t| t.topic.clone());

    let posts_lines = match selected_topic {
        Some(topic) => discussion_post_lines(state, &topic),
        None => vec![Line::from("(no topic selected)")],
    };
    let visible_rows = chunks[1].height.saturating_sub(2).max(1);
    app.discussion_page_rows = visible_rows;
    app.discussion_scroll_max = posts_lines
        .len()
        .saturating_sub(visible_rows as usize)
        .min(u16::MAX as usize) as u16;
    app.discussion_scroll = app.discussion_scroll.min(app.discussion_scroll_max);

    let title = match app.topics_state.selected().and_then(|i| topics.get(i)) {
        Some(t) if app.discussion_scroll_max > 0 => format!(
            "Posts in {} — {}  scroll {}/{}",
            t.topic, t.title, app.discussion_scroll, app.discussion_scroll_max
        ),
        Some(t) => format!("Posts in {} — {}", t.topic, t.title),
        None => "Posts".to_string(),
    };
    f.render_widget(
        Paragraph::new(posts_lines)
            .wrap(Wrap { trim: false })
            .scroll((app.discussion_scroll, 0))
            .block(Block::default().borders(Borders::ALL).title(title)),
        chunks[1],
    );
}

fn discussion_post_lines(state: &State, topic: &str) -> Vec<Line<'static>> {
    let posts = state.board_posts_for(Some(topic));
    if posts.is_empty() {
        return vec![Line::from(Span::styled(
            "(no posts)",
            Style::default().fg(Color::DarkGray),
        ))];
    }
    let mut lines: Vec<Line> = Vec::new();
    for p in posts {
        let sticky = if p.sticky { " ★" } else { "" };
        lines.push(Line::from(vec![
            Span::styled(p.post_id.clone(), Style::default().fg(Color::Cyan)),
            Span::styled(sticky.to_string(), Style::default().fg(Color::Yellow)),
            Span::raw("  "),
            Span::styled(p.from.clone(), Style::default().fg(Color::Yellow)),
            Span::raw("  "),
            Span::styled(p.sent_ts.clone(), Style::default().fg(Color::DarkGray)),
            Span::raw(if p.reply_to.is_some() { "  ↪" } else { "" }),
        ]));
        for body_line in p.body.lines() {
            lines.push(Line::from(format!("  {body_line}")));
        }
        lines.push(Line::from(""));
    }
    lines
}

fn render_activity(f: &mut Frame, app: &mut App, _state: &State, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Min(20)])
        .split(area);

    let items: Vec<ListItem> = app
        .activity
        .iter()
        .map(|e| {
            let status_span = if e.accepted {
                Span::styled(" ok ", Style::default().fg(Color::Green))
            } else {
                Span::styled(" REJ", Style::default().fg(Color::Red).bold())
            };
            let entity = e.entity.as_deref().unwrap_or("-");
            ListItem::new(Line::from(vec![
                status_span,
                Span::raw(" "),
                Span::styled(
                    format!("{:<14}", e.kind),
                    Style::default().fg(Color::Magenta),
                ),
                Span::raw(" "),
                Span::styled(
                    format!("{:<10}", truncate(&e.actor, 10)),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw(" "),
                Span::styled(
                    format!("{:<14}", truncate(entity, 14)),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw(" "),
                Span::styled(e.ts.clone(), Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("Activity ({} most recent)", app.activity.len())),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, chunks[0], &mut app.activity_state);

    let detail_lines = match app
        .activity_state
        .selected()
        .and_then(|i| app.activity.get(i))
    {
        Some(e) => activity_detail_lines(e),
        None => vec![Line::from("(no selection)")],
    };
    f.render_widget(
        Paragraph::new(detail_lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title("Op detail")),
        chunks[1],
    );
}

fn activity_detail_lines(e: &ActivityEntry) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(vec![
            Span::styled("op:     ", Style::default().fg(Color::DarkGray)),
            Span::styled(e.op_id.clone(), Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::styled("kind:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(e.kind.clone(), Style::default().fg(Color::Magenta)),
        ]),
        Line::from(vec![
            Span::styled("actor:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(e.actor.clone(), Style::default().fg(Color::Yellow)),
        ]),
        Line::from(vec![
            Span::styled("ts:     ", Style::default().fg(Color::DarkGray)),
            Span::raw(e.ts.clone()),
        ]),
        Line::from(vec![
            Span::styled("entity: ", Style::default().fg(Color::DarkGray)),
            Span::raw(e.entity.clone().unwrap_or_else(|| "-".into())),
        ]),
        Line::from(vec![
            Span::styled("state:  ", Style::default().fg(Color::DarkGray)),
            if e.accepted {
                Span::styled("accepted", Style::default().fg(Color::Green))
            } else {
                Span::styled("rejected", Style::default().fg(Color::Red).bold())
            },
        ]),
    ];
    if let Some(r) = &e.reason {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "reason:",
            Style::default().add_modifier(Modifier::UNDERLINED),
        )));
        for body_line in r.lines() {
            lines.push(Line::from(body_line.to_string()));
        }
    }
    lines
}

fn render_help(f: &mut Frame, area: Rect) {
    let w = area.width.saturating_sub(20).min(60);
    let h = 14u16.min(area.height.saturating_sub(4));
    let x = area.x + (area.width - w) / 2;
    let y = area.y + (area.height - h) / 2;
    let popup = Rect {
        x,
        y,
        width: w,
        height: h,
    };

    let lines = vec![
        Line::from(Span::styled(
            "mote ui — read-only oversight",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("q, Esc        quit"),
        Line::from("Tab / S-Tab   next / prev tab"),
        Line::from("1 2 3 4       jump to tab"),
        Line::from("j/k or ↑/↓    move selection"),
        Line::from("g / G         top / bottom of selected list"),
        Line::from("PgUp / PgDn   page list; in Discussion, scroll posts"),
        Line::from("Home / End    in Discussion, top / bottom of posts"),
        Line::from("r             force refresh now"),
        Line::from("? / h         show / hide help"),
        Line::from(""),
        Line::from(Span::styled(
            "press any key to dismiss",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" Help ")),
        popup,
    );
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::BoardPostRecord;

    fn test_app() -> App {
        App {
            actor: None,
            tab: Tab::Discussion,
            state: None,
            last_refresh_ts: None,
            error: None,
            show_help: false,
            store_root: ".mote".into(),
            beads_state: ListState::default(),
            topics_state: ListState::default(),
            activity_state: ListState::default(),
            discussion_scroll: 0,
            discussion_scroll_max: 0,
            discussion_page_rows: 1,
            bead_ids: Vec::new(),
            topic_names: Vec::new(),
            activity: Vec::new(),
        }
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn discussion_page_keys_scroll_post_pane_without_moving_topic_selection() {
        let mut app = test_app();
        app.topic_names = vec!["alpha".into(), "beta".into()];
        app.topics_state.select(Some(1));
        app.discussion_page_rows = 5;
        app.discussion_scroll_max = 12;

        app.page_down();
        assert_eq!(app.topics_state.selected(), Some(1));
        assert_eq!(app.discussion_scroll, 5);

        app.page_down();
        app.page_down();
        assert_eq!(app.discussion_scroll, 12);

        app.page_up();
        assert_eq!(app.discussion_scroll, 7);

        app.scroll_discussion_top();
        assert_eq!(app.discussion_scroll, 0);
        app.scroll_discussion_bottom();
        assert_eq!(app.discussion_scroll, 12);
    }

    #[test]
    fn changing_discussion_topic_resets_post_scroll() {
        let mut app = test_app();
        app.topic_names = vec!["alpha".into(), "beta".into()];
        app.topics_state.select(Some(0));
        app.discussion_scroll = 8;
        app.discussion_scroll_max = 20;

        app.move_down();
        assert_eq!(app.topics_state.selected(), Some(1));
        assert_eq!(app.discussion_scroll, 0);
    }

    #[test]
    fn discussion_post_lines_include_full_bodies_and_all_posts() {
        let mut state = State::default();
        for i in 0..61 {
            let body = if i == 0 {
                (1..=6)
                    .map(|line| format!("line {line}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                format!("body {i}")
            };
            let post_id = format!("post-{i:03}");
            state.board_posts.insert(
                post_id.clone(),
                BoardPostRecord {
                    post_id,
                    from: "alice".into(),
                    topic: "planning".into(),
                    body,
                    reply_to: None,
                    sticky: false,
                    sticky_op_id: None,
                    sent_ts: format!("2026-05-14T00:00:{i:02}Z"),
                    sent_op_id: format!("op-{i:03}"),
                },
            );
        }

        let lines = discussion_post_lines(&state, "planning");
        let text = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(text.contains("line 6"), "body text was truncated:\n{text}");
        assert!(
            text.contains("post-060"),
            "post list was truncated:\n{text}"
        );
    }
}
