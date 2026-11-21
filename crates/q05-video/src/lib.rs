//! Q0.5 视频采集（UVC / Media Foundation）。
//!
//! 计划实现：
//! - 设备枚举与按“Oray Q0.5”匹配；
//! - `IMFSourceReader` 读取 native MJPEG（1080p/720p）；
//! - controlling / share mode 选择；
//! - 帧解码（`zune-jpeg`）与最新帧槽。
