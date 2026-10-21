//! Bounded, indexed portraits. Image decoding/quantization happens once on a
//! background thread; peers receive pixels, never an image decoder workload.
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    widgets::Widget,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

pub const SIDE: usize = 32;
pub const COLORS: usize = 32;
pub const RESOLUTIONS: [u16; 6] = [16, 32, 48, 64, 96, 128];
pub const MAX_SIDE: usize = 128;
pub const MAX_BYTES: usize = COLORS * 3 + MAX_SIDE * MAX_SIDE;
pub const CHUNK_BYTES: usize = 1024;
pub const MAX_CHUNKS: usize = MAX_BYTES.div_ceil(CHUNK_BYTES);

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Avatar {
    palette: Vec<[u8; 3]>,
    pixels: Vec<u8>,
}
impl std::fmt::Debug for Avatar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Avatar({0}×{0})", self.side())
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AvatarStyle {
    #[default]
    Pixel,
    Ascii,
}

impl Avatar {
    pub fn side(&self) -> usize {
        RESOLUTIONS
            .into_iter()
            .map(usize::from)
            .find(|n| n * n == self.pixels.len())
            .unwrap_or(0)
    }
    pub fn valid(&self) -> bool {
        self.palette.len() == COLORS
            && self.side() != 0
            && self.pixels.iter().all(|p| (*p as usize) < COLORS)
    }
    pub fn encode(&self) -> Vec<u8> {
        self.palette
            .iter()
            .flatten()
            .copied()
            .chain(self.pixels.iter().copied())
            .collect()
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if !RESOLUTIONS
            .iter()
            .any(|&n| bytes.len() == COLORS * 3 + n as usize * n as usize)
        {
            return None;
        }
        let value = Self {
            palette: bytes[..COLORS * 3]
                .chunks_exact(3)
                .map(|p| [p[0], p[1], p[2]])
                .collect(),
            pixels: bytes[COLORS * 3..].to_vec(),
        };
        value.valid().then_some(value)
    }
    #[cfg(test)]
    pub fn load(path: &Path) -> Result<Self, String> {
        Self::load_at(path, SIDE as u16)
    }
    pub fn load_at(path: &Path, resolution: u16) -> Result<Self, String> {
        if !RESOLUTIONS.contains(&resolution) {
            return Err("размер аватарки: 16, 32, 48, 64, 96 или 128".into());
        }
        let metadata = std::fs::metadata(path).map_err(|e| format!("картинка: {e}"))?;
        if !metadata.is_file() {
            return Err("нужен обычный файл PNG/JPEG/WebP/GIF".into());
        }
        if metadata.len() > 16 * 1024 * 1024 {
            return Err("картинка больше 16 МиБ".into());
        }
        let mut reader = image::ImageReader::open(path)
            .map_err(|e| format!("картинка: {e}"))?
            .with_guessed_format()
            .map_err(|e| e.to_string())?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(4096);
        limits.max_image_height = Some(4096);
        limits.max_alloc = Some(96 * 1024 * 1024);
        reader.limits(limits.clone());
        use image::ImageDecoder;
        let mut decoder = reader
            .into_decoder()
            .map_err(|e| format!("PNG/JPEG/WebP/GIF: {e}"))?;
        let orientation = decoder
            .orientation()
            .unwrap_or(image::metadata::Orientation::NoTransforms);
        // Match ImageReader::decode's allocation accounting before decoding.
        limits
            .reserve(decoder.total_bytes())
            .map_err(|e| e.to_string())?;
        decoder.set_limits(limits).map_err(|e| e.to_string())?;
        let mut image = image::DynamicImage::from_decoder(decoder)
            .map_err(|e| format!("PNG/JPEG/WebP/GIF: {e}"))?;
        image.apply_orientation(orientation);
        let side = image.width().min(image.height());
        if side == 0 {
            return Err("пустая картинка".into());
        }
        // Crop first: an extremely wide image must not create a huge resize intermediate.
        let square = image
            .crop_imm(
                (image.width() - side) / 2,
                (image.height() - side) / 2,
                side,
                side,
            )
            .resize_exact(
                resolution as u32,
                resolution as u32,
                image::imageops::FilterType::Lanczos3,
            )
            .into_rgba8();
        Ok(Self::quantize(square.as_raw()))
    }
    /// Rebuild from the cached 128px source; never repeatedly resize the thumbnail.
    pub fn resized(&self, resolution: u16) -> Self {
        assert!(self.valid() && RESOLUTIONS.contains(&resolution));
        if self.side() == resolution as usize {
            return self.clone();
        }
        let rgba = self
            .pixels
            .iter()
            .flat_map(|&i| {
                let [r, g, b] = self.palette[i as usize];
                [r, g, b, if i == 0 { 0 } else { 255 }]
            })
            .collect();
        let image =
            image::RgbaImage::from_raw(self.side() as u32, self.side() as u32, rgba).unwrap();
        let scaled = image::imageops::resize(
            &image,
            resolution as u32,
            resolution as u32,
            image::imageops::FilterType::Lanczos3,
        );
        Self::quantize(scaled.as_raw())
    }
    fn quantize(rgba: &[u8]) -> Self {
        let colors = rgba
            .chunks_exact(4)
            .map(|p| {
                if p[3] < 24 {
                    None
                } else {
                    Some(std::array::from_fn(|c| {
                        ((p[c] as u32 * p[3] as u32 + [18, 22, 29][c] * (255 - p[3] as u32)) / 255)
                            as u8
                    }))
                }
            })
            .collect::<Vec<_>>();
        let opaque = colors.iter().flatten().copied().collect::<Vec<_>>();
        let mut centers = vec![[0; 3]; COLORS];
        if let Some(first) = opaque.first() {
            centers[1] = *first;
            let mut distance = vec![u32::MAX; opaque.len()];
            for index in 2..COLORS {
                for (pixel, best) in opaque.iter().zip(&mut distance) {
                    *best = (*best).min(dist(*pixel, centers[index - 1]));
                }
                let next = distance
                    .iter()
                    .enumerate()
                    .max_by_key(|(_, d)| *d)
                    .unwrap()
                    .0;
                centers[index] = opaque[next];
            }
            for _ in 0..10 {
                let mut sums = [[0u32; 3]; COLORS];
                let mut count = [0u32; COLORS];
                for pixel in &opaque {
                    let i = nearest(*pixel, &centers);
                    count[i] += 1;
                    for c in 0..3 {
                        sums[i][c] += pixel[c] as u32;
                    }
                }
                for i in 1..COLORS {
                    if count[i] > 0 {
                        centers[i] = sums[i].map(|s| (s / count[i]) as u8);
                    }
                }
            }
        }
        let pixels = colors
            .into_iter()
            .map(|p| p.map_or(0, |p| nearest(p, &centers) as u8))
            .collect();
        Self {
            palette: centers,
            pixels,
        }
    }
    pub fn identicon(name: &str) -> Self {
        let hash = Sha256::digest(name.as_bytes());
        let mut palette = vec![[0; 3]; COLORS];
        palette[1] = [20, 28, 40];
        palette[2] = [85 + hash[0] / 2, 115 + hash[1] / 2, 140 + hash[2] / 3];
        palette[3] = [120 + hash[2] / 2, 155 + hash[3] / 3, 170 + hash[4] / 3];
        let mut pixels = vec![1; SIDE * SIDE];
        for y in 0..5 {
            for x in 0..5 {
                let bit = y * 3 + x.min(4 - x);
                if hash[5 + bit / 8] & (1 << (bit % 8)) != 0 {
                    for dy in 0..5 {
                        for dx in 0..5 {
                            pixels[(3 + y * 5 + dy) * SIDE + 3 + x * 5 + dx] =
                                2 + ((x + y) % 2) as u8;
                        }
                    }
                }
            }
        }
        Self { palette, pixels }
    }
    fn color(&self, x: usize, y: usize) -> Option<[u8; 3]> {
        let side = self.side();
        let radius = (side * 6 / 32) as i32;
        let dx = (radius - x as i32)
            .max(x as i32 - (side as i32 - radius - 1))
            .max(0);
        let dy = (radius - y as i32)
            .max(y as i32 - (side as i32 - radius - 1))
            .max(0);
        if dx * dx + dy * dy > radius * radius {
            return None;
        }
        let index = self.pixels[y * side + x];
        (index != 0).then(|| self.palette[index as usize])
    }
}
fn dist(a: [u8; 3], b: [u8; 3]) -> u32 {
    (0..3)
        .map(|c| {
            let d = a[c] as i32 - b[c] as i32;
            (d * d) as u32 * [2, 3, 1][c]
        })
        .sum()
}
fn nearest(pixel: [u8; 3], palette: &[[u8; 3]]) -> usize {
    (1..COLORS)
        .min_by_key(|&i| dist(pixel, palette[i]))
        .unwrap()
}

pub fn display(avatar: Option<&Avatar>, name: &str) -> Arc<Avatar> {
    Arc::new(
        avatar
            .filter(|a| a.valid())
            .cloned()
            .unwrap_or_else(|| Avatar::identicon(name)),
    )
}

pub struct Portrait<'a> {
    pub avatar: &'a Avatar,
    pub mode: AvatarStyle,
}
impl Widget for Portrait<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let width = area
            .width
            .min(area.height.saturating_mul(2))
            .min(self.avatar.side() as u16)
            / 2
            * 2;
        if width == 0 {
            return;
        }
        let height = width / 2;
        let left = area.x + (area.width - width) / 2;
        let top = area.y + (area.height - height) / 2;
        for y in 0..height {
            for x in 0..width {
                let upper = self.avatar.color(
                    x as usize * self.avatar.side() / width as usize,
                    y as usize * 2 * self.avatar.side() / (height as usize * 2),
                );
                let lower = self.avatar.color(
                    x as usize * self.avatar.side() / width as usize,
                    (y as usize * 2 + 1) * self.avatar.side() / (height as usize * 2),
                );
                let cell = &mut buffer[(left + x, top + y)];
                if self.mode == AvatarStyle::Ascii {
                    let rgb = match (upper, lower) {
                        (Some(a), Some(b)) => {
                            std::array::from_fn(|c| ((a[c] as u16 + b[c] as u16) / 2) as u8)
                        }
                        (Some(a), _) | (_, Some(a)) => a,
                        _ => {
                            cell.set_symbol(" ").set_style(Style::default());
                            continue;
                        }
                    };
                    let luma =
                        (54 * rgb[0] as usize + 183 * rgb[1] as usize + 19 * rgb[2] as usize) / 256;
                    let ramp = b" .:-=+*#%@";
                    let symbol =
                        std::str::from_utf8(&ramp[(luma * 9 / 255)..=(luma * 9 / 255)]).unwrap();
                    cell.set_symbol(symbol)
                        .set_style(Style::new().fg(Color::Gray));
                } else {
                    let color = |rgb: [u8; 3]| Color::Rgb(rgb[0], rgb[1], rgb[2]);
                    match (upper, lower) {
                        (Some(a), Some(b)) => {
                            cell.set_symbol("▀")
                                .set_style(Style::new().fg(color(a)).bg(color(b)));
                        }
                        (Some(a), None) => {
                            cell.set_symbol("▀").set_style(Style::new().fg(color(a)));
                        }
                        (None, Some(b)) => {
                            cell.set_symbol("▄").set_style(Style::new().fg(color(b)));
                        }
                        _ => {
                            cell.set_symbol(" ").set_style(Style::default());
                        }
                    }
                }
            }
        }
    }
}

/// Each peer acknowledges a revision. Stop after bounded retries, including
/// when talking to a client that predates avatars. No timer/traffic once ACKed.
#[derive(Default)]
pub struct Delivery {
    revision: u64,
    acked: bool,
    tries: u8,
    sent: Option<Instant>,
    chunk: usize,
}
impl Delivery {
    pub fn due(&mut self, revision: u64, now: Instant) -> bool {
        if self.revision != revision {
            *self = Self {
                revision,
                ..Self::default()
            };
        }
        if self.acked
            || self.tries >= 6
            || self
                .sent
                .is_some_and(|sent| now.duration_since(sent) < Duration::from_secs(2))
        {
            return false;
        }
        self.sent = Some(now);
        self.tries += 1;
        true
    }
    pub fn ack(&mut self, revision: u64) {
        if self.revision == revision {
            self.acked = true;
        }
    }
}
/// A single bounded revision per peer. Partial retries preserve received chunks;
/// a newer revision replaces the assembly, including after avatar reset.
#[derive(Default)]
pub struct Assembly {
    revision: u64,
    chunks: Vec<Option<Vec<u8>>>,
}
impl Assembly {
    pub fn push(
        &mut self,
        revision: u64,
        index: u8,
        count: u8,
        bytes: Vec<u8>,
    ) -> Option<Arc<Avatar>> {
        let (index, count) = (index as usize, count as usize);
        if revision == 0
            || count == 0
            || count > MAX_CHUNKS
            || index >= count
            || bytes.is_empty()
            || bytes.len() > CHUNK_BYTES
            || (index + 1 < count && bytes.len() != CHUNK_BYTES)
        {
            return None;
        }
        if revision < self.revision {
            return None;
        }
        if revision > self.revision {
            self.revision = revision;
            self.chunks = vec![None; count];
        }
        if self.chunks.len() != count {
            return None;
        }
        self.chunks[index] = Some(bytes);
        if self.chunks.iter().any(Option::is_none) {
            return None;
        }
        let bytes = self
            .chunks
            .iter()
            .flat_map(|c| c.as_ref().unwrap().iter().copied())
            .collect::<Vec<_>>();
        Avatar::decode(&bytes).map(Arc::new)
    }
}

pub struct Profile {
    pub revision: u64,
    pub avatar: Option<Arc<Avatar>>,
    bytes: Vec<u8>,
}
impl Profile {
    pub fn new(avatar: Option<&Avatar>) -> Self {
        let avatar = avatar.filter(|a| a.valid()).cloned().map(Arc::new);
        let bytes = avatar.as_ref().map_or_else(Vec::new, |a| a.encode());
        Self {
            revision: 1,
            avatar,
            bytes,
        }
    }
    /// At most one small packet per maintenance tick; cached bytes avoid image
    /// work in the voice loop. One ACK covers the completed image revision.
    pub fn next(&self, delivery: &mut Delivery, now: Instant) -> Option<crate::proto::Packet> {
        if delivery.revision != self.revision {
            *delivery = Delivery {
                revision: self.revision,
                ..Default::default()
            };
        }
        if delivery.acked {
            return None;
        }
        let count = self.bytes.len().div_ceil(CHUNK_BYTES);
        if self.bytes.len() <= COLORS * 3 + SIDE * SIDE {
            return delivery
                .due(self.revision, now)
                .then(|| crate::proto::Packet::Avatar {
                    revision: self.revision,
                    avatar: self.avatar.clone(),
                });
        }
        if delivery.chunk == 0 && !delivery.due(self.revision, now) {
            return None;
        }
        let index = delivery.chunk;
        let bytes = self.bytes
            [index * CHUNK_BYTES..((index + 1) * CHUNK_BYTES).min(self.bytes.len())]
            .to_vec();
        delivery.chunk = (index + 1) % count;
        Some(crate::proto::Packet::AvatarChunk {
            revision: self.revision,
            index: index as u8,
            count: count as u8,
            bytes,
        })
    }
    pub fn set(&mut self, avatar: Option<Arc<Avatar>>) {
        self.revision = self.revision.wrapping_add(1).max(1);
        self.bytes = avatar.as_ref().map_or_else(Vec::new, |a| a.encode());
        self.avatar = avatar;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wire_roundtrip_transparency_and_bounds() {
        let avatar = Avatar::identicon("Лексей");
        assert_eq!(avatar.encode().len(), 1120);
        assert_eq!(Avatar::decode(&avatar.encode()), Some(avatar.clone()));
        assert_ne!(avatar, Avatar::identicon("Друг"));
        let mut bytes = avatar.encode();
        bytes[96] = 255;
        assert!(Avatar::decode(&bytes).is_none());
        assert!(Avatar::decode(&bytes[..bytes.len() - 1]).is_none());
        let transparent = Avatar::quantize(&vec![0; SIDE * SIDE * 4]);
        assert!(transparent.valid());
        assert!(transparent.pixels.iter().all(|&p| p == 0));
    }
    #[test]
    fn imports_center_crop_and_keeps_owned_pixels_after_source_is_removed() {
        let path =
            std::env::temp_dir().join(format!("remember-me-avatar-{}.png", std::process::id()));
        let mut image = image::RgbaImage::from_pixel(96, 32, image::Rgba([255, 0, 0, 255]));
        for y in 0..32 {
            for x in 32..64 {
                image.put_pixel(x, y, image::Rgba([20, 100, 220, 255]));
            }
        }
        image.save(&path).unwrap();
        let avatar = Avatar::load(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(avatar.valid());
        assert!(
            avatar
                .pixels
                .iter()
                .all(|&i| avatar.palette[i as usize] == [20, 100, 220])
        );
        let config = crate::config::Config {
            avatar: Some(avatar.clone()),
            ..Default::default()
        };
        let saved = toml::to_string(&config).unwrap();
        let restored: crate::config::Config = toml::from_str(&saved).unwrap();
        assert_eq!(restored.avatar, Some(avatar));
    }

    #[test]
    fn smartphone_jpeg_orientation_is_applied_before_cropping() {
        let path =
            std::env::temp_dir().join(format!("remember-me-avatar-{}.jpg", std::process::id()));
        let image = image::RgbImage::from_fn(32, 32, |_, y| {
            if y < 16 {
                image::Rgb([0, 0, 240])
            } else {
                image::Rgb([240, 0, 0])
            }
        });
        image.save(&path).unwrap();
        let jpeg = std::fs::read(&path).unwrap();
        let mut exif =
            b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0".to_vec();
        let mut oriented = vec![0xff, 0xd8, 0xff, 0xe1];
        oriented.extend(((exif.len() + 2) as u16).to_be_bytes());
        oriented.append(&mut exif);
        oriented.extend_from_slice(&jpeg[2..]);
        std::fs::write(&path, oriented).unwrap();
        let avatar = Avatar::load(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let left = avatar.palette[avatar.pixels[16 * SIDE + 8] as usize];
        let right = avatar.palette[avatar.pixels[16 * SIDE + 24] as usize];
        assert!(left[0] > 190 && left[2] < 60, "{left:?}");
        assert!(right[2] > 190 && right[0] < 60, "{right:?}");
    }
    #[test]
    fn pixel_and_ascii_renderers_fit_and_do_not_emit_control_characters() {
        let avatar = Avatar::identicon("test");
        let area = Rect::new(0, 0, 32, 16);
        for mode in [AvatarStyle::Pixel, AvatarStyle::Ascii] {
            let mut buffer = Buffer::empty(area);
            Portrait {
                avatar: &avatar,
                mode,
            }
            .render(area, &mut buffer);
            let symbols = buffer
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(!symbols.chars().any(char::is_control));
            if mode == AvatarStyle::Ascii {
                assert!(symbols.is_ascii());
                assert!(symbols.contains(['#', '+', '*', '=']));
            } else {
                assert!(symbols.contains('▀'));
                assert!(
                    buffer
                        .content
                        .iter()
                        .any(|c| matches!(c.fg, Color::Rgb(..)))
                );
            }
        }
    }
    #[test]
    fn detail_preserves_fine_source_pixels_and_old_configs_still_load() {
        let path =
            std::env::temp_dir().join(format!("remember-me-detail-{}.png", std::process::id()));
        let source = image::RgbaImage::from_fn(128, 128, |x, _| {
            if x % 2 == 0 {
                image::Rgba([240, 20, 20, 255])
            } else {
                image::Rgba([20, 20, 240, 255])
            }
        });
        source.save(&path).unwrap();
        let master = Avatar::load_at(&path, 128).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_ne!(master.color(64, 64), master.color(65, 64));
        let small = master.resized(32);
        // Increasing detail uses the cached source, not the already blurred 32px image.
        assert_ne!(small.resized(128), master);
        for size in RESOLUTIONS {
            let avatar = master.resized(size);
            assert_eq!(avatar.side(), size as usize);
            assert_eq!(Avatar::decode(&avatar.encode()), Some(avatar.clone()));
            let cfg = crate::config::Config {
                avatar: Some(avatar.clone()),
                avatar_original: Some(master.clone()),
                avatar_resolution: size,
                ..Default::default()
            };
            let restored: crate::config::Config =
                toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
            assert_eq!(restored.avatar, Some(avatar));
            assert_eq!(restored.avatar_original, Some(master.clone()));
            assert_eq!(restored.avatar_resolution, size);
        }
        let old: crate::config::Config = toml::from_str("name='old'\n").unwrap();
        assert_eq!(old.avatar_resolution, 32);
        assert!(old.avatar_original.is_none());
        assert!(Avatar::decode(&vec![0; MAX_BYTES + 1]).is_none());
        assert!(Avatar::load_at(Path::new("unused"), 256).is_err());
    }
    #[test]
    fn chunk_sync_survives_data_and_ack_loss_and_stays_within_vpn_mtu() {
        use crate::proto::{Cipher, Packet};
        for size in RESOLUTIONS {
            let image = Avatar::identicon("high detail").resized(size);
            let profile = Profile::new(Some(&image));
            let mut delivery = Delivery::default();
            let mut assembly = Assembly::default();
            let mut tx = Cipher::new(&[5; 32], &[7; 32], true);
            let rx = Cipher::new(&[5; 32], &[7; 32], false);
            let (mut control, mut audio) = (0, 0);
            let start = Instant::now();
            let (mut lost_data, mut completions) = (false, 0);
            for tick in 0..250 {
                let now = start + Duration::from_millis(tick * 25);
                let Some(packet) = profile.next(&mut delivery, now) else {
                    continue;
                };
                let wire = Packet::encrypt_wire(&mut tx, &packet);
                assert!(wire.len() + 17 + 28 <= 1280);
                if !lost_data
                    && matches!(
                        &packet,
                        Packet::AvatarChunk { index: 1, .. } | Packet::Avatar { .. }
                    )
                {
                    lost_data = true;
                    continue;
                }
                match Packet::decrypt_wire(&rx, &wire, &mut control, &mut audio).unwrap() {
                    Packet::AvatarChunk {
                        revision,
                        index,
                        count,
                        bytes,
                    } => {
                        if let Some(received) = assembly.push(revision, index, count, bytes) {
                            assert_eq!(*received, image);
                            completions += 1;
                        }
                    }
                    Packet::Avatar {
                        avatar: Some(received),
                        ..
                    } => {
                        assert_eq!(*received, image);
                        completions += 1;
                    }
                    _ => panic!("unexpected packet"),
                }
                // Lose the first complete-revision ACK too.
                if completions >= 2 {
                    delivery.ack(profile.revision);
                    break;
                }
            }
            assert!(lost_data && completions >= 2, "{size}px");
            assert!(
                profile
                    .next(&mut delivery, start + Duration::from_secs(20))
                    .is_none()
            );
        }
    }
    #[test]
    fn partial_assembly_is_bounded_and_newer_revision_discards_older_chunks() {
        let image = Avatar::identicon("chunks").resized(128);
        let data = image.encode();
        let chunks = data.chunks(CHUNK_BYTES).map(Vec::from).collect::<Vec<_>>();
        let count = chunks.len() as u8;
        let mut assembly = Assembly::default();
        assert!(assembly.push(1, 0, 255, vec![0; CHUNK_BYTES]).is_none());
        assert!(assembly.chunks.is_empty());
        assert!(
            assembly
                .push(1, 0, count, vec![0; CHUNK_BYTES + 1])
                .is_none()
        );
        assert!(assembly.push(1, 0, count, chunks[0].clone()).is_none());
        assert!(assembly.push(2, 1, count, chunks[1].clone()).is_none());
        assert!(assembly.push(1, 0, count, chunks[0].clone()).is_none());
        for index in (0..chunks.len()).rev().filter(|&i| i != 1) {
            let result = assembly.push(2, index as u8, count, chunks[index].clone());
            if index == 0 {
                assert_eq!(result, Some(Arc::new(image.clone())));
            } else {
                assert!(result.is_none());
            }
        }
    }
    #[test]
    fn sync_retries_loss_stops_after_ack_and_resets_for_new_picture() {
        let now = Instant::now();
        let mut delivery = Delivery::default();
        assert!(delivery.due(1, now));
        assert!(!delivery.due(1, now + Duration::from_millis(20)));
        delivery.ack(0);
        assert!(delivery.due(1, now + Duration::from_secs(2)));
        delivery.ack(1);
        assert!(!delivery.due(1, now + Duration::from_secs(4)));
        assert!(delivery.due(2, now + Duration::from_secs(4)));
        for second in [6, 8, 10, 12, 14] {
            assert!(delivery.due(2, now + Duration::from_secs(second)));
        }
        assert!(!delivery.due(2, now + Duration::from_secs(20)));
    }
}
