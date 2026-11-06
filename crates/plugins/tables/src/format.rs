//! 格式气泡：选中格上的样式工具条（字体 / 字号 / B I U S / 字色 / 底色 / 清除）。
//! 以窗口级浮层提交（不随面裁剪，可盖住面板头部）；条目命中由交互层按几何判定。

use atelyx_format::table::ops::{self, KeyOp, OptState, StylePatch, StyleSummary};
use atelyx_plugin_protocol::frame::{
    FrameRect, HitCursor, HitRegion, PopupCapture, PopupLayer, SurfacePopup,
};
use atelyx_plugin_protocol::paint::{ColorToken, DrawCommand, PaintColor};

use crate::model::{Rect, Session, Size};

/// 气泡内边距与条目间距（px）。
const BAR_PAD_X: f32 = 6.0;
const BAR_PAD_Y: f32 = 4.0;
const BAR_GAP: f32 = 2.0;
const BAR_BTN_PAD_X: f32 = 6.0;
/// 气泡与选区的间距（px）。
const BAR_GAP_SELECTION: f32 = 4.0;
/// 估宽余量（长字体族名可能低估）。
const BAR_W_MARGIN: f32 = 16.0;

/// 气泡条目。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    Font,
    Size,
    Separator,
    Bold,
    Italic,
    Underline,
    Strike,
    Color,
    Background,
    Clear,
}

/// 条目顺序：字体、字号、分隔、B/I/U/S、分隔、字色、底色、分隔、清除。
const ITEMS: &[Item] = &[
    Item::Font,
    Item::Size,
    Item::Separator,
    Item::Bold,
    Item::Italic,
    Item::Underline,
    Item::Strike,
    Item::Separator,
    Item::Color,
    Item::Background,
    Item::Separator,
    Item::Clear,
];

/// 气泡命中结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatHit {
    Font,
    Size,
    Bold,
    Italic,
    Underline,
    Strike,
    Color,
    Background,
    Clear,
    /// 气泡内未落在条目上（内边距 / 条目间隙 / 分隔线）：消费按压，不改样式。
    Blank,
}

impl FormatHit {
    fn of(item: Item) -> Option<Self> {
        Some(match item {
            Item::Font => Self::Font,
            Item::Size => Self::Size,
            Item::Bold => Self::Bold,
            Item::Italic => Self::Italic,
            Item::Underline => Self::Underline,
            Item::Strike => Self::Strike,
            Item::Color => Self::Color,
            Item::Background => Self::Background,
            Item::Clear => Self::Clear,
            Item::Separator => return None,
        })
    }
}

/// 选区样式汇总（无选区 = None）：逐属性判定全体同值还是 Mixed，气泡与下拉的当前值同源。
pub fn selection_style(session: &Session) -> Option<StyleSummary> {
    let region = ops::region_of(
        session.view.selection.as_ref(),
        &session.data.fields,
        &session.data.rows,
    )?;
    Some(ops::style_summary(
        &session.data.fields,
        &session.data.rows,
        &region,
    ))
}

/// 条目文字（字体 / 字号按汇总结论显示：混合回落占位词，缺省显示“默认”）。
fn label(item: Item, summary: &StyleSummary) -> String {
    match item {
        Item::Font => match &summary.font {
            OptState::Mixed => "字体".to_string(),
            OptState::Value(Some(name)) if !name.is_empty() => font_label(name),
            OptState::Value(_) => "默认".to_string(),
        },
        Item::Size => match summary.size {
            OptState::Value(Some(size)) => format!("{}", size as i32),
            _ => "字号".to_string(),
        },
        Item::Bold => "B".to_string(),
        Item::Italic => "I".to_string(),
        Item::Underline => "U".to_string(),
        Item::Strike => "S".to_string(),
        Item::Separator | Item::Color | Item::Background | Item::Clear => String::new(),
    }
}

/// 字体值 → 显示名：预设键用中文名，已装族名原样显示。
fn font_label(value: &str) -> String {
    match value {
        "default" => "默认".to_string(),
        "sans" => "无衬线".to_string(),
        "serif" => "衬线".to_string(),
        "mono" => "等宽".to_string(),
        "hei" => "黑体".to_string(),
        "song" => "宋体".to_string(),
        "kai" => "楷体".to_string(),
        "fang" => "仿宋".to_string(),
        other => other.to_string(),
    }
}

/// 条目宽（估算：字号按字符宽度累加）。
fn item_width(item: Item, summary: &StyleSummary) -> f32 {
    let size = crate::theme::font_caption();
    match item {
        Item::Font => (label(Item::Font, summary).chars().count() as f32 * size * 0.55)
            .clamp(40.0, 120.0)
            + BAR_BTN_PAD_X * 2.0,
        Item::Size => 40.0,
        Item::Separator => 1.0 + BAR_GAP * 2.0,
        Item::Color | Item::Background => 22.0,
        Item::Clear => 24.0,
        _ => 22.0,
    }
}

/// 布尔三态：全开 / 混合（部分开）。
fn tri(state: &OptState<bool>) -> (bool, bool) {
    match state {
        OptState::Value(Some(true)) => (true, false),
        OptState::Mixed => (false, true),
        _ => (false, false),
    }
}

/// 颜色色块值：全体同值才取色，缺省 / 混合回落到占位 token。
fn swatch_fill(state: &OptState<String>) -> Option<[u8; 4]> {
    match state {
        OptState::Value(Some(value)) => crate::text::parse_hex(value),
        _ => None,
    }
}

/// 气泡矩形（面本地坐标）：锚在选区左上、向上展开。面尺寸只用于横向避让，
/// 不收缩宽高、不限 y——越出面矩形的部分由窗口级浮层按窗口裁剪。
pub fn bar_rect(session: &Session, size: Size) -> Option<Rect> {
    let region = ops::region_of(
        session.view.selection.as_ref(),
        &session.data.fields,
        &session.data.rows,
    )?;
    let summary = selection_style(session)?;
    let width = ITEMS
        .iter()
        .map(|item| item_width(*item, &summary))
        .sum::<f32>()
        + BAR_PAD_X * 2.0
        + BAR_W_MARGIN;
    let label_h = crate::theme::font_caption() * 16.0 / 12.0;
    let height = label_h + 2.0 * (BAR_PAD_Y + 3.0);
    let geo = crate::geom::geo(
        &session.view.layout,
        session.view.scroll_x,
        session.view.scroll_y,
        session.view.zoom,
        size,
    )?;
    let first = geo.cell_rect(region.row_start, region.col_start);
    let left = first.x.max(geo.grid_left());
    // 右侧放不下就整体左移；面比气泡还窄时贴面左缘铺开，宽度保持完整
    let x = left.min((size.width - width - 6.0).max(6.0));
    let y = first.y - height - BAR_GAP_SELECTION;
    Some(Rect::new(x, y, width, height))
}

/// 气泡是否可见（编辑中 / 拖拽中 / 灯箱打开时不显示）。
pub fn visible(session: &Session) -> bool {
    session.view.bar_shown
        && session.view.selection.is_some()
        && session.view.editing.is_none()
        && session.view.lightbox.is_none()
        && session.view.press.is_none()
}

/// 命中：气泡外返回 `None`；气泡内落在条目上返回条目，否则返回 [`FormatHit::Blank`]。
pub fn hit(session: &Session, size: Size, point: crate::model::Point) -> Option<FormatHit> {
    if !visible(session) {
        return None;
    }
    let rect = bar_rect(session, size)?;
    if !(point.x >= rect.x && point.x <= rect.right() && point.y >= rect.y && point.y <= rect.bottom()) {
        return None;
    }
    let summary = selection_style(session)?;
    let mut x = rect.x + BAR_PAD_X;
    for item in ITEMS {
        let width = item_width(*item, &summary);
        if point.x >= x && point.x <= x + width {
            if let Some(hit) = FormatHit::of(*item) {
                return Some(hit);
            }
            break;
        }
        x += width + BAR_GAP;
    }
    Some(FormatHit::Blank)
}

/// 气泡窗口级浮层；不可见时返回 `None`（本帧不带浮层即收起）。
pub fn popup_frame(session: &Session, size: Size) -> Option<SurfacePopup> {
    if !visible(session) {
        return None;
    }
    let rect = bar_rect(session, size)?;
    let summary = selection_style(session)?;
    let mut commands = Vec::new();
    paint_into(&mut commands, session, size);
    let mut hits = Vec::new();
    let mut x = rect.x + BAR_PAD_X;
    for (index, item) in ITEMS.iter().enumerate() {
        let width = item_width(*item, &summary);
        if FormatHit::of(*item).is_some() {
            hits.push(HitRegion {
                id: format!("format:{index}"),
                x,
                y: rect.y,
                w: width,
                h: rect.h,
                cursor: Some(HitCursor::Pointer),
            });
        }
        x += width + BAR_GAP;
    }
    Some(SurfacePopup {
        rect: FrameRect { x: rect.x, y: rect.y, w: rect.w, h: rect.h },
        capture: PopupCapture::Inside,
        layer: PopupLayer::Window,
        order: 1,
        commands,
        hits,
        ime: None,
    })
}

/// 绘制气泡：底 + 边框 + 条目（B/I/U/S 按当前样式高亮，颜色条目画色块）。
fn paint_into(out: &mut Vec<DrawCommand>, session: &Session, size: Size) {
    if !visible(session) {
        return;
    }
    let Some(rect) = bar_rect(session, size) else { return };
    let Some(summary) = selection_style(session) else { return };
    out.push(DrawCommand::Rect {
        x: rect.x,
        y: rect.y,
        w: rect.w,
        h: rect.h,
        fill: Some(PaintColor::Token { name: ColorToken::SurfaceOverlay }),
        stroke: Some(PaintColor::Token { name: ColorToken::Border }),
        stroke_width: Some(1.0),
        corner_radius: Some(6.0),
        shadow: None,
    });
    let label_h = crate::theme::font_caption() * 16.0 / 12.0;
    let mut x = rect.x + BAR_PAD_X;
    let text_y = rect.y + BAR_PAD_Y + (rect.h - 2.0 * BAR_PAD_Y - label_h) / 2.0;
    for item in ITEMS {
        let item = *item;
        let width = item_width(item, &summary);
        match item {
            Item::Separator => {
                out.push(DrawCommand::Line {
                    x1: x + BAR_GAP,
                    y1: rect.y + BAR_PAD_Y + 3.0,
                    x2: x + BAR_GAP,
                    y2: rect.bottom() - BAR_PAD_Y - 3.0,
                    stroke: PaintColor::Token { name: ColorToken::Border },
                    width: 1.0,
                    dash: None,
                });
            }
            Item::Color | Item::Background => {
                let fill = match item {
                    Item::Color => swatch_fill(&summary.color),
                    _ => swatch_fill(&summary.bg),
                };
                let swatch = fill.map(|rgba| PaintColor::Rgba { rgba }).unwrap_or(PaintColor::Token {
                    name: if item == Item::Color { ColorToken::Text } else { ColorToken::Hover },
                });
                out.push(DrawCommand::Rect {
                    x: x + 3.0,
                    y: rect.y + (rect.h - 14.0) / 2.0,
                    w: width - 6.0,
                    h: 14.0,
                    fill: Some(swatch),
                    stroke: Some(PaintColor::Token { name: ColorToken::Border }),
                    stroke_width: Some(1.0),
                    corner_radius: Some(3.0),
                    shadow: None,
                });
                if item == Item::Background {
                    out.push(DrawCommand::Text {
                        x: x + width / 2.0 - 3.0,
                        y: text_y,
                        content: "底".to_string(),
                        size: crate::theme::font_micro(),
                        color: PaintColor::Token { name: ColorToken::TextMuted },
                        weight: None,
                        family: None,
                        family_name: None,
                        italic: false,
                        max_width: Some(width),
                        align: Some(atelyx_plugin_protocol::paint::TextAlign::Center),
                        layout: None,
                    });
                }
            }
            Item::Clear => {
                // 橡皮：两条斜线组成的简化图形
                let cx = x + width / 2.0;
                let cy = rect.y + rect.h / 2.0;
                out.push(DrawCommand::Rect {
                    x: cx - 6.0,
                    y: cy - 5.0,
                    w: 12.0,
                    h: 10.0,
                    fill: None,
                    stroke: Some(PaintColor::Token { name: ColorToken::TextSecondary }),
                    stroke_width: Some(1.0),
                    corner_radius: Some(2.0),
                    shadow: None,
                });
            }
            _ => {
                let (active, mixed) = match item {
                    Item::Bold => tri(&summary.bold),
                    Item::Italic => tri(&summary.italic),
                    Item::Underline => tri(&summary.underline),
                    Item::Strike => tri(&summary.strike),
                    _ => (false, false),
                };
                if active || mixed {
                    out.push(DrawCommand::Rect {
                        x,
                        y: rect.y + BAR_PAD_Y,
                        w: width,
                        h: rect.h - 2.0 * BAR_PAD_Y,
                        fill: Some(PaintColor::Token {
                            name: if active { ColorToken::AccentSoft } else { ColorToken::Hover },
                        }),
                        stroke: None,
                        stroke_width: None,
                        corner_radius: Some(4.0),
                        shadow: None,
                    });
                }
                out.push(DrawCommand::Text {
                    x,
                    y: text_y,
                    content: label(item, &summary),
                    size: crate::theme::font_caption(),
                    color: if active {
                        PaintColor::Token { name: ColorToken::Accent }
                    } else {
                        PaintColor::Token { name: ColorToken::Text }
                    },
                    weight: matches!(item, Item::Bold).then_some(atelyx_plugin_protocol::paint::TextWeight::SemiBold),
                    family: None,
                    family_name: None,
                    italic: matches!(item, Item::Italic),
                    max_width: Some(width),
                    align: Some(atelyx_plugin_protocol::paint::TextAlign::Center),
                    layout: None,
                });
            }
        }
        x += width + BAR_GAP;
    }
}

/// 应用一次样式补丁（入撤销栈；无变化则回退栈顶）。
pub fn apply(session: &mut Session, patch: StylePatch) -> bool {
    let Some(region) = ops::region_of(
        session.view.selection.as_ref(),
        &session.data.fields,
        &session.data.rows,
    ) else {
        return false;
    };
    crate::input::push_undo(session);
    let changed = ops::apply_style_patch(
        &session.data.fields,
        &mut session.data.rows,
        &region,
        &patch,
    );
    if changed {
        session.touch();
    } else {
        session.view.undo.pop();
    }
    changed
}

/// 切换四个强调位之一：全开 → 清除该位；混合 / 全关 → 统一置位。
pub fn toggle(session: &mut Session, hit: FormatHit) -> bool {
    let Some(summary) = selection_style(session) else { return false };
    let set = |state: &OptState<bool>| {
        let all_on = matches!(state, OptState::Value(Some(true)));
        KeyOp::Set(if all_on { None } else { Some(true) })
    };
    let patch = match hit {
        FormatHit::Bold => StylePatch { b: set(&summary.bold), ..empty() },
        FormatHit::Italic => StylePatch { i: set(&summary.italic), ..empty() },
        FormatHit::Underline => StylePatch { u: set(&summary.underline), ..empty() },
        FormatHit::Strike => StylePatch { s: set(&summary.strike), ..empty() },
        _ => return false,
    };
    apply(session, patch)
}

/// 清除格式。
pub fn clear(session: &mut Session) -> bool {
    apply(
        session,
        StylePatch {
            b: KeyOp::Set(None),
            i: KeyOp::Set(None),
            u: KeyOp::Set(None),
            s: KeyOp::Set(None),
            color: KeyOp::Set(None),
            bg: KeyOp::Set(None),
            font: KeyOp::Set(None),
            size: KeyOp::Set(None),
        },
    )
}

/// 空补丁（只改需要的那几个键）。
fn empty() -> StylePatch {
    StylePatch {
        b: KeyOp::Keep,
        i: KeyOp::Keep,
        u: KeyOp::Keep,
        s: KeyOp::Keep,
        color: KeyOp::Keep,
        bg: KeyOp::Keep,
        font: KeyOp::Keep,
        size: KeyOp::Keep,
    }
}

/// 字体 / 字号 / 颜色补丁。
pub fn patch_font(name: Option<String>) -> StylePatch {
    StylePatch { font: KeyOp::Set(name), ..empty() }
}

pub fn patch_size(size: Option<f64>) -> StylePatch {
    StylePatch { size: KeyOp::Set(size), ..empty() }
}

pub fn patch_color(color: Option<String>, background: bool) -> StylePatch {
    if background {
        StylePatch { bg: KeyOp::Set(color), ..empty() }
    } else {
        StylePatch { color: KeyOp::Set(color), ..empty() }
    }
}

/// 当前选区的字色 / 底色（取色器初始值；混合 / 缺省 = None）。
pub fn current_color(session: &Session, background: bool) -> Option<String> {
    let summary = selection_style(session)?;
    let state = if background { summary.bg } else { summary.color };
    match state {
        OptState::Value(value) => value,
        OptState::Mixed => None,
    }
}

#[cfg(test)]
mod tests {
    use atelyx_format::table::ops::TableSelection;
    use atelyx_format::table::{TableField, TableFile, TableRow};
    use atelyx_plugin_protocol::paint::DrawCommand;

    use super::popup_frame;
    use crate::model::{Session, Size};

    fn session(width: f32) -> Session {
        let mut values = serde_json::Map::new();
        values.insert("f".into(), serde_json::json!("abc"));
        let mut session = Session::new(
            "t.atb".into(),
            TableFile {
                schema: "atelyx-table/v1".into(),
                id: "t".into(),
                title: "T".into(),
                fields: vec![TableField {
                    id: "f".into(),
                    name: "A".into(),
                    field_type: "text".into(),
                    options: None,
                    width: None,
                    calc_type: None,
                }],
                rows: vec![TableRow { id: "r".into(), values, height: None, styles: None }],
                created_at: 0,
                updated_at: 0,
            },
        );
        session.view.canvas_size = Size { width, height: 200.0 };
        session.view.selection =
            Some(TableSelection::Cell { row_id: "r".into(), field_id: "f".into() });
        session.view.bar_shown = true;
        session
    }

    #[test]
    fn bubble_is_window_layer_and_not_clipped_by_narrow_panel() {
        let session = session(160.0);
        let popup = popup_frame(&session, session.view.canvas_size).expect("选中格应弹气泡");
        assert_eq!(popup.layer, atelyx_plugin_protocol::frame::PopupLayer::Window);
        assert_eq!(popup.capture, atelyx_plugin_protocol::frame::PopupCapture::Inside);
        assert!(
            popup.rect.w > session.view.canvas_size.width,
            "面比气泡窄时不应压窄气泡：{}",
            popup.rect.w
        );
        assert!(popup.rect.y < 0.0, "首行选区的气泡应越出面顶盖住面板头部：{}", popup.rect.y);
        assert!(
            popup.commands.iter().any(|command| matches!(
                command,
                DrawCommand::Text { content, .. } if content == "B"
            )),
            "气泡应含样式条目"
        );
        assert!(!popup.hits.is_empty(), "条目光标区应随浮层提交");
    }
}
