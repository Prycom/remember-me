// Копирование через OSC 52: работает по SSH и в tmux (set -g set-clipboard on).

use std::io::Write;

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn copy(text: &str) -> std::io::Result<()> {
    let mut stdout = std::io::stdout();
    stdout.write_all(b"\x1b]52;c;")?;
    stdout.write_all(encode(text).as_bytes())?;
    stdout.write_all(b"\x07")?;
    stdout.flush()
}

fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 4 / 3 + 4);
    for chunk in text.as_bytes().chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        out.push(B64[(b[0] >> 2) as usize] as char);
        out.push(B64[(((b[0] & 3) << 4) | (b[1] >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            B64[(((b[1] & 0xf) << 2) | (b[2] >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[(b[2] & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_vectors() {
        for (text, want) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("10.0.0.1:51500", "MTAuMC4wLjE6NTE1MDA="),
        ] {
            assert_eq!(encode(text), want);
        }
    }
}
