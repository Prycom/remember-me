use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

fn main() {
    let host = cpal::host_from_id(cpal::HostId::PulseAudio).expect("pulse");
    let mut names: Vec<Option<String>> = vec![None];
    if let Some(d) = host.default_input_device() {
        names.push(Some(d.to_string()));
    }
    names.push(Some("USB PnP Audio Device Mono".into()));
    names.push(Some("Ryzen HD Audio Controller Analog Stereo".into()));
    for want in names.iter() {
        let dev = match want {
            None => host.default_input_device(),
            Some(n) => host
                .input_devices()
                .ok()
                .and_then(|mut it| it.find(|d| d.to_string() == *n)),
        };
        let Some(dev) = dev else {
            println!("запрос «{want:?}»: устройство не найдено");
            continue;
        };
        use cpal::SampleFormat as SF;
        let cfg = match dev.supported_input_configs()
            .ok()
            .and_then(|mut it| it.find(|r| r.sample_format() == SF::F32 && r.channels() == 1 && r.min_sample_rate() <= 48000 && r.max_sample_rate() >= 48000).map(|r| r.with_sample_rate(48000)))
            .or_else(|| dev.default_input_config().ok())
        {
            Some(c) => c,
            None => { println!("«{}»: нет конфигов", dev.to_string()); continue; }
        };
        let conf = cpal::StreamConfig {
            channels: cfg.channels(),
            sample_rate: cfg.sample_rate(),
            buffer_size: cpal::BufferSize::Fixed(960),
        };
        let peak = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let p2 = peak.clone();
        let s2 = stop.clone();
        let fmt = cfg.sample_format();
        let stream = match dev.build_input_stream(
            conf.clone(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                if s2.load(Ordering::Relaxed) { return; }
                let mx = data.iter().fold(0.0f32, |a, v| a.max(v.abs()));
                let cur = p2.load(Ordering::Relaxed);
                let mxu = (mx * 1_000_000.0) as u32;
                if mxu > cur {
                    p2.store(mxu, Ordering::Relaxed);
                }
            },
            move |e| eprintln!("err: {e}"),
            None,
        ) {
            Ok(s) => s,
            Err(e) => {
                println!("«{}»: build_stream err {e}", dev.to_string());
                continue;
            }
        };
        if let Err(e) = stream.play() {
            println!("«{}»: play err {e}", dev.to_string());
            continue;
        }
        let t0 = std::time::Instant::now();
        while t0.elapsed() < std::time::Duration::from_millis(2000) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        stop.store(true, Ordering::Relaxed);
        drop(stream);
        let pk = peak.load(Ordering::Relaxed) as f32 / 1_000_000.0;
        println!("«{}»: ch={} rate={} fmt={fmt:?} peak2s={pk}",
            dev.to_string(), conf.channels, conf.sample_rate);
    }
}
