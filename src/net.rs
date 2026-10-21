// Сетевой движок: UDP, роли хоста и присоединяющегося.
//
// Управление повторяет порядок Discord (наблюдения REA по discord_voice):
//   HANDSHAKE (открытые пакеты)  ↔  Voice Gateway + SelectProtocol
//   ACCEPT + режим шифрования    ↔  encryption_mode aead_xchacha20_poly1305_rtpsize
//   Ping/Pong каждые 5 c         ↔  setPingInterval/setPingCallback
//   ретраи при разрыве           ↔  configureConnectionRetries / fastUdpReconnect
//   обновление адреса peer       ↔  setUdpEndpoint (фича udp_endpoint_update)
//   mute/deafen статусы          ↔  setOnNativeMuteChangedCallback
//
// Работает и через VPN, и напрямую: инициатор — присоединяющийся,
// порт хоста должен быть открыт (проброшен) либо обе стороны в одной VPN-сети.
// Смена адреса во время звонка обрабатывается автоматически: новый источник
// приходящих пакетов становится адресом peer.

use std::collections::VecDeque;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use crate::audio::AudioEngine;
use crate::config::Config;
use crate::proto::*;

pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(5);
pub const MINOR_SLOT: Duration = Duration::from_millis(25);
pub const PEER_DROP_AFTER: Duration = Duration::from_secs(12);
pub const ACCEPT_WAIT: Duration = Duration::from_secs(60);
pub const HANDSHAKE_MAX_ATTEMPTS: u32 = 60;
pub const RETRY_PAUSE: Duration = Duration::from_millis(900);
pub const HANDSHAKE_REPLY_WAIT: Duration = Duration::from_millis(700);
pub const TEXT_RETRY_INTERVAL: Duration = Duration::from_millis(1500);
pub const TEXT_RETRY_MAX: u32 = 4;
pub const STATUS_SEND_INTERVAL: Duration = Duration::from_millis(1000);
pub const SPEAK_TIMEOUT: Duration = Duration::from_millis(400);
pub const MTU: usize = 1500;

/// Всё, о чём net сообщает UI.
#[derive(Debug, Clone)]
pub enum NetEvent {
    /// хост слушает
    Waiting {
        local_addrs: Vec<String>,
    },
    /// код комнаты (звонок через ретранслятор)
    RoomCode(String),
    /// попытка соединения (attempt 0 — быстрый retry после обрыва)
    Joining {
        attempt: u32,
        addr: String,
    },
    /// входящий звонок на хосте
    CallIncoming {
        name: String,
        addr: String,
    },
    InCall {
        peer_name: String,
        peer_addr: String,
    },
    GroupEntered {
        room: String,
        server: String,
    },
    GroupRoster(Vec<crate::group::Participant>),
    GroupText {
        name: String,
        text: String,
    },
    Rtt(u16),
    PeerStatus {
        mute_mic: bool,
        deaf: bool,
        lvl: u8,
    },
    PeerSpeaking(bool),
    PeerAvatar(Option<Arc<crate::avatar::Avatar>>),
    PeerName(String),
    Text(String),
    Transfer(crate::transfer::TransferUpdate),
    /// наше сообщение доставлено
    TextAck(u32),
    CallEnded(String),
    Progress(String),
    Fatal(String),
}

/// Команды UI к net.
#[derive(Debug, Clone)]
pub enum NetCmd {
    AcceptCall,
    Name(String),
    Avatar(Option<Arc<crate::avatar::Avatar>>),
    DeclineCall,
    Text(String),
    SendFile(std::path::PathBuf),
    DownloadDirectory(std::path::PathBuf),
    CancelFile(u64),
    FileDecision {
        id: u64,
        peer_id: Option<u64>,
        accept: bool,
    },
    HangUp {
        reason: u8,
    },
    Stop,
}

pub struct Session {
    pub events: mpsc::UnboundedReceiver<NetEvent>,
    pub cmd_tx: mpsc::UnboundedSender<NetCmd>,
    pub killed: Arc<AtomicBool>,
    handle: tokio::task::JoinHandle<()>,
}

impl Session {
    /// Отправить команду сессии (false — сессия уже мертва).
    pub fn send(&self, cmd: NetCmd) -> bool {
        self.cmd_tx.send(cmd).is_ok()
    }

    pub fn stop(&mut self) {
        let _ = self.cmd_tx.send(NetCmd::Stop);
        self.killed.store(true, Ordering::Relaxed);
        // Drop the membership guard even when cancelling a pending dial.
        // Release the socket promptly so the same local port can be reused.
        self.handle.abort();
    }
}

pub enum Start {
    /// Хост слушает порт (все интерфейсы) и обслуживает входящие решением:
    /// Enter — принять, Esc — отклонить.
    Host { cfg: Config },
    /// Присоединяющийся к host: IP:порт или hostname:порт (или код@IP:порт —
    /// через ретранслятор).
    Joiner { cfg: Config, target: String },
}

pub fn spawn(start: Start, engine: Arc<AudioEngine>) -> Session {
    let (events_tx, events) = mpsc::unbounded_channel();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (media_tx, media_rx) = mpsc::channel(crate::audio::MAX_PENDING_AUDIO);
    let killed = Arc::new(AtomicBool::new(false));
    let k2 = killed.clone();
    let handle = tokio::spawn(async move {
        let _media_route = engine.route_media(media_tx);
        match start {
            Start::Host { cfg } => {
                if !cfg.server_addr.trim().is_empty() {
                    let server = cfg.server_addr.clone();
                    crate::group::run(
                        cfg,
                        room_code(),
                        server,
                        events_tx,
                        cmd_rx,
                        media_rx,
                        engine,
                    )
                    .await;
                } else {
                    run_host(cfg, events_tx, cmd_rx, media_rx, engine, k2).await;
                }
            }
            Start::Joiner { cfg, target } => {
                let (room, server) = split_room_target(&target);
                if let Some(room) = room {
                    crate::group::run(cfg, room, server, events_tx, cmd_rx, media_rx, engine).await;
                } else if target.contains('@') {
                    let _ = events_tx.send(NetEvent::Fatal("неверный код комнаты".into()));
                } else {
                    run_joiner(cfg, target, events_tx, cmd_rx, media_rx, engine, k2).await;
                }
            }
        }
    });
    Session {
        events,
        cmd_tx,
        killed,
        handle,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// RM_DEBUG=1 — трассировка сети в stderr.
macro_rules! dbg_net {
    ($($t:tt)*) => {
        if std::env::var("RM_DEBUG").ok().as_deref() == Some("1") {
            eprintln!("[net] {}", format!($($t)*));
        }
    };
}

/// Локальные адреса интерфейсов (подсказка хосту, что дать собеседнику).
/// Тоннельные интерфейсы (wireguard/tun/…, флаг POINTOPOINT) помечаются VPN.
fn local_addrs(port: u16) -> Vec<String> {
    let mut out: Vec<(u8, String)> = Vec::new();
    #[cfg(unix)]
    unsafe {
        let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifap) == 0 {
            let mut cur = ifap;
            while !cur.is_null() {
                if let Some(a) = std::ptr::NonNull::new((*cur).ifa_addr) {
                    let sa: &libc::sockaddr = &*a.as_ptr();
                    if sa.sa_family as i32 == libc::AF_INET {
                        let sin = a.as_ptr() as *const libc::sockaddr_in;
                        let ip = std::net::Ipv4Addr::from(u32::from_be((*sin).sin_addr.s_addr));
                        let ifname = std::ffi::CStr::from_ptr((*cur).ifa_name)
                            .to_string_lossy()
                            .into_owned();
                        if !ip.is_loopback() {
                            let flags = (*cur).ifa_flags;
                            let (prio, tag) = if flags & libc::IFF_POINTOPOINT as u32 != 0 {
                                (0, "VPN")
                            } else if flags & libc::IFF_BROADCAST as u32 != 0 {
                                (1, "LAN")
                            } else {
                                (2, "if")
                            };
                            out.push((prio, format!("{ifname} [{tag}] {ip}:{port}")));
                        }
                    }
                }
                cur = (*cur).ifa_next;
            }
            libc::freeifaddrs(ifap);
        }
    }
    #[cfg(windows)]
    if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0")
        && socket.connect("1.1.1.1:80").is_ok()
        && let Ok(addr) = socket.local_addr()
    {
        // UDP connect selects a local route without sending any packets.
        out.push((1, format!("локальный адрес {}:{port}", addr.ip())));
    }
    out.sort();
    out.into_iter().map(|(_, s)| s).collect()
}

fn rnd_vec(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    // Fail closed: never create predictable X25519 keys/nonce domains if the OS RNG fails.
    getrandom::fill(&mut buf).expect("системный генератор случайных чисел недоступен");
    buf
}

pub(crate) fn rnd32() -> [u8; 32] {
    let v = rnd_vec(32);
    let mut a = [0u8; 32];
    a.copy_from_slice(&v);
    a
}

fn ssrc_rand() -> u32 {
    0x4000_0001 | (u32::from_be_bytes(rnd_vec(4).try_into().unwrap()) & 0x3FFF_FFFF)
}

/// `target` — ip:port или hostname:port (IPV4/IPV6).
fn select_addr(target: &str) -> Option<SocketAddr> {
    use std::str::FromStr;
    if let Ok(a) = SocketAddr::from_str(target) {
        return a.is_ipv4().then_some(a);
    }
    if let Some((host, port)) = target.rsplit_once(':') {
        if let Ok(mut addrs) = (host, port.parse::<u16>().ok()?).to_socket_addrs() {
            if let Some(a) = addrs.find(SocketAddr::is_ipv4) {
                return Some(a);
            }
        }
    }
    None
}

async fn send_open(sock: &Arc<UdpSocket>, to: SocketAddr, p: &Packet) {
    let _ = sock.send_to(&p.encode_open(), to).await.unwrap_or(0);
}

async fn send_secure(sock: &Arc<UdpSocket>, cipher: &mut Cipher, peer: SocketAddr, p: &Packet) {
    let w = Packet::encrypt_wire(cipher, p);
    let _ = sock.send_to(&w, peer).await.unwrap_or(0);
}

// ── Established: общий цикл звонка для обеих ролей ──────────────────────────

enum Outcome {
    /// нормальное завершение (наш hangup или Bye от peer)
    Closed,
    /// потеря связи без By
    Lost,
}

async fn run_established(
    sock: Arc<UdpSocket>,
    mut cipher: Cipher,
    peer0: SocketAddr,
    ssrc_self: u32,
    events: mpsc::UnboundedSender<NetEvent>,
    cmd_rx: &mut mpsc::UnboundedReceiver<NetCmd>,
    media_rx: &mut mpsc::Receiver<Vec<u8>>,
    engine: Arc<AudioEngine>,
    accepted_hello: Option<([u8; 32], Packet)>,
    download_directory: &mut std::path::PathBuf,
    profile: &mut crate::avatar::Profile,
    own_name: &mut crate::profile::Name,
) -> Outcome {
    let mut files = crate::transfer::spawn(download_directory.clone());
    let mut name_delivery = crate::avatar::Delivery::default();
    let mut name_received = 0;
    let mut avatar_delivery = crate::avatar::Delivery::default();
    let mut avatar_received = 0u64;
    let mut avatar_assembly = crate::avatar::Assembly::default();
    let mut seen_text = VecDeque::new();
    let mut peer = peer0;
    let mut last_ctl_rx: u64 = 0;
    let mut last_audio_seq_rx: u32 = 0;
    let mut last_rx = Instant::now();
    let mut last_ping = Instant::now() - KEEPALIVE_INTERVAL;
    let mut last_status = Instant::now() - STATUS_SEND_INTERVAL;
    let mut ping_id: u64 = 0;
    let mut ping_sent: VecDeque<(u64, u64)> = VecDeque::new();
    let mut last_audio_seen = Instant::now();
    let mut peer_speaking = false;
    let mut audio_seq: u32 = 0;
    let mut previous_audio_seq: Option<u32> = None;
    let mut msg_counter: u32 = 0;
    let mut pending_text: VecDeque<(u32, String, Instant, u32)> = VecDeque::new();
    let mut last_my_status = (engine.params.mute_mic(), engine.params.deaf());
    let mut buf = vec![0u8; MTU];

    while media_rx.try_recv().is_ok() {}
    engine.prefill_out();
    let mut maintenance = tokio::time::interval(MINOR_SLOT);
    maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            // ── входящие пакеты ─────────────────────────────────────────────
            res = sock.recv_from(&mut buf) => {
                let (n, from) = match res {
                    Ok(v) => v,
                    Err(_) => { tokio::time::sleep(Duration::from_millis(50)).await; continue; }
                };
                // если мы заняты звонком, а нам дозвонился другой абонент — BUSY
                if classify(&buf[..n]) == WireClass::Open {
                    if let Some(Packet::Hello(hello)) = Packet::parse_open(&buf[..n]) {
                        // ACCEPT can be lost. A retry from this same dial campaign
                        // must receive the same ACCEPT, rather than a BUSY response.
                        if let Some((pub_key, accept)) = &accepted_hello
                            && from == peer && hello.pub_key == *pub_key
                        {
                            dbg_net!("повтор HELLO: повторяем ACCEPT для {from}");
                            send_open(&sock, from, accept).await;
                        } else {
                            send_open(&sock, from, &Packet::Decline { reason: REASON_BUSY }).await;
                        }
                    }
                    continue;
                }
                let pkt = Packet::decrypt_wire(&cipher, &buf[..n], &mut last_ctl_rx, &mut last_audio_seq_rx);
                if pkt.is_some() {
                    last_rx = Instant::now();
                    // Accept endpoint changes only after authentication.
                    peer = from;
                }
                if std::env::var("RM_DEBUG").ok().as_deref() == Some("1") {
                    let kind = match &pkt {
                        Some(Packet::Audio{..}) => "AUDIO",
                        Some(Packet::Ping{..}) => "PING",
                        Some(Packet::Pong{..}) => "PONG",
                        Some(Packet::Text{..}) => "TEXT",
                        Some(Packet::Ack{..}) => "ACK",
                        Some(Packet::Status{..}) => "STATUS",
                        Some(Packet::Bye{..}) => "BYE",
                        Some(_) => "OTHER",
                        None => "<расшифровка не удалась>",
                    };
                    eprintln!("[net] rx {} {} от {}", kind, classify(&buf[..n]) == WireClass::Secure, from);
                }
                match pkt {
                    Some(Packet::Name{revision,name}) => {
                        send_secure(&sock,&mut cipher,peer,&Packet::NameAck{revision}).await;
                        if revision > name_received {
                            name_received=revision;dbg_net!("name received: {name} revision={revision}");
                            let _=events.send(NetEvent::PeerName(name));
                        }
                    }
                    Some(Packet::NameAck{revision}) => name_delivery.ack(revision),
                    Some(Packet::Avatar{revision,avatar}) => {
                        send_secure(&sock,&mut cipher,peer,&Packet::AvatarAck{revision}).await;
                        if revision>avatar_received {avatar_received=revision;let _=events.send(NetEvent::PeerAvatar(avatar));dbg_net!("avatar received revision={revision}");}
                    }
                    Some(Packet::AvatarChunk{revision,index,count,bytes}) => {
                        if revision <= avatar_received {
                            send_secure(&sock,&mut cipher,peer,&Packet::AvatarAck{revision}).await;
                        } else if let Some(avatar) = avatar_assembly.push(revision,index,count,bytes) {
                            avatar_received=revision;
                            send_secure(&sock,&mut cipher,peer,&Packet::AvatarAck{revision}).await;
                            let _=events.send(NetEvent::PeerAvatar(Some(avatar)));
                            dbg_net!("avatar received revision={revision}");
                        }
                    }
                    Some(Packet::AvatarAck{revision}) => avatar_delivery.ack(revision),
                    Some(Packet::File(packet)) => { let _ = files.commands.try_send(crate::transfer::Command::Wire(packet)); }
                    Some(Packet::Audio { payload, seq, .. }) => {
                        if payload.len() <= AUDIO_PAYLOAD_MAX {
                            last_audio_seen = Instant::now();
                            let missing = previous_audio_seq.map_or(0, |previous| seq.wrapping_sub(previous).saturating_sub(1));
                            previous_audio_seq = Some(seq);
                            if missing > 0 { dbg_net!("audio: восстанавливаем {} пропущенных кадров", missing.min(3)); }
                            if let Some(samples) = engine.decode_opus(&payload, missing) {
                                engine.push_out(&samples);
                            }
                            if !peer_speaking {
                                peer_speaking = true;
                                let _ = events.send(NetEvent::PeerSpeaking(true));
                            }
                        }
                    }
                    Some(Packet::Ping { msg_id, ts_ms }) => {
                        send_secure(&sock, &mut cipher, peer, &Packet::Pong { msg_id, ts_ms }).await;
                    }
                    Some(Packet::Pong { msg_id, ts_ms }) => {
                        let now = now_ms();
                        if let Some(idx) = ping_sent.iter().position(|&(id, _)| id == msg_id) {
                            ping_sent.remove(idx);
                            let rtt = now.saturating_sub(ts_ms).min(65535) as u16;
                            dbg_net!("rtt={}", rtt);
                            let _ = events.send(NetEvent::Rtt(rtt));
                        }
                    }
                    Some(Packet::Text { msg_id, text }) => {
                        send_secure(&sock, &mut cipher, peer, &Packet::Ack { msg_id }).await;
                        if !seen_text.contains(&msg_id) {
                            seen_text.push_back(msg_id);
                            if seen_text.len()>256 { seen_text.pop_front(); }
                            let _ = events.send(NetEvent::Text(text));
                        }
                    }
                    Some(Packet::Ack { msg_id }) => {
                        let _ = events.send(NetEvent::TextAck(msg_id));
                        pending_text.retain(|(id, _, _, _)| *id != msg_id);
                    }
                    Some(Packet::Status { mute_mic, deaf, lvl }) => {
                        dbg_net!("rx status mute={} deaf={} lvl={}", mute_mic, deaf, lvl);
                        let _ = events.send(NetEvent::PeerStatus { mute_mic, deaf, lvl });
                    }
                    Some(Packet::Bye { reason }) => {
                        let msg = if reason == REASON_HANGUP {
                            "собеседник завершил звонок".to_string()
                        } else {
                            "звонок завершён".to_string()
                        };
                        let _ = events.send(NetEvent::CallEnded(msg));
                        return Outcome::Closed;
                    }
                    _ => {}
                }
            }

            // ── исходящие аудио-кадры ───────────────────────────────────────
            Some(frame) = media_rx.recv() => {
                dbg_net!("tx audio seq={} len={}", audio_seq, frame.len());
                audio_seq = audio_seq.wrapping_add(1);
                let wire = Packet::encrypt_wire(
                    &mut cipher,
                    &Packet::Audio { ssrc: ssrc_self, seq: audio_seq, payload: frame },
                );
                let _ = sock.send_to(&wire, peer).await.unwrap_or(0);
            }

            // ── команды UI ──────────────────────────────────────────────────
            Some(cmd) = cmd_rx.recv() => {
                dbg_net!("← cmd {:?}", cmd);
                match cmd {
                    NetCmd::Name(value) => own_name.set(&value),
                    NetCmd::Avatar(avatar) => profile.set(avatar),
                    NetCmd::SendFile(path) => {
                        if files.commands.try_send(crate::transfer::Command::Send(path)).is_err() { let _ = events.send(NetEvent::Progress("очередь передачи занята, повторите отправку".into())); }
                    }
                    NetCmd::DownloadDirectory(path) => { *download_directory=path.clone(); files.set_directory(path); }
                    NetCmd::FileDecision{id,accept,..} => {let _=files.commands.try_send(if accept {crate::transfer::Command::Accept(id)} else {crate::transfer::Command::Decline(id)});}
                    NetCmd::CancelFile(id) => { let _ = files.commands.try_send(crate::transfer::Command::Cancel(id)); }
                    NetCmd::Text(t) => {
                        msg_counter += 1;
                        pending_text.push_back((msg_counter, t.clone(), Instant::now(), 0));
                        send_secure(&sock, &mut cipher, peer,
                            &Packet::Text { msg_id: msg_counter, text: t }).await;
                    }
                    NetCmd::HangUp { reason } => {
                        send_secure(&sock, &mut cipher, peer, &Packet::Bye { reason }).await;
                        let _ = events.send(NetEvent::CallEnded("звонок завершён".into()));
                        return Outcome::Closed;
                    }
                    NetCmd::Stop => {
                        send_secure(&sock, &mut cipher, peer, &Packet::Bye { reason: REASON_HANGUP }).await;
                        let _ = events.send(NetEvent::CallEnded("звонок завершён".into()));
                        return Outcome::Closed;
                    }
                    NetCmd::AcceptCall | NetCmd::DeclineCall => {}
                }
            }

            Some(packet) = files.packets.recv() => {
                send_secure(&sock, &mut cipher, peer, &Packet::File(packet)).await;
            }
            Some(update) = files.updates.recv() => { let _ = events.send(NetEvent::Transfer(update)); }

            // ── мелкие периодические дела ───────────────────────────────────
            _ = maintenance.tick() => {
                let now = Instant::now();
                if let Some(packet)=own_name.next(&mut name_delivery,now) {send_secure(&sock,&mut cipher,peer,&packet).await;}
                if let Some(packet)=profile.next(&mut avatar_delivery,now) {send_secure(&sock,&mut cipher,peer,&packet).await;}
                if now.duration_since(last_ping) >= KEEPALIVE_INTERVAL {
                    last_ping = now;
                    dbg_net!("→ ping");
                    ping_id += 1;
                    let ts = now_ms();
                    ping_sent.push_back((ping_id, ts));
                    while ping_sent.len() > 8 { ping_sent.pop_front(); }
                    send_secure(&sock, &mut cipher, peer, &Packet::Ping { msg_id: ping_id, ts_ms: ts }).await;
                }
                let my = (engine.params.mute_mic(), engine.params.deaf());
                if my != last_my_status || now.duration_since(last_status) >= STATUS_SEND_INTERVAL {
                    last_status = now;
                    last_my_status = my;
                    let lvl = engine.params.in_level();
                    dbg_net!("tx status mute={} deaf={} lvl={}", my.0, my.1, lvl);
                    send_secure(&sock, &mut cipher, peer,
                        &Packet::Status { mute_mic: my.0, deaf: my.1, lvl }).await;
                }
                let mut resend: Vec<(u32, String)> = Vec::new();
                pending_text.retain_mut(|(id, text, sent, tries)| {
                    if now.duration_since(*sent) >= TEXT_RETRY_INTERVAL {
                        if *tries >= TEXT_RETRY_MAX { return false; }
                        *tries += 1;
                        *sent = now;
                        resend.push((*id, text.clone()));
                    }
                    true
                });
                for (id, text) in resend {
                    send_secure(&sock, &mut cipher, peer, &Packet::Text { msg_id: id, text }).await;
                }
                if peer_speaking && now.duration_since(last_audio_seen) > SPEAK_TIMEOUT {
                    peer_speaking = false;
                    let _ = events.send(NetEvent::PeerSpeaking(false));
                }
                if now.duration_since(last_rx) > PEER_DROP_AFTER {
                    let _ = events.send(NetEvent::CallEnded("собеседник не отвечает".into()));
                    return Outcome::Lost;
                }
            }
        }
    }
}

/// Код комнаты: 4 символа без путаницы (I/1/O/0), как на хосте, так и судит joiner.
const ROOM_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

fn room_code() -> [u8; 4] {
    let mut out = [0u8; 4];
    let r = rnd32();
    for (i, o) in out.iter_mut().enumerate() {
        *o = ROOM_ALPHABET[(r[i] as usize) % ROOM_ALPHABET.len()];
    }
    out
}

fn room_to_string(code: &[u8; 4]) -> String {
    code.iter().map(|c| *c as char).collect()
}

/// Разбор строки цели `код@адрес` или `адрес`.
fn split_room_target(target: &str) -> (Option<[u8; 4]>, String) {
    match target.split_once('@') {
        Some((code, addr)) => {
            let bytes = code.trim().to_uppercase().into_bytes();
            let room = if bytes.len() == 4 && bytes.iter().all(|c| ROOM_ALPHABET.contains(c)) {
                let mut r = [0u8; 4];
                r.copy_from_slice(&bytes);
                Some(r)
            } else {
                None
            };
            (room, addr.trim().to_string())
        }
        None => (None, target.trim().to_string()),
    }
}

/// ── HOST ────────────────────────────────────────────────────────────────────

/// Хост крутится вечно (как Discord в голосовом канале): потерянный звонок не
/// останавливает обслуживание — продолжает получать новые HELLO.
async fn run_host(
    cfg: Config,
    events_tx: mpsc::UnboundedSender<NetEvent>,
    mut cmd_rx: mpsc::UnboundedReceiver<NetCmd>,
    mut media_rx: mpsc::Receiver<Vec<u8>>,
    engine: Arc<AudioEngine>,
    killed: Arc<AtomicBool>,
) {
    let events = events_tx.clone();
    let bind = format!("0.0.0.0:{}", cfg.port);
    let sock = match UdpSocket::bind(&bind).await {
        Ok(s) => Arc::new(s),
        Err(e) => {
            let _ = events.send(NetEvent::Fatal(format!("порт {}: {e}", cfg.port)));
            return;
        }
    };
    let _ = events.send(NetEvent::Waiting {
        local_addrs: local_addrs(cfg.port),
    });

    let mut download_directory = cfg.download_directory();
    let mut profile = crate::avatar::Profile::new(cfg.avatar.as_ref());
    let mut own_name = crate::profile::Name::new(&cfg.name);
    let mut buf = vec![0u8; MTU];

    // серверная комната (если настроена): код + периодическая регистрация
    let server: Option<SocketAddr> = if cfg.server_addr.is_empty() {
        None
    } else {
        tokio::net::lookup_host(&cfg.server_addr)
            .await
            .ok()
            .and_then(|mut it| it.find(SocketAddr::is_ipv4))
    };
    if let Some(s) = server {
        dbg_net!("host: комната через сервер {}", s);
    } else if !cfg.server_addr.is_empty() {
        let _ = events.send(NetEvent::Fatal(format!(
            "не могу разобрать адрес сервера: {}",
            cfg.server_addr
        )));
        return;
    }
    let mut room: Option<[u8; 4]> = None;
    if let Some(s) = server {
        let code = room_code();
        let _ = events_tx.send(NetEvent::RoomCode(room_to_string(&code)));
        let _ = s;
        room = Some(code);
        dbg_net!("host: код комнаты {}", room_to_string(&code));
    }
    let _membership = server
        .zip(room)
        .map(|(addr, code)| remember_me::server::Membership::new(sock.clone(), addr, code));
    let mut registration_tick = tokio::time::interval(Duration::from_secs(4));
    registration_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    #[allow(unused_assignments)] // incoming пере-/перезаписывается в select
    loop {
        // Между звонками кодек продолжает клать кадры в канал — сливаем старьё.
        while media_rx.try_recv().is_ok() {}

        // фаза 1: ждём HELLO
        let mut incoming: Option<(String, [u8; 32], Vec<u8>, SocketAddr)> = None;
        // регистрируем комнату (если сервер настроен) — каждые 4 с держим TTL
        if let Some(s) = server {
            if let Some(code) = room {
                let _ = sock
                    .send_to(&Packet::RelayReg { room: code }.encode_open(), s)
                    .await
                    .unwrap_or(0);
            }
        }
        loop {
            tokio::select! {
                res = sock.recv_from(&mut buf) => {
                    if let Ok((n, from)) = res {
                        if server.is_some_and(|s| s != from) { continue; }
                        if let Some(Packet::Hello(h)) = Packet::parse_open(&buf[..n]) {
                            dbg_net!("host: HELLO от {} pub[:5]={:02x?}", from, &h.pub_key[..5]);
                            incoming = Some((h.name, h.pub_key, h.modes, from));
                            break;
                        }
                    }
                }
                maybe = cmd_rx.recv() => match maybe {
                    Some(NetCmd::DownloadDirectory(path))=>download_directory=path,
                    Some(NetCmd::Name(value))=>own_name.set(&value),
                    Some(NetCmd::Avatar(avatar))=>profile.set(avatar),
                    Some(NetCmd::Stop) | None => return,
                    _ => {}
                },
                _ = registration_tick.tick() => {
                    while media_rx.try_recv().is_ok() {}
                    if killed.load(Ordering::Relaxed) { return; }
                    if let Some(s) = server && let Some(code) = room {
                        send_open(&sock, s, &Packet::RelayReg { room: code }).await;
                    }
                }
            }
        }
        if killed.load(Ordering::Relaxed) {
            return;
        }
        let (name, pub_key, modes, from) = incoming.unwrap();

        if !modes.contains(&MODE_XCHACHA20POLY1305) {
            let _ = events.send(NetEvent::Progress(
                "несовместимая версия: обновите клиент у обоих участников".into(),
            ));
            // несовпадение набора encryption modes (как у Discord) — отклоняем
            send_open(
                &sock,
                from,
                &Packet::Decline {
                    reason: REASON_VERSION,
                },
            )
            .await;
            continue;
        }

        // фаза 2: решение по входящему
        let shown_addr = if server.is_some() {
            format!("комната {} (ретранслятор)", room_to_string(&room.unwrap()))
        } else {
            from.to_string()
        };
        let _ = events.send(NetEvent::CallIncoming {
            name: name.clone(),
            addr: shown_addr,
        });
        let mut accepted: Option<bool> = None;
        let decision_deadline = tokio::time::Instant::now() + ACCEPT_WAIT;
        send_open(
            &sock,
            from,
            &Packet::Incoming {
                modes: vec![MODE_XCHACHA20POLY1305],
            },
        )
        .await;
        while accepted.is_none() {
            tokio::select! {
                maybe = cmd_rx.recv() => match maybe {
                    Some(NetCmd::DownloadDirectory(path))=>download_directory=path,
                    Some(NetCmd::Name(value))=>own_name.set(&value),
                    Some(NetCmd::Avatar(avatar))=>profile.set(avatar),
                    Some(NetCmd::AcceptCall) => accepted = Some(true),
                    Some(NetCmd::DeclineCall) => accepted = Some(false),
                    Some(NetCmd::Stop) | None => {
                        send_open(&sock, from, &Packet::Decline { reason: REASON_BUSY }).await;
                        return;
                    }
                    _ => {}
                },
                res = sock.recv_from(&mut buf) => {
                    if let Ok((n, source)) = res
                        && let Some(Packet::Hello(hello)) = Packet::parse_open(&buf[..n])
                    {
                        if source == from && hello.pub_key == pub_key {
                            send_open(&sock, source, &Packet::Incoming { modes: vec![MODE_XCHACHA20POLY1305] }).await;
                        } else {
                            send_open(&sock, source, &Packet::Decline { reason: REASON_BUSY }).await;
                        }
                    }
                }
                _ = registration_tick.tick() => {
                    if let Some(s) = server && let Some(code) = room {
                        send_open(&sock, s, &Packet::RelayReg { room: code }).await;
                    }
                }
                _ = tokio::time::sleep_until(decision_deadline) => {
                    while media_rx.try_recv().is_ok() {}
                    send_open(&sock, from, &Packet::Decline { reason: REASON_TIMEOUT }).await;
                    let _ = events.send(NetEvent::CallEnded("звонок сорвался".into()));
                    accepted = Some(false);
                }
            }
        }

        if !accepted.unwrap() {
            send_open(
                &sock,
                from,
                &Packet::Decline {
                    reason: REASON_DECLINED,
                },
            )
            .await;
            let _ = events.send(NetEvent::CallEnded("звонок отклонён".into()));
            continue;
        }

        // фаза 3: ACCEPT + сессия
        let secret = x25519_dalek::StaticSecret::from(rnd32());
        let peer_pub = x25519_dalek::PublicKey::from(pub_key);
        let shared = secret.diffie_hellman(&peer_pub);
        let prefixes = rnd32();
        let ssrc_self = ssrc_rand();
        let ssrc_peer = ssrc_rand();
        dbg_net!(
            "host self_pub[:5]={:02x?}",
            &x25519_dalek::PublicKey::from(&secret).to_bytes()[..5]
        );
        let acc = Packet::Accept {
            mode: MODE_XCHACHA20POLY1305,
            host_pub: x25519_dalek::PublicKey::from(&secret).to_bytes(),
            prefixes,
            ssrc_joiner: ssrc_peer,
            ssrc_host: ssrc_self,
            name: own_name.value.clone(),
        };
        let _ = sock.send_to(&acc.encode_open(), from).await.unwrap_or(0);

        let shown_peer = if server.is_some() {
            format!("комната {} (ретранслятор)", room_to_string(&room.unwrap()))
        } else {
            from.to_string()
        };
        let _ = events.send(NetEvent::InCall {
            peer_name: name,
            peer_addr: shown_peer,
        });
        dbg_net!(
            "host keys: shared[:6]={:02x?} prefixes[:6]={:02x?} peerpub[:6]={:02x?}",
            &shared.to_bytes()[..6],
            &prefixes[..6],
            &peer_pub.to_bytes()[..6]
        );
        let _ = run_established(
            sock.clone(),
            Cipher::new(&shared.to_bytes(), &prefixes, true),
            from,
            ssrc_self,
            events_tx.clone(),
            &mut cmd_rx,
            &mut media_rx,
            engine.clone(),
            Some((pub_key, acc)),
            &mut download_directory,
            &mut profile,
            &mut own_name,
        )
        .await;

        let _ = events.send(NetEvent::Waiting {
            local_addrs: local_addrs(cfg.port),
        });
    }
}

// ── JOINER ──────────────────────────────────────────────────────────────────

/// Присоединяющийся: цикл дозвона (свежие ключи на каждую попытку), затем
/// установление связи. Потеря peer → возврат к фазе дозвона (reconnect).
async fn run_joiner(
    cfg: Config,
    target: String,
    events_tx: mpsc::UnboundedSender<NetEvent>,
    mut cmd_rx: mpsc::UnboundedReceiver<NetCmd>,
    mut media_rx: mpsc::Receiver<Vec<u8>>,
    engine: Arc<AudioEngine>,
    killed: Arc<AtomicBool>,
) {
    let events = events_tx.clone();
    let sock = match UdpSocket::bind("0.0.0.0:0").await {
        Ok(s) => Arc::new(s),
        Err(e) => {
            let _ = events.send(NetEvent::Fatal(format!("локальный порт: {e}")));
            return;
        }
    };

    // код@адрес или просто адрес
    let (room, addr_str) = split_room_target(&target);
    if target.contains('@') && room.is_none() {
        let _ = events.send(NetEvent::Fatal(
            "формат: КОД@адрес — код 4 символа (напр., K7T2@1.2.3.4:51900)".into(),
        ));
        return;
    }
    if let Some(code) = room {
        dbg_net!(
            "joiner: звоним по комнате {} через {}",
            room_to_string(&code),
            addr_str
        );
    }

    let mut download_directory = cfg.download_directory();
    let mut profile = crate::avatar::Profile::new(cfg.avatar.as_ref());
    let mut own_name = crate::profile::Name::new(&cfg.name);
    let mut buf = vec![0u8; MTU];
    let relay_addr = if room.is_some() {
        select_addr(&addr_str)
    } else {
        None
    };
    let _membership = relay_addr
        .zip(room)
        .map(|(addr, code)| remember_me::server::Membership::new(sock.clone(), addr, code));

    #[allow(unused_assignments)]
    'outer: loop {
        // сливаем кадры, оставшиеся от прошлой сессии
        while media_rx.try_recv().is_ok() {}
        let mut attempt: u32 = 0;
        let mut established: Option<(
            SocketAddr,
            [u8; 32], // shared
            [u8; 32], // prefixes
            String,   // имя хоста
            u8,       // mode
        )> = None;

        // фаза 1: дозвон (как «Звонок…»). Одна эфемерная пара на кампанию
        // (у Discord токен ждущего channels живёт до select protocol) — ACCEPT
        // хоста может дожидаться попытки N, а уйти в N+1, тогда ключи должны
        // совпасть в любом случае.
        let campaign_secret = x25519_dalek::StaticSecret::from(rnd32());
        let campaign_pub: [u8; 32] = x25519_dalek::PublicKey::from(&campaign_secret).to_bytes();
        'dial: loop {
            if killed.load(Ordering::Relaxed) {
                break 'outer;
            }
            attempt += 1;
            let addr = match select_addr(&addr_str) {
                Some(a) => a,
                None => {
                    if attempt >= HANDSHAKE_MAX_ATTEMPTS {
                        let _ = events.send(NetEvent::CallEnded(format!(
                            "не удалось разрешить адрес: {addr_str}"
                        )));
                        break 'outer;
                    }
                    tokio::time::sleep(RETRY_PAUSE).await;
                    continue;
                }
            };
            let _ = events.send(NetEvent::Joining {
                attempt,
                addr: addr.to_string(),
            });

            let hb = Packet::Hello(Hello {
                name: own_name.value.clone(),
                pub_key: campaign_pub,
                modes: vec![MODE_XCHACHA20POLY1305],
            });
            // регистрирует себя в комнате (если звоним через ретранслятор)
            if let Some(code) = room {
                let _ = sock
                    .send_to(&Packet::RelayReg { room: code }.encode_open(), addr)
                    .await
                    .unwrap_or(0);
            }
            let hello_wire = hb.encode_open();
            dbg_net!(
                "joiner → HELLO pub[:5]={:02x?} wire[:38]={:02x?}",
                &campaign_pub[..5],
                &hello_wire[..38]
            );
            let _ = sock.send_to(&hello_wire, addr).await.unwrap_or(0);

            // ожидание ACCEPT / DECLINE
            let reply_deadline = tokio::time::Instant::now() + HANDSHAKE_REPLY_WAIT;
            let reply = loop {
                tokio::select! {
                    res = sock.recv_from(&mut buf) => {
                        match res {
                            Ok((n, from)) if from == addr => {
                                match Packet::parse_open(&buf[..n]) {
                                    Some(Packet::Incoming { .. }) => {
                                        let _ = events.send(NetEvent::Progress(
                                            "вызов доставлен хосту; ждём принятия клавишей Enter".into()));
                                    }
                                    Some(Packet::Accept { .. } | Packet::Decline { .. }) => break Some((n, from)),
                                    _ => {}
                                }
                            }
                            Ok(_) => {},
                            Err(_) => break None,
                        }
                    }
                    maybe = cmd_rx.recv() => match maybe {
                    Some(NetCmd::DownloadDirectory(path))=>download_directory=path,
                    Some(NetCmd::Name(value))=>own_name.set(&value),
                    Some(NetCmd::Avatar(avatar))=>profile.set(avatar),
                        Some(NetCmd::Stop | NetCmd::HangUp { .. }) | None => break 'outer,
                        _ => {},
                    },
                    _ = tokio::time::sleep_until(reply_deadline) => break None,
                }
            };

            match reply {
                Some((n, from)) => match Packet::parse_open(&buf[..n]) {
                    Some(Packet::Accept {
                        mode,
                        host_pub,
                        prefixes,
                        name,
                        ..
                    }) => {
                        dbg_net!(
                            "joiner: ACCEPT от {} host_pub[:5]={:02x?} мой pub[:5]={:02x?}",
                            from,
                            &host_pub[..5],
                            &campaign_pub[..5]
                        );
                        // хост сам выбрал режим из нашего списка Hello.modes;
                        // мы поддерживаем только XChaCha20-Poly1305
                        if mode != MODE_XCHACHA20POLY1305 {
                            let _ = events
                                .send(NetEvent::CallEnded("нет общего режима шифрования".into()));
                            break 'outer;
                        }
                        let shared = campaign_secret
                            .diffie_hellman(&x25519_dalek::PublicKey::from(host_pub))
                            .to_bytes();
                        established = Some((from, shared, prefixes, name, mode));
                        break 'dial;
                    }
                    Some(Packet::Decline { reason }) => {
                        let msg = if reason == REASON_BUSY {
                            "хост занят другим звонком".to_string()
                        } else if reason == REASON_VERSION {
                            "несовместимая версия: обновите клиент у обоих участников".to_string()
                        } else if reason == REASON_TIMEOUT {
                            "хост не подтвердил вызов".to_string()
                        } else {
                            "звонок отклонён хостом".to_string()
                        };
                        let _ = events.send(NetEvent::CallEnded(msg));
                        break 'outer;
                    }
                    _ => {
                        // мусор/повторы — идём дальше в ретрай
                        if attempt >= HANDSHAKE_MAX_ATTEMPTS {
                            let _ = events
                                .send(NetEvent::CallEnded(format!("хост недоступен: {target}")));
                            break 'outer;
                        }
                        tokio::time::sleep(RETRY_PAUSE).await;
                    }
                },
                None => {
                    if attempt >= HANDSHAKE_MAX_ATTEMPTS {
                        let _ =
                            events.send(NetEvent::CallEnded(format!("хост недоступен: {target}")));
                        break 'outer;
                    }
                    tokio::time::sleep(RETRY_PAUSE).await;
                }
            }
        }

        let Some((from, shared, prefixes, peer_name, _mode)) = established else {
            continue;
        };
        let shown_peer = if room.is_some() {
            format!("комната {} (ретранслятор)", room_to_string(&room.unwrap()))
        } else {
            from.to_string()
        };
        let _ = events.send(NetEvent::InCall {
            peer_name: peer_name.clone(),
            peer_addr: shown_peer,
        });

        // фаза 2: звонок
        dbg_net!(
            "join keys: shared[:6]={:02x?} prefixes[:6]={:02x?}",
            &shared[..6],
            &prefixes[..6]
        );
        let outcome = run_established(
            sock.clone(),
            Cipher::new(&shared, &prefixes, false),
            from,
            ssrc_rand(),
            events_tx.clone(),
            &mut cmd_rx,
            &mut media_rx,
            engine.clone(),
            None,
            &mut download_directory,
            &mut profile,
            &mut own_name,
        )
        .await;

        // фаза 3: reconnect только при потере сети (аналог configureConnectionRetries);
        // «нормальное» завершение звонка оставляет очередь — дозвона не будет.
        if matches!(outcome, Outcome::Lost) {
            tokio::time::sleep(RETRY_PAUSE).await;
            if killed.load(Ordering::Relaxed) {
                break 'outer;
            }
            let _ = events.send(NetEvent::Joining {
                attempt: 0,
                addr: target.clone(),
            });
            continue 'outer;
        }
        break 'outer;
    }
}
