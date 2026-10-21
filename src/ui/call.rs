// Экран звонка: участники (микрофон, звук, уровень) и переписка.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};

use super::{Ui, accent, bad, dim, green, strong};
use crate::action::{Action, Focus, Pane};
use crate::app::{App, ChatLine};

/// Ширина колонки имени и смещения кликабельных ячеек в строке участника.
const NAME_W: usize = 12;
const MIC_X: u16 = 15;
const DEAF_X: u16 = 20;
const BAR_CELLS: usize = 8;

pub fn render(app: &App, f: &mut Frame, ui: &mut Ui, area: Rect) {
    if area.width >= 72 {
        let [left, right] =
            Layout::horizontal([Constraint::Length(36), Constraint::Min(20)]).areas(area);
        participants(app, f, ui, left);
        chat(app, f, ui, right);
    } else {
        let [top, bottom] =
            Layout::vertical([Constraint::Length(6), Constraint::Min(6)]).areas(area);
        participants(app, f, ui, top);
        chat(app, f, ui, bottom);
    }
}

fn participants(app: &App, f: &mut Frame, ui: &mut Ui, area: Rect) {
    let focused = app.focus == Focus::Pane(Pane::Side);
    let ping = match app.peer.as_ref().and_then(|p| p.rtt) {
        Some(ms) => format!("{ms} мс"),
        None => "измеряем…".to_string(),
    };
    let block = Block::bordered()
        .title(format!(" Участники · {ping} "))
        .border_style(if focused { accent() } else { dim() });
    let inner = block.inner(area);
    f.render_widget(block, area);
    ui.push_pane(inner, Pane::Side);

    let p = &app.engine.params;
    let self_muted = p.mute_mic();
    let self_speaking = app
        .engine
        .self_speaking
        .load(std::sync::atomic::Ordering::Relaxed);
    let mut lines = vec![row_line(
        focused,
        app.call_sel == 0,
        format!("{} (вы)", app.cfg.name),
        Some(self_muted),
        Some(p.deaf()),
        self_speaking,
        if self_muted { 0 } else { p.in_level() },
        accent(),
    )];
    let peer_line = match &app.peer {
        Some(peer) => row_line(
            focused,
            app.call_sel == 1,
            peer.name.clone(),
            peer.status_known.then_some(peer.mute_mic || peer.deaf),
            peer.status_known.then_some(peer.deaf),
            peer.speaking && peer.status_known && !peer.mute_mic && !peer.deaf,
            if peer.mute_mic { 0 } else { p.out_level() },
            green(),
        ),
        None => row_line(
            focused,
            app.call_sel == 1,
            "Ожидаем собеседника".to_string(),
            None,
            None,
            false,
            0,
            green(),
        ),
    };
    lines.push(peer_line);

    let rows = Rect {
        height: inner.height.min(lines.len() as u16),
        ..inner
    };
    f.render_widget(Paragraph::new(lines), rows);

    for idx in 0..rows.height {
        let y = inner.y + idx;
        ui.push(
            Rect::new(inner.x, y, inner.width, 1),
            Action::Click(Pane::Side, idx as usize),
        );
        if idx == 0 {
            for (offset, action) in [(MIC_X, Action::ToggleMute), (DEAF_X, Action::ToggleDeaf)] {
                if let Some(cell) = clip(Rect::new(inner.x + offset, y, 4, 1), inner) {
                    ui.push(cell, action);
                }
            }
        }
    }
}

/// Строка участника: маркер, имя, микрофон, звук, «говорит», полоска уровня.
fn row_line(
    focused: bool,
    selected: bool,
    name: String,
    muted: Option<bool>,
    deaf: Option<bool>,
    speaking: bool,
    level: u8,
    bar_style: Style,
) -> Line<'static> {
    let marker = if focused && selected {
        Span::styled("▸ ", accent().add_modifier(Modifier::BOLD))
    } else {
        Span::raw("  ")
    };
    let name = truncate(&name, NAME_W);
    let name_style = if speaking {
        green().add_modifier(Modifier::BOLD)
    } else {
        strong()
    };
    Line::from(vec![
        marker,
        Span::styled(format!("{name:<NAME_W$} "), name_style),
        Span::styled(format!("{:<4} ", status(muted)), state_style(muted)),
        Span::styled(format!("{:<4} ", status(deaf)), state_style(deaf)),
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

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let mut out: String = value.chars().take(max - 1).collect();
    out.push('…');
    out
}

/// Полоска уровня: 8 делений, заполнение по процентам.
fn level_bar(pct: u8) -> String {
    let filled = (pct as usize * BAR_CELLS / 100).min(BAR_CELLS);
    format!("{}{}", "█".repeat(filled), "░".repeat(BAR_CELLS - filled))
}

fn clip(rect: Rect, area: Rect) -> Option<Rect> {
    let x1 = rect.x.max(area.x);
    let y1 = rect.y.max(area.y);
    let x2 = (rect.x + rect.width).min(area.x + area.width);
    let y2 = (rect.y + rect.height).min(area.y + area.height);
    (x1 < x2 && y1 < y2).then(|| Rect::new(x1, y1, x2 - x1, y2 - y1))
}

fn chat(app: &App, f: &mut Frame, ui: &mut Ui, area: Rect) {
    let focused = matches!(app.focus, Focus::Pane(Pane::Main) | Focus::Input);
    let block = Block::bordered()
        .title(" Переписка ")
        .border_style(if focused { accent() } else { dim() });
    let inner = block.inner(area);
    f.render_widget(block, area);
    ui.push_pane(inner, Pane::Main);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // нижняя строка блока — ввод сообщения, над ней разделитель
    let input = Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1);
    let mut msg = Rect {
        height: inner.height - 1,
        ..inner
    };
    if inner.height >= 3 {
        msg.height -= 1;
        let sep = Rect::new(inner.x, inner.y + inner.height - 2, inner.width, 1);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "─".repeat(sep.width as usize),
                dim(),
            ))),
            sep,
        );
    }
    let prompt = match app.focus {
        Focus::Input => Line::from(vec![
            Span::styled("> ", dim()),
            Span::styled(format!("{}▏", app.input), strong()),
        ]),
        _ if app.input.is_empty() => Line::from(Span::styled("> ", dim())),
        _ => Line::from(vec![
            Span::styled("> ", dim()),
            Span::raw(app.input.clone()),
        ]),
    };
    f.render_widget(Paragraph::new(prompt), input);
    ui.push(input, Action::FocusInput);
    ui.push(msg, Action::Click(Pane::Main, 0));

    if app.chat.is_empty() {
        if msg.height > 0 {
            f.render_widget(
                Paragraph::new(Line::from(Span::styled("пока пусто", dim()))),
                msg,
            );
        }
        return;
    }

    let total = app.chat.len();
    let inner_h = msg.height as usize;
    let start = total.saturating_sub(inner_h + app.chat_scroll);
    let scrolling = total > inner_h && msg.width > 1;
    let text = Rect {
        width: msg.width - if scrolling { 1 } else { 0 },
        ..msg
    };
    let lines: Vec<Line> = app.chat.iter().map(chat_line).collect();
    f.render_widget(Paragraph::new(lines).scroll((start as u16, 0)), text);

    if scrolling {
        let mut state = ScrollbarState::new(total)
            .position(start)
            .viewport_content_length(inner_h);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight).style(dim()),
            Rect::new(msg.right() - 1, msg.y, 1, msg.height),
            &mut state,
        );
    }
}

fn chat_line(line: &ChatLine) -> Line<'static> {
    let who = if line.mine {
        format!("{} (вы)", line.name)
    } else {
        line.name.clone()
    };
    let style = if line.mine { accent() } else { green() };
    let mut spans = vec![
        Span::styled(format!("{who}:"), style),
        Span::styled(" ", dim()),
        Span::raw(line.text.clone()),
    ];
    if line.mine {
        spans.push(match line.status {
            0 => Span::styled(" …", dim()),
            1 => Span::styled(" ✓", green()),
            2 => Span::styled(" ✗не доставлено", bad()),
            _ => Span::raw(""),
        });
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{PeerInfo, Screen};
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
        });
        let (_, ui) = draw(&app, 80, 24);
        let wheel = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 40,
            row: 10,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(ui.mouse(wheel), Some(Action::Scroll(Pane::Main, 3)));
        assert_eq!(ui.hit(40, 10), Some(Action::Click(Pane::Main, 0)));
        assert_eq!(ui.hit(40, 22), Some(Action::FocusInput));
    }
}
