//! 真机冒烟：采集 Q0.5 音频并播放到默认输出，持续 10 秒。
//!
//! 运行：cargo run -p q05-audio --example audio_smoke

use std::error::Error;
use std::thread;
use std::time::Duration;

fn main() -> Result<(), Box<dyn Error>> {
    let engine = q05_audio::AudioEngine::start(q05_audio::AudioMute::new())?;
    println!("config: {:?}", engine.config());
    for second in 1..=10 {
        thread::sleep(Duration::from_secs(1));
        println!("stats {second}s: {:?}", engine.stats());
    }
    Ok(())
}
