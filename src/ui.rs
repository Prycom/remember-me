// Отрисовка экранов (ratatui). Тексты и подсказки — на русском.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Clear, Paragraph, Row, Table};

use crate::app::{App, Screen};
use crate::config::MicMode;

fn dim() -> Style {
    Style::new().fg(Color::DarkGray)
}
fn strong() -> Style {
    Style::new().fg(Color::White).add_modifier(Modifier::BOLD)
}
fn accent() -> Style {
    Style::new().fg(Color::Cyan)
}
fn green() -> Style {
    Style::new().fg(Color::Green)
}
fn bad() -> Style {
    Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)
}

/// нижняя строка-подсказка/статус
fn footer(f: &mut Frame, app: &App) {
    let area = Rect {
        y: f.area().height.saturating_sub(1),
        height: 1,
        ..f.area()
    };
    let text = if let Some((t, _)) = &app.toast {
        Line::from(Span::styled(t.clone(), accent()))
    } else {
        Line::from(Span::styled(app.status.clone(), dim()))
    };
    f.render_widget(Paragraph::new(text), area);
}

fn centered_rect(f: &Frame, w: u16, h: u16) -> Rect {
    let a = f.area();
    let dw = a.width.min(w);
    let dh = a.height.min(h);
    Rect {
        x: a.x + (a.width.saturating_sub(dw)) / 2,
        y: a.y + (a.height.saturating_sub(dh)) / 2,
        width: dw,
        height: dh,
    }
}

pub fn render(app: &App, f: &mut Frame) {
    match app.screen {
        Screen::Home => render_home(app, f),
        Screen::Connect => render_connect(app, f),
        Screen::HostWait => render_hostwait(app, f),
        Screen::Incoming => render_incoming(app, f),
        Screen::Call => render_call(app, f),
        Screen::Settings => render_settings(app, f),
    }
    footer(f, app);
}

// ── HOME ───────────────────────────────────────────────────────────────────

fn render_home(app: &App, f: &mut Frame) {
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "remember-me — голосовой звонок и переписка",
            strong(),
        )),
        Line::from(Span::styled("Напрямую или через ваш сервер", dim())),
        Line::from(""),
        Line::from(vec![
            Span::styled(" [H]", accent()),
            Span::styled("  слушать (host) — ждём собеседника", Style::new()),
        ]),
        Line::from(vec![
            Span::styled(" [C]", accent()),
            Span::styled("  позвонить (connect к ip:порт)", Style::new()),
        ]),
        Line::from(vec![
            Span::styled(" [S]", accent()),
            Span::styled("  настройки: громкости, микрофон, устройства", Style::new()),
        ]),
        Line::from(vec![
            Span::styled(" [Q]", accent()),
            Span::styled("  выход", Style::new()),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            " CLI: remember-me --host | remember-me --connect ip:порт",
            dim(),
        )),
        Line::from(Span::styled(
            " Для звонка через сервер укажите его адрес в настройках.",
            dim(),
        )),
    ];
    let mut lines = lines;
    lines.insert(
        3,
        Line::from(Span::styled(format!("Вы: {}", app.cfg.name), accent())),
    );
    let area = centered_rect(f, 66, lines.len() as u16 + 3);
    let block = Block::bordered()
        .title(" remember-me ".bold())
        .title_alignment(Alignment::Center);
    f.render_widget(Paragraph::new(lines).centered().block(block), area);
}

// ── CONNECT ────────────────────────────────────────────────────────────────

fn render_connect(app: &App, f: &mut Frame) {
    let lines = vec![
        Line::from(Span::styled(
            "Адрес собеседника (ip:порт) или звук через сервер:",
            dim(),
        )),
        Line::from(Span::styled("  код@ip:порт", dim())),
        Line::from(Span::styled(format!("  > {}▏", app.conn_input), strong())),
        Line::from(""),
        Line::from(vec![
            Span::styled(" [Enter]", accent()),
            Span::styled(" звонить   ", Style::new()),
            Span::styled("[Esc]", accent()),
            Span::styled(" назад", Style::new()),
        ]),
    ];
    let area = centered_rect(f, 60, lines.len() as u16 + 3);
    let block = Block::bordered()
        .title(" Позвонить ".bold())
        .title_alignment(Alignment::Center);
    f.render_widget(Paragraph::new(lines).block(block), area);
}

// ── HOSTWAIT ───────────────────────────────────────────────────────────────

fn render_hostwait(app: &App, f: &mut Frame) {
    let mut lines = vec![
        Line::from(Span::styled("Ожидание входящего звонка…", strong())),
        Line::from(""),
        Line::from(Span::styled(
            "адреса интерфейсов этой машины (дайте собеседнику один для звонка):",
            dim(),
        )),
    ];
    if app.hint_addrs.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (не нашли внешних адресов — проверьте сеть)",
            dim(),
        )));
    } else {
        for a in &app.hint_addrs {
            lines.push(Line::from(Span::styled(format!("  {a}"), green())));
        }
    }
    if let Some(code) = &app.room_code {
        let server_line = app.cfg.server_addr.clone();
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("код комнаты: ", dim()),
            Span::styled(code.clone(), accent().bold()),
            Span::styled(format!("  (сервер {server_line})"), dim()),
        ]));
        lines.push(Line::from(Span::styled(
            format!("  собеседник звонит: {code}@{server_line}"),
            green(),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " [VPN] — адрес внутри туннеля (если вы оба в одной VPN — звоните на него),",
        dim(),
    )));
    lines.push(Line::from(Span::styled(
        " [LAN] — местная сеть (если вы в одной wifi/eth-сети). Публичного IP",
        dim(),
    )));
    lines.push(Line::from(Span::styled(
        " «как меня видит интернет» тут нет — чтобы звонить на него, нужен",
        dim(),
    )));
    lines.push(Line::from(Span::styled(
        " проброс UDP-порта. Звонит тот, кто может дойти до адреса собеседника.",
        dim(),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(" [Esc]", accent()),
        Span::styled(" назад", Style::new()),
    ]));
    let area = centered_rect(f, 70, lines.len() as u16 + 3);
    let block = Block::bordered()
        .title(" Звоню?? Нет — слушаю ".bold())
        .title_alignment(Alignment::Center);
    f.render_widget(Paragraph::new(lines).block(block), area);
}

// ── INCOMING ───────────────────────────────────────────────────────────────

fn render_incoming(app: &App, f: &mut Frame) {
    let lines = vec![
        Line::from(Span::styled("● входящий звонок", bad())),
        Line::from(""),
        Line::from(vec![
            Span::styled("от: ", dim()),
            Span::styled(
                app.incoming
                    .as_ref()
                    .map(|(n, _)| n.clone())
                    .unwrap_or_default(),
                strong(),
            ),
        ]),
        Line::from(vec![
            Span::styled("адрес: ", dim()),
            Span::styled(
                app.incoming
                    .as_ref()
                    .map(|(_, a)| a.clone())
                    .unwrap_or_default(),
                dim(),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(" [Enter]", accent()),
            Span::styled(" принять   ", Style::new()),
            Span::styled("[Esc]", accent()),
            Span::styled(" отклонить", Style::new()),
        ]),
    ];
    let area = centered_rect(f, 52, lines.len() as u16 + 3);
    let block = Block::bordered()
        .border_style(Style::new().fg(Color::Red))
        .title(" Кто там? ");
    f.render_widget(Paragraph::new(lines).block(block), area);
}

// ── CALL ───────────────────────────────────────────────────────────────────

fn render_call(app: &App, f: &mut Frame) {
    let area = Rect {
        height: f.area().height.saturating_sub(1),
        ..f.area()
    };
    let chunks = Layout::vertical([
        Constraint::Length(5),
        Constraint::Min(4),
        Constraint::Length(2),
        Constraint::Length(3),
    ])
    .split(area);

    let p = &app.engine.params;
    let me_speaking = app
        .engine
        .self_speaking
        .load(std::sync::atomic::Ordering::Relaxed);
    let compact = area.width < 64;
    let mut rows = vec![participant_row(
        format!("{} (вы)", app.cfg.name),
        Some(p.mute_mic()),
        Some(p.deaf()),
        me_speaking,
        if p.mute_mic() { 0 } else { p.in_level() },
        compact,
    )];
    let ping = match &app.peer {
        Some(peer) => {
            rows.push(participant_row(
                peer.name.clone(),
                peer.status_known.then_some(peer.mute_mic || peer.deaf),
                peer.status_known.then_some(peer.deaf),
                peer.speaking,
                if peer.mute_mic { 0 } else { p.out_level() },
                compact,
            ));
            match peer.rtt {
                Some(ms) => format!("пинг соединения: {ms} мс"),
                None => "пинг: измеряем…".into(),
            }
        }
        None => {
            rows.push(participant_row(
                "Ожидаем собеседника".into(),
                None,
                None,
                false,
                0,
                compact,
            ));
            "соединяемся…".into()
        }
    };
    let (headers, widths) = if compact {
        (
            vec!["Участник", "Микрофон", "Звук"],
            vec![
                Constraint::Min(12),
                Constraint::Length(9),
                Constraint::Length(7),
            ],
        )
    } else {
        (
            vec!["Участник", "Микрофон", "Звук", "Сейчас", "Уровень"],
            vec![
                Constraint::Min(14),
                Constraint::Length(9),
                Constraint::Length(7),
                Constraint::Length(12),
                Constraint::Length(8),
            ],
        )
    };
    f.render_widget(
        Table::new(rows, widths)
            .header(Row::new(headers).style(dim()))
            .column_spacing(1)
            .block(Block::bordered().title(format!(" Участники · {ping} "))),
        chunks[0],
    );

    // ── чат
    let mut lines: Vec<Line> = Vec::new();
    for l in &app.chat {
        let who = if l.mine {
            format!("{} (вы)", l.name)
        } else {
            l.name.clone()
        };
        let style = if l.mine { accent() } else { green() };
        let mut spans = vec![
            Span::styled(format!("{who}:"), style),
            Span::styled(" ", dim()),
            Span::raw(l.text.clone()),
        ];
        if l.mine {
            let st = match l.status {
                0 => Span::styled(" …", dim()),
                1 => Span::styled(" ✓", green()),
                2 => Span::styled(" ✗не доставлено", bad()),
                _ => Span::raw(""),
            };
            spans.push(st);
        }
        lines.push(Line::from(spans));
    }
    let inner_h = chunks[1].height.saturating_sub(2) as usize;
    let total = lines.len();
    let start = total.saturating_sub(inner_h + app.chat_scroll);
    let mut para = Paragraph::new(lines)
        .block(Block::bordered().title(" переписка  [T] писать · [PgUp/PgDn] листать "))
        .scroll((start as u16, 0));
    if app.input_focus {
        para = para.block(Block::bordered().title(" переписка (ввод) ".fg(Color::Cyan).bold()));
    }
    f.render_widget(para, chunks[1]);

    // ── ввод/статус
    let (ci, co) = app.engine.current_devices();
    let cut = |s: &str| {
        if s.is_empty() {
            "(по умолчанию)".to_string()
        } else if s.chars().count() > 26 {
            format!("{}…", s.chars().take(25).collect::<String>())
        } else {
            s.to_string()
        }
    };
    let line = if app.input_focus {
        Line::from(Span::styled(format!("> {}▏", app.input), strong()))
    } else if app.arm_hangup {
        Line::from(Span::styled(
            "завершаем звонок! [Y] — подтвердить, [Esc] — отмена",
            bad(),
        ))
    } else {
        Line::from(Span::styled(
            "[F2] настройки  [T] чат  [M] микро  [D] звук  [U,Y] завершить",
            dim(),
        ))
    };
    let devline = Line::from(vec![
        Span::styled("вывод: ", dim()),
        Span::styled(cut(&co), Style::new()),
        Span::styled("  [O] сменить   ", dim()),
        Span::styled("ввод: ", dim()),
        Span::styled(cut(&ci), Style::new()),
        Span::styled("  [I] сменить", dim()),
    ]);
    let volume_keys = Line::from(Span::styled(
        "[↑/↓] громкость собеседника  [ / ] микрофон  [A] автогромкость",
        dim(),
    ));
    f.render_widget(Paragraph::new(vec![line, volume_keys]), chunks[2]);
    let levels = Line::from(vec![
        Span::styled(
            if p.input_clipping() {
                "ПЕРЕГРУЗ ВХОДА  "
            } else {
                ""
            },
            bad(),
        ),
        Span::styled(
            format!(
                "Микрофон: {}%  Собеседник: {}%  ",
                p.gain_in(),
                p.gain_peer()
            ),
            dim(),
        ),
        Span::styled(
            format!(
                "Авто: {}  Фильтр: {}  Шумодав: {}",
                if p.auto_gain() { "ВКЛ" } else { "ВЫКЛ" },
                if p.voice_filter() {
                    "ВКЛ"
                } else {
                    "ВЫКЛ"
                },
                if p.noise_suppression() {
                    "ВКЛ"
                } else {
                    "ВЫКЛ"
                }
            ),
            accent(),
        ),
    ]);
    let connection = Line::from(Span::styled(
        app.peer
            .as_ref()
            .map(|peer| format!("Соединение: {}", peer.addr))
            .unwrap_or_else(|| app.status.clone()),
        dim(),
    ));
    f.render_widget(Paragraph::new(vec![levels, devline, connection]), chunks[3]);
}

fn participant_row(
    name: String,
    muted: Option<bool>,
    deaf: Option<bool>,
    speaking: bool,
    level: u8,
    compact: bool,
) -> Row<'static> {
    let state = |disabled: Option<bool>| match disabled {
        Some(true) => Cell::from("ВЫКЛ").style(bad()),
        Some(false) => Cell::from("ВКЛ").style(green()),
        None => Cell::from("…").style(dim()),
    };
    let active = speaking && muted == Some(false);
    let mut cells = vec![Cell::from(name).style(strong()), state(muted), state(deaf)];
    if !compact {
        cells.push(
            Cell::from(if active {
                "говорит ●"
            } else {
                "молчит"
            })
            .style(if active { green() } else { dim() }),
        );
        cells.push(Cell::from(level_bar(level)).style(accent()));
    }
    Row::new(cells)
}

/// Полоска уровня: 8 делений, заполнение по процентам.
fn level_bar(pct: u8) -> String {
    let filled = ((pct as usize) * 8 / 100).min(8);
    let mut s = String::with_capacity(16);
    for _ in 0..filled {
        s.push('█');
    }
    for _ in filled..8 {
        s.push('░');
    }
    s
}

// ── SETTINGS ───────────────────────────────────────────────────────────────

fn render_settings(app: &App, f: &mut Frame) {
    if let Some(pick) = &app.dev_pick {
        return render_dev_pick(app, f, pick.input, pick.idx);
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
    let mut area = centered_rect(f, 86, rows.len() as u16 + 8);
    area.height = area.height.min(f.area().height.saturating_sub(1));
    area.y = (f
        .area()
        .height
        .saturating_sub(1)
        .saturating_sub(area.height))
        / 2;
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
}

fn render_dev_pick(app: &App, f: &mut Frame, input: bool, idx: usize) {
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
    let area = centered_rect(f, 80, lines.len() as u16 + 4);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{ChatLine, PeerInfo};
    use crate::config::Config;
    use ratatui::{Terminal, backend::TestBackend};

    fn draw(app: &App, width: u16) -> String {
        draw_size(app, width, 24)
    }

    fn draw_size(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(app, frame)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn app() -> App {
        App::for_test(Config {
            name: "Лексей".into(),
            ..Config::default()
        })
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
            let screen = draw_size(&app, 80, height);
            assert!(screen.contains("▸ порог VOX  25"));
            assert!(screen.contains("[Esc] вернуться к звонку"));
            app.settings_sel = crate::app::ROW_FILTER;
            let screen = draw_size(&app, 80, height);
            assert!(screen.contains("▸ фильтр голоса (лёгкий)  ВЫКЛ"));
            app.settings_sel = crate::app::ROW_DENOISE;
            let screen = draw_size(&app, 80, height);
            assert!(screen.contains("▸ шумоподавление  ВЫКЛ"));
            app.engine.params.set_noise_suppression(true);
            assert!(draw_size(&app, 80, height).contains("▸ шумоподавление  ВКЛ"));
            app.engine.params.set_noise_suppression(false);
        }
    }

    #[test]
    fn call_shows_both_names_correct_mute_sound_ping_and_chat_author() {
        let mut app = app();
        app.screen = Screen::Call;
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
            let screen = draw(&app, width);
            assert!(screen.contains("Лексей (вы)"));
            assert!(screen.contains("Лексей (вы): Привет!"));
            assert!(screen.contains("83 мс"));
            let peer = screen.lines().find(|line| line.contains("Друг")).unwrap();
            let columns: Vec<_> = peer.split_whitespace().collect();
            assert_eq!(&columns[1..3], &["ВЫКЛ", "ВКЛ"]);
            assert!(!peer.contains("говорит"));
        }
        app.engine.params.set_mute_mic(true);
        app.peer.as_mut().unwrap().mute_mic = false;
        app.peer.as_mut().unwrap().deaf = true;
        let screen = draw(&app, 80);
        let me = screen
            .lines()
            .find(|line| line.contains("Лексей (вы)"))
            .unwrap();
        assert!(me.contains("ВЫКЛ"));
        let peer = screen.lines().find(|line| line.contains("Друг")).unwrap();
        assert_eq!(peer.matches("ВЫКЛ").count(), 2);
    }

    #[test]
    fn peer_status_is_unknown_until_first_status_packet() {
        let mut app = app();
        app.screen = Screen::Call;
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
        let screen = draw(&app, 80);
        let peer = screen.lines().find(|line| line.contains("Друг")).unwrap();
        assert!(!peer.contains("ВКЛ"));
        assert!(peer.contains('…'));
    }
}
