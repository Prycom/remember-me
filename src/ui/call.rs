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
const BAR_CELLS: usize = 6;

pub fn render(app: &App, f: &mut Frame, ui: &mut Ui, area: Rect) {
    if area.width >= 86 {
        let [left, right] =
            Layout::horizontal([Constraint::Length(44), Constraint::Min(20)]).areas(area);
        participants(app, f, ui, left);
        chat(app, f, ui, right);
    } else {
        let count = app.participant_count() as u16;
        let height = (count * 3 + 2)
            .max(6)
            .min(12)
            .min(area.height.saturating_sub(6));
        let [top, bottom] =
            Layout::vertical([Constraint::Length(height), Constraint::Min(3)]).areas(area);
        participants(app, f, ui, top);
        chat(app, f, ui, bottom);
    }
}
struct MemberCard<'a> {
    name: String,
    avatar: &'a crate::avatar::Avatar,
    mute: Option<bool>,
    deaf: Option<bool>,
    speaking: bool,
    level: u8,
    ping: String,
}
fn participants(app: &App, f: &mut Frame, ui: &mut Ui, area: Rect) {
    let focused = app.focus == Focus::Pane(Pane::Side);
    let p = &app.engine.params;
    let block = Block::bordered()
        .title(format!(
            " Участники · {} · {}% ",
            app.participant_count(),
            p.gain_peer()
        ))
        .border_style(if focused { accent() } else { dim() });
    let inner = block.inner(area);
    f.render_widget(block, area);
    ui.push_pane(inner, Pane::Side);
    if inner.height == 0 || inner.width < 8 {
        return;
    }
    let mut cards = vec![MemberCard {
        name: format!("{} (вы)", app.cfg.name),
        avatar: &app.avatar,
        mute: Some(p.mute_mic()),
        deaf: Some(p.deaf()),
        speaking: app
            .engine
            .self_speaking
            .load(std::sync::atomic::Ordering::Relaxed)
            && !p.mute_mic(),
        level: if p.mute_mic() { 0 } else { p.in_level() },
        ping: "вы".into(),
    }];
    let waiting = crate::avatar::Avatar::identicon("waiting");
    if app.group_room.is_some() {
        for peer in &app.group_peers {
            cards.push(MemberCard {
                name: peer.name.clone(),
                avatar: &peer.avatar,
                mute: peer.known.then_some(peer.mute || peer.deaf),
                deaf: peer.known.then_some(peer.deaf),
                speaking: peer.speaking && peer.known && !peer.mute && !peer.deaf,
                level: if peer.mute || peer.deaf {
                    0
                } else {
                    peer.level
                },
                ping: peer.rtt.map_or_else(|| "…".into(), |ms| format!("{ms} мс")),
            });
        }
    } else if let Some(peer) = &app.peer {
        cards.push(MemberCard {
            name: peer.name.clone(),
            avatar: &peer.avatar,
            mute: peer.status_known.then_some(peer.mute_mic || peer.deaf),
            deaf: peer.status_known.then_some(peer.deaf),
            speaking: peer.speaking && peer.status_known && !peer.mute_mic && !peer.deaf,
            level: if peer.mute_mic { 0 } else { p.out_level() },
            ping: peer.rtt.map_or_else(|| "…".into(), |ms| format!("{ms} мс")),
        });
    } else {
        cards.push(MemberCard {
            name: "Ожидаем собеседника".into(),
            avatar: &waiting,
            mute: None,
            deaf: None,
            speaking: false,
            level: 0,
            ping: "…".into(),
        });
    }
    let selected = app.call_sel.min(cards.len() - 1);
    let portrait_height = if inner.height >= 28 && inner.width >= 32 {
        18
    } else {
        0
    };
    let list_height = inner.height.saturating_sub(portrait_height);
    let visible = (list_height as usize / 3).max(1);
    let start = (selected + 1)
        .saturating_sub(visible)
        .min(cards.len().saturating_sub(visible));
    for (index, card) in cards.iter().enumerate().skip(start).take(visible) {
        let y = inner.y + ((index - start) * 3) as u16;
        let height = 3.min(inner.y + list_height - y);
        if height == 0 {
            continue;
        }
        let row = Rect::new(inner.x, y, inner.width, height);
        ui.push(row, Action::Click(Pane::Side, index));
        let marker = if card.speaking {
            green()
        } else if focused && selected == index {
            accent()
        } else {
            dim()
        };
        f.render_widget(
            Paragraph::new("▎\n▎\n▎").style(marker),
            Rect::new(row.x, row.y, 1, row.height),
        );
        f.render_widget(
            crate::avatar::Portrait {
                avatar: card.avatar,
                mode: app.cfg.avatar_style,
            },
            Rect::new(row.x + 1, row.y, 6, row.height),
        );
        ui.push(
            Rect::new(row.x + 1, row.y, 6, row.height),
            Action::ShowProfile(index),
        );
        let x = row.x + 8;
        let width = row.width.saturating_sub(8);
        let name = pad_name(&card.name, width.saturating_sub(8) as usize);
        let header = Line::from(vec![
            Span::styled(
                name,
                if card.speaking {
                    green().add_modifier(Modifier::BOLD)
                } else {
                    strong()
                },
            ),
            Span::styled(format!(" {}", card.ping), dim()),
        ]);
        let states = Line::from(vec![
            Span::styled("микро ", dim()),
            Span::styled(status(card.mute), state_style(card.mute)),
            Span::styled("  звук ", dim()),
            Span::styled(status(card.deaf), state_style(card.deaf)),
        ]);
        let live = if card.speaking {
            "говорит"
        } else if card.mute == Some(true) {
            "микро выкл"
        } else if card.mute.is_some() {
            "в комнате"
        } else {
            "соединяемся"
        };
        let meter = Line::from(vec![
            Span::styled(
                format!("{} ", level_bar(card.level)),
                if card.speaking { green() } else { accent() },
            ),
            Span::styled(live, dim()),
        ]);
        f.render_widget(
            Paragraph::new(vec![header, states, meter]),
            Rect::new(x, y, width, height),
        );
        if index == 0 && height >= 2 {
            if width >= 9 {
                ui.push(Rect::new(x, y + 1, 9, 1), Action::ToggleMute);
            }
            if width >= 20 {
                ui.push(Rect::new(x + 11, y + 1, 9, 1), Action::ToggleDeaf);
            }
        }
    }
    if portrait_height > 0 {
        let card = &cards[selected];
        let area = Rect::new(
            inner.x,
            inner.bottom() - portrait_height,
            inner.width,
            portrait_height,
        );
        let title = format!(
            " {} ",
            pad_name(&card.name, area.width.saturating_sub(4) as usize).trim_end()
        );
        let block = Block::bordered()
            .title(title)
            .title_bottom(if selected == 0 {
                " клик: открыть профиль "
            } else {
                " профиль участника "
            })
            .border_style(if card.speaking {
                green()
            } else if focused {
                accent()
            } else {
                dim()
            });
        let image = block.inner(area);
        f.render_widget(block, area);
        f.render_widget(
            crate::avatar::Portrait {
                avatar: card.avatar,
                mode: app.cfg.avatar_style,
            },
            image,
        );
        ui.push(image, Action::ShowProfile(selected));
    }
}
fn status(disabled: Option<bool>) -> &'static str {
    match disabled {
        Some(true) => "ВЫКЛ",
        Some(false) => "ВКЛ",
        None => "…",
    }
}

pub(super) fn render_profile(app: &App, f: &mut Frame, ui: &mut Ui, body: Rect) {
    let p = &app.engine.params;
    let (name, avatar, mute, deaf, ping, speaking) = if app.call_sel == 0 {
        (
            format!("{} (вы)", app.cfg.name),
            &app.avatar,
            Some(p.mute_mic()),
            Some(p.deaf()),
            "Ваш профиль".into(),
            app.engine
                .self_speaking
                .load(std::sync::atomic::Ordering::Relaxed)
                && !p.mute_mic(),
        )
    } else if app.group_room.is_some()
        && let Some(peer) = app.group_peers.get(app.call_sel - 1)
    {
        (
            peer.name.clone(),
            &peer.avatar,
            peer.known.then_some(peer.mute || peer.deaf),
            peer.known.then_some(peer.deaf),
            peer.rtt
                .map_or_else(|| "измеряем пинг…".into(), |ms| format!("{ms} мс")),
            peer.speaking && !peer.mute && !peer.deaf,
        )
    } else if let Some(peer) = &app.peer {
        (
            peer.name.clone(),
            &peer.avatar,
            peer.status_known.then_some(peer.mute_mic || peer.deaf),
            peer.status_known.then_some(peer.deaf),
            peer.rtt
                .map_or_else(|| "измеряем пинг…".into(), |ms| format!("{ms} мс")),
            peer.speaking && !peer.mute_mic && !peer.deaf,
        )
    } else {
        (
            app.cfg.name.clone(),
            &app.avatar,
            Some(p.mute_mic()),
            Some(p.deaf()),
            "Ваш профиль".into(),
            false,
        )
    };
    let area = super::centered_rect(body, 48, 24);
    f.render_widget(ratatui::widgets::Clear, area);
    let block = Block::bordered()
        .title(format!(
            " {} ",
            pad_name(&name, area.width.saturating_sub(4) as usize).trim_end()
        ))
        .title_bottom(" ← → участники · Esc закрыть ")
        .border_style(if speaking { green() } else { accent() });
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [info, image, buttons] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    let states = Line::from(vec![
        Span::styled("микро ", dim()),
        Span::styled(status(mute), state_style(mute)),
        Span::styled(" · звук ", dim()),
        Span::styled(status(deaf), state_style(deaf)),
        Span::styled(format!(" · {ping}"), dim()),
    ]);
    f.render_widget(Paragraph::new(states).centered(), info);
    f.render_widget(
        crate::avatar::Portrait {
            avatar,
            mode: app.cfg.avatar_style,
        },
        image,
    );
    let choices = if app.call_sel == 0 {
        vec![
            ("[ A картинка ]", Action::EditAvatar),
            ("[ V вид ]", Action::CycleAvatars),
            ("[ Закрыть ]", Action::Cancel),
        ]
    } else {
        vec![
            ("[ V пиксели / ASCII ]", Action::CycleAvatars),
            ("[ Закрыть ]", Action::Cancel),
        ]
    };
    let text = choices
        .iter()
        .map(|(label, _)| *label)
        .collect::<Vec<_>>()
        .join("  ");
    let total = text.chars().count() as u16;
    f.render_widget(Paragraph::new(text).style(accent()).centered(), buttons);
    if buttons.width >= total {
        let mut x = buttons.x + (buttons.width - total) / 2;
        for (label, action) in choices {
            let width = label.chars().count() as u16;
            ui.push(Rect::new(x, buttons.y, width, buttons.height), action);
            x += width + 2;
        }
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
            avatar: crate::avatar::display(None, "Друг"),
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
            let lines = screen.lines().collect::<Vec<_>>();
            let row = lines.iter().position(|line| line.contains("Друг")).unwrap();
            assert!(lines[row + 1].contains("ВЫКЛ"));
            assert!(lines[row + 1].contains("ВКЛ"));
            assert!(
                !lines[row + 2].contains("говорит"),
                "звук собеседника выключен"
            );
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
            avatar: crate::avatar::display(None, "Друг"),
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
