//! litmon-q05 主程序入口。
//!
//! 当前最小闭环：启动 Q0.5 MF 视频采集 → GPUI 窗口显示。
//! 输入/音频/会话将在后续 LF 接入。

use anyhow::Result;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let (tx, rx) = async_channel::bounded(2);
    let mut capture = q05_video::VideoCapture::start(q05_video::VideoFormat::FHD30, tx)?;

    let result = q05_ui::run(rx);
    capture.stop();
    result
}
