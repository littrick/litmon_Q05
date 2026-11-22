# litmon-q05

**Oray Q0.5 的第三方 Windows 客户端。**

直接使用 Q0.5 提供的标准 USB 摄像头（UVC）、USB 音频（UAC）和 HID 接口，
在本机显示目标机画面、播放目标机声音，并把窗口内的键鼠/滚轮事件反向透传给目标机。
不依赖官方客户端、虚拟显示驱动或云端中转。

> 本项目是非官方实现，与 Oray / 向日葵官方无关。

## 为什么用它

- **更快、延迟更低**：视频直接走 Media Foundation 的 UVC/MJPEG 采集，解码后进入 GPUI
  渲染，不经过虚拟显示器、虚拟驱动或远程中转链路。本机实测 1080p30 从采集到 UI
  约 **6–8 ms**。
- **原生 Windows 应用**：GPUI + DirectX 11，窗口缩放、全屏、DPI 缩放下画面和坐标映射
  保持一致。
- **完整媒体链路**：视频、音频、输入由后台 supervisor 统一管理；设备拔出后黑屏，
  重新插入自动重建三条流。
- **按窗口透传**：只捕获应用窗口内的输入，不做全局键盘钩子；失焦立即释放所有按键。
- **轻量可诊断**：默认无控制台，`--console` 查看 INFO 日志；HUD 显示设备、视频、
  音频和 FPS 状态。

## 功能

- 目标屏幕显示：1920×1080 MJPEG → BGRA → GPUI，30 fps。
- 目标声音：Q0.5 UAC 采集 48 kHz 立体声，播放到默认输出设备。
- 反向控制：绝对坐标鼠标、左/中/右键、滚轮、HID usage 键盘。
- 输入安全开关：`Ctrl+Alt+Q` 启用/停用透传；窗口失焦/退出自动 release-all。
- 设备插拔：拔出黑屏、输入停用；重插自动恢复视频、音频、输入。
- 屏幕状态：识别目标息屏/无信号（黑屏或 SMPTE 彩条）。
- 右下角 HUD：设备/视频/音频状态、FPS、右键菜单。
- 右键菜单：全屏、自适应大小、置顶、HUD 大小/位置、关闭窗口。
- `F11` 全屏，同时把 F11 按键透传给目标。
- 应用图标：仓库只保存 SVG，构建时生成多尺寸 ICO 并嵌入 exe。

## 项目结构

```text
crates/
├─ q05-proto/     HID 私有协议编解码
├─ q05-hid/       413D:2107 输入 HID 读写线程
├─ q05-input/     坐标归一化、键鼠状态、HID usage 映射
├─ q05-video/     345F:2131 UVC / Media Foundation 采集与 MJPEG 解码
├─ q05-audio/     Q0.5 UAC 采集 + 默认输出播放
├─ q05-session/   设备在线检测、媒体 supervisor、自动重连
├─ q05-ui/        GPUI 界面、HUD、右键菜单、窗口内输入
└─ q05-app/       程序入口、控制台/图标等 Windows 集成
```

## 系统要求

- Windows 10/11 x64；
- Oray Q0.5 设备：
  - `USB\VID_345F&PID_2131`：视频 + 音频复合设备；
  - `USB\VID_413D&PID_2107`：输入 HID；
- Rust 1.88+（edition 2024 + let-chains；当前使用 nightly 验证）；
- 构建图标需要 Windows SDK `rc.exe`；
- 不要求安装 Oray 官方驱动，使用系统自带 usbvideo/usbaudio/HidUsb。

## 构建与运行

```powershell
cargo build --release -p q05-app
.\target\release\litmon-q05.exe
```

查看日志：

```powershell
.\target\release\litmon-q05.exe --console
# 或
cargo run -p q05-app -- --console
```

双击启动默认不显示控制台；从终端启动会自动附加父控制台，可用 `Ctrl+C` 结束。

## 使用

| 操作 | 说明 |
|------|------|
| `Ctrl+Alt+Q` | 启用/停用窗口内输入透传 |
| `F11` | 本地全屏/退出全屏，同时透传 F11 给目标 |
| 右键 HUD | 打开菜单：全屏、自适应大小、置顶、HUD 大小/位置、关闭窗口 |
| 左键/滚轮/键盘 | 窗口内透传给目标 |
| 右键视频区域 | 透传给目标，不弹本地菜单 |

## 已知限制

- 目标机真实分辨率无法从固定 1080p 的 UVC 流中获取；HUD 显示的是采集流分辨率。
- HUD 大小/位置目前只保存在内存中，重启恢复默认。
- 中文 IME、中键、音画同步、50 次插拔压力测试仍需继续验证。
- 单实例、配置持久化、安装包/签名尚未完成。
- 设备拾音/HDMI 音频依赖目标机 HDMI 音频链路；目标机切换分辨率/DPMS 后如果
  ELD 异常，可能需要在目标机做一次 DPMS off/on 或重新插拔 HDMI。

## 免责声明

- 本项目是非官方、非商业的互操作性实现，不代表 Oray / 向日葵；
- Q0.5、Oray、向日葵等名称/商标归其各自所有者；
- 不包含官方固件、二进制或商业插件；
- 反向控制会操作另一台真实主机，请在获得授权的前提下使用，风险自负。

## License

MIT，见 [LICENSE](LICENSE)。
