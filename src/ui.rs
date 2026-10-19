// Отрисовка экранов (ratatui). Тексты и подсказки — на русском.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

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

fn render_home(_app: &App, f: &mut Frame) {
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "remember-me — прямой P2P-звонок: голос по UDP + переписка",
            strong(),
        )),
        Line::from(Span::styled(
            "(как discord voice без лишнего: VPN или напрямую)",
            dim(),
        )),
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
            " Работает и за NAT-сетью поверх VPN: обе стороны в одной VPN, либо проброс UDP.",
            dim(),
        )),
    ];
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
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(4),
        Constraint::Length(2),
        Constraint::Length(3),
    ])
    .split(area);

    // ── верхняя плашка состояния
    let p = app.engine.params.clone();
    let me_speaking = app
        .engine
        .self_speaking
        .load(std::sync::atomic::Ordering::Relaxed);
    let mut top: Vec<Span> = Vec::new();
    match &app.peer {
        Some(peer) => {
            top.push(Span::styled(peer.name.clone(), green().bold()));
            top.push(Span::styled(format!(" ({})", peer.addr), dim()));
            top.push(Span::styled(
                match peer.rtt {
                    Some(rtt) => format!("  ping {rtt} ms"),
                    None => "  ping —".into(),
                },
                dim(),
            ));
            top.push(Span::styled("  ", dim()));
            top.push(if peer.speaking {
                Span::styled("пишет ●", green())
            } else {
                Span::styled("молчит ·", dim())
            });
            // статусы собеседника — как в Discord рядом с его ником
            if peer.mute_mic {
                top.push(Span::styled(" [МИКРО ВКЛ]", bad()));
            }
            if peer.deaf {
                top.push(Span::styled(" [ГЛУХОЙ]", bad()));
            }
            top.push(gap());
        }
        None => {
            top.push(Span::styled("собеседник ещё не на связи ", dim()));
            top.push(Span::styled(app.status.clone(), accent()));
        }
    }
    top.push(gap());
    if p.mute_mic() {
        top.push(Span::styled("[МИКРО ВЫКЛ]", bad()));
    } else {
        top.push(Span::styled("микро", dim()));
    }
    top.push(gap());
    if p.deaf() {
        top.push(Span::styled("[ГЛУХОЙ]", bad()));
    } else {
        top.push(Span::styled("звук", dim()));
    }
    top.push(Span::styled(format!("  звук: {}%", p.gain_peer()), dim()));
    top.push(Span::styled(format!("  микро: {}%", p.gain_in()), dim()));
    // индикаторы уровня: мой микрофон и вход собеседника — видно, где теряется звук
    top.push(gap());
    top.push(Span::styled(
        format!("микрофон {}", level_bar(p.in_level())),
        accent(),
    ));
    top.push(gap());
    if app.peer.is_some() {
        let level = p.out_level();
        top.push(Span::styled(
            format!("вход {}", level_bar(level)),
            if level > 0 { green() } else { dim() },
        ));
    }
    top.push(if me_speaking {
        Span::styled("  говорю я ●", accent())
    } else {
        Span::styled("  говорю я ·", dim())
    });
    let title_seg = Line::from(top);
    f.render_widget(
        Paragraph::new(title_seg).block(
            Block::bordered()
                .title(" звонок ")
                .title_alignment(Alignment::Left),
        ),
        chunks[0],
    );

    // ── чат
    let mut lines: Vec<Line> = Vec::new();
    for l in &app.chat {
        let (who, style) = if l.mine {
            ("Я", accent())
        } else {
            (l.name.as_str(), green())
        };
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
            "[M] микро  [D] глухой  [T] писать  [↑/↓] звук  [ [ ] ] микро громк.  [Пробел] говорить(Ptt)  [U] повесить",
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
    f.render_widget(Paragraph::new(vec![line, devline]), chunks[2]);

    // ── блок denied?
    let warn: Vec<Line> = Vec::new();
    let warn = warn;
    let _ = warn;
    f.render_widget(
        Paragraph::new(""), /*нижний блок — просто тосты*/
        chunks[3],
    );
}

fn gap() -> Span<'static> {
    Span::raw("  ")
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
        ("имя".into(), String::new()),
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
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("[↑/↓]", accent()),
        Span::styled(" поле   ", Style::new()),
        Span::styled("[Enter/Enter/←/→/Space]", accent()),
        Span::styled(" менять   ", Style::new()),
        Span::styled("[L]", accent()),
        Span::styled(" эхо   ", Style::new()),
        Span::styled("[Esc]", accent()),
        Span::styled(" назад", Style::new()),
    ]));
    let area = centered_rect(f, 74, lines.len() as u16 + 4);
    let block = Block::bordered().title(" Настройки ".bold());
    f.render_widget(Paragraph::new(lines).block(block), area);
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
