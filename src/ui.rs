// Отрисовка каркаса (заголовок/подвал/панели). Экран звонка — в call.rs.
// Тексты и подсказки — на русском.

mod call;
pub(crate) mod keymap;
mod mouse;
pub use mouse::Ui;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::action::{Action, Focus, Overlay, Pane};
use crate::app::{App, Screen};
use crate::config::MicMode;

pub(crate) fn dim() -> Style {
    Style::new().fg(Color::DarkGray)
}
pub(crate) fn strong() -> Style {
    Style::new().fg(Color::White).add_modifier(Modifier::BOLD)
}
pub(crate) fn accent() -> Style {
    Style::new().fg(Color::Cyan)
}
pub(crate) fn green() -> Style {
    Style::new().fg(Color::Green)
}
pub(crate) fn bad() -> Style {
    Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)
}

fn centered_rect(area: Rect, w: u16, h: u16) -> Rect {
    let dw = area.width.min(w);
    let dh = area.height.min(h);
    Rect {
        x: area.x + (area.width.saturating_sub(dw)) / 2,
        y: area.y + (area.height.saturating_sub(dh)) / 2,
        width: dw,
        height: dh,
    }
}

fn cut(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        format!(
            "{}…",
            s.chars().take(n.saturating_sub(1)).collect::<String>()
        )
    } else {
        s.to_string()
    }
}

pub fn render(app: &App, f: &mut Frame, ui: &mut Ui) {
    ui.clear();
    let [head, body, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(f.area());
    header(app, f, head);
    match app.screen {
        Screen::Home => render_home(app, f, ui, body),
        Screen::Connect => render_connect(app, f, ui, body),
        Screen::HostWait => render_hostwait(app, f, ui, body),
        Screen::Incoming => render_incoming(app, f, ui, body),
        Screen::Call => call::render(app, f, ui, body),
        Screen::Settings => render_settings(app, f, ui, body),
    }
    footer(app, f, ui, foot);
    overlays(app, f, ui, body);
    toast(app, f, body);
}

// ── ЗАГОЛОВОК / ПОДВАЛ ─────────────────────────────────────────────────────

fn header(app: &App, f: &mut Frame, area: Rect) {
    let (state, style) = match app.screen {
        Screen::Home => ("меню".to_string(), dim()),
        Screen::Connect => ("новый звонок".to_string(), dim()),
        Screen::HostWait => (format!("● ждём звонок · :{}", app.cfg.port), green()),
        Screen::Incoming => ("● входящий звонок".to_string(), bad()),
        Screen::Call => match app.peer.as_ref() {
            Some(p) => (
                format!(
                    "● в звонке · {} · {} · {}",
                    p.name,
                    p.rtt
                        .map(|ms| format!("{ms} мс"))
                        .unwrap_or_else(|| "пинг…".into()),
                    p.addr
                ),
                green(),
            ),
            None => ("● дозвон…".to_string(), green()),
        },
        Screen::Settings => {
            let mut s = String::from("настройки");
            if app.peer.is_some() {
                s.push_str(" · звонок идёт");
            }
            (s, dim())
        }
    };
    let line = Line::from(vec![
        Span::styled(" remember-me ", strong()),
        Span::styled("│ ", dim()),
        Span::styled(state, style),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn footer(app: &App, f: &mut Frame, ui: &mut Ui, area: Rect) {
    if app.screen == Screen::Call && call_buttons(app, f, ui, area) {
        return;
    }
    let mut text = footer_hint(app).to_string();
    let suffix = " · [?] команды";
    if text.chars().count() + suffix.chars().count() <= area.width as usize {
        text.push_str(suffix);
    }
    let left = text.chars().count() as u16;
    f.render_widget(Paragraph::new(Line::from(Span::styled(text, dim()))), area);
    status_right(app, f, area, left);
}

fn footer_hint(app: &App) -> &'static str {
    match app.screen {
        Screen::Home => "↑↓ выбор · ⏎ открыть",
        Screen::Connect => "⏎ позвонить · esc назад",
        Screen::HostWait => "↑↓ выбор · ⏎/y копировать · s настройки · esc отменить",
        Screen::Incoming => "⏎ принять · esc отклонить",
        Screen::Settings => "↑↓ поле · ←→ значение · ⏎ изменить · esc назад",
        Screen::Call => match app.focus {
            Focus::Input => "⏎ отправить · esc назад",
            Focus::Pane(Pane::Main) => "i писать · tab участники · space говорить",
            Focus::Pane(Pane::Side) => "tab чат · m микро · d звук · u завершить",
        },
    }
}

/// Кнопки звонка в подвале. false — не поместились, нужна текстовая подсказка.
fn call_buttons(app: &App, f: &mut Frame, ui: &mut Ui, area: Rect) -> bool {
    let first = match app.focus {
        Focus::Input => ("[ esc назад ]", Action::Cancel),
        _ => ("[ i писать ]", Action::FocusInput),
    };
    let segs: [(&str, Action); 6] = [
        first,
        ("[ m микро ]", Action::ToggleMute),
        ("[ d звук ]", Action::ToggleDeaf),
        ("[ s настройки ]", Action::OpenSettings),
        ("[ u завершить ]", Action::HangUp),
        ("[?]", Action::Help),
    ];
    let total: usize = segs.iter().map(|(t, _)| t.chars().count()).sum::<usize>() + segs.len() - 1;
    if total > area.width as usize {
        return false;
    }
    let mut spans = Vec::with_capacity(segs.len() * 2);
    let mut x = area.x;
    for (i, (text, action)) in segs.iter().enumerate() {
        let w = text.chars().count() as u16;
        ui.push(
            Rect {
                x,
                y: area.y,
                width: w,
                height: 1,
            },
            action.clone(),
        );
        spans.push(Span::styled(*text, accent()));
        if i + 1 < segs.len() {
            spans.push(Span::styled(" ", dim()));
        }
        x += w + 1;
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    status_right(app, f, area, total as u16 + 1);
    true
}

fn status_right(app: &App, f: &mut Frame, area: Rect, left: u16) {
    if app.status.is_empty() {
        return;
    }
    let avail = area.width.saturating_sub(left + 2);
    if avail < 4 {
        return;
    }
    let rect = Rect {
        x: area.x + left + 2,
        y: area.y,
        width: avail,
        height: 1,
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            cut(&app.status, avail as usize),
            dim(),
        )))
        .alignment(Alignment::Right),
        rect,
    );
}

// ── HOME ───────────────────────────────────────────────────────────────────

fn render_home(app: &App, f: &mut Frame, ui: &mut Ui, body: Rect) {
    let (left, right) = if body.width >= 64 {
        let [l, r] = Layout::horizontal([Constraint::Length(28), Constraint::Min(20)]).areas(body);
        (l, r)
    } else {
        let [l, r] = Layout::vertical([Constraint::Length(6), Constraint::Min(4)]).areas(body);
        (l, r)
    };

    let items = ["Слушать (host)", "Позвонить", "Настройки", "Выход"];
    let lines: Vec<Line> = items
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let sel = i == app.home_sel;
            let marker = if sel {
                Span::styled("▸ ", accent().bold())
            } else {
                Span::raw("  ")
            };
            let text = if sel {
                Span::styled(*label, accent())
            } else {
                Span::raw(*label)
            };
            Line::from(vec![marker, text])
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" Действия ")),
        left,
    );
    for i in 0..items.len() {
        if (i as u16) < left.height.saturating_sub(2) {
            ui.push(
                Rect {
                    x: left.x + 1,
                    y: left.y + 1 + i as u16,
                    width: left.width.saturating_sub(2),
                    height: 1,
                },
                Action::Click(Pane::Side, i),
            );
        }
    }

    let server = if app.cfg.server_addr.is_empty() {
        "прямой звонок".to_string()
    } else {
        cut(&app.cfg.server_addr, 26)
    };
    let (ci, co) = app.engine.current_devices();
    let dev = |s: &str| {
        if s.is_empty() {
            "(по умолчанию)".to_string()
        } else {
            cut(s, 26)
        }
    };
    let info = vec![
        Line::from(vec![
            Span::styled("имя  ", dim()),
            Span::styled(app.cfg.name.clone(), Style::new()),
        ]),
        Line::from(vec![
            Span::styled("порт  ", dim()),
            Span::styled(app.cfg.port.to_string(), Style::new()),
        ]),
        Line::from(vec![
            Span::styled("сервер  ", dim()),
            Span::styled(server, Style::new()),
        ]),
        Line::from(vec![
            Span::styled("ввод  ", dim()),
            Span::styled(dev(&ci), Style::new()),
        ]),
        Line::from(vec![
            Span::styled("вывод  ", dim()),
            Span::styled(dev(&co), Style::new()),
        ]),
    ];
    let copies = [
        Some(app.cfg.name.clone()),
        Some(app.cfg.port.to_string()),
        (!app.cfg.server_addr.is_empty()).then(|| app.cfg.server_addr.clone()),
        (!ci.is_empty()).then(|| ci.clone()),
        (!co.is_empty()).then(|| co.clone()),
    ];
    let block = Block::bordered().title(" Информация ");
    let inner = block.inner(right);
    f.render_widget(block, right);
    f.render_widget(Paragraph::new(info), inner);
    for (i, text) in copies.into_iter().enumerate() {
        if let Some(text) = text
            && (i as u16) < inner.height
        {
            ui.push(
                Rect {
                    x: inner.x,
                    y: inner.y + i as u16,
                    width: inner.width,
                    height: 1,
                },
                Action::Copy(text),
            );
        }
    }
    ui.push_pane(body, Pane::Side);
}

// ── CONNECT ────────────────────────────────────────────────────────────────

fn render_connect(app: &App, f: &mut Frame, ui: &mut Ui, body: Rect) {
    let area = centered_rect(body, 64, 9);
    f.render_widget(Clear, area);
    let block = Block::bordered().title(" Позвонить ".bold());
    let inner = block.inner(area);
    f.render_widget(block, area);
    let lines = vec![
        Line::from(Span::styled(
            "Адрес собеседника (ip:порт) или код@ip:порт:",
            dim(),
        )),
        Line::from(Span::styled(format!("> {}▏", app.conn_input), strong())),
        Line::from(Span::styled("192.168.0.112:51500 или КОД@ip:порт", dim())),
        Line::from(""),
        Line::from(vec![
            Span::styled("[ Позвонить ]", accent()),
            Span::raw(" "),
            Span::styled("[ Назад ]", accent()),
        ]),
    ];
    f.render_widget(Paragraph::new(lines), inner);
    if inner.height >= 5 {
        ui.push(
            Rect {
                x: inner.x,
                y: inner.y + 1,
                width: inner.width,
                height: 1,
            },
            Action::FocusInput,
        );
        let y = inner.y + 4;
        ui.push(
            Rect {
                x: inner.x,
                y,
                width: 13,
                height: 1,
            },
            Action::Confirm,
        );
        ui.push(
            Rect {
                x: inner.x + 14,
                y,
                width: 9,
                height: 1,
            },
            Action::Cancel,
        );
    }
    ui.push_pane(body, Pane::Side);
}

// ── HOSTWAIT ───────────────────────────────────────────────────────────────

fn render_hostwait(app: &App, f: &mut Frame, ui: &mut Ui, body: Rect) {
    let parts = if body.width >= 70 {
        Layout::horizontal([Constraint::Min(28), Constraint::Min(20)]).split(body)
    } else {
        Layout::vertical([Constraint::Min(28), Constraint::Min(20)]).split(body)
    };

    let block = Block::bordered().title(" Ждём звонок ");
    let inner = block.inner(parts[0]);
    f.render_widget(block, parts[0]);

    let targets = app.copy_targets();
    let sel = app.host_sel.min(targets.len().saturating_sub(1));
    let mut lines = vec![
        Line::from(Span::styled("Ожидание входящего звонка…", strong())),
        Line::from(""),
    ];
    let mut rows: Vec<(u16, String)> = Vec::new();
    if targets.is_empty() {
        lines.push(Line::from(Span::styled(
            "(не нашли внешних адресов — проверьте сеть)",
            dim(),
        )));
    } else {
        for (i, (label, text)) in targets.iter().enumerate() {
            let selected = i == sel;
            let marker = if selected {
                Span::styled("▸ ", accent().bold())
            } else {
                Span::raw("  ")
            };
            let style = if selected { strong() } else { green() };
            lines.push(Line::from(vec![marker, Span::styled(label.clone(), style)]));
            rows.push(((lines.len() - 1) as u16, text.clone()));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "VPN/LAN — из одной сети, публичный IP — нужен проброс UDP",
        dim(),
    )));
    f.render_widget(Paragraph::new(lines), inner);
    for (y, text) in rows {
        if y < inner.height {
            ui.push(
                Rect {
                    x: inner.x,
                    y: inner.y + y,
                    width: inner.width,
                    height: 1,
                },
                Action::Copy(text),
            );
        }
    }

    let inner_h = parts[1].height.saturating_sub(2) as usize;
    let log: Vec<Line> = if app.log.is_empty() || inner_h == 0 {
        vec![Line::from(Span::styled(app.status.clone(), dim()))]
    } else {
        app.log
            .iter()
            .rev()
            .take(inner_h)
            .rev()
            .map(|l| Line::from(Span::styled(l.clone(), dim())))
            .collect()
    };
    f.render_widget(
        Paragraph::new(log).block(Block::bordered().title(" События ")),
        parts[1],
    );
    ui.push_pane(parts[0], Pane::Side);
    ui.push_pane(parts[1], Pane::Main);
}

// ── INCOMING ───────────────────────────────────────────────────────────────

fn render_incoming(app: &App, f: &mut Frame, ui: &mut Ui, body: Rect) {
    let area = centered_rect(body, 52, 9);
    f.render_widget(Clear, area);
    let block = Block::bordered()
        .border_style(Style::new().fg(Color::Red))
        .title(" Входящий звонок ".bold());
    let inner = block.inner(area);
    f.render_widget(block, area);
    let (name, addr) = app.incoming.clone().unwrap_or_default();
    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("от: ", dim()),
            Span::styled(name, strong()),
        ]),
        Line::from(vec![
            Span::styled("адрес: ", dim()),
            Span::styled(addr, dim()),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("[ Принять ]", accent()),
            Span::raw(" "),
            Span::styled("[ Отклонить ]", accent()),
        ]),
    ];
    f.render_widget(Paragraph::new(lines).centered(), inner);
    if inner.height >= 5 {
        let y = inner.y + 4;
        let x = inner.x + inner.width.saturating_sub(25) / 2;
        ui.push(
            Rect {
                x,
                y,
                width: 11,
                height: 1,
            },
            Action::Accept,
        );
        ui.push(
            Rect {
                x: x + 12,
                y,
                width: 13,
                height: 1,
            },
            Action::Decline,
        );
    }
    ui.push_pane(body, Pane::Side);
}

// ── SETTINGS ───────────────────────────────────────────────────────────────

fn render_settings(app: &App, f: &mut Frame, ui: &mut Ui, body: Rect) {
    if let Some(pick) = &app.dev_pick {
        return render_dev_pick(app, f, body, pick.input, pick.idx);
    }
    let p = app.engine.params.clone();
    let cfg = &app.cfg;
    let cur = app.engine.current_devices();
    let fmt_dev = |s: &String| {
        if s.is_empty() {
            "(по умолчанию)".to_string()
        } else {
            s.clone()
        }
    };

    let rows: Vec<(String, String)> = vec![
        ("имя".into(), cfg.name.clone()),
        ("порт".into(), cfg.port.to_string()),
        ("громкость микрофона".into(), format!("{}%", p.gain_in())),
        (
            "громкость собеседника".into(),
            format!("{}%", p.gain_peer()),
        ),
        (
            "режим микрофона".into(),
            match cfg.mic_mode {
                MicMode::Vox => "голосовая активность (VOX)".into(),
                MicMode::Ptt => "push-to-talk (держать Пробел)".into(),
            },
        ),
        ("порог VOX".into(), p.vad_threshold().to_string()),
        (
            "шумовой гейт".into(),
            if p.noise_gate() {
                "вкл".into()
            } else {
                "выкл".into()
            },
        ),
        ("устройство ввода".into(), fmt_dev(&cur.0)),
        ("устройство вывода".into(), fmt_dev(&cur.1)),
        (
            "эхо-тест [L]".into(),
            if p.loopback() {
                "ИДЁТ (слышите себя)".into()
            } else {
                "выкл".into()
            },
        ),
        (
            "адрес сервера (код-звонок)".into(),
            if cfg.server_addr.is_empty() {
                "(не задан — прямой звонок)".to_string()
            } else {
                cfg.server_addr.clone()
            },
        ),
    ];
    let mut rows = rows;
    rows.push((
        "автогромкость речи".into(),
        if p.auto_gain() {
            "ВКЛ — выравнивание тихой речи".into()
        } else {
            "ВЫКЛ".into()
        },
    ));
    rows.push((
        "фильтр голоса (лёгкий)".into(),
        if p.voice_filter() {
            "ВКЛ".into()
        } else {
            "ВЫКЛ".into()
        },
    ));
    rows.push((
        "шумоподавление".into(),
        if p.noise_suppression() {
            "ВКЛ".into()
        } else {
            "ВЫКЛ".into()
        },
    ));
    let mut lines = Vec::new();
    for (i, (label, val)) in rows.iter().enumerate() {
        let cursor = if i == app.settings_sel && app.edit.is_none() {
            Span::styled("▸ ", accent().bold())
        } else {
            Span::raw("  ")
        };
        let val = if let Some((row, buf)) = &app.edit {
            if *row == i {
                format!("{buf}▏")
            } else {
                val.clone()
            }
        } else {
            val.clone()
        };
        let val_style = if i == app.settings_sel {
            strong()
        } else {
            Style::new()
        };
        lines.push(Line::from(vec![
            cursor,
            Span::raw(label.clone()),
            Span::raw("  "),
            Span::styled(val, val_style),
        ]));
    }
    let mut area = centered_rect(body, 86, rows.len() as u16 + 8);
    area.height = area.height.min(body.height);
    area.y = body.y + (body.height.saturating_sub(area.height)) / 2;
    let title = if app.peer.is_some() {
        " Настройки · звонок продолжается "
    } else {
        " Настройки "
    };
    let block = Block::bordered().title(title.bold());
    let inner = block.inner(area);
    f.render_widget(block, area);
    let parts = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(3),
    ])
    .split(inner);
    let meter = Line::from(Span::styled(
        format!(
            "Микрофон: {}% · VOX: {}{}",
            p.in_level(),
            p.vad_threshold(),
            if p.input_clipping() {
                " · ПЕРЕГРУЗ ВХОДА: уменьшите усиление устройства"
            } else {
                ""
            }
        ),
        if p.input_clipping() { bad() } else { dim() },
    ));
    f.render_widget(Paragraph::new(meter), parts[0]);
    let scroll = (app.settings_sel + 1).saturating_sub(parts[1].height as usize);
    f.render_widget(Paragraph::new(lines).scroll((scroll as u16, 0)), parts[1]);
    let help = match app.settings_sel {
        crate::app::ROW_VAD => "← выше чувствительность · → меньше фонового шума",
        crate::app::ROW_DENOISE => "Enter: подавлять фон и клавиши · обработка на этом ПК",
        crate::app::ROW_FILTER => "Enter: фильтр гула/шипения 80–8000 Гц, без нейросети",
        crate::app::ROW_NAME | crate::app::ROW_PORT | crate::app::ROW_SERVER
            if app.session.is_some() =>
        {
            "Имя, порт и сервер применяются при следующем соединении"
        }
        crate::app::ROW_ECHO if app.session.is_some() => {
            "Эхо-тест доступен после завершения сессии"
        }
        _ => "←/→: значение · Enter/Пробел: переключить или редактировать",
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(help, accent()))),
        parts[2],
    );
    f.render_widget(
        Paragraph::new(vec![
            Line::from("[↑/↓] поле   [←/→/Enter/Пробел] менять"),
            Line::from(if app.session_screen() == Screen::Call {
                "[Esc] вернуться к звонку"
            } else {
                "[Esc] назад"
            }),
            Line::from(if app.session.is_some() {
                "Голос и чат работают, аудионастройки меняются сразу"
            } else {
                "[L] эхо-тест"
            }),
        ])
        .style(dim()),
        parts[3],
    );
    ui.push_pane(body, Pane::Side);
}

fn render_dev_pick(app: &App, f: &mut Frame, body: Rect, input: bool, idx: usize) {
    let list = if input {
        &app.devices.inputs
    } else {
        &app.devices.outputs
    };
    let kind = if input { "ввода" } else { "вывода" };
    let mut lines = vec![Line::from(Span::styled(
        format!("устройство {kind}"),
        dim(),
    ))];
    lines.push(Line::from(vec![
        cursor(idx == 0),
        Span::styled("(по умолчанию)".to_string(), value_style(idx == 0)),
    ]));
    for (i, d) in list.iter().enumerate() {
        let sel = idx == i + 1;
        lines.push(Line::from(vec![
            cursor(sel),
            Span::styled(d.clone(), value_style(sel)),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("[↑/↓]", accent()),
        Span::styled(" выбор   ", Style::new()),
        Span::styled("[Enter]", accent()),
        Span::styled(" применить   ", Style::new()),
        Span::styled("[Esc]", accent()),
        Span::styled(" отмена", Style::new()),
    ]));
    let area = centered_rect(body, 80, lines.len() as u16 + 4);
    let block = Block::bordered().title(" Устройства ".bold());
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn cursor(sel: bool) -> Span<'static> {
    if sel {
        Span::styled("▸ ", accent().bold())
    } else {
        Span::raw("  ")
    }
}

fn value_style(sel: bool) -> Style {
    if sel { strong() } else { Style::new() }
}

// ── ОВЕРЛЕИ / ТОСТ ─────────────────────────────────────────────────────────

fn overlays(app: &App, f: &mut Frame, ui: &mut Ui, body: Rect) {
    match app.overlay {
        Some(Overlay::Help) => {
            let area = centered_rect(body, 60, 18);
            f.render_widget(Clear, area);
            let block = Block::bordered().title(" Клавиши ".bold());
            let inner = block.inner(area);
            f.render_widget(block, area);
            let keys: &[&str] = match app.screen {
                Screen::Home => &[
                    "↑↓  выбор",
                    "⏎  открыть",
                    "h  слушать · c  позвонить",
                    "s  настройки · q  выход",
                ],
                Screen::Connect => &["⏎  позвонить", "esc  назад"],
                Screen::HostWait => &[
                    "↑↓  выбор",
                    "⏎ / y  копировать",
                    "s  настройки · esc  отменить",
                ],
                Screen::Incoming => &["⏎  принять", "esc  отклонить", "s  настройки"],
                Screen::Call => &[
                    "tab  участники / чат",
                    "i  писать · esc  назад",
                    "space  говорить (PTT)",
                    "m  микрофон · d  звук",
                    "s  настройки · u  завершить",
                    "a  автогромкость · [ ]  громкость микрофона",
                    "↑↓  громкость собеседника",
                    "shift+i / shift+o  устройство ввода / вывода",
                ],
                Screen::Settings => &["↑↓  поле", "←→  значение", "⏎  изменить", "esc  назад"],
            };
            let mut lines: Vec<Line> = keys
                .iter()
                .map(|k| Line::from(Span::styled(*k, Style::new())))
                .collect();
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "клик по адресу/коду/значению — копирует",
                dim(),
            )));
            lines.push(Line::from(Span::styled(
                "shift+мышь — выделение терминала",
                dim(),
            )));
            lines.push(Line::from(Span::styled("? / esc — закрыть", dim())));
            f.render_widget(Paragraph::new(lines), inner);
            ui.push(area, Action::Cancel);
        }
        Some(Overlay::HangUp) => {
            let area = centered_rect(body, 36, 7);
            f.render_widget(Clear, area);
            let block = Block::bordered().title(" Завершение ".bold());
            let inner = block.inner(area);
            f.render_widget(block, area);
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled("Завершить звонок?", strong())),
                Line::from(""),
                Line::from(vec![
                    Span::styled("[ Завершить ]", accent()),
                    Span::raw(" "),
                    Span::styled("[ Отмена ]", accent()),
                ]),
            ];
            f.render_widget(Paragraph::new(lines).centered(), inner);
            if inner.height >= 4 {
                let y = inner.y + 3;
                let x = inner.x + inner.width.saturating_sub(24) / 2;
                ui.push(
                    Rect {
                        x,
                        y,
                        width: 13,
                        height: 1,
                    },
                    Action::HangUp,
                );
                ui.push(
                    Rect {
                        x: x + 14,
                        y,
                        width: 10,
                        height: 1,
                    },
                    Action::Cancel,
                );
            }
        }
        None => {}
    }
}

fn toast(app: &App, f: &mut Frame, body: Rect) {
    if let Some((text, _)) = &app.toast {
        let w = (text.chars().count() as u16 + 2).min(body.width);
        if w > 0 {
            let rect = Rect {
                x: body.x + body.width - w,
                y: body.y,
                width: w,
                height: 1,
            };
            f.render_widget(Clear, rect);
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(text.clone(), accent()))),
                rect,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use ratatui::{Terminal, backend::TestBackend};

    fn draw(app: &App, width: u16) -> String {
        draw_ui(app, width, 24).0
    }

    fn draw_ui(app: &App, width: u16, height: u16) -> (String, Ui) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut ui = Ui::default();
        terminal.draw(|frame| render(app, frame, &mut ui)).unwrap();
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

    /// Позиция символа на экране (в символах, не в байтах).
    fn pos(screen: &str, needle: &str) -> (u16, u16) {
        for (y, line) in screen.lines().enumerate() {
            if let Some(byte) = line.find(needle) {
                return (line[..byte].chars().count() as u16, y as u16);
            }
        }
        panic!("не нашли {needle:?} на экране");
    }

    fn app() -> App {
        App::for_test(Config {
            name: "Лексей".into(),
            ..Config::default()
        })
    }

    #[test]
    fn home_lists_four_actions_and_clicks_select_them() {
        let mut app = app();
        app.screen = Screen::Home;
        let (screen, ui) = draw_ui(&app, 100, 30);
        for (i, label) in ["Слушать (host)", "Позвонить", "Настройки", "Выход"]
            .iter()
            .enumerate()
        {
            let (x, y) = pos(&screen, label);
            assert_eq!(ui.hit(x, y), Some(Action::Click(Pane::Side, i)));
        }
    }

    #[test]
    fn connect_registers_call_and_back_buttons() {
        let mut app = app();
        app.screen = Screen::Connect;
        let (screen, ui) = draw_ui(&app, 100, 30);
        let (x, y) = pos(&screen, "[ Позвонить ]");
        assert_eq!(ui.hit(x, y), Some(Action::Confirm));
        let (x, y) = pos(&screen, "[ Назад ]");
        assert_eq!(ui.hit(x, y), Some(Action::Cancel));
    }

    #[test]
    fn call_footer_is_buttons_not_the_old_hint_wall() {
        let mut app = app();
        app.screen = Screen::Call;
        let (screen, ui) = draw_ui(&app, 80, 24);
        let footer = screen.lines().last().unwrap();
        assert!(!(footer.contains("[F2]") && footer.contains("[T]")));
        assert!(footer.contains("[ i писать ]"));
        assert!(footer.contains("[ s настройки ]"));
        assert!(footer.contains("[?]"));
        assert!(!footer.contains("? клавиши"));
        let (x, y) = pos(&screen, "[ i писать ]");
        assert_eq!(ui.hit(x, y), Some(Action::FocusInput));
        let (x, y) = pos(&screen, "[?]");
        assert_eq!(ui.hit(x, y), Some(Action::Help));

        app.focus = Focus::Input;
        let (screen, ui) = draw_ui(&app, 120, 24);
        let footer = screen.lines().last().unwrap();
        assert!(footer.contains("[ esc назад ]"));
        let (x, y) = pos(&screen, "[ m микро ]");
        assert_eq!(ui.hit(x, y), Some(Action::ToggleMute));
        let (x, y) = pos(&screen, "[ s настройки ]");
        assert_eq!(ui.hit(x, y), Some(Action::OpenSettings));
        let (x, y) = pos(&screen, "[ esc назад ]");
        assert_eq!(ui.hit(x, y), Some(Action::Cancel));
    }

    #[test]
    fn address_and_code_are_click_copy_targets() {
        let mut app = app();
        app.screen = Screen::HostWait;
        app.hint_addrs = vec!["wlan0 [LAN] 10.0.0.5:51500".into()];
        app.room_code = Some("AX3P".into());
        app.cfg.server_addr = "1.2.3.4:51900".into();
        let (screen, ui) = draw_ui(&app, 90, 24);
        let (x, y) = pos(&screen, "wlan0");
        assert_eq!(ui.hit(x, y), Some(Action::Copy("10.0.0.5:51500".into())));
        let (x, y) = pos(&screen, "код комнаты");
        assert_eq!(ui.hit(x, y), Some(Action::Copy("AX3P".into())));
        let (x, y) = pos(&screen, "AX3P@");
        assert_eq!(
            ui.hit(x, y),
            Some(Action::Copy("AX3P@1.2.3.4:51900".into()))
        );
    }

    #[test]
    fn home_values_are_click_copy_targets() {
        let app = app();
        let (screen, ui) = draw_ui(&app, 80, 24);
        let (x, y) = pos(&screen, "имя");
        assert_eq!(ui.hit(x, y), Some(Action::Copy("Лексей".into())));
        let (x, y) = pos(&screen, "порт");
        assert_eq!(ui.hit(x, y), Some(Action::Copy("51500".into())));
    }

    #[test]
    fn saved_nickname_is_visible_without_editing_settings() {
        let mut app = app();
        app.screen = Screen::Settings;
        let screen = draw(&app, 80);
        assert!(screen.contains("имя  Лексей"));
        assert!(screen.contains("автогромкость речи"));
        assert!(app.edit.is_none());
    }

    #[test]
    fn live_settings_keep_vox_and_optional_filter_accessible_in_short_terminals() {
        let mut app = app();
        app.screen = Screen::Call;
        app.open_settings();
        for height in [12, 24] {
            app.settings_sel = crate::app::ROW_VAD;
            let screen = draw_ui(&app, 80, height).0;
            assert!(screen.contains("▸ порог VOX  25"));
            assert!(screen.contains("[Esc] вернуться к звонку"));
            app.settings_sel = crate::app::ROW_FILTER;
            let screen = draw_ui(&app, 80, height).0;
            assert!(screen.contains("▸ фильтр голоса (лёгкий)  ВЫКЛ"));
            app.settings_sel = crate::app::ROW_DENOISE;
            let screen = draw_ui(&app, 80, height).0;
            assert!(screen.contains("▸ шумоподавление  ВЫКЛ"));
            app.engine.params.set_noise_suppression(true);
            assert!(
                draw_ui(&app, 80, height)
                    .0
                    .contains("▸ шумоподавление  ВКЛ")
            );
            app.engine.params.set_noise_suppression(false);
        }
    }
}
