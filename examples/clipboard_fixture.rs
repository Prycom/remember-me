//! Test fixture: keep a clipboard image owned on an isolated test display.
use std::{
    borrow::Cow,
    io::{self, Write},
};
fn main() {
    let path = std::env::args_os().nth(1).expect("image path");
    if path == "--gif" {
        let path = std::env::args_os().nth(2).expect("GIF path");
        let mut encoder = image::codecs::gif::GifEncoder::new(std::fs::File::create(path).unwrap());
        encoder
            .set_repeat(image::codecs::gif::Repeat::Infinite)
            .unwrap();
        for i in 0..12u8 {
            let pixels = image::RgbaImage::from_fn(64, 32, |x, y| {
                image::Rgba([i * 20, x as u8 * 3, y as u8 * 7, 255])
            });
            encoder
                .encode_frame(image::Frame::from_parts(
                    pixels,
                    0,
                    0,
                    image::Delay::from_numer_denom_ms(100, 1),
                ))
                .unwrap();
        }
        return;
    }
    let image = image::open(path).unwrap().into_rgba8();
    let (width, height) = image.dimensions();
    let mut clipboard = arboard::Clipboard::new().unwrap();
    clipboard
        .set_image(arboard::ImageData {
            width: width as usize,
            height: height as usize,
            bytes: Cow::Owned(image.into_raw()),
        })
        .unwrap();
    println!("READY");
    io::stdout().flush().unwrap();
    let mut line = String::new();
    let _ = io::stdin().read_line(&mut line);
}
