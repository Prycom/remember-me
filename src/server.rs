// Режим ретранслятора: `remember-me --server`.
//
// Двое участников одной «комнаты» пересылаются друг через друга вслепую:
// сервер заглядывает ТОЛЬКО в открытые пакеты (HELLO/RELAY_REG), содержимое
// звонка шифровано XChaCha20-Poly1305 end-to-end — сервер слышать не может.
// Это решает «не проброшенные порты» и разность VPN: оба участника открывают
// исходящее UDP-окно к серверу, все пакеты ходят через него туда-обратно.
//
// Протокол регистрации: OP_RELAY_REG { room } открытым пакетом. Первый,
// кто послал REG, занимает комнату; второй подключается — и с этого момента
// сервер тупо пересылает каждый датаграмм собеседнику противоположной стороны.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;

/// Shared with the client wire protocol. The relay needs only this packet;
/// the remaining datagrams are forwarded without parsing or decryption.
pub const OP_RELAY_REG: u8 = 20;

fn registration_room(wire: &[u8]) -> Option<[u8; 4]> {
    if wire.first().copied() != Some(OP_RELAY_REG) {
        return None;
    }
    wire.get(1..5)?.try_into().ok()
}

/// Тишина дольше этого — комната выбрасывается.
const ROOM_IDLE: Duration = Duration::from_secs(20);

struct Room {
    a: Option<SocketAddr>,
    b: Option<SocketAddr>,
    last: Instant,
}

pub async fn run_relay(port: u16) -> Result<(), String> {
    let sock = UdpSocket::bind(format!("0.0.0.0:{port}"))
        .await
        .map_err(|e| e.to_string())?;
    println!("ретранслятор remember-me слушает UDP :{port}");
    println!("участники звонят кодом комнаты; содержимое звонка e2e-шифровано, сервер не слышит");

    let rx = &sock;
    let mut rooms: HashMap<[u8; 4], Room> = HashMap::new();
    let mut buf = vec![0u8; 1500];

    loop {
        let (n, from) = match tokio::time::timeout(ROOM_IDLE / 20, rx.recv_from(&mut buf)).await {
            Ok(Ok(v)) => v,
            _ => {
                rooms_reap(&mut rooms);
                continue;
            }
        };
        rooms_reap(&mut rooms);
        let wire = &buf[..n];

        // открытая регистрация комнаты?
        if let Some(room) = registration_room(wire) {
            let entry = rooms.entry(room).or_insert(Room {
                a: None,
                b: None,
                last: Instant::now(),
            });
            let known = entry.a == Some(from) || entry.b == Some(from);
            if !known {
                if entry.a.is_none() {
                    entry.a = Some(from);
                } else if entry.b.is_none() {
                    entry.b = Some(from);
                }
            }
            entry.last = Instant::now();
            announce(&rooms, room);
            continue;
        }

        // переслать напарнику той комнаты, откуда пришла датаграмма
        let mut found = None;
        for (k, r) in rooms.iter() {
            if r.a == Some(from) || r.b == Some(from) {
                found = Some(*k);
                break;
            }
        }
        if let Some(room) = found {
            let r = rooms.get_mut(&room).expect("комната из поиска есть");
            r.last = Instant::now();
            // адрес мог измениться (NAT-переклейка) — обновляем свою сторону
            if r.a.is_none() {
                r.a = Some(from);
            }
            if r.b.is_none() {
                r.b = Some(from);
            }
            let dest = if r.a == Some(from) { r.b } else { r.a };
            if let Some(dest) = dest {
                let _ = sock.send_to(wire, dest).await;
            }
        }
    }
}

fn rooms_reap(rooms: &mut HashMap<[u8; 4], Room>) {
    let now = Instant::now();
    rooms.retain(|k, r| {
        let live = now.duration_since(r.last) < ROOM_IDLE;
        if !live {
            println!("комната {}: тишина — убрана", room_str(k));
        }
        live
    });
}

fn room_str(k: &[u8; 4]) -> String {
    k.iter().map(|c| *c as char).collect()
}

fn announce(rooms: &HashMap<[u8; 4], Room>, room: [u8; 4]) {
    if let Some(r) = rooms.get(&room) {
        if let (Some(a), Some(b)) = (r.a, r.b) {
            println!(
                "комната {}: обе стороны на связи ({} ↔ {})",
                room_str(&room),
                a,
                b
            );
        } else {
            println!("комната {}: ждём второго", room_str(&room));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_registration_is_compatible_with_client_packets() {
        assert_eq!(
            registration_room(&[OP_RELAY_REG, b'K', b'7', b'T', b'2']),
            Some(*b"K7T2")
        );
        assert_eq!(registration_room(&[]), None);
        assert_eq!(registration_room(&[OP_RELAY_REG, b'K', b'7']), None);
        assert_eq!(registration_room(&[7, b'K', b'7', b'T', b'2']), None);
    }
}
