//! `mote ui`: read-only TUI for oversight of the op log.
//!
//! Like `mote watch`, this only ever calls `reducer::replay_store`. There is
//! no write path; the TUI is a passive viewer.

use std::collections::{HashMap, HashSet};
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
    Candidates,
    Discussion,
    Activity,
    Agents,
}

const TABS: [Tab; 6] = [
    Tab::Overview,
    Tab::Beads,
    Tab::Candidates,
    Tab::Discussion,
    Tab::Activity,
    Tab::Agents,
];
const TAB_TITLES: [&str; 6] = [
    "Overview",
    "Beads",
    "Candidates",
    "Discussion",
    "Activity",
    "Agents",
];

impl Tab {
    fn index(self) -> usize {
        TABS.iter().position(|t| *t == self).unwrap_or(0)
    }
}

/// Which pane of the Discussion tab has the keyboard.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DiscussionFocus {
    Topics,
    Posts,
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
    candidates_state: ListState,
    topics_state: ListState,
    activity_state: ListState,
    agents_state: ListState,
    discussion_scroll: u16,
    discussion_scroll_max: u16,
    discussion_page_rows: u16,
    discussion_focus: DiscussionFocus,
    /// Index of the highlighted post within the selected topic.
    post_cursor: usize,
    /// Row offset of every post in the currently rendered thread, in render
    /// order. Refreshed on each draw; key handling jumps by these offsets.
    post_starts: Vec<u16>,
    post_unread: Vec<bool>,

    // Derived caches refreshed on each replay.
    bead_ids: Vec<String>,
    candidate_ids: Vec<String>,
    topic_names: Vec<String>,
    activity: Vec<ActivityEntry>,
    actor_names: Vec<String>,
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
            candidates_state: ListState::default(),
            topics_state: ListState::default(),
            activity_state: ListState::default(),
            agents_state: ListState::default(),
            discussion_scroll: 0,
            discussion_scroll_max: 0,
            discussion_page_rows: 1,
            discussion_focus: DiscussionFocus::Topics,
            post_cursor: 0,
            post_starts: Vec::new(),
            post_unread: Vec::new(),
            bead_ids: Vec::new(),
            candidate_ids: Vec::new(),
            topic_names: Vec::new(),
            activity: Vec::new(),
            actor_names: Vec::new(),
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

                self.candidate_ids = state.candidates.keys().cloned().collect();

                self.topic_names = state.board_topics.keys().cloned().collect();
                self.topic_names.sort();

                self.actor_names = crate::actor_status::known_actor_names(&state)
                    .into_iter()
                    .collect();
                if let Some(actor) = &self.actor {
                    self.actor_names.push(actor.clone());
                    self.actor_names.sort();
                    self.actor_names.dedup();
                }

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
                clamp_selection(&mut self.candidates_state, self.candidate_ids.len());
                clamp_selection(&mut self.topics_state, self.topic_names.len());
                clamp_selection(&mut self.activity_state, self.activity.len());
                clamp_selection(&mut self.agents_state, self.actor_names.len());
                if self.beads_state.selected().is_none() && !self.bead_ids.is_empty() {
                    self.beads_state.select(Some(0));
                }
                if self.candidates_state.selected().is_none() && !self.candidate_ids.is_empty() {
                    self.candidates_state.select(Some(0));
                }
                if self.topics_state.selected().is_none() && !self.topic_names.is_empty() {
                    self.topics_state.select(Some(0));
                }
                if self.activity_state.selected().is_none() && !self.activity.is_empty() {
                    self.activity_state.select(Some(0));
                }
                if self.agents_state.selected().is_none() && !self.actor_names.is_empty() {
                    self.agents_state.select(Some(0));
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
        if self.posts_focused() {
            self.next_post();
            return;
        }
        let before = self.topics_state.selected();
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
        self.reset_posts_if_topic_changed(before);
    }

    fn move_up(&mut self) {
        if self.posts_focused() {
            self.prev_post();
            return;
        }
        let before = self.topics_state.selected();
        let (state, _) = self.current_list_mut();
        let next = match state.selected() {
            Some(i) if i > 0 => i - 1,
            _ => 0,
        };
        state.select(Some(next));
        self.reset_posts_if_topic_changed(before);
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
        if self.posts_focused() {
            self.select_post(0);
            return;
        }
        let before = self.topics_state.selected();
        let (state, len) = self.current_list_mut();
        if len > 0 {
            state.select(Some(0));
        }
        self.reset_posts_if_topic_changed(before);
    }

    fn end(&mut self) {
        if self.posts_focused() {
            self.select_post(self.post_starts.len().saturating_sub(1));
            return;
        }
        let before = self.topics_state.selected();
        let (state, len) = self.current_list_mut();
        if len > 0 {
            state.select(Some(len - 1));
        }
        self.reset_posts_if_topic_changed(before);
    }

    fn posts_focused(&self) -> bool {
        self.tab == Tab::Discussion && self.discussion_focus == DiscussionFocus::Posts
    }

    fn focus_posts(&mut self) {
        if self.tab == Tab::Discussion {
            self.discussion_focus = DiscussionFocus::Posts;
        }
    }

    fn focus_topics(&mut self) {
        if self.tab == Tab::Discussion {
            self.discussion_focus = DiscussionFocus::Topics;
        }
    }

    /// A new topic means a new thread: rewind to its first post.
    fn reset_posts_if_topic_changed(&mut self, before: Option<usize>) {
        if self.tab == Tab::Discussion && self.topics_state.selected() != before {
            self.discussion_scroll = 0;
            self.post_cursor = 0;
            self.post_starts.clear();
            self.post_unread.clear();
        }
    }

    /// Put post `idx` at the top of the reading pane and highlight it.
    fn select_post(&mut self, idx: usize) {
        if self.post_starts.is_empty() {
            self.post_cursor = 0;
            return;
        }
        let idx = idx.min(self.post_starts.len() - 1);
        self.post_cursor = idx;
        self.discussion_scroll = self.post_starts[idx].min(self.discussion_scroll_max);
    }

    fn next_post(&mut self) {
        if self.post_starts.is_empty() {
            return;
        }
        self.select_post(self.post_cursor.saturating_add(1));
    }

    fn prev_post(&mut self) {
        if self.post_starts.is_empty() {
            return;
        }
        self.select_post(self.post_cursor.saturating_sub(1));
    }

    /// Next post the current actor has not read yet, wrapping around.
    fn next_unread_post(&mut self) {
        if self.post_unread.is_empty() {
            return;
        }
        let n = self.post_unread.len();
        for step in 1..=n {
            let idx = (self.post_cursor + step) % n;
            if self.post_unread[idx] {
                self.select_post(idx);
                self.focus_posts();
                return;
            }
        }
    }

    /// Free scrolling moves the highlight to the topmost visible post so the
    /// cursor never drifts away from what is on screen.
    fn sync_cursor_to_scroll(&mut self) {
        if self.post_starts.is_empty() {
            return;
        }
        let scroll = self.discussion_scroll;
        self.post_cursor = self
            .post_starts
            .iter()
            .rposition(|start| *start <= scroll)
            .unwrap_or(0);
    }

    fn scroll_discussion_down(&mut self) {
        let step = self.discussion_page_rows.max(1);
        self.discussion_scroll = self
            .discussion_scroll
            .saturating_add(step)
            .min(self.discussion_scroll_max);
        self.sync_cursor_to_scroll();
    }

    fn scroll_discussion_up(&mut self) {
        let step = self.discussion_page_rows.max(1);
        self.discussion_scroll = self.discussion_scroll.saturating_sub(step);
        self.sync_cursor_to_scroll();
    }

    fn current_list_mut(&mut self) -> (&mut ListState, usize) {
        match self.tab {
            Tab::Beads => (&mut self.beads_state, self.bead_ids.len()),
            Tab::Candidates => (&mut self.candidates_state, self.candidate_ids.len()),
            Tab::Discussion => (&mut self.topics_state, self.topic_names.len()),
            Tab::Activity => (&mut self.activity_state, self.activity.len()),
            Tab::Agents => (&mut self.agents_state, self.actor_names.len()),
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
                        (KeyCode::Char('3'), _) => app.tab = Tab::Candidates,
                        (KeyCode::Char('4'), _) => app.tab = Tab::Discussion,
                        (KeyCode::Char('5'), _) => app.tab = Tab::Activity,
                        (KeyCode::Char('6'), _) => app.tab = Tab::Agents,
                        (KeyCode::Right, _) | (KeyCode::Enter, _) if app.tab == Tab::Discussion => {
                            app.focus_posts()
                        }
                        (KeyCode::Left, _) if app.tab == Tab::Discussion => app.focus_topics(),
                        (KeyCode::Char('n'), _) if app.tab == Tab::Discussion => {
                            app.focus_posts();
                            app.next_post()
                        }
                        (KeyCode::Char('p'), _) if app.tab == Tab::Discussion => {
                            app.focus_posts();
                            app.prev_post()
                        }
                        (KeyCode::Char('u'), _) if app.tab == Tab::Discussion => {
                            app.next_unread_post()
                        }
                        (KeyCode::Down, _) | (KeyCode::Char('j'), _) => app.move_down(),
                        (KeyCode::Up, _) | (KeyCode::Char('k'), _) => app.move_up(),
                        (KeyCode::PageDown, _) => app.page_down(),
                        (KeyCode::PageUp, _) => app.page_up(),
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
    if app.tab == Tab::Discussion {
        spans.push(Span::raw(
            "  q quit · 1-6 tab · ←/→ pane · j/k or n/p post · u unread · PgUp/PgDn scroll · ? help"
                .to_string(),
        ));
    } else {
        spans.push(Span::raw(
            "  q quit · Tab next · 1-6 jump · j/k move · PgUp/PgDn page · r refresh · ? help"
                .to_string(),
        ));
    }
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
        Tab::Candidates => render_candidates(f, app, &state, area),
        Tab::Discussion => render_discussion(f, app, &state, area),
        Tab::Activity => render_activity(f, app, &state, area),
        Tab::Agents => render_agents(f, app, &state, area),
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
        .filter(|b| state.claim_disposition(b, &now) == crate::state::LeaseDisposition::Active)
        .collect();
    let active_reservations: Vec<_> = state
        .reservations
        .values()
        .filter(|r| {
            state.reservation_disposition(r, &now) == crate::state::LeaseDisposition::Active
        })
        .collect();
    let orphaned_claims: Vec<&Bead> = state
        .beads
        .values()
        .filter(|b| state.claim_disposition(b, &now) == crate::state::LeaseDisposition::Orphaned)
        .collect();
    let orphaned_reservations: Vec<_> = state
        .reservations
        .values()
        .filter(|r| {
            state.reservation_disposition(r, &now) == crate::state::LeaseDisposition::Orphaned
        })
        .collect();
    let expiring_reservations: Vec<_> = state
        .reservations
        .values()
        .filter(|reservation| {
            state.reservation_expiry_phase(reservation, &now)
                == Some(crate::state::ReservationExpiryPhase::Expiring)
        })
        .collect();
    let expired_reservations: Vec<_> = state
        .reservations
        .values()
        .filter(|reservation| {
            state.reservation_expiry_phase(reservation, &now)
                == Some(crate::state::ReservationExpiryPhase::Expired)
        })
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
            Span::raw("    candidates: "),
            Span::styled(state.candidates.len().to_string(), Style::default().bold()),
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

    let mut orphan_items: Vec<ListItem> = orphaned_claims
        .iter()
        .map(|b| {
            let claim = b.claim.as_ref().expect("orphan disposition requires claim");
            ListItem::new(format!(
                "claim {} by {} until {}",
                b.id, claim.claimed_by, claim.lease_until_ts
            ))
        })
        .collect();
    orphan_items.extend(orphaned_reservations.iter().map(|r| {
        ListItem::new(format!(
            "reservation {} by {} on {}: {}",
            r.reservation_id,
            r.actor,
            r.entity,
            r.live_paths().join(", ")
        ))
    }));
    if orphan_items.is_empty() {
        orphan_items.push(ListItem::new(Span::styled(
            "(none)",
            Style::default().fg(Color::DarkGray),
        )));
    }
    f.render_widget(
        List::new(orphan_items).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Orphaned leases"),
        ),
        chunks[3],
    );

    let mut expiry_items: Vec<ListItem> = expiring_reservations
        .iter()
        .map(|reservation| {
            ListItem::new(format!(
                "EXPIRING {} by {} deadline {} — {}",
                reservation.reservation_id,
                reservation.actor,
                reservation.lease_until_ts,
                reservation.live_paths().join(", ")
            ))
        })
        .collect();
    expiry_items.extend(expired_reservations.iter().map(|reservation| {
        ListItem::new(format!(
            "EXPIRED {} by {} deadline {} reason=ttl_elapsed — {}",
            reservation.reservation_id,
            reservation.actor,
            reservation.lease_until_ts,
            reservation.live_paths().join(", ")
        ))
    }));
    if expiry_items.is_empty() {
        expiry_items.push(ListItem::new(Span::styled(
            "(none)",
            Style::default().fg(Color::DarkGray),
        )));
    }
    f.render_widget(
        List::new(expiry_items).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Reservation expiry"),
        ),
        chunks[4],
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
    if !b.rels.is_empty() {
        let rels = b
            .rels
            .iter()
            .map(|(p, k)| format!("{p}({k})"))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(Line::from(format!("rels: {rels}")));
    }
    let children = state.relation_children_of(&b.id);
    if !children.is_empty() {
        let child_ids = children
            .iter()
            .map(|(child, kind)| format!("{}({kind})", child.id))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(Line::from(format!("children: {child_ids}")));
    }
    let dependents = state.dependency_children_of(&b.id);
    if !dependents.is_empty() {
        let dependent_ids = dependents
            .iter()
            .map(|(child, kind)| format!("{}({kind})", child.id))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(Line::from(format!("dependents: {dependent_ids}")));
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

fn render_candidates(f: &mut Frame, app: &mut App, state: &State, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Min(24)])
        .split(area);
    let items: Vec<ListItem> = app
        .candidate_ids
        .iter()
        .filter_map(|id| state.candidates.get(id))
        .map(|candidate| {
            let landability =
                state.candidate_landability(&candidate.candidate_id, app.actor.as_deref());
            let disposition = if landability.landable {
                "landable".to_string()
            } else {
                landability
                    .reason_codes
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "blocked".into())
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:>10}", candidate.phase.as_str()),
                    Style::default().fg(candidate_phase_color(candidate.phase)),
                ),
                Span::raw(" "),
                Span::styled(
                    candidate.candidate_id.clone(),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw("  "),
                Span::styled(
                    truncate(&disposition, 28),
                    if landability.landable {
                        Style::default().fg(Color::Green)
                    } else {
                        Style::default().fg(Color::Red)
                    },
                ),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("Candidates ({})", app.candidate_ids.len())),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, chunks[0], &mut app.candidates_state);

    let lines = app
        .candidates_state
        .selected()
        .and_then(|index| app.candidate_ids.get(index))
        .and_then(|id| state.candidates.get(id))
        .map(|candidate| candidate_detail_lines(state, candidate, app.actor.as_deref()))
        .unwrap_or_else(|| vec![Line::from("(no selection)")]);
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Candidate detail"),
        ),
        chunks[1],
    );
}

fn candidate_phase_color(phase: crate::candidate::CandidatePhase) -> Color {
    match phase {
        crate::candidate::CandidatePhase::Pending => Color::Yellow,
        crate::candidate::CandidatePhase::Landed => Color::Green,
        crate::candidate::CandidatePhase::Superseded => Color::Blue,
        crate::candidate::CandidatePhase::Abandoned => Color::DarkGray,
    }
}

fn candidate_detail_lines(
    state: &State,
    candidate: &crate::state::CandidateRecord,
    actor: Option<&str>,
) -> Vec<Line<'static>> {
    let landability = state.candidate_landability(&candidate.candidate_id, actor);
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                candidate.candidate_id.clone(),
                Style::default().fg(Color::Cyan).bold(),
            ),
            Span::raw("  "),
            Span::styled(
                candidate.phase.as_str().to_string(),
                Style::default().fg(candidate_phase_color(candidate.phase)),
            ),
        ]),
        Line::from(format!("issue:       {}", candidate.entity)),
        Line::from(format!("proposer:    {}", candidate.proposer)),
        Line::from(format!("commit:      {}", candidate.commit_oid)),
        Line::from(format!("base:        {}", candidate.base_oid)),
        Line::from(format!("paths:       {}", candidate.paths.join(", "))),
        Line::from(format!("authorizer:  {}", candidate.authorizer)),
    ];
    let reviews = candidate
        .reviewers
        .iter()
        .map(|reviewer| {
            let verdict = candidate
                .reviews
                .get(reviewer)
                .map(|review| review.verdict.as_str())
                .unwrap_or("missing");
            format!("{reviewer}={verdict}")
        })
        .collect::<Vec<_>>()
        .join(", ");
    lines.push(Line::from(format!("reviews:     {reviews}")));
    let evidence = candidate
        .evidence
        .values()
        .map(|receipt| {
            format!(
                "{}:{}={}",
                receipt.name,
                receipt.producer,
                receipt.outcome.as_str()
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    lines.push(Line::from(format!(
        "evidence:    {}",
        if evidence.is_empty() {
            "none"
        } else {
            evidence.as_str()
        }
    )));
    let authorization = candidate
        .authorization
        .as_ref()
        .map(|authorization| {
            format!(
                "{} [{}]",
                authorization.status.as_str(),
                authorization.grantees.join(",")
            )
        })
        .unwrap_or_else(|| "absent".into());
    lines.push(Line::from(format!("authorization: {authorization}")));
    let now = ids::format_rfc3339(Timestamp::now());
    for reservation in state.candidate_reservations(&candidate.candidate_id) {
        lines.push(Line::from(format!(
            "reservation: {} {} by {} — {}",
            reservation.reservation_id,
            state.reservation_disposition(reservation, &now).as_str(),
            reservation.actor,
            reservation.live_paths().join(", ")
        )));
    }
    if let Some(successor) = &candidate.successor_id {
        lines.push(Line::from(format!("successor:   {successor}")));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("landability: ", Style::default().fg(Color::DarkGray)),
        if landability.landable {
            Span::styled("landable", Style::default().fg(Color::Green).bold())
        } else {
            Span::styled("blocked", Style::default().fg(Color::Red).bold())
        },
    ]));
    for reason in landability.reasons {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {}", reason.code),
                Style::default().fg(Color::Red),
            ),
            Span::raw(format!(" — {}", reason.detail)),
        ]));
    }
    lines
}

fn render_discussion(f: &mut Frame, app: &mut App, state: &State, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(32), Constraint::Min(24)])
        .split(area);

    let topics = state.board_topics_by_activity();
    let topics_focused = app.discussion_focus == DiscussionFocus::Topics;

    // One pass over the actor's unread posts feeds both the per-topic badge and
    // the per-post "new" dot.
    let mut unread_ids: HashSet<String> = HashSet::new();
    let mut unread_by_topic: HashMap<String, usize> = HashMap::new();
    if let Some(actor) = app.actor.as_deref() {
        for post in state.unread_board_posts_for(actor, None) {
            unread_ids.insert(post.post_id.clone());
            *unread_by_topic.entry(post.topic.clone()).or_insert(0) += 1;
        }
    }

    let items: Vec<ListItem> = topics
        .iter()
        .map(|t| {
            let mark = if t.explicit { "★" } else { " " };
            let mut spans = vec![
                Span::styled(mark.to_string(), Style::default().fg(Color::Yellow)),
                Span::raw(" "),
                Span::styled(t.topic.clone(), Style::default().fg(Color::Cyan)),
            ];
            match unread_by_topic.get(&t.topic) {
                Some(n) => spans.push(Span::styled(
                    format!("  ●{n}"),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                )),
                None => spans.push(Span::raw("  ")),
            }
            let counts = if t.sticky_count > 0 {
                format!(" posts={} sticky={}", t.post_count, t.sticky_count)
            } else {
                format!(" posts={}", t.post_count)
            };
            spans.push(Span::styled(counts, Style::default().fg(Color::DarkGray)));
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(pane_border_style(topics_focused))
                .title(format!("Topics ({})", topics.len())),
        )
        .highlight_style(selection_style(topics_focused))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, chunks[0], &mut app.topics_state);

    let selected = app.topics_state.selected().and_then(|i| topics.get(i));
    let inner_width = chunks[1].width.saturating_sub(2) as usize;
    let visible_rows = chunks[1].height.saturating_sub(2).max(1);

    let rendered = match selected {
        Some(t) => discussion_post_lines(
            state,
            &t.topic,
            inner_width,
            app.post_cursor,
            &unread_ids,
            !topics_focused,
        ),
        None => RenderedThread {
            lines: vec![Line::from(Span::styled(
                "(no topic selected)",
                Style::default().fg(Color::DarkGray),
            ))],
            starts: Vec::new(),
            unread: Vec::new(),
        },
    };

    app.discussion_page_rows = visible_rows;
    app.discussion_scroll_max = rendered
        .lines
        .len()
        .saturating_sub(visible_rows as usize)
        .min(u16::MAX as usize) as u16;
    app.discussion_scroll = app.discussion_scroll.min(app.discussion_scroll_max);
    app.post_starts = rendered.starts;
    app.post_unread = rendered.unread;
    if app.post_cursor >= app.post_starts.len() {
        app.post_cursor = app.post_starts.len().saturating_sub(1);
    }

    let title = match selected {
        Some(t) => {
            let mut title = if t.title.is_empty() || t.title == t.topic {
                t.topic.clone()
            } else {
                format!("{} — {}", t.topic, t.title)
            };
            if !app.post_starts.is_empty() {
                title.push_str(&format!(
                    "  [post {}/{}]",
                    app.post_cursor + 1,
                    app.post_starts.len()
                ));
            }
            if app.discussion_scroll_max > 0 {
                title.push_str(&format!(
                    "  {}%",
                    percent_scrolled(app.discussion_scroll, app.discussion_scroll_max)
                ));
            }
            title
        }
        None => "Posts".to_string(),
    };

    f.render_widget(
        Paragraph::new(rendered.lines)
            .scroll((app.discussion_scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(pane_border_style(!topics_focused))
                    .title(title),
            ),
        chunks[1],
    );
}

fn pane_border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn selection_style(focused: bool) -> Style {
    if focused {
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    }
}

fn percent_scrolled(scroll: u16, max: u16) -> u16 {
    if max == 0 {
        return 100;
    }
    ((scroll as u32 * 100) / max as u32) as u16
}

/// A topic's posts laid out as terminal rows, plus the row each post starts on
/// so key handling can jump post-to-post instead of line-by-line.
struct RenderedThread {
    lines: Vec<Line<'static>>,
    starts: Vec<u16>,
    unread: Vec<bool>,
}

/// Posts of one topic in reading order: root posts newest-activity-last as the
/// board already orders them, each followed by its reply subtree.
fn ordered_topic_posts<'a>(
    state: &'a State,
    topic: &str,
) -> Vec<(usize, &'a crate::state::BoardPostRecord)> {
    let posts = state.board_posts_for(Some(topic));
    let in_topic: HashSet<&str> = posts.iter().map(|p| p.post_id.as_str()).collect();
    let mut out: Vec<(usize, &crate::state::BoardPostRecord)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for post in &posts {
        let is_root = match post.reply_to.as_deref() {
            None => true,
            // A reply whose parent lives elsewhere still has to be readable.
            Some(parent) => !in_topic.contains(parent),
        };
        if !is_root || seen.contains(&post.post_id) {
            continue;
        }
        for (depth, threaded) in state.thread_posts(&post.post_id) {
            if threaded.topic != topic || !seen.insert(threaded.post_id.clone()) {
                continue;
            }
            out.push((depth, threaded));
        }
    }
    // Anything a cycle or a cross-topic parent kept out of the walk.
    for post in posts {
        if seen.insert(post.post_id.clone()) {
            out.push((0, post));
        }
    }
    out
}

fn discussion_post_lines(
    state: &State,
    topic: &str,
    width: usize,
    cursor: usize,
    unread_ids: &HashSet<String>,
    posts_focused: bool,
) -> RenderedThread {
    let ordered = ordered_topic_posts(state, topic);
    if ordered.is_empty() {
        return RenderedThread {
            lines: vec![Line::from(Span::styled(
                "(no posts)",
                Style::default().fg(Color::DarkGray),
            ))],
            starts: Vec::new(),
            unread: Vec::new(),
        };
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut starts: Vec<u16> = Vec::new();
    let mut unread: Vec<bool> = Vec::new();

    for (idx, (depth, post)) in ordered.iter().enumerate() {
        let selected = idx == cursor;
        let is_unread = unread_ids.contains(&post.post_id);
        starts.push(lines.len().min(u16::MAX as usize) as u16);
        unread.push(is_unread);

        let indent = "  ".repeat((*depth).min(6));
        let gutter = if selected {
            Span::styled(
                "▌",
                if posts_focused {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            )
        } else {
            Span::raw(" ")
        };

        lines.push(Line::from(post_header_spans(
            gutter.clone(),
            &indent,
            idx,
            post,
            *depth,
            selected,
            is_unread,
            width,
        )));

        let body_indent = format!("{indent}  ");
        let body_width = width.saturating_sub(1 + display_width(&body_indent));
        for source_line in post.body.lines() {
            for wrapped in wrap_line(source_line, body_width) {
                lines.push(Line::from(vec![
                    gutter.clone(),
                    Span::raw(format!("{body_indent}{wrapped}")),
                ]));
            }
        }
        lines.push(Line::from(gutter));
    }

    RenderedThread {
        lines,
        starts,
        unread,
    }
}

#[allow(clippy::too_many_arguments)]
fn post_header_spans(
    gutter: Span<'static>,
    indent: &str,
    idx: usize,
    post: &crate::state::BoardPostRecord,
    depth: usize,
    selected: bool,
    is_unread: bool,
    width: usize,
) -> Vec<Span<'static>> {
    let mut spans = vec![gutter, Span::raw(indent.to_string())];
    spans.push(Span::styled(
        format!("{}.", idx + 1),
        Style::default().fg(if selected {
            Color::Cyan
        } else {
            Color::DarkGray
        }),
    ));
    spans.push(Span::styled(
        if is_unread { " ● " } else { "   " }.to_string(),
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
    ));
    if post.sticky {
        spans.push(Span::styled("★ ", Style::default().fg(Color::Yellow)));
    }
    let mut author = Style::default().fg(Color::Yellow);
    if selected {
        author = author.add_modifier(Modifier::BOLD);
    }
    spans.push(Span::styled(format!("@{}", post.from), author));
    spans.push(Span::styled(
        format!("  {}", short_ts(&post.sent_ts)),
        Style::default().fg(Color::DarkGray),
    ));
    if post.post_kind != "post" {
        spans.push(Span::styled(
            format!("  {}", post.post_kind),
            Style::default().fg(Color::Magenta),
        ));
    }
    if post.retracted {
        spans.push(Span::styled(
            "  RETRACTED",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
    } else if let Some(replacement) = post.superseded_by.as_deref() {
        spans.push(Span::styled(
            format!("  SUPERSEDED -> {}", truncate(replacement, 16)),
            Style::default().fg(Color::Red),
        ));
    } else if !post.supersedes.is_empty() {
        spans.push(Span::styled(
            format!("  ACTIVE replaces {}", post.supersedes.len()),
            Style::default().fg(Color::Green),
        ));
    }
    // Indentation already says "reply"; only call it out when the parent is
    // somewhere else and there is no visible nesting.
    if depth == 0 && post.reply_to.is_some() {
        spans.push(Span::styled("  ↪", Style::default().fg(Color::DarkGray)));
    }

    // The post id is what an agent needs to reply, so keep it whenever the pane
    // is wide enough rather than letting it clip mid-id.
    let used: usize = spans.iter().map(|s| s.width()).sum();
    let room = width.saturating_sub(used);
    if room >= post.post_id.len() + 2 {
        spans.push(Span::styled(
            format!("  {}", post.post_id),
            Style::default().fg(Color::DarkGray),
        ));
    } else if room >= 12 {
        spans.push(Span::styled(
            format!("  {}", truncate(&post.post_id, room - 2)),
            Style::default().fg(Color::DarkGray),
        ));
    }
    spans
}

/// `2026-05-14T09:31:07Z` -> `05-14 09:31`; anything unexpected passes through.
fn short_ts(ts: &str) -> String {
    if ts.len() >= 16 && ts.is_char_boundary(5) && ts.is_char_boundary(16) {
        format!("{} {}", &ts[5..10], &ts[11..16])
    } else {
        ts.to_string()
    }
}

fn display_width(s: &str) -> usize {
    Span::raw(s).width()
}

/// Word-wrap one source line to `width` columns, keeping its leading
/// indentation on continuation rows so quoted code and lists stay readable.
fn wrap_line(text: &str, width: usize) -> Vec<String> {
    let text = text.replace('\t', "    ");
    if width == 0 || display_width(&text) <= width {
        return vec![text];
    }
    let indent: String = text.chars().take_while(|c| *c == ' ').collect();
    let cont_indent = if display_width(&indent) + 8 <= width {
        indent
    } else {
        String::new()
    };

    let mut out: Vec<String> = Vec::new();
    let mut cur = cont_indent.clone();
    let mut cur_w = display_width(&cur);
    let mut has_word = false;

    for word in text.split_whitespace() {
        let word_w = display_width(word);
        if has_word && cur_w + 1 + word_w > width {
            out.push(std::mem::replace(&mut cur, cont_indent.clone()));
            cur_w = display_width(&cur);
            has_word = false;
        }
        if has_word {
            cur.push(' ');
            cur_w += 1;
        }
        if word_w > width.saturating_sub(cur_w) {
            // A single word longer than the pane: break it across rows.
            let mut rest = word;
            while !rest.is_empty() {
                let room = width.saturating_sub(cur_w).max(1);
                let (chunk, tail) = split_at_width(rest, room);
                cur.push_str(chunk);
                cur_w += display_width(chunk);
                rest = tail;
                if !rest.is_empty() {
                    out.push(std::mem::replace(&mut cur, cont_indent.clone()));
                    cur_w = display_width(&cur);
                }
            }
        } else {
            cur.push_str(word);
            cur_w += word_w;
        }
        has_word = true;
    }
    out.push(cur);
    out
}

/// Split `s` at the last char boundary that fits in `width` columns, always
/// consuming at least one char so callers cannot loop forever.
fn split_at_width(s: &str, width: usize) -> (&str, &str) {
    let mut used = 0usize;
    let mut end = 0usize;
    for (offset, ch) in s.char_indices() {
        let w = display_width(ch.encode_utf8(&mut [0u8; 4]));
        if end > 0 && used + w > width {
            break;
        }
        used += w;
        end = offset + ch.len_utf8();
    }
    s.split_at(end)
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

fn render_agents(f: &mut Frame, app: &mut App, state: &State, area: Rect) {
    let as_of = Timestamp::now();
    let statuses = crate::actor_status::actor_statuses(
        state,
        app.actor.as_deref(),
        as_of,
        crate::actor_status::DEFAULT_RECENT_WINDOW_S,
    );
    let horizontal = area.width >= 72;
    let chunks = Layout::default()
        .direction(if horizontal {
            Direction::Horizontal
        } else {
            Direction::Vertical
        })
        .constraints(if horizontal {
            [Constraint::Percentage(45), Constraint::Min(28)]
        } else {
            [Constraint::Percentage(42), Constraint::Min(5)]
        })
        .split(area);
    let summary_width = chunks[0].width.saturating_sub(4) as usize;
    let items: Vec<ListItem> = statuses
        .iter()
        .map(|status| ListItem::new(agent_summary(status, summary_width)))
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("Agents ({})", statuses.len())),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, chunks[0], &mut app.agents_state);

    let detail = app
        .agents_state
        .selected()
        .and_then(|index| statuses.get(index))
        .map(agent_detail_lines)
        .unwrap_or_else(|| vec![Line::from("(no identified agents)")]);
    f.render_widget(
        Paragraph::new(detail).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Presence detail"),
        ),
        chunks[1],
    );
}

fn agent_summary(status: &crate::actor_status::ActorStatus, width: usize) -> String {
    let current = if status.current { "*" } else { " " };
    let summary = format!(
        "{current}{}  {} ({})  sessions={} inbox={} requests={}",
        status.actor,
        status.presence.state,
        status.presence.reason,
        status.presence.live_session_count,
        status.attention.inbox_unacked,
        status.attention.incoming_open_requests,
    );
    truncate(&summary, width.max(1))
}

fn agent_detail_lines(status: &crate::actor_status::ActorStatus) -> Vec<Line<'static>> {
    let intents = if status.intent.states.is_empty() {
        "-".to_string()
    } else {
        status.intent.states.join(",")
    };
    let mut lines = vec![
        agent_field("actor:       ", status.actor.clone()),
        agent_field("state:       ", status.presence.state.clone()),
        agent_field("source:      ", status.presence.source.clone()),
        agent_field("reason:      ", status.presence.reason.clone()),
        agent_field("as-of:       ", status.as_of_ts.clone()),
        agent_field(
            "lease-until: ",
            status
                .presence
                .latest_lease_until_ts
                .clone()
                .unwrap_or_else(|| "-".into()),
        ),
        agent_field("intent:      ", intents),
        agent_field(
            "observed:    ",
            activity_label(status.activity.last_observed.as_ref()),
        ),
        agent_field(
            "work:        ",
            activity_label(status.activity.last_work.as_ref()),
        ),
        agent_field(
            "interaction: ",
            activity_label(status.activity.last_interaction.as_ref()),
        ),
        agent_field(
            "attention:   ",
            format!(
                "inbox={} requests={} discussion={} notifications={}",
                status.attention.inbox_unacked,
                status.attention.incoming_open_requests,
                status.attention.discussion_unread,
                status.attention.topic_notifications_unread,
            ),
        ),
        agent_field(
            "work sets:   ",
            format!(
                "claims={} reservations={} doing={} candidates={}",
                status.work.active_claims.len(),
                status.work.active_reservations.len(),
                status.work.doing_beads.len(),
                status.work.candidates.len(),
            ),
        ),
    ];
    if !status.sessions.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "sessions:",
            Style::default().add_modifier(Modifier::UNDERLINED),
        )));
        for session in &status.sessions {
            let intent = session
                .intent
                .as_ref()
                .map(|intent| intent.state.as_str())
                .unwrap_or("-");
            lines.push(Line::from(format!(
                "{}  live={} lease={} intent={}",
                session.session_id, session.live, session.lease_until_ts, intent
            )));
        }
    }
    lines
}

fn agent_field(label: &'static str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(label, Style::default().fg(Color::DarkGray)),
        Span::raw(value),
    ])
}

fn activity_label(evidence: Option<&crate::actor_status::ActivityEvidence>) -> String {
    evidence
        .map(|evidence| format!("{} {}", evidence.ts, evidence.event_type))
        .unwrap_or_else(|| "-".into())
}

fn render_help(f: &mut Frame, area: Rect) {
    let w = area.width.saturating_sub(20).min(60);
    let h = 20u16.min(area.height.saturating_sub(4));
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
        Line::from("1 2 3 4 5 6   jump to tab"),
        Line::from("j/k or ↑/↓    move selection"),
        Line::from("g / G         first / last item"),
        Line::from("PgUp / PgDn   page list; in Discussion, scroll posts"),
        Line::from("r             force refresh now"),
        Line::from("? / h         show / hide help"),
        Line::from(""),
        Line::from(Span::styled(
            "Discussion",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("→ / Enter     read the selected topic"),
        Line::from("←             back to the topic list"),
        Line::from("j/k, n/p      next / previous post"),
        Line::from("g / G         first / last post"),
        Line::from("u             next unread post"),
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
    use crate::candidate::{
        CandidateEvidencePayload, CandidatePhase, EvidenceOutcome, EvidenceRequirement,
        GIT_ANCESTRY_EVIDENCE, GitAncestryReceipt,
    };
    use crate::state::{BoardPostRecord, CandidateEvidenceRecord, CandidateRecord};

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
            candidates_state: ListState::default(),
            topics_state: ListState::default(),
            activity_state: ListState::default(),
            agents_state: ListState::default(),
            discussion_scroll: 0,
            discussion_scroll_max: 0,
            discussion_page_rows: 1,
            discussion_focus: DiscussionFocus::Topics,
            post_cursor: 0,
            post_starts: Vec::new(),
            post_unread: Vec::new(),
            bead_ids: Vec::new(),
            candidate_ids: Vec::new(),
            topic_names: Vec::new(),
            activity: Vec::new(),
            actor_names: Vec::new(),
        }
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn agent_view_is_explicit_and_useful_at_narrow_widths() {
        let as_of: Timestamp = "2026-08-29T12:00:00Z".parse().unwrap();
        let mut status = crate::actor_status::actor_status(
            &State::default(),
            "alice",
            Some("alice"),
            as_of,
            600,
        );
        for suffix in ["A", "B"] {
            status.sessions.push(crate::actor_status::SessionStatus {
                session_id: format!("sess-{suffix}"),
                actor: "alice".into(),
                label: None,
                pid: None,
                ttl_s: 300,
                started_ts: "2026-08-29T11:55:00Z".into(),
                started_op_id: format!("op-start-{suffix}"),
                last_heartbeat_ts: "2026-08-29T11:59:00Z".into(),
                last_heartbeat_op_id: format!("op-heartbeat-{suffix}"),
                lease_until_ts: "2026-08-29T12:04:00Z".into(),
                ended_ts: None,
                ended_op_id: None,
                live: true,
                intent: None,
            });
        }

        let summary = agent_summary(&status, 28);
        assert!(summary.chars().count() <= 28);
        assert!(summary.contains("alice"));
        assert!(!summary.to_ascii_lowercase().contains("online"));

        let detail = agent_detail_lines(&status)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(detail.contains("source:      none"));
        assert!(detail.contains("reason:      no_presence_evidence"));
        assert!(detail.contains("as-of:       2026-08-29T12:00:00"));
        assert!(detail.contains("sess-A"));
        assert!(detail.contains("sess-B"));
    }

    #[test]
    fn candidate_detail_distinguishes_unavailable_git_evidence_from_landable() {
        let candidate_id = "cand-01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string();
        let commit = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
        let payload = CandidateEvidencePayload::GitAncestry(GitAncestryReceipt {
            repository_id: "repo-test".into(),
            object_format: "sha1".into(),
            common_dir_hash: String::new(),
            commit_oid: commit.clone(),
            base_oid: "1111111111111111111111111111111111111111".into(),
            parent_oids: vec!["1111111111111111111111111111111111111111".into()],
            base_is_ancestor: None,
            candidate_relations: Vec::new(),
            covered_candidates: Vec::new(),
            git_version: "unavailable".into(),
            detail: Some("shallow clone".into()),
        });
        let mut evidence = std::collections::BTreeMap::new();
        evidence.insert(
            (GIT_ANCESTRY_EVIDENCE.into(), "proposer".into()),
            CandidateEvidenceRecord {
                producer: "proposer".into(),
                producer_tool: "unavailable".into(),
                evidence_id: crate::candidate::evidence_id(&payload).unwrap(),
                name: GIT_ANCESTRY_EVIDENCE.into(),
                evidence_kind: "git".into(),
                candidate_oid: commit.clone(),
                outcome: EvidenceOutcome::Unavailable,
                payload,
                refs: Vec::new(),
                op_id: "op-evidence".into(),
                ts: "2026-08-29T00:00:00Z".into(),
            },
        );
        let candidate = CandidateRecord {
            candidate_id: candidate_id.clone(),
            entity: "bd-test".into(),
            proposer: "proposer".into(),
            proposal_op_id: "op-proposal".into(),
            store_id: "st-test".into(),
            repository_id: "repo-test".into(),
            object_format: "sha1".into(),
            commit_oid: commit,
            base_oid: "1111111111111111111111111111111111111111".into(),
            parent_oids: vec!["1111111111111111111111111111111111111111".into()],
            paths: vec!["src/lib.rs".into()],
            authorizer: "authorizer".into(),
            reviewers: vec!["reviewer".into()],
            evidence_requirements: vec![EvidenceRequirement {
                name: GIT_ANCESTRY_EVIDENCE.into(),
                kind: "git".into(),
                producers: vec!["proposer".into()],
            }],
            evidence_refs: Vec::new(),
            phase: CandidatePhase::Pending,
            phase_op_id: "op-proposal".into(),
            successor_id: None,
            reviews: std::collections::BTreeMap::new(),
            evidence,
            authorization: None,
            landed: None,
        };
        let mut state = State::default();
        state.candidates.insert(candidate_id.clone(), candidate);
        let text = candidate_detail_lines(&state, &state.candidates[&candidate_id], None)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("git_evidence_unavailable"));
        assert!(text.contains("authorization_absent"));
        assert!(!text.contains("landability: landable"));
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
    }

    #[test]
    fn changing_discussion_topic_resets_post_cursor_and_scroll() {
        let mut app = test_app();
        app.topic_names = vec!["alpha".into(), "beta".into()];
        app.topics_state.select(Some(0));
        app.discussion_scroll = 8;
        app.discussion_scroll_max = 20;
        app.post_cursor = 3;
        app.post_starts = vec![0, 4, 9, 14];

        app.move_down();
        assert_eq!(app.topics_state.selected(), Some(1));
        assert_eq!(app.discussion_scroll, 0);
        assert_eq!(app.post_cursor, 0);
        assert!(app.post_starts.is_empty());
    }

    #[test]
    fn focused_posts_pane_jumps_post_to_post() {
        let mut app = test_app();
        app.topic_names = vec!["alpha".into()];
        app.topics_state.select(Some(0));
        app.post_starts = vec![0, 6, 21, 40];
        app.discussion_scroll_max = 30;
        app.focus_posts();

        app.move_down();
        assert_eq!(app.post_cursor, 1);
        assert_eq!(app.discussion_scroll, 6);

        app.move_down();
        assert_eq!(app.post_cursor, 2);
        assert_eq!(app.discussion_scroll, 21);

        // The last post starts past the end of the scrollable range.
        app.move_down();
        assert_eq!(app.post_cursor, 3);
        assert_eq!(app.discussion_scroll, 30);

        app.move_down();
        assert_eq!(app.post_cursor, 3, "cursor stops at the last post");

        app.move_up();
        assert_eq!(app.post_cursor, 2);
        assert_eq!(app.discussion_scroll, 21);

        // Topic selection is untouched while the posts pane has focus.
        assert_eq!(app.topics_state.selected(), Some(0));

        app.home();
        assert_eq!(app.post_cursor, 0);
        assert_eq!(app.discussion_scroll, 0);
        app.end();
        assert_eq!(app.post_cursor, 3);
    }

    #[test]
    fn unfocused_posts_pane_still_moves_topics() {
        let mut app = test_app();
        app.topic_names = vec!["alpha".into(), "beta".into()];
        app.topics_state.select(Some(0));
        app.post_starts = vec![0, 6];

        app.move_down();
        assert_eq!(app.topics_state.selected(), Some(1));
        assert_eq!(app.post_cursor, 0);
    }

    #[test]
    fn page_scroll_keeps_cursor_on_the_topmost_visible_post() {
        let mut app = test_app();
        app.topic_names = vec!["alpha".into()];
        app.topics_state.select(Some(0));
        app.post_starts = vec![0, 6, 21, 40];
        app.post_unread = vec![false; 4];
        app.discussion_page_rows = 10;
        app.discussion_scroll_max = 60;

        app.page_down();
        assert_eq!(app.discussion_scroll, 10);
        assert_eq!(app.post_cursor, 1);

        app.page_down();
        assert_eq!(app.discussion_scroll, 20);
        assert_eq!(app.post_cursor, 1);

        app.page_down();
        assert_eq!(app.discussion_scroll, 30);
        assert_eq!(app.post_cursor, 2);

        app.page_up();
        assert_eq!(app.post_cursor, 1);
    }

    #[test]
    fn next_unread_post_wraps_around() {
        let mut app = test_app();
        app.topic_names = vec!["alpha".into()];
        app.topics_state.select(Some(0));
        app.post_starts = vec![0, 6, 21, 40];
        app.post_unread = vec![true, false, false, true];
        app.discussion_scroll_max = 60;

        app.next_unread_post();
        assert_eq!(app.post_cursor, 3);
        assert!(
            app.posts_focused(),
            "reading an unread post focuses the pane"
        );

        app.next_unread_post();
        assert_eq!(app.post_cursor, 0, "wraps back to the first unread post");
        assert_eq!(app.discussion_scroll, 0);
    }

    #[test]
    fn post_navigation_is_inert_without_posts() {
        let mut app = test_app();
        app.focus_posts();
        app.move_down();
        app.move_up();
        app.next_unread_post();
        assert_eq!(app.post_cursor, 0);
        assert_eq!(app.discussion_scroll, 0);
    }

    fn post(id: &str, from: &str, body: &str, reply_to: Option<&str>) -> BoardPostRecord {
        BoardPostRecord {
            post_id: id.into(),
            from: from.into(),
            topic: "planning".into(),
            body: body.into(),
            reply_to: reply_to.map(String::from),
            answers: Vec::new(),
            explicit_notify: Vec::new(),
            notification_recipients: Vec::new(),
            idempotency_key: None,
            post_kind: "post".into(),
            sticky: false,
            sticky_op_id: None,
            superseded_by: None,
            superseded_op_id: None,
            supersedes: Vec::new(),
            retracted: false,
            retraction_reason: None,
            retracted_op_id: None,
            route: Default::default(),
            sent_ts: "2026-05-14T09:31:07Z".into(),
            sent_op_id: format!("op-{id}"),
        }
    }

    fn rendered(state: &State, cursor: usize) -> RenderedThread {
        discussion_post_lines(state, "planning", 60, cursor, &HashSet::new(), true)
    }

    #[test]
    fn discussion_headers_expose_superseded_and_retracted_posts() {
        let mut state = State::default();
        let mut old = post("post-old", "alice", "old", None);
        old.superseded_by = Some("post-new".into());
        state.board_posts.insert(old.post_id.clone(), old);
        let mut withdrawn = post("post-withdrawn", "alice", "bad premise", None);
        withdrawn.retracted = true;
        withdrawn.retraction_reason = Some("incorrect".into());
        withdrawn.sent_op_id = "op-z".into();
        state
            .board_posts
            .insert(withdrawn.post_id.clone(), withdrawn);
        let text = rendered(&state, 0)
            .lines
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("SUPERSEDED -> post-new"), "{text}");
        assert!(text.contains("RETRACTED"), "{text}");
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
            let mut record = post(&post_id, "alice", &body, None);
            record.sent_ts = format!("2026-05-14T00:00:{i:02}Z");
            record.sent_op_id = format!("op-{i:03}");
            state.board_posts.insert(post_id, record);
        }

        let out = rendered(&state, 0);
        let text = out
            .lines
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("line 6"), "body text was truncated:\n{text}");
        assert!(
            text.contains("post-060"),
            "post list was truncated:\n{text}"
        );
        assert_eq!(out.starts.len(), 61, "one start offset per post");
        assert_eq!(out.unread.len(), 61);
    }

    #[test]
    fn post_start_offsets_point_at_each_header_row() {
        let mut state = State::default();
        for (id, body) in [
            ("post-a", "one\ntwo"),
            ("post-b", "solo"),
            ("post-c", "x\ny\nz"),
        ] {
            state
                .board_posts
                .insert(id.into(), post(id, "alice", body, None));
        }

        let out = rendered(&state, 0);
        assert_eq!(out.starts.len(), 3);
        for (idx, start) in out.starts.iter().enumerate() {
            let header = line_text(&out.lines[*start as usize]);
            assert!(
                header.contains(&format!("{}.", idx + 1)),
                "row {start} is not the header of post {}: {header}",
                idx + 1
            );
            assert!(header.contains('@'), "header lacks an author: {header}");
        }
        // header + 2 body + blank
        assert_eq!(out.starts[1], 4);
    }

    #[test]
    fn replies_are_indented_under_their_parent() {
        let mut state = State::default();
        state
            .board_posts
            .insert("post-a".into(), post("post-a", "alice", "root", None));
        state
            .board_posts
            .insert("post-c".into(), post("post-c", "carol", "later root", None));
        state.board_posts.insert(
            "post-b".into(),
            post("post-b", "bob", "reply", Some("post-a")),
        );

        let ordered = ordered_topic_posts(&state, "planning");
        let ids: Vec<(usize, &str)> = ordered
            .iter()
            .map(|(d, p)| (*d, p.post_id.as_str()))
            .collect();
        assert_eq!(
            ids,
            vec![(0, "post-a"), (1, "post-b"), (0, "post-c")],
            "replies must follow their parent"
        );
    }

    #[test]
    fn orphan_replies_still_appear_once() {
        let mut state = State::default();
        state.board_posts.insert(
            "post-b".into(),
            post("post-b", "bob", "reply to another topic", Some("post-zz")),
        );
        let ordered = ordered_topic_posts(&state, "planning");
        assert_eq!(ordered.len(), 1);
        assert_eq!(ordered[0].0, 0, "orphan replies render at the root");

        let out = rendered(&state, 0);
        let text = out
            .lines
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("↪"), "orphan reply is marked:\n{text}");
    }

    #[test]
    fn wrap_line_fits_the_pane_and_keeps_indentation() {
        let wrapped = wrap_line("    let x = compute(alpha, beta, gamma, delta);", 20);
        assert!(wrapped.len() > 1);
        for row in &wrapped {
            assert!(display_width(row) <= 20, "row too wide: {row:?}");
        }
        assert!(wrapped[0].starts_with("    "));
        assert!(wrapped[1].starts_with("    "), "continuation keeps indent");
        assert_eq!(
            wrapped.join(" ").split_whitespace().collect::<Vec<_>>(),
            "let x = compute(alpha, beta, gamma, delta);"
                .split_whitespace()
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn wrap_line_breaks_words_longer_than_the_pane() {
        let wrapped = wrap_line(&"z".repeat(45), 10);
        assert_eq!(wrapped.len(), 5);
        for row in &wrapped {
            assert!(display_width(row) <= 10);
        }
        assert_eq!(wrapped.concat(), "z".repeat(45));
    }

    #[test]
    fn wrap_line_handles_empty_and_zero_width() {
        assert_eq!(wrap_line("", 10), vec![String::new()]);
        assert_eq!(wrap_line("abc", 0), vec!["abc".to_string()]);
    }

    #[test]
    fn wrapped_body_rows_never_exceed_the_pane_width() {
        let mut state = State::default();
        state.board_posts.insert(
            "post-a".into(),
            post(
                "post-a",
                "alice",
                "the quick brown fox jumps over the lazy dog again and again and again",
                None,
            ),
        );
        let width = 32;
        let out = discussion_post_lines(&state, "planning", width, 0, &HashSet::new(), true);
        for line in &out.lines {
            assert!(
                line.width() <= width,
                "line overflows the pane: {:?}",
                line_text(line)
            );
        }
    }

    fn topic_record(topic: &str, posts: usize) -> crate::state::BoardTopicRecord {
        crate::state::BoardTopicRecord {
            topic: topic.into(),
            title: "Planning".into(),
            body: String::new(),
            created_by: "alice".into(),
            created_ts: "2026-05-14T09:00:00Z".into(),
            created_op_id: "op-000".into(),
            explicit: true,
            last_activity_ts: "2026-05-14T09:31:07Z".into(),
            last_activity_op_id: "op-999".into(),
            post_count: posts,
            sticky_count: 0,
            decision_count: 0,
            summary_post_id: None,
            route: Default::default(),
        }
    }

    fn row_text(buf: &ratatui::buffer::Buffer, y: u16) -> String {
        (0..buf.area.width)
            .map(|x| buf[(x, y)].symbol())
            .collect::<String>()
    }

    /// The whole point of the pane: one keystroke moves a whole post, and the
    /// post you land on is at the top of the reading area, not somewhere below
    /// the fold.
    #[test]
    fn jumping_posts_puts_the_target_header_at_the_top_of_the_pane() {
        let mut state = State::default();
        state
            .board_topics
            .insert("planning".into(), topic_record("planning", 8));
        for i in 0..8 {
            let id = format!("post-{i:03}");
            let mut record = post(
                &id,
                &format!("agent{i}"),
                "a body long enough to wrap at least once in a narrow pane, plus\na second source line",
                None,
            );
            record.sent_op_id = format!("op-{i:03}");
            state.board_posts.insert(id, record);
        }

        let mut app = test_app();
        app.state = Some(state);
        app.topic_names = vec!["planning".into()];
        app.topics_state.select(Some(0));
        app.focus_posts();

        let mut term = Terminal::new(ratatui::backend::TestBackend::new(80, 20)).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
        assert_eq!(app.post_starts.len(), 8, "every post gets a row offset");

        // Header rows are 3 tall and the pane has a border, so the first
        // readable row of the posts pane is y = 4.
        let top_row = 4;
        app.next_post();
        app.next_post();
        term.draw(|f| render(f, &mut app)).unwrap();

        assert_eq!(app.post_cursor, 2);
        let row = row_text(term.backend().buffer(), top_row);
        assert!(
            row.contains("3. ") && row.contains("@agent2"),
            "post 3 should sit at the top of the pane, got: {row}"
        );
        assert!(row.contains('▌'), "selected post is marked: {row}");
    }

    #[test]
    fn short_ts_compacts_rfc3339() {
        assert_eq!(short_ts("2026-05-14T09:31:07Z"), "05-14 09:31");
        assert_eq!(short_ts("bogus"), "bogus");
    }
}
