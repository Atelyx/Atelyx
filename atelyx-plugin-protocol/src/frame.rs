//! 面帧：插件把整个面画成一份指令清单 + 命中区，宿主执行绘制并把原始输入原样回发。
//! 坐标一律面本地逻辑像素、左上原点；宿主不理解命中区语义，只按几何做命中与光标形状。

use serde::{Deserialize, Serialize};

use crate::canvas::{validate_commands, DrawCommand, MAX_CANVAS_FPS};
use crate::view::{Key, KeyAction, Modifiers, MouseButton};

/// 命中区数量上限（单帧）。
pub const MAX_HITS: usize = 4096;
/// 命中区 id 上限（字节）。
pub const MAX_HIT_ID_LEN: usize = 64;

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

/// 一面一帧的完整内容。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceFrame {
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
}

impl SurfaceFrame {
    /// 挂载前校验：指令逐条、命中区 id 非空唯一、矩形有限非负、帧率在范围内。
    pub fn validate(&self) -> Result<(), String> {
        validate_commands(&self.commands)?;
        if self.hits.len() > MAX_HITS {
            return Err(format!("命中区不得超过 {MAX_HITS} 个"));
        }
        let mut ids = std::collections::HashSet::new();
        for hit in &self.hits {
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
        if let Some(ime) = self.ime {
            ensure_rect(ime.x, ime.y, ime.w, ime.h, "输入法光标")?;
        }
        if let Some(fps) = self.fps {
            if !(1..=MAX_CANVAS_FPS).contains(&fps) {
                return Err(format!("帧率须在 1–{MAX_CANVAS_FPS} 之间"));
            }
        }
        Ok(())
    }

    /// 命中区查询：坐标落在多个矩形时取最后声明的（同帧内后声明在上）。
    pub fn hit_at(&self, x: f32, y: f32) -> Option<&str> {
        self.hits
            .iter()
            .rev()
            .find(|hit| x >= hit.x && x <= hit.x + hit.w && y >= hit.y && y <= hit.y + hit.h)
            .map(|hit| hit.id.as_str())
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

/// `frame.submit` 的 params：整面重建（一帧即当前完整界面，未提交过的面显示占位）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameSubmitParams {
    pub surface_id: String,
    #[serde(flatten)]
    pub frame: SurfaceFrame,
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
    use crate::canvas::{CanvasColor, TextAlign};

    fn hit(id: &str, x: f32, y: f32, w: f32, h: f32) -> HitRegion {
        HitRegion { id: id.into(), x, y, w, h, cursor: None }
    }

    #[test]
    fn frame_validation_checks_hits_and_commands() {
        let mut frame = SurfaceFrame {
            commands: vec![DrawCommand::Text {
                x: 0.0,
                y: 0.0,
                content: "计数器".into(),
                size: 14.0,
                color: CanvasColor::Token { name: crate::view::ColorToken::Text },
                weight: None,
                family: None,
                max_width: Some(80.0),
                align: Some(TextAlign::Center),
            }],
            hits: vec![hit("inc", 0.0, 0.0, 40.0, 24.0)],
            ime: None,
            keys: true,
            fps: Some(60),
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

        frame.fps = Some(0);
        assert!(frame.validate().unwrap_err().contains("帧率"));
        frame.fps = None;

        frame.commands = vec![DrawCommand::Text {
            x: 0.0,
            y: 0.0,
            content: "x".into(),
            size: 0.0,
            color: CanvasColor::Token { name: crate::view::ColorToken::Text },
            weight: None,
            family: None,
            max_width: None,
            align: None,
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
    fn frame_wire_shape_is_flat_and_defaults_are_omitted() {
        let params = FrameSubmitParams {
            surface_id: "board".into(),
            frame: SurfaceFrame {
                commands: vec![],
                hits: vec![HitRegion {
                    id: "cell-1".into(),
                    x: 1.0,
                    y: 2.0,
                    w: 3.0,
                    h: 4.0,
                    cursor: Some(HitCursor::Pointer),
                }],
                ..SurfaceFrame::default()
            },
        };
        let wire = serde_json::to_value(&params).unwrap();
        assert_eq!(wire["surfaceId"], "board");
        assert!(wire.get("frame").is_none(), "帧内容平铺在 params 上");
        assert_eq!(wire["hits"][0]["cursor"], "pointer");
        assert!(wire.get("keys").is_none() && wire.get("ime").is_none());
        assert_eq!(serde_json::from_value::<FrameSubmitParams>(wire).unwrap(), params);
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
