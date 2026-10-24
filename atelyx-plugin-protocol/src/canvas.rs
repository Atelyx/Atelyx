//! 画布显示清单：插件提交绘图指令，宿主 canvas 逐条执行并转发指针事件；命中由插件按指令几何自算。
//! 指令集为封闭枚举，扩容随协议升版；坐标一律节点本地逻辑像素、左上原点。

use serde::{Deserialize, Serialize};

use crate::view::{ColorToken, ImageFit, ImageSource, TextWeight};

fn default_stroke_width() -> f32 {
    1.0
}

/// 颜色：主题 token（跟随宿主主题）或插件自带 RGBA（内容谱系色，如连线分类、播放头高亮）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
#[serde(rename_all_fields = "camelCase")]
pub enum CanvasColor {
    Token {
        name: ColorToken,
    },
    Rgba {
        /// `[r, g, b, a]`，各 0~255。
        rgba: [u8; 4],
    },
}

/// 二维坐标点（逻辑像素）。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CanvasPoint {
    pub x: f32,
    pub y: f32,
}

/// 绘图指令。`fill` 与 `stroke` 至少声明其一；`stroke_width` 只在 `stroke` 存在时生效（缺省 1.0）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[serde(rename_all_fields = "camelCase")]
pub enum DrawCommand {
    Line {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        stroke: CanvasColor,
        #[serde(default = "default_stroke_width")]
        width: f32,
    },
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        fill: Option<CanvasColor>,
        stroke: Option<CanvasColor>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke_width: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        corner_radius: Option<f32>,
    },
    Path {
        points: Vec<CanvasPoint>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        closed: bool,
        fill: Option<CanvasColor>,
        stroke: Option<CanvasColor>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke_width: Option<f32>,
    },
    Circle {
        cx: f32,
        cy: f32,
        r: f32,
        fill: Option<CanvasColor>,
        stroke: Option<CanvasColor>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke_width: Option<f32>,
    },
    /// 文字以 (x, y) 为左上角。宿主 canvas 的文字永远绘制在所有图层之上（图形引擎限制），
    /// 插件若需文字被后续图形遮盖，应改用宿主原生 Text 控件分层。
    Text {
        x: f32,
        y: f32,
        content: String,
        size: f32,
        color: CanvasColor,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        weight: Option<TextWeight>,
    },
    /// 图片贴片：绘制在给定矩形内。`fit` 缺省 = 拉伸填满；`contain` 等比完整可见（留空边），
    /// `cover` 等比覆盖并裁剪溢出部分。
    Image {
        source: ImageSource,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fit: Option<ImageFit>,
    },
}

/// 画布帧率上限（`canvas` 节点的 `fps` 上限，也是宿主调度帧通知的最细节拍）。
pub const MAX_CANVAS_FPS: u16 = 120;

/// 增量提交的一项：按 `id` 更新已有指令，或追加到清单末尾（`id` 由插件自定，同一画布内唯一）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CanvasUpsert {
    pub id: String,
    pub command: DrawCommand,
}

/// 增量条目的 `id` 上限（字节）。id 只在单块画布内定位，不需要长键。
pub(crate) const MAX_UPSERT_ID_LEN: usize = 64;

/// 增量条目校验：`id` 非空、不超过 64 字节且不含控制字符，指令本身与整表提交同口径
/// （逐条校验 + `fill` / `stroke` 至少声明其一）。
pub fn validate_upsert(item: &CanvasUpsert) -> Result<(), String> {
    if item.id.trim().is_empty() {
        return Err("增量条目的 id 不能为空".into());
    }
    if item.id.len() > MAX_UPSERT_ID_LEN {
        return Err(format!("增量条目的 id 不得超过 {MAX_UPSERT_ID_LEN} 字节"));
    }
    if item.id.chars().any(char::is_control) {
        return Err("增量条目的 id 不得含控制字符".into());
    }
    item.command.validate()?;
    if !item.command.has_paint() {
        return Err("fill 与 stroke 至少声明其一".into());
    }
    Ok(())
}

impl DrawCommand {
    fn validate(&self) -> Result<(), String> {
        match self {
            Self::Line { width, .. } => {
                ensure_positive(*width, "线宽")
            }
            Self::Rect { w, h, stroke_width, corner_radius, .. } => {
                ensure_non_negative(*w, "矩形宽")?;
                ensure_non_negative(*h, "矩形高")?;
                ensure_non_negative(corner_radius.unwrap_or(0.0), "圆角半径")?;
                ensure_positive(stroke_width.unwrap_or(1.0), "描边宽")
            }
            Self::Path { points, stroke_width, .. } => {
                if points.len() < 2 {
                    return Err("路径至少需要 2 个点".into());
                }
                ensure_positive(stroke_width.unwrap_or(1.0), "描边宽")
            }
            Self::Circle { r, stroke_width, .. } => {
                ensure_non_negative(*r, "圆半径")?;
                ensure_positive(stroke_width.unwrap_or(1.0), "描边宽")
            }
            Self::Text { size, .. } => ensure_positive(*size, "字号"),
            Self::Image { w, h, .. } => {
                ensure_non_negative(*w, "贴片宽")?;
                ensure_non_negative(*h, "贴片高")
            }
        }
    }

    /// 是否声明了绘制内容（填充或描边）。矩形 / 路径 / 圆两条都不带即是空指令。
    fn has_paint(&self) -> bool {
        match self {
            Self::Rect { fill, stroke, .. }
            | Self::Path { fill, stroke, .. }
            | Self::Circle { fill, stroke, .. } => fill.is_some() || stroke.is_some(),
            _ => true,
        }
    }
}

/// 清单整体校验：逐条报可读错误（错误信息带指令下标，插件可定位到条）。
pub fn validate_commands(commands: &[DrawCommand]) -> Result<(), String> {
    for (index, command) in commands.iter().enumerate() {
        command
            .validate()
            .map_err(|e| format!("第 {index} 条指令：{e}"))?;
        if !command.has_paint() {
            return Err(format!("第 {index} 条指令：fill 与 stroke 至少声明其一"));
        }
    }
    Ok(())
}

fn ensure_non_negative(value: f32, name: &str) -> Result<(), String> {
    if value.is_nan() || value < 0.0 {
        return Err(format!("{name}不可为负"));
    }
    Ok(())
}

fn ensure_positive(value: f32, name: &str) -> Result<(), String> {
    if value.is_nan() || value <= 0.0 {
        return Err(format!("{name}必须大于 0"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(v: [u8; 4]) -> Option<CanvasColor> {
        Some(CanvasColor::Rgba { rgba: v })
    }

    #[test]
    fn validation_reports_indexed_errors() {
        assert!(validate_commands(&[]).is_ok(), "空清单 = 清空画布，合法");
        assert!(validate_commands(&[DrawCommand::Rect {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
            fill: rgba([255, 0, 0, 255]),
            stroke: None,
            stroke_width: None,
            corner_radius: None,
        }])
        .is_ok());

        let empty = DrawCommand::Circle {
            cx: 0.0,
            cy: 0.0,
            r: 5.0,
            fill: None,
            stroke: None,
            stroke_width: None,
        };
        assert!(validate_commands(std::slice::from_ref(&empty)).unwrap_err().contains("至少声明其一"));

        let short = DrawCommand::Path {
            points: vec![CanvasPoint { x: 0.0, y: 0.0 }],
            closed: false,
            fill: None,
            stroke: rgba([0, 0, 0, 255]),
            stroke_width: None,
        };
        let valid = DrawCommand::Circle {
            cx: 0.0,
            cy: 0.0,
            r: 5.0,
            fill: None,
            stroke: rgba([0, 0, 0, 255]),
            stroke_width: None,
        };
        let err = validate_commands(&[valid, short]).unwrap_err();
        assert!(err.starts_with("第 1 条指令"), "错误须带指令下标：{err}");
    }

    #[test]
    fn color_encoding_is_frozen() {
        let token = CanvasColor::Token { name: ColorToken::Accent };
        assert_eq!(
            serde_json::to_value(token).unwrap(),
            serde_json::json!({"type": "token", "name": "accent"})
        );
        let rgba = CanvasColor::Rgba { rgba: [16, 32, 64, 255] };
        assert_eq!(
            serde_json::to_value(rgba).unwrap(),
            serde_json::json!({"type": "rgba", "rgba": [16, 32, 64, 255]})
        );
    }

    #[test]
    fn upsert_ids_are_validated() {
        let command = DrawCommand::Line {
            x1: 0.0,
            y1: 0.0,
            x2: 10.0,
            y2: 10.0,
            stroke: CanvasColor::Rgba { rgba: [0, 0, 0, 255] },
            width: 1.0,
        };
        assert!(validate_upsert(&CanvasUpsert { id: "row-1".into(), command: command.clone() }).is_ok());
        assert!(validate_upsert(&CanvasUpsert { id: "  ".into(), command: command.clone() }).is_err());
        assert!(validate_upsert(&CanvasUpsert { id: "a\nb".into(), command: command.clone() }).is_err());
        assert!(
            validate_upsert(&CanvasUpsert { id: "x".repeat(MAX_UPSERT_ID_LEN + 1), command: command.clone() }).is_err()
        );
        let broken = DrawCommand::Line {
            x1: 0.0,
            y1: 0.0,
            x2: 1.0,
            y2: 1.0,
            stroke: CanvasColor::Rgba { rgba: [0, 0, 0, 255] },
            width: 0.0,
        };
        assert!(validate_upsert(&CanvasUpsert { id: "row-1".into(), command: broken }).is_err(), "指令本身的校验不被 id 校验盖过");
    }

    #[test]
    fn image_fit_is_optional_and_roundtrips() {
        let source = ImageSource::Vault { path: "a.png".into() };
        let stretch = DrawCommand::Image {
            source: source.clone(),
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
            fit: None,
        };
        assert!(serde_json::to_value(&stretch).unwrap().get("fit").is_none(), "缺省不落字段");
        let contain = DrawCommand::Image {
            source,
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
            fit: Some(ImageFit::Contain),
        };
        let wire = serde_json::to_value(&contain).unwrap();
        assert_eq!(wire["fit"], "contain");
        assert_eq!(serde_json::from_value::<DrawCommand>(wire).unwrap(), contain);
    }
}
