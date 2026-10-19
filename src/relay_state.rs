use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

pub(crate) type Code = [u8; 4];
pub(crate) const MEMBER_IDLE: Duration = Duration::from_secs(20);

pub(crate) struct Member {
    pub addr: SocketAddr,
    pub last_seen: Instant,
}

pub(crate) struct Room {
    pub members: [Option<Member>; 2],
    pub forwarded: [u64; 2],
    pub send_errors: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Registration {
    Added,
    Refreshed,
    Full,
}

#[derive(Default)]
pub(crate) struct RelayState {
    pub rooms: HashMap<Code, Room>,
}

impl RelayState {
    pub fn register(&mut self, code: Code, addr: SocketAddr, now: Instant) -> Registration {
        // A UDP endpoint can belong to only one room. Reusing the same local
        // port for a new room must not leave an ambiguous route in the old one.
        self.remove(addr, None, Some(code));
        let room = self.rooms.entry(code).or_insert_with(|| Room {
            members: [None, None],
            forwarded: [0, 0],
            send_errors: 0,
        });
        if let Some(member) = room.members.iter_mut().flatten().find(|m| m.addr == addr) {
            member.last_seen = now;
            return Registration::Refreshed;
        }
        if let Some(slot) = room.members.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(Member {
                addr,
                last_seen: now,
            });
            return Registration::Added;
        }
        Registration::Full
    }

    pub fn remove(&mut self, addr: SocketAddr, only: Option<Code>, except: Option<Code>) {
        self.rooms.retain(|code, room| {
            if only.is_none_or(|c| c == *code) && except != Some(*code) {
                for slot in &mut room.members {
                    if slot.as_ref().is_some_and(|m| m.addr == addr) {
                        *slot = None;
                    }
                }
            }
            room.members.iter().any(Option::is_some)
        });
    }

    pub fn expire(&mut self, now: Instant) -> Vec<(Code, SocketAddr)> {
        let mut expired = Vec::new();
        self.rooms.retain(|code, room| {
            for slot in &mut room.members {
                if slot
                    .as_ref()
                    .is_some_and(|m| now.duration_since(m.last_seen) >= MEMBER_IDLE)
                {
                    expired.push((*code, slot.as_ref().unwrap().addr));
                    *slot = None;
                }
            }
            room.members.iter().any(Option::is_some)
        });
        expired
    }

    pub fn destination(
        &mut self,
        from: SocketAddr,
        now: Instant,
    ) -> Option<(Code, SocketAddr, usize)> {
        for (code, room) in &mut self.rooms {
            for direction in 0..2 {
                if let Some(member) = &mut room.members[direction]
                    && member.addr == from
                {
                    member.last_seen = now;
                    return room.members[1 - direction]
                        .as_ref()
                        .map(|other| (*code, other.addr, direction));
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    #[test]
    fn same_endpoint_moves_to_new_room_even_if_old_peer_is_still_dialing() {
        let mut state = RelayState::default();
        let now = Instant::now();
        state.register(*b"AX3P", addr(52744), now);
        state.register(*b"AX3P", addr(4115), now);
        state.register(*b"4UN2", addr(52744), now);
        state.register(*b"4UN2", addr(8778), now);
        assert_eq!(
            state.destination(addr(8778), now),
            Some((*b"4UN2", addr(52744), 1))
        );
        assert_eq!(
            state.destination(addr(52744), now),
            Some((*b"4UN2", addr(8778), 0))
        );
        assert_eq!(state.destination(addr(4115), now), None);
    }

    #[test]
    fn host_heartbeats_do_not_keep_a_disconnected_peer_alive() {
        let mut state = RelayState::default();
        let now = Instant::now();
        state.register(*b"AX3P", addr(1), now);
        state.register(*b"AX3P", addr(2), now);
        let later = now + MEMBER_IDLE;
        state.register(*b"AX3P", addr(1), later);
        assert_eq!(state.expire(later), vec![(*b"AX3P", addr(2))]);
        assert_eq!(
            state.register(*b"AX3P", addr(3), later),
            Registration::Added
        );
        assert_eq!(
            state.destination(addr(1), later),
            Some((*b"AX3P", addr(3), 0))
        );
    }

    #[test]
    fn full_room_rejects_third_endpoint_and_leaving_releases_slot() {
        let mut state = RelayState::default();
        let now = Instant::now();
        assert_eq!(state.register(*b"AX3P", addr(1), now), Registration::Added);
        assert_eq!(
            state.register(*b"AX3P", addr(1), now),
            Registration::Refreshed
        );
        state.register(*b"AX3P", addr(2), now);
        assert_eq!(state.register(*b"AX3P", addr(3), now), Registration::Full);
        state.remove(addr(2), Some(*b"AX3P"), None);
        assert_eq!(state.register(*b"AX3P", addr(3), now), Registration::Added);
        state.remove(addr(1), Some(*b"AX3P"), None);
        state.remove(addr(3), Some(*b"AX3P"), None);
        assert!(state.rooms.is_empty());
    }
}
