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

/// 字重四档（正文 400 / 中等 500 / 半粗 600 / 粗 700）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TextWeight {
    Normal,
    Medium,
    SemiBold,
    Bold,
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
    /// 画布 / 内容区的底色（比次级表面深一档）。
    Background,
    /// 最弱的分隔线（格线、列表内分隔）。
    BorderSubtle,
    /// 文本选区底。
    Selection,
    /// 模态遮罩（灯箱 / 确认层）。
    Scrim,
    /// 无底色 / 无边框。
    Transparent,
    /// 浮层表面（菜单 / 提示条底，比面板底亮一档）。
    SurfaceOverlay,
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
#[serde(rename_all_fields = "camelCase")]
pub enum ImageSource {
    Private { path: String },
    Vault {
        path: String,
        /// 缩略图长边上限（像素）：给定时宿主按 EXIF 方向摆正并等比缩到该长边以内再上传，
        /// 同一路径的不同上限各占一份缓存。缺省 = 原图。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_edge: Option<u32>,
    },
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

/// 换行口径：`word` 只在词间断行（超长词溢出），`wordOrGlyph` 超长词退化为逐字形断行。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TextWrap {
    Word,
    WordOrGlyph,
}

/// 字形整形策略：`auto` 纯 ASCII 走基础整形、含非 ASCII 走高级整形（含字体回退）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TextShaping {
    Auto,
    Basic,
    Advanced,
}

/// 文字排版口径：量算、换行与绘制共用同一份，缺省项取宿主默认。
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextLayout {
    /// 相对字号的行高倍数（缺省 = 宿主默认行高）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_height: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrap: Option<TextWrap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shaping: Option<TextShaping>,
}

impl TextLayout {
    /// 全部取默认口径（等于不声明）。
    pub fn is_default(&self) -> bool {
        self.line_height.is_none() && self.wrap.is_none() && self.shaping.is_none()
    }
}

/// 矩形投影：偏移、模糊半径与颜色。模糊按宿主渲染层的 quad 阴影实现，只对矩形生效。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaintShadow {
    #[serde(default)]
    pub offset_x: f32,
    #[serde(default)]
    pub offset_y: f32,
    #[serde(default)]
    pub blur: f32,
    pub color: PaintColor,
}

/// 绘图指令。矩形 / 路径 / 圆：`fill` 与 `stroke` 至少声明其一，`stroke_width` 只在 `stroke`
/// 存在时生效（缺省 1.0）；线只有 `stroke`（字段名 `width`）；文字与图片没有描边概念。
/// 裁剪：`PushClip` 之后的指令按该矩形与已压栈矩形的交集绘制，`PopClip` 弹出最近一层。
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
        /// 虚线：`[实线长, 空白长]`（像素，沿线的行程计）。缺省 = 实线。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dash: Option<[f32; 2]>,
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
        /// 投影：只对矩形生效，绘制在填充与描边之下。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shadow: Option<PaintShadow>,
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
        /// 斜体：字族没有斜体字形时按合成倾斜绘制（强调正文里的 `*斜体*` 用它）。
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        italic: bool,
        /// 显式字体族名（用户在单元格样式里选的系统字体）：给出时优先于 `family`，
        /// 该族不存在则回落 `family` 口径。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        family_name: Option<String>,
        /// 可用宽度：给定即按该宽度换行，并作为 `align` 的对齐区域。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_width: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        align: Option<TextAlign>,
        /// 排版口径（行高倍数 / 换行 / 整形）：与 `query.measure` 的同一项同口径，
        /// 量出来的行高与折行点才对得上绘制结果。缺省 = 宿主默认。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        layout: Option<TextLayout>,
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
    /// 压入裁剪矩形：之后的指令只画在该矩形内，嵌套时取交集。
    PushClip { x: f32, y: f32, w: f32, h: f32 },
    /// 弹出最近一次 `PushClip`；与 `PushClip` 必须成对出现。
    PopClip,
}

impl DrawCommand {
    fn validate(&self) -> Result<(), String> {
        match self {
            Self::Line { width, dash, .. } => {
                ensure_positive(*width, "线宽")?;
                if let Some([on, off]) = dash {
                    ensure_positive(*on, "虚线段长")?;
                    ensure_non_negative(*off, "虚线间隔")?;
                }
                Ok(())
            }
            Self::Rect { w, h, stroke_width, corner_radius, shadow, .. } => {
                ensure_non_negative(*w, "矩形宽")?;
                ensure_non_negative(*h, "矩形高")?;
                ensure_non_negative(corner_radius.unwrap_or(0.0), "圆角半径")?;
                ensure_positive(stroke_width.unwrap_or(1.0), "描边宽")?;
                if let Some(shadow) = shadow {
                    ensure_finite(shadow.offset_x, "阴影横向偏移")?;
                    ensure_finite(shadow.offset_y, "阴影纵向偏移")?;
                    ensure_non_negative(shadow.blur, "阴影模糊半径")?;
                }
                Ok(())
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
            Self::Text { size, max_width, align, layout, .. } => {
                ensure_positive(*size, "字号")?;
                if let Some(max_width) = max_width {
                    ensure_positive(*max_width, "文字可用宽度")?;
                }
                if align.is_some() && max_width.is_none() {
                    return Err("文字对齐须与 maxWidth 同时给出".into());
                }
                if let Some(layout) = layout {
                    if let Some(line_height) = layout.line_height {
                        ensure_positive(line_height, "文字行高倍数")?;
                    }
                    if layout.wrap.is_some() && max_width.is_none() {
                        return Err("换行口径须与 maxWidth 同时给出".into());
                    }
                }
                Ok(())
            }
            Self::Image { w, h, .. } => {
                ensure_non_negative(*w, "贴片宽")?;
                ensure_non_negative(*h, "贴片高")
            }
            Self::PushClip { x, y, w, h } => {
                ensure_finite(*x, "裁剪左缘")?;
                ensure_finite(*y, "裁剪上缘")?;
                ensure_non_negative(*w, "裁剪宽")?;
                ensure_non_negative(*h, "裁剪高")
            }
            Self::PopClip => Ok(()),
        }
    }

    /// 是否声明了绘制内容（填充或描边）。矩形 / 路径 / 圆两条都不带即是空指令。
    fn has_paint(&self) -> bool {
        match self {
            Self::Rect { fill, stroke, .. }
            | Self::Path { fill, stroke, .. }
            | Self::Circle { fill, stroke, .. } => fill.is_some() || stroke.is_some(),
            Self::PushClip { .. } | Self::PopClip => false,
            _ => true,
        }
    }
}

/// 清单整体校验：逐条报可读错误（错误信息带指令下标，插件可定位到条）。
pub fn validate_commands(commands: &[DrawCommand]) -> Result<(), String> {
    let mut clip_depth = 0usize;
    for (index, command) in commands.iter().enumerate() {
        command.validate().map_err(|e| format!("第 {index} 条指令：{e}"))?;
        match command {
            DrawCommand::PushClip { .. } => clip_depth += 1,
            DrawCommand::PopClip if clip_depth == 0 => {
                return Err(format!("第 {index} 条指令：PopClip 没有对应的 PushClip"));
            }
            DrawCommand::PopClip => clip_depth -= 1,
            _ if !command.has_paint() => {
                return Err(format!("第 {index} 条指令：fill 与 stroke 至少声明其一"));
            }
            _ => {}
        }
    }
    if clip_depth != 0 {
        return Err(format!("裁剪栈未闭合：还有 {clip_depth} 层 PushClip 未弹出"));
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

fn ensure_finite(value: f32, name: &str) -> Result<(), String> {
    if !value.is_finite() {
        return Err(format!("{name}必须是有限数"));
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
            italic: false,
            max_width,
            align,
            family_name: None,
            layout: None,
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
            shadow: None,
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
    fn clip_stack_must_balance() {
        let push = DrawCommand::PushClip { x: 0.0, y: 0.0, w: 10.0, h: 10.0 };
        assert!(validate_commands(&[push.clone(), DrawCommand::PopClip]).is_ok());
        assert!(validate_commands(&[push.clone(), push.clone(), DrawCommand::PopClip, DrawCommand::PopClip]).is_ok());

        let err = validate_commands(&[DrawCommand::PopClip]).unwrap_err();
        assert!(err.contains("没有对应的 PushClip"), "多余的出栈须报错：{err}");

        let err = validate_commands(&[push]).unwrap_err();
        assert!(err.contains("未弹出"), "未闭合的裁剪栈须报错：{err}");
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
    fn rect_shadow_is_optional_and_validated() {
        let shaded = DrawCommand::Rect {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
            fill: rgba([255, 255, 255, 255]),
            stroke: None,
            stroke_width: None,
            corner_radius: Some(6.0),
            shadow: Some(PaintShadow {
                offset_x: 0.0,
                offset_y: 4.0,
                blur: 12.0,
                color: PaintColor::Rgba { rgba: [0, 0, 0, 48] },
            }),
        };
        let wire = serde_json::to_value(&shaded).unwrap();
        assert_eq!(
            wire["shadow"],
            serde_json::json!({
                "offsetX": 0.0,
                "offsetY": 4.0,
                "blur": 12.0,
                "color": { "type": "rgba", "rgba": [0, 0, 0, 48] }
            })
        );
        assert_eq!(serde_json::from_value::<DrawCommand>(wire).unwrap(), shaded);
        assert!(validate_commands(&[shaded]).is_ok());

        let negative_blur = DrawCommand::Rect {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
            fill: rgba([255, 255, 255, 255]),
            stroke: None,
            stroke_width: None,
            corner_radius: None,
            shadow: Some(PaintShadow {
                offset_x: 0.0,
                offset_y: 0.0,
                blur: -1.0,
                color: PaintColor::Rgba { rgba: [0, 0, 0, 48] },
            }),
        };
        assert!(validate_commands(&[negative_blur]).is_err());

        let plain = DrawCommand::Rect {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
            fill: rgba([255, 255, 255, 255]),
            stroke: None,
            stroke_width: None,
            corner_radius: None,
            shadow: None,
        };
        assert!(serde_json::to_value(plain).unwrap().get("shadow").is_none(), "缺省不落字段");
    }

    #[test]
    fn image_fit_is_optional_and_roundtrips() {
        let source = ImageSource::Vault { path: "a.png".into(), max_edge: None };
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
