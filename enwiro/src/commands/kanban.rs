use anyhow::Context;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{self, EnterAlternateScreen};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Padding, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

use crate::context::CommandContext;
use crate::environments::Environment;
use crate::usage_stats::EnvStats;
use enwiro_daemon::meta::{CookedPhase, Status};
use enwiro_sdk::client::CachedRecipe;

#[derive(clap::Args)]
#[command(about = "interactive kanban board of environments grouped by status")]
pub struct KanbanArgs {}

#[derive(Clone)]
struct Card {
    name: String,
    description: Option<String>,
    is_recipe: bool,
}

enum Action {
    Quit,
    Activate(String),
    Mark(String, &'static str),
}

struct KanbanState {
    columns: [Vec<Card>; 4],
    selected_col: usize,
    selected_row: [usize; 4],
    status_menu: Option<StatusMenu>,
}

struct StatusMenu {
    selected: usize,
}

const COLUMN_NAMES: [&str; 4] = ["Ready", "Active", "Waiting", "Done"];
const COLUMN_COLORS: [Color; 4] = [Color::Blue, Color::Green, Color::Yellow, Color::Magenta];
const STATUS_OPTIONS: [&str; 4] = ["ready", "active", "waiting", "done"];

fn classify(status: Option<&Status>) -> Option<usize> {
    match status {
        None | Some(Status::Uncooked) | Some(Status::Cooked { phase: None, .. }) => Some(0),
        Some(Status::Cooked {
            phase: Some(CookedPhase::Active),
            ..
        }) => Some(1),
        Some(Status::Cooked {
            phase: Some(CookedPhase::Waiting),
            ..
        }) => Some(2),
        Some(Status::Done { .. }) => Some(3),
        Some(Status::Evergreen) => None,
    }
}

fn load_board<W: Write>(
    context: &CommandContext<W>,
    prev: Option<&KanbanState>,
) -> anyhow::Result<KanbanState> {
    let envs: Vec<Environment> = context.get_all_environments()?.into_values().collect();

    let mut meta_map: HashMap<String, EnvStats> = HashMap::new();
    for env in &envs {
        let env_dir = Path::new(&context.config.workspaces_directory).join(&env.name);
        let meta = crate::usage_stats::load_env_meta(&env_dir);
        meta_map.insert(env.name.clone(), meta);
    }

    let mut columns: [Vec<Card>; 4] = Default::default();

    for env in &envs {
        let meta = meta_map.get(&env.name);
        let status = meta.and_then(|m| m.status.as_ref());
        if let Some(col) = classify(status) {
            columns[col].push(Card {
                name: env.name.clone(),
                description: meta.and_then(|m| m.description.clone()),
                is_recipe: false,
            });
        }
    }

    let env_names: std::collections::HashSet<String> =
        envs.iter().map(|e| e.name.clone()).collect();
    let recipes = load_recipes(context);
    for recipe in recipes {
        if !env_names.contains(&recipe.name) {
            columns[0].push(Card {
                name: recipe.name,
                description: recipe.description,
                is_recipe: true,
            });
        }
    }

    for col in &mut columns {
        col.sort_by(|a, b| a.name.cmp(&b.name));
    }

    let (selected_col, selected_row) = match prev {
        Some(p) => (p.selected_col, p.selected_row),
        None => (0, [0; 4]),
    };

    let mut selected_row = selected_row;
    for (i, col) in columns.iter().enumerate() {
        if selected_row[i] >= col.len() && !col.is_empty() {
            selected_row[i] = col.len() - 1;
        }
    }

    Ok(KanbanState {
        columns,
        selected_col,
        selected_row,
        status_menu: None,
    })
}

fn load_recipes<W: Write>(context: &CommandContext<W>) -> Vec<CachedRecipe> {
    let cache = match &context.cache_dir {
        Some(dir) => enwiro_daemon::DaemonCache::with_runtime_dir(dir.clone()),
        None => match enwiro_daemon::DaemonCache::open() {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        },
    };

    let content = match cache.read_recipes() {
        Ok(Some(s)) => s,
        _ => return Vec::new(),
    };

    content
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str::<CachedRecipe>(l).ok())
        .collect()
}

pub fn kanban<W: Write>(context: &mut CommandContext<W>, _args: KanbanArgs) -> anyhow::Result<()> {
    let mut state = load_board(context, None)?;

    crossterm::execute!(std::io::stderr(), EnterAlternateScreen)
        .context("failed to enter alternate screen")?;
    terminal::enable_raw_mode().context("failed to enable raw mode")?;

    let mut terminal = ratatui::init();

    let final_action = loop {
        match run_until_action(&mut terminal, &mut state)? {
            Action::Quit => break None,
            Action::Activate(name) => break Some(name),
            Action::Mark(name, status) => {
                crate::context::mark_via_daemon(&name, status, enwiro_sdk::rpc::MarkSource::User);
                state = load_board(context, Some(&state))?;
            }
        }
    };

    ratatui::restore();

    if let Some(name) = final_action {
        // TODO: call activate via IPC instead of shelling out
        let status = std::process::Command::new("enw")
            .arg("activate")
            .arg(&name)
            .status()
            .context("failed to run enw activate")?;
        if !status.success() {
            anyhow::bail!("enw activate '{}' failed", name);
        }
    }

    Ok(())
}

fn selected_card(state: &KanbanState) -> Option<&Card> {
    let col = state.selected_col;
    let row = state.selected_row[col];
    state.columns[col].get(row)
}

fn run_until_action(
    terminal: &mut DefaultTerminal,
    state: &mut KanbanState,
) -> anyhow::Result<Action> {
    loop {
        terminal
            .draw(|f| draw(f, state))
            .context("failed to draw")?;

        if let Event::Key(key) = event::read().context("failed to read event")? {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            if let Some(menu) = &mut state.status_menu {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        state.status_menu = None;
                    }
                    KeyCode::Up | KeyCode::Char('k') if menu.selected > 0 => {
                        menu.selected -= 1;
                    }
                    KeyCode::Down | KeyCode::Char('j')
                        if menu.selected < STATUS_OPTIONS.len() - 1 =>
                    {
                        menu.selected += 1;
                    }
                    KeyCode::Enter => {
                        let status = STATUS_OPTIONS[menu.selected];
                        if let Some(card) = selected_card(state)
                            && !card.is_recipe
                        {
                            return Ok(Action::Mark(card.name.clone(), status));
                        }
                        state.status_menu = None;
                    }
                    _ => {}
                }
                continue;
            }

            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(Action::Quit),
                KeyCode::Left | KeyCode::Char('h') if state.selected_col > 0 => {
                    state.selected_col -= 1;
                }
                KeyCode::Right | KeyCode::Char('l') if state.selected_col < 3 => {
                    state.selected_col += 1;
                }
                KeyCode::Up | KeyCode::Char('k') if state.selected_row[state.selected_col] > 0 => {
                    state.selected_row[state.selected_col] -= 1;
                }
                KeyCode::Down | KeyCode::Char('j')
                    if state.selected_row[state.selected_col]
                        < state.columns[state.selected_col].len().saturating_sub(1) =>
                {
                    state.selected_row[state.selected_col] += 1;
                }
                KeyCode::Enter => {
                    if let Some(card) = selected_card(state) {
                        return Ok(Action::Activate(card.name.clone()));
                    }
                }
                KeyCode::Char(' ') => {
                    if let Some(card) = selected_card(state)
                        && !card.is_recipe
                    {
                        state.status_menu = Some(StatusMenu { selected: 0 });
                    }
                }
                _ => {}
            }
        }
    }
}

fn draw(frame: &mut Frame, state: &KanbanState) {
    let area = frame.area();

    let col_gaps = Layout::horizontal([
        Constraint::Percentage(25),
        Constraint::Percentage(25),
        Constraint::Percentage(25),
        Constraint::Percentage(25),
    ])
    .split(area);

    for (i, col_area) in col_gaps.iter().enumerate() {
        let is_selected_col = i == state.selected_col;
        let color = COLUMN_COLORS[i];
        let count = state.columns[i].len();

        let header_text = format!("{} ({})", COLUMN_NAMES[i], count);
        let header_style = if is_selected_col {
            Style::default()
                .fg(color)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let inner = shrink(*col_area, 1, 0);

        let header = Paragraph::new(Line::from(Span::styled(header_text, header_style)));
        let header_area = Rect { height: 1, ..inner };
        frame.render_widget(header, header_area);

        let cards_area = Rect {
            y: inner.y + 2,
            height: inner.height.saturating_sub(3),
            ..inner
        };

        draw_cards(
            frame,
            &state.columns[i],
            cards_area,
            is_selected_col,
            state.selected_row[i],
            color,
        );
    }

    let help = " hjkl/arrows: move | Enter: activate | Space: set status | q: quit ";
    let help_area = Rect {
        x: 0,
        y: area.height.saturating_sub(1),
        width: area.width,
        height: 1,
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            help,
            Style::default().fg(Color::DarkGray),
        ))),
        help_area,
    );

    if state.status_menu.is_some() {
        draw_status_menu(frame, state);
    }
}

fn draw_cards(
    frame: &mut Frame,
    cards: &[Card],
    area: Rect,
    is_selected_col: bool,
    selected_row: usize,
    color: Color,
) {
    let card_height: u16 = 3;
    let gap: u16 = 1;
    let mut y = area.y;

    for (j, card) in cards.iter().enumerate() {
        if y + card_height > area.y + area.height {
            break;
        }

        let is_selected = is_selected_col && j == selected_row;
        let card_area = Rect {
            x: area.x,
            y,
            width: area.width,
            height: card_height,
        };

        let badge = if card.is_recipe { " [recipe]" } else { "" };
        let name_line = format!("{}{}", card.name, badge);

        let (name_style, desc_style, block_style) = if is_selected {
            (
                Style::default()
                    .fg(color)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED),
                Style::default().add_modifier(Modifier::REVERSED),
                Style::default().add_modifier(Modifier::REVERSED),
            )
        } else {
            (
                Style::default()
                    .fg(Color::White)
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
                Style::default().fg(Color::White).bg(Color::DarkGray),
                Style::default().bg(Color::DarkGray),
            )
        };

        let mut lines = vec![Line::from(Span::styled(name_line, name_style))];

        if let Some(desc) = &card.description {
            lines.push(Line::from(Span::styled(desc.clone(), desc_style)));
        }

        let block = Block::default()
            .padding(Padding::horizontal(1))
            .style(block_style);

        let inner_area = block.inner(card_area);
        frame.render_widget(block, card_area);
        frame.render_widget(Paragraph::new(lines), inner_area);

        y += card_height + gap;
    }
}

fn draw_status_menu(frame: &mut Frame, state: &KanbanState) {
    let area = frame.area();
    let menu = state.status_menu.as_ref().unwrap();

    let menu_width: u16 = 20;
    let menu_height: u16 = STATUS_OPTIONS.len() as u16 + 2;
    let x = area.width.saturating_sub(menu_width) / 2;
    let y = area.height.saturating_sub(menu_height) / 2;

    let menu_area = Rect::new(x, y, menu_width, menu_height);

    frame.render_widget(Clear, menu_area);

    let block = Block::default()
        .title(" Set status ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::White))
        .padding(Padding::horizontal(1));

    let items: Vec<ListItem> = STATUS_OPTIONS
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let color = COLUMN_COLORS[i];
            let is_selected = i == menu.selected;
            let style = if is_selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(color)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(color)
            };
            ListItem::new(Line::from(Span::styled(*label, style)))
        })
        .collect();

    let list = List::new(items).block(block);
    frame.render_widget(list, menu_area);
}

fn shrink(area: Rect, horizontal: u16, vertical: u16) -> Rect {
    Rect {
        x: area.x + horizontal,
        y: area.y + vertical,
        width: area.width.saturating_sub(horizontal * 2),
        height: area.height.saturating_sub(vertical * 2),
    }
}
