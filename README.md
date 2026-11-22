# litmon-q05

**Oray Q0.5 的第三方 Windows 客户端。**

直接使用 Q0.5 的 UVC 摄像头、UAC 音频和 HID 输入，显示目标机画面、播放声音，
并把窗口内的键鼠/滚轮事件反向透传给目标机。不依赖官方客户端、虚拟驱动或云端中转。

> 非官方项目，与 Oray / 向日葵官方无关。

## 构建运行

要求：Windows 10/11、Rust 1.88+、Windows SDK `rc.exe`、Q0.5 设备。

```powershell
cargo build --release -p q05-app
.\target\release\litmon-q05.exe
```

查看日志：

```powershell
.\target\release\litmon-q05.exe --console
```

## 快捷键

| 操作 | 说明 |
|------|------|
| `Ctrl+Alt+Q` | 启用/停用输入透传 |
| `F11` | 全屏/退出全屏，同时透传 F11 |
| 右键 HUD | 打开菜单（全屏、自适应大小、置顶、HUD 大小/位置、关闭） |
| 左键/滚轮/键盘 | 窗口内透传给目标 |
| 右键视频区域 | 透传给目标 |

## License

MIT，见 [LICENSE](LICENSE)。
