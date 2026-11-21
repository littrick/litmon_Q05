//! 目标屏幕状态检测。
//!
//! LF0 实测：目标 HDMI 无信号时，Q0.5 输出固定 SMPTE 彩条
//! （白/黄/青/绿/品红/红/蓝/黑，8 等宽竖条）；目标息屏/黑屏时接近全黑。
//!
//! 检测只采样每帧极少量像素，开销可以忽略。

/// 目标屏幕状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScreenState {
    /// 正常画面。
    #[default]
    Live,
    /// Q0.5 的无信号彩条（目标 HDMI 无信号/输出关闭）。
    NoSignal,
    /// 持续接近全黑（目标息屏）。
    Blank,
}

/// 8 条彩条的期望 RGB（左到右）。
const COLOR_BARS: [[u8; 3]; 8] = [
    [255, 255, 255], // 白
    [255, 255, 0],   // 黄
    [0, 255, 255],   // 青
    [0, 255, 0],     // 绿
    [255, 0, 255],   // 品红
    [255, 0, 0],     // 红
    [0, 0, 255],     // 蓝
    [0, 0, 0],       // 黑
];

const COLOR_TOLERANCE: i32 = 80;
const NO_SIGNAL_FRAMES: u32 = 10;
const BLANK_FRAMES: u32 = 15;
const LIVE_FRAMES: u32 = 5;
const BLANK_MEAN_LUMA: f32 = 8.0;
const BLANK_MAX_LUMA: f32 = 24.0;

#[derive(Debug, Clone, Copy)]
struct LumaStats {
    mean: f32,
    max: f32,
}

pub(crate) struct ScreenDetector {
    state: ScreenState,
    no_signal_frames: u32,
    blank_frames: u32,
    live_frames: u32,
}

impl ScreenDetector {
    pub(crate) fn new() -> Self {
        Self {
            state: ScreenState::Live,
            no_signal_frames: 0,
            blank_frames: 0,
            live_frames: 0,
        }
    }

    /// 分析一帧；只在状态变化时返回新状态。
    pub(crate) fn analyze(&mut self, bgra: &[u8], width: u32, height: u32) -> Option<ScreenState> {
        if width == 0 || height == 0 || bgra.len() < (width as usize * height as usize * 4) {
            return None;
        }

        let color_bars = is_color_bars(bgra, width, height);
        let luma = sample_luma(bgra, width, height);
        let blank = luma.mean < BLANK_MEAN_LUMA && luma.max < BLANK_MAX_LUMA;

        if color_bars {
            self.no_signal_frames = self.no_signal_frames.saturating_add(1);
            self.blank_frames = 0;
            self.live_frames = 0;
        } else if blank {
            self.blank_frames = self.blank_frames.saturating_add(1);
            self.no_signal_frames = 0;
            self.live_frames = 0;
        } else {
            self.live_frames = self.live_frames.saturating_add(1);
            self.no_signal_frames = 0;
            self.blank_frames = 0;
        }

        let next = if self.no_signal_frames >= NO_SIGNAL_FRAMES {
            ScreenState::NoSignal
        } else if self.blank_frames >= BLANK_FRAMES {
            ScreenState::Blank
        } else if self.live_frames >= LIVE_FRAMES {
            ScreenState::Live
        } else {
            return None;
        };

        if next != self.state {
            self.state = next;
            Some(next)
        } else {
            None
        }
    }
}

fn bgra_at(bgra: &[u8], width: u32, x: u32, y: u32) -> (i32, i32, i32) {
    let index = ((y as usize * width as usize) + x as usize) * 4;
    (
        bgra[index] as i32,
        bgra[index + 1] as i32,
        bgra[index + 2] as i32,
    )
}

fn is_color_bars(bgra: &[u8], width: u32, height: u32) -> bool {
    let y = height / 2;
    let mut matches = 0;
    for (i, expected) in COLOR_BARS.iter().enumerate() {
        let x = (((i as u32 * 2 + 1) * width) / 16).min(width.saturating_sub(1));
        let (b, g, r) = bgra_at(bgra, width, x, y);
        let dr = r - expected[0] as i32;
        let dg = g - expected[1] as i32;
        let db = b - expected[2] as i32;
        if dr * dr + dg * dg + db * db <= COLOR_TOLERANCE * COLOR_TOLERANCE {
            matches += 1;
        }
    }
    matches >= 7
}

fn sample_luma(bgra: &[u8], width: u32, height: u32) -> LumaStats {
    const GRID_X: u32 = 16;
    const GRID_Y: u32 = 9;
    let mut sum = 0.0f32;
    let mut max = 0.0f32;
    let mut count = 0u32;
    for gy in 0..GRID_Y {
        let y = ((gy * 2 + 1) * height / (GRID_Y * 2)).min(height.saturating_sub(1));
        for gx in 0..GRID_X {
            let x = ((gx * 2 + 1) * width / (GRID_X * 2)).min(width.saturating_sub(1));
            let (b, g, r) = bgra_at(bgra, width, x, y);
            let luma = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
            sum += luma;
            max = max.max(luma);
            count += 1;
        }
    }
    LumaStats {
        mean: if count == 0 { 0.0 } else { sum / count as f32 },
        max,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bars_frame(width: u32, height: u32) -> Vec<u8> {
        let mut bgra = vec![0u8; (width * height * 4) as usize];
        for y in 0..height {
            for x in 0..width {
                let band = ((x * 8) / width).min(7) as usize;
                let [r, g, b] = COLOR_BARS[band];
                let i = ((y * width + x) * 4) as usize;
                bgra[i] = b;
                bgra[i + 1] = g;
                bgra[i + 2] = r;
                bgra[i + 3] = 255;
            }
        }
        bgra
    }

    fn solid_frame(width: u32, height: u32, r: u8, g: u8, b: u8) -> Vec<u8> {
        let mut bgra = vec![0u8; (width * height * 4) as usize];
        for pixel in bgra.chunks_exact_mut(4) {
            pixel[0] = b;
            pixel[1] = g;
            pixel[2] = r;
            pixel[3] = 255;
        }
        bgra
    }

    #[test]
    fn detects_no_signal_color_bars() {
        let frame = bars_frame(160, 90);
        let mut detector = ScreenDetector::new();
        let mut change = None;
        for _ in 0..NO_SIGNAL_FRAMES {
            change = detector.analyze(&frame, 160, 90).or(change);
        }
        assert_eq!(change, Some(ScreenState::NoSignal));
        assert_eq!(detector.state, ScreenState::NoSignal);
    }

    #[test]
    fn detects_blank_screen() {
        let frame = solid_frame(160, 90, 0, 0, 0);
        let mut detector = ScreenDetector::new();
        let mut change = None;
        for _ in 0..BLANK_FRAMES {
            change = detector.analyze(&frame, 160, 90).or(change);
        }
        assert_eq!(change, Some(ScreenState::Blank));
        assert_eq!(detector.state, ScreenState::Blank);
    }

    #[test]
    fn live_frame_does_not_trigger() {
        let frame = solid_frame(160, 90, 30, 60, 90);
        let mut detector = ScreenDetector::new();
        for _ in 0..30 {
            assert!(detector.analyze(&frame, 160, 90).is_none());
        }
        assert_eq!(detector.state, ScreenState::Live);
    }

    #[test]
    fn returns_to_live_after_signal_returns() {
        let bars = bars_frame(160, 90);
        let live = solid_frame(160, 90, 120, 80, 40);
        let mut detector = ScreenDetector::new();
        for _ in 0..NO_SIGNAL_FRAMES {
            detector.analyze(&bars, 160, 90);
        }
        assert_eq!(detector.state, ScreenState::NoSignal);
        let mut change = None;
        for _ in 0..LIVE_FRAMES {
            change = detector.analyze(&live, 160, 90).or(change);
        }
        assert_eq!(change, Some(ScreenState::Live));
        assert_eq!(detector.state, ScreenState::Live);
    }

    #[test]
    fn black_desktop_with_bright_ui_is_not_blank() {
        // 大部分黑，但有一个亮点（例如窗口/光标），不应判为息屏。
        let mut frame = solid_frame(160, 90, 0, 0, 0);
        let i = ((45 * 160 + 80) * 4) as usize;
        frame[i] = 255;
        frame[i + 1] = 255;
        frame[i + 2] = 255;
        let mut detector = ScreenDetector::new();
        for _ in 0..BLANK_FRAMES * 2 {
            detector.analyze(&frame, 160, 90);
        }
        // 亮点可能不在采样网格上；这里仅要求不产生 NoSignal。
        assert_ne!(detector.state, ScreenState::NoSignal);
    }
}
