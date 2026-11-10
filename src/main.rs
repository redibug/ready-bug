//! Ready Bug v0 — agent-first local AI command center.
//!
//! Screens: Agents (home) · Chat · Models (Ollama) · New Agent.
//! See DESIGN.md for where this is all going.

mod ollama;
mod store;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ollama::{ChatEvent, ChatMessage, ModelInfo};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};
use ratatui_image::{picker::Picker, protocol::StatefulProtocol, Resize, StatefulImage};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;
use store::Agent;

const THINK_MODES: [&str; 6] = ["on", "auto", "low", "medium", "high", "off"];

/// A short message with no hard-problem words counts as simple.
fn is_simple(text: &str) -> bool {
    if text.chars().count() > 90 {
        return false;
    }
    let lower = text.to_lowercase();
    const HARD: &[&str] = &[
        "why", "how", "explain", "code", "debug", "error", "fix", "write", "plan", "compare",
        "help", "create", "build", "story", "think",
    ];
    !HARD.iter().any(|w| lower.contains(w))
}

fn resolve_think(mode: Option<&str>, last_user: &str) -> ollama::Think {
    match mode.unwrap_or("on") {
        "off" => ollama::Think::Off,
        "low" => ollama::Think::Level("low".to_string()),
        "medium" => ollama::Think::Level("medium".to_string()),
        "high" => ollama::Think::Level("high".to_string()),
        // auto: small talk answers directly; real questions keep full thought.
        "auto" => {
            if is_simple(last_user) {
                ollama::Think::Off
            } else {
                ollama::Think::Default
            }
        }
        _ => ollama::Think::Default,
    }
}

const MIN_W: u16 = 60;
const MIN_H: u16 = 15;
const PINK: Color = Color::Rgb(255, 121, 198);
const DIM: Color = Color::Rgb(140, 140, 160);

#[derive(PartialEq, Clone, Copy)]
enum Screen {
    Home,
    Chat,
    Models,
    NewAgent,
    PickModel,
}

enum ModelsStatus {
    Loading,
    Ok(Vec<ModelInfo>),
    Err(String),
}

struct App {
    screen: Screen,
    agents: Vec<Agent>,
    selected: usize,
    models: ModelsStatus,
    models_rx: Receiver<Result<Vec<ModelInfo>, String>>,
    models_tx: Sender<Result<Vec<ModelInfo>, String>>,
    chat: Vec<ChatMessage>,
    chat_input: String,
    streaming: bool,
    chat_rx: Option<Receiver<ChatEvent>>,
    form: [String; 3], // name, model, avatar path
    form_focus: usize,
    pick_selected: usize,
    status: String,
    picker: Picker,
    avatar_cache: Option<(String, StatefulProtocol)>,
}

impl App {
    fn current_agent(&self) -> Option<&Agent> {
        self.agents.get(self.selected)
    }

    fn refresh_models(&mut self) {
        self.models = ModelsStatus::Loading;
        let tx = self.models_tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(ollama::fetch_models());
        });
    }

    fn drain_channels(&mut self) {
        while let Ok(res) = self.models_rx.try_recv() {
            self.models = match res {
                Ok(m) => ModelsStatus::Ok(m),
                Err(e) => ModelsStatus::Err(e),
            };
        }
        if let Some(rx) = &self.chat_rx {
            loop {
                match rx.try_recv() {
                    Ok(ChatEvent::Token(tok)) => {
                        if let Some(last) = self.chat.last_mut() {
                            if last.role == "assistant" {
                                last.content.push_str(&tok);
                            }
                        }
                    }
                    Ok(ChatEvent::Thinking(tok)) => {
                        if let Some(last) = self.chat.last_mut() {
                            if last.role == "assistant" {
                                last.thinking.push_str(&tok);
                            }
                        }
                    }
                    Ok(ChatEvent::Done) => {
                        self.streaming = false;
                        break;
                    }
                    Ok(ChatEvent::Error(e)) => {
                        if let Some(last) = self.chat.last_mut() {
                            if last.role == "assistant" {
                                if !last.content.is_empty() {
                                    last.content.push_str("\n");
                                }
                                last.content.push_str(&format!("[{e}]"));
                            }
                        }
                        self.streaming = false;
                        break;
                    }
                    Err(_) => break,
                }
            }
        }
        if !self.streaming {
            self.chat_rx = None;
        }
    }

    fn send_chat(&mut self) {
        let Some(agent) = self.current_agent().cloned() else {
            return;
        };
        let text = self.chat_input.trim().to_string();
        if text.is_empty() || self.streaming {
            return;
        }
        let think = resolve_think(agent.think.as_deref(), &text);
        self.chat_input.clear();
        self.chat.push(ChatMessage {
            role: "user".to_string(),
            content: text,
            thinking: String::new(),
        });
        self.chat.push(ChatMessage {
            role: "assistant".to_string(),
            content: String::new(),
            thinking: String::new(),
        });
        let mut messages = vec![ChatMessage {
            role: "system".to_string(),
            content: store::system_prompt(&agent),
            thinking: String::new(),
        }];
        // History without the empty assistant placeholder.
        messages.extend(self.chat.iter().take(self.chat.len() - 1).cloned());
        let (tx, rx) = channel();
        self.chat_rx = Some(rx);
        self.streaming = true;
        std::thread::spawn(move || {
            ollama::stream_chat(agent.model.clone(), messages, think, tx);
        });
    }

    fn cycle_think(&mut self) {
        let Some(agent) = self.agents.get_mut(self.selected) else {
            return;
        };
        let current = agent.think.as_deref().unwrap_or("on");
        let idx = THINK_MODES.iter().position(|m| *m == current).unwrap_or(0);
        let next = THINK_MODES[(idx + 1) % THINK_MODES.len()];
        agent.think = if next == "on" {
            None
        } else {
            Some(next.to_string())
        };
        let (name, snapshot) = (agent.name.clone(), agent.clone());
        match store::save_config(&snapshot) {
            Ok(()) => self.status = format!("{name} thinking: {next} (saved)"),
            Err(e) => self.status = format!("could not save thinking mode: {e}"),
        }
    }

    fn ensure_avatar(&mut self, agent: &Agent) -> Option<&mut StatefulProtocol> {
        let slug = agent.slug.clone();
        let stale = self
            .avatar_cache
            .as_ref()
            .map(|(s, _)| s != &slug)
            .unwrap_or(true);
        if stale {
            self.avatar_cache = None;
            if let Some(path) = &agent.avatar {
                if let Ok(img) = image::ImageReader::open(path).and_then(|r| r.decode().map_err(std::io::Error::other)) {
                    let proto = self.picker.new_resize_protocol(img);
                    self.avatar_cache = Some((slug, proto));
                }
            }
        }
        self.avatar_cache.as_mut().map(|(_, p)| p)
    }
}

fn main() -> std::io::Result<()> {
    if let Err(e) = store::ensure_seed() {
        eprintln!("warning: could not prepare data dir: {e}");
    }
    let (models_tx, models_rx) = channel();
    let mut app = App {
        screen: Screen::Home,
        agents: store::load_agents(),
        selected: 0,
        models: ModelsStatus::Loading,
        models_rx,
        models_tx,
        chat: Vec::new(),
        chat_input: String::new(),
        streaming: false,
        chat_rx: None,
        form: [String::new(), String::new(), String::new()],
        form_focus: 0,
        pick_selected: 0,
        status: format!("data: {}", store::data_dir().display()),
        picker: Picker::halfblocks(),
        avatar_cache: None,
    };
    app.refresh_models();

    // Init first, THEN query the terminal's graphics protocol: the picker
    // query saves/restores termios itself, and if it restores after raw
    // mode is on it would undo it (keys would echo instead of registering).
    // With raw mode already on, its save/restore preserves it.
    let mut terminal = ratatui::init();
    app.picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    let result = run(&mut terminal, &mut app);
    ratatui::restore();
    result
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> std::io::Result<()> {
    loop {
        app.drain_channels();
        terminal.draw(|f| ui(f, app))?;
        if event::poll(Duration::from_millis(80))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                    break;
                }
                match app.screen {
                    Screen::Home => {
                        match key.code {
                            KeyCode::Char('q') => break,
                            KeyCode::Up | KeyCode::Char('k') => {
                                app.selected = app.selected.saturating_sub(1);
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                if app.selected + 1 < app.agents.len() {
                                    app.selected += 1;
                                }
                            }
                            KeyCode::Enter => {
                                if app.current_agent().is_some() {
                                    app.chat.clear();
                                    app.chat_input.clear();
                                    app.screen = Screen::Chat;
                                }
                            }
                            KeyCode::Char('n') => {
                                app.form = [String::new(), String::new(), String::new()];
                                app.form_focus = 0;
                                app.screen = Screen::NewAgent;
                            }
                            KeyCode::Char('e') => {
                                if let Some(agent) = app.current_agent().cloned() {
                                    match &app.models {
                                        ModelsStatus::Ok(list) if !list.is_empty() => {
                                            app.pick_selected = list
                                                .iter()
                                                .position(|m| m.name == agent.model)
                                                .unwrap_or(0);
                                            app.screen = Screen::PickModel;
                                        }
                                        ModelsStatus::Ok(_) => {
                                            app.status =
                                                "no models installed to pick from".to_string();
                                        }
                                        ModelsStatus::Loading => {
                                            app.status = "still looking for ollama…".to_string();
                                        }
                                        ModelsStatus::Err(_) => {
                                            app.status =
                                                "ollama not detected — start it to swap models"
                                                    .to_string();
                                        }
                                    }
                                }
                            }
                            KeyCode::Char('t') => app.cycle_think(),
                            KeyCode::Char('m') => app.screen = Screen::Models,
                            KeyCode::Char('r') => app.refresh_models(),
                            _ => {}
                        }
                    }
                    Screen::Models => match key.code {
                        KeyCode::Esc | KeyCode::Char('q') => app.screen = Screen::Home,
                        KeyCode::Char('r') => app.refresh_models(),
                        _ => {}
                    },
                    Screen::PickModel => {
                        let count = match &app.models {
                            ModelsStatus::Ok(list) => list.len(),
                            _ => 0,
                        };
                        match key.code {
                            KeyCode::Esc => app.screen = Screen::Home,
                            KeyCode::Up | KeyCode::Char('k') => {
                                app.pick_selected = app.pick_selected.saturating_sub(1);
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                if app.pick_selected + 1 < count {
                                    app.pick_selected += 1;
                                }
                            }
                            KeyCode::Enter => {
                                if let (Some(agent), ModelsStatus::Ok(list)) =
                                    (app.current_agent().cloned(), &app.models)
                                {
                                    if let Some(chosen) = list.get(app.pick_selected) {
                                        let model = chosen.name.clone();
                                        match store::set_model(&agent, &model) {
                                            Ok(()) => {
                                                if let Some(a) =
                                                    app.agents.get_mut(app.selected)
                                                {
                                                    a.model = model.clone();
                                                }
                                                app.status = format!(
                                                    "{} now runs {} — saved",
                                                    agent.name, model
                                                );
                                            }
                                            Err(e) => {
                                                app.status =
                                                    format!("could not save model: {e}");
                                            }
                                        }
                                        app.screen = Screen::Home;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    Screen::Chat => match key.code {
                        KeyCode::Esc => app.screen = Screen::Home,
                        KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.cycle_think()
                        }
                        KeyCode::Enter => app.send_chat(),
                        KeyCode::Backspace => {
                            app.chat_input.pop();
                        }
                        KeyCode::Char(c) => {
                            app.chat_input.push(c);
                        }
                        _ => {}
                    },
                    Screen::NewAgent => match key.code {
                        KeyCode::Esc => app.screen = Screen::Home,
                        KeyCode::Tab | KeyCode::Down => {
                            app.form_focus = (app.form_focus + 1) % 3;
                        }
                        KeyCode::BackTab | KeyCode::Up => {
                            app.form_focus = (app.form_focus + 2) % 3;
                        }
                        KeyCode::Backspace => {
                            app.form[app.form_focus].pop();
                        }
                        KeyCode::Char(c) => {
                            let f = &mut app.form[app.form_focus];
                            f.push(c);
                        }
                        KeyCode::Enter => {
                            if app.form_focus < 2 {
                                app.form_focus += 1;
                            } else if app.form[0].trim().is_empty() {
                                app.status = "an agent needs a name first".to_string();
                                app.form_focus = 0;
                            } else {
                                let name = app.form[0].clone();
                                match store::create_agent(
                                    &app.form[0],
                                    &app.form[1],
                                    &app.form[2],
                                ) {
                                    Ok(agent) => {
                                        app.status = format!("welcome, {} 🐞", agent.name);
                                        app.agents = store::load_agents();
                                        app.selected = app
                                            .agents
                                            .iter()
                                            .position(|a| a.slug == agent.slug)
                                            .unwrap_or(0);
                                        app.screen = Screen::Home;
                                    }
                                    Err(e) => {
                                        app.status = format!("could not create agent: {e}");
                                    }
                                }
                                let _ = name;
                            }
                        }
                        _ => {}
                    },
                }
            }
        }
    }
    Ok(())
}

fn ollama_status(app: &App) -> (String, Color) {
    match &app.models {
        ModelsStatus::Loading => ("ollama: …".to_string(), DIM),
        ModelsStatus::Ok(m) => (format!("ollama: ● {} models", m.len()), Color::Green),
        ModelsStatus::Err(_) => ("ollama: ○ not detected".to_string(), Color::Red),
    }
}

fn ui(f: &mut Frame, app: &mut App) {
    let area = f.area();
    if area.width < MIN_W || area.height < MIN_H {
        let msg = format!("too small! needs {MIN_W}x{MIN_H}, is {}x{}", area.width, area.height);
        let p = Paragraph::new(msg)
            .style(Style::default().fg(PINK))
            .alignment(ratatui::layout::Alignment::Center);
        let mid = Rect { x: 0, y: area.height / 2, width: area.width, height: 1 };
        f.render_widget(p, mid);
        return;
    }
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);

    let (stat, stat_color) = ollama_status(app);
    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            " READY BUG ",
            Style::default().fg(Color::Black).bg(PINK).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  agent command center   "),
        Span::styled(stat, Style::default().fg(stat_color)),
    ]));
    f.render_widget(header, rows[0]);

    match app.screen {
        Screen::Home => ui_home(f, app, rows[1]),
        Screen::Chat => ui_chat(f, app, rows[1]),
        Screen::Models => ui_models(f, app, rows[1]),
        Screen::NewAgent => ui_new_agent(f, app, rows[1]),
        Screen::PickModel => ui_pick_model(f, app, rows[1]),
    }

    let status = Paragraph::new(format!(" {}", app.status)).style(Style::default().fg(DIM));
    f.render_widget(status, rows[2]);
    let hints = match app.screen {
        Screen::Home => " ↑↓ pick · enter chat · e swap model · t thinking · n new agent · m models · r refresh · q quit",
        Screen::Chat => " enter send · ctrl+t thinking mode · esc back · ctrl+c quit",
        Screen::Models => " r refresh · esc back",
        Screen::NewAgent => " tab next field · enter create (on last field) · esc cancel",
        Screen::PickModel => " ↑↓ pick · enter use this model · esc cancel",
    };
    let footer = Paragraph::new(hints).style(Style::default().fg(DIM));
    f.render_widget(footer, rows[3]);
}

fn ui_home(f: &mut Frame, app: &mut App, area: Rect) {
    let cols = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).split(area);
    let items: Vec<ListItem> = app
        .agents
        .iter()
        .map(|a| {
            ListItem::new(Line::from(vec![
                Span::styled(a.name.clone(), Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(format!("  {}", a.model), Style::default().fg(DIM)),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" agents "))
        .highlight_style(Style::default().fg(Color::Black).bg(PINK))
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    if !app.agents.is_empty() {
        state.select(Some(app.selected));
    }
    f.render_stateful_widget(list, cols[0], &mut state);

    let Some(agent) = app.current_agent().cloned() else {
        let p = Paragraph::new("no agents yet — press n to make one")
            .block(Block::default().borders(Borders::ALL).title(" hello "));
        f.render_widget(p, cols[1]);
        return;
    };
    let detail = Block::default().borders(Borders::ALL).title(format!(" {} ", agent.name));
    let inner = detail.inner(cols[1]);
    f.render_widget(detail, cols[1]);
    let parts = Layout::vertical([Constraint::Length(12), Constraint::Min(0)]).split(inner);
    if agent.avatar.is_some() {
        if let Some(proto) = app.ensure_avatar(&agent) {
            let widget = StatefulImage::new().resize(Resize::Fit(None));
            f.render_stateful_widget(widget, parts[0], proto);
        }
    } else {
        let p = Paragraph::new("\n   (no avatar yet — add avatar.png to my folder,\n    or give a path when creating an agent)")
            .style(Style::default().fg(DIM));
        f.render_widget(p, parts[0]);
    }
    let info = Paragraph::new(vec![
        Line::from(vec![Span::styled("model: ", Style::default().fg(DIM)), Span::raw(agent.model.clone())]),
        Line::from(vec![Span::styled("files: ", Style::default().fg(DIM)), Span::raw(agent.dir.display().to_string())]),
        Line::from(""),
        Line::from(Span::styled("persona, memory + lessons load into every chat.", Style::default().fg(DIM))),
    ])
    .wrap(Wrap { trim: false });
    f.render_widget(info, parts[1]);
}

fn ui_chat(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(agent) = app.current_agent().cloned() else {
        return;
    };
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(3),
    ])
    .split(area);
    let phase = if !app.streaming {
        ""
    } else if let Some(last) = app.chat.last().filter(|m| m.role == "assistant") {
        if !last.content.is_empty() {
            " · receiving…"
        } else if !last.thinking.is_empty() {
            " · thinking…"
        } else {
            " · waiting…"
        }
    } else {
        " · waiting…"
    };
    let title = Paragraph::new(Line::from(vec![
        Span::styled(format!(" {} ", agent.name), Style::default().fg(PINK).add_modifier(Modifier::BOLD)),
        Span::styled(format!(" · {}", agent.model), Style::default().fg(DIM)),
        Span::styled(
            format!(" · think: {}", agent.think.as_deref().unwrap_or("on")),
            Style::default().fg(DIM),
        ),
        Span::styled(phase, Style::default().fg(DIM)),
    ]))
    .block(Block::default().borders(Borders::ALL));
    f.render_widget(title, rows[0]);

    // Build display lines, keep the tail that fits (anchored to bottom).
    let width = rows[1].width.saturating_sub(2).max(10) as usize;
    let mut lines: Vec<Line> = Vec::new();
    if app.chat.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("say hi to {} — persona, memory + lessons are loaded.", agent.name),
            Style::default().fg(DIM),
        )));
    }
    for msg in &app.chat {
        let (who, color) = if msg.role == "user" {
            ("you".to_string(), Color::Cyan)
        } else {
            (agent.name.clone(), PINK)
        };
        if !msg.thinking.is_empty() {
            let mut first = true;
            for part in msg.thinking.split('\n') {
                if first {
                    lines.push(Line::from(Span::styled(
                        format!("💭 {part}"),
                        Style::default().fg(DIM),
                    )));
                    first = false;
                } else {
                    lines.push(Line::from(Span::styled(
                        format!("   {part}"),
                        Style::default().fg(DIM),
                    )));
                }
            }
        }
        let text = if msg.content.is_empty() && msg.thinking.is_empty() && app.streaming {
            "…".to_string()
        } else {
            msg.content.clone()
        };
        let mut first = true;
        for part in text.split('\n') {
            if first {
                lines.push(Line::from(vec![
                    Span::styled(format!("{who}: "), Style::default().fg(color).add_modifier(Modifier::BOLD)),
                    Span::raw(part.to_string()),
                ]));
                first = false;
            } else {
                lines.push(Line::from(format!("{}  {part}", " ".repeat(who.chars().count()))));
            }
        }
        lines.push(Line::from(""));
    }
    let height = rows[1].height.saturating_sub(2) as usize;
    let est: usize = lines
        .iter()
        .map(|l| {
            let w: usize = l.spans.iter().map(|s| s.content.chars().count()).sum();
            (w / width) + 1
        })
        .sum();
    let skip = est.saturating_sub(height.max(1));
    let visible: Vec<Line> = {
        let mut acc = 0usize;
        let mut out = Vec::new();
        for l in lines {
            let w: usize = l.spans.iter().map(|s| s.content.chars().count()).sum();
            let h = (w / width) + 1;
            if acc >= skip {
                out.push(l);
            }
            acc += h;
        }
        out
    };
    let body = Paragraph::new(visible)
        .block(Block::default().borders(Borders::ALL).title(" chat "))
        .wrap(Wrap { trim: false });
    f.render_widget(body, rows[1]);

    let input = Paragraph::new(format!("{}_", app.chat_input))
        .block(Block::default().borders(Borders::ALL).title(" message "));
    f.render_widget(input, rows[2]);
}

fn ui_pick_model(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(agent) = app.current_agent().cloned() else {
        return;
    };
    let ModelsStatus::Ok(models) = &app.models else {
        return;
    };
    let items: Vec<ListItem> = models
        .iter()
        .map(|m| {
            let marker = if m.name == agent.model { "● current  " } else { "           " };
            ListItem::new(Line::from(vec![
                Span::styled(marker.to_string(), Style::default().fg(DIM)),
                Span::styled(m.name.clone(), Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(
                    format!("  {}", ollama::format_size(m.size)),
                    Style::default().fg(DIM),
                ),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" swap model for {} ", agent.name)),
        )
        .highlight_style(Style::default().fg(Color::Black).bg(PINK))
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    state.select(Some(app.pick_selected.min(models.len().saturating_sub(1))));
    f.render_stateful_widget(list, area, &mut state);
}

fn ui_models(f: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title(" ollama models ");
    match &app.models {
        ModelsStatus::Loading => {
            let p = Paragraph::new("looking for ollama…").block(block);
            f.render_widget(p, area);
        }
        ModelsStatus::Err(e) => {
            let p = Paragraph::new(vec![
                Line::from(Span::styled(format!("ollama {e}"), Style::default().fg(Color::Red))),
                Line::from(""),
                Line::from(format!("expected it at {}", ollama::base_url())),
                Line::from("start ollama, then press r to refresh"),
            ])
            .block(block);
            f.render_widget(p, area);
        }
        ModelsStatus::Ok(models) => {
            let mut lines = vec![
                Line::from(Span::styled(
                    format!("{:<28} {:>10}   pulled", "name", "size"),
                    Style::default().fg(DIM),
                )),
                Line::from(""),
            ];
            for m in models {
                let date = m.modified_at.get(..10).unwrap_or(&m.modified_at);
                lines.push(Line::from(format!(
                    "{:<28} {:>10}   {}",
                    m.name,
                    ollama::format_size(m.size),
                    date
                )));
            }
            if models.is_empty() {
                lines.push(Line::from("no models installed yet — browser lands in v1"));
            }
            let p = Paragraph::new(lines).block(block);
            f.render_widget(p, area);
        }
    }
}

fn ui_new_agent(f: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title(" new agent ");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Min(0),
    ])
    .split(inner);
    let labels = ["name", "model (blank = qwen3.5:9b)", "avatar image path (optional)"];
    for (i, label) in labels.iter().enumerate() {
        let active = app.form_focus == i;
        let style = if active {
            Style::default().fg(PINK).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(DIM)
        };
        let p = Paragraph::new(vec![
            Line::from(Span::styled(format!("{label}:"), style)),
            Line::from(format!("  {}{}", app.form[i], if active { "_" } else { "" })),
        ]);
        f.render_widget(p, rows[i]);
    }
    let note = Paragraph::new("creates a folder of plain-text files you own:\nagent.toml · persona.md · memory.md · lessons.md")
        .style(Style::default().fg(DIM));
    f.render_widget(note, rows[4]);
}
