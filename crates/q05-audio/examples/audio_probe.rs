//! 枚举 cpal 输入/输出设备与默认格式（只读，不打开流）。
//!
//! 运行：cargo run -p q05-audio --example audio_probe

use std::error::Error;

use cpal::traits::{DeviceTrait, HostTrait};

fn main() -> Result<(), Box<dyn Error>> {
    let host = cpal::default_host();
    println!("default host: {:?}", host.id());

    println!("== input devices ==");
    for (i, device) in host.input_devices()?.enumerate() {
        let name = device
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "<unknown>".into());
        println!("IN[{i}] {name}");
        match device.default_input_config() {
            Ok(config) => println!(
                "  default: {:?}, {} ch, {} Hz",
                config.sample_format(),
                config.channels(),
                config.sample_rate()
            ),
            Err(err) => println!("  default config error: {err}"),
        }
        if let Ok(configs) = device.supported_input_configs() {
            for config in configs {
                println!(
                    "  supported: {:?}, {}..{} ch, {}..{} Hz",
                    config.sample_format(),
                    config.channels(),
                    config.channels(),
                    config.min_sample_rate(),
                    config.max_sample_rate()
                );
            }
        }
    }

    println!("== output devices ==");
    for (i, device) in host.output_devices()?.enumerate() {
        let name = device
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "<unknown>".into());
        println!("OUT[{i}] {name}");
        match device.default_output_config() {
            Ok(config) => println!(
                "  default: {:?}, {} ch, {} Hz",
                config.sample_format(),
                config.channels(),
                config.sample_rate()
            ),
            Err(err) => println!("  default config error: {err}"),
        }
    }

    match host.default_output_device() {
        Some(device) => println!(
            "default output: {:?}",
            device
                .description()
                .map(|d| d.name().to_string())
                .unwrap_or_else(|_| "<unknown>".into())
        ),
        None => println!("default output: none"),
    }
    Ok(())
}
