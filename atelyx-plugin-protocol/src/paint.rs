//! 绘制指令集：插件把界面画成指令，宿主逐条执行。指令集为封闭枚举，扩容随协议升版；
//! 坐标一律面本地逻辑像素、左上原点。命中由插件声明的命中区（见 [`crate::frame`]）表达，
//! 绘制指令本身不带交互语义。

use serde::{Deserialize, Serialize};

/// 帧率上限（帧声明的 `fps` 上限，也是宿主调度帧通知的最细节拍）。
pub const MAX_FPS: u16 = 120;

/// 文字水平对齐：以 `maxWidth` 为对齐区域、`x` 为区域左缘，按量算出的整段宽度换算
/// （换行后段内各行仍左对齐）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TextAlign {
    Start,
    Center,
    End,
}

/// 字体族：界面字体（跟随用户设置）或系统等宽族。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TextFamily {
    Ui,
    Mono,
}

/// 字重三档。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TextWeight {
    Normal,
    Medium,
    SemiBold,
}

/// 主题 token：取色引用宿主调色板，主题变化时自动跟随。
/// `accentSoft` / `warningSoft` / `dangerSoft` / `successSoft` 为对应语义色的低饱和底，
/// `surface*` / `hover` / `border*` 为层次底色与边界。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ColorToken {
    Text,
    TextSecondary,
    TextMuted,
    Accent,
    AccentSoft,
    Danger,
    DangerSoft,
    Success,
    SuccessSoft,
    Warning,
    WarningSoft,
    /// 内嵌表面（比面板底色深一档：输入框、内嵌卡片）。
    Surface,
    /// 次级表面（面板头 / 工具条底）。
    SurfaceMuted,
    /// 最深表面（内陷区域）。
    SurfaceSunken,
    /// 悬停反馈底色。
    Hover,
    Border,
    BorderStrong,
    /// 无底色 / 无边框。
    Transparent,
}

/// 图片填充方式：`contain` 等比完整可见（留空边），`cover` 等比覆盖并裁剪溢出，缺省 = 拉伸填满。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImageFit {
    Contain,
    Cover,
    Fill,
}

/// 图片来源：`private` = 插件私有目录内相对路径；`vault` = 仓库根相对路径（宿主按仓库内容读取，
/// 协作空间走同构内容面）；`url` = http(s) 地址（缺省按公网策略下载并缓存，`local` 改为本机 /
/// 局域网策略）；`file` = 本机绝对路径（插件自己读得到的文件，交给宿主显示）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ImageSource {
    Private { path: String },
    Vault { path: String },
    Url {
        url: String,
        /// 按本机 / 局域网策略取图：放行回环与私网、仅拒链路本地与广播。
        /// 缺省 = 公网策略（拒回环与私网）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        local: Option<bool>,
    },
    /// 本机绝对路径。插件要读它的内容本来就能经 `fs` 自己读，这里不增加也不收紧范围。
    File { path: String },
}

/// 颜色：主题 token（跟随宿主主题）或插件自带 RGBA（内容谱系色，如连线分类、播放头高亮）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
#[serde(rename_all_fields = "camelCase")]
pub enum PaintColor {
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
pub struct PaintPoint {
    pub x: f32,
    pub y: f32,
}

fn default_stroke_width() -> f32 {
    1.0
}

/// 绘图指令。矩形 / 路径 / 圆：`fill` 与 `stroke` 至少声明其一，`stroke_width` 只在 `stroke`
/// 存在时生效（缺省 1.0）；线只有 `stroke`（字段名 `width`）；文字与图片没有描边概念。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[serde(rename_all_fields = "camelCase")]
pub enum DrawCommand {
    Line {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        stroke: PaintColor,
        #[serde(default = "default_stroke_width")]
        width: f32,
    },
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        fill: Option<PaintColor>,
        stroke: Option<PaintColor>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke_width: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        corner_radius: Option<f32>,
    },
    Path {
        points: Vec<PaintPoint>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        closed: bool,
        fill: Option<PaintColor>,
        stroke: Option<PaintColor>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke_width: Option<f32>,
    },
    Circle {
        cx: f32,
        cy: f32,
        r: f32,
        fill: Option<PaintColor>,
        stroke: Option<PaintColor>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke_width: Option<f32>,
    },
    /// 文字以 (x, y) 为左上角。给 `maxWidth` 时按该宽度换行并作为对齐区域（`x` 为区域左缘）；
    /// `align` 只在给了 `maxWidth` 时成立，缺省 `start`。
    Text {
        x: f32,
        y: f32,
        content: String,
        size: f32,
        color: PaintColor,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        weight: Option<TextWeight>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        family: Option<TextFamily>,
        /// 可用宽度：给定即按该宽度换行，并作为 `align` 的对齐区域。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_width: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        align: Option<TextAlign>,
    },
    /// 图片贴片：绘制在给定矩形内。
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

impl DrawCommand {
    fn validate(&self) -> Result<(), String> {
        match self {
            Self::Line { width, .. } => ensure_positive(*width, "线宽"),
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
            Self::Text { size, max_width, align, .. } => {
                ensure_positive(*size, "字号")?;
                if let Some(max_width) = max_width {
                    ensure_positive(*max_width, "文字可用宽度")?;
                }
                if align.is_some() && max_width.is_none() {
                    return Err("文字对齐须与 maxWidth 同时给出".into());
                }
                Ok(())
            }
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
        command.validate().map_err(|e| format!("第 {index} 条指令：{e}"))?;
        if !command.has_paint() {
            return Err(format!("第 {index} 条指令：fill 与 stroke 至少声明其一"));
        }
    }
    Ok(())
}

fn ensure_non_negative(value: f32, name: &str) -> Result<(), String> {
    if !value.is_finite() || value < 0.0 {
        return Err(format!("{name}不可为负"));
    }
    Ok(())
}

fn ensure_positive(value: f32, name: &str) -> Result<(), String> {
    if !value.is_finite() || value <= 0.0 {
        return Err(format!("{name}必须大于 0"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(v: [u8; 4]) -> Option<PaintColor> {
        Some(PaintColor::Rgba { rgba: v })
    }

    fn text(max_width: Option<f32>, align: Option<TextAlign>) -> DrawCommand {
        DrawCommand::Text {
            x: 0.0,
            y: 0.0,
            content: "示例".into(),
            size: 14.0,
            color: PaintColor::Token { name: ColorToken::Text },
            weight: Some(TextWeight::SemiBold),
            family: Some(TextFamily::Mono),
            max_width,
            align,
        }
    }

    #[test]
    fn validation_reports_indexed_errors() {
        assert!(validate_commands(&[]).is_ok(), "空清单 = 空面，合法");
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
        assert!(validate_commands(std::slice::from_ref(&empty))
            .unwrap_err()
            .contains("至少声明其一"));

        let short = DrawCommand::Path {
            points: vec![PaintPoint { x: 0.0, y: 0.0 }],
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
        let token = PaintColor::Token { name: ColorToken::Accent };
        assert_eq!(
            serde_json::to_value(token).unwrap(),
            serde_json::json!({"type": "token", "name": "accent"})
        );
        let rgba = PaintColor::Rgba { rgba: [16, 32, 64, 255] };
        assert_eq!(
            serde_json::to_value(rgba).unwrap(),
            serde_json::json!({"type": "rgba", "rgba": [16, 32, 64, 255]})
        );
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
        let contain = DrawCommand::Image { source, x: 0.0, y: 0.0, w: 10.0, h: 10.0, fit: Some(ImageFit::Contain) };
        let wire = serde_json::to_value(&contain).unwrap();
        assert_eq!(wire["fit"], "contain");
        assert_eq!(serde_json::from_value::<DrawCommand>(wire).unwrap(), contain);
    }

    #[test]
    fn text_alignment_requires_a_width() {
        assert!(validate_commands(&[text(None, None)]).is_ok());
        assert!(validate_commands(&[text(Some(100.0), Some(TextAlign::Center))]).is_ok());
        let err = validate_commands(&[text(None, Some(TextAlign::End))]).unwrap_err();
        assert!(err.contains("maxWidth"), "对齐缺宽度须拒收：{err}");
        assert!(validate_commands(&[text(Some(0.0), None)]).is_err());
        let wire = serde_json::to_value(text(None, None)).unwrap();
        assert!(wire.get("maxWidth").is_none() && wire.get("align").is_none());
        assert_eq!(wire["family"], "mono");
    }
}
