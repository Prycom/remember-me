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
        let [left, right] = Layout::horizontal([
            Constraint::Length(44.min(area.width / 2)),
            Constraint::Min(20),
        ])
        .areas(area);
        participants(app, f, ui, left);
        chat(app, f, ui, right);
    } else {
        let count = app.participant_count() as u16;
        let height = (count * 5 + 1)
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
    avatar: &'a std::sync::Arc<crate::avatar::Avatar>,
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
    static WAITING: std::sync::OnceLock<std::sync::Arc<crate::avatar::Avatar>> =
        std::sync::OnceLock::new();
    let waiting = WAITING.get_or_init(|| crate::avatar::display(None, "waiting"));
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
            avatar: waiting,
            mute: None,
            deaf: None,
            speaking: false,
            level: 0,
            ping: "…".into(),
        });
    }
    let selected = app.call_sel.min(cards.len() - 1);
    // Resolution controls image detail, never the size of the participant card.
    let thumbnail =
        (if inner.height < 8 { 6u16 } else { 8 }).min(inner.width.saturating_sub(22)) / 2 * 2;
    let card_height = (thumbnail / 2).max(3);
    let stride = card_height + 1; // A full blank row between participants.
    let list_height = inner.height;
    let visible = ((list_height as usize + 1) / stride as usize).max(1);
    let start = (selected + 1)
        .saturating_sub(visible)
        .min(cards.len().saturating_sub(visible));
    for (index, card) in cards.iter().enumerate().skip(start).take(visible) {
        let y = inner.y + (index - start) as u16 * stride;
        let height = card_height.min(inner.y + list_height - y);
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
            Paragraph::new((0..height).map(|_| "▎").collect::<Vec<_>>().join("\n")).style(marker),
            Rect::new(row.x, row.y, 1, row.height),
        );
        let image = Rect::new(row.x + 1, row.y, thumbnail, row.height);
        super::draw_avatar(app, f, ui, card.avatar, image, false);
        ui.push(
            Rect::new(row.x + 1, row.y, thumbnail, row.height),
            Action::ShowProfile(index),
        );
        let x = row.x + thumbnail + 2;
        let width = row.width.saturating_sub(thumbnail + 2);
        let text_y = y + height.saturating_sub(3) / 2;
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
            ""
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
            Rect::new(x, text_y, width, 3.min(height)),
        );
        if index == 0 && height >= 2 {
            if width >= 9 {
                ui.push(Rect::new(x, text_y + 1, 9, 1), Action::ToggleMute);
            }
            if width >= 20 {
                ui.push(Rect::new(x + 11, text_y + 1, 9, 1), Action::ToggleDeaf);
            }
        }
    }
}

fn status(disabled: Option<bool>) -> &'static str {
    match disabled {
        Some(true) => "ВЫКЛ",
        Some(false) => "ВКЛ",
        None => "…",
    }
}

pub(super) fn profile_rect(app: &App, body: Rect) -> Rect {
    let avatar = if app.call_sel == 0 {
        &app.avatar
    } else if app.group_room.is_some() {
        app.group_peers
            .get(app.call_sel - 1)
            .map(|p| &p.avatar)
            .unwrap_or(&app.avatar)
    } else {
        app.peer.as_ref().map(|p| &p.avatar).unwrap_or(&app.avatar)
    };
    let side = (avatar.side() as u16)
        .min(body.width.saturating_sub(4))
        .min(body.height.saturating_sub(7).saturating_mul(2));
    super::centered_rect(body, (side + 4).max(48), (side / 2 + 7).max(24))
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
    let area = profile_rect(app, body);
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
    super::draw_avatar(app, f, ui, avatar, image, true);
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

pub(super) fn input_window(text: &str, cursor: usize, cells: usize) -> (String, u16) {
    if cells == 0 {
        return (String::new(), 0);
    }
    let mut cursor = cursor.min(text.len());
    while !text.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let budget = if cursor == text.len() {
        cells.saturating_sub(1)
    } else {
        cells / 2
    };
    let mut start = cursor;
    let mut before = 0;
    for (i, c) in text[..cursor].char_indices().rev() {
        let width = c.width().unwrap_or(0);
        if before + width > budget {
            break;
        }
        before += width;
        start = i;
    }
    let mut visible = String::new();
    let mut used = 0;
    for c in text[start..].chars() {
        let width = c.width().unwrap_or(0);
        if used + width > cells {
            break;
        }
        visible.push(c);
        used += width;
    }
    (visible, before as u16)
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
    let (visible, cursor) = input_window(
        &app.input,
        app.input_cursor,
        input.width.saturating_sub(3) as usize,
    );
    f.render_widget(
        Paragraph::new(format!("> {visible}")).style(if app.focus == Focus::Input {
            strong()
        } else {
            dim()
        }),
        input,
    );
    if app.focus == Focus::Input
        && app.overlay.is_none()
        && app.file_input.is_none()
        && input.width > 2
    {
        f.set_cursor_position((input.x + 2 + cursor, input.y));
    }
    if inner.height >= 3
        && app.focus == Focus::Pane(Pane::Main)
        && let Some(file) = app.selected_file()
        && let crate::transfer::TransferStatus::Offered { expires_at } = file.status
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let seconds = expires_at.saturating_sub(now);
        let label = format!(
            "[ Y скачать ] [ N отказаться ] · {}:{:02}",
            seconds / 60,
            seconds % 60
        );
        let area = Rect::new(inner.x, input.y - 1, inner.width, 1);
        f.render_widget(Paragraph::new(label).style(accent()), area);
        ui.push(
            Rect::new(area.x, area.y, 13.min(area.width), 1),
            Action::FileDecision(true),
        );
        if area.width > 14 {
            ui.push(
                Rect::new(area.x + 14, area.y, 15.min(area.width - 14), 1),
                Action::FileDecision(false),
            );
        }
    }
    ui.push(input, Action::FocusInput);
    ui.push(
        msg,
        Action::Click(Pane::Main, app.chat.len().saturating_sub(1)),
    );
    let text = Rect {
        width: msg.width.saturating_sub(1),
        ..msg
    };
    let layout_width = text.width.saturating_sub(4);
    let mut cache = app.chat_layout.borrow_mut();
    if !cache.as_ref().is_some_and(|c| {
        c.messages == app.chat.len() && c.width == layout_width && c.selected == app.selected_link
    }) {
        *cache = Some(crate::chat_links::LayoutCache {
            messages: app.chat.len(),
            width: layout_width,
            selected: app.selected_link,
            layout: crate::chat_links::layout(&app.chat, layout_width, app.selected_link),
        });
    }
    let layout = &cache.as_ref().unwrap().layout;
    if app.chat_targets.borrow().as_slice() != layout.targets.as_slice() {
        *app.chat_targets.borrow_mut() = layout.targets.clone();
    }
    let height = text.height as usize;
    app.chat_view_height.set(height);
    let mut start = layout.rows.len().saturating_sub(height + app.chat_scroll);
    if let Some(selected) = app.chat_selected {
        let first = layout.rows.iter().position(|r| r.message == selected);
        let last = layout.rows.iter().rposition(|r| r.message == selected);
        if let (Some(first), Some(last)) = (first, last) {
            if last - first + 1 > height {
                start = first + app.chat_scroll.min(last - first + 1 - height);
            } else if first < start {
                start = first;
            } else if last >= start + height {
                start = (last + 1).saturating_sub(height).min(first);
            }
        }
    }
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
            ui.push(
                Rect::new(text.x, text.y + y as u16, text.width, 1),
                Action::Click(Pane::Main, row.message),
            );
            for &(x, width, index) in &row.hits {
                let rect = Rect::new(text.x + 2 + x, text.y + y as u16, width, 1);
                app.chat_hits
                    .borrow_mut()
                    .push(crate::chat_links::Hit { area: rect, index });
                ui.push(rect, Action::OpenLink(index));
            }
            let selected =
                app.chat_selected == Some(row.message) && app.focus == Focus::Pane(Pane::Main);
            let mut line = row.line.clone();
            line.spans.insert(
                0,
                Span::styled(if selected { "[ " } else { "  " }, accent()),
            );
            if selected {
                line.spans.push(Span::styled(" ]", accent()));
                line = line.style(Style::new().bg(ratatui::style::Color::Rgb(24, 35, 49)));
            }
            line
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
    fn page_navigation_reveals_the_end_of_a_wrapped_message() {
        let mut app = app();
        app.focus = Focus::Pane(Pane::Main);
        app.chat_selected = Some(0);
        app.chat.push(ChatLine {
            mine: false,
            name: "Friend".into(),
            text: format!("{}LASTPART", "wrapped ".repeat(120)),
            status: 3,
            attachment: None,
        });
        let (before, _) = draw(&app, 100, 12);
        assert!(!before.contains("LASTPART"));
        let rows = app.chat_layout.borrow().as_ref().unwrap().layout.rows.len();
        let height = app.chat_view_height.get();
        for _ in 0..(rows - height).div_ceil(height) {
            app.on_action(Action::PageDown);
        }
        let (after, _) = draw(&app, 100, 12);
        assert!(after.contains("LASTPART"));
        assert_eq!(app.chat_selected, Some(0));
    }
    #[test]
    fn input_viewport_keeps_wide_unicode_cursor_inside_the_line() {
        for text in ["А🙂界abcxyz", "abcdefghijklmnop", "é🙂"] {
            for cursor in text
                .char_indices()
                .map(|(i, _)| i)
                .chain(std::iter::once(text.len()))
            {
                for width in 1..12 {
                    let (visible, column) = input_window(text, cursor, width);
                    assert!(visible.width() <= width);
                    assert!((column as usize) < width);
                }
            }
        }
    }
    #[test]
    fn history_selection_scrolls_highlights_and_offers_have_context_actions() {
        let mut app = app();
        for n in 0..40 {
            app.chat.push(ChatLine {
                mine: false,
                name: "Friend".into(),
                text: format!("message {n}"),
                status: 3,
                attachment: None,
            });
        }
        app.chat_selected = Some(2);
        app.focus = Focus::Pane(Pane::Main);
        let (screen, _) = draw(&app, 120, 24);
        assert!(screen.contains("[ Friend: message 2 ]"));
        assert!(!screen.contains("message 39"));
        let expires_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 120;
        app.chat[2].attachment = Some(crate::transfer::TransferUpdate {
            id: 22,
            sending: false,
            peer_id: Some(7),
            peer_name: None,
            name: "choice.txt".into(),
            size: 12,
            done: 0,
            status: crate::transfer::TransferStatus::Offered { expires_at },
            path: None,
        });
        app.chat_layout.get_mut().take();
        let (screen, ui) = draw(&app, 140, 28);
        assert!(screen.contains("[ Y скачать ]"));
        let row = screen
            .lines()
            .position(|s| s.contains("[ Y скачать ]"))
            .unwrap() as u16;
        let line = screen.lines().nth(row as usize).unwrap();
        let x = line.chars().position(|c| c == '[').unwrap() as u16;
        assert_eq!(ui.hit(x + 2, row), Some(Action::FileDecision(true)));
        app.chat[2].attachment.as_mut().unwrap().status = crate::transfer::TransferStatus::Complete;
        app.chat[2].attachment.as_mut().unwrap().path =
            Some(std::path::PathBuf::from("/tmp/choice.txt"));
        app.chat_layout.get_mut().take();
        let (_, ui) = draw(&app, 140, 28);
        assert_eq!(
            app.chat_targets.borrow().as_slice(),
            &[crate::chat_links::OpenTarget::File(
                std::path::PathBuf::from("/tmp/choice.txt")
            )]
        );
        let hit = app.chat_hits.borrow().first().unwrap().clone();
        assert_eq!(
            ui.hit(hit.area.x, hit.area.y),
            Some(Action::OpenLink(hit.index))
        );
    }
    #[test]
    fn avatar_resolution_never_enlarges_cards_and_cards_have_a_blank_row_between_them() {
        let mut app = app();
        app.avatar = std::sync::Arc::new(crate::avatar::Avatar::identicon("large").resized(256));
        let capture = |app: &App| {
            let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
            let mut ui = Ui::default();
            terminal
                .draw(|f| render(app, f, &mut ui, f.area()))
                .unwrap();
            (terminal.backend().buffer().clone(), ui)
        };
        app.cfg.avatar_resolution = 32;
        let (small, _) = capture(&app);
        app.cfg.avatar_resolution = 256;
        let (large, ui) = capture(&app);
        assert_eq!(small, large, "resolution changed the thumbnail rectangle");
        assert!(ui.hit(3, 2) == Some(Action::ShowProfile(0)));
        assert!(ui.hit(3, 7) == Some(Action::ShowProfile(1)));
        for x in 1..43 {
            assert_eq!(large[(x, 5)].symbol(), " ", "cards have no gap at {x}");
        }
        assert!(
            !large
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>()
                .contains("в комнате")
        );
        let (screen, _) = draw(&app, 80, 24);
        assert!(screen.contains("Лексей (вы)"));
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
            avatar_custom: false,
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
            avatar_custom: false,
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
            row: 12,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(ui.mouse(wheel), Some(Action::Scroll(Pane::Main, 3)));
        assert_eq!(ui.hit(60, 12), Some(Action::Click(Pane::Main, 0)));
        assert_eq!(ui.hit(60, 22), Some(Action::FocusInput));
    }
}
