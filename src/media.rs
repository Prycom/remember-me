//! On-demand attachment viewing. Decoding never runs in the voice/UI loop.
use image::{AnimationDecoder, ImageDecoder};
use std::{
    io::{BufReader, Read},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

pub struct Frame {
    pub image: Arc<image::RgbaImage>,
    pub delay: Duration,
}
pub enum Content {
    Image(Vec<Frame>),
    Text(String),
}
pub struct Viewer {
    pub path: PathBuf,
    pub content: Option<Content>,
    pub error: Option<String>,
    pub scroll: usize,
    pub frame: usize,
    pub paused: bool,
    pub prepared: std::cell::Cell<bool>,
    pub text_rows: std::cell::Cell<usize>,
    pub wrapped: std::cell::RefCell<Option<(u16, Vec<String>)>>,
    elapsed: Duration,
    last: Instant,
    cancelled: Arc<AtomicBool>,
    result: Option<mpsc::Receiver<Result<Content, String>>>,
}
impl Drop for Viewer {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}
impl Viewer {
    pub fn new(path: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let task_path = path.clone();
        let cancel = cancelled.clone();
        std::thread::spawn(move || {
            let _ = tx.send(load(&task_path, &cancel));
        });
        Self {
            path,
            content: None,
            error: None,
            scroll: 0,
            frame: 0,
            paused: false,
            prepared: std::cell::Cell::new(false),
            elapsed: Duration::ZERO,
            last: Instant::now(),
            cancelled,
            result: Some(rx),
            text_rows: std::cell::Cell::new(0),
            wrapped: std::cell::RefCell::new(None),
        }
    }
    pub fn poll(&mut self) {
        if let Some(rx) = &self.result
            && let Ok(result) = rx.try_recv()
        {
            match result {
                Ok(content) => {
                    if std::env::var("RM_DEBUG").ok().as_deref() == Some("1") {
                        let kind = match &content {
                            Content::Image(frames) => format!("image frames={}", frames.len()),
                            Content::Text(_) => "text".into(),
                        };
                        eprintln!(
                            "[media] loaded {} {kind}",
                            self.path.file_name().unwrap_or_default().to_string_lossy()
                        );
                    }
                    self.content = Some(content);
                }
                Err(error) => self.error = Some(error),
            }
            self.result = None;
            self.last = Instant::now();
        }
        let now = Instant::now();
        if !self.prepared.get() {
            self.elapsed = Duration::ZERO;
            self.frame = 0;
        }
        if self.prepared.get()
            && !self.paused
            && let Some(Content::Image(frames)) = &self.content
            && frames.len() > 1
        {
            self.elapsed += now.duration_since(self.last).min(Duration::from_secs(1));
            for _ in 0..frames.len() {
                if self.elapsed < frames[self.frame].delay {
                    break;
                }
                self.elapsed -= frames[self.frame].delay;
                self.frame = (self.frame + 1) % frames.len();
            }
        }
        self.last = now;
    }
    pub fn step(&mut self, direction: i16) {
        if let Some(Content::Image(frames)) = &self.content {
            self.paused = true;
            self.frame = (self.frame as i16 + direction).rem_euclid(frames.len() as i16) as usize;
            self.elapsed = Duration::ZERO;
        }
    }
    pub fn scroll_by(&mut self, delta: i32) {
        let max = match &self.content {
            Some(Content::Text(text)) => self
                .text_rows
                .get()
                .max(text.lines().count())
                .saturating_sub(1),
            _ => 0,
        };
        self.scroll = (self.scroll as i64 + delta as i64).clamp(0, max as i64) as usize;
    }
}
pub fn supported(path: &Path) -> bool {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        extension.as_str(),
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "ico" | "tif" | "tiff"
    ) || text_type(&extension)
}
fn text_type(extension: &str) -> bool {
    matches!(
        extension,
        "txt"
            | "md"
            | "log"
            | "toml"
            | "json"
            | "yaml"
            | "yml"
            | "ini"
            | "conf"
            | "cfg"
            | "csv"
            | "rs"
            | "py"
            | "sh"
            | "c"
            | "h"
            | "cpp"
            | "js"
            | "ts"
            | "css"
            | "html"
            | "xml"
    )
}
fn load(path: &Path, cancel: &AtomicBool) -> Result<Content, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("нужен обычный файл".into());
    }
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if text_type(&extension) {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(65537)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        let truncated = bytes.len() > 65536;
        bytes.truncate(65536);
        let mut text: String = String::from_utf8_lossy(&bytes)
            .chars()
            .filter_map(|c| match c {
                '\n' => Some(c),
                '\t' => Some(' '),
                c if c.is_control() => None,
                c => Some(c),
            })
            .collect();
        if truncated {
            text.push_str("\n… показаны первые 64 КиБ; O — открыть целиком");
        }
        return Ok(Content::Text(text));
    }
    if meta.len() > 32 * 1024 * 1024 {
        return Err(
            "для встроенного просмотра картинка должна быть меньше 32 МиБ; O — открыть внешне"
                .into(),
        );
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(96 * 1024 * 1024);
    let mut reader = image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    reader.limits(limits.clone());
    if reader.format() == Some(image::ImageFormat::Gif) {
        let mut decoder = image::codecs::gif::GifDecoder::new(BufReader::new(
            std::fs::File::open(path).map_err(|e| e.to_string())?,
        ))
        .map_err(|e| e.to_string())?;
        decoder.set_limits(limits).map_err(|e| e.to_string())?;
        let mut frames: Vec<Frame> = Vec::new();
        let mut candidate = None;
        let mut delay = Duration::ZERO;
        let mut slot = Duration::from_millis(100);
        let mut total = Duration::ZERO;
        for (index, frame) in decoder.into_frames().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Err("просмотр закрыт".into());
            }
            if index >= 2000 || total > Duration::from_secs(120) {
                return Err("GIF длиннее лимита встроенного просмотра; O — открыть внешне".into());
            }
            let frame = frame.map_err(|e| e.to_string())?;
            let (n, d) = frame.delay().numer_denom_ms();
            let ms = if n == 0 {
                100
            } else {
                (n as u64 / d.max(1) as u64).max(20)
            };
            delay += Duration::from_millis(ms);
            total += Duration::from_millis(ms);
            if candidate.is_none() {
                let image = image::DynamicImage::ImageRgba8(frame.into_buffer());
                let scaled = if image.width() > 256 || image.height() > 256 {
                    image.resize(256, 256, image::imageops::FilterType::Triangle)
                } else {
                    image
                }
                .into_rgba8();
                candidate = Some(Arc::new(scaled));
            }
            if delay >= slot {
                frames.push(Frame {
                    image: candidate.take().unwrap(),
                    delay,
                });
                delay = Duration::ZERO;
                if frames.len() > 32 {
                    frames = compact(frames);
                    slot *= 2;
                }
            }
        }
        if let Some(image) = candidate {
            frames.push(Frame {
                image,
                delay: delay.max(Duration::from_millis(100)),
            });
        }
        if frames.len() > 32 {
            frames = compact(frames);
        }
        if frames.is_empty() {
            return Err("GIF не содержит кадров".into());
        }
        return Ok(Content::Image(frames));
    }
    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    limits
        .reserve(decoder.total_bytes())
        .map_err(|e| e.to_string())?;
    decoder.set_limits(limits).map_err(|e| e.to_string())?;
    let mut image = image::DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    image.apply_orientation(orientation);
    if cancel.load(Ordering::Relaxed) {
        return Err("просмотр закрыт".into());
    }
    let image = if image.width() > 1600 || image.height() > 1600 {
        image.resize(1600, 1600, image::imageops::FilterType::Lanczos3)
    } else {
        image
    };
    Ok(Content::Image(vec![Frame {
        image: Arc::new(image.into_rgba8()),
        delay: Duration::from_secs(1),
    }]))
}
fn compact(frames: Vec<Frame>) -> Vec<Frame> {
    let mut result = Vec::new();
    let mut iter = frames.into_iter();
    while let Some(mut frame) = iter.next() {
        if let Some(next) = iter.next() {
            frame.delay += next.delay;
        }
        result.push(frame);
    }
    result
}

pub fn wrap_text(text: &str, width: u16) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;
    let mut lines = Vec::new();
    for input in text.split('\n') {
        let mut line = String::new();
        let mut used = 0;
        for c in input.chars() {
            let size = c.width().unwrap_or(0);
            if used + size > width.max(1) as usize && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            line.push(c);
            used += size;
        }
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gif_animation_is_bounded_preserves_duration_and_can_be_paused() {
        let path = std::env::temp_dir().join(format!("rm-animation-{}.gif", std::process::id()));
        {
            let mut encoder =
                image::codecs::gif::GifEncoder::new(std::fs::File::create(&path).unwrap());
            encoder
                .set_repeat(image::codecs::gif::Repeat::Infinite)
                .unwrap();
            for i in 0..100u8 {
                let pixels = image::RgbaImage::from_pixel(16, 8, image::Rgba([i * 2, 20, 80, 255]));
                encoder
                    .encode_frame(image::Frame::from_parts(
                        pixels,
                        0,
                        0,
                        image::Delay::from_numer_denom_ms(100, 1),
                    ))
                    .unwrap();
            }
        }
        let content = load(&path, &AtomicBool::new(false)).unwrap();
        let Content::Image(frames) = &content else {
            panic!("not animated")
        };
        assert!(frames.len() > 1 && frames.len() <= 32);
        assert_eq!(
            frames.iter().map(|f| f.delay).sum::<Duration>(),
            Duration::from_secs(10)
        );
        assert!(
            frames
                .iter()
                .all(|f| f.image.width() <= 256 && f.image.height() <= 256)
        );
        let mut viewer = Viewer::new(path.clone());
        viewer.result = None;
        viewer.content = Some(content);
        viewer.last = Instant::now() - Duration::from_millis(800);
        viewer.poll();
        assert_eq!(
            viewer.frame, 0,
            "the loading screen must not advance playback"
        );
        viewer.prepared.set(true);
        viewer.last = Instant::now() - Duration::from_millis(800);
        viewer.poll();
        assert!(viewer.frame > 0);
        let frame = viewer.frame;
        viewer.paused = true;
        viewer.last -= Duration::from_secs(1);
        viewer.poll();
        assert_eq!(viewer.frame, frame);
        viewer.step(-1);
        assert_eq!(viewer.frame, frame - 1);
        drop(viewer);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn text_preview_strips_terminal_controls_and_wraps_wide_characters() {
        let path = std::env::temp_dir().join(format!("rm-text-{}.txt", std::process::id()));
        std::fs::write(&path, "А🙂界\x1b[31m\0\nследующая строка").unwrap();
        let Content::Text(text) = load(&path, &AtomicBool::new(false)).unwrap() else {
            panic!()
        };
        assert!(!text.contains('\x1b') && !text.contains('\0'));
        assert_eq!(wrap_text("A🙂界B", 3), ["A🙂", "界B"]);
        assert!(supported(&path));
        assert!(!supported(Path::new("program.exe")));
        std::fs::remove_file(path).unwrap();
    }
}
