// Состояние приложения: экраны, горячие клавиши, события net, чат.
//
// Повторяет функциональность пользовательского звена Discord без излишеств:
//   Ежатка звона screen         ↔  VoiceConnection states (waiting/waiting…)
//   Принять/Отклонить звонок     ↔  согласие на звонок
//   M/D, громкости, [ ]          ↔  setSelfMute/setSelfDeafen/setInputVolume/setLocalVolume
//   Текст поверх звонка          ↔  переписка (DM)
//   Настройки сохраняются сразу  ↔  appSettings.set (см. config.rs)

use crate::chat_links::{Hit, OpenTarget};
use crate::transfer::{TransferStatus, TransferUpdate};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
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
    pub status_known: bool,
}

/// status для своих сообщений: 0 напоследок, 1 доставлено, 2 не доставлено
#[derive(serde::Serialize, serde::Deserialize, Hash)]
pub struct ChatLine {
    pub mine: bool,
    pub name: String,
    pub text: String,
    /// 0 напоследок, 1 доставлено, 2 не доставлено, 3 без статуса
    pub status: u8,
    #[serde(default)]
    pub attachment: Option<TransferUpdate>,
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
pub const ROW_AUTO_GAIN: usize = 11;
pub const ROW_FILTER: usize = 12;
pub const ROW_DENOISE: usize = 13;
pub const ROW_DOWNLOAD: usize = 14;
pub const ROW_AUTO_UPDATE: usize = 15;
pub const SETTINGS_ROWS: usize = 16;

const PENDING_TIMEOUT: Duration = Duration::from_secs(8);
pub const DIAL_MAX_TXT: &str = "60";

pub struct App {
    pub cfg: Config,
    pub engine: Arc<AudioEngine>,
    pub devices: DeviceList,
    pub mode: Option<Mode>,
    pub screen: Screen,
    pub session: Option<Session>,
    pub peer: Option<PeerInfo>,
    pub group_room: Option<String>,
    pub group_peers: Vec<crate::group::Participant>,
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
    pub file_input: Option<String>,
    pub chat_hits: RefCell<Vec<Hit>>,
    pub chat_layout: RefCell<Option<crate::chat_links::LayoutCache>>,
    pub chat_targets: RefCell<Vec<OpenTarget>>,
    pub selected_link: Option<usize>,
    open_results: (
        std::sync::mpsc::Sender<Result<(), String>>,
        std::sync::mpsc::Receiver<Result<(), String>>,
    ),
    pub msg_id: u32,
    pending: HashMap<u32, (usize, Instant)>,
    pub settings_sel: usize,
    settings_return: Screen,
    pub dev_pick: Option<DevPick>,
    pub edit: Option<(usize, String)>,
    pub conn_input: String,
    pub arm_hangup: bool,
    pub toast: Option<(String, Instant)>,
    pub quit: bool,
    update_result: Option<(
        bool,
        std::sync::mpsc::Receiver<Result<Option<String>, String>>,
    )>,
}

impl App {
    pub fn new(cfg: Config, engine: Arc<AudioEngine>) -> App {
        let automatic = cfg.auto_update;
        let mut app = Self::with_devices(cfg, engine, crate::audio::list_devices());
        if automatic {
            app.start_update(false);
        }
        app
    }

    #[cfg(test)]
    pub fn for_test(cfg: Config) -> App {
        let (engine, _encoded_rx) = AudioEngine::for_test();
        Self::with_devices(
            cfg,
            engine,
            DeviceList {
                inputs: vec![],
                outputs: vec![],
            },
        )
    }

    fn with_devices(cfg: Config, engine: Arc<AudioEngine>, devices: DeviceList) -> App {
        App {
            cfg,
            engine,
            devices,
            mode: None,
            screen: Screen::Home,
            session: None,
            peer: None,
            group_room: None,
            group_peers: Vec::new(),
            incoming: None,
            status: String::new(),
            hint_addrs: Vec::new(),
            room_code: None,
            chat: Vec::new(),
            peer_file: None,
            input: String::new(),
            input_focus: false,
            chat_scroll: 0,
            file_input: None,
            chat_hits: RefCell::new(Vec::new()),
            chat_layout: RefCell::new(None),
            chat_targets: RefCell::new(Vec::new()),
            selected_link: None,
            open_results: std::sync::mpsc::channel(),
            msg_id: 0,
            pending: HashMap::new(),
            settings_sel: 0,
            settings_return: Screen::Home,
            dev_pick: None,
            edit: None,
            conn_input: String::new(),
            arm_hangup: false,
            toast: None,
            quit: false,
            update_result: None,
        }
    }

    // ── главный тик: события сети + перекачка аудио ─────────────────────────

    pub fn poll(&mut self) {
        if let Some((manual, rx)) = &self.update_result
            && let Ok(result) = rx.try_recv()
        {
            let manual = *manual;
            self.update_result = None;
            match result {
                Ok(Some(message)) => self.set_toast(message),
                Ok(None) if manual => self.set_toast("установлена актуальная сборка".into()),
                Err(error) if manual => self.set_toast(error),
                _ => {}
            }
        }
        while let Ok(result) = self.open_results.1.try_recv() {
            if let Err(error) = result {
                self.set_toast(error);
            }
        }
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
            if self.session_screen() == Screen::Call {
                self.set_session_screen(Screen::Home);
                self.input_focus = false;
                self.peer = None;
                self.group_room = None;
                self.group_peers.clear();
            }
        }
        for ev in inbox {
            self.handle_net(ev);
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
                self.chat_layout.get_mut().take();
            }
        }
    }

    pub fn send_chat(&mut self) {
        if let Some(path) = self.input.strip_prefix("/file ") {
            let path = path.to_owned();
            self.send_file(&path);
            self.input.clear();
            return;
        }
        if self.input.len() > crate::proto::TEXT_MAX {
            self.set_toast(format!("сообщение длиннее {} байт", crate::proto::TEXT_MAX));
            return;
        }
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
        self.chat_layout.get_mut().take();
        self.chat.push(ChatLine {
            mine: true,
            name: self.cfg.name.clone(),
            text,
            status: 0,
            attachment: None,
        });
        self.pending.insert(id, (idx, Instant::now()));
        if let Some(k) = self.peer_file.clone() {
            append_chat_log(&k, true, &self.cfg.name, &self.chat[idx].text);
        }
        self.chat_scroll = 0;
    }

    fn send_file(&mut self, path: &str) {
        if path.trim().is_empty() {
            self.set_toast("укажите путь к файлу".into());
            return;
        }
        if let Some(session) = &self.session {
            if !session.send(NetCmd::SendFile(crate::config::expand_path(path))) {
                self.set_toast("сессия закрыта".into());
            }
        }
    }
    fn open_link(&mut self, index: usize) {
        let target = self.chat_targets.borrow().get(index).cloned();
        if let Some(target) = target {
            self.selected_link = Some(index);
            let tx = self.open_results.0.clone();
            std::thread::spawn(move || {
                let _ = tx.send(crate::chat_links::open(target));
            });
        }
    }
    fn interrupt_transfers(&mut self) {
        for line in &mut self.chat {
            if let Some(file) = &mut line.attachment
                && file.active()
            {
                file.status = TransferStatus::Failed("сессия завершена".into());
                self.chat_layout.get_mut().take();
                line.text = file.label();
                if let Some(key) = &self.peer_file {
                    append_file_log(key, line);
                }
            }
        }
    }

    pub fn start_update(&mut self, manual: bool) {
        if self.update_result.is_some() {
            if manual {
                self.set_toast("обновление уже проверяется".into());
            }
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.update_result = Some((manual, rx));
        if manual {
            self.set_toast("скачиваем обновление из GitHub…".into());
        }
        std::thread::spawn(move || {
            let _ = tx.send(crate::update::run());
        });
    }
    fn received_text(&mut self, name: String, text: String) {
        self.chat_layout.get_mut().take();
        self.chat.push(ChatLine {
            mine: false,
            name,
            text,
            status: 3,
            attachment: None,
        });
        let line = self.chat.last().unwrap();
        if let Some(key) = &self.peer_file {
            append_chat_log(key, false, &line.name, &line.text);
        }
        self.chat_scroll = 0;
    }
    fn handle_net(&mut self, ev: NetEvent) {
        match ev {
            NetEvent::Transfer(update) => {
                self.chat_layout.get_mut().take();
                let complete = !update.active();
                let index = self.chat.iter().position(|line| {
                    line.mine == update.sending
                        && line
                            .attachment
                            .as_ref()
                            .is_some_and(|file| file.id == update.id)
                });
                let name = if update.sending {
                    self.cfg.name.clone()
                } else {
                    update
                        .peer_name
                        .clone()
                        .or_else(|| self.peer.as_ref().map(|p| p.name.clone()))
                        .unwrap_or_else(|| "собеседник".into())
                };
                let line = ChatLine {
                    mine: update.sending,
                    name,
                    text: update.label(),
                    status: 3,
                    attachment: Some(update.clone()),
                };
                let index = if let Some(i) = index {
                    self.chat[i] = line;
                    i
                } else {
                    self.chat.push(line);
                    self.chat.len() - 1
                };
                if complete && let Some(key) = &self.peer_file {
                    append_file_log(key, &self.chat[index]);
                }
                if update.status == TransferStatus::Complete && !update.sending {
                    self.set_toast(format!(
                        "файл сохранён: {}",
                        update
                            .path
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default()
                    ));
                }
            }
            NetEvent::Progress(message) => self.status = message,
            NetEvent::Waiting { local_addrs } => {
                self.hint_addrs = local_addrs;
                if matches!(self.mode, Some(Mode::Host))
                    && !matches!(self.session_screen(), Screen::Incoming)
                {
                    if self.session_screen() != Screen::Call {
                        self.set_session_screen(Screen::HostWait);
                    }
                }
                self.status = "ожидаю звонок".into();
            }
            NetEvent::RoomCode(code) => {
                self.room_code = Some(code);
            }
            NetEvent::Joining { attempt, addr } => {
                self.set_session_screen(Screen::Call);
                self.status = if attempt == 0 {
                    format!("восстанавливаю связь с {addr}…")
                } else {
                    format!("дозвон до {addr}: попытка {attempt}/{DIAL_MAX_TXT}")
                };
            }
            NetEvent::CallIncoming { name, addr } => {
                if self.screen == Screen::Settings {
                    self.set_toast(format!(
                        "Входящий звонок от {name}: Esc, затем Enter — принять"
                    ));
                }
                self.incoming = Some((name, addr));
                self.set_session_screen(Screen::Incoming);
            }
            NetEvent::InCall {
                peer_name,
                peer_addr,
            } => {
                // история этого собеседника (у каждого свой файл — тогда в
                // переписке не мелькают чужие/прошлые разговоры)
                self.peer_file = Some(chat_peer_key(&peer_name));
                self.chat_layout.get_mut().take();
                self.chat = load_chat_log(&self.peer_file.clone().unwrap(), &self.cfg.name);
                self.chat_scroll = 0;
                self.peer = Some(PeerInfo {
                    name: peer_name,
                    addr: peer_addr,
                    rtt: None,
                    mute_mic: false,
                    deaf: false,
                    speaking: false,
                    level: 0,
                    status_known: false,
                });
                self.set_session_screen(Screen::Call);
                self.status = "в звонке".into();
            }
            NetEvent::GroupEntered { room, server } => {
                use sha2::{Digest, Sha256};
                self.interrupt_transfers();
                let suffix = Sha256::digest(format!("{room}@{server}"));
                let key = format!(
                    "group-{room}-{:02x}{:02x}{:02x}{:02x}",
                    suffix[0], suffix[1], suffix[2], suffix[3]
                );
                self.chat_layout.get_mut().take();
                self.chat = load_chat_log(&key, &self.cfg.name);
                self.peer_file = Some(key);
                self.group_room = Some(format!("{room}@{server}"));
                self.group_peers.clear();
                self.peer = None;
                self.pending.clear();
                self.msg_id = 0;
                self.chat_scroll = 0;
                self.set_session_screen(Screen::Call);
                self.status = format!("Комната {room}@{server} · код можно отправить друзьям");
            }
            NetEvent::GroupRoster(peers) => {
                for line in &mut self.chat {
                    if let Some(file) = &mut line.attachment
                        && file.active()
                        && file
                            .peer_id
                            .is_some_and(|id| !peers.iter().any(|p| p.id == id))
                    {
                        file.status = TransferStatus::Failed("участник вышел из комнаты".into());
                        self.chat_layout.get_mut().take();
                        line.text = file.label();
                        if let Some(key) = &self.peer_file {
                            append_file_log(key, line);
                        }
                    }
                }
                self.group_peers = peers;
            }
            NetEvent::GroupText { name, text } => self.received_text(name, text),
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
                    p.status_known = true;
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
                self.received_text(name, text);
            }
            NetEvent::TextAck(id) => {
                if let Some((idx, _)) = self.pending.remove(&id) {
                    if let Some(l) = self.chat.get_mut(idx) {
                        l.status = 1;
                        self.chat_layout.get_mut().take();
                    }
                }
            }
            NetEvent::CallEnded(msg) => {
                self.interrupt_transfers();
                self.file_input = None;
                self.selected_link = None;
                self.peer = None;
                self.group_room = None;
                self.group_peers.clear();
                self.incoming = None;
                self.peer_file = None;
                self.input.clear();
                self.input_focus = false;
                self.chat_scroll = 0;
                self.status = msg;
                match self.mode {
                    Some(Mode::Host) => self.set_session_screen(Screen::HostWait),
                    Some(Mode::Joiner) => {
                        // сессия доделает сама; экран пусть дождётся
                        self.set_session_screen(Screen::Home);
                    }
                    None => {}
                }
            }
            NetEvent::Fatal(msg) => {
                self.status = format!("ошибка: {msg}");
                self.stop_session();
                self.mode = None;
                self.set_session_screen(Screen::Home);
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
        self.interrupt_transfers();
        self.file_input = None;
        if let Some(s) = self.session.as_mut() {
            s.stop();
        }
        self.session = None;
        self.mode = None;
        self.peer = None;
        self.group_room = None;
        self.group_peers.clear();
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
        p.set_auto_gain(self.cfg.auto_gain);
        p.set_voice_filter(self.cfg.voice_filter);
        p.set_noise_suppression(self.cfg.noise_suppression);
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
        self.selected_link = None;
        if up {
            self.chat_scroll = self.chat_scroll.saturating_add(5);
        } else {
            self.chat_scroll = self.chat_scroll.saturating_sub(5);
        }
    }

    // ── клавиатура ─────────────────────────────────────────────────────────

    pub fn session_screen(&self) -> Screen {
        if self.screen == Screen::Settings {
            self.settings_return
        } else {
            self.screen
        }
    }

    fn set_session_screen(&mut self, screen: Screen) {
        if self.screen == Screen::Settings {
            self.settings_return = screen;
        } else {
            self.screen = screen;
        }
    }

    pub fn open_settings(&mut self) {
        if self.screen != Screen::Settings {
            self.settings_return = self.screen;
            self.screen = Screen::Settings;
            self.engine.params.set_ptt_active(false);
            self.arm_hangup = false;
        }
    }

    pub fn on_event(&mut self, ev: Event) {
        if let Event::Key(k) = &ev
            && k.code == KeyCode::F(5)
            && k.kind == KeyEventKind::Press
        {
            self.start_update(true);
            return;
        }
        if let Event::Paste(text) = &ev {
            if let Some(path) = &mut self.file_input {
                path.push_str(text.trim_end_matches(['\r', '\n']));
            } else if let Some((_, buf)) = &mut self.edit {
                if buf.len() + text.len() <= 4096 {
                    buf.push_str(text);
                }
            } else if self.screen == Screen::Call && self.input_focus {
                if self.input.len() + text.len() <= crate::proto::TEXT_MAX {
                    self.input.push_str(text);
                }
            } else if self.screen == Screen::Connect {
                self.conn_input.push_str(text.trim());
            }
            return;
        }
        if self.file_input.is_some() {
            if let Event::Key(k) = ev {
                if k.kind == KeyEventKind::Release {
                    return;
                }
                match k.code {
                    KeyCode::Esc => self.file_input = None,
                    KeyCode::Enter => {
                        let path = self.file_input.take().unwrap();
                        self.send_file(&path);
                    }
                    KeyCode::Backspace => {
                        self.file_input.as_mut().unwrap().pop();
                    }
                    KeyCode::Char(c)
                        if !k
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        let path = self.file_input.as_mut().unwrap();
                        if path.len() < 4096 {
                            push_text_key(path, c, k.modifiers);
                        }
                    }
                    _ => {}
                }
            }
            return;
        }
        if self.screen == Screen::Call {
            if let Event::Mouse(mouse) = &ev {
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        let index = self
                            .chat_hits
                            .borrow()
                            .iter()
                            .find(|hit| hit.area.contains((mouse.column, mouse.row).into()))
                            .map(|hit| hit.index);
                        if let Some(index) = index {
                            self.open_link(index);
                        }
                    }
                    MouseEventKind::ScrollUp => self.scroll_chat(true),
                    MouseEventKind::ScrollDown => self.scroll_chat(false),
                    _ => {}
                }
                return;
            }
            if let Event::Key(k) = &ev
                && k.kind == KeyEventKind::Press
            {
                if k.code == KeyCode::F(3) {
                    self.file_input = Some(String::new());
                    self.selected_link = None;
                    self.engine.params.set_ptt_active(false);
                    return;
                }
                if k.code == KeyCode::F(4) {
                    if let Some(file) = self
                        .chat
                        .iter()
                        .rev()
                        .filter_map(|line| line.attachment.as_ref())
                        .find(|file| file.active())
                        && let Some(session) = &self.session
                    {
                        session.send(NetCmd::CancelFile(file.id));
                    }
                    return;
                }
                if !self.input_focus {
                    if matches!(k.code, KeyCode::Tab | KeyCode::BackTab) {
                        let count = self.chat_targets.borrow().len();
                        if count > 0 {
                            let reverse = k.code == KeyCode::BackTab
                                || k.modifiers.contains(KeyModifiers::SHIFT);
                            self.selected_link = Some(match self.selected_link {
                                Some(i) => {
                                    if reverse {
                                        (i + count - 1) % count
                                    } else {
                                        (i + 1) % count
                                    }
                                }
                                None => self.chat_hits.borrow().first().map_or(0, |h| h.index),
                            });
                        }
                        return;
                    }
                    if k.code == KeyCode::Enter
                        && let Some(index) = self.selected_link
                    {
                        self.open_link(index);
                        return;
                    }
                }
            }
        }

        if let Event::Key(k) = &ev {
            if k.code == KeyCode::F(2) && k.kind == KeyEventKind::Press {
                self.open_settings();
                return;
            }
        }
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
                    self.open_settings();
                }
                (KeyCode::Char('q') | KeyCode::Char('й'), _) => self.quit = true,
                _ => {}
            }
        }
    }

    fn keys_connect(&mut self, ev: Event) {
        if let Event::Key(k) = ev {
            if k.kind == KeyEventKind::Release
                || (k.kind == KeyEventKind::Repeat
                    && !matches!(k.code, KeyCode::Char(_) | KeyCode::Backspace))
            {
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
                    push_text_key(&mut self.conn_input, c, k.modifiers);
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
            if k.kind == KeyEventKind::Release
                || (k.kind == KeyEventKind::Repeat
                    && !(self.input_focus
                        && matches!(k.code, KeyCode::Char(_) | KeyCode::Backspace)))
            {
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
                        push_text_key(&mut self.input, c, k.modifiers);
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
                (KeyCode::Char('s') | KeyCode::Char('ы'), _) => self.open_settings(),
                (KeyCode::Char('t') | KeyCode::Char('е'), _) => {
                    self.engine.params.set_ptt_active(false);
                    self.input_focus = true;
                    self.selected_link = None;
                    self.chat_scroll = 0;
                }
                (KeyCode::Char('a') | KeyCode::Char('ф'), _) => {
                    let enabled = !self.engine.params.auto_gain();
                    self.engine.params.set_auto_gain(enabled);
                    self.cfg_save_state();
                }
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
            if k.kind == KeyEventKind::Release
                || (k.kind == KeyEventKind::Repeat
                    && !matches!(
                        k.code,
                        KeyCode::Backspace
                            | KeyCode::Up
                            | KeyCode::Down
                            | KeyCode::Left
                            | KeyCode::Right
                    )
                    && !(self.edit.is_some() && matches!(k.code, KeyCode::Char(_))))
            {
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
                        } else if row == ROW_DOWNLOAD {
                            self.cfg.download_dir = buf.trim().to_owned();
                            if let Some(session) = &self.session {
                                session
                                    .send(NetCmd::DownloadDirectory(self.cfg.download_directory()));
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
                        if buf.len() < if *row == ROW_DOWNLOAD { 4096 } else { 64 } {
                            push_text_key(buf, c, k.modifiers);
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
                    self.screen = self.settings_return;
                }
                (KeyCode::Char('l') | KeyCode::Char('д'), _) => self.toggle_echo(),
                (KeyCode::Char('c'), true) => self.quit = true,
                _ => {}
            }
            match sel {
                ROW_NAME | ROW_PORT | ROW_SERVER | ROW_DOWNLOAD => {
                    if matches!(k.code, KeyCode::Enter | KeyCode::Char(' ')) {
                        let val = match sel {
                            ROW_NAME => self.cfg.name.clone(),
                            ROW_PORT => self.cfg.port.to_string(),
                            ROW_DOWNLOAD => self.cfg.download_directory().display().to_string(),
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
                ROW_FILTER => {
                    if matches!(k.code, KeyCode::Enter | KeyCode::Char(' ')) {
                        self.engine
                            .params
                            .set_voice_filter(!self.engine.params.voice_filter());
                        self.cfg_save_state();
                    }
                }
                ROW_DENOISE => {
                    if matches!(k.code, KeyCode::Enter | KeyCode::Char(' ')) {
                        self.engine
                            .params
                            .set_noise_suppression(!self.engine.params.noise_suppression());
                        self.cfg_save_state();
                    }
                }
                ROW_AUTO_UPDATE => {
                    if matches!(k.code, KeyCode::Enter | KeyCode::Char(' ')) {
                        self.cfg.auto_update = !self.cfg.auto_update;
                        self.cfg.save();
                    }
                }
                ROW_AUTO_GAIN => {
                    if matches!(k.code, KeyCode::Enter | KeyCode::Char(' ')) {
                        self.engine
                            .params
                            .set_auto_gain(!self.engine.params.auto_gain());
                        self.cfg_save_state();
                    }
                }
                _ => {}
            }
        }
    }

    pub fn toggle_echo(&mut self) {
        if self.session.is_some() {
            self.set_toast("Эхо-тест доступен после завершения сессии".into());
            return;
        }
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
        self.cfg.auto_gain = p.auto_gain();
        self.cfg.voice_filter = p.voice_filter();
        self.cfg.noise_suppression = p.noise_suppression();
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

fn push_text_key(buffer: &mut String, c: char, modifiers: KeyModifiers) {
    if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) {
        if modifiers.contains(KeyModifiers::SHIFT) && c.is_lowercase() {
            buffer.extend(c.to_uppercase());
        } else {
            buffer.push(c);
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

// A versioned tab-separated record keeps ownership separate from the actual
// nickname. Escape controls so messages and names cannot create extra records.
fn escape_log(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

fn unescape_log(value: &str) -> String {
    let mut chars = value.chars();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some(c) => {
                    out.push(c);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn chat_record(mine: bool, name: &str, text: &str) -> String {
    format!(
        "@rm2\t{}\t{}\t{}",
        u8::from(mine),
        escape_log(name),
        escape_log(text)
    )
}

fn append_chat_log(key: &str, mine: bool, name: &str, text: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(chat_path(key))
    {
        let _ = writeln!(f, "[{}] {}", fmt_ts(), chat_record(mine, name, text));
    }
}

fn append_file_log(key: &str, line: &ChatLine) {
    if let Ok(record) = toml::to_string(line)
        && let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(chat_path(key))
    {
        let _ = writeln!(file, "[{}] @rm3\t{}", fmt_ts(), escape_log(&record));
    }
}

fn parse_chat_log(body: &str, my_name: &str) -> Vec<ChatLine> {
    body.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let rest = line.split_once("] ").map(|(_, r)| r).unwrap_or(line);
            if let Some(record) = rest.strip_prefix("@rm3\t")
                && let Ok(mut line) = toml::from_str::<ChatLine>(&unescape_log(record))
            {
                line.status = 3;
                return line;
            }
            if let Some(fields) = rest.strip_prefix("@rm2\t") {
                let mut fields = fields.splitn(3, '\t');
                let mine = fields.next() == Some("1");
                let name = unescape_log(fields.next().unwrap_or(""));
                let text = unescape_log(fields.next().unwrap_or(""));
                ChatLine {
                    mine,
                    name,
                    text,
                    status: 3,
                    attachment: None,
                }
            } else {
                // Legacy logs stored only "Я" for own messages. The historical
                // nickname is unrecoverable; display the current local nickname.
                let (who, text) = rest.split_once(": ").unwrap_or(("собеседник", rest));
                let mine = who == "Я";
                ChatLine {
                    mine,
                    name: if mine {
                        my_name.to_string()
                    } else {
                        who.to_string()
                    },
                    text: text.to_string(),
                    status: 3,
                    attachment: None,
                }
            }
        })
        .collect()
}

fn load_chat_log(key: &str, my_name: &str) -> Vec<ChatLine> {
    std::fs::read_to_string(chat_path(key))
        .map(|body| parse_chat_log(&body, my_name))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachments_keep_real_names_and_local_paths_in_history() {
        let line = ChatLine {
            mine: false,
            name: "Друг".into(),
            text: "файл".into(),
            status: 3,
            attachment: Some(TransferUpdate {
                id: 17,
                peer_id: None,
                peer_name: None,
                sending: false,
                name: "имя файла.txt".into(),
                size: 5,
                done: 5,
                status: TransferStatus::Complete,
                path: Some(std::path::PathBuf::from("/tmp/downloads/имя файла.txt")),
            }),
        };
        let body = format!(
            "[12:00] @rm3\t{}\n",
            escape_log(&toml::to_string(&line).unwrap())
        );
        let restored = parse_chat_log(&body, "ЯСейчас");
        assert_eq!(restored[0].name, "Друг");
        let file = restored[0].attachment.as_ref().unwrap();
        assert_eq!(file.status, TransferStatus::Complete);
        assert_eq!(file.path, line.attachment.unwrap().path);
        assert_eq!(restored[0].status, 3);
    }

    #[test]
    fn file_prompt_preserves_chat_draft_and_bracketed_paste() {
        let mut app = App::for_test(Config::default());
        app.screen = Screen::Call;
        app.input_focus = true;
        app.input = "черновик".into();
        key(
            &mut app,
            KeyCode::F(3),
            KeyModifiers::NONE,
            KeyEventKind::Press,
        );
        app.on_event(Event::Paste("'/tmp/файл с пробелами.txt'".into()));
        assert_eq!(
            app.file_input.as_deref(),
            Some("'/tmp/файл с пробелами.txt'")
        );
        key(
            &mut app,
            KeyCode::Esc,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        );
        assert!(app.file_input.is_none());
        assert_eq!(app.input, "черновик");
        assert!(app.input_focus);
    }
    use crossterm::event::KeyEvent;

    fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers, kind: KeyEventKind) {
        app.on_event(Event::Key(KeyEvent::new_with_kind(code, modifiers, kind)));
    }

    #[test]
    fn history_keeps_real_authors_and_reads_legacy_own_messages() {
        let own_name = "Лексей: [ПК]";
        let text = "Привет: A\\B\tстрока\nещё ] строка";
        let log = format!(
            "[12:00:00] Я: старое сообщение\n[12:00:01] Друг: ответ\n[12:00:02] {}\n[12:00:03] {}\n",
            chat_record(true, own_name, text),
            chat_record(false, "Я", "это ник собеседника")
        );
        let lines = parse_chat_log(&log, "ТекущийНик");
        assert_eq!(lines.len(), 4);
        assert!(lines[0].mine);
        assert_eq!(lines[0].name, "ТекущийНик");
        assert!(!lines[1].mine);
        assert_eq!(lines[1].name, "Друг");
        assert!(lines[2].mine);
        assert_eq!(lines[2].name, own_name);
        assert_eq!(lines[2].text, text);
        assert!(!lines[3].mine);
        assert_eq!(lines[3].name, "Я");
    }

    #[test]
    fn chat_accepts_shift_unicode_and_repeat_but_ignores_release() {
        let mut app = App::for_test(Config::default());
        app.screen = Screen::Call;
        app.input_focus = true;
        for c in ['a', 'ж'] {
            key(
                &mut app,
                KeyCode::Char(c),
                KeyModifiers::SHIFT,
                KeyEventKind::Press,
            );
        }
        for c in [':', '!', '_'] {
            key(
                &mut app,
                KeyCode::Char(c),
                KeyModifiers::NONE,
                KeyEventKind::Press,
            );
        }
        assert_eq!(app.input, "AЖ:!_");
        for kind in [KeyEventKind::Press, KeyEventKind::Repeat] {
            key(&mut app, KeyCode::Backspace, KeyModifiers::NONE, kind);
        }
        key(
            &mut app,
            KeyCode::Char('я'),
            KeyModifiers::NONE,
            KeyEventKind::Repeat,
        );
        key(
            &mut app,
            KeyCode::Backspace,
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );
        assert_eq!(app.input, "AЖ:я");
    }

    #[test]
    fn repeating_backspace_works_in_address_and_settings_fields() {
        let mut app = App::for_test(Config::default());
        app.screen = Screen::Connect;
        app.conn_input = "адрес123".into();
        for _ in 0..3 {
            key(
                &mut app,
                KeyCode::Backspace,
                KeyModifiers::NONE,
                KeyEventKind::Repeat,
            );
        }
        assert_eq!(app.conn_input, "адрес");
        app.open_settings();
        app.edit = Some((ROW_NAME, "Имя123".into()));
        for _ in 0..3 {
            key(
                &mut app,
                KeyCode::Backspace,
                KeyModifiers::NONE,
                KeyEventKind::Repeat,
            );
        }
        assert_eq!(app.edit.as_ref().unwrap().1, "Имя");
    }

    #[test]
    fn settings_preserve_call_and_draft_and_follow_a_hangup() {
        let mut app = App::for_test(Config::default());
        app.screen = Screen::Call;
        app.mode = Some(Mode::Joiner);
        app.input = "Черновик: Привет!".into();
        app.input_focus = true;
        key(
            &mut app,
            KeyCode::F(2),
            KeyModifiers::NONE,
            KeyEventKind::Press,
        );
        assert_eq!(app.screen, Screen::Settings);
        assert_eq!(app.session_screen(), Screen::Call);
        key(
            &mut app,
            KeyCode::Esc,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        );
        assert_eq!(app.screen, Screen::Call);
        assert!(app.input_focus);
        assert_eq!(app.input, "Черновик: Привет!");
        app.open_settings();
        app.handle_net(NetEvent::CallEnded("Друг вышел".into()));
        assert_eq!(app.screen, Screen::Settings);
        key(
            &mut app,
            KeyCode::Esc,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        );
        assert_eq!(app.screen, Screen::Home);
    }
}
