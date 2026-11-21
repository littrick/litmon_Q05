//! 真机冒烟：打开 Q0.5 输入 HID，把目标光标移到屏幕中心并打印统计。
//!
//! 运行：cargo run -p q05-hid --example hid_smoke

use std::error::Error;
use std::thread;
use std::time::Duration;

fn main() -> Result<(), Box<dyn Error>> {
    let mut hid = q05_hid::HidHandle::open()?;
    // 0..32767（15-bit）；16384,16384 = 目标屏幕中心。
    hid.send(q05_proto::Frame::mouse_abs(
        16384, 16384, false, false, false, 0,
    ))?;
    thread::sleep(Duration::from_millis(500));
    println!("stats: {:?}", hid.stats());
    hid.close();
    Ok(())
}
