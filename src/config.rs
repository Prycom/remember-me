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
    /// Усиление входного сигнала до шумодава и VOX, 0..200 (%).
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
    /// Empty uses the platform Downloads directory.
    pub download_dir: String,
    /// Empty uses the OS temporary directory; files live until application exit.
    pub clipboard_dir: String,
    /// Check the fixed GitHub test channel once at startup; replace atomically.
    pub auto_update: bool,
    pub avatar: Option<crate::avatar::Avatar>,
    pub avatar_file: String,
    pub avatar_style: crate::avatar::AvatarStyle,
    /// Pixel width/height of the transmitted thumbnail (8..256 presets).
    pub avatar_resolution: u16,
    /// Cached full-detail source, so changing detail does not require the original file.
    pub avatar_original: Option<crate::avatar::Avatar>,
    /// Old versions cropped photos. Reimport them once if the source still exists.
    pub avatar_contained: bool,
    /// Smooth full-colour source instead of an intentionally pixelated portrait.
    pub avatar_smooth: bool,
    pub avatar_round: bool,
    /// Prefer the built-in viewer; unsupported types still open externally.
    pub media_in_terminal: bool,
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
            download_dir: String::new(),
            clipboard_dir: String::new(),
            auto_update: true,
            avatar: None,
            avatar_file: String::new(),
            avatar_style: Default::default(),
            avatar_resolution: 32,
            avatar_original: None,
            avatar_contained: false,
            avatar_smooth: false,
            avatar_round: false,
            media_in_terminal: true,
        }
    }
}

/// Причина, по которой конфиг ушёл в дефолт, если такое было.
static PARSE_ERR: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Причина, по которой конфиг ушёл в дефолт, если такое было.
pub fn parse_error() -> Option<&'static String> {
    PARSE_ERR.get()
}

pub fn home_directory() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

pub fn expand_path(value: &str) -> PathBuf {
    let value = value.trim();
    let value = value
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| value.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(value);
    if value == "~" {
        home_directory()
    } else if let Some(suffix) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        home_directory().join(suffix)
    } else {
        PathBuf::from(value)
    }
}

impl Config {
    pub fn clipboard_directory(&self) -> PathBuf {
        if self.clipboard_dir.trim().is_empty() {
            std::env::temp_dir()
        } else {
            expand_path(&self.clipboard_dir)
        }
    }
    pub fn download_directory(&self) -> PathBuf {
        if !self.download_dir.trim().is_empty() {
            let path = expand_path(&self.download_dir);
            return if path.is_absolute() {
                path
            } else {
                std::env::current_dir().unwrap_or_default().join(path)
            };
        }
        // Honour Linux's localized XDG Downloads directory without invoking a shell.
        let user_dirs = Self::path()
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.join("user-dirs.dirs"));
        if let Some(text) = user_dirs.and_then(|p| std::fs::read_to_string(p).ok()) {
            for line in text.lines() {
                if let Some(value) = line.trim().strip_prefix("XDG_DOWNLOAD_DIR=") {
                    let value = value.trim().trim_matches('"');
                    if value == "$HOME" {
                        return home_directory();
                    }
                    if let Some(suffix) = value.strip_prefix("$HOME/") {
                        return home_directory().join(suffix);
                    }
                    let path = PathBuf::from(value);
                    if path.is_absolute() {
                        return path;
                    }
                }
            }
        }
        home_directory().join("Downloads")
    }
    pub fn path() -> PathBuf {
        let dir = std::env::var(if cfg!(windows) {
            "APPDATA"
        } else {
            "XDG_CONFIG_HOME"
        })
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
