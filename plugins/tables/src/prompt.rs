//! 浮层输入：字段改名 / 左右插列命名 / 添加字段（名称 + 类型）/ 单选选项编辑。
//! 请求来自菜单，浮层由插件自绘（浮层帧），回车提交、Esc 取消、点外取消。

use atelyx_format::table::ops;
use atelyx_plugin_protocol::frame::{FrameRect, PopupCapture, PopupLayer, SurfacePopup};
use atelyx_plugin_protocol::paint::{ColorToken, DrawCommand, PaintColor};

use crate::editor::Editor;
use crate::menu::FIELD_TYPES;
use crate::model::{Point, Rect, Session};

/// 输入浮层用途。
#[derive(Clone, Debug, PartialEq)]
pub enum PromptKind {
    RenameField { field_id: String },
    InsertColumn { field_id: String, side: i32 },
    AddField { field_type: String },
    Options { field_id: String },
}

/// 输入浮层状态。
pub struct Prompt {
    pub kind: PromptKind,
    pub editor: Editor,
    /// 锚点（面本地坐标，浮层落在其右下，放不下翻到左上）。
    pub anchor: Point,
}

/// 浮层四周内衬（与绘制侧的输入框落点同一口径）。
const PROMPT_PAD: f32 = 8.0;
/// 输入框左右内衬。
const INPUT_PAD_X: f32 = 6.0;
/// 输入框上下内衬。
const INPUT_PAD_Y: f32 = 4.0;
/// 输入行高（与输入法光标高度同口径）。
const INPUT_LINE_H: f32 = 18.0;
/// 多行输入最多显示的行数（超出部分不再撑高浮层）。
const MAX_INPUT_LINES: usize = 6;
/// 类型行高与行距。
const TYPE_ROW_H: f32 = 24.0;
const TYPE_ROW_GAP: f32 = 2.0;
/// 区块间距。
const SECTION_GAP: f32 = 6.0;

/// 新建浮层（初值按用途取当前值）。
pub fn open(session: &Session, kind: PromptKind, anchor: Point) -> Prompt {
    let value = match &kind {
        PromptKind::RenameField { field_id } | PromptKind::InsertColumn { field_id, .. } => session
            .data
            .fields
            .iter()
            .find(|field| field.id == *field_id)
            .map(|field| field.name.clone())
            .unwrap_or_default(),
        PromptKind::AddField { .. } => String::new(),
        PromptKind::Options { field_id } => session
            .data
            .fields
            .iter()
            .find(|field| field.id == *field_id)
            .and_then(|field| field.options.clone())
            .map(|options| options.join("\n"))
            .unwrap_or_default(),
    };
    Prompt { kind, editor: Editor::new(value), anchor }
}

/// 浮层是否多行输入。
pub fn multiline(prompt: &Prompt) -> bool {
    matches!(prompt.kind, PromptKind::Options { .. })
}

/// 「回车确认」提示行高。
fn hint_height() -> f32 {
    crate::theme::font_micro() * 16.0 / 12.0
}

/// 输入框显示的行数（单行输入固定 1 行，多行按内容封顶）。
fn input_lines(prompt: &Prompt) -> usize {
    if !multiline(prompt) {
        return 1;
    }
    prompt
        .editor
        .display_text()
        .split('\n')
        .count()
        .clamp(1, MAX_INPUT_LINES)
}

/// 输入框高：行数 × 行高 + 上下内衬。
fn input_height(prompt: &Prompt) -> f32 {
    input_lines(prompt) as f32 * INPUT_LINE_H + 2.0 * INPUT_PAD_Y
}

/// 类型行区高（不含与输入框之间的间距）。
fn type_rows_height() -> f32 {
    let count = FIELD_TYPES.len() as f32;
    count * TYPE_ROW_H + (count - 1.0).max(0.0) * TYPE_ROW_GAP
}

/// 浮层矩形（面本地坐标）：锚点右下，放不下翻到左上，再钳进面内。
pub fn rect(prompt: &Prompt, size: crate::model::Size) -> Rect {
    let width = 220.0f32;
    let rows = if matches!(prompt.kind, PromptKind::AddField { .. }) {
        SECTION_GAP + type_rows_height()
    } else {
        0.0
    };
    let height = PROMPT_PAD + input_height(prompt) + rows + SECTION_GAP + hint_height() + PROMPT_PAD;
    let mut x = prompt.anchor.x + 2.0;
    let mut y = prompt.anchor.y + 2.0;
    if x + width > size.width - 6.0 {
        x = (prompt.anchor.x - width - 2.0).max(6.0);
    }
    if y + height > size.height - 6.0 {
        y = (size.height - height - 6.0).max(0.0);
    }
    Rect::new(x.max(6.0), y.max(0.0), width, height)
}

/// 类型行矩形（添加字段时才有）。
fn type_row(prompt: &Prompt, size: crate::model::Size, index: usize) -> Option<Rect> {
    if !matches!(prompt.kind, PromptKind::AddField { .. }) {
        return None;
    }
    let base = rect(prompt, size);
    let top = base.y
        + PROMPT_PAD
        + input_height(prompt)
        + SECTION_GAP
        + index as f32 * (TYPE_ROW_H + TYPE_ROW_GAP);
    Some(Rect::new(
        base.x + PROMPT_PAD,
        top,
        base.w - 2.0 * PROMPT_PAD,
        TYPE_ROW_H,
    ))
}

/// 浮层帧；无浮层返回 `None`。
pub fn popup_frame(session: &Session, size: crate::model::Size) -> Option<SurfacePopup> {
    let prompt = session.view.prompt.as_ref()?;
    let area = rect(prompt, size);
    let mut commands = Vec::new();
    commands.push(DrawCommand::Rect {
        x: area.x,
        y: area.y,
        w: area.w,
        h: area.h,
        fill: Some(PaintColor::Token { name: ColorToken::SurfaceOverlay }),
        stroke: Some(PaintColor::Token { name: ColorToken::Border }),
        stroke_width: Some(1.0),
        corner_radius: Some(6.0),
        shadow: Some(atelyx_plugin_protocol::paint::PaintShadow {
            offset_x: 0.0,
            offset_y: 4.0,
            blur: 12.0,
            color: PaintColor::Token { name: ColorToken::Scrim },
        }),
    });
    // 输入区（高随行数增长，多行不溢出浮层）
    let input = Rect::new(
        area.x + PROMPT_PAD,
        area.y + PROMPT_PAD,
        area.w - 2.0 * PROMPT_PAD,
        input_height(prompt),
    );
    commands.push(DrawCommand::Rect {
        x: input.x,
        y: input.y,
        w: input.w,
        h: input.h,
        fill: Some(PaintColor::Token { name: ColorToken::Background }),
        stroke: Some(PaintColor::Token { name: ColorToken::BorderStrong }),
        stroke_width: Some(1.0),
        corner_radius: Some(4.0),
        shadow: None,
    });
    let text = prompt.editor.display_text();
    for (index, line) in text.split('\n').take(MAX_INPUT_LINES).enumerate() {
        commands.push(DrawCommand::Text {
            x: input.x + INPUT_PAD_X,
            y: input.y + INPUT_PAD_Y + index as f32 * INPUT_LINE_H,
            content: line.to_string(),
            size: crate::theme::font_caption(),
            color: PaintColor::Token { name: ColorToken::Text },
            weight: None,
            family: None,
            family_name: None,
            italic: false,
            max_width: Some(input.w - 2.0 * INPUT_PAD_X),
            align: Some(atelyx_plugin_protocol::paint::TextAlign::Start),
            layout: None,
        });
    }
    // 回车提示
    let hint_y = input.bottom()
        + if matches!(prompt.kind, PromptKind::AddField { .. }) {
            SECTION_GAP + type_rows_height() + SECTION_GAP
        } else {
            SECTION_GAP
        };
    commands.push(DrawCommand::Text {
        x: input.x,
        y: hint_y,
        content: "回车确认".to_string(),
        size: crate::theme::font_micro(),
        color: PaintColor::Token { name: ColorToken::TextMuted },
        weight: None,
        family: None,
        family_name: None,
        italic: false,
        max_width: Some(input.w),
        align: Some(atelyx_plugin_protocol::paint::TextAlign::End),
        layout: None,
    });
    // 类型行
    if matches!(prompt.kind, PromptKind::AddField { .. }) {
        for (index, (_, label)) in FIELD_TYPES.iter().enumerate() {
            let Some(row) = type_row(prompt, size, index) else { continue };
            let selected = matches!(&prompt.kind, PromptKind::AddField { field_type } if field_type == FIELD_TYPES[index].0);
            if selected {
                commands.push(DrawCommand::Rect {
                    x: row.x,
                    y: row.y,
                    w: row.w,
                    h: row.h,
                    fill: Some(PaintColor::Token { name: ColorToken::AccentSoft }),
                    stroke: None,
                    stroke_width: None,
                    corner_radius: Some(4.0),
                    shadow: None,
                });
            }
            commands.push(DrawCommand::Text {
                x: row.x + 8.0,
                y: row.y + 5.0,
                content: (*label).to_string(),
                size: crate::theme::font_caption(),
                color: if selected {
                    PaintColor::Token { name: ColorToken::Accent }
                } else {
                    PaintColor::Token { name: ColorToken::TextSecondary }
                },
                weight: None,
                family: None,
                family_name: None,
                italic: false,
                max_width: Some(row.w - 16.0),
                align: Some(atelyx_plugin_protocol::paint::TextAlign::Start),
                layout: None,
            });
        }
    }
    Some(SurfacePopup {
        rect: FrameRect { x: area.x, y: area.y, w: area.w, h: area.h },
        capture: PopupCapture::All,
        layer: PopupLayer::Panel,
        order: 0,
        commands,
        hits: Vec::new(),
        ime: None,
    })
}

/// 点击落在浮层内：命中类型行则返回类型键；点在输入框内返回 `Some(None)`；点在框外返回 `None`。
#[allow(clippy::option_option)]
pub fn hit(prompt: &Prompt, size: crate::model::Size, point: Point) -> Option<Option<&'static str>> {
    let area = rect(prompt, size);
    if !(point.x >= area.x && point.x <= area.right() && point.y >= area.y && point.y <= area.bottom())
    {
        return None;
    }
    for (index, (kind, _)) in FIELD_TYPES.iter().enumerate() {
        if let Some(row) = type_row(prompt, size, index) {
            if row.contains(point.x, point.y) {
                return Some(Some(kind));
            }
        }
    }
    Some(None)
}

/// 提交浮层：按用途落成模型变更。
pub fn submit(session: &mut Session) -> bool {
    let Some(prompt) = session.view.prompt.take() else { return false };
    let text = prompt.editor.text.trim().to_string();
    match prompt.kind {
        PromptKind::RenameField { field_id } => {
            if text.is_empty() {
                return false;
            }
            crate::input::push_undo(session);
            let changed = ops::rename_field(&mut session.data, &field_id, &text);
            if changed {
                session.touch();
            } else {
                session.view.undo.pop();
            }
            changed
        }
        PromptKind::InsertColumn { field_id, side } => {
            let Some(index) = session.data.fields.iter().position(|field| field.id == field_id)
            else {
                return false;
            };
            let at = if side < 0 { index } else { index + 1 };
            let name = if text.is_empty() { format!("字段{}", session.data.fields.len() + 1) } else { text };
            crate::input::push_undo(session);
            ops::insert_field(&mut session.data, at, nanoid::nanoid!(), &name, "text");
            session.touch();
            true
        }
        PromptKind::AddField { field_type } => {
            let name = if text.is_empty() { format!("字段{}", session.data.fields.len() + 1) } else { text };
            let at = session.data.fields.len();
            crate::input::push_undo(session);
            ops::insert_field(&mut session.data, at, nanoid::nanoid!(), &name, &field_type);
            session.touch();
            true
        }
        PromptKind::Options { field_id } => {
            let options: Vec<String> = text
                .split('\n')
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect();
            crate::input::push_undo(session);
            let changed = ops::set_field_options(&mut session.data, &field_id, options);
            if changed {
                session.touch();
            } else {
                session.view.undo.pop();
            }
            changed
        }
    }
}

/// 单行输入的横向落点 → 字节位（按估算字宽累加，落点误差不超过一个字）。
pub fn caret_at(text: &str, size: f32, local_x: f32) -> usize {
    let mut width = 0.0f32;
    for (index, ch) in text.char_indices() {
        if ch == '\n' {
            break;
        }
        let advance = size * if ch.is_ascii() { 0.55 } else { 1.0 };
        if width + advance / 2.0 >= local_x {
            return index;
        }
        width += advance;
    }
    text.find('\n').unwrap_or(text.len())
}
