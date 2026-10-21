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

type Key = (usize, u16, u16);
struct Entry {
    // Keep the Arc alive so allocator pointer reuse cannot return another image.
    _avatar: Arc<Avatar>,
    image: Option<Protocol>,
    last_used: u64,
}
struct Worker {
    requests: mpsc::SyncSender<(Key, Arc<Avatar>)>,
    results: mpsc::Receiver<(Key, Option<Protocol>)>,
}
#[derive(Default)]
pub struct Thumbnails {
    worker: Option<Worker>,
    cache: HashMap<Key, Entry>,
    frame: u64,
}
impl Thumbnails {
    /// Query once, after alternate-screen setup and before keyboard input.
    pub fn detect(&mut self) {
        // The picker changes tmux options when probing; leave those to the user.
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
        let (requests, receive) = mpsc::sync_channel::<(Key, Arc<Avatar>)>(32);
        let (send, results) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("avatar-graphics".into())
            .spawn(move || {
                while let Ok((key, avatar)) = receive.recv() {
                    let image = picker
                        .new_protocol(
                            avatar.rgba().into(),
                            Size::new(key.1, key.2),
                            Resize::Scale(Some(image::imageops::FilterType::Nearest)),
                        )
                        .ok();
                    if send.send((key, image)).is_err() {
                        break;
                    }
                }
            });
        if thread.is_ok() {
            self.worker = Some(Worker { requests, results });
        }
    }
    pub fn begin_frame(&mut self) {
        self.frame += 1;
        if let Some(worker) = &self.worker {
            while let Ok((key, image)) = worker.results.try_recv() {
                if let Some(entry) = self.cache.get_mut(&key) {
                    entry.image = image;
                }
            }
        }
        if self.cache.len() > 48 {
            let frame = self.frame;
            self.cache
                .retain(|_, entry| frame.saturating_sub(entry.last_used) < 2);
        }
    }
    pub fn render(&mut self, avatar: &Arc<Avatar>, mode: AvatarStyle, f: &mut Frame, area: Rect) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        if mode == AvatarStyle::Pixel
            && let Some(worker) = &self.worker
        {
            let key = (Arc::as_ptr(avatar) as usize, area.width, area.height);
            if !self.cache.contains_key(&key)
                && self.cache.len() < 64
                && worker.requests.try_send((key, avatar.clone())).is_ok()
            {
                self.cache.insert(
                    key,
                    Entry {
                        _avatar: avatar.clone(),
                        image: None,
                        last_used: self.frame,
                    },
                );
            }
            if let Some(entry) = self.cache.get_mut(&key) {
                entry.last_used = self.frame;
                if let Some(image) = &entry.image {
                    // Entire encoded image must fit: never crop its right edge.
                    let size = image.size();
                    if size.width <= area.width && size.height <= area.height {
                        let target = Rect::new(
                            area.x + (area.width - size.width) / 2,
                            area.y + (area.height - size.height) / 2,
                            size.width,
                            size.height,
                        );
                        f.render_widget(Image::new(image), target);
                        return;
                    }
                }
            }
        }
        f.render_widget(Portrait { avatar, mode }, area);
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
            let key = (Arc::as_ptr(&avatar) as usize, 8, 4);
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
