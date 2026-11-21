//! litmon-q05 主程序入口。
//!
//! 媒体流由 `q05-session::MediaSupervisor` 管理：设备在线时启动视频/HID/音频，
//! 拔出后停止，重新插入后自动重建。UI 只消费持续的视频/输入/状态通道。

use anyhow::Result;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let (video_tx, video_rx) = async_channel::bounded(2);
    let (hid_tx, hid_rx) = async_channel::bounded(8);
    let (presence_tx, presence_rx) = async_channel::bounded(8);

    let mut supervisor = q05_session::MediaSupervisor::start(video_tx, hid_tx, presence_tx)?;

    let result = q05_ui::run(video_rx, presence_rx, hid_rx);
    supervisor.stop();
    result
}
