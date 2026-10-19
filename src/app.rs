// Состояние приложения: экраны, горячие клавиши, события net, чат.
//
// Повторяет функциональность пользовательского звена Discord без излишеств:
//   Ежатка звона screen         ↔  VoiceConnection states (waiting/waiting…)
//   Принять/Отклонить звонок     ↔  согласие на звонок
//   M/D, громкости, [ ]          ↔  setSelfMute/setSelfDeafen/setInputVolume/setLocalVolume
//   Текст поверх звонка          ↔  переписка (DM)
//   Настройки сохраняются сразу  ↔  appSettings.set (см. config.rs)

use std::collections::HashMap;
use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use tokio::sync::mpsc;

use crate::audio::{AudioEngine, DeviceList};
use crate::config::{Config, MicMode};
use crate::net::{NetCmd, NetEvent, Session, Start};
use crate::proto::REASON_HANGUP;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Home,
    Connect,
    HostWait,
    Incoming,
    Call,
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Host,
    Joiner,
}

pub struct PeerInfo {
    pub name: String,
    pub addr: String,
    pub rtt: Option<u16>,
    pub mute_mic: bool,
    pub deaf: bool,
    pub speaking: bool,
    /// уровень входа собеседника (из Status), 0..100%
    pub level: u8,
}

/// status для своих сообщений: 0 напоследок, 1 доставлено, 2 не доставлено
pub struct ChatLine {
    pub mine: bool,
    pub name: String,
    pub text: String,
    /// 0 напоследок, 1 доставлено, 2 не доставлено, 3 без статуса
    pub status: u8,
}
pub struct DevPick {
    pub input: bool,
    pub idx: usize, // 0 = «(по умолчанию)»
}

/// настройки: номер строки
pub const ROW_NAME: usize = 0;
pub const ROW_PORT: usize = 1;
pub const ROW_GAIN_IN: usize = 2;
pub const ROW_GAIN_PEER: usize = 3;
pub const ROW_MIC_MODE: usize = 4;
pub const ROW_VAD: usize = 5;
pub const ROW_GATE: usize = 6;
pub const ROW_IN_DEV: usize = 7;
pub const ROW_OUT_DEV: usize = 8;
pub const ROW_ECHO: usize = 9;
pub const ROW_SERVER: usize = 10;
pub const SETTINGS_ROWS: usize = 11;

const PENDING_TIMEOUT: Duration = Duration::from_secs(8);
pub const DIAL_MAX_TXT: &str = "60";

pub struct App {
    pub cfg: Config,
    pub engine: Arc<AudioEngine>,
    pub devices: DeviceList,
    pub mode: Option<Mode>,
    pub screen: Screen,
    pub session: Option<Session>,
    encoded_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    pub peer: Option<PeerInfo>,
    pub incoming: Option<(String, String)>,
    pub status: String,
    pub hint_addrs: Vec<String>,
    pub room_code: Option<String>,
    pub chat: Vec<ChatLine>,
    /// файл истории прошлого чата (per-peer), Some только в звонке
    pub peer_file: Option<String>,
    pub input: String,
    pub input_focus: bool,
    pub chat_scroll: usize,
    pub msg_id: u32,
    pending: HashMap<u32, (usize, Instant)>,
    pub settings_sel: usize,
    pub dev_pick: Option<DevPick>,
    pub edit: Option<(usize, String)>,
    pub conn_input: String,
    pub arm_hangup: bool,
    pub toast: Option<(String, Instant)>,
    pub quit: bool,
}

impl App {
    pub fn new(
        cfg: Config,
        engine: Arc<AudioEngine>,
        encoded_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    ) -> App {
        App {
            cfg,
            engine,
            devices: crate::audio::list_devices(),
            mode: None,
            screen: Screen::Home,
            session: None,
            encoded_rx,
            peer: None,
            incoming: None,
            status: String::new(),
            hint_addrs: Vec::new(),
            room_code: None,
            chat: Vec::new(),
            peer_file: None,
            input: String::new(),
            input_focus: false,
            chat_scroll: 0,
            msg_id: 0,
            pending: HashMap::new(),
            settings_sel: 0,
            dev_pick: None,
            edit: None,
            conn_input: String::new(),
            arm_hangup: false,
            toast: None,
            quit: false,
        }
    }

    // ── главный тик: события сети + перекачка аудио ─────────────────────────

    pub fn poll(&mut self) {
        // события net (лежат внутри Session); собираем в буфер, чтобы не
        // конфликтовать с handle_net по &mut self
        let mut inbox: Vec<NetEvent> = Vec::new();
        let mut dead = false;
        if let Some(sess) = &mut self.session {
            loop {
                match sess.events.try_recv() {
                    Ok(ev) => inbox.push(ev),
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        dead = true;
                        break;
                    }
                }
            }
        }
        if dead {
            self.session = None;
            self.mode = None;
            if self.screen == Screen::Call {
                self.screen = Screen::Home;
                self.input_focus = false;
                self.peer = None;
            }
        }
        for ev in inbox {
            self.handle_net(ev);
        }
        // аудио: кодированные кадры → net-сессия
        while let Ok(frame) = self.encoded_rx.try_recv() {
            // Drop audio captured before a call is accepted.
            if let Some(sess) = &self.session
                && self.peer.is_some()
            {
                let _ = sess.media_tx.send(frame);
            }
        }
        // «не доставлено» по времени
        let now = Instant::now();
        let mut failed: Vec<(u32, usize)> = Vec::new();
        for (id, (idx, sent)) in self.pending.iter() {
            if now.duration_since(*sent) > PENDING_TIMEOUT {
                failed.push((*id, *idx));
            }
        }
        for (id, idx) in failed {
            self.pending.remove(&id);
            if let Some(line) = self.chat.get_mut(idx) {
                line.status = 2;
            }
        }
    }

    pub fn send_chat(&mut self) {
        let text = std::mem::take(&mut self.input);
        if text.is_empty() {
            return;
        }
        if self.session.is_none() {
            return;
        }
        let tx = self.session.as_ref().unwrap().cmd_tx.clone();
        self.msg_id += 1;
        let id = self.msg_id;
        let _ = tx.send(NetCmd::Text(text.clone()));
        let idx = self.chat.len();
        self.chat.push(ChatLine {
            mine: true,
            name: self.cfg.name.clone(),
            text,
            status: 0,
        });
        self.pending.insert(id, (idx, Instant::now()));
        if let Some(k) = self.peer_file.clone() {
            append_chat_log(&k, true, &self.cfg.name, self.chat[idx].text.clone());
        }
        self.chat_scroll = 0;
    }

    fn handle_net(&mut self, ev: NetEvent) {
        match ev {
            NetEvent::Waiting { local_addrs } => {
                self.hint_addrs = local_addrs;
                if matches!(self.mode, Some(Mode::Host)) && !matches!(self.screen, Screen::Incoming)
                {
                    if self.screen != Screen::Call {
                        self.screen = Screen::HostWait;
                    }
                }
                self.status = "ожидаю звонок".into();
            }
            NetEvent::RoomCode(code) => {
                self.room_code = Some(code);
            }
            NetEvent::Joining { attempt, addr } => {
                self.screen = Screen::Call;
                self.status = if attempt == 0 {
                    format!("восстанавливаю связь с {addr}…")
                } else {
                    format!("дозвон до {addr}: попытка {attempt}/{DIAL_MAX_TXT}")
                };
            }
            NetEvent::CallIncoming { name, addr } => {
                self.incoming = Some((name, addr));
                self.screen = Screen::Incoming;
            }
            NetEvent::InCall {
                peer_name,
                peer_addr,
            } => {
                // история этого собеседника (у каждого свой файл — тогда в
                // переписке не мелькают чужие/прошлые разговоры)
                self.peer_file = Some(chat_peer_key(&peer_name));
                self.chat = load_chat_log(&self.peer_file.clone().unwrap());
                self.chat_scroll = 0;
                self.peer = Some(PeerInfo {
                    name: peer_name,
                    addr: peer_addr,
                    rtt: None,
                    mute_mic: false,
                    deaf: false,
                    speaking: false,
                    level: 0,
                });
                self.screen = Screen::Call;
                self.status = "в звонке".into();
            }
            NetEvent::Rtt(rtt) => {
                if let Some(p) = self.peer.as_mut() {
                    p.rtt = Some(rtt);
                }
            }
            NetEvent::PeerStatus {
                mute_mic,
                deaf,
                lvl,
            } => {
                if let Some(p) = self.peer.as_mut() {
                    p.mute_mic = mute_mic;
                    p.deaf = deaf;
                    p.level = lvl.min(100);
                }
            }
            NetEvent::PeerSpeaking(v) => {
                if let Some(p) = self.peer.as_mut() {
                    p.speaking = v;
                }
            }
            NetEvent::Text(text) => {
                let name = self
                    .peer
                    .as_ref()
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| "собеседник".into());
                self.chat.push(ChatLine {
                    mine: false,
                    name,
                    text,
                    status: 3,
                });
                let line = self.chat.last().unwrap();
                if let Some(k) = self.peer_file.clone() {
                    append_chat_log(&k, false, &line.name, line.text.clone());
                }
                self.chat_scroll = 0;
            }
            NetEvent::TextAck(id) => {
                if let Some((idx, _)) = self.pending.remove(&id) {
                    if let Some(l) = self.chat.get_mut(idx) {
                        l.status = 1;
                    }
                }
            }
            NetEvent::CallEnded(msg) => {
                self.peer = None;
                self.incoming = None;
                self.peer_file = None;
                self.input.clear();
                self.input_focus = false;
                self.chat_scroll = 0;
                self.status = msg;
                match self.mode {
                    Some(Mode::Host) => self.screen = Screen::HostWait,
                    Some(Mode::Joiner) => {
                        // сессия доделает сама; экран пусть дождётся
                        self.screen = Screen::Home;
                    }
                    None => {}
                }
            }
            NetEvent::Fatal(msg) => {
                self.status = format!("ошибка: {msg}");
                self.stop_session();
                self.mode = None;
                self.screen = Screen::Home;
            }
        }
    }

    // ── управление сессией ──────────────────────────────────────────────────

    pub fn start_host(&mut self) {
        self.stop_session();
        self.engine.params.set_loopback(false);
        self.save_all();
        let sess = crate::net::spawn(
            Start::Host {
                cfg: self.cfg.clone(),
            },
            self.engine.clone(),
        );
        self.session = Some(sess);
        self.mode = Some(Mode::Host);
        self.screen = Screen::HostWait;
        self.status = "ожидаю звонок…".into();
    }

    pub fn start_connect(&mut self, target: String) {
        self.stop_session();
        self.engine.params.set_loopback(false);
        self.save_all();
        let sess = crate::net::spawn(
            Start::Joiner {
                cfg: self.cfg.clone(),
                target: target.clone(),
            },
            self.engine.clone(),
        );
        self.session = Some(sess);
        self.mode = Some(Mode::Joiner);
        self.screen = Screen::Call;
        self.status = format!("дозвон до {target}…");
    }

    pub fn stop_session(&mut self) {
        if let Some(s) = self.session.as_mut() {
            s.stop();
        }
        self.session = None;
        self.mode = None;
        self.peer = None;
        self.incoming = None;
        self.peer_file = None;
        self.input_focus = false;
    }

    // ── настройки: публичные действия ──────────────────────────────────────

    pub fn save_all(&mut self) {
        let p = self.engine.params.clone();
        p.set_gain_in(self.cfg.gain_in);
        p.set_gain_peer(self.cfg.gain_peer);
        p.set_vox_mode(matches!(self.cfg.mic_mode, MicMode::Vox));
        p.set_vad_threshold(self.cfg.vad_threshold);
        p.set_noise_gate(self.cfg.noise_gate);
        self.cfg.save();
    }

    fn apply_devices(&mut self, input_name: &str, output_name: &str) {
        let _ = input_name;
        let _ = output_name;
        if let Err(e) = self.engine.rebuild_devices(input_name, output_name) {
            // как Discord device update — откат на дефолт
            match self.engine.rebuild_devices("", "") {
                Ok(()) => self.set_toast(format!(
                    "устройство не открылось ({e}); подключено по умолчанию"
                )),
                Err(e2) => self.set_toast(format!("устройства не открылись: {e2}")),
            }
        }
        self.apply_device_names();
    }

    /// Прочитать реальные имена из движка в конфиг.
    fn apply_device_names(&mut self) {
        let (i, o) = self.engine.current_devices();
        self.cfg.input_device = i;
        self.cfg.output_device = o;
        self.cfg.save();
    }

    pub fn set_toast(&mut self, s: String) {
        self.toast = Some((s, Instant::now()));
    }

    /// Смена устройства на лету (в звонке): `I/O` со скроллом списка.
    /// `input` — вход или выход; `back` — против списка.
    fn cycle_device(&mut self, input: bool, back: bool) {
        // свежий список: устройства могли переподключиться
        self.devices = crate::audio::list_devices();
        let (ci, co) = self.engine.current_devices();
        let mut list: Vec<String> = vec![String::new()];
        list.extend(if input {
            self.devices.inputs.clone()
        } else {
            self.devices.outputs.clone()
        });
        let cur = if input { ci.clone() } else { co.clone() };
        let idx = list.iter().position(|n| *n == cur).unwrap_or(0);
        let step: isize = if back { -1 } else { 1 };
        let n_list = list.len() as isize;
        let what = if input { "ввод" } else { "вывод" };
        for k in 1..=n_list {
            let cand = ((idx as isize) + step * k).rem_euclid(n_list) as usize;
            let target = list[cand].clone();
            let (ni, no) = if input {
                (target.clone(), co.clone())
            } else {
                (ci.clone(), target.clone())
            };
            if self.engine.rebuild_devices(&ni, &no).is_ok() {
                let (bi, bo) = self.engine.current_devices();
                let bound = if input { bi.clone() } else { bo.clone() };
                let ok =
                    target.is_empty() == bound.is_empty() && (bound.is_empty() || bound == target);
                if ok {
                    self.cfg.input_device = bi;
                    self.cfg.output_device = bo;
                    self.cfg.save();
                    let shown = if bound.is_empty() {
                        "(по умолчанию)".to_string()
                    } else {
                        bound
                    };
                    self.set_toast(format!("{what}: {shown}"));
                    return;
                }
                // привязалось не то — идём к следующему кандидату
            }
        }
        // перебрали всё — вернуться на дефолт
        let _ = self.engine.rebuild_devices("", "");
        self.set_toast(format!("{what}: ни одно устройство не открылось"));
    }

    // чат-скролл
    pub fn scroll_chat(&mut self, up: bool) {
        if up {
            self.chat_scroll = self.chat_scroll.saturating_add(5);
        } else {
            self.chat_scroll = self.chat_scroll.saturating_sub(5);
        }
    }

    // ── клавиатура ─────────────────────────────────────────────────────────

    pub fn on_event(&mut self, ev: Event) {
        match self.screen {
            Screen::Home => self.keys_home(ev),
            Screen::Connect => self.keys_connect(ev),
            Screen::HostWait => self.keys_hostwait(ev),
            Screen::Incoming => self.keys_incoming(ev),
            Screen::Call => self.keys_call(ev),
            Screen::Settings => self.keys_settings(ev),
        }
    }

    fn keys_home(&mut self, ev: Event) {
        if let Event::Key(k) = ev {
            if k.kind != KeyEventKind::Press {
                return;
            }
            match (k.code, k.modifiers.contains(KeyModifiers::CONTROL)) {
                (KeyCode::Char('c'), true) => self.quit = true,
                (KeyCode::Char('h') | KeyCode::Char('H') | KeyCode::Char('х'), _) => {
                    self.start_host()
                }
                (KeyCode::Char('c') | KeyCode::Char('C') | KeyCode::Char('с'), _) => {
                    self.conn_input.clear();
                    self.screen = Screen::Connect;
                }
                (KeyCode::Char('s') | KeyCode::Char('S') | KeyCode::Char('ы'), _) => {
                    self.screen = Screen::Settings;
                }
                (KeyCode::Char('q') | KeyCode::Char('й'), _) => self.quit = true,
                _ => {}
            }
        }
    }

    fn keys_connect(&mut self, ev: Event) {
        if let Event::Key(k) = ev {
            if k.kind != KeyEventKind::Press {
                return;
            }
            match k.code {
                KeyCode::Esc => {
                    self.screen = Screen::Home;
                }
                KeyCode::Enter => {
                    let t = self.conn_input.trim().to_string();
                    if t.contains(':') && t.len() > 3 {
                        let target = t.clone();
                        self.conn_input = t;
                        self.start_connect(target);
                    } else {
                        self.set_toast("адрес в форме ip:port".into());
                    }
                }
                KeyCode::Backspace => {
                    self.conn_input.pop();
                }
                KeyCode::Char(c) => {
                    if k.modifiers.contains(KeyModifiers::CONTROL) {
                        if c == 'c' {
                            self.quit = true;
                        }
                        return;
                    }
                    if k.modifiers.contains(KeyModifiers::SHIFT) {
                        self.conn_input.push(c.to_ascii_uppercase());
                    } else {
                        self.conn_input.push(c);
                    }
                }
                _ => {}
            }
        }
    }

    fn keys_hostwait(&mut self, ev: Event) {
        if let Event::Key(k) = ev {
            if k.kind != KeyEventKind::Press {
                return;
            }
            match (k.code, k.modifiers.contains(KeyModifiers::CONTROL)) {
                (KeyCode::Esc, _) | (KeyCode::Char('q') | KeyCode::Char('й'), _) => {
                    self.stop_session();
                    self.screen = Screen::Home;
                    self.status = "меню".into();
                }
                (KeyCode::Char('c'), true) => self.quit = true,
                _ => {}
            }
        }
    }

    fn keys_incoming(&mut self, ev: Event) {
        if let Event::Key(k) = ev {
            if k.kind != KeyEventKind::Press {
                return;
            }
            match (k.code, k.modifiers.contains(KeyModifiers::CONTROL)) {
                (KeyCode::Enter, _) => {
                    if let Some(s) = self.session.as_mut() {
                        s.send(NetCmd::AcceptCall);
                    }
                    self.screen = Screen::Call;
                    self.status = "принимаю…".into();
                }
                (KeyCode::Esc, _) => {
                    if let Some(s) = self.session.as_mut() {
                        s.send(NetCmd::DeclineCall);
                    }
                    self.screen = Screen::HostWait;
                    self.status = "звонок отклонён".into();
                }
                (KeyCode::Char('c'), true) => self.quit = true,
                _ => {}
            }
        }
    }

    fn keys_call(&mut self, ev: Event) {
        if let Event::Key(k) = ev {
            // PTT: спейсику интересны и press, и release
            if k.code == KeyCode::Char(' ') && !self.input_focus {
                if self.engine.params.vox_mode() {
                    return; // VOX — спейс не влияет
                }
                match k.kind {
                    KeyEventKind::Press | KeyEventKind::Repeat => {
                        self.engine.params.set_ptt_active(true);
                    }
                    KeyEventKind::Release => {
                        self.engine.params.set_ptt_active(false);
                    }
                }
                return;
            }
            if k.kind != KeyEventKind::Press {
                return;
            }

            if self.input_focus {
                match k.code {
                    KeyCode::Enter => {
                        self.send_chat();
                        // после отправки возвращаем управление голосу (Discord
                        // вернул бы фокус через Esc — здесь всегда)
                        self.input_focus = false;
                    }
                    KeyCode::Esc => self.input_focus = false,
                    KeyCode::Backspace => {
                        self.input.pop();
                    }
                    KeyCode::Char(c) => {
                        if k.modifiers.contains(KeyModifiers::CONTROL) {
                            if c == 'c' {
                                self.quit = true;
                            }
                            return;
                        }
                        self.input.push(c);
                    }
                    _ => {}
                }
                return;
            }

            let vox = self.engine.params.vox_mode();
            match (k.code, k.modifiers.contains(KeyModifiers::CONTROL)) {
                (KeyCode::Char('m') | KeyCode::Char('ь'), _) => {
                    let v = !self.engine.params.mute_mic();
                    self.engine.params.set_mute_mic(v);
                    self.cfg_save_state();
                }
                (KeyCode::Char('d') | KeyCode::Char('в'), _) => {
                    let v = !self.engine.params.deaf();
                    self.engine.params.set_deaf(v);
                    self.cfg_save_state();
                }
                (KeyCode::Char('t') | KeyCode::Char('е'), _) => self.input_focus = true,
                (KeyCode::Up, _) => {
                    let v = self.engine.params.gain_peer().saturating_add(5).min(200);
                    self.engine.params.set_gain_peer(v);
                    self.cfg_save_state();
                }
                (KeyCode::Down, _) => {
                    let v = self.engine.params.gain_peer().saturating_sub(5);
                    self.engine.params.set_gain_peer(v);
                    self.cfg_save_state();
                }
                (KeyCode::Char('[') | KeyCode::Char('х'), _) => {
                    let v = self.engine.params.gain_in().saturating_sub(5);
                    self.engine.params.set_gain_in(v);
                    self.cfg_save_state();
                }
                (KeyCode::Char(']') | KeyCode::Char('ъ'), _) => {
                    let v = self.engine.params.gain_in().saturating_add(5).min(200);
                    self.engine.params.set_gain_in(v);
                    self.cfg_save_state();
                }
                (KeyCode::Char('u') | KeyCode::Char('г'), _) => self.arm_hangup = true,
                // смена устройств на лету: I/O (рус. ш/щ), Shift — назад
                (KeyCode::Char('o') | KeyCode::Char('щ'), false) => {
                    self.cycle_device(false, false)
                }
                (KeyCode::Char('O') | KeyCode::Char('Щ'), _) => self.cycle_device(false, true),
                (KeyCode::Char('i') | KeyCode::Char('ш'), false) => self.cycle_device(true, false),
                (KeyCode::Char('I') | KeyCode::Char('Ш'), _) => self.cycle_device(true, true),
                (KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Char('н'), _) => {
                    if self.arm_hangup {
                        self.arm_hangup = false;
                        if !vox {
                            self.engine.params.set_ptt_active(false);
                        }
                        if let Some(s) = self.session.as_mut() {
                            s.send(NetCmd::HangUp {
                                reason: REASON_HANGUP,
                            });
                        }
                        self.status = "завершаю звонок…".into();
                    }
                }
                (KeyCode::Esc, _) => self.arm_hangup = false,
                (KeyCode::PageUp, _) => self.scroll_chat(true),
                (KeyCode::PageDown, _) => self.scroll_chat(false),
                (KeyCode::Char('c'), true) => self.quit = true,
                _ => {}
            }
        }
    }

    fn keys_settings(&mut self, ev: Event) {
        if let Event::Key(k) = ev {
            if k.kind != KeyEventKind::Press {
                return;
            }

            // редактирование строки
            if let Some((row, buf)) = self.edit.as_mut() {
                match k.code {
                    KeyCode::Enter | KeyCode::Esc => {
                        let row = *row;
                        let buf = std::mem::take(buf);
                        self.edit = None;
                        if row == ROW_NAME {
                            if !buf.trim().is_empty() {
                                self.cfg.name = buf.trim().to_string();
                            }
                        } else if row == ROW_PORT {
                            if let Ok(p) = buf.trim().parse::<u16>() {
                                if p > 0 {
                                    self.cfg.port = p;
                                }
                            }
                        } else if row == ROW_SERVER {
                            let v = buf.trim().to_string();
                            // пусто = прямой режим; иначе ip:порт или host:порт
                            self.cfg.server_addr = v;
                        }
                        self.save_all();
                    }
                    KeyCode::Backspace => {
                        buf.pop();
                    }
                    KeyCode::Char(c) => {
                        if k.modifiers.contains(KeyModifiers::CONTROL) {
                            if c == 'c' {
                                self.quit = true;
                                return;
                            }
                        }
                        if buf.len() < 64 {
                            buf.push(if k.modifiers.contains(KeyModifiers::SHIFT) {
                                c.to_ascii_uppercase()
                            } else {
                                c
                            });
                        }
                    }
                    _ => {}
                }
                return;
            }

            // выбор устройства
            if let Some(pick) = self.dev_pick.as_mut() {
                let len = if pick.input {
                    self.devices.inputs.len()
                } else {
                    self.devices.outputs.len()
                };
                match k.code {
                    KeyCode::Up => pick.idx = pick.idx.saturating_sub(1),
                    KeyCode::Down => pick.idx = (pick.idx + 1).min(len), // + «(по умолчанию)»
                    KeyCode::Enter => {
                        let (input, idx) = (pick.input, pick.idx);
                        self.dev_pick = None;
                        let dev = if idx == 0 {
                            String::new()
                        } else if input {
                            self.devices.inputs[idx - 1].clone()
                        } else {
                            self.devices.outputs[idx - 1].clone()
                        };
                        let (ci, co) = self.engine.current_devices();
                        if input {
                            self.apply_devices(&dev, &co);
                        } else {
                            self.apply_devices(&ci, &dev);
                        }
                        self.devices = crate::audio::list_devices();
                    }
                    KeyCode::Esc => self.dev_pick = None,
                    KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.quit = true
                    }
                    _ => {}
                }
                return;
            }

            // навигация и правка
            let sel = self.settings_sel;
            match (k.code, k.modifiers.contains(KeyModifiers::CONTROL)) {
                (KeyCode::Up, _) => self.settings_sel = sel.saturating_sub(1),
                (KeyCode::Down, _) => {
                    self.settings_sel = (sel + 1).min(SETTINGS_ROWS.saturating_sub(1))
                }
                (KeyCode::Esc, _) => {
                    self.save_all();
                    self.screen = Screen::Home;
                }
                (KeyCode::Char('l') | KeyCode::Char('д'), _) => self.toggle_echo(),
                (KeyCode::Char('c'), true) => self.quit = true,
                _ => {}
            }
            match sel {
                ROW_NAME | ROW_PORT | ROW_SERVER => {
                    if matches!(k.code, KeyCode::Enter | KeyCode::Char(' ')) {
                        let val = match sel {
                            ROW_NAME => self.cfg.name.clone(),
                            ROW_PORT => self.cfg.port.to_string(),
                            _ => self.cfg.server_addr.clone(),
                        };
                        self.edit = Some((sel, val));
                    }
                }
                ROW_GAIN_IN => match k.code {
                    KeyCode::Left => {
                        self.engine
                            .params
                            .set_gain_in(self.engine.params.gain_in().saturating_sub(5));
                        self.cfg_save_state();
                    }
                    KeyCode::Right => {
                        self.engine
                            .params
                            .set_gain_in(self.engine.params.gain_in().saturating_add(5).min(200));
                        self.cfg_save_state();
                    }
                    _ => {}
                },
                ROW_GAIN_PEER => match k.code {
                    KeyCode::Left => {
                        self.engine
                            .params
                            .set_gain_peer(self.engine.params.gain_peer().saturating_sub(5));
                        self.cfg_save_state();
                    }
                    KeyCode::Right => {
                        self.engine.params.set_gain_peer(
                            self.engine.params.gain_peer().saturating_add(5).min(200),
                        );
                        self.cfg_save_state();
                    }
                    _ => {}
                },
                ROW_MIC_MODE => {
                    if matches!(k.code, KeyCode::Enter | KeyCode::Char(' ')) {
                        let vox = !self.engine.params.vox_mode();
                        self.engine.params.set_vox_mode(vox);
                        self.cfg.mic_mode = if vox { MicMode::Vox } else { MicMode::Ptt };
                        self.cfg_save_state();
                    }
                }
                ROW_VAD => match k.code {
                    KeyCode::Left => {
                        self.engine.params.set_vad_threshold(
                            self.engine.params.vad_threshold().saturating_sub(5),
                        );
                        self.cfg_save_state();
                    }
                    KeyCode::Right => {
                        self.engine.params.set_vad_threshold(
                            self.engine
                                .params
                                .vad_threshold()
                                .saturating_add(5)
                                .min(100),
                        );
                        self.cfg_save_state();
                    }
                    _ => {}
                },
                ROW_GATE => {
                    if matches!(k.code, KeyCode::Enter | KeyCode::Char(' ')) {
                        self.engine.params.set_noise_gate(!self.noise_gate_on());
                        self.cfg_save_state();
                    }
                }
                ROW_IN_DEV => {
                    if matches!(k.code, KeyCode::Enter | KeyCode::Char(' ')) {
                        self.dev_pick = Some(DevPick {
                            input: true,
                            idx: 0,
                        });
                    }
                }
                ROW_OUT_DEV => {
                    if matches!(k.code, KeyCode::Enter | KeyCode::Char(' ')) {
                        self.dev_pick = Some(DevPick {
                            input: false,
                            idx: 0,
                        });
                    }
                }
                ROW_ECHO => {
                    if matches!(k.code, KeyCode::Enter | KeyCode::Char(' ')) {
                        self.toggle_echo();
                    }
                }
                _ => {}
            }
        }
    }

    pub fn toggle_echo(&mut self) {
        let v = !self.engine.params.loopback();
        self.engine.params.set_loopback(v);
        self.set_toast(if v {
            "эхо-тест ВКЛ (слышите себя)".into()
        } else {
            "эхо-тест выключен".into()
        });
    }

    pub fn noise_gate_on(&self) -> bool {
        self.engine.params.noise_gate()
    }

    // сохранение текущего состояния движка в конфиг (вызываем при каждой правке)
    fn cfg_save_state(&mut self) {
        let p = self.engine.params.clone();
        self.cfg.gain_in = p.gain_in();
        self.cfg.gain_peer = p.gain_peer();
        self.cfg.vad_threshold = p.vad_threshold();
        self.cfg.mic_mode = if p.vox_mode() {
            MicMode::Vox
        } else {
            MicMode::Ptt
        };
        self.cfg.noise_gate = self.noise_gate_on();
        self.cfg.save();
    }

    // статус-тост со временем
    pub fn tick_toast(&mut self) {
        if let Some((_, t)) = self.toast {
            if t.elapsed() > Duration::from_secs(4) {
                self.toast = None;
            }
        }
    }
}

// ── чат-лог ────────────────────────────────────────────────────────────────

fn fmt_ts() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&secs, &mut tm);
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
    }
}

/// Ключ чата собеседника: безопасное имя файла, максимум 24 символа.
fn chat_peer_key(name: &str) -> String {
    let s: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(24)
        .collect();
    if s.is_empty() { "peer".to_string() } else { s }
}

fn chat_path(key: &str) -> std::path::PathBuf {
    crate::config::Config::path()
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join(format!("chat-{key}.log"))
}

fn append_chat_log(key: &str, mine: bool, name: &str, text: String) {
    let path = chat_path(key);
    let lang_name = if mine {
        "Я".to_string()
    } else {
        name.to_string()
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "[{}] {}: {}", fmt_ts(), lang_name, text);
    }
}

fn load_chat_log(key: &str) -> Vec<ChatLine> {
    let path = chat_path(key);
    let mut out = Vec::new();
    if let Ok(body) = std::fs::read_to_string(&path) {
        for l in body.lines() {
            let l = l.trim_end();
            if l.is_empty() {
                continue;
            }
            // формат: [hh:mm:ss] Я: текст | [hh:mm:ss] Имя: текст
            let rest = l.split_once("] ").map(|(_, r)| r).unwrap_or(l);
            let (who, text) = match rest.split_once(": ") {
                Some((w, t)) => (w, t.to_string()),
                None => ("собеседник", rest.to_string()),
            };
            out.push(ChatLine {
                mine: who == "Я",
                name: who.to_string(),
                text,
                status: 3,
            });
        }
    }
    out
}
