//! 面帧：插件把整个面画成一份指令清单 + 命中区，宿主执行绘制并把原始输入原样回发。
//! 坐标一律面本地逻辑像素、左上原点；宿主不理解命中区语义，只按几何做命中与光标形状。

use serde::{Deserialize, Serialize};

use crate::paint::{validate_commands, DrawCommand, MAX_FPS};

/// 命中区数量上限（单帧）。
pub const MAX_HITS: usize = 4096;
/// 命中区 id 上限（字节）：插件可以把它当作不透明标识用（仓库相对路径这类天然偏长的串），
/// 上限按 Linux 的 `PATH_MAX` 留足，中文一字 3 字节也不至于撑破。
pub const MAX_HIT_ID_LEN: usize = 4096;

/// 命中区光标形状（悬停时的鼠标指针）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HitCursor {
    #[default]
    Default,
    Pointer,
    Text,
    Grab,
    Grabbing,
    Crosshair,
    NotAllowed,
    ResizeHorizontal,
    ResizeVertical,
}

/// 命中区：矩形 + 标识 + 光标形状。同帧内按声明序叠放（后声明的在上）。
/// 宿主把落在其上的指针事件的 `hit` 填成该 id，插件按 id 自行解释语义。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HitRegion {
    /// 插件自定义的命中标识，同帧内唯一。
    pub id: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// 缺省 = 系统默认箭头。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<HitCursor>,
}

/// 面本地矩形（输入法光标 / 候选框锚点）。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// 帧的排版尺寸（逻辑像素）：插件按这个尺寸算出这一帧。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameSize {
    pub w: f32,
    pub h: f32,
}

/// 一面一帧的完整内容。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceFrame {
    /// 本帧按多大尺寸排版。宿主据此判定这张帧是否对得上当前面尺寸——改宽期间手里那帧常常
    /// 还是上一拍宽度排的，直接画出来右对齐项与滚动条就会停在旧边缘。
    /// 缺省 = 未声明，宿主无从判定，只能按「拿到什么画什么」处理。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<FrameSize>,
    pub commands: Vec<DrawCommand>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hits: Vec<HitRegion>,
    /// 声明后宿主承接输入法：组合串与提交文本经 [`FrameInput::Text`] 回发，光标与候选窗按此矩形定位。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ime: Option<FrameRect>,
    /// 声明后宿主把键盘交给本面：指针按下取焦，随后全部按键（含修饰键组合）经 [`FrameInput::Key`] 回发。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keys: bool,
    /// 帧率（1–120）：声明后宿主按该节拍回发 `host:frame.tick`；缺省不推帧。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<u16>,
    /// 浮层（下拉 / 菜单 / 气泡 / 灯箱）：宿主画在本面之上并按窗口裁剪，可越出面矩形。
    /// 本帧不带 = 收起。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub popup: Option<SurfacePopup>,
}

/// 浮层承接点击的范围：`inside` = 只有落在浮层矩形内的按下交给浮层（外部按下照常送本面），
/// `all` = 浮层展开期间由宿主处理其覆盖区外的按下，并向插件回发 [`FrameInput::Outside`]。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PopupCapture {
    #[default]
    Inside,
    All,
}

/// 面的浮层：绘制在本面之上（宿主按窗口裁剪，可越出面矩形）。
/// `rect` / `commands` / `hits` / `ime` 全部用**面本地坐标**（与主帧同一坐标系）。
/// 与主帧同寿命：本帧带浮层即展开，下一帧不带即收起。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfacePopup {
    /// 相对面左上角的位置与尺寸：可为负（例如盖住面板顶部的头部行），也可大于面尺寸。
    pub rect: FrameRect,
    #[serde(default)]
    pub capture: PopupCapture,
    pub commands: Vec<DrawCommand>,
    /// 浮层自己的命中区：浮层矩形内的按下先在这里命中（坐标同面本地）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hits: Vec<HitRegion>,
    /// 声明后输入法候选窗按此矩形（面本地坐标）定位；缺省用主帧 `ime`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ime: Option<FrameRect>,
}

impl SurfaceFrame {
    /// 挂载前校验：指令逐条、命中区 id 非空唯一、矩形有限非负、帧率在范围内。
    pub fn validate(&self) -> Result<(), String> {
        validate_commands(&self.commands)?;
        validate_hits(&self.hits)?;
        if let Some(ime) = self.ime {
            ensure_rect(ime.x, ime.y, ime.w, ime.h, "输入法光标")?;
        }
        if let Some(size) = self.size {
            ensure_rect(0.0, 0.0, size.w, size.h, "排版尺寸")?;
        }
        if let Some(fps) = self.fps {
            if !(1..=MAX_FPS).contains(&fps) {
                return Err(format!("帧率须在 1–{MAX_FPS} 之间"));
            }
        }
        if let Some(popup) = &self.popup {
            validate_commands(&popup.commands).map_err(|e| format!("浮层{e}"))?;
            validate_hits(&popup.hits).map_err(|e| format!("浮层{e}"))?;
            if !popup.rect.x.is_finite()
                || !popup.rect.y.is_finite()
                || !popup.rect.w.is_finite()
                || !popup.rect.h.is_finite()
            {
                return Err("浮层矩形必须是有限数".into());
            }
            if popup.rect.w < 0.0 || popup.rect.h < 0.0 {
                return Err("浮层矩形的宽高不可为负".into());
            }
            if let Some(ime) = popup.ime {
                ensure_rect(ime.x, ime.y, ime.w, ime.h, "浮层输入法光标")?;
            }
        }
        Ok(())
    }

    /// 命中区查询：坐标落在多个矩形时取最后声明的（同帧内后声明在上）。
    /// 返回命中区本身，调用方拿光标形状不必再按 id 扫一遍。
    pub fn hit_region_at(&self, x: f32, y: f32) -> Option<&HitRegion> {
        self.hits
            .iter()
            .rev()
            .find(|hit| x >= hit.x && x <= hit.x + hit.w && y >= hit.y && y <= hit.y + hit.h)
    }

    /// 命中区 id（见 [`Self::hit_region_at`]）。
    pub fn hit_at(&self, x: f32, y: f32) -> Option<&str> {
        self.hit_region_at(x, y).map(|hit| hit.id.as_str())
    }

    /// 某命中区的光标形状（未命中或缺省 = 系统默认）。
    pub fn hit_cursor(&self, id: &str) -> HitCursor {
        self.hits
            .iter()
            .find(|hit| hit.id == id)
            .and_then(|hit| hit.cursor)
            .unwrap_or_default()
    }
}

/// 命中区校验：数量上限、id 非空唯一可读、矩形有限非负。
fn validate_hits(hits: &[HitRegion]) -> Result<(), String> {
    if hits.len() > MAX_HITS {
        return Err(format!("命中区不得超过 {MAX_HITS} 个"));
    }
    let mut ids = std::collections::HashSet::with_capacity(hits.len());
    for hit in hits {
        if hit.id.trim().is_empty() {
            return Err("命中区 id 不能为空".into());
        }
        if hit.id.len() > MAX_HIT_ID_LEN {
            return Err(format!("命中区 id 不得超过 {MAX_HIT_ID_LEN} 字节"));
        }
        if hit.id.chars().any(char::is_control) {
            return Err(format!("命中区 id 不得含控制字符：{}", hit.id));
        }
        if !ids.insert(hit.id.as_str()) {
            return Err(format!("命中区 id 重复：{}", hit.id));
        }
        ensure_rect(hit.x, hit.y, hit.w, hit.h, &format!("命中区 {}", hit.id))?;
    }
    Ok(())
}

fn ensure_rect(x: f32, y: f32, w: f32, h: f32, name: &str) -> Result<(), String> {
    for (value, slot) in [(x, "x"), (y, "y"), (w, "宽"), (h, "高")] {
        if !value.is_finite() {
            return Err(format!("{name}的{slot}不是有限数值"));
        }
    }
    if w < 0.0 || h < 0.0 {
        return Err(format!("{name}的宽高不可为负"));
    }
    Ok(())
}

/// 键盘焦点事件与按键的归属：`Press` 含系统重复按键（长按方向键逐次到达），`Release` 在抬起时到达。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum KeyAction {
    Press,
    Release,
}

/// 按键：具名键（功能键与编辑键）或可打印字符。字符按布局产出（与用户所见一致），
/// 大小写与符号跟随 Shift，因此 `a` 与 `A` 是两个值、`1` 与 `!` 也是两个值。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Key {
    Named { name: NamedKey },
    Character { text: String },
}

/// 具名键。可打印字符走 [`Key::Character`]；这里放不可打印的编辑 / 功能键，外加空格
/// （它与编辑语义绑定，且布局差异下字符形态不稳定）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NamedKey {
    Escape,
    Enter,
    Tab,
    Space,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}

/// 修饰键状态（按键发生时的快照）：`control` = Ctrl（macOS 的 Control，不含 Command），
/// `meta` = Windows / Super / Command 键。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Modifiers {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shift: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub control: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub alt: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub meta: bool,
}

impl Modifiers {
    /// 四个修饰键都未按下（用于省略序列化字段）。
    pub fn is_empty(&self) -> bool {
        !self.shift && !self.control && !self.alt && !self.meta
    }
}

/// 鼠标按键。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// 宿主回发的原始输入。坐标 = 面本地逻辑像素；`hit` 为命中的命中区 id（未命中缺省）。
/// 插件按帧自行维护交互状态（悬停 / 按下 / 焦点 / 滚动位置 / 文本编辑）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[serde(rename_all_fields = "camelCase")]
pub enum FrameInput {
    PointerDown {
        x: f32,
        y: f32,
        button: MouseButton,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hit: Option<String>,
    },
    PointerUp {
        x: f32,
        y: f32,
        button: MouseButton,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hit: Option<String>,
    },
    PointerMove {
        x: f32,
        y: f32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hit: Option<String>,
    },
    /// 指针离开本面（从内到外边沿触发一次）。
    PointerLeave,
    /// 按在浮层外：宿主已按浮层声明的承接范围处理这次按下（`all` 时不再送往本面），
    /// 插件据此丢弃浮层状态。`x` / `y` 为窗口坐标（仅供描述位置）。
    Outside {
        x: f32,
        y: f32,
    },
    /// 同键二次按下（间隔与位移都在宿主阈值内），插件无需自己算。
    DoubleClick {
        x: f32,
        y: f32,
        button: MouseButton,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hit: Option<String>,
    },
    /// 滚轮增量统一为像素（宿主已把滚轮「行」折算成像素），正 = 向下 / 向右（Web 惯例，宿主已完成方向换算）。
    Wheel {
        x: f32,
        y: f32,
        delta_x: f32,
        delta_y: f32,
        /// 滚轮发生时的修饰键快照（Ctrl+滚轮缩放等）。
        #[serde(default, skip_serializing_if = "Modifiers::is_empty")]
        modifiers: Modifiers,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hit: Option<String>,
    },
    /// 键盘：`text` 为该键在输入法提交后可插入的文本（字符键才有，宿主已滤掉控制字符）。
    Key {
        key: Key,
        action: KeyAction,
        modifiers: Modifiers,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    /// 输入法：`composing` = 组合串更新（空串 = 组合结束，不插入文本），
    /// 否则 `text` 为已提交的文本；`caret` 为组合期的光标字节位；`candidates` 为候选窗锚点矩形。
    Text {
        text: String,
        composing: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caret: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        candidates: Option<FrameRect>,
    },
    /// 焦点进入 / 离开本面（按下即取焦，按在面外即失焦）。
    Focus { focused: bool },
    /// 面尺寸变化（逻辑像素）：插件据此重排并重绘。
    Resize { width: f32, height: f32 },
}

/// `host:frame.input` 的 params。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameInputParams {
    pub surface_id: String,
    pub input: FrameInput,
}

/// `host:frame.tick` 的 params：帧序号与距上一帧的毫秒数（首帧为 0）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameTickParams {
    pub surface_id: String,
    pub frame: u64,
    pub delta_ms: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paint::{ColorToken, PaintColor, TextAlign};

    fn hit(id: &str, x: f32, y: f32, w: f32, h: f32) -> HitRegion {
        HitRegion { id: id.into(), x, y, w, h, cursor: None }
    }

    #[test]
    fn frame_validation_checks_hits_and_commands() {
        let mut frame = SurfaceFrame {
            size: None,
            commands: vec![DrawCommand::Text {
                x: 0.0,
                y: 0.0,
                content: "计数器".into(),
                size: 14.0,
                color: PaintColor::Token { name: ColorToken::Text },
                weight: None,
                family: None,
                italic: false,
                max_width: Some(80.0),
                align: Some(TextAlign::Center),
                family_name: None,
                layout: None,
            }],
            hits: vec![hit("inc", 0.0, 0.0, 40.0, 24.0)],
            ime: None,
            keys: true,
            fps: Some(60),
            popup: None,
        };
        assert!(frame.validate().is_ok());

        frame.hits.push(hit("inc", 0.0, 0.0, 10.0, 10.0));
        assert!(frame.validate().unwrap_err().contains("重复"));
        frame.hits.pop();

        frame.hits.push(hit(" ", 0.0, 0.0, 10.0, 10.0));
        assert!(frame.validate().unwrap_err().contains("id 不能为空"));
        frame.hits.pop();

        frame.hits.push(hit("neg", 0.0, 0.0, -1.0, 10.0));
        assert!(frame.validate().is_err());
        frame.hits.pop();

        // 命中区 id 可承载仓库相对路径（中文一字 3 字节）：上限按 PATH_MAX 留足
        let long = format!("row:d:{}", "长目录/".repeat(60));
        assert!(long.len() > 64 && long.len() <= MAX_HIT_ID_LEN, "{}", long.len());
        frame.hits.push(hit(&long, 0.0, 0.0, 10.0, 10.0));
        assert!(frame.validate().is_ok(), "长路径 id 应通过校验");
        frame.hits.pop();
        let too_long = "x".repeat(MAX_HIT_ID_LEN + 1);
        frame.hits.push(hit(&too_long, 0.0, 0.0, 10.0, 10.0));
        assert!(frame.validate().unwrap_err().contains("不得超过"));
        frame.hits.pop();

        frame.fps = Some(0);
        assert!(frame.validate().unwrap_err().contains("帧率"));
        frame.fps = None;

        frame.commands = vec![DrawCommand::Text {
            x: 0.0,
            y: 0.0,
            content: "x".into(),
            size: 0.0,
            color: PaintColor::Token { name: ColorToken::Text },
            weight: None,
            family: None,
            italic: false,
            max_width: None,
            align: None,
            family_name: None,
            layout: None,
        }];
        assert!(frame.validate().unwrap_err().contains("第 0 条指令"));
    }

    #[test]
    fn hit_testing_prefers_last_declared() {
        let frame = SurfaceFrame {
            hits: vec![hit("panel", 0.0, 0.0, 100.0, 100.0), hit("button", 10.0, 10.0, 20.0, 20.0)],
            ..SurfaceFrame::default()
        };
        assert_eq!(frame.hit_at(15.0, 15.0), Some("button"));
        assert_eq!(frame.hit_at(90.0, 90.0), Some("panel"));
        assert_eq!(frame.hit_at(200.0, 0.0), None);
    }

    #[test]
    fn input_payload_shapes_are_tagged() {
        let down = FrameInput::PointerDown {
            x: 1.0,
            y: 2.0,
            button: MouseButton::Left,
            hit: Some("inc".into()),
        };
        assert_eq!(
            serde_json::to_value(&down).unwrap(),
            serde_json::json!({ "kind": "pointerDown", "x": 1.0, "y": 2.0, "button": "left", "hit": "inc" })
        );
        let ime = FrameInput::Text {
            text: "中".into(),
            composing: true,
            caret: Some(3),
            candidates: Some(FrameRect { x: 1.0, y: 2.0, w: 3.0, h: 4.0 }),
        };
        let wire = serde_json::to_value(&ime).unwrap();
        assert_eq!(wire["kind"], "text");
        assert_eq!(wire["candidates"]["h"], 4.0);
        let leave = FrameInput::PointerLeave;
        assert_eq!(serde_json::to_value(leave).unwrap(), serde_json::json!({ "kind": "pointerLeave" }));
    }
}
