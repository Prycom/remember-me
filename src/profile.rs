//! Reliable, authenticated profile metadata, independent of relay rosters.
use crate::{
    avatar::Delivery,
    proto::{Packet, name_from_bytes, name_to_bytes},
};
use std::time::Instant;

pub struct Name {
    pub value: String,
    revision: u64,
}
impl Name {
    pub fn new(value: &str) -> Self {
        Self {
            value: normalize_name(value),
            revision: 1,
        }
    }
    pub fn set(&mut self, value: &str) {
        let value = normalize_name(value);
        if self.value != value {
            self.value = value;
            self.revision += 1;
        }
    }
    pub fn next(&self, delivery: &mut Delivery, now: Instant) -> Option<Packet> {
        delivery.due(self.revision, now).then(|| Packet::Name {
            revision: self.revision,
            name: self.value.clone(),
        })
    }
}

pub fn normalize_name(value: &str) -> String {
    let clean: String = value.chars().filter(|c| !c.is_control()).collect();
    name_from_bytes(&name_to_bytes(clean.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn names_retry_until_acked_and_reach_late_joiners() {
        let mut name = Name::new("Старое имя");
        let mut delivery = Delivery::default();
        let now = Instant::now();
        assert!(name.next(&mut delivery, now).is_some()); // Lost datagram.
        assert!(
            name.next(&mut delivery, now + Duration::from_secs(1))
                .is_none()
        );
        assert!(
            name.next(&mut delivery, now + Duration::from_secs(2))
                .is_some()
        );
        delivery.ack(1);
        assert!(
            name.next(&mut delivery, now + Duration::from_secs(4))
                .is_none()
        );
        name.set("Новое имя");
        delivery.ack(1); // An old ACK must not acknowledge the new name.
        assert!(
            matches!(name.next(&mut delivery, now), Some(Packet::Name { revision: 2, name }) if name == "Новое имя")
        );
        let mut late_joiner = Delivery::default();
        assert!(
            matches!(name.next(&mut late_joiner, now), Some(Packet::Name { revision: 2, name }) if name == "Новое имя")
        );
        delivery.ack(2);
        assert!(
            name.next(&mut delivery, now + Duration::from_secs(10))
                .is_none()
        );
    }
    #[test]
    fn names_fit_wire_utf8_without_terminal_controls() {
        let value = normalize_name("  Я\n\0\u{1b}яяяяяяяяяяяяяяяяяяяяяяяяяяяяяяяяяяя  ");
        assert!(value.len() <= 32);
        assert!(value.chars().all(|c| !c.is_control()));
        assert_eq!(value, normalize_name(&value));
    }
}
