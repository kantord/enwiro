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
    CookAndMark(String, &'static str),
}

struct KanbanState {
    columns: [Vec<Card>; 4],
    selected_col: usize,
    selected_row: [usize; 4],
    /// Index of the first visible card per column. Persisted so scrolling is
    /// edge-triggered (the view only moves when the cursor reaches the margin),
    /// not recomputed-from-cursor every frame.
    scroll_offset: [usize; 4],
    command_menu: Option<CommandMenu>,
}

struct CommandMenu {
    selected: usize,
}

const COLUMN_NAMES: [&str; 4] = ["Ready", "Active", "Waiting", "Done"];
const COLUMN_COLORS: [Color; 4] = [Color::Blue, Color::Green, Color::Yellow, Color::Magenta];
const STATUS_OPTIONS: [&str; 4] = ["ready", "active", "waiting", "done"];

/// Command palette entries: index 0 is "Work on" (activate); indices 1..=4 set the
/// status at `STATUS_OPTIONS[index - 1]`.
const COMMAND_COUNT: usize = STATUS_OPTIONS.len() + 1;

fn command_label(index: usize) -> String {
    if index == 0 {
        "Work on".to_string()
    } else {
        format!("Set status: {}", STATUS_OPTIONS[index - 1])
    }
}

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

    let (selected_col, selected_row, scroll_offset) = match prev {
        Some(p) => (p.selected_col, p.selected_row, p.scroll_offset),
        None => (0, [0; 4], [0; 4]),
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
        scroll_offset,
        command_menu: None,
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
            Action::CookAndMark(name, status) => {
                terminal.draw(|f| draw_cooking(f, &name)).ok();
                let cfg = crate::context::CookConfig { no_hooks: false };
                context.cook_environment(&name, &name, &cfg)?;
                let flat_name = name.replace('/', "-");
                crate::context::mark_via_daemon(
                    &flat_name,
                    status,
                    enwiro_sdk::rpc::MarkSource::User,
                );
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

            if let Some(menu) = &mut state.command_menu {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        state.command_menu = None;
                    }
                    KeyCode::Up | KeyCode::Char('k') if menu.selected > 0 => {
                        menu.selected -= 1;
                    }
                    KeyCode::Down | KeyCode::Char('j') if menu.selected < COMMAND_COUNT - 1 => {
                        menu.selected += 1;
                    }
                    KeyCode::Enter => {
                        let selected = menu.selected;
                        state.command_menu = None;
                        if let Some(card) = selected_card(state) {
                            if selected == 0 {
                                return Ok(Action::Activate(card.name.clone()));
                            }
                            let status = STATUS_OPTIONS[selected - 1];
                            if card.is_recipe {
                                return Ok(Action::CookAndMark(card.name.clone(), status));
                            }
                            return Ok(Action::Mark(card.name.clone(), status));
                        }
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
                KeyCode::Enter if selected_card(state).is_some() => {
                    state.command_menu = Some(CommandMenu { selected: 0 });
                }
                _ => {}
            }
        }
    }
}

fn draw(frame: &mut Frame, state: &mut KanbanState) {
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
            &mut state.scroll_offset[i],
            color,
        );
    }

    let help = " hjkl/arrows: move | Enter: commands | q: quit ";
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

    if state.command_menu.is_some() {
        draw_command_menu(frame, state);
    }
}

/// Edge-triggered scroll: keep `current` offset unless the cursor reaches within
/// `margin` (one item, where the viewport allows) of an edge, then scroll the
/// minimum needed. Returns the new first-visible index.
fn scroll_window(current: usize, selected: usize, len: usize, visible: usize) -> usize {
    if len <= visible {
        return 0;
    }
    let max_offset = len - visible;
    let margin = 1usize.min(visible.saturating_sub(1) / 2);

    let mut offset = current.min(max_offset);
    if selected < offset + margin {
        offset = selected.saturating_sub(margin);
    }
    let bottom = offset + visible - 1;
    if selected + margin > bottom {
        offset = (selected + margin + 1).saturating_sub(visible);
    }
    offset.min(max_offset)
}

fn draw_cards(
    frame: &mut Frame,
    cards: &[Card],
    area: Rect,
    is_selected_col: bool,
    selected_row: usize,
    scroll_offset: &mut usize,
    color: Color,
) {
    let card_height: u16 = 3;
    let gap: u16 = 1;

    let stride = card_height + gap;
    let visible = (((area.height + gap) / stride).max(1)) as usize;

    let first = scroll_window(*scroll_offset, selected_row, cards.len(), visible);
    *scroll_offset = first;

    let mut y = area.y;

    for (j, card) in cards.iter().enumerate().skip(first) {
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

fn draw_command_menu(frame: &mut Frame, state: &KanbanState) {
    let area = frame.area();
    let menu = state.command_menu.as_ref().unwrap();

    let menu_width: u16 = 24;
    let menu_height: u16 = COMMAND_COUNT as u16 + 2;
    let x = area.width.saturating_sub(menu_width) / 2;
    let y = area.height.saturating_sub(menu_height) / 2;

    let menu_area = Rect::new(x, y, menu_width, menu_height);

    frame.render_widget(Clear, menu_area);

    let block = Block::default()
        .title(" Commands ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::White))
        .padding(Padding::horizontal(1));

    let items: Vec<ListItem> = (0..COMMAND_COUNT)
        .map(|i| {
            // "Work on" (i == 0) is uncolored; status commands take their column color.
            let color = if i == 0 {
                Color::White
            } else {
                COLUMN_COLORS[i - 1]
            };
            let is_selected = i == menu.selected;
            let style = if is_selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(color)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(color)
            };
            ListItem::new(Line::from(Span::styled(command_label(i), style)))
        })
        .collect();

    let list = List::new(items).block(block);
    frame.render_widget(list, menu_area);
}

fn draw_cooking(frame: &mut Frame, name: &str) {
    let area = frame.area();

    let text = format!(" Cooking {}... ", name);
    let box_width = (text.len() as u16 + 2).min(area.width);
    let box_height: u16 = 3;
    let x = area.width.saturating_sub(box_width) / 2;
    let y = area.height.saturating_sub(box_height) / 2;

    let box_area = Rect::new(x, y, box_width, box_height);
    frame.render_widget(Clear, box_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));
    let paragraph = Paragraph::new(Line::from(Span::styled(
        text,
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
    )))
    .block(block);
    frame.render_widget(paragraph, box_area);
}

fn shrink(area: Rect, horizontal: u16, vertical: u16) -> Rect {
    Rect {
        x: area.x + horizontal,
        y: area.y + vertical,
        width: area.width.saturating_sub(horizontal * 2),
        height: area.height.saturating_sub(vertical * 2),
    }
}

#[cfg(test)]
mod tests {
    use super::scroll_window;

    /// Everything fits: never scroll.
    #[test]
    fn no_scroll_when_content_fits() {
        assert_eq!(scroll_window(0, 0, 3, 5), 0);
        assert_eq!(scroll_window(0, 2, 3, 5), 0);
    }

    /// Moving down stays put until the cursor hits the bottom margin (one item
    /// to spare), then scrolls the minimum needed.
    #[test]
    fn down_is_edge_triggered_with_margin() {
        // visible=5, margin=1. Rows 0..3 keep offset 0 (row 4 stays visible as spare).
        assert_eq!(scroll_window(0, 3, 10, 5), 0);
        // Row 4 reaches the bottom margin -> scroll one, keeping row 5 as the spare.
        assert_eq!(scroll_window(0, 4, 10, 5), 1);
        // Row 5 from offset 1 -> scroll one more.
        assert_eq!(scroll_window(1, 5, 10, 5), 2);
    }

    /// Moving up stays put until the cursor hits the top margin, then scrolls up.
    #[test]
    fn up_is_edge_triggered_with_margin() {
        // offset 3 shows rows 3..7. Cursor at row 5 is comfortably inside -> no move.
        assert_eq!(scroll_window(3, 5, 10, 5), 3);
        // Cursor at row 4 hits the top margin -> scroll up one (keeps row 3 spare).
        assert_eq!(scroll_window(3, 4, 10, 5), 3);
        // Cursor at row 3 (top visible) -> scroll up to keep a spare above.
        assert_eq!(scroll_window(3, 3, 10, 5), 2);
    }

    /// At the ends there is no room for a spare; offset clamps to the extremes.
    #[test]
    fn clamps_at_ends() {
        // Top: cursor at row 0 cannot keep a spare above.
        assert_eq!(scroll_window(2, 0, 10, 5), 0);
        // Bottom: last row pins the window to max offset (len - visible), no spare below.
        assert_eq!(scroll_window(0, 9, 10, 5), 5);
    }

    /// Tiny viewports collapse the margin so the cursor still stays visible.
    #[test]
    fn small_viewport_keeps_cursor_visible() {
        // visible=1, margin collapses to 0: offset always equals the selected row.
        assert_eq!(scroll_window(0, 0, 10, 1), 0);
        assert_eq!(scroll_window(0, 4, 10, 1), 4);
        assert_eq!(scroll_window(4, 9, 10, 1), 9);
    }
}
