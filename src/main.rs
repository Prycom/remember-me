// remember-me — P2P звонок (голос+чат) по мотивам локальных наблюдений REA
// над discord_voice: UDP, X25519→XChaCha20-Poly1305, PTT/VOX, без лишнего.
//
// Запуск:
//   remember-me                    — меню
//   remember-me --host             — сразу ждём звонок (адрес/код на экране)
//   remember-me --connect ip:port  — сразу звоним напрямую
//   remember-me --connect КОД@ip:порт — через ретранслятор
//   remember-me --server [-p порт] — ретранслятор на сервере (headless)
//   [-p порт] [-n имя]             — переопределения настроек

mod app;
mod audio;
mod config;
mod net;
mod noise_suppress;
mod proto;
mod ui;
mod voice_level;

use std::time::Duration;

use argh::FromArgs;
use crossterm::event::{
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use tokio::sync::mpsc;

use app::App;
use audio::AudioEngine;
use config::Config;
use remember_me::server;

#[derive(FromArgs)]
#[argh(description = "remember-me: P2P voice call + text chat")]
struct Opts {
    /// сразу в режим прослушивания (host)
    #[argh(switch, short = 'H')]
    host: bool,

    /// сразу звонить на host ip:порт или КОД@ip:порт (через сервер) ([t]arget)
    #[argh(option)]
    connect: Option<String>,

    /// режим ретранслятора: только сеть, без UI (звонки по коду комнаты)
    #[argh(switch, short = 's')]
    server: bool,

    /// порт прослушивания (переопределяет конфиг)
    #[argh(option, short = 'p')]
    port: Option<u16>,

    /// своё имя (переопределяет конфиг)
    #[argh(option, short = 'n')]
    name: Option<String>,

    /// проверить UDP в обе стороны до обновлённого сервера (ip:порт), без звука и UI
    #[argh(option)]
    check_server: Option<String>,
}

fn main() {
    let opts: Opts = argh::from_env();

    if let Some(target) = opts.check_server {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        if let Err(error) = rt.block_on(server::check_relay(&target)) {
            eprintln!("проверка сервера: {error}");
            std::process::exit(1);
        }
        return;
    }

    if opts.server {
        // ретранслятор — headless, TUI не нужен
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let port = opts.port.unwrap_or(51900);
        if let Err(e) = rt.block_on(server::run_relay(port)) {
            eprintln!("ретранслятор: {e}");
            std::process::exit(1);
        }
        return;
    }

    let mut cfg = Config::load();
    if let Some(p) = opts.port {
        cfg.port = p;
    }
    if let Some(n) = opts.name {
        cfg.name = n;
    }
    // сохранённые имена устройств могли стать мусором (виртуалки/лена),
    // привязка к ним = тишина; чистим ДО открытия звука
    audio::sanitize_config_devices(&mut cfg);

    // --- звук до входа в TUI: если не сможет — честно скажем и выйдем
    let (encoded_tx, encoded_rx) = mpsc::unbounded_channel();
    let engine = match audio::start(&cfg, encoded_tx) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("аудио не открылось: {e}");
            std::process::exit(1);
        }
    };

    let direct: Option<(bool, Option<String>)> = if opts.host {
        Some((true, None))
    } else {
        opts.connect.map(|t| (false, Some(t)))
    };

    // --- рантайм для сетевой задачи
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let _ = rt.block_on(run(cfg, engine, encoded_rx, direct));
}

async fn run(
    cfg: Config,
    engine: std::sync::Arc<AudioEngine>,
    encoded_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    direct: Option<(bool, Option<String>)>,
) {
    let mut app = App::new(cfg, engine, encoded_rx);
    if let Some(err) = config::parse_error() {
        app.status = format!("внимание: конфиг не разобран ({err}) — настройки по умолчанию");
    }

    // CLI-быстрый старт
    if let Some((is_host, target)) = direct {
        if is_host {
            app.start_host();
        } else {
            app.start_connect(target.unwrap());
        }
    }

    let mut term = ratatui::init();
    // kitty-style расширенные события клавиш: нужны для держания-space (PTT);
    // на терминалах без поддержки тихо игнорируется
    let _ = crossterm::execute!(
        std::io::stdout(),
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
        )
    );

    let mut res: std::io::Result<()> = Ok(());
    loop {
        app.poll();
        app.tick_toast();

        // вычитываем всё, что накопилось, в один кадр
        loop {
            match crossterm::event::poll(Duration::from_millis(10)) {
                Ok(true) => {}
                _ => break,
            }
            match crossterm::event::read() {
                Ok(ev) => {
                    app.on_event(ev);
                    if app.quit {
                        break;
                    }
                }
                Err(e) => {
                    res = Err(e);
                    break;
                }
            }
        }
        if app.quit || res.is_err() {
            break;
        }

        res = term.draw(|f| ui::render(&app, f)).map(|_| ());
        if res.is_err() {
            break;
        }
    }

    let _ = crossterm::execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    ratatui::restore();
    app.stop_session();
    if let Err(e) = res {
        eprintln!("терминал: {e}");
    }
}
