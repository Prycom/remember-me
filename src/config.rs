// Сохранение настроек: ~/.config/remember-me/config.toml
// (аналог appSettings.getSync/set в обёртке discord_voice — там audioSubsystem,
// inputVolume, offloadAdmControls и т.п.; здесь — flat TOML).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub name: String,
    pub port: u16,
    /// id выбранного устройства ввода "(дефолт)" / имя устройства
    pub input_device: String,
    pub output_device: String,
    /// громкость микрофона 0..200 (%)
    pub gain_in: u8,
    /// громкость собеседника 0..200 (%)
    pub gain_peer: u8,
    /// PTT (нажми-и-говори) или голосовая активность (VOX) — Discord имеет оба режима
    pub mic_mode: MicMode,
    /// порог VOX 0..100: -70..-30 dBFS (25 = -60 dBFS)
    pub vad_threshold: u8,
    /// Амплитудный гейт, в том числе при удержании PTT; отдельно от шумодава.
    pub noise_gate: bool,
    /// Automatically bring speech to a conversational level before encoding.
    pub auto_gain: bool,
    /// Lightweight 80 Hz–8 kHz microphone filter; no model or worker threads.
    pub voice_filter: bool,
    /// Optional RNNoise suppression of background noise / keyboard sounds.
    pub noise_suppression: bool,
    /// адрес ретранслятора для звонков по коду комнаты ("1.2.3.4:51900")
    pub server_addr: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MicMode {
    Vox,
    Ptt,
}

impl Default for MicMode {
    fn default() -> Self {
        MicMode::Vox
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            name: "user".into(),
            port: 51500,
            input_device: String::new(),
            output_device: String::new(),
            gain_in: 100,
            gain_peer: 100,
            mic_mode: MicMode::Vox,
            vad_threshold: 25,
            noise_gate: false,
            auto_gain: true,
            voice_filter: false,
            noise_suppression: false,
            server_addr: String::new(),
        }
    }
}

/// Причина, по которой конфиг ушёл в дефолт, если такое было.
static PARSE_ERR: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Причина, по которой конфиг ушёл в дефолт, если такое было.
pub fn parse_error() -> Option<&'static String> {
    PARSE_ERR.get()
}

impl Config {
    pub fn path() -> PathBuf {
        let dir = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .ok()
            .filter(|p| !p.as_os_str().is_empty())
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|h| PathBuf::from(h).join(".config"))
            })
            .unwrap_or_else(|| PathBuf::from("."));
        dir.join("remember-me").join("config.toml")
    }

    pub fn load() -> Config {
        match std::fs::read_to_string(Self::path()) {
            Ok(s) => match toml::from_str(&s) {
                Ok(c) => c,
                Err(e) => {
                    let msg = format!("{e}");
                    eprintln!("[config] {msg}\n[config] конфиг не разбирается — дефолтный конфиг");
                    let _ = PARSE_ERR.set(msg);
                    Config::default()
                }
            },
            Err(_) => Config::default(),
        }
    }

    pub fn save(&self) {
        if let Some(dir) = Self::path().parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = toml::to_string_pretty(self) {
            let _ = std::fs::write(Self::path(), s);
        }
    }
}
