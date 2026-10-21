// Копирование через OSC 52: работает по SSH и в tmux (set -g set-clipboard on).

use std::io::Write;

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub enum Input {
    Text(String),
    Image(tempfile::TempPath),
}

/// Called only for an explicit paste, on a short-lived background thread.
pub fn read(allow_image: bool, directory: &std::path::Path) -> Result<Input, String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| format!("буфер обмена: {e}"))?;
    if allow_image && let Ok(image) = clipboard.get_image() {
        return save_image(directory, image.width, image.height, image.bytes.as_ref())
            .map(Input::Image);
    }
    clipboard
        .get_text()
        .map(Input::Text)
        .map_err(|e| format!("в буфере нет доступной картинки или текста: {e}"))
}
fn save_image(
    directory: &std::path::Path,
    width: usize,
    height: usize,
    bytes: &[u8],
) -> Result<tempfile::TempPath, String> {
    if width == 0
        || height == 0
        || width > 4096
        || height > 4096
        || width.checked_mul(height).and_then(|n| n.checked_mul(4)) != Some(bytes.len())
    {
        return Err("картинка из буфера должна быть не больше 4096×4096".into());
    }
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut temporary = tempfile::Builder::new()
        .prefix("remember-me-clipboard-")
        .suffix(".png")
        .tempfile_in(directory)
        .map_err(|e| e.to_string())?;
    let file = temporary.as_file_mut();
    let result = image::codecs::png::PngEncoder::new(&mut *file);
    use image::ImageEncoder;
    if let Err(error) = result.write_image(
        bytes,
        width as u32,
        height as u32,
        image::ExtendedColorType::Rgba8,
    ) {
        return Err(error.to_string());
    }
    file.flush().map_err(|e| e.to_string())?;
    Ok(temporary.into_temp_path())
}

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
    fn pasted_image_is_a_private_lossless_file_and_invalid_dimensions_are_rejected() {
        let pixels = vec![20, 40, 80, 255].repeat(16 * 8);
        let path = save_image(&std::env::temp_dir(), 16, 8, &pixels).unwrap();
        let decoded = image::open(&path).unwrap().into_rgba8();
        assert_eq!(decoded.dimensions(), (16, 8));
        assert_eq!(decoded.as_raw(), &pixels);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let removed = path.to_path_buf();
        drop(path);
        assert!(
            !removed.exists(),
            "temporary clipboard image must be deleted on drop"
        );
        assert!(save_image(&std::env::temp_dir(), 0, 8, &[]).is_err());
        assert!(save_image(&std::env::temp_dir(), 4097, 1, &[]).is_err());
        assert!(save_image(&std::env::temp_dir(), 16, 8, &pixels[..4]).is_err());
    }

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
