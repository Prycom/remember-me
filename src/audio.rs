// Звуковой конвейер: захват → Opus-кодирование → [сеть]; [сеть] → декодирование → воспроизведение.
//
// Референс — наблюдаемая поверхность discord_voice (setInputVolume, setLocalVolume,
// setSelfMute/setSelfDeafen, setPTTActive, фича 'loopback'):
// 48 кГц моно, Opus 20 мс кадры, VBR ~32 kbps, FEC 10%, PTT/VOX, шумовой гейт.
// Смена входного/выходного устройства — на лету (как device update у Discord).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use crate::noise_suppress::NoiseSuppressor;
use crate::voice_level::{PeakLimiter, SpeechFilter, SpeechLeveler, VoiceGate};
use cpal::Sample as _;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use opusic_sys::{
    OPUS_APPLICATION_VOIP, OPUS_OK, OPUS_SET_BITRATE_REQUEST, OPUS_SET_DTX_REQUEST,
    OPUS_SET_INBAND_FEC_REQUEST, OPUS_SET_PACKET_LOSS_PERC_REQUEST, OPUS_SET_VBR_REQUEST,
    opus_decode_float, opus_decoder_create, opus_decoder_destroy, opus_encode, opus_encoder_create,
    opus_encoder_ctl, opus_encoder_destroy,
};

/// RM_DEBUG=1 — трассировка звука в stderr (ошибки стримов cpal и т.п.).
macro_rules! dbg_audio {
    ($($t:tt)*) => {
        if std::env::var("RM_DEBUG").ok().as_deref() == Some("1") {
            eprintln!("[audio] {}", format!($($t)*));
        }
    };
}

pub const SAMPLE_RATE: u32 = 48_000;
pub const FRAME_SAMPLES: usize = 960; // 20 мс моно @48k
pub const BITRATE: i32 = 32_000;
pub const ECHO_DELAY_MS: u64 = 250;
/// Never replay a long recording after a stalled network task.
pub const MAX_PENDING_AUDIO: usize = 6;
/// маркер «устройство ещё не подключено»
const NEVER: &str = "\0";

enum OpusEnc {}
enum OpusDec {}

pub struct Encoder {
    ptr: *mut OpusEnc,
}
unsafe impl Send for Encoder {}

impl Encoder {
    pub fn new() -> Result<Encoder, String> {
        let mut err: i32 = 0;
        let ptr =
            unsafe { opus_encoder_create(SAMPLE_RATE as i32, 1, OPUS_APPLICATION_VOIP, &mut err) };
        if err != OPUS_OK {
            return Err(format!("opus_encoder_create: {err}"));
        }
        let e = Encoder { ptr: ptr.cast() };
        e.ctl(OPUS_SET_BITRATE_REQUEST, BITRATE);
        e.ctl(OPUS_SET_VBR_REQUEST, 1);
        e.ctl(OPUS_SET_INBAND_FEC_REQUEST, 1);
        e.ctl(OPUS_SET_PACKET_LOSS_PERC_REQUEST, 10);
        e.ctl(OPUS_SET_DTX_REQUEST, 0);
        Ok(e)
    }

    fn ctl(&self, request: i32, value: i32) {
        unsafe {
            opus_encoder_ctl(self.ptr.cast(), request, value);
        }
    }

    pub fn encode(&mut self, pcm_i16: &[i16]) -> Result<Vec<u8>, String> {
        let mut out = [0u8; 400];
        let n = unsafe {
            opus_encode(
                self.ptr.cast(),
                pcm_i16.as_ptr(),
                FRAME_SAMPLES as i32,
                out.as_mut_ptr(),
                out.len() as i32,
            )
        };
        if n < 0 {
            return Err(format!("opus_encode: {n}"));
        }
        Ok(out[..n as usize].to_vec())
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe {
            opus_encoder_destroy(self.ptr.cast());
        }
    }
}

pub struct Decoder {
    ptr: *mut OpusDec,
}
unsafe impl Send for Decoder {}

impl Decoder {
    pub fn new() -> Result<Decoder, String> {
        let mut err: i32 = 0;
        let ptr = unsafe { opus_decoder_create(SAMPLE_RATE as i32, 1, &mut err) };
        if err != OPUS_OK {
            return Err(format!("opus_decoder_create: {err}"));
        }
        Ok(Decoder { ptr: ptr.cast() })
    }

    pub fn decode(&mut self, pkt: &[u8]) -> Result<Vec<f32>, String> {
        self.decode_frame(Some(pkt), false)
    }

    fn decode_frame(&mut self, pkt: Option<&[u8]>, fec: bool) -> Result<Vec<f32>, String> {
        // Keep codec overshoots in float until the playback limiter. Decoding
        // into i16 would clip them before the limiter can preserve the wave.
        let mut pcm = [0f32; FRAME_SAMPLES];
        let n = unsafe {
            opus_decode_float(
                self.ptr.cast(),
                pkt.map_or(std::ptr::null(), |p| p.as_ptr()),
                pkt.map_or(0, |p| p.len() as i32),
                pcm.as_mut_ptr(),
                FRAME_SAMPLES as i32,
                i32::from(fec),
            )
        };
        if n < 0 {
            return Err(format!("opus_decode: {n}"));
        }
        Ok(pcm[..n as usize].to_vec())
    }

    /// Preserve the codec timeline on packet loss. Recover the most recent
    /// missing frame using in-band FEC (PLC if absent), earlier ones by PLC.
    pub fn decode_with_loss(&mut self, pkt: &[u8], missing: u32) -> Result<Vec<f32>, String> {
        let missing = missing.min(3);
        let mut samples = Vec::with_capacity((missing as usize + 1) * FRAME_SAMPLES);
        for i in 0..missing {
            samples.extend(self.decode_frame(
                if i + 1 == missing { Some(pkt) } else { None },
                i + 1 == missing,
            )?);
        }
        samples.extend(self.decode(pkt)?);
        Ok(samples)
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe {
            opus_decoder_destroy(self.ptr.cast());
        }
    }
}

#[inline]
fn f32_to_i16(v: f32) -> i16 {
    (v.clamp(-1.0, 1.0) * 32768.0) as i16
}

fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt()
}

/// 0..100 maps to -70..-30 dBFS (25 = -60 dBFS). Compare amplitudes,
/// not signal power; a normal microphone is often much quieter than -20 dBFS.
fn vad_amplitude(threshold: u8) -> f32 {
    10.0_f32.powf((-70.0 + threshold.min(100) as f32 * 0.4) / 20.0)
}

fn level_percent(samples: &[f32]) -> u8 {
    (rms(samples) * 300.0).min(100.0) as u8
}

// ── Ресемпллер входа (линейная интерполяция) ───────────────────────────────

pub struct Resampler {
    rate_in: u32,
    phase: f64,
    buf: Vec<f32>,
    pending: Vec<f32>,
}

impl Resampler {
    pub fn anything(rate_in: u32) -> Resampler {
        Resampler {
            rate_in: rate_in.max(1),
            phase: 0.0,
            buf: Vec::new(),
            pending: Vec::new(),
        }
    }

    /// Возвращает накопившиеся целые кадры по 960 сэмплов @48k.
    pub fn feed(&mut self, chunk: &[f32]) -> Vec<Vec<f32>> {
        let ratio = self.rate_in as f64 / SAMPLE_RATE as f64;
        self.buf.extend(chunk.iter().copied());
        let mut out = std::mem::take(&mut self.pending);
        loop {
            let i = self.phase.floor() as usize;
            if i + 1 >= self.buf.len() {
                break;
            }
            let frac = (self.phase - i as f64) as f32;
            out.push(self.buf[i] * (1.0 - frac) + self.buf[i + 1] * frac);
            self.phase += ratio;
        }
        let consumed = self.phase.floor() as usize;
        let drop = consumed.min(self.buf.len().saturating_sub(1));
        self.buf.drain(..drop);
        self.phase -= drop as f64;
        let frames = out.len() / FRAME_SAMPLES;
        let mut res = Vec::with_capacity(frames);
        for k in 0..frames {
            res.push(out[k * FRAME_SAMPLES..(k + 1) * FRAME_SAMPLES].to_vec());
        }
        self.pending = out.split_off(frames * FRAME_SAMPLES);
        res
    }
}

// ── Общие параметры (громкости/режимы, меняются из UI на лету) ─────────────

#[derive(Clone)]
pub struct AudioParams {
    gain_in: Arc<AtomicU32>,   // 0..200 (%)
    gain_peer: Arc<AtomicU32>, // 0..200 (%)
    mute_mic: Arc<AtomicBool>,
    deaf: Arc<AtomicBool>,
    noise_gate: Arc<AtomicBool>,
    vad_threshold: Arc<AtomicU32>, // 0..100
    ptt_active: Arc<AtomicBool>,
    vox_mode: Arc<AtomicBool>,
    loopback: Arc<AtomicBool>,
    auto_gain: Arc<AtomicBool>,
    voice_filter: Arc<AtomicBool>,
    noise_suppression: Arc<AtomicBool>,
    input_clipping: Arc<AtomicBool>,
    /// уровень микрофона после gain, 0..100% (для индикатора на экране)
    in_level: Arc<AtomicU8>,
    out_level: Arc<AtomicU8>,
}

impl AudioParams {
    pub fn new(gain_in: u8, gain_peer: u8, noise_gate: bool, vad_threshold: u8) -> AudioParams {
        AudioParams {
            gain_in: Arc::new(AtomicU32::new(gain_in as u32)),
            gain_peer: Arc::new(AtomicU32::new(gain_peer as u32)),
            mute_mic: Arc::new(AtomicBool::new(false)),
            deaf: Arc::new(AtomicBool::new(false)),
            noise_gate: Arc::new(AtomicBool::new(noise_gate)),
            vad_threshold: Arc::new(AtomicU32::new(vad_threshold as u32)),
            ptt_active: Arc::new(AtomicBool::new(false)),
            vox_mode: Arc::new(AtomicBool::new(true)),
            loopback: Arc::new(AtomicBool::new(false)),
            auto_gain: Arc::new(AtomicBool::new(true)),
            voice_filter: Arc::new(AtomicBool::new(false)),
            noise_suppression: Arc::new(AtomicBool::new(false)),
            input_clipping: Arc::new(AtomicBool::new(false)),
            in_level: Arc::new(AtomicU8::new(0)),
            out_level: Arc::new(AtomicU8::new(0)),
        }
    }
    pub fn in_level(&self) -> u8 {
        self.in_level.load(Ordering::Relaxed)
    }
    pub fn out_level(&self) -> u8 {
        self.out_level.load(Ordering::Relaxed)
    }
    pub fn store_in_level(&self, pct: u8) {
        self.in_level.store(pct, Ordering::Relaxed);
    }
    pub fn set_gain_in(&self, v: u8) {
        self.gain_in.store(v as u32, Ordering::Relaxed);
    }
    pub fn gain_in(&self) -> u8 {
        self.gain_in.load(Ordering::Relaxed).min(200) as u8
    }
    pub fn set_gain_peer(&self, v: u8) {
        self.gain_peer.store(v as u32, Ordering::Relaxed);
    }
    pub fn gain_peer(&self) -> u8 {
        self.gain_peer.load(Ordering::Relaxed).min(200) as u8
    }
    pub fn set_voice_filter(&self, enabled: bool) {
        self.voice_filter.store(enabled, Ordering::Relaxed);
    }
    pub fn voice_filter(&self) -> bool {
        self.voice_filter.load(Ordering::Relaxed)
    }
    pub fn set_noise_suppression(&self, enabled: bool) {
        self.noise_suppression.store(enabled, Ordering::Relaxed);
    }
    pub fn noise_suppression(&self) -> bool {
        self.noise_suppression.load(Ordering::Relaxed)
    }
    pub fn input_clipping(&self) -> bool {
        self.input_clipping.load(Ordering::Relaxed)
    }
    pub fn set_mute_mic(&self, v: bool) {
        self.mute_mic.store(v, Ordering::Relaxed);
    }
    pub fn mute_mic(&self) -> bool {
        self.mute_mic.load(Ordering::Relaxed) || self.deaf()
    }
    pub fn set_deaf(&self, v: bool) {
        self.deaf.store(v, Ordering::Relaxed);
    }
    pub fn deaf(&self) -> bool {
        self.deaf.load(Ordering::Relaxed)
    }
    pub fn set_noise_gate(&self, v: bool) {
        self.noise_gate.store(v, Ordering::Relaxed);
    }
    pub fn noise_gate(&self) -> bool {
        self.noise_gate.load(Ordering::Relaxed)
    }
    pub fn set_vad_threshold(&self, v: u8) {
        self.vad_threshold.store(v as u32, Ordering::Relaxed);
    }
    pub fn vad_threshold(&self) -> u8 {
        self.vad_threshold.load(Ordering::Relaxed).min(100) as u8
    }
    pub fn set_vox_mode(&self, v: bool) {
        self.vox_mode.store(v, Ordering::Relaxed);
    }
    pub fn vox_mode(&self) -> bool {
        self.vox_mode.load(Ordering::Relaxed)
    }
    pub fn set_ptt_active(&self, v: bool) {
        self.ptt_active.store(v, Ordering::Relaxed);
    }
    pub fn set_loopback(&self, v: bool) {
        self.loopback.store(v, Ordering::Relaxed);
    }
    pub fn loopback(&self) -> bool {
        self.loopback.load(Ordering::Relaxed)
    }
    pub fn auto_gain(&self) -> bool {
        self.auto_gain.load(Ordering::Relaxed)
    }
    pub fn set_auto_gain(&self, value: bool) {
        self.auto_gain.store(value, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone)]
pub struct DeviceList {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}

/// Типы имён, которые человек в списке не желает видеть:
/// null-«Discard…» (заглушка, звук с неё нулевой) и двусмысленный
/// «Default Audio Device» (как вывод открывается, как вход ведёт в тишину
/// системного дефолта — пользователь тогда всё равно не различает их со
/// своим «(по умолчанию)»).
const JUNK_DEVICES: [&str; 2] = [
    "Discard all samples (playback) or generate zero samples (capture)",
    "Default Audio Device",
];

/// Алиасы системного микшера — рабочие, но по смыслу равны «(по умолчанию)».
/// Оставляем их в конце списка: реальные источники идут первыми.
const ALIAS_DEVICES: [&str; 2] = [
    "PipeWire Sound Server",
    "Default ALSA Output (currently PipeWire Media Server)",
];

fn clean_names(raw: Vec<String>, input: bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(raw.len());
    let mut aliases: Vec<String> = Vec::new();
    for n in raw {
        if JUNK_DEVICES.contains(&n.as_str()) {
            continue;
        }
        if out.iter().any(|o| o == &n) || aliases.iter().any(|o| o == &n) {
            continue; // дубли покидают список
        }
        if ALIAS_DEVICES.contains(&n.as_str()) {
            aliases.push(n);
        } else {
            out.push(n);
        }
    }
    out.extend(aliases);
    // Порядок входов: живые USB-микрофоны — первыми (частый случай
    // внешнего микрофона), затем прочие реальные входы (у встроенного
    // 3.5-мм джека вход может оставить тишину, если в него не воткнут
    // микрофон), мониторы («Monitor of …») — в конце: запись системного
    // звука — полезный, но не основной сценарий.
    if input && out.len() > 1 {
        let mut usb: Vec<String> = Vec::new();
        let mut other: Vec<String> = Vec::new();
        let mut mons: Vec<String> = Vec::new();
        for p in out {
            if p.starts_with("Monitor of ") {
                mons.push(p);
            } else if p.contains("USB") {
                usb.push(p);
            } else {
                other.push(p);
            }
        }
        usb.extend(other);
        usb.extend(mons);
        return usb;
    }
    out
}

pub fn list_devices() -> DeviceList {
    let host = audio_host();
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    if let Ok(it) = host.input_devices() {
        for d in it {
            inputs.push(d.to_string());
        }
    }
    if let Ok(it) = host.output_devices() {
        for d in it {
            outputs.push(d.to_string());
        }
    }
    DeviceList {
        inputs: clean_names(inputs, true),
        outputs: clean_names(outputs, false),
    }
}

/// Один хост на весь процесс: имена из списка и привязка при смене
/// устройств должны из одного источника.
/// Предпочитаем PulseAudio-хост: он отдаёт человеческие имена устройств
/// (как pavucontrol — «Ryzen HD Audio Controller Analog Stereo»,
/// «USB PnP Audio Device») и умеет resample. Нет сервера (headless) —
/// откат на системный (ALSA): там остаются технические имена, но звук
/// открывается.
static AUDIO_HOST: std::sync::OnceLock<cpal::Host> = std::sync::OnceLock::new();

pub fn audio_host() -> &'static cpal::Host {
    AUDIO_HOST.get_or_init(|| {
        #[cfg(target_os = "linux")]
        if let Ok(h) = cpal::host_from_id(cpal::HostId::PulseAudio) {
            let outputs = h.output_devices().map(|it| it.count()).unwrap_or(0);
            let inputs = h.input_devices().map(|it| it.count()).unwrap_or(0);
            if outputs > 0 || inputs > 0 {
                return h;
            }
        }
        cpal::default_host()
    })
}

/// Проверка сохранённых имён устройств: если имя не числится в списке
/// реальных (сохранился мусор прошлой сессии) — откат на дефолт.
/// Вызывать ДО audio::start.
pub fn sanitize_config_devices(cfg: &mut crate::config::Config) {
    if cfg.input_device.is_empty() && cfg.output_device.is_empty() {
        return;
    }
    let list = list_devices();
    let mut dirty = false;
    if !cfg.input_device.is_empty() && !list.inputs.iter().any(|i| i == &cfg.input_device) {
        dbg_audio!(
            "конфиг: устройство ввода «{}» не найдено — дефолт",
            cfg.input_device
        );
        cfg.input_device.clear();
        dirty = true;
    }
    if !cfg.output_device.is_empty() && !list.outputs.iter().any(|o| o == &cfg.output_device) {
        dbg_audio!(
            "конфиг: устройство вывода «{}» не найдено — дефолт",
            cfg.output_device
        );
        cfg.output_device.clear();
        dirty = true;
    }
    if dirty {
        cfg.save();
    }
}

/// Имена устройств в stderr (RM_DEBUG=1) — для отладки подмены устройств.
pub fn dbg_devices() {
    if std::env::var("RM_DEBUG").ok().as_deref() != Some("1") {
        return;
    }
    let list = list_devices();
    eprintln!("[audio] хост: {:?}", audio_host().id());
    eprintln!("[audio] входы:");
    for (i, d) in list.inputs.iter().enumerate() {
        eprintln!("[audio]   {i}: {d}");
    }
    eprintln!("[audio] выходы:");
    for (i, d) in list.outputs.iter().enumerate() {
        eprintln!("[audio]   {i}: {d}");
    }
}

fn find_device(input: bool, name: &str) -> Option<cpal::Device> {
    let host = audio_host();
    let by_default = if input {
        host.default_input_device()
    } else {
        host.default_output_device()
    };
    if name.is_empty() {
        return by_default;
    }
    let it = if input {
        match host.input_devices() {
            Ok(i) => i,
            Err(_) => return by_default,
        }
    } else {
        match host.output_devices() {
            Ok(i) => i,
            Err(_) => return by_default,
        }
    };
    for d in it {
        if d.to_string() == name {
            return Some(d);
        }
    }
    by_default
}

/// Буфер стрима: 960 кадров — 20 мс @48 кГц. У pulse-хоста Default
/// означает серверные буферы (у PipeWire ~200 мс — слишком много для
/// звонка), фикса даёт и низкую задержку, и ровные куски на разбор.
const STREAM_BUF_FRAMES: u32 = 960;

/// Конфигурация потока: предпочитаем 48 кГц и «нормальные» форматы;
/// иначе любой формат из F32/I16/U16/U8 (ресемплинг у нас свой).
fn plan_stream(
    dev: &cpal::Device,
    input: bool,
) -> Option<(cpal::StreamConfig, cpal::SampleFormat, u32)> {
    use cpal::SampleFormat as SF;
    let ranges: Vec<cpal::SupportedStreamConfigRange> = if input {
        dev.supported_input_configs().ok()?.collect()
    } else {
        dev.supported_output_configs().ok()?.collect()
    };
    let fmt_pref = [SF::F32, SF::I16, SF::U16, SF::U8];
    let try_candidates = |rate: Option<u32>| -> Option<(cpal::StreamConfig, SF, u32)> {
        for fmt in fmt_pref {
            for want_ch in [1u16, 2, 0] {
                // 0 = любые каналы
                for r in &ranges {
                    if r.sample_format() != fmt {
                        continue;
                    }
                    if r.channels() < 1 || (want_ch != 0 && r.channels() != want_ch) {
                        continue;
                    }
                    let (lo, hi) = (r.min_sample_rate(), r.max_sample_rate());
                    let target = match rate {
                        Some(x) if lo <= x && x <= hi => x,
                        Some(_) => continue,
                        None => lo.max(sound_lowest_rate(lo, hi)),
                    };
                    return Some((
                        cpal::StreamConfig {
                            channels: r.channels().min(2),
                            sample_rate: target,
                            buffer_size: cpal::BufferSize::Fixed(STREAM_BUF_FRAMES),
                        },
                        r.sample_format(),
                        target,
                    ));
                }
            }
        }
        None
    };
    // 1) 48 кГц
    if let Some(f) = try_candidates(Some(SAMPLE_RATE)) {
        return Some(f);
    }
    // 2) любая ставка, но нормальный формат
    if let Some(f) = try_candidates(None) {
        return Some(f);
    }
    // 3) совсем не убогое — дефолт конфиг какой есть
    let d = if input {
        dev.default_input_config().ok()?
    } else {
        dev.default_output_config().ok()?
    };
    let cfg = cpal::StreamConfig {
        channels: d.channels(),
        sample_rate: d.sample_rate(),
        buffer_size: cpal::BufferSize::Fixed(STREAM_BUF_FRAMES),
    };
    Some((cfg, d.sample_format(), d.sample_rate()))
}

/// любит верхнюю часть диапазона (обычно 44100+)
fn sound_lowest_rate(lo: u32, hi: u32) -> u32 {
    for w in [48_000, 44_100, 32_000, 22_050, 16_000] {
        if w >= lo && w <= hi {
            return w;
        }
    }
    lo
}

// ── Выход ──────────────────────────────────────────────────────────────────

struct OutFeed {
    deq: VecDeque<f32>, // сэмплы @48k (от декодера)
    phase: f64,
    rate_out: f64,
    starved: u32,
    limiter: PeakLimiter,
    buffering: bool,
    envelope: f32,
    last_sample: f32,
}

impl OutFeed {
    /// Заполняет out_mono сэмплами из очереди, растягивая/сжимая с 48k до rate_out.
    fn mono_fill(&mut self, out_mono: &mut [f32], gain: f32) {
        // сколько исходных (48k) сэмплов тратится на один выходной
        let step = SAMPLE_RATE as f64 / self.rate_out.max(1.0);
        // Refill after each real gap, rather than only once before a call.
        // 40 ms absorbs modest UDP jitter without a large voice latency.
        if self.buffering && self.deq.len() >= 2 * FRAME_SAMPLES {
            self.buffering = false;
        }
        let fade_step = 1.0 / (self.rate_out as f32 * 0.005).max(1.0);
        let decay = (-1.0 / (self.rate_out as f32 * 0.001).max(1.0)).exp();
        for o in out_mono.iter_mut() {
            let i = self.phase.floor() as usize;
            let frac = (self.phase - i as f64) as f32;
            if !self.buffering && i < self.deq.len() && (frac < 1e-6 || i + 1 < self.deq.len()) {
                let next = self.deq.get(i + 1).copied().unwrap_or(self.deq[i]);
                let s = self.deq[i] * (1.0 - frac) + next * frac;
                self.envelope = (self.envelope + fade_step).min(1.0);
                self.last_sample = s * self.envelope;
                *o = self.last_sample * gain;
                self.starved = 0;
                self.phase += step;
            } else {
                // Avoid a full-scale step into zero at an underrun. Resume
                // with a short fade once enough new audio has accumulated.
                self.last_sample *= decay;
                if self.last_sample.abs() < 1e-20 {
                    self.last_sample = 0.0;
                }
                *o = self.last_sample * gain;
                self.envelope = 0.0;
                self.buffering = true;
                self.starved = self.starved.saturating_add(1);
            }
        }
        let consumed = self.phase.floor() as usize;
        if consumed > 0 {
            let cut = consumed.min(self.deq.len());
            self.deq.drain(..cut);
            self.phase -= cut as f64;
        }
        // Silence is not audio in the queue. Never accrue a read-position
        // debt while waiting for the next packet (startup, VOX, packet loss).
        if self.deq.is_empty() {
            self.phase = 0.0;
        }
        self.limiter.process(out_mono);
    }
}

fn st_push(st: &mut OutFeed, samples: &[f32]) {
    st.deq.extend(samples.iter().copied());
    let excess = st.deq.len().saturating_sub(10 * FRAME_SAMPLES);
    if excess > 0 {
        st.deq.drain(..excess);
        st.phase = 0.0;
    }
}

// ── Держатель всего звука ──────────────────────────────────────────────────

type MediaTarget = Arc<Mutex<Option<tokio::sync::mpsc::Sender<Vec<u8>>>>>;

pub struct MediaRoute {
    target: MediaTarget,
    sender: tokio::sync::mpsc::Sender<Vec<u8>>,
}

impl Drop for MediaRoute {
    fn drop(&mut self) {
        let mut target = self.target.lock().unwrap();
        // An old session must not detach a newly started one.
        if target
            .as_ref()
            .is_some_and(|s| s.same_channel(&self.sender))
        {
            *target = None;
        }
    }
}

pub struct AudioEngine {
    pub params: AudioParams,
    /// канал, куда кодер кладёт Opus-кадры (main pump шлёт их в net-сессию)
    pub encoded_tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    media_target: MediaTarget,
    /// состояние «я говорю» (для UI/индикатора)
    pub self_speaking: Arc<AtomicBool>,
    out: Arc<Mutex<OutFeed>>,
    echo_tx: mpsc::SyncSender<Vec<u8>>,
    dec: Mutex<Option<Decoder>>,
    in_stream: Mutex<Option<cpal::Stream>>,
    out_stream: Mutex<Option<cpal::Stream>>,
    /// текущие привязанные устройства (пусто = дефолт), NEVER — ещё не подключены
    cur_in: Mutex<String>,
    cur_out: Mutex<String>,
}

impl AudioEngine {
    pub fn route_media(&self, sender: tokio::sync::mpsc::Sender<Vec<u8>>) -> MediaRoute {
        *self.media_target.lock().unwrap() = Some(sender.clone());
        MediaRoute {
            target: self.media_target.clone(),
            sender,
        }
    }

    /// Runs on a runtime worker, independently of terminal input, rendering,
    /// settings and chat history. No UI polling is needed to send voice.
    pub async fn forward_encoded(
        self: Arc<Self>,
        mut rx: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    ) {
        while let Some(frame) = rx.recv().await {
            let mut pending = VecDeque::from([frame]);
            while let Ok(frame) = rx.try_recv() {
                if pending.len() == MAX_PENDING_AUDIO {
                    pending.pop_front();
                }
                pending.push_back(frame);
            }
            let target = self.media_target.lock().unwrap().clone();
            if let Some(target) = target {
                for frame in pending {
                    let _ = target.try_send(frame);
                }
            }
        }
    }

    #[cfg(test)]
    pub fn for_test() -> (Arc<Self>, tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>) {
        let (encoded_tx, encoded_rx) = tokio::sync::mpsc::unbounded_channel();
        let (echo_tx, _) = mpsc::sync_channel(1);
        (
            Arc::new(Self {
                params: AudioParams::new(100, 100, false, 25),
                encoded_tx,
                media_target: Arc::new(Mutex::new(None)),
                self_speaking: Arc::new(AtomicBool::new(false)),
                out: Arc::new(Mutex::new(OutFeed {
                    deq: VecDeque::new(),
                    phase: 0.0,
                    rate_out: SAMPLE_RATE as f64,
                    starved: 0,
                    buffering: true,
                    envelope: 0.0,
                    last_sample: 0.0,
                    limiter: PeakLimiter::default(),
                })),
                echo_tx,
                dec: Mutex::new(None),
                in_stream: Mutex::new(None),
                out_stream: Mutex::new(None),
                cur_in: Mutex::new(String::new()),
                cur_out: Mutex::new(String::new()),
            }),
            encoded_rx,
        )
    }

    /// Push декодированного кадра @48k (net-задача / эхо-поток).
    pub fn push_out(&self, samples: &[f32]) {
        let mut st = self.out.lock().unwrap();
        st_push(&mut st, samples);
    }
    /// Сброс декодера и очереди перед новым звонком.
    pub fn prefill_out(&self) {
        let mut st = self.out.lock().unwrap();
        st.deq.clear();
        st.phase = 0.0;
        st.buffering = true;
        st.envelope = 0.0;
        st.last_sample = 0.0;
        *self.dec.lock().unwrap() = None;
        // The jitter buffer starts on actual audio, not prefilled silence.
    }

    /// Декодирование входящего Opus-кадра (для net-задачи).
    pub fn decode_opus(&self, pkt: &[u8], missing: u32) -> Option<Vec<f32>> {
        let mut g = self.dec.lock().unwrap();
        if g.is_none() {
            *g = Decoder::new().ok();
        }
        let d = g.as_mut()?;
        d.decode_with_loss(pkt, missing).ok()
    }

    /// Имена реально привязанных устройств; "" значит «по умолчанию».
    pub fn current_devices(&self) -> (String, String) {
        let i = self.cur_in.lock().unwrap();
        let o = self.cur_out.lock().unwrap();
        (i.clone(), o.clone())
    }

    fn stop_input(&self) {
        let mut slot = self.in_stream.lock().unwrap();
        *slot = None; // старые колбэки отвязаны, кодер-поток умрёт сам (канал закрыт)
        *self.cur_in.lock().unwrap() = NEVER.into();
        self.self_speaking.store(false, Ordering::Relaxed);
    }
    fn stop_output(&self) {
        let mut slot = self.out_stream.lock().unwrap();
        *slot = None;
        *self.cur_out.lock().unwrap() = NEVER.into();
        self.params.out_level.store(0, Ordering::Relaxed);
    }

    /// Подменяет входное и выходное устройство по имени ("" = дефолт).
    /// Меняется только та сторона, чьё имя реально изменилось.
    pub fn rebuild_devices(&self, input_name: &str, output_name: &str) -> Result<(), String> {
        let (ci, co) = self.current_devices();
        let in_changed = ci != input_name;
        let out_changed = co != output_name;
        if !in_changed && !out_changed {
            return Ok(());
        }

        // выход: там живёт общий OutFeed (к нему же пишет эхо-тест)
        let mut err_out: Option<String> = None;
        if out_changed {
            self.stop_output();
            let res = self.attach_output(output_name);
            dbg_audio!(
                "attach_output «{output_name}» → {}",
                match &res {
                    Ok(()) => "ok".to_string(),
                    Err(e) => e.to_string(),
                }
            );
            match res {
                Ok(()) => *self.cur_out.lock().unwrap() = output_name.into(),
                Err(e) => err_out = Some(format!("выход: {e}")),
            }
        }

        // вход: при подмене выбрасываем поток ввода — кодер-поток умрёт от
        // Disconnected, спавним новый
        let mut err_in: Option<String> = None;
        if in_changed {
            self.stop_input();
            let res = self.attach_input(input_name);
            dbg_audio!(
                "attach_input «{input_name}» → {}",
                match &res {
                    Ok(()) => "ok".to_string(),
                    Err(e) => e.to_string(),
                }
            );
            match res {
                Ok(()) => *self.cur_in.lock().unwrap() = input_name.into(),
                Err(e) => err_in = Some(format!("вход: {e}")),
            }
        }

        match (err_in, err_out) {
            (None, None) => Ok(()),
            (i, o) => Err([i, o].into_iter().flatten().collect::<Vec<_>>().join("; ")),
        }
    }

    fn attach_output(&self, out_name: &str) -> Result<(), String> {
        let out_dev = find_device(false, out_name).ok_or("аудио-выход не найден")?;
        let (out_cfg, out_fmt, out_rate) =
            plan_stream(&out_dev, false).ok_or("выходные форматы недоступны")?;
        let out_channels = out_cfg.channels as usize;
        let st = self.out.clone();
        let gain = self.params.gain_peer.clone();
        let df = self.params.deaf.clone();
        let level = self.params.out_level.clone();

        macro_rules! build_out {
            ($t:ty) => {{
                out_dev
                    .build_output_stream::<$t, _, _>(
                        out_cfg.clone(),
                        move |data: &mut [$t], _info| {
                            let g_now = if df.load(Ordering::Relaxed) {
                                0.0
                            } else {
                                gain.load(Ordering::Relaxed) as f32 / 100.0
                            };
                            let ch = out_channels.max(1);
                            let frames = data.len() / ch;
                            let mut mono = vec![0f32; frames];
                            {
                                let mut f = st.lock().unwrap();
                                f.mono_fill(&mut mono, g_now);
                            }
                            level.store(level_percent(&mono), Ordering::Relaxed);
                            if ch <= 1 {
                                for (d, m) in data.iter_mut().zip(&mono) {
                                    *d = m.to_sample::<$t>();
                                }
                            } else {
                                for (i, m) in mono.iter().enumerate() {
                                    for c in 0..ch {
                                        if let Some(d) = data.get_mut(i * ch + c) {
                                            *d = m.to_sample::<$t>();
                                        }
                                    }
                                }
                            }
                        },
                        |err| {
                            dbg_audio!("ошибка выходного стрима: {err}");
                        },
                        None,
                    )
                    .map_err(|e| format!("открыть выход: {e}"))
            }};
        }
        let out_stream = match out_fmt {
            cpal::SampleFormat::F32 => build_out!(f32),
            cpal::SampleFormat::I16 => build_out!(i16),
            cpal::SampleFormat::U16 => build_out!(u16),
            cpal::SampleFormat::U8 => build_out!(u8),
            cpal::SampleFormat::I8 => build_out!(i8),
            other => return Err(format!("неподдержанный выходной формат {other:?}")),
        }?;
        let mut o_rate = self.out.lock().unwrap();
        o_rate.rate_out = out_rate as f64;
        o_rate.phase = 0.0;
        drop(o_rate);
        out_stream
            .play()
            .map_err(|e| format!("запуск выхода: {e}"))?;
        *self.out_stream.lock().unwrap() = Some(out_stream);
        Ok(())
    }

    fn attach_input(&self, in_name: &str) -> Result<(), String> {
        let in_dev = find_device(true, in_name).ok_or("микрофон не найден")?;
        let (in_cfg, in_fmt, in_rate) =
            plan_stream(&in_dev, true).ok_or("входные форматы недоступны")?;
        let in_channels = in_cfg.channels as usize;
        let (raw_tx, raw_rx) = mpsc::sync_channel::<Vec<f32>>(64);

        macro_rules! build_in {
            ($t:ty) => {{
                let tx = raw_tx.clone();
                in_dev
                    .build_input_stream::<$t, _, _>(
                        in_cfg.clone(),
                        move |data: &[$t], _info| {
                            if in_channels <= 1 {
                                let mono: Vec<f32> =
                                    data.iter().map(|s| s.to_sample::<f32>()).collect();
                                let _ = tx.try_send(mono);
                            } else {
                                let mut mono = Vec::with_capacity(data.len() / in_channels);
                                for ch_chunk in data.chunks(in_channels) {
                                    let sum: f32 =
                                        ch_chunk.iter().map(|s| s.to_sample::<f32>()).sum();
                                    mono.push(sum / in_channels as f32);
                                }
                                let _ = tx.try_send(mono);
                            }
                        },
                        |err| {
                            dbg_audio!("ошибка входного стрима: {err}");
                        },
                        None,
                    )
                    .map_err(|e| format!("открыть вход: {e}"))
            }};
        }
        let in_stream = match in_fmt {
            cpal::SampleFormat::F32 => build_in!(f32),
            cpal::SampleFormat::I16 => build_in!(i16),
            cpal::SampleFormat::U16 => build_in!(u16),
            cpal::SampleFormat::I8 => build_in!(i8),
            cpal::SampleFormat::U8 => build_in!(u8),
            other => return Err(format!("неподдержанный входной формат {other:?}")),
        }?;
        in_stream.play().map_err(|e| format!("запуск входа: {e}"))?;

        self.spawn_encoder(raw_rx, in_rate);
        *self.in_stream.lock().unwrap() = Some(in_stream);
        Ok(())
    }

    /// Поток кодера: ресемпл → mute/PTT/VOX → шумовой гейт → Opus → канал.
    fn spawn_encoder(&self, raw_rx: mpsc::Receiver<Vec<f32>>, enc_rate: u32) {
        let params = self.params.clone();
        let speak_flag = self.self_speaking.clone();
        let encoded_tx = self.encoded_tx.clone();
        let echo_tx = self.echo_tx.clone();
        let _ = std::thread::Builder::new()
            .name("rm-encoder".into())
            .spawn(move || {
                let mut enc = match Encoder::new() {
                    Ok(e) => e,
                    Err(e) => {
                        eprintln!("opus encoder: {e}");
                        return;
                    }
                };
                let mut res = Resampler::anything(enc_rate);
                let mut gate = VoiceGate::default();
                let mut was_speaking = false;
                let mut leveler = SpeechLeveler::default();
                let mut filter = SpeechFilter::default();
                let mut denoiser = NoiseSuppressor::default();
                let mut clipping_hold = 0_u32;
                loop {
                    let chunk = match raw_rx.recv_timeout(Duration::from_millis(200)) {
                        Ok(v) => v,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    };
                    for mut frame in res.feed(&chunk) {
                        if frame.len() != FRAME_SAMPLES {
                            continue;
                        }
                        if frame.iter().any(|s| s.abs() >= 0.99) {
                            clipping_hold = 50;
                        } else {
                            clipping_hold = clipping_hold.saturating_sub(1);
                        }
                        params
                            .input_clipping
                            .store(clipping_hold > 0, Ordering::Relaxed);
                        filter.process(&mut frame, params.voice_filter());
                        let unsuppressed_rms = rms(&frame);
                        let gain = params.gain_in() as f32 / 100.0;
                        for sample in &mut frame {
                            *sample *= gain;
                        }
                        let speech_probability = denoiser
                            .process(&mut frame, params.noise_suppression() && !params.mute_mic());
                        // Keep suppressed residual noise out of VOX and AGC.
                        // The existing hangover still preserves quiet endings.
                        let likely_speech = speech_probability.is_none_or(|p| p >= 0.35);
                        let continuing_speech = likely_speech
                            || (was_speaking && speech_probability.is_some_and(|p| p >= 0.1));
                        // уровень микрофона для индикатора (RMS проценты)
                        {
                            params.store_in_level((rms(&frame) * 300.0).min(100.0) as u8);
                        }
                        // Режимы как в Discord: mute перекрывает всё;
                        // PTT — говорим только при зажатом спейсе; VOX — по порогу
                        let vox = params.vox_mode();
                        let ptt = params.ptt_active.load(Ordering::Relaxed);
                        // Noise gating uses the same frame envelope, including
                        // in PTT. Zeroing individual quiet samples distorts
                        // zero crossings and makes fading words buzz.
                        let (speaking_now, strong_speech) = gate.update(
                            if continuing_speech { rms(&frame) } else { 0.0 },
                            vad_amplitude(params.vad_threshold()),
                            vox || params.noise_gate(),
                            ptt,
                            params.mute_mic() || (!vox && !ptt),
                        );
                        if was_speaking != speaking_now {
                            speak_flag.store(speaking_now, Ordering::Relaxed);
                            was_speaking = speaking_now;
                        }
                        if !speaking_now {
                            leveler.idle();
                            continue;
                        }
                        leveler.process_input(
                            &mut frame,
                            strong_speech && speech_probability.is_none_or(|p| p >= 0.6),
                            params.auto_gain(),
                            gain,
                            speech_probability.map(|_| unsuppressed_rms),
                        );
                        gate.apply(&mut frame);
                        params.store_in_level(level_percent(&frame));
                        let pcm_i16: Vec<i16> = frame.iter().map(|s| f32_to_i16(*s)).collect();
                        match enc.encode(&pcm_i16) {
                            Ok(bytes) => {
                                if params.loopback.load(Ordering::Relaxed) {
                                    let _ = echo_tx.send(bytes);
                                } else {
                                    let _ = encoded_tx.send(bytes);
                                }
                            }
                            Err(e) => eprintln!("opus encode: {e}"),
                        }
                    }
                }
            });
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.stop_input();
        self.stop_output();
    }
}

/// Запуск захвата/кодера/воспроизведения.
pub fn start(
    cfg: &crate::config::Config,
    encoded_tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
) -> Result<Arc<AudioEngine>, String> {
    let params = AudioParams::new(
        cfg.gain_in,
        cfg.gain_peer,
        cfg.noise_gate,
        cfg.vad_threshold,
    );
    params.set_vox_mode(matches!(cfg.mic_mode, crate::config::MicMode::Vox));
    params.set_auto_gain(cfg.auto_gain);
    params.set_voice_filter(cfg.voice_filter);
    params.set_noise_suppression(cfg.noise_suppression);

    let self_speaking = Arc::new(AtomicBool::new(false));
    let out = Arc::new(Mutex::new(OutFeed {
        deq: VecDeque::new(),
        phase: 0.0,
        rate_out: SAMPLE_RATE as f64,
        starved: 0,
        buffering: true,
        envelope: 0.0,
        last_sample: 0.0,
        limiter: PeakLimiter::default(),
    }));

    // ── канал эхо-теста (живёт на весь срок движка) ────────────────────────
    let (echo_tx, echo_rx) = mpsc::sync_channel::<Vec<u8>>(32);
    {
        let out_feed = out.clone();
        let _ = std::thread::Builder::new()
            .name("rm-echo".into())
            .spawn(move || {
                let mut dec = match Decoder::new() {
                    Ok(d) => d,
                    Err(_) => return,
                };
                // отставание: окно накопления ~ECHO_DELAY_MS
                let mut window: VecDeque<(Instant, Vec<u8>)> = VecDeque::new();
                let lag = Duration::from_millis(ECHO_DELAY_MS);
                // сама задержка — «честность» эха: слышим себя как собеседник
                loop {
                    match echo_rx.recv_timeout(Duration::from_millis(10)) {
                        Ok(frame) => window.push_back((Instant::now() + lag, frame)),
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                    while let Some(front) = window.front() {
                        if front.0 > Instant::now() {
                            break;
                        }
                        let (_, frame) = window.pop_front().unwrap();
                        if let Ok(samples) = dec.decode(&frame) {
                            let mut st = out_feed.lock().unwrap();
                            st_push(&mut st, &samples);
                        }
                    }
                }
            });
    }

    let engine = Arc::new(AudioEngine {
        params,
        encoded_tx,
        media_target: Arc::new(Mutex::new(None)),
        self_speaking,
        out,
        echo_tx,
        dec: Mutex::new(None),
        in_stream: Mutex::new(None),
        out_stream: Mutex::new(None),
        cur_in: Mutex::new(NEVER.into()),
        cur_out: Mutex::new(NEVER.into()),
    });

    dbg_devices();
    // Подключаем устройства; при неудаче — на дефолтные, как Discord (device update).
    if let Err(e) = engine.rebuild_devices(&cfg.input_device, &cfg.output_device) {
        match engine.rebuild_devices("", "") {
            Ok(()) => eprintln!("звук: использованы устройства по умолчанию ({e})"),
            Err(e2) => return Err(format!("{e}; резерв тоже не удался: {e2}")),
        }
    }
    Ok(engine)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ptt_release_and_modals_always_stop_capture() {
        use crate::{
            action::{Action, Focus, Pane},
            app::{App, Screen},
            config::Config,
        };
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
        for modal in [Action::Help, Action::HangUp, Action::ShowProfile(0)] {
            let mut app = App::for_test(Config::default());
            app.screen = Screen::Call;
            app.focus = Focus::Pane(Pane::Main);
            app.engine.params.set_vox_mode(false);
            app.on_event(Event::Key(KeyEvent::new(
                KeyCode::Char(' '),
                KeyModifiers::NONE,
            )));
            assert!(app.engine.params.ptt_active.load(Ordering::Relaxed));
            app.on_action(modal);
            assert!(!app.engine.params.ptt_active.load(Ordering::Relaxed));
            app.on_event(Event::Key(KeyEvent::new_with_kind(
                KeyCode::Char(' '),
                KeyModifiers::NONE,
                KeyEventKind::Release,
            )));
            app.on_action(Action::Cancel);
            assert!(!app.engine.params.ptt_active.load(Ordering::Relaxed));
        }
    }

    #[tokio::test]
    async fn voice_routing_does_not_need_ui_polling_and_bounds_old_audio() {
        let (engine, rx) = AudioEngine::for_test();
        let (tx, mut media) = tokio::sync::mpsc::channel(MAX_PENDING_AUDIO);
        let route = engine.route_media(tx);
        // Simulate a delayed worker: keep the most recent 120 ms only.
        for n in 0..100 {
            engine.encoded_tx.send(vec![n]).unwrap();
        }
        let pump = tokio::spawn(engine.clone().forward_encoded(rx));
        for n in 94..100 {
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(1), media.recv())
                    .await
                    .unwrap(),
                Some(vec![n])
            );
        }
        engine.encoded_tx.send(vec![100]).unwrap();
        assert_eq!(media.recv().await, Some(vec![100]));
        drop(route);
        assert!(media.recv().await.is_none());
        pump.abort();
    }

    #[tokio::test]
    async fn cancelling_an_old_session_keeps_the_new_voice_route() {
        let (engine, rx) = AudioEngine::for_test();
        let (old_tx, _) = tokio::sync::mpsc::channel(MAX_PENDING_AUDIO);
        let old = engine.route_media(old_tx);
        let (new_tx, mut media) = tokio::sync::mpsc::channel(MAX_PENDING_AUDIO);
        let new = engine.route_media(new_tx);
        drop(old);
        let pump = tokio::spawn(engine.clone().forward_encoded(rx));
        engine.encoded_tx.send(vec![42]).unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), media.recv())
                .await
                .unwrap(),
            Some(vec![42])
        );
        drop(new);
        assert!(media.recv().await.is_none());
        pump.abort();
    }

    fn encoder() -> (
        AudioParams,
        mpsc::SyncSender<Vec<f32>>,
        tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    ) {
        let params = AudioParams::new(100, 100, false, 25);
        let (encoded_tx, encoded_rx) = tokio::sync::mpsc::unbounded_channel();
        let (echo_tx, _) = mpsc::sync_channel(1);
        let engine = AudioEngine {
            params: params.clone(),
            encoded_tx,
            media_target: Arc::new(Mutex::new(None)),
            self_speaking: Arc::new(AtomicBool::new(false)),
            out: Arc::new(Mutex::new(output(SAMPLE_RATE))),
            echo_tx,
            dec: Mutex::new(None),
            in_stream: Mutex::new(None),
            out_stream: Mutex::new(None),
            cur_in: Mutex::new(NEVER.into()),
            cur_out: Mutex::new(NEVER.into()),
        };
        let (tx, rx) = mpsc::sync_channel(4);
        engine.spawn_encoder(rx, SAMPLE_RATE);
        (params, tx, encoded_rx)
    }

    fn speech(amplitude: f32) -> Vec<f32> {
        (0..2 * FRAME_SAMPLES + 1)
            .map(|i| {
                amplitude * (i as f32 * 440.0 * std::f32::consts::TAU / SAMPLE_RATE as f32).sin()
            })
            .collect()
    }

    async fn receive_frames(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>) {
        let mut dec = Decoder::new().unwrap();
        for _ in 0..2 {
            let packet = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("voice was suppressed")
                .unwrap();
            let pcm = dec.decode(&packet).unwrap();
            assert_eq!(pcm.len(), FRAME_SAMPLES);
            assert!(rms(&pcm) > 0.001, "encoded voice was silent");
        }
    }

    #[tokio::test]
    async fn quiet_speech_passes_default_vox_and_opus() {
        let (_, tx, mut rx) = encoder();
        tx.send(speech(0.02)).unwrap();
        receive_frames(&mut rx).await;
    }

    #[tokio::test]
    async fn very_quiet_speech_starts_at_default_volume_without_needing_200_percent() {
        let (_, tx, mut rx) = encoder();
        tx.send(speech(0.002)).unwrap();
        receive_frames(&mut rx).await;
    }

    #[tokio::test]
    async fn denoising_keeps_quiet_actual_speech_audible_through_vox_agc_and_opus() {
        let (params, tx, mut rx) = encoder();
        params.set_noise_suppression(true);
        params.set_voice_filter(true);
        let mut speech = crate::noise_suppress::tests::reference_speech();
        speech.truncate(4 * SAMPLE_RATE as usize + 1);
        let input_rms = rms(&speech);
        for s in &mut speech {
            *s *= 0.002 / input_rms;
        }
        tx.send(speech).unwrap();
        drop(tx);
        let mut decoder = Decoder::new().unwrap();
        let mut output = Vec::new();
        while let Some(packet) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
        {
            output.extend(decoder.decode(&packet).unwrap());
        }
        assert!(
            output.len() > SAMPLE_RATE as usize * 2,
            "quiet speech was cut off"
        );
        assert!(
            rms(&output) > 0.03,
            "quiet speech no longer audible: {}",
            rms(&output)
        );
    }

    #[tokio::test]
    async fn input_preamp_changes_decoded_volume_with_and_without_denoising_and_agc() {
        let mut input = crate::noise_suppress::tests::reference_speech();
        input.truncate(4 * SAMPLE_RATE as usize + 1);
        let scale = 0.006 / rms(&input);
        for sample in &mut input {
            *sample *= scale;
        }
        for denoise in [false, true] {
            for automatic in [false, true] {
                let mut levels = Vec::new();
                for gain in [0, 50, 100, 200] {
                    let (params, tx, mut rx) = encoder();
                    params.set_noise_suppression(denoise);
                    params.set_auto_gain(automatic);
                    params.set_gain_in(gain);
                    params.set_vox_mode(false);
                    params.set_ptt_active(true);
                    tx.send(input.clone()).unwrap();
                    drop(tx);
                    let mut decoder = Decoder::new().unwrap();
                    let mut output = Vec::new();
                    while let Some(packet) =
                        tokio::time::timeout(Duration::from_secs(10), rx.recv())
                            .await
                            .unwrap()
                    {
                        output.extend(decoder.decode(&packet).unwrap());
                    }
                    assert!(output.len() > 3 * SAMPLE_RATE as usize);
                    levels.push(rms(&output[SAMPLE_RATE as usize..]));
                }
                println!("preamp denoise={denoise} auto={automatic}: {levels:?}");
                assert!(levels[0] < 0.0001, "0% must silence the input");
                for pair in levels[1..].windows(2) {
                    let ratio = pair[1] / pair[0];
                    assert!(
                        (1.65..2.25).contains(&ratio),
                        "preamp cancelled or applied twice: {ratio}, denoise={denoise} auto={automatic}"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn agc_does_not_bring_suppressed_background_back_up() {
        let (params, tx, mut rx) = encoder();
        params.set_noise_suppression(true);
        let mut seed = 17_u32;
        let input: Vec<_> = (0..4 * SAMPLE_RATE + 1)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed as f64 / u32::MAX as f64 * 2.0 - 1.0) as f32 * 0.015
            })
            .collect();
        tx.send(input).unwrap();
        drop(tx);
        let mut decoder = Decoder::new().unwrap();
        let mut power = 0.0;
        while let Some(packet) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
        {
            power += decoder
                .decode(&packet)
                .unwrap()
                .iter()
                .map(|s| s * s)
                .sum::<f32>();
        }
        let output_rms = (power / (4 * SAMPLE_RATE) as f32).sqrt();
        assert!(
            output_rms < 0.003,
            "AGC amplified suppressed noise: {output_rms}"
        );
    }

    #[tokio::test]
    async fn mic_gain_affects_vox_and_level_meter() {
        let (params, tx, mut rx) = encoder();
        tx.send(speech(0.001)).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), rx.recv())
                .await
                .is_err()
        );
        params.set_gain_in(200);
        tx.send(speech(0.001)).unwrap();
        receive_frames(&mut rx).await;
        assert!(params.in_level() > 0);
    }

    #[tokio::test]
    async fn mute_and_ptt_release_suppress_voice_immediately() {
        let (params, tx, mut rx) = encoder();
        tx.send(speech(0.02)).unwrap();
        receive_frames(&mut rx).await;
        params.set_mute_mic(true);
        tx.send(speech(0.02)).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), rx.recv())
                .await
                .is_err()
        );
        params.set_mute_mic(false);
        params.set_vox_mode(false);
        params.set_ptt_active(true);
        tx.send(speech(0.02)).unwrap();
        receive_frames(&mut rx).await;
        params.set_ptt_active(false);
        tx.send(speech(0.02)).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), rx.recv())
                .await
                .is_err()
        );
    }

    #[test]
    fn deafen_restores_the_previous_microphone_state() {
        let params = AudioParams::new(100, 100, false, 25);
        params.set_deaf(true);
        assert!(params.mute_mic());
        params.set_deaf(false);
        assert!(!params.mute_mic());
        params.set_mute_mic(true);
        params.set_deaf(true);
        params.set_deaf(false);
        assert!(params.mute_mic());
    }

    #[test]
    fn output_volume_is_limited_without_integer_sample_overflow() {
        let mut feed = output(SAMPLE_RATE);
        st_push(&mut feed, &vec![0.9; 2 * FRAME_SAMPLES]);
        let mut block = vec![0.0; FRAME_SAMPLES];
        feed.mono_fill(&mut block, 2.0);
        assert!(block.iter().all(|&s| s >= 0.0 && s <= 0.95001));
        assert!(block[FRAME_SAMPLES / 4..].iter().all(|&s| s > 0.8));
    }

    fn output(rate: u32) -> OutFeed {
        OutFeed {
            deq: VecDeque::new(),
            phase: 0.0,
            rate_out: rate as f64,
            starved: 0,
            buffering: true,
            envelope: 0.0,
            last_sample: 0.0,
            limiter: PeakLimiter::default(),
        }
    }

    #[test]
    fn playback_recovers_after_waiting_and_speech_pauses() {
        for rate in [16_000, 44_100, 48_000, 96_000] {
            let mut feed = output(rate);
            let mut block = vec![0.0; rate as usize / 50];
            for _ in 0..3 {
                // The output device runs while waiting for a call / while VOX is closed.
                for _ in 0..100 {
                    feed.mono_fill(&mut block, 1.0);
                }
                st_push(&mut feed, &vec![0.25; FRAME_SAMPLES]);
                feed.mono_fill(&mut block, 1.0);
                assert!(
                    block.iter().all(|s| s.abs() < 0.0001),
                    "one packet should wait for jitter headroom"
                );
                for _ in 0..10 {
                    st_push(&mut feed, &vec![0.25; FRAME_SAMPLES]);
                    feed.mono_fill(&mut block, 1.0);
                    assert!(
                        block[block.len() / 4..].iter().all(|s| *s > 0.2),
                        "playback stayed silent after idle at {rate} Hz"
                    );
                }
            }
        }
    }

    #[test]
    fn output_handles_callback_larger_than_available_audio() {
        let mut feed = output(SAMPLE_RATE);
        st_push(&mut feed, &vec![0.5; 2 * FRAME_SAMPLES]);
        let mut block = vec![0.0; 4 * FRAME_SAMPLES];
        feed.mono_fill(&mut block, 1.0);
        assert!(block[240..2 * FRAME_SAMPLES].iter().all(|&s| s == 0.5));
        assert!(block[3 * FRAME_SAMPLES..].iter().all(|s| s.abs() < 0.0001));
        assert!(
            block.windows(2).all(|s| (s[1] - s[0]).abs() < 0.02),
            "underrun abruptly cut the wave"
        );
        st_push(&mut feed, &vec![0.25; 2 * FRAME_SAMPLES]);
        feed.mono_fill(&mut block[..FRAME_SAMPLES], 1.0);
        assert!(block[240..FRAME_SAMPLES].iter().all(|&s| s == 0.25));
    }

    #[test]
    fn modest_packet_jitter_does_not_create_silence_holes() {
        let mut feed = output(SAMPLE_RATE);
        let mut block = vec![0.0; SAMPLE_RATE as usize / 100]; // 10 ms callback
        // Packet intervals alternate between 30 and 10 ms, averaging 20 ms.
        for tick in 0..100 {
            if tick % 4 == 0 || tick % 4 == 3 {
                st_push(&mut feed, &vec![0.25; FRAME_SAMPLES]);
            }
            feed.mono_fill(&mut block, 1.0);
            if tick > 4 {
                assert!(
                    block.iter().all(|s| *s > 0.24),
                    "jitter hole at tick {tick}"
                );
            }
        }
    }

    #[test]
    fn opus_recovers_missing_frames_and_bounds_large_loss_bursts() {
        let mut enc = Encoder::new().unwrap();
        let mut dec = Decoder::new().unwrap();
        let input: Vec<_> = speech(0.12)[..FRAME_SAMPLES]
            .iter()
            .map(|s| f32_to_i16(*s))
            .collect();
        for _ in 0..30 {
            let packet = enc.encode(&input).unwrap();
            dec.decode(&packet).unwrap();
        }
        enc.encode(&input).unwrap(); // lost packet
        let packet = enc.encode(&input).unwrap();
        let recovered = dec.decode_with_loss(&packet, 1).unwrap();
        assert_eq!(recovered.len(), 2 * FRAME_SAMPLES);
        assert!(
            rms(&recovered[..FRAME_SAMPLES]) > 0.005,
            "lost frame replaced with hard silence"
        );
        assert!(recovered.iter().all(|s| s.is_finite()));
        assert_eq!(
            dec.decode_with_loss(&packet, u32::MAX).unwrap().len(),
            4 * FRAME_SAMPLES
        );
    }
}
