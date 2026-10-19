// Протокол "remember-me": бинарный формат пакетов поверх UDP.
//
// Открытые пакеты (signaling, как у Discord — Voice Gateway → SelectProtocol):
//   HELLO, INCOMING, ACCEPT, DECLINE.
// Все остальные пакеты шифрованы. Ключ вырабатывается по X25519 (ВДП обмена
// в HELLO/ACCEPT) + SHA-256(shared || prefixes) — аналог транспортного
// шифрования Discord (их режим aead_xchacha20_poly1305_rtpsize / libsodium
// crypto_aead_xchacha20poly1305_ietf).
//
// Формат защищённых wire-пакетов:
//   wire = clear_header(9) || ct(tag16 || body)
//   clear_header = [op][ssrc u32BE][seq u32BE]              (OP_AUDIO)
//   clear_header = [op][counter u64BE]                      (остальные)
//   nonce = prefix(16) || counter(8 BE), aad = clear_header
//   заголовок op/ssrc/seq остаётся открытым, как RTP header в Discord.
//   Ответные пакеты — строго нарастающие counter'ы: replay/дубли отбрасываются.

use chacha20poly1305::aead::{Aead, Nonce, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305};
use sha2::{Digest, Sha256};

pub const OP_HELLO: u8 = 1;
pub const OP_INCOMING: u8 = 2;
pub const OP_ACCEPT: u8 = 3;
pub const OP_DECLINE: u8 = 4;
pub const OP_PING: u8 = 5;
pub const OP_PONG: u8 = 6;
pub const OP_AUDIO: u8 = 7;
pub const OP_TEXT: u8 = 8;
pub const OP_ACK: u8 = 9;
pub const OP_STATUS: u8 = 10; // mute_mic / deafen (для отображения у собеседника)
pub const OP_BYE: u8 = 11;
pub use remember_me::server::OP_RELAY_REG;

/// Крипто-режим (список по аналогии с getEncryptionModes у Discord)
pub const MODE_XCHACHA20POLY1305: u8 = 0;

pub const NAME_BYTES: usize = 32;
pub const TEXT_MAX: usize = 1200;
pub const AUDIO_PAYLOAD_MAX: usize = 400;

pub const REASON_DECLINED: u8 = 0;
pub const REASON_BUSY: u8 = 1;
pub const REASON_TIMEOUT: u8 = 2;
pub const REASON_HANGUP: u8 = 0;

#[derive(Debug, Clone)]
pub struct Hello {
    pub name: String,
    /// публичный X25519 ключ (32B)
    pub pub_key: [u8; 32],
    pub modes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub enum Packet {
    Hello(Hello),
    /// входящий звонок (host -> joiner), joiner ждёт решения
    Incoming { modes: Vec<u8> },
    Accept {
        mode: u8,
        host_pub: [u8; 32],
        /// 32 случайных байта: prefix_host(16) || prefix_joiner(16)
        prefixes: [u8; 32],
        ssrc_joiner: u32,
        ssrc_host: u32,
        /// имя хоста (до name_to_bytes максимум)
        name: String,
    },
    Decline { reason: u8 },
    Ping { msg_id: u64, ts_ms: u64 },
    Pong { msg_id: u64, ts_ms: u64 },
    /// готовый opus-кадр (payload < AUDIO_PAYLOAD_MAX)
    Audio { ssrc: u32, seq: u32, payload: Vec<u8> },
    Text { msg_id: u32, text: String },
    Ack { msg_id: u32 },
    Status { mute_mic: bool, deaf: bool, lvl: u8 },
    Bye { reason: u8 },
    /// регистрация комнаты на ретрансляторе (`remember-me --server`)
    RelayReg { room: [u8; 4] },
}

/// Класс пакетов по первому байту — нужна ли расшифровка
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireClass {
    Open,
    Secure,
    Unknown,
}

pub fn classify(b: &[u8]) -> WireClass {
    match b.first().copied() {
        Some(OP_HELLO) | Some(OP_INCOMING) | Some(OP_ACCEPT) | Some(OP_DECLINE)
        | Some(OP_RELAY_REG) => WireClass::Open,
        Some(OP_PING) | Some(OP_PONG) | Some(OP_AUDIO) | Some(OP_TEXT) | Some(OP_ACK)
        | Some(OP_STATUS) | Some(OP_BYE) => WireClass::Secure,
        Some(_) => WireClass::Unknown,
        None => WireClass::Unknown,
    }
}

pub fn name_to_bytes(name: &str) -> [u8; NAME_BYTES] {
    let mut out = [0u8; NAME_BYTES];
    let mut copy = String::new();
    for c in name.chars() {
        if copy.len() + c.len_utf8() > NAME_BYTES { break; }
        copy.push(c);
    }
    out[..copy.len()].copy_from_slice(copy.as_bytes());
    out
}

pub fn name_from_bytes(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

fn modes_to_wire(modes: &[u8], buf: &mut Vec<u8>) {
    buf.push(modes.len().min(4) as u8);
    for m in modes.iter().take(4) { buf.push(*m); }
}

fn parse_modes(off: usize, b: &[u8]) -> Option<Vec<u8>> {
    let n = *b.get(off)?;
    let rest = b.get(off + 1..off + 1 + n as usize)?;
    Some(rest.to_vec())
}

/// Сессионный шифратор/дешифратор одной стороны.
/// prefix_tx — наш «выходной» 16-байтовый nonce-префикс,
/// prefix_rx — встречный (используется при расшифровке приходящих пакетов).
pub struct Cipher {
    aead: XChaCha20Poly1305,
    prefix_tx: [u8; 16],
    prefix_rx: [u8; 16],
    ctl_tx_counter: u64,
}

impl Cipher {
    /// Шифрование пакета. Для управляющих пакетов counter выдаёт сам Cipher
    /// (строго монотонный), для AUDIO используется seq.
    pub fn new(shared: &[u8; 32], prefixes: &[u8; 32], host_is_self: bool) -> Cipher {
        // если мы хост — наши исходящие prefix = prefixes[0..16]; иначе [16..32]
        let (tx, rx) = if host_is_self {
            (&prefixes[0..16], &prefixes[16..32])
        } else {
            (&prefixes[16..32], &prefixes[0..16])
        };
        let mut salt = [0u8; 64];
        salt[..32].copy_from_slice(shared);
        salt[32..].copy_from_slice(prefixes);
        let digest = Sha256::digest(salt);
        let key: chacha20poly1305::Key = digest.into(); // 32 байта ключа
        Cipher {
            aead: XChaCha20Poly1305::new(&key),
            prefix_tx: tx.try_into().unwrap(),
            prefix_rx: rx.try_into().unwrap(),
            ctl_tx_counter: 1,
        }
    }

    fn nonce(prefix: &[u8; 16], counter: u64) -> Nonce<XChaCha20Poly1305> {
        let mut n = [0u8; 24];
        n[..16].copy_from_slice(prefix);
        n[16..].copy_from_slice(&counter.to_be_bytes());
        Nonce::<XChaCha20Poly1305>::try_from(&n[..]).expect("_nonce 24")
    }

    fn encrypt_with(&self, counter: u64, clear: &[u8], pt: &[u8]) -> Vec<u8> {
        let nonce = Self::nonce(&self.prefix_tx, counter);
        let ct = self
            .aead
            .encrypt(&nonce, Payload { msg: pt, aad: clear })
            .expect("xchacha20poly1305 шифрование");
        let mut out = Vec::with_capacity(clear.len() + ct.len());
        out.extend_from_slice(clear);
        out.extend_from_slice(&ct);
        out
    }

    /// Монотонный счётчик для управляющих пакетов.
    pub fn next_ctl_counter(&mut self) -> u64 {
        self.ctl_tx_counter += 1;
        self.ctl_tx_counter
    }

    pub fn encrypt_audio(&self, ssrc: u32, seq: u32, payload: &[u8]) -> Vec<u8> {
        let mut clear = Vec::with_capacity(9);
        clear.push(OP_AUDIO);
        clear.extend_from_slice(&ssrc.to_be_bytes());
        clear.extend_from_slice(&seq.to_be_bytes());
        let mut pt = Vec::with_capacity(payload.len() + 1);
        // тело шифруем, заголовок остаётся открытым (как RTP header в Discord)
        pt.push(OP_AUDIO);
        pt.extend_from_slice(payload);
        self.encrypt_with(seq as u64, &clear, &pt)
    }

    pub fn encrypt_control(&mut self, pkt: &Packet) -> Vec<u8> {
        let counter = self.next_ctl_counter();
        let mut clear = Vec::with_capacity(9);
        clear.push(op_of(pkt));
        clear.extend_from_slice(&counter.to_be_bytes());
        let mut pt = Vec::with_capacity(64);
        match pkt {
            Packet::Ping { msg_id, ts_ms } => {
                pt.push(OP_PING);
                pt.extend_from_slice(&msg_id.to_be_bytes());
                pt.extend_from_slice(&ts_ms.to_be_bytes());
            }
            Packet::Pong { msg_id, ts_ms } => {
                pt.push(OP_PONG);
                pt.extend_from_slice(&msg_id.to_be_bytes());
                pt.extend_from_slice(&ts_ms.to_be_bytes());
            }
            Packet::Text { msg_id, text } => {
                pt.push(OP_TEXT);
                pt.extend_from_slice(&msg_id.to_be_bytes());
                let len = text.len().min(TEXT_MAX) as u16;
                pt.extend_from_slice(&len.to_be_bytes());
                let b: &[u8] = &text.as_bytes()[..len as usize];
                pt.extend_from_slice(b);
            }
            Packet::Ack { msg_id } => {
                pt.push(OP_ACK);
                pt.extend_from_slice(&msg_id.to_be_bytes());
            }
            Packet::Status { mute_mic, deaf, lvl } => {
                pt.push(OP_STATUS);
                pt.push((*mute_mic as u8) | ((*deaf as u8) << 1) | ((lvl / 2) << 2));
            }
            Packet::Bye { reason } => {
                pt.push(OP_BYE);
                pt.push(*reason);
            }
            _ => {}
        }
        self.encrypt_with(counter, &clear, &pt)
    }
}

fn op_of(p: &Packet) -> u8 {
    match p {
        Packet::Ping { .. } => OP_PING,
        Packet::Pong { .. } => OP_PONG,
        Packet::Text { .. } => OP_TEXT,
        Packet::Ack { .. } => OP_ACK,
        Packet::Status { .. } => OP_STATUS,
        Packet::Bye { .. } => OP_BYE,
        _ => 0,
    }
}

impl Packet {
    /// Сериализация ОТКРЫТЫХ пакетов (Hello/Incoming/Accept/Decline)
    pub fn encode_open(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(96);
        match self {
            Packet::Hello(h) => {
                b.push(OP_HELLO);
                b.extend_from_slice(&name_to_bytes(&h.name));
                b.extend_from_slice(&h.pub_key);
                modes_to_wire(&h.modes, &mut b);
            }
            Packet::Incoming { modes } => {
                b.push(OP_INCOMING);
                modes_to_wire(modes, &mut b);
            }
            Packet::Accept { mode, host_pub, prefixes, ssrc_joiner, ssrc_host, name } => {
                b.push(OP_ACCEPT);
                b.push(*mode);
                b.extend_from_slice(host_pub);
                b.extend_from_slice(prefixes);
                b.extend_from_slice(&ssrc_joiner.to_be_bytes());
                b.extend_from_slice(&ssrc_host.to_be_bytes());
                b.extend_from_slice(&name_to_bytes(name));
            }
            Packet::Decline { reason } => {
                b.push(OP_DECLINE);
                b.push(*reason);
            }
            Packet::RelayReg { room } => {
                b.push(OP_RELAY_REG);
                b.extend_from_slice(room);
            }
            _ => {}
        }
        b
    }

    pub fn parse_open(b: &[u8]) -> Option<Packet> {
        if b.is_empty() { return None; }
        match b[0] {
            OP_HELLO => {
                if b.len() < 1 + NAME_BYTES + 32 + 1 { return None; }
                let name = name_from_bytes(&b[1..1 + NAME_BYTES]);
                let mut key = [0u8; 32];
                key.copy_from_slice(&b[33..65]);
                let modes = parse_modes(65, b)?;
                Some(Packet::Hello(Hello { name, pub_key: key, modes }))
            }
            OP_INCOMING => {
                let modes = parse_modes(1, b)?;
                Some(Packet::Incoming { modes })
            }
            OP_ACCEPT => {
                if b.len() < 1 + 1 + 32 + 32 + 4 + 4 + NAME_BYTES { return None; }
                let mut host_pub = [0u8; 32];
                host_pub.copy_from_slice(&b[2..34]);
                let mut prefixes = [0u8; 32];
                prefixes.copy_from_slice(&b[34..66]);
                let mut a = [0u8; 4]; a.copy_from_slice(&b[66..70]);
                let mut c = [0u8; 4]; c.copy_from_slice(&b[70..74]);
                Some(Packet::Accept {
                    mode: b[1],
                    host_pub,
                    prefixes,
                    ssrc_joiner: u32::from_be_bytes(a),
                    ssrc_host: u32::from_be_bytes(c),
                    name: name_from_bytes(&b[74..74 + NAME_BYTES]),
                })
            }
            OP_DECLINE => Some(Packet::Decline { reason: *b.get(1).unwrap_or(&0) }),
            OP_RELAY_REG => {
                if b.len() < 5 { return None; }
                let mut room = [0u8; 4];
                room.copy_from_slice(&b[1..5]);
                Some(Packet::RelayReg { room })
            }
            _ => None,
        }
    }

    /// Зашифрованная отправка: готовый wire bytes (использует Cipher).
    pub fn encrypt_wire(c: &mut Cipher, p: &Packet) -> Vec<u8> {
        match p {
            Packet::Audio { ssrc, seq, payload } => c.encrypt_audio(*ssrc, *seq, payload),
            _ => c.encrypt_control(p),
        }
    }

    /// Парсинг зашифрованного пакета. Управление: counter строго нарастающий
    /// (внутри себя), для AUDIO — seq без дублей.
    pub fn decrypt_wire(c: &Cipher, wire: &[u8], last_ctl: &mut u64, last_aud_seq: &mut u32) -> Option<Packet> {
        if wire.len() < 9 + 16 { return None; }
        let clear = &wire[..9];
        let ct = &wire[9..];
        match clear[0] {
            OP_AUDIO => {
                let mut s = [0u8; 4]; s.copy_from_slice(&clear[5..9]);
                let seq = u32::from_be_bytes(s);
                // защита от дублей/replay (wrap-safe): новый seq должен «двигаться вперёд»
                let forward = {
                    let diff = u32::wrapping_sub(seq, *last_aud_seq);
                    diff != 0 && diff < 0x4000_0000
                };
                if !forward { return None; }
                let pt = Cipher::decrypt_borrow(c, seq as u64, clear, ct)?;
                if pt.len() < 2 || pt[0] != OP_AUDIO { return None; }
                *last_aud_seq = seq;
                let mut h = [0u8; 4]; h.copy_from_slice(&clear[1..5]);
                Some(Packet::Audio {
                    ssrc: u32::from_be_bytes(h),
                    seq,
                    payload: pt[1..].to_vec(),
                })
            }
            OP_PING | OP_PONG | OP_TEXT | OP_ACK | OP_STATUS | OP_BYE => {
                let mut b = [0u8; 8]; b.copy_from_slice(&clear[1..9]);
                let counter = u64::from_be_bytes(b);
                if counter <= *last_ctl { return None; }
                let pt = Cipher::decrypt_borrow(c, counter, clear, ct)?;
                if pt.len() < 1 { return None; }
                let (op, body) = (pt[0], &pt[1..]);
                if op != clear[0] { return None; }
                *last_ctl = counter;
                match op {
                    OP_PING => {
                        if body.len() < 16 { return None; }
                        Some(Packet::Ping {
                            msg_id: u64::from_be_bytes(body[0..8].try_into().unwrap()),
                            ts_ms: u64::from_be_bytes(body[8..16].try_into().unwrap()),
                        })
                    }
                    OP_PONG => {
                        if body.len() < 16 { return None; }
                        Some(Packet::Pong {
                            msg_id: u64::from_be_bytes(body[0..8].try_into().unwrap()),
                            ts_ms: u64::from_be_bytes(body[8..16].try_into().unwrap()),
                        })
                    }
                    OP_TEXT => {
                        if body.len() < 6 { return None; }
                        let n = u16::from_be_bytes(body[4..6].try_into().unwrap()) as usize;
                        if body.len() < 6 + n { return None; }
                        Some(Packet::Text {
                            msg_id: u32::from_be_bytes(body[0..4].try_into().unwrap()),
                            text: String::from_utf8_lossy(&body[6..6 + n]).into_owned(),
                        })
                    }
                    OP_ACK => {
                        if body.len() < 4 { return None; }
                        Some(Packet::Ack { msg_id: u32::from_be_bytes(body[0..4].try_into().unwrap()) })
                    }
                    OP_STATUS => {
                        let f = body.first().copied().unwrap_or(0);
                        Some(Packet::Status {
                            mute_mic: f & 1 != 0,
                            deaf: f & 2 != 0,
                            lvl: (f >> 2).saturating_mul(2),
                        })
                    }
                    OP_BYE => Some(Packet::Bye { reason: body.first().copied().unwrap_or(REASON_HANGUP) }),
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

impl Cipher {
    /// Расшифровка (внутренняя)
    fn decrypt_borrow(&self, counter: u64, clear: &[u8], ct: &[u8]) -> Option<Vec<u8>> {
        let nonce = Self::nonce(&self.prefix_rx, counter);
        self.aead
            .decrypt(&nonce, Payload { msg: ct, aad: clear })
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use x25519_dalek::{PublicKey, StaticSecret};

    fn dh(a: &StaticSecret, b: &PublicKey) -> [u8; 32] {
        a.diffie_hellman(b).to_bytes()
    }

    #[test]
    fn cipher_roundtrip_files() {
        let mut r = [0u8; 32]; rand_bytes(&mut r);
        let sk = StaticSecret::from(r);
        let mut r2 = [0u8; 32]; rand_bytes(&mut r2);
        let sk2 = StaticSecret::from(r2);
        let sh_a = dh(&sk, &PublicKey::from(&sk2));
        let sh_b = dh(&sk2, &PublicKey::from(&sk));
        assert_eq!(sh_a, sh_b, "DH shared должен совпадать");

        let mut pf = [0u8; 32]; rand_bytes(&mut pf);
        let mut a = Cipher::new(&sh_a, &pf, true);
        let mut b = Cipher::new(&sh_b, &pf, false);

        let p1 = Packet::Text { msg_id: 7, text: "priv".into() };
        let w = Packet::encrypt_wire(&mut a, &Packet::Text { msg_id: 7, text: "priv".into() });
        let mut lc = 0u64; let mut la = 0u32;
        let got = Packet::decrypt_wire(&b, &w, &mut lc, &mut la);
        match got {
            Some(Packet::Text { msg_id, text }) => assert_eq!((msg_id, text), (7, "priv".into())),
            other => panic!("не расшифровалось: {:?}", other),
        }
        let _ = p1;

        // Ping/pong round-trip
        let w2 = Packet::encrypt_wire(&mut a, &Packet::Ping { msg_id: 3, ts_ms: 123456 });
        match Packet::decrypt_wire(&b, &w2, &mut lc, &mut la) {
            Some(Packet::Ping { msg_id, ts_ms }) => assert_eq!((msg_id, ts_ms), (3, 123456)),
            other => panic!("ping не расшифровался: {:?}", other),
        }

        // аудио-кадр
        let w3 = Packet::encrypt_wire(&mut a, &Packet::Audio { ssrc: 99, seq: 4, payload: vec![1,2,3,4] });
        match Packet::decrypt_wire(&b, &w3, &mut lc, &mut la) {
            Some(Packet::Audio { ssrc, seq, payload }) => {
                assert_eq!((ssrc, seq, payload), (99, 4, vec![1,2,3,4]));
            }
            other => panic!("audio не расшифровался: {:?}", other),
        }
    }

    fn rand_bytes(b: &mut [u8]) {
        use std::io::Read;
        let mut f = std::fs::File::open("/dev/urandom").unwrap();
        f.read_exact(b).unwrap();
    }
}
