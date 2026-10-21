// Экран звонка: участники и чат в общем каркасе PR #1.
use super::{Ui, accent, bad, dim, green, strong};
use crate::{
    action::{Action, Focus, Pane},
    app::App,
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
const NAME_W: usize = 12;
const MIC_X: u16 = 15;
const DEAF_X: u16 = 20;
const BAR_CELLS: usize = 6;

pub fn render(app: &App, f: &mut Frame, ui: &mut Ui, area: Rect) {
    if area.width >= 86 {
        let [left, right] =
            Layout::horizontal([Constraint::Length(44), Constraint::Min(20)]).areas(area);
        participants(app, f, ui, left);
        chat(app, f, ui, right);
    } else {
        let count = app.participant_count() as u16;
        let height = (count + 3)
            .max(6)
            .min(12)
            .min(area.height.saturating_sub(6));
        let [top, bottom] =
            Layout::vertical([Constraint::Length(height), Constraint::Min(3)]).areas(area);
        participants(app, f, ui, top);
        chat(app, f, ui, bottom);
    }
}
fn participants(app: &App, f: &mut Frame, ui: &mut Ui, area: Rect) {
    let focused = app.focus == Focus::Pane(Pane::Side);
    let p = &app.engine.params;
    let block = Block::bordered()
        .title(format!(" Участники · входящий {}% ", p.gain_peer()))
        .border_style(if focused { accent() } else { dim() });
    let inner = block.inner(area);
    f.render_widget(block, area);
    ui.push_pane(inner, Pane::Side);
    if inner.height == 0 {
        return;
    }
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "  Имя          Мик  Звук Пинг   Уровень",
            dim(),
        ))),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    let mut rows = vec![row_line(
        focused,
        app.call_sel == 0,
        format!("{} (вы)", app.cfg.name),
        Some(p.mute_mic()),
        Some(p.deaf()),
        app.engine
            .self_speaking
            .load(std::sync::atomic::Ordering::Relaxed),
        if p.mute_mic() { 0 } else { p.in_level() },
        "—".into(),
        accent(),
    )];
    if app.group_room.is_some() {
        for (i, peer) in app.group_peers.iter().enumerate() {
            rows.push(row_line(
                focused,
                app.call_sel == i + 1,
                peer.name.clone(),
                peer.known.then_some(peer.mute || peer.deaf),
                peer.known.then_some(peer.deaf),
                peer.speaking && peer.known && !peer.mute && !peer.deaf,
                if peer.mute || peer.deaf {
                    0
                } else {
                    peer.level
                },
                peer.rtt.map_or_else(|| "…".into(), |ms| format!("{ms} мс")),
                green(),
            ));
        }
    } else if let Some(peer) = &app.peer {
        rows.push(row_line(
            focused,
            app.call_sel == 1,
            peer.name.clone(),
            peer.status_known.then_some(peer.mute_mic || peer.deaf),
            peer.status_known.then_some(peer.deaf),
            peer.speaking && peer.status_known && !peer.mute_mic && !peer.deaf,
            if peer.mute_mic { 0 } else { p.out_level() },
            peer.rtt.map_or_else(|| "…".into(), |ms| format!("{ms} мс")),
            green(),
        ));
    } else {
        rows.push(row_line(
            focused,
            app.call_sel == 1,
            "Ожидаем собеседника".into(),
            None,
            None,
            false,
            0,
            "…".into(),
            dim(),
        ));
    }
    let visible = inner.height.saturating_sub(1) as usize;
    let start = (app.call_sel + 1)
        .saturating_sub(visible)
        .min(rows.len().saturating_sub(visible));
    let lines = rows
        .into_iter()
        .skip(start)
        .take(visible)
        .collect::<Vec<_>>();
    let count = lines.len();
    f.render_widget(
        Paragraph::new(lines),
        Rect::new(
            inner.x,
            inner.y + 1,
            inner.width,
            inner.height.saturating_sub(1),
        ),
    );
    for i in 0..count {
        let index = start + i;
        let y = inner.y + 1 + i as u16;
        ui.push(
            Rect::new(inner.x, y, inner.width, 1),
            Action::Click(Pane::Side, index),
        );
        if index == 0 {
            for (offset, action) in [(MIC_X, Action::ToggleMute), (DEAF_X, Action::ToggleDeaf)] {
                if offset + 4 <= inner.width {
                    ui.push(Rect::new(inner.x + offset, y, 4, 1), action);
                }
            }
        }
    }
}
fn row_line(
    focused: bool,
    selected: bool,
    name: String,
    muted: Option<bool>,
    deaf: Option<bool>,
    speaking: bool,
    level: u8,
    ping: String,
    bar_style: Style,
) -> Line<'static> {
    let marker = if focused && selected {
        Span::styled("▸ ", accent().add_modifier(Modifier::BOLD))
    } else {
        Span::raw("  ")
    };
    let name = pad_name(&name, NAME_W);
    Line::from(vec![
        marker,
        Span::styled(
            format!("{name} "),
            if speaking { green().bold() } else { strong() },
        ),
        Span::styled(format!("{:<4} ", status(muted)), state_style(muted)),
        Span::styled(format!("{:<4} ", status(deaf)), state_style(deaf)),
        Span::styled(format!("{ping:<7}"), dim()),
        Span::styled(level_bar(level), bar_style),
    ])
}
fn status(disabled: Option<bool>) -> &'static str {
    match disabled {
        Some(true) => "ВЫКЛ",
        Some(false) => "ВКЛ",
        None => "…",
    }
}
fn state_style(disabled: Option<bool>) -> Style {
    match disabled {
        Some(true) => bad(),
        Some(false) => green(),
        None => dim(),
    }
}
fn pad_name(value: &str, max: usize) -> String {
    let mut out = String::new();
    let mut width = 0;
    let truncate = value.width() > max;
    let limit = if truncate { max.saturating_sub(1) } else { max };
    for c in value.chars() {
        let cells = c.width().unwrap_or(0);
        if width + cells > limit {
            break;
        }
        out.push(c);
        width += cells;
    }
    if truncate {
        out.push('…');
        width += 1;
    }
    out.push_str(&" ".repeat(max.saturating_sub(width)));
    out
}
fn level_bar(pct: u8) -> String {
    let n = (pct as usize * BAR_CELLS / 100).min(BAR_CELLS);
    format!("{}{}", "█".repeat(n), "░".repeat(BAR_CELLS - n))
}

fn chat(app: &App, f: &mut Frame, ui: &mut Ui, area: Rect) {
    let focused = matches!(app.focus, Focus::Pane(Pane::Main) | Focus::Input);
    let block = Block::bordered()
        .title(format!(
            " Переписка · микро {}% ",
            app.engine.params.gain_in()
        ))
        .border_style(if focused { accent() } else { dim() });
    let inner = block.inner(area);
    f.render_widget(block, area);
    ui.push_pane(inner, Pane::Main);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let input = Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1);
    let mut msg = Rect {
        height: inner.height - 1,
        ..inner
    };
    if inner.height >= 3 {
        msg.height -= 1;
        f.render_widget(
            Paragraph::new("─".repeat(inner.width as usize)),
            Rect::new(inner.x, inner.y + inner.height - 2, inner.width, 1),
        );
    }
    let visible: String = app
        .input
        .chars()
        .rev()
        .take(input.width.saturating_sub(3) as usize)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(
                "> {visible}{}",
                if app.focus == Focus::Input { "▏" } else { "" }
            ),
            if app.focus == Focus::Input {
                strong()
            } else {
                dim()
            },
        ))),
        input,
    );
    ui.push(input, Action::FocusInput);
    ui.push(msg, Action::Click(Pane::Main, 0));
    let text = Rect {
        width: msg.width.saturating_sub(1),
        ..msg
    };
    let mut cache = app.chat_layout.borrow_mut();
    if !cache.as_ref().is_some_and(|c| {
        c.messages == app.chat.len() && c.width == text.width && c.selected == app.selected_link
    }) {
        *cache = Some(crate::chat_links::LayoutCache {
            messages: app.chat.len(),
            width: text.width,
            selected: app.selected_link,
            layout: crate::chat_links::layout(&app.chat, text.width, app.selected_link),
        });
    }
    let layout = &cache.as_ref().unwrap().layout;
    if app.chat_targets.borrow().as_slice() != layout.targets.as_slice() {
        *app.chat_targets.borrow_mut() = layout.targets.clone();
    }
    let height = text.height as usize;
    let mut start = layout.rows.len().saturating_sub(height + app.chat_scroll);
    if let Some(index) = app.selected_link
        && let Some(&row) = layout.target_rows.get(index)
    {
        if row < start {
            start = row
        } else if row >= start + height {
            start = row.saturating_sub(height.saturating_sub(1));
        }
    }
    let lines = layout
        .rows
        .iter()
        .skip(start)
        .take(height)
        .enumerate()
        .map(|(y, row)| {
            for &(x, width, index) in &row.hits {
                let rect = Rect::new(text.x + x, text.y + y as u16, width, 1);
                app.chat_hits
                    .borrow_mut()
                    .push(crate::chat_links::Hit { area: rect, index });
                ui.push(rect, Action::OpenLink(index));
            }
            row.line.clone()
        })
        .collect::<Vec<_>>();
    if app.chat.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "пока пусто · i писать · F3 файл",
                dim(),
            ))),
            text,
        );
    } else {
        f.render_widget(Paragraph::new(lines), text);
    }
    if layout.rows.len() > height && msg.width > 1 {
        let mut state = ScrollbarState::new(layout.rows.len())
            .position(start)
            .viewport_content_length(height);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight).style(dim()),
            Rect::new(msg.right() - 1, msg.y, 1, msg.height),
            &mut state,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{ChatLine, PeerInfo, Screen};
    use crate::config::Config;
    use crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};
    use ratatui::{Terminal, backend::TestBackend};

    fn app() -> App {
        let mut app = App::for_test(Config {
            name: "Лексей".into(),
            ..Config::default()
        });
        app.screen = Screen::Call;
        app
    }

    fn draw(app: &App, width: u16, height: u16) -> (String, Ui) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut ui = Ui::default();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render(app, frame, &mut ui, area);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let screen = (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        (screen, ui)
    }

    #[test]
    fn shows_both_names_peer_mute_ping_and_chat_author() {
        let mut app = app();
        app.peer = Some(PeerInfo {
            name: "Друг".into(),
            addr: "комната AX3P".into(),
            rtt: Some(83),
            mute_mic: true,
            deaf: false,
            speaking: true,
            level: 50,
            status_known: true,
        });
        app.chat.push(ChatLine {
            mine: true,
            name: "Лексей".into(),
            text: "Привет!".into(),
            status: 1,
            attachment: None,
        });
        for width in [50, 80, 120] {
            let (screen, _) = draw(&app, width, 24);
            assert!(screen.contains("Лексей (вы)"));
            assert!(screen.contains("Лексей (вы): Привет!"));
            assert!(screen.contains("83 мс"));
            let peer = screen.lines().find(|line| line.contains("Друг")).unwrap();
            assert!(peer.contains("ВЫКЛ"));
            assert!(peer.contains("ВКЛ"));
            assert!(!peer.contains("говорит"), "звук собеседника выключен");
        }
    }

    #[test]
    fn unknown_peer_status_is_ellipsis_not_on() {
        let mut app = app();
        app.peer = Some(PeerInfo {
            name: "Друг".into(),
            addr: "127.0.0.1".into(),
            rtt: None,
            mute_mic: false,
            deaf: false,
            speaking: false,
            level: 0,
            status_known: false,
        });
        let (screen, _) = draw(&app, 80, 24);
        let peer = screen.lines().find(|line| line.contains("Друг")).unwrap();
        assert!(peer.contains('…'));
        assert!(!peer.contains("ВКЛ"));
    }

    #[test]
    fn chat_pane_owns_the_wheel_zone() {
        let mut app = app();
        app.chat.push(ChatLine {
            mine: false,
            name: "Друг".into(),
            text: "Привет!".into(),
            status: 3,
            attachment: None,
        });
        let (_, ui) = draw(&app, 80, 24);
        let wheel = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 60,
            row: 10,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(ui.mouse(wheel), Some(Action::Scroll(Pane::Main, 3)));
        assert_eq!(ui.hit(60, 10), Some(Action::Click(Pane::Main, 0)));
        assert_eq!(ui.hit(60, 22), Some(Action::FocusInput));
    }
}
