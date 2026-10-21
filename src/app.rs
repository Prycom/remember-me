// Состояние приложения: экраны, горячие клавиши, события net, чат.
//
// Повторяет функциональность пользовательского звена Discord без излишеств:
//   Ежатка звона screen         ↔  VoiceConnection states (waiting/waiting…)
//   Принять/Отклонить звонок     ↔  согласие на звонок
//   M/D, громкости, [ ]          ↔  setSelfMute/setSelfDeafen/setInputVolume/setLocalVolume
//   Текст поверх звонка          ↔  переписка (DM)
//   Настройки сохраняются сразу  ↔  appSettings.set (см. config.rs)

use crate::action::{Action, Focus, Overlay, Pane};
use crate::chat_links::{Hit, OpenTarget};
use crate::transfer::{TransferStatus, TransferUpdate};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::Event;
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
    pub avatar: Arc<crate::avatar::Avatar>,
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
pub const ROW_AVATAR: usize = 16;
pub const ROW_AVATAR_STYLE: usize = 17;
pub const ROW_AVATAR_CLEAR: usize = 18;
pub const ROW_AVATAR_RESOLUTION: usize = 19;
pub const SETTINGS_ROWS: usize = 20;

const PENDING_TIMEOUT: Duration = Duration::from_secs(8);
pub const DIAL_MAX_TXT: &str = "60";

pub struct App {
    pub cfg: Config,
    pub avatar: Arc<crate::avatar::Avatar>,
    avatar_load: Option<
        std::sync::mpsc::Receiver<
            Result<(crate::avatar::Avatar, crate::avatar::Avatar, String), String>,
        >,
    >,
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
    pub focus: Focus,
    pub home_sel: usize,
    pub call_sel: usize,
    pub host_sel: usize,
    pub overlay: Option<Overlay>,
    pub log: Vec<String>,
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
    settings_focus: Focus,
    pub dev_pick: Option<DevPick>,
    pub edit: Option<(usize, String)>,
    pub conn_input: String,
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

    fn with_devices(mut cfg: Config, engine: Arc<AudioEngine>, devices: DeviceList) -> App {
        if !crate::avatar::RESOLUTIONS.contains(&cfg.avatar_resolution) {
            cfg.avatar_resolution = 32;
        }
        let avatar = crate::avatar::display(cfg.avatar.as_ref(), &cfg.name);
        App {
            cfg,
            avatar,
            avatar_load: None,
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
            focus: Focus::Pane(Pane::Main),
            home_sel: 0,
            call_sel: 0,
            host_sel: 0,
            overlay: None,
            log: Vec::new(),
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
            settings_focus: Focus::Pane(Pane::Main),
            dev_pick: None,
            edit: None,
            conn_input: String::new(),
            toast: None,
            quit: false,
            update_result: None,
        }
    }

    // ── главный тик: события сети + перекачка аудио ─────────────────────────

    pub fn poll(&mut self) {
        if let Some(rx) = &self.avatar_load
            && let Ok(result) = rx.try_recv()
        {
            self.avatar_load = None;
            match result {
                Ok((avatar, original, path)) => {
                    self.cfg.avatar_resolution = avatar.side() as u16;
                    self.cfg.avatar_original = Some(original);
                    self.cfg.avatar = Some(avatar);
                    self.cfg.avatar_file = path;
                    self.apply_avatar();
                    self.set_toast("аватарка готова · обновлена для участников звонка".into());
                }
                Err(error) => self.set_toast(error),
            }
        }
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
            self.overlay = None;
            self.engine.params.set_ptt_active(false);
            if self.session_screen() == Screen::Call {
                self.set_session_screen(Screen::Home);
                self.focus = Focus::Pane(Pane::Main);
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
            NetEvent::Progress(message) => {
                self.status = message.clone();
                self.push_log(message);
            }
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
                self.push_log(format!("код комнаты: {code}"));
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
                let avatar = crate::avatar::display(None, &peer_name);
                self.call_sel = 1;
                self.peer = Some(PeerInfo {
                    avatar,
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
                if self.group_peers.is_empty()
                    && !peers.is_empty()
                    && self.focus != Focus::Pane(Pane::Side)
                {
                    self.call_sel = 1;
                }
                if self.group_peers.len() != peers.len() {
                    self.push_log(format!("участников в комнате: {}", peers.len() + 1));
                }
                self.group_peers = peers;
                self.call_sel = self
                    .call_sel
                    .min(self.participant_count().saturating_sub(1));
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
            NetEvent::PeerAvatar(avatar) => {
                if let Some(peer) = &mut self.peer {
                    peer.avatar =
                        avatar.unwrap_or_else(|| crate::avatar::display(None, &peer.name));
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
                self.overlay = None;
                self.engine.params.set_ptt_active(false);
                self.file_input = None;
                self.selected_link = None;
                self.peer = None;
                self.group_room = None;
                self.group_peers.clear();
                self.incoming = None;
                self.peer_file = None;
                self.input.clear();
                self.focus = Focus::Pane(Pane::Main);
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
        self.focus = Focus::Pane(Pane::Main);
        self.overlay = None;
        self.engine.params.set_ptt_active(false);
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

    pub fn participant_count(&self) -> usize {
        if self.group_room.is_some() {
            self.group_peers.len() + 1
        } else {
            2
        }
    }
    fn push_log(&mut self, message: String) {
        if self.log.len() >= 200 {
            self.log.remove(0);
        }
        self.log.push(message);
    }
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
            self.settings_focus = self.focus;
            self.focus = Focus::Pane(Pane::Main);
            self.screen = Screen::Settings;
            self.engine.params.set_ptt_active(false);
        }
    }

    pub fn on_event(&mut self, ev: Event) {
        if let Event::Paste(text) = ev {
            self.on_action(Action::Paste(text));
            return;
        }
        if let Event::Key(k) = ev {
            if let Some(a) = crate::ui::keymap::action_for(&k, self) {
                self.on_action(a);
            }
        }
    }

    fn home_item(i: usize) -> Action {
        match i {
            0 => Action::Host,
            1 => Action::Connect,
            2 => Action::OpenSettings,
            _ => Action::Quit,
        }
    }

    fn run_home_item(&mut self) {
        self.home_sel = self.home_sel.min(3);
        self.on_action(App::home_item(self.home_sel));
    }

    pub fn on_action(&mut self, a: Action) {
        if a == Action::Ptt(false) {
            self.engine.params.set_ptt_active(false);
            return;
        }
        if a == Action::Quit {
            self.quit = true;
            return;
        }
        if self.file_input.is_some() {
            match a {
                Action::Confirm => {
                    let path = self.file_input.take().unwrap();
                    self.send_file(&path);
                }
                Action::Cancel => {
                    self.file_input = None;
                }
                Action::Insert(c) => {
                    let path = self.file_input.as_mut().unwrap();
                    if path.len() + c.len_utf8() <= 4096 {
                        path.push(c);
                    }
                }
                Action::Backspace => {
                    self.file_input.as_mut().unwrap().pop();
                }
                Action::Paste(text) => {
                    let text = text.trim_end_matches(['\r', '\n']);
                    let path = self.file_input.as_mut().unwrap();
                    if path.len() + text.len() <= 4096 {
                        path.push_str(text);
                    }
                }
                _ => {}
            }
            return;
        }
        if let Action::Copy(text) = a {
            self.copy_text(text);
            return;
        }
        if self.overlay == Some(Overlay::Profile) {
            match a {
                Action::EditAvatar if self.call_sel == 0 => {
                    self.overlay = None;
                    self.open_settings();
                    self.settings_sel = ROW_AVATAR;
                    self.settings_toggle();
                    return;
                }
                Action::CycleAvatars => {
                    self.cycle_avatars();
                    return;
                }
                Action::Left => {
                    self.call_sel = self.call_sel.saturating_sub(1);
                    return;
                }
                Action::Right => {
                    self.call_sel =
                        (self.call_sel + 1).min(self.participant_count().saturating_sub(1));
                    return;
                }
                _ => {}
            }
        }
        if let Some(overlay) = self.overlay {
            match a {
                Action::Cancel | Action::Help => self.overlay = None,
                Action::Confirm => {
                    self.overlay = None;
                    if overlay == Overlay::HangUp {
                        self.perform_hangup();
                    }
                }
                _ => {}
            }
            return;
        }
        match a {
            Action::ShowProfile(index) => {
                if index > 0 && self.peer.is_none() && self.group_room.is_none() {
                    self.set_toast("собеседник ещё не подключён".into());
                    return;
                }
                self.call_sel = index.min(self.participant_count().saturating_sub(1));
                self.focus = Focus::Pane(Pane::Side);
                self.engine.params.set_ptt_active(false);
                self.overlay = Some(Overlay::Profile);
                return;
            }
            Action::EditAvatar => {
                self.open_settings();
                self.settings_sel = ROW_AVATAR;
                self.settings_toggle();
                return;
            }
            Action::Update => {
                self.start_update(true);
                return;
            }
            Action::OpenSettings => {
                self.open_settings();
                return;
            }
            Action::Help => {
                self.engine.params.set_ptt_active(false);
                self.overlay = Some(Overlay::Help);
                return;
            }
            _ => {}
        }
        if self.edit.is_some() {
            self.settings_action(a);
            return;
        }
        if self.dev_pick.is_some() {
            self.settings_action(a);
            return;
        }
        match a {
            Action::Insert(c) => self.insert_char(c),
            Action::Backspace => self.backspace_char(),
            Action::Paste(s) => self.paste_text(s),
            _ => match self.screen {
                Screen::Home => self.home_action(a),
                Screen::Connect => self.connect_action(a),
                Screen::HostWait => self.hostwait_action(a),
                Screen::Incoming => self.incoming_action(a),
                Screen::Call => self.call_action(a),
                Screen::Settings => self.settings_action(a),
            },
        }
    }

    fn insert_char(&mut self, c: char) {
        if self.screen == Screen::Connect {
            self.conn_input.push(c);
        } else if self.screen == Screen::Call && self.focus == Focus::Input {
            if self.input.len() + c.len_utf8() <= crate::proto::TEXT_MAX {
                self.input.push(c);
            }
        }
    }

    fn backspace_char(&mut self) {
        if self.screen == Screen::Connect {
            self.conn_input.pop();
        } else if self.screen == Screen::Call && self.focus == Focus::Input {
            self.input.pop();
        }
    }

    fn paste_text(&mut self, s: String) {
        if self.screen == Screen::Connect {
            self.conn_input.push_str(&s);
        } else if self.screen == Screen::Call && self.focus == Focus::Input {
            if self.input.len() + s.len() <= crate::proto::TEXT_MAX {
                self.input.push_str(&s);
            }
        }
    }

    fn home_action(&mut self, a: Action) {
        match a {
            Action::Up => self.home_sel = self.home_sel.saturating_sub(1),
            Action::Down => self.home_sel = (self.home_sel + 1).min(3),
            Action::Confirm => self.run_home_item(),
            Action::Host => self.start_host(),
            Action::Connect => {
                self.conn_input.clear();
                self.screen = Screen::Connect;
                self.focus = Focus::Input;
            }
            Action::Click(Pane::Side, i) => {
                self.home_sel = i.min(3);
                self.run_home_item();
            }
            Action::Scroll(pane, d) => {
                self.focus = Focus::Pane(pane);
                if pane == Pane::Side {
                    self.home_sel = (self.home_sel as i16 + d.signum()).clamp(0, 3) as usize;
                }
            }
            _ => {}
        }
    }

    fn connect_action(&mut self, a: Action) {
        match a {
            Action::Confirm => {
                let t = self.conn_input.trim().to_string();
                if t.contains(':') && t.len() > 3 {
                    self.conn_input = t.clone();
                    self.start_connect(t);
                } else {
                    self.set_toast("адрес в форме ip:port".into());
                }
            }
            Action::Cancel => {
                self.screen = Screen::Home;
                self.focus = Focus::Pane(Pane::Side);
            }
            _ => {}
        }
    }

    fn hostwait_action(&mut self, a: Action) {
        match a {
            Action::Up => self.host_sel = self.host_sel.saturating_sub(1),
            Action::Down => {
                let n = self.copy_targets().len();
                if n > 0 {
                    self.host_sel = (self.host_sel + 1).min(n - 1);
                }
            }
            Action::Confirm => {
                let targets = self.copy_targets();
                if let Some((_, text)) =
                    targets.get(self.host_sel.min(targets.len().saturating_sub(1)))
                {
                    self.copy_text(text.clone());
                }
            }
            Action::Cancel => {
                self.stop_session();
                self.screen = Screen::Home;
                self.focus = Focus::Pane(Pane::Side);
                self.status = "меню".into();
            }
            _ => {}
        }
    }

    /// Строки экрана ожидания: (что видно, что копируется).
    pub fn copy_targets(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .hint_addrs
            .iter()
            .map(|a| {
                let ip = a.rsplit_once(' ').map(|(_, t)| t).unwrap_or(a);
                (a.clone(), ip.to_string())
            })
            .collect();
        if let Some(code) = &self.room_code {
            out.push((format!("код комнаты: {code}"), code.clone()));
            let target = format!("{code}@{}", self.cfg.server_addr);
            out.push((target.clone(), target));
        }
        out
    }

    fn copy_text(&mut self, text: String) {
        match crate::clipboard::copy(&text) {
            Ok(()) => self.set_toast(format!("скопировано: {text}")),
            Err(e) => self.set_toast(format!("буфер обмена: {e}")),
        }
    }

    fn incoming_action(&mut self, a: Action) {
        match a {
            Action::Accept => {
                if let Some(s) = self.session.as_mut() {
                    s.send(NetCmd::AcceptCall);
                }
                self.screen = Screen::Call;
                self.status = "принимаю…".into();
            }
            Action::Decline => {
                if let Some(s) = self.session.as_mut() {
                    s.send(NetCmd::DeclineCall);
                }
                self.screen = Screen::HostWait;
                self.status = "звонок отклонён".into();
            }
            _ => {}
        }
    }

    fn call_action(&mut self, a: Action) {
        match a {
            Action::ToggleMute => {
                let v = !self.engine.params.mute_mic();
                self.engine.params.set_mute_mic(v);
                self.cfg_save_state();
            }
            Action::ToggleDeaf => {
                let v = !self.engine.params.deaf();
                self.engine.params.set_deaf(v);
                self.cfg_save_state();
            }
            Action::ToggleAutoGain => {
                let v = !self.engine.params.auto_gain();
                self.engine.params.set_auto_gain(v);
                self.cfg_save_state();
            }
            Action::ToggleEcho => self.toggle_echo(),
            Action::GainPeer(d) => {
                let v = (self.engine.params.gain_peer() as i16 + d).clamp(0, 200) as u8;
                self.engine.params.set_gain_peer(v);
                self.cfg_save_state();
            }
            Action::GainMic(d) => {
                let v = (self.engine.params.gain_in() as i16 + d).clamp(0, 200) as u8;
                self.engine.params.set_gain_in(v);
                self.cfg_save_state();
            }
            Action::DeviceCycle { input } => self.cycle_device(input, false),
            Action::SendFile => {
                self.engine.params.set_ptt_active(false);
                self.file_input = Some(String::new());
                self.selected_link = None;
            }
            Action::CancelFile => {
                if let Some(file) = self
                    .chat
                    .iter()
                    .rev()
                    .filter_map(|l| l.attachment.as_ref())
                    .find(|f| f.active())
                    && let Some(session) = &self.session
                {
                    session.send(NetCmd::CancelFile(file.id));
                }
            }
            Action::OpenLink(index) => self.open_link(index),
            Action::NextLink(reverse) => {
                let count = self.chat_targets.borrow().len();
                if count > 0 {
                    self.focus = Focus::Pane(Pane::Main);
                    self.selected_link = Some(match self.selected_link {
                        Some(i) => {
                            if reverse {
                                (i + count - 1) % count
                            } else {
                                (i + 1) % count
                            }
                        }
                        None => self
                            .chat_hits
                            .borrow()
                            .iter()
                            .min_by_key(|h| (h.area.y, h.area.x))
                            .map_or(0, |h| h.index),
                    });
                }
            }
            Action::HangUp => {
                self.engine.params.set_ptt_active(false);
                self.overlay = Some(Overlay::HangUp);
            }
            Action::Confirm => {
                if self.focus == Focus::Input {
                    self.send_chat();
                    self.focus = Focus::Pane(Pane::Main);
                } else if let Some(index) = self.selected_link {
                    self.open_link(index);
                }
            }
            Action::Cancel => {
                if self.focus == Focus::Input {
                    self.focus = Focus::Pane(Pane::Main);
                }
            }
            Action::FocusInput => {
                self.engine.params.set_ptt_active(false);
                self.focus = Focus::Input;
                self.selected_link = None;
                self.chat_scroll = 0;
            }
            Action::NextPane => {
                self.selected_link = None;
                self.engine.params.set_ptt_active(false);
                self.focus = match self.focus {
                    Focus::Pane(Pane::Side) => Focus::Pane(Pane::Main),
                    _ => Focus::Pane(Pane::Side),
                };
            }
            Action::Up => {
                if self.focus == Focus::Pane(Pane::Side) {
                    self.call_sel = self.call_sel.saturating_sub(1);
                }
            }
            Action::Down => {
                if self.focus == Focus::Pane(Pane::Side) {
                    self.call_sel =
                        (self.call_sel + 1).min(self.participant_count().saturating_sub(1));
                }
            }
            Action::PageUp => self.scroll_chat(true),
            Action::PageDown => self.scroll_chat(false),
            Action::Click(Pane::Side, i) => {
                self.call_sel = i.min(self.participant_count().saturating_sub(1));
                self.focus = Focus::Pane(Pane::Side);
            }
            Action::Click(Pane::Main, _) => self.focus = Focus::Pane(Pane::Main),
            Action::Scroll(pane, d) => {
                self.focus = Focus::Pane(pane);
                match pane {
                    Pane::Main => {
                        if d >= 0 {
                            self.chat_scroll = self.chat_scroll.saturating_sub(d as usize);
                        } else {
                            self.chat_scroll =
                                self.chat_scroll.saturating_add(d.unsigned_abs() as usize);
                        }
                    }
                    Pane::Side => {
                        self.call_sel = (self.call_sel as i16 + d.signum())
                            .clamp(0, self.participant_count().saturating_sub(1) as i16)
                            as usize;
                    }
                }
            }
            Action::Ptt(v) => self.engine.params.set_ptt_active(v),
            _ => {}
        }
    }

    fn perform_hangup(&mut self) {
        self.engine.params.set_ptt_active(false);
        if let Some(s) = self.session.as_mut() {
            s.send(NetCmd::HangUp {
                reason: REASON_HANGUP,
            });
        }
        self.status = "завершаю звонок…".into();
    }

    fn settings_action(&mut self, a: Action) {
        if self.edit.is_some() {
            match a {
                Action::Backspace => {
                    if let Some((_, buf)) = self.edit.as_mut() {
                        buf.pop();
                    }
                }
                Action::Insert(c) => {
                    if let Some((row, buf)) = self.edit.as_mut()
                        && buf.len() + c.len_utf8()
                            <= if matches!(*row, ROW_DOWNLOAD | ROW_AVATAR) {
                                4096
                            } else {
                                64
                            }
                    {
                        buf.push(c);
                    }
                }
                Action::Paste(s) => {
                    if let Some((row, buf)) = self.edit.as_mut()
                        && buf.len() + s.len()
                            <= if matches!(*row, ROW_DOWNLOAD | ROW_AVATAR) {
                                4096
                            } else {
                                64
                            }
                    {
                        buf.push_str(&s);
                    }
                }
                Action::Confirm => self.commit_edit(),
                _ => {}
            }
            return;
        }
        if self.dev_pick.is_some() {
            match a {
                Action::Up => {
                    if let Some(pick) = self.dev_pick.as_mut() {
                        pick.idx = pick.idx.saturating_sub(1);
                    }
                }
                Action::Down => {
                    let len = if self.dev_pick.as_ref().unwrap().input {
                        self.devices.inputs.len()
                    } else {
                        self.devices.outputs.len()
                    };
                    if let Some(pick) = self.dev_pick.as_mut() {
                        pick.idx = (pick.idx + 1).min(len);
                    }
                }
                Action::Confirm => self.commit_dev_pick(),
                Action::Click(Pane::Side, index) => {
                    if let Some(pick) = self.dev_pick.as_mut() {
                        pick.idx = index;
                    }
                    self.commit_dev_pick();
                }
                Action::Cancel => self.dev_pick = None,
                _ => {}
            }
            return;
        }
        match a {
            Action::Up => self.settings_sel = self.settings_sel.saturating_sub(1),
            Action::Click(Pane::Side, index) => {
                self.settings_sel = index.min(SETTINGS_ROWS - 1);
                self.settings_toggle();
            }
            Action::Scroll(_, delta) => {
                self.settings_sel = (self.settings_sel as i16 + delta)
                    .clamp(0, (SETTINGS_ROWS - 1) as i16)
                    as usize;
            }
            Action::Down => self.settings_sel = (self.settings_sel + 1).min(SETTINGS_ROWS - 1),
            Action::Left => self.settings_adjust(-5),
            Action::Right => self.settings_adjust(5),
            Action::Confirm => self.settings_toggle(),
            Action::Cancel => {
                self.screen = self.settings_return;
                self.focus = self.settings_focus;
            }
            Action::ToggleEcho => self.toggle_echo(),
            _ => {}
        }
    }

    fn commit_edit(&mut self) {
        let Some((row, buf)) = self.edit.take() else {
            return;
        };
        let buf = buf.trim().to_string();
        match row {
            ROW_NAME => {
                if !buf.is_empty() {
                    self.cfg.name = buf;
                    self.avatar = crate::avatar::display(self.cfg.avatar.as_ref(), &self.cfg.name);
                }
            }
            ROW_PORT => {
                if let Ok(p) = buf.parse::<u16>()
                    && p > 0
                {
                    self.cfg.port = p;
                }
            }
            ROW_AVATAR => {
                self.load_avatar(&buf);
                return;
            }
            ROW_SERVER => self.cfg.server_addr = buf,
            ROW_DOWNLOAD => {
                self.cfg.download_dir = buf;
                if let Some(session) = &self.session {
                    session.send(NetCmd::DownloadDirectory(self.cfg.download_directory()));
                }
            }
            _ => {}
        }
        self.save_all();
    }

    fn commit_dev_pick(&mut self) {
        let Some(pick) = self.dev_pick.take() else {
            return;
        };
        let dev = if pick.idx == 0 {
            String::new()
        } else if pick.input {
            self.devices.inputs[pick.idx - 1].clone()
        } else {
            self.devices.outputs[pick.idx - 1].clone()
        };
        let (ci, co) = self.engine.current_devices();
        if pick.input {
            self.apply_devices(&dev, &co);
        } else {
            self.apply_devices(&ci, &dev);
        }
        self.devices = crate::audio::list_devices();
    }

    fn settings_adjust(&mut self, d: i16) {
        if self.settings_sel == ROW_AVATAR_RESOLUTION {
            self.adjust_avatar_resolution(d.signum());
            return;
        }
        let p = self.engine.params.clone();
        match self.settings_sel {
            ROW_GAIN_IN => p.set_gain_in((p.gain_in() as i16 + d).clamp(0, 200) as u8),
            ROW_GAIN_PEER => p.set_gain_peer((p.gain_peer() as i16 + d).clamp(0, 200) as u8),
            ROW_VAD => p.set_vad_threshold((p.vad_threshold() as i16 + d).clamp(0, 100) as u8),
            _ => return,
        }
        self.cfg_save_state();
    }

    fn settings_toggle(&mut self) {
        match self.settings_sel {
            ROW_NAME | ROW_PORT | ROW_SERVER | ROW_DOWNLOAD | ROW_AVATAR => {
                let val = match self.settings_sel {
                    ROW_NAME => self.cfg.name.clone(),
                    ROW_PORT => self.cfg.port.to_string(),
                    ROW_DOWNLOAD => self.cfg.download_directory().display().to_string(),
                    ROW_AVATAR => self.cfg.avatar_file.clone(),
                    _ => self.cfg.server_addr.clone(),
                };
                self.edit = Some((self.settings_sel, val));
            }
            ROW_GAIN_IN | ROW_GAIN_PEER | ROW_VAD => {}
            ROW_MIC_MODE => {
                let vox = !self.engine.params.vox_mode();
                self.engine.params.set_vox_mode(vox);
                self.cfg.mic_mode = if vox { MicMode::Vox } else { MicMode::Ptt };
                self.cfg_save_state();
            }
            ROW_GATE => {
                self.engine.params.set_noise_gate(!self.noise_gate_on());
                self.cfg_save_state();
            }
            ROW_IN_DEV => {
                self.dev_pick = Some(DevPick {
                    input: true,
                    idx: 0,
                })
            }
            ROW_OUT_DEV => {
                self.dev_pick = Some(DevPick {
                    input: false,
                    idx: 0,
                })
            }
            ROW_ECHO => self.toggle_echo(),
            ROW_FILTER => {
                self.engine
                    .params
                    .set_voice_filter(!self.engine.params.voice_filter());
                self.cfg_save_state();
            }
            ROW_DENOISE => {
                self.engine
                    .params
                    .set_noise_suppression(!self.engine.params.noise_suppression());
                self.cfg_save_state();
            }
            ROW_AVATAR_STYLE => self.cycle_avatars(),
            ROW_AVATAR_RESOLUTION => self.adjust_avatar_resolution(1),
            ROW_AVATAR_CLEAR => {
                self.load_avatar("");
            }
            ROW_AUTO_UPDATE => {
                self.cfg.auto_update = !self.cfg.auto_update;
                self.cfg.save();
            }
            ROW_AUTO_GAIN => {
                self.engine
                    .params
                    .set_auto_gain(!self.engine.params.auto_gain());
                self.cfg_save_state();
            }
            _ => {}
        }
    }

    fn cycle_avatars(&mut self) {
        self.cfg.avatar_style = if self.cfg.avatar_style == crate::avatar::AvatarStyle::Pixel {
            crate::avatar::AvatarStyle::Ascii
        } else {
            crate::avatar::AvatarStyle::Pixel
        };
        self.cfg.save();
    }
    fn load_avatar(&mut self, path: &str) {
        if path.trim().is_empty() {
            self.cfg.avatar = None;
            self.cfg.avatar_original = None;
            self.cfg.avatar_file.clear();
            self.avatar_load = None;
            self.apply_avatar();
            return;
        }
        if self.avatar_load.is_some() {
            self.set_toast("аватарка ещё обрабатывается".into());
            return;
        }
        self.engine.params.set_ptt_active(false);
        let path = crate::config::expand_path(path);
        let source = path.display().to_string();
        let resolution = self.cfg.avatar_resolution;
        let (tx, rx) = std::sync::mpsc::channel();
        self.avatar_load = Some(rx);
        std::thread::spawn(move || {
            let result = crate::avatar::Avatar::load_at(&path, crate::avatar::MAX_SIDE as u16)
                .map(|original| (original.resized(resolution), original, source));
            let _ = tx.send(result);
        });
        self.set_toast("готовим аватарку… голос продолжает работать".into());
    }
    fn adjust_avatar_resolution(&mut self, direction: i16) {
        if self.avatar_load.is_some() {
            self.set_toast("аватарка ещё обрабатывается".into());
            return;
        }
        let sizes = crate::avatar::RESOLUTIONS;
        let index = sizes
            .iter()
            .position(|&n| n == self.cfg.avatar_resolution)
            .unwrap_or(1);
        let resolution = sizes[(index as i16 + direction).rem_euclid(sizes.len() as i16) as usize];
        if self.cfg.avatar.is_none() {
            self.cfg.avatar_resolution = resolution;
            self.cfg.save();
            return;
        }
        let cached = self
            .cfg
            .avatar_original
            .as_ref()
            .filter(|a| a.valid())
            .cloned();
        let source = self.cfg.avatar_file.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.avatar_load = Some(rx);
        std::thread::spawn(move || {
            let original = cached.map(Ok).unwrap_or_else(|| {
                crate::avatar::Avatar::load_at(
                    &crate::config::expand_path(&source),
                    crate::avatar::MAX_SIDE as u16,
                )
                .map_err(|_| {
                    "для старой аватарки выберите картинку заново: исходный файл не найден"
                        .to_string()
                })
            });
            let _ =
                tx.send(original.map(|original| (original.resized(resolution), original, source)));
        });
        self.set_toast(format!("готовим аватарку {resolution}×{resolution}…"));
    }
    fn apply_avatar(&mut self) {
        self.avatar = crate::avatar::display(self.cfg.avatar.as_ref(), &self.cfg.name);
        if let Some(session) = &self.session {
            session.send(NetCmd::Avatar(self.cfg.avatar.clone().map(Arc::new)));
        }
        self.cfg.save();
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

// ── чат-лог ────────────────────────────────────────────────────────────────

fn fmt_ts() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as libc::time_t;
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        #[cfg(unix)]
        libc::localtime_r(&secs, &mut tm);
        #[cfg(windows)]
        libc::localtime_s(&mut tm, &secs);
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
        app.focus = Focus::Input;
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
        assert_eq!(app.focus, Focus::Input);
    }
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

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
        app.focus = Focus::Input;
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
    fn avatar_detail_changes_live_from_cached_source_and_preserves_call_draft() {
        let original = crate::avatar::Avatar::identicon("source").resized(128);
        let cfg = Config {
            avatar: Some(original.resized(32)),
            avatar_original: Some(original),
            avatar_file: "/removed/image.png".into(),
            ..Default::default()
        };
        let mut app = App::for_test(cfg);
        app.screen = Screen::Call;
        app.input = "черновик".into();
        app.open_settings();
        app.settings_sel = ROW_AVATAR_RESOLUTION;
        app.settings_adjust(5);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.avatar_load.is_some() && std::time::Instant::now() < deadline {
            app.poll();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(app.avatar_load.is_none());
        assert_eq!(app.cfg.avatar_resolution, 48);
        assert_eq!(app.avatar.side(), 48);
        assert_eq!(app.cfg.avatar_original.as_ref().unwrap().side(), 128);
        assert_eq!(app.session_screen(), Screen::Call);
        assert_eq!(app.input, "черновик");
    }

    #[test]
    fn settings_preserve_call_and_draft_and_follow_a_hangup() {
        let mut app = App::for_test(Config::default());
        app.screen = Screen::Call;
        app.mode = Some(Mode::Joiner);
        app.input = "Черновик: Привет!".into();
        app.focus = Focus::Input;
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
        assert_eq!(app.focus, Focus::Input);
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
