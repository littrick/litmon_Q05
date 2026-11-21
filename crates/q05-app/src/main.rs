//! litmon-q05 主程序入口。
//!
//! 当前闭环：Q0.5 MF 视频采集 → GPUI 显示；窗口内键鼠/滚轮 → 413D:2107。
//! 音频与会话管理将在后续 LF 接入。

use anyhow::Result;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let hid = match q05_hid::HidHandle::open() {
        Ok(hid) => Some(hid),
        Err(err) => {
            tracing::warn!(error = %err, "Q0.5 input HID unavailable; input disabled");
            None
        }
    };

    let audio = match q05_audio::AudioEngine::start() {
        Ok(audio) => Some(audio),
        Err(err) => {
            tracing::warn!(error = %err, "Q0.5 audio unavailable; video/input only");
            None
        }
    };

    let (tx, rx) = async_channel::bounded(2);
    let mut capture = q05_video::VideoCapture::start(q05_video::VideoFormat::FHD30, tx)?;

    let result = q05_ui::run(rx, hid);
    drop(audio);
    capture.stop();
    result
}
