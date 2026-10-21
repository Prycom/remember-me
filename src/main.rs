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

mod action;
mod app;
mod audio;
mod avatar;
mod chat_links;
mod clipboard;
mod config;
mod group;
mod net;
mod noise_suppress;
mod proto;
mod transfer;
mod ui;
mod update;
mod voice_level;

use std::time::{Duration, Instant};

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
    /// скачать и атомарно установить обновление из GitHub Release test
    #[argh(switch)]
    update: bool,

    /// версия, время сборки и платформа, без звука и UI
    #[argh(switch)]
    build_info: bool,

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
    if opts.build_info {
        print!("{}", update::build_info());
        return;
    }
    if opts.update {
        match update::run() {
            Ok(Some(message)) => println!("{message}"),
            Ok(None) => println!("установлена актуальная сборка"),
            Err(error) => {
                eprintln!("обновление: {error}");
                std::process::exit(1);
            }
        }
        return;
    }

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
    let audio_pump = tokio::spawn(engine.clone().forward_encoded(encoded_rx));
    let mut app = App::new(cfg, engine);
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
        crossterm::event::EnableMouseCapture,
        crossterm::event::EnableBracketedPaste,
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
        )
    );

    let mut ui = crate::ui::Ui::default();
    let mut res: std::io::Result<()> = Ok(());
    let frame_period = Duration::from_millis(33);
    let mut next_draw = Instant::now();
    loop {
        app.poll();
        app.tick_toast();

        // Bound event draining: holding a key / moving the mouse must not
        // postpone polling and drawing indefinitely. Render at most 30 FPS.
        for _ in 0..64 {
            match crossterm::event::poll(next_draw.saturating_duration_since(Instant::now())) {
                Ok(true) => {}
                Ok(false) => break,
                Err(error) => {
                    res = Err(error);
                    break;
                }
            }
            match crossterm::event::read() {
                Ok(ev) => {
                    if let crossterm::event::Event::Mouse(mouse) = ev {
                        if let Some(action) = ui.mouse(mouse) {
                            app.on_action(action);
                        }
                    } else {
                        app.on_event(ev);
                    }
                    if app.quit {
                        break;
                    }
                }
                Err(e) => {
                    res = Err(e);
                    break;
                }
            }
            if Instant::now() >= next_draw {
                break;
            }
        }
        if app.quit || res.is_err() {
            break;
        }

        if Instant::now() >= next_draw {
            let draw_started = Instant::now();
            res = term.draw(|f| ui::render(&app, f, &mut ui)).map(|_| ());
            if draw_started.elapsed() > Duration::from_millis(100)
                && std::env::var("RM_DEBUG").ok().as_deref() == Some("1")
            {
                eprintln!(
                    "[ui] slow terminal/render: {} ms",
                    draw_started.elapsed().as_millis()
                );
            }
            next_draw = Instant::now() + frame_period;
            if res.is_err() {
                break;
            }
        }
    }

    let _ = crossterm::execute!(
        std::io::stdout(),
        PopKeyboardEnhancementFlags,
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableBracketedPaste
    );
    ratatui::restore();
    app.stop_session();
    audio_pump.abort();
    if let Err(e) = res {
        eprintln!("терминал: {e}");
    }
}
