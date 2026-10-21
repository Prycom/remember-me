//! Bounded, paced file transport over the call's authenticated UDP channel.
//! Disk I/O runs in a separate task. No file-sized allocations or audio work.
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

pub const CHUNK: usize = 1024;
const WINDOW: usize = 32;
const RETRY: Duration = Duration::from_millis(600);
const TIMEOUT: Duration = Duration::from_secs(30);
const OFFER_ACK: u32 = u32::MAX;
const FINISH_ACK: u32 = u32::MAX - 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilePacket {
    Offer { id: u64, name: String, size: u64 },
    Chunk { id: u64, index: u32, data: Vec<u8> },
    Finish { id: u64, hash: [u8; 32] },
    Ack { id: u64, index: u32 },
    Cancel { id: u64, reason: String },
}
impl FilePacket {
    pub fn id(&self) -> u64 {
        match self {
            Self::Offer { id, .. }
            | Self::Chunk { id, .. }
            | Self::Finish { id, .. }
            | Self::Ack { id, .. }
            | Self::Cancel { id, .. } => *id,
        }
    }
    pub fn encode(&self, out: &mut Vec<u8>) {
        let kind = match self {
            Self::Offer { .. } => 1,
            Self::Chunk { .. } => 2,
            Self::Finish { .. } => 3,
            Self::Ack { .. } => 4,
            Self::Cancel { .. } => 5,
        };
        out.push(kind);
        out.extend_from_slice(&self.id().to_be_bytes());
        match self {
            Self::Offer { name, size, .. } => {
                out.extend_from_slice(&size.to_be_bytes());
                out.extend_from_slice(name.as_bytes());
            }
            Self::Chunk { index, data, .. } => {
                out.extend_from_slice(&index.to_be_bytes());
                out.extend_from_slice(data);
            }
            Self::Finish { hash, .. } => out.extend_from_slice(hash),
            Self::Ack { index, .. } => out.extend_from_slice(&index.to_be_bytes()),
            Self::Cancel { reason, .. } => out.extend_from_slice(reason.as_bytes()),
        }
    }
    pub fn decode(b: &[u8]) -> Option<Self> {
        let id = u64::from_be_bytes(b.get(1..9)?.try_into().ok()?);
        let rest = b.get(9..)?;
        Some(match b[0] {
            1 if (9..=263).contains(&rest.len()) => Self::Offer {
                id,
                size: u64::from_be_bytes(rest[..8].try_into().ok()?),
                name: std::str::from_utf8(&rest[8..]).ok()?.to_owned(),
            },
            2 if (5..=4 + CHUNK).contains(&rest.len()) => Self::Chunk {
                id,
                index: u32::from_be_bytes(rest[..4].try_into().ok()?),
                data: rest[4..].to_vec(),
            },
            3 if rest.len() == 32 => Self::Finish {
                id,
                hash: rest.try_into().ok()?,
            },
            4 if rest.len() == 4 => Self::Ack {
                id,
                index: u32::from_be_bytes(rest.try_into().ok()?),
            },
            5 if rest.len() <= 255 => Self::Cancel {
                id,
                reason: std::str::from_utf8(rest).ok()?.to_owned(),
            },
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, Hash)]
pub enum TransferStatus {
    Waiting,
    Active,
    Complete,
    Failed(String),
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, Hash)]
pub struct TransferUpdate {
    pub id: u64,
    pub sending: bool,
    #[serde(default)]
    pub peer_id: Option<u64>,
    #[serde(default)]
    pub peer_name: Option<String>,
    pub name: String,
    pub size: u64,
    pub done: u64,
    pub status: TransferStatus,
    pub path: Option<PathBuf>,
}
impl TransferUpdate {
    pub fn active(&self) -> bool {
        matches!(
            self.status,
            TransferStatus::Waiting | TransferStatus::Active
        )
    }
    pub fn label(&self) -> String {
        let state = match &self.status {
            TransferStatus::Waiting => "ожидаем получателя".into(),
            TransferStatus::Active => format!(
                "{} {}% · {} / {}",
                if self.sending {
                    "отправка"
                } else {
                    "получение"
                },
                self.done.saturating_mul(100) / self.size.max(1),
                bytes(self.done),
                bytes(self.size)
            ),
            TransferStatus::Complete => format!(
                "{} · {}",
                if self.sending {
                    "отправлен"
                } else {
                    "сохранён"
                },
                bytes(self.size)
            ),
            TransferStatus::Failed(error) => format!("ошибка: {error}"),
        };
        format!(
            "📎 {} · {state}{}",
            self.name,
            self.peer_name
                .as_ref()
                .map(|n| format!(" · {n}"))
                .unwrap_or_default()
        )
    }
}
fn bytes(size: u64) -> String {
    if size >= 1_048_576 {
        format!("{:.1} МиБ", size as f64 / 1_048_576.0)
    } else if size >= 1024 {
        format!("{:.1} КиБ", size as f64 / 1024.0)
    } else {
        format!("{size} Б")
    }
}

pub enum Command {
    Send(PathBuf),
    Cancel(u64),
    Wire(FilePacket),
}
pub struct Worker {
    pub commands: mpsc::Sender<Command>,
    pub packets: mpsc::Receiver<FilePacket>,
    pub updates: mpsc::UnboundedReceiver<TransferUpdate>,
    task: tokio::task::JoinHandle<()>,
    directory: std::sync::Arc<std::sync::RwLock<PathBuf>>,
}
impl Worker {
    pub fn set_directory(&self, path: PathBuf) {
        *self.directory.write().unwrap() = path;
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub fn spawn(directory: PathBuf) -> Worker {
    let (commands, rx) = mpsc::channel(128);
    let (tx, packets) = mpsc::channel(64);
    let (events, updates) = mpsc::unbounded_channel();
    let shared_directory = std::sync::Arc::new(std::sync::RwLock::new(directory));
    let directory = shared_directory.clone();
    let task = tokio::spawn(async move {
        State {
            directory,
            outgoing: None,
            incoming: None,
            completed: VecDeque::new(),
            cancelled: VecDeque::new(),
            tx,
            events,
        }
        .run(rx)
        .await;
    });
    Worker {
        commands,
        packets,
        updates,
        task,
        directory: shared_directory,
    }
}

struct Pending {
    data: Vec<u8>,
    sent: Instant,
}
struct Outgoing {
    info: TransferUpdate,
    file: tokio::fs::File,
    hash: Sha256,
    accepted: bool,
    next: u32,
    pending: BTreeMap<u32, Pending>,
    last_control: Instant,
    last_progress: Instant,
    last_ui: Instant,
}
struct Incoming {
    info: TransferUpdate,
    file: Option<tokio::fs::File>,
    temp: PathBuf,
    hash: Sha256,
    next: u32,
    buffered: BTreeMap<u32, Vec<u8>>,
    last_progress: Instant,
    last_ui: Instant,
}
impl Drop for Incoming {
    fn drop(&mut self) {
        self.file.take();
        // Best effort on cancellation/session abort; crash remnants have .part
        // names and never replace or appear as a completed user file.
        let _ = std::fs::remove_file(&self.temp);
    }
}
struct State {
    directory: std::sync::Arc<std::sync::RwLock<PathBuf>>,
    outgoing: Option<Outgoing>,
    incoming: Option<Incoming>,
    completed: VecDeque<(u64, [u8; 32])>,
    cancelled: VecDeque<(u64, String)>,
    tx: mpsc::Sender<FilePacket>,
    events: mpsc::UnboundedSender<TransferUpdate>,
}
impl State {
    async fn run(mut self, mut rx: mpsc::Receiver<Command>) {
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                cmd = rx.recv() => match cmd {
                    Some(Command::Send(path)) => self.start_send(path).await,
                    Some(Command::Cancel(id)) => self.fail(id, "передача отменена".into(), true).await,
                    Some(Command::Wire(packet)) => {
                        let id = packet.id();
                        let offered=match &packet {FilePacket::Offer{name,size,..}=>Some((safe_name(name),*size)),_=>None};
                        if let Err(error) = self.receive(packet).await {
                            if self.incoming.is_none() && let Some((name,size))=offered {
                                let _=self.events.send(TransferUpdate{id,peer_id:None,peer_name:None,sending:false,name,size,done:0,status:TransferStatus::Failed(error.clone()),path:None});
                            }
                            self.fail(id,error,true).await;
                        }
                    }
                    None => return,
                },
                _ = tick.tick(), if self.outgoing.is_some() || self.incoming.is_some() => {
                    if let Some(id) = self.incoming.as_ref().filter(|f| f.last_progress.elapsed()>TIMEOUT).map(|f|f.info.id) {
                        self.fail(id,"получатель: 30 с без данных".into(),true).await;
                    }
                    if let Some(id) = self.outgoing.as_ref().map(|f|f.info.id)
                        && let Err(error) = self.send_tick().await { self.fail(id,error,true).await; }
                }
            }
        }
    }
    async fn start_send(&mut self, path: PathBuf) {
        static IDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let id = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64)
            .wrapping_add(IDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        let name = safe_name(&path.file_name().unwrap_or_default().to_string_lossy());
        let mut info = TransferUpdate {
            id,
            peer_id: None,
            peer_name: None,
            sending: true,
            name,
            size: 0,
            done: 0,
            status: TransferStatus::Waiting,
            path: Some(path.clone()),
        };
        let result = async {
            if self.outgoing.is_some() {
                return Err("дождитесь завершения текущей отправки".into());
            }
            let metadata = tokio::fs::metadata(&path)
                .await
                .map_err(|e| format!("файл: {e}"))?;
            if !metadata.is_file() {
                return Err("выберите обычный файл".into());
            }
            info.size = metadata.len();
            if info.size.div_ceil(CHUNK as u64) >= FINISH_ACK as u64 {
                return Err("слишком большой файл".into());
            }
            tokio::fs::File::open(&path)
                .await
                .map_err(|e| format!("открыть файл: {e}"))
        }
        .await;
        match result {
            Ok(file) => {
                let _ = self.events.send(info.clone());
                self.outgoing = Some(Outgoing {
                    info,
                    file,
                    hash: Sha256::new(),
                    accepted: false,
                    next: 0,
                    pending: BTreeMap::new(),
                    last_control: Instant::now() - RETRY,
                    last_progress: Instant::now(),
                    last_ui: Instant::now(),
                });
            }
            Err(error) => {
                info.status = TransferStatus::Failed(error);
                let _ = self.events.send(info);
            }
        }
    }
    async fn send_tick(&mut self) -> Result<(), String> {
        let Some(f) = self.outgoing.as_mut() else {
            return Ok(());
        };
        if f.last_progress.elapsed() > TIMEOUT {
            return Err(
                "30 с без подтверждений; обновите клиент собеседника и проверьте связь".into(),
            );
        }
        let id = f.info.id;
        if !f.accepted {
            if f.last_control.elapsed() >= RETRY {
                self.tx
                    .send(FilePacket::Offer {
                        id,
                        name: f.info.name.clone(),
                        size: f.info.size,
                    })
                    .await
                    .map_err(|_| "сессия закрыта")?;
                f.last_control = Instant::now();
            }
            return Ok(());
        }
        let count = f.info.size.div_ceil(CHUNK as u64) as u32;
        let mut budget = 4; // <= 200 KiB/s per direction, including retransmits
        for (&index, p) in &mut f.pending {
            if budget == 0 {
                break;
            }
            if p.sent.elapsed() >= RETRY {
                self.tx
                    .send(FilePacket::Chunk {
                        id,
                        index,
                        data: p.data.clone(),
                    })
                    .await
                    .map_err(|_| "сессия закрыта")?;
                p.sent = Instant::now();
                budget -= 1;
            }
        }
        while budget > 0 && f.next < count && f.pending.len() < WINDOW {
            let len = (f.info.size - f.next as u64 * CHUNK as u64).min(CHUNK as u64) as usize;
            let mut data = vec![0; len];
            f.file
                .read_exact(&mut data)
                .await
                .map_err(|e| format!("чтение файла: {e}"))?;
            f.hash.update(&data);
            self.tx
                .send(FilePacket::Chunk {
                    id,
                    index: f.next,
                    data: data.clone(),
                })
                .await
                .map_err(|_| "сессия закрыта")?;
            f.pending.insert(
                f.next,
                Pending {
                    data,
                    sent: Instant::now(),
                },
            );
            f.next += 1;
            budget -= 1;
        }
        if f.next == count && f.pending.is_empty() && f.last_control.elapsed() >= RETRY {
            self.tx
                .send(FilePacket::Finish {
                    id,
                    hash: f.hash.clone().finalize().into(),
                })
                .await
                .map_err(|_| "сессия закрыта")?;
            f.last_control = Instant::now();
        }
        Ok(())
    }
    async fn receive(&mut self, p: FilePacket) -> Result<(), String> {
        let id = p.id();
        if !matches!(p, FilePacket::Cancel { .. } | FilePacket::Ack { .. })
            && let Some((_, reason)) = self.cancelled.iter().find(|(old, _)| *old == id)
        {
            let _ = self
                .tx
                .send(FilePacket::Cancel {
                    id,
                    reason: reason.clone(),
                })
                .await;
            return Ok(());
        }
        match p {
            FilePacket::Offer { name, size, .. } => {
                if self.completed.iter().any(|(old, _)| *old == id) {
                    return Ok(());
                }
                if let Some(f) = self.incoming.as_ref() {
                    if f.info.id != id {
                        return self
                            .tx
                            .send(FilePacket::Cancel {
                                id,
                                reason: "получатель уже принимает другой файл".into(),
                            })
                            .await
                            .map_err(|_| "сессия закрыта".into());
                    }
                    if f.info.size != size || f.info.name != safe_name(&name) {
                        return Err("параметры файла изменились".into());
                    }
                } else {
                    if size.div_ceil(CHUNK as u64) >= FINISH_ACK as u64 {
                        return Err("слишком большой файл".into());
                    }
                    let directory = self.directory.read().unwrap().clone();
                    tokio::fs::create_dir_all(&directory)
                        .await
                        .map_err(|e| format!("папка загрузок: {e}"))?;
                    let temp = directory.join(format!(".remember-me-{id:016x}.part"));
                    let file = tokio::fs::OpenOptions::new()
                        .create_new(true)
                        .write(true)
                        .open(&temp)
                        .await
                        .map_err(|e| format!("создать файл: {e}"))?;
                    let info = TransferUpdate {
                        id,
                        peer_id: None,
                        peer_name: None,
                        sending: false,
                        name: safe_name(&name),
                        size,
                        done: 0,
                        status: TransferStatus::Active,
                        path: None,
                    };
                    let _ = self.events.send(info.clone());
                    self.incoming = Some(Incoming {
                        info,
                        file: Some(file),
                        temp,
                        hash: Sha256::new(),
                        next: 0,
                        buffered: BTreeMap::new(),
                        last_progress: Instant::now(),
                        last_ui: Instant::now(),
                    });
                }
                let _ = self
                    .tx
                    .send(FilePacket::Ack {
                        id,
                        index: OFFER_ACK,
                    })
                    .await;
            }
            FilePacket::Chunk { index, data, .. } => {
                let Some(f) = self.incoming.as_mut().filter(|f| f.info.id == id) else {
                    return Ok(());
                };
                let offset = index as u64 * CHUNK as u64;
                if offset >= f.info.size
                    || data.len() != (f.info.size - offset).min(CHUNK as u64) as usize
                {
                    return Err("некорректный размер блока".into());
                }
                if index >= f.next && index - f.next >= WINDOW as u32 {
                    return Ok(());
                }
                if index >= f.next {
                    f.buffered.entry(index).or_insert(data);
                }
                while let Some(data) = f.buffered.remove(&f.next) {
                    f.file
                        .as_mut()
                        .unwrap()
                        .write_all(&data)
                        .await
                        .map_err(|e| format!("запись файла: {e}"))?;
                    f.hash.update(&data);
                    f.info.done += data.len() as u64;
                    f.next += 1;
                    f.last_progress = Instant::now();
                }
                let _ = self.tx.send(FilePacket::Ack { id, index }).await;
                if f.last_ui.elapsed() > Duration::from_millis(200) {
                    let _ = self.events.send(f.info.clone());
                    f.last_ui = Instant::now();
                }
            }
            FilePacket::Finish { hash, .. } => {
                if self
                    .completed
                    .iter()
                    .any(|(old, h)| *old == id && *h == hash)
                {
                    let _ = self
                        .tx
                        .send(FilePacket::Ack {
                            id,
                            index: FINISH_ACK,
                        })
                        .await;
                    return Ok(());
                }
                let Some(f) = self.incoming.as_mut().filter(|f| f.info.id == id) else {
                    return Ok(());
                };
                if f.info.done != f.info.size {
                    return Ok(());
                }
                if <[u8; 32]>::from(f.hash.clone().finalize()) != hash {
                    return Err("SHA-256 не совпал".into());
                }
                let mut file = f.file.take().unwrap();
                file.flush().await.map_err(|e| format!("сохранение: {e}"))?;
                file.sync_all()
                    .await
                    .map_err(|e| format!("сохранение: {e}"))?;
                drop(file);
                let path = publish(&f.temp, &f.info.name)
                    .await
                    .map_err(|e| format!("сохранение: {e}"))?;
                f.info.path = Some(path);
                f.info.status = TransferStatus::Complete;
                let _ = self.events.send(f.info.clone());
                self.completed.push_back((id, hash));
                while self.completed.len() > 16 {
                    self.completed.pop_front();
                }
                self.incoming = None;
                let _ = self
                    .tx
                    .send(FilePacket::Ack {
                        id,
                        index: FINISH_ACK,
                    })
                    .await;
            }
            FilePacket::Ack { index, .. } => {
                let Some(f) = self.outgoing.as_mut().filter(|f| f.info.id == id) else {
                    return Ok(());
                };
                if index == OFFER_ACK {
                    if !f.accepted {
                        f.accepted = true;
                        f.info.status = TransferStatus::Active;
                        f.last_progress = Instant::now();
                    }
                } else if index == FINISH_ACK
                    && f.accepted
                    && f.next as u64 == f.info.size.div_ceil(CHUNK as u64)
                    && f.pending.is_empty()
                {
                    f.info.done = f.info.size;
                    f.info.status = TransferStatus::Complete;
                    let _ = self.events.send(f.info.clone());
                    self.outgoing = None;
                    return Ok(());
                } else if let Some(p) = f.pending.remove(&index) {
                    f.info.done += p.data.len() as u64;
                    f.last_progress = Instant::now();
                }
                if f.last_ui.elapsed() > Duration::from_millis(200) {
                    let _ = self.events.send(f.info.clone());
                    f.last_ui = Instant::now();
                }
            }
            FilePacket::Cancel { reason, .. } => {
                self.fail(id, format!("собеседник: {reason}"), false).await
            }
        }
        Ok(())
    }
    async fn fail(&mut self, id: u64, error: String, notify: bool) {
        if !self.cancelled.iter().any(|(old, _)| *old == id) {
            self.cancelled.push_back((
                id,
                error
                    .chars()
                    .scan(0, |n, c| {
                        *n += c.len_utf8();
                        (*n <= 255).then_some(c)
                    })
                    .collect(),
            ));
        }
        while self.cancelled.len() > 32 {
            self.cancelled.pop_front();
        }
        for info in [
            self.outgoing
                .as_ref()
                .filter(|f| f.info.id == id)
                .map(|f| f.info.clone()),
            self.incoming
                .as_ref()
                .filter(|f| f.info.id == id)
                .map(|f| f.info.clone()),
        ]
        .into_iter()
        .flatten()
        {
            let mut info = info;
            info.status = TransferStatus::Failed(error.clone());
            let _ = self.events.send(info);
        }
        if self.outgoing.as_ref().is_some_and(|f| f.info.id == id) {
            self.outgoing = None;
        }
        if self.incoming.as_ref().is_some_and(|f| f.info.id == id) {
            self.incoming = None;
        }
        if notify {
            let reason = error
                .chars()
                .scan(0, |n, c| {
                    *n += c.len_utf8();
                    (*n <= 255).then_some(c)
                })
                .collect();
            let _ = self.tx.send(FilePacket::Cancel { id, reason }).await;
        }
    }
}

pub fn safe_name(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_control() || "/\\<>:\"|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .scan(0, |n, c| {
            *n += c.len_utf8();
            (*n <= 180).then_some(c)
        })
        .collect();
    out = out.trim_matches([' ', '.']).to_owned();
    if out.is_empty() {
        out = "file".into();
    }
    let stem = out.split('.').next().unwrap_or("").to_ascii_uppercase();
    if [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ]
    .contains(&stem.as_str())
    {
        out.insert(0, '_');
    }
    out
}
async fn publish(temp: &Path, name: &str) -> std::io::Result<PathBuf> {
    let directory = temp.parent().unwrap();
    let name = Path::new(name);
    let stem = name.file_stem().unwrap_or_default().to_string_lossy();
    let extension = name
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for n in 0..10_000 {
        let path = directory.join(if n == 0 {
            name.to_string_lossy().into_owned()
        } else {
            format!("{stem} ({n}){extension}")
        });
        match tokio::fs::hard_link(temp, &path).await {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "слишком много файлов с таким именем",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{Cipher, Packet};
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "remember-me-files-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn decrypt(receiver: &Cipher, wire: &[u8], counter: &mut u64) -> Option<FilePacket> {
        assert!(wire.len() < 1200, "file packet would fragment");
        match Packet::decrypt_wire(receiver, wire, counter, &mut 0) {
            Some(Packet::File(p)) => Some(p),
            None => None,
            _ => panic!("wrong packet"),
        }
    }

    #[tokio::test]
    async fn encrypted_transfer_recovers_loss_reordering_and_lost_completion_without_overwriting() {
        let dir = Directory::new();
        let source = dir.0.join("данные с пробелами.bin");
        let data: Vec<_> = (0..110 * CHUNK + 57)
            .map(|n| (n * 71 + n / 1009) as u8)
            .collect();
        tokio::fs::write(&source, &data).await.unwrap();
        let downloads = dir.0.join("downloads");
        tokio::fs::create_dir(&downloads).await.unwrap();
        tokio::fs::write(
            downloads.join(source.file_name().unwrap()),
            b"existing file",
        )
        .await
        .unwrap();
        let mut a = spawn(dir.0.join("a"));
        let mut b = spawn(downloads.clone());
        a.commands
            .send(Command::Send(source.clone()))
            .await
            .unwrap();
        let mut ca = Cipher::new(&[7; 32], &[8; 32], true);
        let mut cb = Cipher::new(&[7; 32], &[8; 32], false);
        let (mut counter_a, mut counter_b) = (0, 0);
        let (mut lost_offer, mut lost_chunk, mut lost_ack, mut lost_finish) =
            (false, false, false, false);
        let mut reordered = None;
        let mut did_reorder = false;
        let mut received = None;
        let mut sender_done = false;
        let mut completed_events = 0;
        let deadline = tokio::time::sleep(Duration::from_secs(15));
        tokio::pin!(deadline);
        while !sender_done {
            tokio::select! {
                Some(p)=a.packets.recv()=>{
                    let wire=Packet::encrypt_wire(&mut ca,&Packet::File(p.clone()));
                    if matches!(p,FilePacket::Offer{..})&&!lost_offer{lost_offer=true;continue;}
                    if matches!(p,FilePacket::Chunk{index:4,..})&&!lost_chunk{lost_chunk=true;continue;}
                    if matches!(p,FilePacket::Chunk{index:2,..})&&!did_reorder {reordered=Some(wire);did_reorder=true;continue;}
                    if let Some(p)=decrypt(&cb,&wire,&mut counter_b){b.commands.send(Command::Wire(p)).await.unwrap();}
                    if let Some(old)=reordered.take(){
                        // Higher control counter already arrived: replay filtering
                        // drops the reordered packet, the transfer must retry it.
                        assert!(decrypt(&cb,&old,&mut counter_b).is_none());
                    }
                }
                Some(p)=b.packets.recv()=>{
                    let wire=Packet::encrypt_wire(&mut cb,&Packet::File(p.clone()));
                    if matches!(p,FilePacket::Ack{index:6,..})&&!lost_ack{lost_ack=true;continue;}
                    if matches!(p,FilePacket::Ack{index:FINISH_ACK,..})&&!lost_finish{lost_finish=true;continue;}
                    if let Some(p)=decrypt(&ca,&wire,&mut counter_a){a.commands.send(Command::Wire(p)).await.unwrap();}
                }
                Some(update)=a.updates.recv()=>{
                    assert!(!matches!(update.status,TransferStatus::Failed(_)),"{update:?}");
                    if update.status==TransferStatus::Complete{sender_done=true;assert_eq!(update.done,data.len() as u64);}
                }
                Some(update)=b.updates.recv()=>{
                    assert!(!matches!(update.status,TransferStatus::Failed(_)),"{update:?}");
                    if update.status==TransferStatus::Complete{received=update.path;completed_events+=1;}
                }
                _=&mut deadline=>panic!("file transfer stalled"),
            }
        }
        assert!(lost_offer && lost_chunk && lost_ack && lost_finish && did_reorder);
        assert_eq!(completed_events, 1);
        let received = received.unwrap();
        assert_eq!(received.file_name().unwrap(), "данные с пробелами (1).bin");
        assert_eq!(tokio::fs::read(received).await.unwrap(), data);
        assert_eq!(
            tokio::fs::read(downloads.join(source.file_name().unwrap()))
                .await
                .unwrap(),
            b"existing file"
        );
        assert_eq!(
            std::fs::read_dir(downloads).unwrap().count(),
            2,
            "partial file leaked"
        );
    }

    #[tokio::test]
    async fn wrong_hash_and_cancelled_session_never_publish_a_file() {
        let dir = Directory::new();
        let mut worker = spawn(dir.0.clone());
        worker
            .commands
            .send(Command::Wire(FilePacket::Offer {
                id: 1,
                name: "../../CON.txt".into(),
                size: 3,
            }))
            .await
            .unwrap();
        worker.updates.recv().await.unwrap();
        worker
            .commands
            .send(Command::Wire(FilePacket::Chunk {
                id: 1,
                index: 0,
                data: b"abc".to_vec(),
            }))
            .await
            .unwrap();
        worker
            .commands
            .send(Command::Wire(FilePacket::Finish {
                id: 1,
                hash: [0; 32],
            }))
            .await
            .unwrap();
        let failure = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let update = worker.updates.recv().await.unwrap();
                if matches!(update.status, TransferStatus::Failed(_)) {
                    break update;
                }
            }
        })
        .await
        .unwrap();
        assert!(matches!(failure.status,TransferStatus::Failed(ref e) if e.contains("SHA-256")));
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 0);
        worker
            .commands
            .send(Command::Wire(FilePacket::Offer {
                id: 2,
                name: "unfinished.txt".into(),
                size: CHUNK as u64,
            }))
            .await
            .unwrap();
        worker.updates.recv().await.unwrap();
        drop(worker);
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(
            std::fs::read_dir(&dir.0).unwrap().count(),
            0,
            "session cancellation leaked .part"
        );
    }

    #[tokio::test]
    async fn cancellation_stays_cancelled_when_a_late_offer_is_retried() {
        let dir = Directory::new();
        let mut worker = spawn(dir.0.clone());
        let offer = FilePacket::Offer {
            id: 9,
            name: "cancelled.bin".into(),
            size: 1024,
        };
        worker
            .commands
            .send(Command::Wire(offer.clone()))
            .await
            .unwrap();
        assert_eq!(
            worker.updates.recv().await.unwrap().status,
            TransferStatus::Active
        );
        worker.commands.send(Command::Cancel(9)).await.unwrap();
        assert!(matches!(
            worker.updates.recv().await.unwrap().status,
            TransferStatus::Failed(_)
        ));
        worker.commands.send(Command::Wire(offer)).await.unwrap();
        let mut cancels = 0;
        tokio::time::timeout(Duration::from_secs(2), async {
            while cancels < 2 {
                if matches!(
                    worker.packets.recv().await.unwrap(),
                    FilePacket::Cancel { id: 9, .. }
                ) {
                    cancels += 1;
                }
            }
        })
        .await
        .unwrap();
        assert!(
            worker.updates.try_recv().is_err(),
            "cancelled offer must not restart"
        );
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn empty_file_is_verified_and_directory_change_applies_to_next_file() {
        let dir = Directory::new();
        let new = dir.0.join("chosen downloads");
        let mut worker = spawn(dir.0.join("old"));
        worker.set_directory(new.clone());
        worker
            .commands
            .send(Command::Wire(FilePacket::Offer {
                id: 1,
                name: "empty.txt".into(),
                size: 0,
            }))
            .await
            .unwrap();
        worker
            .commands
            .send(Command::Wire(FilePacket::Finish {
                id: 1,
                hash: Sha256::digest([]).into(),
            }))
            .await
            .unwrap();
        let update = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let u = worker.updates.recv().await.unwrap();
                if !u.active() {
                    break u;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(update.status, TransferStatus::Complete);
        assert_eq!(update.path, Some(new.join("empty.txt")));
        assert_eq!(std::fs::read(new.join("empty.txt")).unwrap(), b"");
    }

    #[test]
    fn filenames_cannot_escape_downloads_or_create_windows_device_paths() {
        for bad in [
            "../../evil",
            "C:\\Windows\\file.txt",
            "a\u{1b}[0m\n.txt",
            "..",
            "CON.txt",
            "NUL",
            ".hidden/",
        ] {
            let name = safe_name(bad);
            assert!(!name.contains(['/', '\\', ':', '\u{1b}', '\n']));
            assert_ne!(name, "..");
            assert_ne!(name, "CON.txt");
            assert_ne!(name, "NUL");
            assert!(name.len() <= 181);
        }
        let p = FilePacket::Chunk {
            id: 7,
            index: 2,
            data: vec![42; CHUNK],
        };
        let mut bytes = Vec::new();
        p.encode(&mut bytes);
        assert_eq!(FilePacket::decode(&bytes), Some(p));
        for len in 0..13 {
            assert!(FilePacket::decode(&vec![0; len]).is_none());
        }
        let mut bytes = vec![2];
        bytes.extend_from_slice(&7u64.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend(vec![0; CHUNK + 1]);
        assert!(FilePacket::decode(&bytes).is_none());
    }
}

/// A bounded shared output stream avoids polling every peer's file worker.
pub enum RoutedOutput {
    Packet(FilePacket),
    Update(TransferUpdate),
}
pub struct RoutedWorker {
    pub commands: mpsc::Sender<Command>,
    directory: std::sync::Arc<std::sync::RwLock<PathBuf>>,
    task: tokio::task::JoinHandle<()>,
}
impl RoutedWorker {
    pub fn set_directory(&self, path: PathBuf) {
        *self.directory.write().unwrap() = path;
    }
}
impl Drop for RoutedWorker {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub fn spawn_routed(
    directory: PathBuf,
    id: u64,
    output: mpsc::Sender<(u64, RoutedOutput)>,
) -> RoutedWorker {
    let mut worker = spawn(directory);
    let commands = worker.commands.clone();
    let directory = worker.directory.clone();
    let task = tokio::spawn(async move {
        loop {
            let message = tokio::select! {
                Some(packet)=worker.packets.recv()=>RoutedOutput::Packet(packet),
                Some(update)=worker.updates.recv()=>RoutedOutput::Update(update),
                else=>break,
            };
            if output.send((id, message)).await.is_err() {
                break;
            }
        }
    });
    RoutedWorker {
        commands,
        directory,
        task,
    }
}
