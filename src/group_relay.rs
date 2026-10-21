//! Versioned room routing. The relay sees membership/public keys, never media keys.
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    time::{Duration, Instant},
};
use tokio::net::UdpSocket;
pub const REGISTER: u8 = 25;
pub const ROSTER: u8 = 26;
pub const DATA: u8 = 27;
pub const LEAVE: u8 = 28;
pub const ERROR: u8 = 29;
pub const MAX_MEMBERS: usize = 16;
pub const VERSION: u8 = 1;
pub type RoomCode = [u8; 4];
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub id: u64,
    pub generation: u64,
    pub key: [u8; 32],
    pub name: String,
}
fn name_bytes(name: &str) -> [u8; 32] {
    let mut out = [0; 32];
    let mut n = 0;
    for c in name.chars().filter(|c| !c.is_control()) {
        if n + c.len_utf8() > 32 {
            break;
        }
        n += c.encode_utf8(&mut out[n..]).len();
    }
    out
}
fn name(b: &[u8]) -> String {
    String::from_utf8_lossy(&b[..b.iter().position(|c| *c == 0).unwrap_or(b.len())]).into_owned()
}
pub fn registration(code: RoomCode, identity: &Identity) -> Vec<u8> {
    let mut b = vec![REGISTER, VERSION];
    b.extend(code);
    b.extend(identity.id.to_be_bytes());
    b.extend(identity.key);
    b.extend(name_bytes(&identity.name));
    b
}
pub fn leave(code: RoomCode, id: u64) -> Vec<u8> {
    let mut b = vec![LEAVE];
    b.extend(code);
    b.extend(id.to_be_bytes());
    b
}
pub fn data(from: u64, to: u64, body: &[u8]) -> Vec<u8> {
    let mut b = vec![DATA];
    b.extend(from.to_be_bytes());
    b.extend(to.to_be_bytes());
    b.extend(body);
    b
}
pub fn parse_data(b: &[u8]) -> Option<(u64, u64, &[u8])> {
    if b.first() != Some(&DATA) || b.len() < 18 {
        return None;
    }
    Some((
        u64::from_be_bytes(b[1..9].try_into().ok()?),
        u64::from_be_bytes(b[9..17].try_into().ok()?),
        &b[17..],
    ))
}
pub fn parse_roster(b: &[u8]) -> Option<(RoomCode, u64, u64, Vec<Identity>)> {
    if b.first() != Some(&ROSTER) || b.get(1) != Some(&VERSION) || b.len() < 23 {
        return None;
    }
    let count = b[22] as usize;
    if count > MAX_MEMBERS || b.len() != 23 + 80 * count {
        return None;
    }
    let mut members = Vec::new();
    for chunk in b[23..].chunks_exact(80) {
        let identity = Identity {
            id: u64::from_be_bytes(chunk[..8].try_into().ok()?),
            generation: u64::from_be_bytes(chunk[8..16].try_into().ok()?),
            key: chunk[16..48].try_into().ok()?,
            name: name(&chunk[48..]),
        };
        if members.iter().any(|m: &Identity| m.id == identity.id) {
            return None;
        }
        members.push(identity);
    }
    Some((
        b[2..6].try_into().ok()?,
        u64::from_be_bytes(b[6..14].try_into().ok()?),
        u64::from_be_bytes(b[14..22].try_into().ok()?),
        members,
    ))
}
struct Member {
    identity: Identity,
    addr: SocketAddr,
    seen: Instant,
}
struct Room {
    members: BTreeMap<u64, Member>,
    revision: u64,
}
pub struct Rooms {
    rooms: BTreeMap<RoomCode, Room>,
    epoch: u64,
    next: u64,
}
impl Default for Rooms {
    fn default() -> Self {
        Self {
            rooms: BTreeMap::new(),
            epoch: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64,
            next: 0,
        }
    }
}
impl Rooms {
    fn roster(&self, code: RoomCode) -> Option<Vec<u8>> {
        let room = self.rooms.get(&code)?;
        let mut b = vec![ROSTER, VERSION];
        b.extend(code);
        b.extend(self.epoch.to_be_bytes());
        b.extend(room.revision.to_be_bytes());
        b.push(room.members.len() as u8);
        for m in room.members.values() {
            b.extend(m.identity.id.to_be_bytes());
            b.extend(m.identity.generation.to_be_bytes());
            b.extend(m.identity.key);
            b.extend(name_bytes(&m.identity.name));
        }
        Some(b)
    }
    async fn announce(&self, socket: &UdpSocket, code: RoomCode) {
        if let Some(b) = self.roster(code) {
            for m in self.rooms[&code].members.values() {
                let _ = socket.send_to(&b, m.addr).await;
            }
        }
    }
    pub async fn expire(&mut self, socket: &UdpSocket, now: Instant) {
        let mut changed = Vec::new();
        for (code, room) in &mut self.rooms {
            let before = room.members.len();
            room.members
                .retain(|_, m| now.duration_since(m.seen) < Duration::from_secs(20));
            if room.members.len() != before {
                room.revision += 1;
                changed.push(*code);
            }
        }
        for code in changed {
            self.announce(socket, code).await;
        }
        self.rooms.retain(|_, r| !r.members.is_empty());
    }
    pub async fn receive(
        &mut self,
        socket: &UdpSocket,
        b: &[u8],
        from: SocketAddr,
        now: Instant,
    ) -> bool {
        match b.first() {
            Some(&REGISTER) => {
                if b.len() != 78 || b[1] != VERSION {
                    return true;
                }
                let code: RoomCode = b[2..6].try_into().unwrap();
                if !code
                    .iter()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                {
                    return true;
                }
                let id = u64::from_be_bytes(b[6..14].try_into().unwrap());
                let key: [u8; 32] = b[14..46].try_into().unwrap();
                if !self.rooms.contains_key(&code) && self.rooms.len() >= 1024 {
                    return true;
                }
                if let Some(old) = self.rooms.get(&code).and_then(|r| r.members.get(&id))
                    && (old.addr != from || old.identity.key != key)
                {
                    let _ = socket.send_to(&[ERROR, 2], from).await;
                    return true;
                }
                let mut moved = Vec::new();
                for (old_code, room) in &mut self.rooms {
                    let before = room.members.len();
                    room.members
                        .retain(|old, m| m.addr != from || (*old_code == code && *old == id));
                    if before != room.members.len() {
                        room.revision += 1;
                        moved.push(*old_code);
                    }
                }
                for old in moved {
                    self.announce(socket, old).await;
                }
                self.rooms.retain(|_, r| !r.members.is_empty());
                let room = self.rooms.entry(code).or_insert_with(|| Room {
                    members: BTreeMap::new(),
                    revision: 0,
                });
                let changed = if let Some(m) = room.members.get_mut(&id) {
                    m.seen = now;
                    false
                } else {
                    if room.members.len() >= MAX_MEMBERS {
                        let _ = socket.send_to(&[ERROR, 1], from).await;
                        return true;
                    }
                    self.next += 1;
                    room.revision += 1;
                    room.members.insert(
                        id,
                        Member {
                            identity: Identity {
                                id,
                                generation: self.next,
                                key,
                                name: name(&b[46..78]),
                            },
                            addr: from,
                            seen: now,
                        },
                    );
                    true
                };
                if changed {
                    println!(
                        "группа {}: участников {}/{}",
                        String::from_utf8_lossy(&code),
                        room.members.len(),
                        MAX_MEMBERS
                    );
                    self.announce(socket, code).await;
                } else if let Some(b) = self.roster(code) {
                    let _ = socket.send_to(&b, from).await;
                }
                true
            }
            Some(&DATA) => {
                if let Some((id, to, _)) = parse_data(b) {
                    for room in self.rooms.values_mut() {
                        if let Some(member) = room.members.get_mut(&id)
                            && member.addr == from
                        {
                            member.seen = now;
                            if id != to
                                && let Some(other) = room.members.get(&to)
                            {
                                let _ = socket.send_to(b, other.addr).await;
                            }
                            break;
                        }
                    }
                }
                true
            }
            Some(&LEAVE) => {
                if b.len() == 13 {
                    let code: RoomCode = b[1..5].try_into().unwrap();
                    let id = u64::from_be_bytes(b[5..13].try_into().unwrap());
                    if let Some(room) = self.rooms.get_mut(&code)
                        && room.members.get(&id).is_some_and(|m| m.addr == from)
                    {
                        room.members.remove(&id);
                        room.revision += 1;
                        self.announce(socket, code).await;
                        println!("группа {}: участник вышел", String::from_utf8_lossy(&code));
                    }
                    self.rooms.retain(|_, r| !r.members.is_empty());
                }
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn accepts_sixteen_members_and_rejects_seventeenth_without_evicting_anyone() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut rooms = Rooms::default();
        let now = Instant::now();
        let mut clients = Vec::new();
        for id in 1..=17 {
            let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            rooms
                .receive(
                    &server,
                    &registration(
                        *b"AX3P",
                        &Identity {
                            id,
                            generation: 0,
                            key: [id as u8; 32],
                            name: format!("participant{id}"),
                        },
                    ),
                    client.local_addr().unwrap(),
                    now,
                )
                .await;
            clients.push(client);
        }
        assert_eq!(rooms.rooms[b"AX3P"].members.len(), MAX_MEMBERS);
        assert!(!rooms.rooms[b"AX3P"].members.contains_key(&17));
        let roster = rooms.roster(*b"AX3P").unwrap();
        assert!(roster.len() < 1500);
        assert_eq!(parse_roster(&roster).unwrap().3.len(), 16);
        let mut reply = [0; 1500];
        let n = tokio::time::timeout(Duration::from_secs(1), clients[16].recv(&mut reply))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&reply[..n], &[ERROR, 1]);
    }
    #[tokio::test]
    async fn routes_three_members_isolates_rooms_and_keeps_room_without_creator() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut rooms = Rooms::default();
        let now = Instant::now();
        let mut clients = Vec::new();
        for id in 1..=4 {
            let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let code = if id == 4 { *b"ZZZZ" } else { *b"AX3P" };
            rooms
                .receive(
                    &server,
                    &registration(
                        code,
                        &Identity {
                            id,
                            generation: 0,
                            key: [id as u8; 32],
                            name: format!("user{id}"),
                        },
                    ),
                    socket.local_addr().unwrap(),
                    now,
                )
                .await;
            clients.push(socket);
        }
        assert_eq!(rooms.rooms[b"AX3P"].members.len(), 3);
        assert!(rooms.roster(*b"AX3P").unwrap().len() < 1500);
        for c in &clients {
            c.readable().await.unwrap();
            let mut b = [0; 1500];
            while c.try_recv(&mut b).is_ok() {}
        }
        rooms
            .receive(
                &server,
                &data(1, 2, b"opaque encrypted data"),
                clients[0].local_addr().unwrap(),
                now,
            )
            .await;
        let mut buf = [0; 1500];
        let n = tokio::time::timeout(Duration::from_secs(1), clients[1].recv(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(parse_data(&buf[..n]).unwrap().2, b"opaque encrypted data");
        rooms
            .receive(
                &server,
                &data(1, 4, b"cross-room"),
                clients[0].local_addr().unwrap(),
                now,
            )
            .await;
        assert!(clients[3].try_recv(&mut buf).is_err());
        rooms
            .receive(
                &server,
                &leave(*b"AX3P", 1),
                clients[0].local_addr().unwrap(),
                now,
            )
            .await;
        assert_eq!(rooms.rooms[b"AX3P"].members.len(), 2);
        let generation = rooms.rooms[b"AX3P"].members[&2].identity.generation;
        rooms
            .receive(
                &server,
                &leave(*b"AX3P", 2),
                clients[1].local_addr().unwrap(),
                now,
            )
            .await;
        rooms
            .receive(
                &server,
                &registration(
                    *b"AX3P",
                    &Identity {
                        id: 2,
                        generation: 0,
                        key: [2; 32],
                        name: "user2".into(),
                    },
                ),
                clients[1].local_addr().unwrap(),
                now,
            )
            .await;
        assert_ne!(
            rooms.rooms[b"AX3P"].members[&2].identity.generation,
            generation
        );
        rooms.expire(&server, now + Duration::from_secs(20)).await;
        assert!(rooms.rooms.is_empty());
    }
}
