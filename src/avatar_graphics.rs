//! Small, fixed-size thumbnails. Native graphics retain sub-cell detail when
//! supported; encoding is cached and runs outside the UI and voice threads.
use crate::avatar::{Avatar, AvatarStyle, Portrait};
use ratatui::{
    Frame,
    layout::{Rect, Size},
};
use ratatui_image::{
    Image, Resize,
    picker::{Picker, ProtocolType},
    protocol::Protocol,
};
use std::{
    collections::HashMap,
    sync::{Arc, mpsc},
};

type Key = (usize, u16, u16, u8);
#[derive(Clone)]
enum Source {
    Avatar(Arc<Avatar>),
    Image(Arc<image::RgbaImage>),
}
impl Source {
    fn ptr(&self) -> usize {
        match self {
            Self::Avatar(a) => Arc::as_ptr(a) as usize,
            Self::Image(a) => Arc::as_ptr(a) as usize,
        }
    }
    fn rgba(&self) -> image::RgbaImage {
        match self {
            Self::Avatar(a) => a.rgba(),
            Self::Image(a) => (**a).clone(),
        }
    }
    fn retained_bytes(&self) -> usize {
        match self {
            Self::Avatar(a) => a.side() * a.side() * 4,
            Self::Image(a) => a.as_raw().len(),
        }
    }
}
struct Entry {
    _source: Source,
    image: Option<Protocol>,
    completed: bool,
    last_used: u64,
    kitty_id: Option<u32>,
    bytes: usize,
}
struct Worker {
    requests: mpsc::SyncSender<(Key, Source, u32)>,
    results: mpsc::Receiver<(Key, Option<Protocol>)>,
    font: ratatui_image::FontSize,
    kitty: bool,
}
#[derive(Default)]
pub struct Thumbnails {
    worker: Option<Worker>,
    cache: HashMap<Key, Entry>,
    frame: u64,
    garbage: Vec<u32>,
    next_id: u32,
}
impl Thumbnails {
    pub fn detect(&mut self) {
        if std::env::var_os("TMUX").is_some() {
            return;
        }
        if let Some(picker) = detect_picker() {
            self.configure(picker);
        }
    }
    fn configure(&mut self, picker: Picker) {
        if picker.protocol_type() == ProtocolType::Halfblocks {
            return;
        }
        let font = picker.font_size();
        let kitty = picker.protocol_type() == ProtocolType::Kitty;
        self.next_id = u32::from_be_bytes(crate::net::rnd32()[..4].try_into().unwrap()).max(1);
        let (requests, receive) = mpsc::sync_channel::<(Key, Source, u32)>(4);
        let (send, results) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("terminal-images".into())
            .spawn(move || {
                while let Ok((key, source, id)) = receive.recv() {
                    let image = source.rgba();
                    let filter = if key.3 & 1 != 0 {
                        image::imageops::FilterType::Lanczos3
                    } else {
                        image::imageops::FilterType::Nearest
                    };
                    let resize = Resize::Scale(Some(filter));
                    let dynamic = image::DynamicImage::ImageRgba8(image);
                    let size = resize.size_for(&dynamic, font, Size::new(key.1, key.2));
                    let mut resized = resize.resize(&dynamic, font, size, None).into_rgba8();
                    if key.3 & 2 != 0 {
                        round_image(&mut resized);
                    }
                    let resized = image::DynamicImage::ImageRgba8(resized);
                    let protocol = if kitty {
                        ratatui_image::protocol::kitty::Kitty::new(resized, size, id, false, false)
                            .ok()
                            .map(Protocol::Kitty)
                    } else {
                        picker
                            .new_protocol(
                                resized,
                                size,
                                Resize::Fit(Some(image::imageops::FilterType::Nearest)),
                            )
                            .ok()
                    };
                    if send.send((key, protocol)).is_err() {
                        break;
                    }
                }
            });
        if thread.is_ok() {
            self.worker = Some(Worker {
                requests,
                results,
                font,
                kitty,
            });
        }
    }
    pub fn begin_frame(&mut self) {
        self.frame += 1;
        if let Some(worker) = &self.worker {
            while let Ok((key, image)) = worker.results.try_recv() {
                if let Some(entry) = self.cache.get_mut(&key) {
                    entry.image = image;
                    entry.completed = true;
                }
            }
        }
        // Bound decoded sources and encoded terminal payloads, including GIFs.
        let mut cost: usize = self.cache.values().map(|e| e.bytes).sum();
        if self.cache.len() > 64 || cost > 24 * 1024 * 1024 {
            let mut old: Vec<_> = self
                .cache
                .iter()
                .filter(|(_, e)| self.frame.saturating_sub(e.last_used) > 1)
                .map(|(k, e)| (*k, e.last_used))
                .collect();
            old.sort_by_key(|(_, used)| *used);
            for (key, _) in old {
                if self.cache.len() <= 48 && cost <= 20 * 1024 * 1024 {
                    break;
                }
                if let Some(entry) = self.cache.remove(&key) {
                    cost = cost.saturating_sub(entry.bytes);
                    if let Some(id) = entry.kitty_id {
                        self.garbage.push(id);
                    }
                }
            }
        }
    }
    pub fn clear_images(&mut self) {
        let mut deleted = Vec::new();
        self.cache.retain(|key, e| {
            if key.3 & 4 != 0 {
                if let Some(id) = e.kitty_id {
                    deleted.push(id);
                }
                false
            } else {
                true
            }
        });
        self.garbage.extend(deleted);
    }
    pub fn flush_cleanup(&mut self) {
        if self.garbage.is_empty() {
            return;
        }
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        for id in self.garbage.drain(..) {
            let _ = write!(out, "\x1b_Ga=d,d=I,i={id},q=2;\x1b\\");
        }
        let _ = out.flush();
    }
    pub fn shutdown(&mut self) {
        self.garbage
            .extend(self.cache.values().filter_map(|e| e.kitty_id));
        self.cache.clear();
        self.flush_cleanup();
    }
    pub fn animation_area(&self, area: Rect) -> Rect {
        let (fw, fh) = self
            .worker
            .as_ref()
            .map_or((8, 16), |w| (w.font.width, w.font.height));
        let w = area.width.min(256 / fw.max(1));
        let h = area.height.min(256 / fh.max(1));
        Rect::new(
            area.x + (area.width - w) / 2,
            area.y + (area.height - h) / 2,
            w,
            h,
        )
    }
    #[cfg(test)]
    pub fn render(&mut self, avatar: &Arc<Avatar>, mode: AvatarStyle, f: &mut Frame, area: Rect) {
        self.render_avatar(avatar, mode, false, false, f, area);
    }
    pub fn render_avatar(
        &mut self,
        avatar: &Arc<Avatar>,
        mode: AvatarStyle,
        smooth: bool,
        round: bool,
        f: &mut Frame,
        area: Rect,
    ) {
        if mode == AvatarStyle::Pixel
            && self.native(Source::Avatar(avatar.clone()), smooth, round, f, area)
        {
            return;
        }
        f.render_widget(Portrait { avatar, mode }.rounded(round), area);
    }
    pub fn render_image(&mut self, image: &Arc<image::RgbaImage>, f: &mut Frame, area: Rect) {
        if !self.native(Source::Image(image.clone()), true, false, f, area) {
            f.render_widget(
                Raster {
                    image,
                    round: false,
                },
                area,
            );
        }
    }
    // Queue every frame before playback. Touching the whole set also keeps it
    // resident while an animation is playing; no per-frame quality fallback.
    pub fn prepare_images(&mut self, frames: &[crate::media::Frame], area: Rect) -> usize {
        let Some(worker) = &self.worker else {
            return frames.len();
        };
        let w = area.width.min(1024 / worker.font.width.max(1)).min(240);
        let h = area.height.min(1024 / worker.font.height.max(1)).min(240);
        if w == 0 || h == 0 {
            return frames.len();
        }
        let mut ready = 0;
        for frame in frames {
            let source = Source::Image(frame.image.clone());
            let key = (source.ptr(), w, h, 5);
            self.request(key, source);
            if let Some(entry) = self.cache.get_mut(&key) {
                entry.last_used = self.frame;
                ready += usize::from(entry.completed);
            }
        }
        ready
    }
    fn request(&mut self, key: Key, source: Source) {
        let Some(worker) = &self.worker else { return };
        if !self.cache.contains_key(&key) && self.cache.len() < 80 {
            self.next_id = self.next_id.wrapping_add(1).max(1);
            if worker
                .requests
                .try_send((key, source.clone(), self.next_id))
                .is_ok()
            {
                let retained = source.retained_bytes();
                self.cache.insert(
                    key,
                    Entry {
                        _source: source,
                        image: None,
                        completed: false,
                        last_used: self.frame,
                        kitty_id: worker.kitty.then_some(self.next_id),
                        bytes: key.1 as usize
                            * key.2 as usize
                            * worker.font.width as usize
                            * worker.font.height as usize
                            * 6
                            + retained,
                    },
                );
            }
        }
    }
    fn native(
        &mut self,
        source: Source,
        smooth: bool,
        round: bool,
        f: &mut Frame,
        area: Rect,
    ) -> bool {
        if area.width == 0 || area.height == 0 {
            return false;
        }
        let Some(worker) = &self.worker else {
            return false;
        };
        // Kitty placeholders have a finite diacritic coordinate table too.
        let w = area.width.min(1024 / worker.font.width.max(1)).min(240);
        let h = area.height.min(1024 / worker.font.height.max(1)).min(240);
        let area = Rect::new(
            area.x + (area.width - w) / 2,
            area.y + (area.height - h) / 2,
            w,
            h,
        );
        if area.is_empty() {
            return false;
        }
        let flags = u8::from(smooth)
            | u8::from(round) * 2
            | if matches!(source, Source::Image(_)) {
                4
            } else {
                0
            };
        let key = (source.ptr(), area.width, area.height, flags);
        self.request(key, source);
        if let Some(entry) = self.cache.get_mut(&key) {
            entry.last_used = self.frame;
            if let Some(image) = &entry.image {
                let size = image.size();
                if size.width <= area.width && size.height <= area.height {
                    f.render_widget(
                        Image::new(image),
                        Rect::new(
                            area.x + (area.width - size.width) / 2,
                            area.y + (area.height - size.height) / 2,
                            size.width,
                            size.height,
                        ),
                    );
                    return true;
                }
            }
        }
        false
    }
}

pub fn round_image(image: &mut image::RgbaImage) {
    let (w, h) = (image.width() as f32, image.height() as f32);
    for (x, y, p) in image.enumerate_pixels_mut() {
        let radius = ((x as f32 + 0.5 - w / 2.0) / (w / 2.0)).powi(2)
            + ((y as f32 + 0.5 - h / 2.0) / (h / 2.0)).powi(2);
        if radius > 1.0 {
            p[3] = 0;
        }
    }
}

pub struct Raster<'a> {
    pub image: &'a image::RgbaImage,
    pub round: bool,
}
impl ratatui::widgets::Widget for Raster<'_> {
    fn render(self, area: Rect, buffer: &mut ratatui::buffer::Buffer) {
        use ratatui::style::{Color, Style};
        let (w, h) = self.image.dimensions();
        if w == 0 || h == 0 || area.is_empty() {
            return;
        }
        let scale = (area.width as f64 / w as f64).min(area.height as f64 * 2.0 / h as f64);
        let width = ((w as f64 * scale).floor() as u16).max(1).min(area.width);
        let height = ((h as f64 * scale / 2.0).ceil() as u16)
            .max(1)
            .min(area.height);
        let left = area.x + (area.width - width) / 2;
        let top = area.y + (area.height - height) / 2;
        let color = |x: u16, y: u16| {
            let sx = (x as u32 * w / width as u32).min(w - 1);
            let sy = (y as u32 * h / (height as u32 * 2)).min(h - 1);
            let p = self.image.get_pixel(sx, sy);
            let circle = ((sx as f32 + 0.5) / w as f32 * 2.0 - 1.0).powi(2)
                + ((sy as f32 + 0.5) / h as f32 * 2.0 - 1.0).powi(2);
            (p[3] >= 128 && (!self.round || circle <= 1.0)).then_some(Color::Rgb(p[0], p[1], p[2]))
        };
        for y in 0..height {
            for x in 0..width {
                let cell = &mut buffer[(left + x, top + y)];
                match (color(x, y * 2), color(x, y * 2 + 1)) {
                    (Some(a), Some(b)) => {
                        cell.set_symbol("▀").set_style(Style::new().fg(a).bg(b));
                    }
                    (Some(a), None) => {
                        cell.set_symbol("▀").set_style(Style::new().fg(a));
                    }
                    (None, Some(b)) => {
                        cell.set_symbol("▄").set_style(Style::new().fg(b));
                    }
                    _ => {
                        cell.set_symbol(" ").set_style(Style::default());
                    }
                }
            }
        }
    }
}

// Do not use Picker::from_query_stdio here: its timed-out reader keeps running
// and can steal subsequent keys in terminals that never answer (including PTYs).
// Poll stdin synchronously, once, before crossterm starts reading events.
#[cfg(unix)]
fn detect_picker() -> Option<Picker> {
    use ratatui_image::{
        FontSize,
        picker::cap_parser::{Parser, QueryStdioOptions, Response},
    };
    use std::{
        io::Write,
        time::{Duration, Instant},
    };
    let query = Parser::query(false, QueryStdioOptions::default());
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(query.as_bytes()).ok()?;
    stdout.flush().ok()?;
    drop(stdout);
    let mut parser = Parser::new();
    let mut font = crossterm::terminal::window_size().ok().and_then(|s| {
        (s.columns > 0 && s.rows > 0 && s.width >= s.columns && s.height >= s.rows)
            .then(|| FontSize::new(s.width / s.columns, s.height / s.rows))
    });
    let mut protocol = ProtocolType::Halfblocks;
    let deadline = Instant::now() + Duration::from_millis(250);
    'query: while Instant::now() < deadline {
        let mut fd = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        let wait = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .max(1) as i32;
        // SAFETY: a single initialized pollfd, and read writes only into this array.
        if unsafe { libc::poll(&mut fd, 1, wait) } <= 0 {
            break;
        }
        if fd.revents & libc::POLLIN == 0 {
            break;
        }
        let mut bytes = [0u8; 256];
        let count =
            unsafe { libc::read(libc::STDIN_FILENO, bytes.as_mut_ptr().cast(), bytes.len()) };
        if count <= 0 {
            break;
        }
        for &byte in &bytes[..count as usize] {
            for response in parser.push(char::from(byte)) {
                match response {
                    Response::Kitty => protocol = ProtocolType::Kitty,
                    Response::Sixel if protocol != ProtocolType::Kitty => {
                        protocol = ProtocolType::Sixel
                    }
                    Response::CellSize(Some((w, h))) => font = Some(FontSize::new(w, h)),
                    Response::Status => break 'query,
                    _ => {}
                }
            }
        }
    }
    let font = font?;
    if font.width > 128 || font.height > 256 {
        return None;
    }
    let program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    if matches!(program.as_str(), "WezTerm" | "iTerm.app" | "mintty") {
        protocol = ProtocolType::Iterm2;
    } else if std::env::var_os("KONSOLE_VERSION").is_some() {
        return None; // Kitty placeholders are not supported correctly there.
    }
    if protocol == ProtocolType::Halfblocks {
        return None;
    }
    #[allow(deprecated)]
    let mut picker = Picker::from_fontsize(font);
    picker.set_protocol_type(protocol);
    Some(picker)
}

#[cfg(not(unix))]
fn detect_picker() -> Option<Picker> {
    // Console keyboard input on Windows must stay owned by crossterm.
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    #[test]
    fn animation_prepares_every_native_frame_before_playback_and_reprepares_on_resize() {
        let frames: Vec<_> = (0..32)
            .map(|i| crate::media::Frame {
                image: Arc::new(image::RgbaImage::from_pixel(
                    64,
                    64,
                    image::Rgba([i, 30, 90, 255]),
                )),
                delay: std::time::Duration::from_millis(100),
            })
            .collect();
        let mut picker = Picker::halfblocks();
        picker.set_protocol_type(ProtocolType::Kitty);
        let mut thumbs = Thumbnails::default();
        thumbs.configure(picker);
        let mut terminal = Terminal::new(TestBackend::new(24, 12)).unwrap();
        for area in [Rect::new(0, 0, 16, 8), Rect::new(0, 0, 24, 12)] {
            assert_eq!(thumbs.prepare_images(&frames, area), 0);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                thumbs.begin_frame();
                if thumbs.prepare_images(&frames, area) == frames.len() {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "animation never became ready"
                );
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            for frame in &frames {
                terminal
                    .draw(|f| thumbs.render_image(&frame.image, f, area))
                    .unwrap();
                assert!(
                    terminal
                        .backend()
                        .buffer()
                        .content
                        .iter()
                        .any(|c| c.symbol().contains('\u{10eeee}')),
                    "a preloaded GIF frame fell back to coarse blocks"
                );
            }
        }
        assert_eq!(
            Thumbnails::default().prepare_images(&frames, Rect::new(0, 0, 16, 8)),
            frames.len()
        );
    }
    fn prepared(
        thumbs: &mut Thumbnails,
        terminal: &mut Terminal<TestBackend>,
        avatar: &Arc<Avatar>,
        size: Size,
        round: bool,
    ) -> Vec<u8> {
        let rect = Rect::new(0, 0, size.width, size.height);
        terminal
            .draw(|f| {
                thumbs.render_avatar(avatar, AvatarStyle::Pixel, avatar.is_full(), round, f, rect)
            })
            .unwrap();
        let (key, image) = thumbs
            .worker
            .as_ref()
            .unwrap()
            .results
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap();
        thumbs.cache.get_mut(&key).unwrap().image = image;
        terminal
            .draw(|f| {
                thumbs.render_avatar(avatar, AvatarStyle::Pixel, avatar.is_full(), round, f, rect)
            })
            .unwrap();
        let symbol = terminal.backend().buffer()[(0, 0)].symbol();
        let mut raw = Vec::new();
        for segment in symbol.split("\x1b_G").skip(1) {
            let Some((_, payload)) = segment.split_once(';') else {
                continue;
            };
            let payload = payload.split("\x1b\\").next().unwrap();
            raw.extend(base64_simd::STANDARD.decode_to_vec(payload).unwrap());
        }
        raw
    }
    #[test]
    fn thumbnails_and_profiles_keep_the_same_pixel_grid_and_rounding_is_optional() {
        let original = Avatar::full_color(image::RgbaImage::from_fn(32, 32, |x, y| {
            image::Rgba([x as u8 * 7, y as u8 * 7, 80, 255])
        }));
        let avatar = Arc::new(original.resized(8));
        let mut picker = Picker::halfblocks();
        picker.set_protocol_type(ProtocolType::Kitty);
        let mut thumbs = Thumbnails::default();
        thumbs.configure(picker);
        let mut terminal = Terminal::new(TestBackend::new(32, 16)).unwrap();
        let small = prepared(&mut thumbs, &mut terminal, &avatar, Size::new(8, 4), false);
        let large = prepared(
            &mut thumbs,
            &mut terminal,
            &avatar,
            Size::new(32, 16),
            false,
        );
        assert_eq!(small.len(), 80 * 80 * 4);
        assert_eq!(large.len(), 320 * 320 * 4);
        for y in 0..8 {
            for x in 0..8 {
                let a = (y * 10 * 80 + x * 10) * 4;
                let b = (y * 40 * 320 + x * 40) * 4;
                assert_eq!(&small[a..a + 4], &large[b..b + 4]);
            }
        }
        assert_eq!(small[3], 255);
        let rounded = prepared(&mut thumbs, &mut terminal, &avatar, Size::new(8, 4), true);
        assert_eq!(rounded[3], 0);
        assert_eq!(rounded[(40 * 80 + 40) * 4 + 3], 255);
        let full = Arc::new(original);
        let smooth = prepared(&mut thumbs, &mut terminal, &full, Size::new(8, 4), false);
        let colours: std::collections::HashSet<_> = smooth.chunks_exact(4).collect();
        assert!(
            colours.len() > 32,
            "maximum quality still quantizes colours"
        );
    }
    #[test]
    fn opening_a_profile_keeps_uncovered_thumbnails_native_and_media_cleanup_preserves_avatars() {
        let mut picker = Picker::halfblocks();
        picker.set_protocol_type(ProtocolType::Kitty);
        let mut ui = crate::ui::Ui::default();
        ui.thumbnails.configure(picker);
        let mut app = crate::app::App::for_test(crate::config::Config::default());
        app.screen = crate::app::Screen::Call;
        let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
        terminal
            .draw(|f| crate::ui::render(&app, f, &mut ui))
            .unwrap();
        for _ in 0..2 {
            let (key, image) = ui
                .thumbnails
                .worker
                .as_ref()
                .unwrap()
                .results
                .recv_timeout(std::time::Duration::from_secs(3))
                .unwrap();
            ui.thumbnails.cache.get_mut(&key).unwrap().image = image;
        }
        terminal
            .draw(|f| crate::ui::render(&app, f, &mut ui))
            .unwrap();
        app.overlay = Some(crate::action::Overlay::Profile);
        terminal
            .draw(|f| crate::ui::render(&app, f, &mut ui))
            .unwrap();
        assert!(
            terminal.backend().buffer()[(3, 2)]
                .symbol()
                .contains('\u{10eeee}')
        );
        let image = Arc::new(image::RgbaImage::from_pixel(
            8,
            8,
            image::Rgba([1, 2, 3, 255]),
        ));
        terminal
            .draw(|f| ui.thumbnails.render_image(&image, f, Rect::new(0, 0, 8, 4)))
            .unwrap();
        assert!(ui.thumbnails.cache.keys().any(|k| k.3 & 4 != 0));
        ui.thumbnails.clear_images();
        assert!(ui.thumbnails.cache.keys().all(|k| k.3 & 4 == 0));
        assert!(!ui.thumbnails.cache.is_empty());
        assert!(!ui.thumbnails.garbage.is_empty());
    }
    #[test]
    fn a_modal_cannot_consume_an_images_first_transmission() {
        let mut picker = Picker::halfblocks();
        picker.set_protocol_type(ProtocolType::Kitty);
        let mut ui = crate::ui::Ui::default();
        ui.thumbnails.configure(picker);
        let mut app = crate::app::App::for_test(crate::config::Config::default());
        app.screen = crate::app::Screen::Call;
        let mut terminal = Terminal::new(TestBackend::new(40, 24)).unwrap();
        // Queue graphics, then open a modal before background encoding finishes.
        terminal
            .draw(|f| crate::ui::render(&app, f, &mut ui))
            .unwrap();
        for _ in 0..2 {
            let (key, image) = ui
                .thumbnails
                .worker
                .as_ref()
                .unwrap()
                .results
                .recv_timeout(std::time::Duration::from_secs(3))
                .unwrap();
            ui.thumbnails.cache.get_mut(&key).unwrap().image = image;
        }
        app.overlay = Some(crate::action::Overlay::Profile);
        terminal
            .draw(|f| crate::ui::render(&app, f, &mut ui))
            .unwrap();
        app.overlay = None;
        terminal
            .draw(|f| crate::ui::render(&app, f, &mut ui))
            .unwrap();
        assert!(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .any(|c| c.symbol().contains("a=T,U=1")),
            "closing the profile lost the image's only transmission"
        );
    }
    #[test]
    fn native_thumbnails_keep_more_detail_inside_the_same_small_rectangle() {
        let mut picker = Picker::halfblocks();
        picker.set_protocol_type(ProtocolType::Kitty);
        let mut thumbnails = Thumbnails::default();
        thumbnails.configure(picker);
        let base = Avatar::identicon("portrait");
        let mut terminal = Terminal::new(TestBackend::new(20, 10)).unwrap();
        for resolution in [8, 32, 128, 256] {
            let avatar = Arc::new(base.resized(resolution));
            let area = Rect::new(2, 2, 8, 4);
            let key = (Arc::as_ptr(&avatar) as usize, 8, 4, 0);
            terminal
                .draw(|f| thumbnails.render(&avatar, AvatarStyle::Pixel, f, area))
                .unwrap();
            let (received, protocol) = thumbnails
                .worker
                .as_ref()
                .unwrap()
                .results
                .recv_timeout(std::time::Duration::from_secs(3))
                .unwrap();
            assert_eq!(received, key);
            let protocol = protocol.unwrap();
            assert_eq!(protocol.size(), Size::new(8, 4));
            thumbnails.cache.get_mut(&key).unwrap().image = Some(protocol);
            terminal
                .draw(|f| thumbnails.render(&avatar, AvatarStyle::Pixel, f, area))
                .unwrap();
            assert!(
                terminal.backend().buffer()[(9, 5)]
                    .symbol()
                    .contains('\u{10eeee}')
            );
            assert_eq!(terminal.backend().buffer()[(10, 5)].symbol(), " ");
            assert_eq!(terminal.backend().buffer()[(9, 6)].symbol(), " ");
        }
    }
}
