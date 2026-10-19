use cpal::traits::{DeviceTrait, HostTrait};

fn main() {
    println!("доступные хосты: {:?}", cpal::available_hosts());
    let host = cpal::host_from_id(cpal::HostId::PulseAudio).expect("pulse host");
    println!("=== PULSE входы ===");
    if let Ok(it) = host.input_devices() {
        for d in it {
            println!("  [{}]", d.to_string());
        }
    }
    println!("дефолт-вход: {:?}",
        host.default_input_device().map(|d| d.to_string()));
    println!("=== PULSE выходы ===");
    if let Ok(it) = host.output_devices() {
        for d in it {
            println!("  [{}]", d.to_string());
        }
    }
    println!("дефолт-выход: {:?}",
        host.default_output_device().map(|d| d.to_string()));
    // форматы дефолт-выхода
    if let Some(d) = host.default_output_device() {
        if let Ok(cfgs) = d.supported_output_configs() {
            println!("=== форматы дефолт-выхода (до 5) ===");
            for c in cfgs.into_iter().take(5) {
                println!("  {:?}", c);
            }
        }
    }
}
