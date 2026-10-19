//! UDP relay and a bidirectional reachability check; no audio dependencies.

use std::net::SocketAddr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::net::UdpSocket;

use crate::relay_state::{Code, Registration, RelayState};

pub const OP_RELAY_REG: u8 = 20;
pub const OP_RELAY_LEAVE: u8 = 22;
const OP_PROBE: u8 = 23;
const OP_PROBE_REPLY: u8 = 24;

fn room_packet(wire: &[u8], op: u8) -> Option<Code> {
    if wire.first().copied() != Some(op) {
        return None;
    }
    wire.get(1..5)?.try_into().ok()
}

pub fn leave_packet(room: Code) -> [u8; 5] {
    [OP_RELAY_LEAVE, room[0], room[1], room[2], room[3]]
}

/// The guard also releases membership if the client's task is cancelled.
pub struct Membership {
    socket: std::sync::Arc<UdpSocket>,
    server: SocketAddr,
    room: Code,
}

impl Membership {
    pub fn new(socket: std::sync::Arc<UdpSocket>, server: SocketAddr, room: Code) -> Self {
        Self {
            socket,
            server,
            room,
        }
    }
}

impl Drop for Membership {
    fn drop(&mut self) {
        // UDP leave is best-effort. An independent per-member timeout handles
        // crashes, network loss, and a lost leave packet.
        let _ = self
            .socket
            .try_send_to(&leave_packet(self.room), self.server);
    }
}

pub async fn run_relay(port: u16) -> Result<(), String> {
    run_relay_with_options(port, false).await
}

pub async fn run_relay_with_options(port: u16, verbose: bool) -> Result<(), String> {
    let socket = UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, port))
        .await
        .map_err(|e| e.to_string())?;
    println!(
        "ретранслятор remember-me слушает UDP :{}",
        socket.local_addr().unwrap().port()
    );
    println!(
        "регистрация двух адресов не подтверждает установление звонка; пакеты HELLO/ACCEPT и счётчики пересылки показаны ниже"
    );
    let mut state = RelayState::default();
    let mut buf = [0u8; 1500];
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_report = Instant::now();
    loop {
        let received = tokio::select! {
            result = socket.recv_from(&mut buf) => Some(result.map_err(|e| format!("приём UDP: {e}"))?),
            _ = tick.tick() => None,
        };
        let now = Instant::now();
        for (code, addr) in state.expire(now) {
            println!(
                "комната {}: участник {addr} удалён — 20 с без его пакетов",
                room_str(&code)
            );
        }
        if now.duration_since(last_report) >= Duration::from_secs(5) {
            last_report = now;
            for (code, room) in &state.rooms {
                let addresses: Vec<_> = room
                    .members
                    .iter()
                    .flatten()
                    .map(|m| m.addr.to_string())
                    .collect();
                println!(
                    "комната {}: зарегистрировано {}/2 [{}]; переслано A→B={} B→A={}, ошибок отправки={}",
                    room_str(code),
                    addresses.len(),
                    addresses.join(", "),
                    room.forwarded[0],
                    room.forwarded[1],
                    room.send_errors
                );
            }
        }
        let Some((n, from)) = received else { continue };
        let wire = &buf[..n];
        if n == 9 && wire[0] == OP_PROBE {
            let mut reply = [0u8; 9];
            reply.copy_from_slice(wire);
            reply[0] = OP_PROBE_REPLY;
            match socket.send_to(&reply, from).await {
                Ok(_) => println!("UDP-проверка: запрос от {from}, ответ отправлен"),
                Err(e) => eprintln!("UDP-проверка: отправить ответ к {from}: {e}"),
            }
            continue;
        }
        if let Some(code) = room_packet(wire, OP_RELAY_LEAVE) {
            if state
                .rooms
                .get(&code)
                .is_some_and(|r| r.members.iter().flatten().any(|m| m.addr == from))
            {
                state.remove(from, Some(code), None);
                println!("комната {}: участник {from} вышел", room_str(&code));
            }
            continue;
        }
        if let Some(code) = room_packet(wire, OP_RELAY_REG) {
            for (old_code, room) in &state.rooms {
                if *old_code != code && room.members.iter().flatten().any(|m| m.addr == from) {
                    println!(
                        "участник {from}: переход из комнаты {} в {}",
                        room_str(old_code),
                        room_str(&code)
                    );
                }
            }
            match state.register(code, from, now) {
                Registration::Added => {
                    let members: Vec<_> = state.rooms[&code]
                        .members
                        .iter()
                        .flatten()
                        .map(|m| m.addr.to_string())
                        .collect();
                    println!(
                        "комната {}: зарегистрировано {}/2 [{}]; ожидаем установления звонка клиентами",
                        room_str(&code),
                        members.len(),
                        members.join(", ")
                    );
                }
                Registration::Full => eprintln!(
                    "комната {}: отказ для {from} — два места заняты",
                    room_str(&code)
                ),
                Registration::Refreshed => {}
            }
            continue;
        }
        if let Some((code, to, direction)) = state.destination(from, now) {
            let room = state.rooms.get_mut(&code).unwrap();
            let kind = packet_kind(wire);
            match socket.send_to(wire, to).await {
                Ok(_) => {
                    room.forwarded[direction] += 1;
                    if verbose || matches!(wire.first(), Some(1..=4) | Some(11)) {
                        println!(
                            "комната {}: {kind} {from} → {to}, {n} байт отправлено",
                            room_str(&code)
                        );
                    }
                }
                Err(e) => {
                    room.send_errors += 1;
                    eprintln!(
                        "комната {}: ошибка отправки {kind} {from} → {to}: {e}",
                        room_str(&code)
                    );
                }
            }
        } else if verbose {
            println!(
                "UDP {} от {from}: нет второго зарегистрированного участника",
                packet_kind(wire)
            );
        }
    }
}

fn room_str(code: &Code) -> String {
    String::from_utf8_lossy(code).into_owned()
}

fn packet_kind(wire: &[u8]) -> &'static str {
    match wire.first() {
        Some(1) => "HELLO",
        Some(2) => "INCOMING",
        Some(3) => "ACCEPT",
        Some(4) => "DECLINE",
        Some(5) => "PING",
        Some(6) => "PONG",
        Some(7) => "AUDIO",
        Some(11) => "BYE",
        _ => "DATA",
    }
}

/// Unlike send_to success, a matching reply proves the UDP return path.
pub async fn check_relay(target: &str) -> Result<(), String> {
    let server = tokio::net::lookup_host(target)
        .await
        .map_err(|e| format!("адрес сервера: {e}"))?
        .find(SocketAddr::is_ipv4)
        .ok_or("у сервера нет IPv4-адреса")?;
    let socket = UdpSocket::bind("0.0.0.0:0")
        .await
        .map_err(|e| e.to_string())?;
    println!(
        "проверка UDP к {server}: 3 запроса; сервер должен быть обновлён до версии с --check-server"
    );
    let mut received = 0;
    for attempt in 1u64..=3 {
        let nonce = (SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64)
            ^ attempt;
        let mut probe = [OP_PROBE; 9];
        probe[1..].copy_from_slice(&nonce.to_be_bytes());
        socket
            .send_to(&probe, server)
            .await
            .map_err(|e| format!("отправка UDP: {e}"))?;
        let started = Instant::now();
        let reply = tokio::time::timeout(Duration::from_millis(1500), async {
            let mut buf = [0u8; 1500];
            loop {
                let (n, from) = socket.recv_from(&mut buf).await?;
                if from == server && n == 9 && buf[0] == OP_PROBE_REPLY && buf[1..9] == probe[1..9]
                {
                    return Ok::<(), std::io::Error>(());
                }
            }
        })
        .await;
        match reply {
            Ok(Ok(())) => {
                received += 1;
                println!(
                    "{attempt}/3: ответ сервера получен, RTT {:.1} мс",
                    started.elapsed().as_secs_f64() * 1000.0
                );
            }
            Ok(Err(e)) => println!("{attempt}/3: ошибка приёма: {e}"),
            Err(_) => println!("{attempt}/3: ответ не получен за 1500 мс"),
        }
    }
    if received == 0 {
        Err(format!(
            "0/3 ответов от {server}: проверьте адрес, обновление сервера и UDP-порт в файрволе VPS/провайдера"
        ))
    } else {
        println!("получено {received}/3 ответов: UDP работает в обе стороны");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_registration_is_compatible_with_client_packets() {
        assert_eq!(
            room_packet(&[OP_RELAY_REG, b'K', b'7', b'T', b'2'], OP_RELAY_REG),
            Some(*b"K7T2")
        );
        assert_eq!(room_packet(&[], OP_RELAY_REG), None);
        assert_eq!(room_packet(&[OP_RELAY_REG, b'K', b'7'], OP_RELAY_REG), None);
        assert_eq!(
            room_packet(&[7, b'K', b'7', b'T', b'2'], OP_RELAY_REG),
            None
        );
    }
}
