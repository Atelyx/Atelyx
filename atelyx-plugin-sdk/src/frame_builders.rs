//! 整面帧构建辅助：命中区、输入法锚点与帧外壳的薄构造器；图元复用 [`crate::canvas_builders`]。
//! 命中判断由宿主按几何完成，插件只按回发的命中 id 解释语义。

use crate::protocol::canvas::DrawCommand;
use crate::protocol::frame::{FrameRect, HitCursor, HitRegion, SurfaceFrame};

/// 命中区（光标缺省 = 系统箭头）。
pub fn hit(id: impl Into<String>, x: f32, y: f32, w: f32, h: f32) -> HitRegion {
    HitRegion { id: id.into(), x, y, w, h, cursor: None }
}

/// 带光标形状的命中区（悬停时宿主据此换指针）。
pub fn hit_with_cursor(
    id: impl Into<String>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    cursor: HitCursor,
) -> HitRegion {
    HitRegion { id: id.into(), x, y, w, h, cursor: Some(cursor) }
}

/// 输入法光标 / 候选窗锚点矩形（面本地逻辑像素）。
pub fn ime_rect(x: f32, y: f32, w: f32, h: f32) -> FrameRect {
    FrameRect { x, y, w, h }
}

/// 帧外壳：指令与命中区，不声明按键 / 输入法 / 帧率。
pub fn frame(commands: Vec<DrawCommand>, hits: Vec<HitRegion>) -> SurfaceFrame {
    SurfaceFrame { commands, hits, ime: None, keys: false, fps: None }
}

/// 声明本面收键盘：指针按下取焦，随后全部按键（含修饰键组合）经 `FrameInput::Key` 回发。
pub fn keys(mut frame: SurfaceFrame) -> SurfaceFrame {
    frame.keys = true;
    frame
}

/// 声明本面收输入法：组合串与提交文本经 `FrameInput::Text` 回发，`rect` 定位光标与候选窗。
pub fn ime(mut frame: SurfaceFrame, rect: FrameRect) -> SurfaceFrame {
    frame.ime = Some(rect);
    frame
}

/// 声明帧率（1–120）：宿主按该节拍回发 `host:frame.tick`。
pub fn fps(mut frame: SurfaceFrame, fps: u16) -> SurfaceFrame {
    frame.fps = Some(fps);
    frame
}
