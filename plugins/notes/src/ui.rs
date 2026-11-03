//! 笔记面板的帧声明：工具条 + 页面内标题 + 属性面板 + 三档内容面 + 反链区 + 浮层。

use atelyx_plugin_protocol::paint::{
    ColorToken, DrawCommand, ImageFit, ImageSource, PaintColor, TextWeight,
};
use atelyx_plugin_sdk::frame_ui::{BoxStyle, BoxWidth, Edges, Rect, TextRole, TextStyle, Ui};

use crate::model::{
    BlockInfo, Mode, NoteModel, BODY_SCROLL, EMPTY_EDIT, SOURCE, SOURCE_SCROLL, TITLE,
};
use crate::props;

/// 正文源码行高倍数（与阅读面同一口径）。
const PROSE_LINE: f32 = 1.625;

/// 预览编辑的块 id；双击 / 单击回发同一个 id。
pub fn block_id(index: usize) -> String {
    format!("note:block:{index}")
}

/// 空行输入面 id：`gap` = 其后块序（末块之后用块数），`line` = 空行区内行序。
pub fn gap_input_id(gap: usize, line: usize) -> String {
    format!("note:gap:{gap}:{line}")
}

/// 列表项输入面 / 命中区 id：`block` = 列表块序，`item` = 项序。
pub fn item_id(block: usize, item: usize) -> String {
    format!("note:item:{block}:{item}")
}

pub fn backlink_id(index: usize) -> String {
    format!("note:backlink:{index}")
}

pub fn frame(ui: &mut Ui<'_, '_>, model: &NoteModel) {
    if !model.is_open() {
        empty(ui, model);
        return;
    }
    ui.fill_column(0.0, |ui| {
        toolbar(ui, model);
        ui.rule();
        if model.prefs.in_page_title && model.mode != Mode::Source {
            page_title(ui, model);
        }
        if model.mode != Mode::Source {
            props::panel(ui, model);
        }
        content(ui, model);
        ui.rule();
        backlinks(ui, model);
    });
    overlays(ui, model);
}

/// 未打开笔记：整页居中的引导（图标 + 标题 + 说明）。
fn empty(ui: &mut Ui<'_, '_>, model: &NoteModel) {
    ui.fill_column(0.0, |ui| {
        ui.fill_spacer();
        ui.center_column(16.0, |ui| {
            ui.icon_named("file-text", 64.0, ColorToken::TextMuted);
            ui.text_styled_fit(
                "打开笔记",
                TextStyle::new(TextRole::Title).weight(TextWeight::SemiBold),
            );
            let hint = match &model.open_error {
                Some(error) => short(error, "打开失败"),
                None => "在文件面板或搜索面板单击一个 .md 笔记开始编辑。".to_string(),
            };
            ui.text_styled_fit(hint, TextStyle::new(TextRole::Caption));
        });
        ui.fill_spacer();
    });
}

fn toolbar(ui: &mut Ui<'_, '_>, model: &NoteModel) {
    ui.boxed(
        BoxWidth::Fill,
        BoxStyle::new(Some(ColorToken::SurfaceMuted), None, 0.0),
        Edges::symmetric(12.0, 6.0),
        0.0,
        |ui| {
            ui.row(8.0, |ui| {
                collab_pills(ui, model);
                status(ui, model);
                ui.grow();
                mode_control(ui, model);
                let menu_open = model.menu_open;
                ui.icon_button_named(
                    "note:menu",
                    "ellipsis",
                    15.0,
                    26.0,
                    if menu_open { ColorToken::Accent } else { ColorToken::TextMuted },
                    true,
                );
            });
        },
    );
}

fn collab_pills(ui: &mut Ui<'_, '_>, model: &NoteModel) {
    const MAX_PILLS: usize = 3;
    if model.remote.is_empty() {
        return;
    }
    ui.row_fit(4.0, |ui| {
        for cursor in model.remote.iter().take(MAX_PILLS) {
            let rgba = parse_color(&cursor.color);
            ui.canvas_sized(8.0, 8.0, move |canvas| {
                canvas.draw(DrawCommand::Rect {
                    x: canvas.area().x,
                    y: canvas.area().y + 2.0,
                    w: 4.0,
                    h: 4.0,
                    fill: Some(PaintColor::Rgba { rgba }),
                    stroke: None,
                    stroke_width: None,
                    corner_radius: Some(2.0),
                    shadow: None,
                });
            });
            let style = TextStyle::new(TextRole::Micro).color(ColorToken::TextSecondary);
            ui.text_styled_fit(cursor.name.clone(), style);
        }
        let more = model.remote.len().saturating_sub(MAX_PILLS);
        if more > 0 {
            let style = TextStyle::new(TextRole::Micro).color(ColorToken::TextMuted);
            ui.text_styled_fit(format!("+{more}"), style);
        }
    });
}

fn status(ui: &mut Ui<'_, '_>, model: &NoteModel) {
    if let Some(error) = &model.save_error {
        ui.row_fit(4.0, |ui| {
            let style = TextStyle::new(TextRole::Micro).color(ColorToken::Danger);
            ui.text_styled_fit(short(error, "保存失败"), style);
            ui.ghost_button("note:retry", "重试");
            ui.icon_button_named("note:error-dismiss", "x", 10.0, 18.0, ColorToken::Danger, true);
        });
    } else if model.dirty {
        let style = TextStyle::new(TextRole::Micro).color(ColorToken::TextMuted);
        ui.text_styled_fit("未保存", style);
    }
}

fn mode_control(ui: &mut Ui<'_, '_>, model: &NoteModel) {
    let labels = [("只读", Mode::Read, "eye"), ("预览编辑", Mode::Edit, "pencil"), ("源码", Mode::Source, "code")];
    let gap = 2.0;
    let pad = 2.0;
    let widths: Vec<f32> = labels
        .iter()
        .map(|(label, _, _)| ui.text_width(label, TextRole::Micro) + 12.0 + 4.0 + 16.0)
        .collect();
    let total = widths.iter().sum::<f32>() + gap * (labels.len() - 1) as f32 + pad * 2.0;
    ui.boxed(
        BoxWidth::Fixed(total),
        BoxStyle::new(Some(ColorToken::SurfaceSunken), Some(ColorToken::Border), 5.0),
        Edges::all(pad),
        gap,
        |ui| {
            ui.row_fit(gap, |ui| {
                for ((label, mode, icon), width) in labels.iter().zip(widths.iter()) {
                    let active = model.mode == *mode;
                    let enabled = *mode == Mode::Read || model.can_write;
                    let tint = if active {
                        ColorToken::Accent
                    } else if enabled {
                        ColorToken::TextSecondary
                    } else {
                        ColorToken::TextMuted
                    };
                    let id = match mode {
                        Mode::Read => "note:mode:read",
                        Mode::Edit => "note:mode:edit",
                        Mode::Source => "note:mode:source",
                    };
                    ui.click_box(
                        id,
                        BoxWidth::Fixed(*width),
                        Edges::symmetric(8.0, 5.0),
                        3.0,
                        active,
                        enabled,
                        |ui| {
                            ui.center_column(0.0, |ui| {
                                ui.row_fit(4.0, |ui| {
                                    ui.icon_named(icon, 12.0, tint);
                                    let style = TextStyle::new(TextRole::Micro)
                                        .color(tint)
                                        .weight(TextWeight::SemiBold);
                                    ui.text_styled_fit((*label).to_string(), style);
                                });
                            });
                        },
                    );
                }
            });
        },
    );
}

fn page_title(ui: &mut Ui<'_, '_>, model: &NoteModel) {
    bounded(ui, model, |ui| {
        ui.pad(16.0, 0.0, |ui| {
            ui.pad(0.0, 12.0, |ui| {
                if let Some(draft) = &model.renaming {
                    let size = ui.role_size(TextRole::Title);
                    let height = size * 1.4;
                    let pad = ((height - ui.text_height(TextRole::Title)) / 2.0).max(0.0);
                    ui.text_area_plain_fixed(
                        crate::model::TITLE_INPUT,
                        draft.clone(),
                        height,
                        size,
                        Edges::symmetric(0.0, pad),
                    );
                } else {
                    let style = TextStyle::new(TextRole::Title).weight(TextWeight::SemiBold);
                    ui.click_box(
                        TITLE,
                        BoxWidth::Fill,
                        Edges::symmetric(0.0, 2.0),
                        3.0,
                        false,
                        true,
                        |ui| {
                            ui.text_styled(model.title(), style);
                        },
                    );
                }
                if model.renaming.is_some() {
                    ui.rule();
                }
            });
            ui.spacer(4.0);
        });
    });
}

fn content(ui: &mut Ui<'_, '_>, model: &NoteModel) {
    let border = if model.mode == Mode::Read {
        ColorToken::Transparent
    } else {
        ColorToken::Accent
    };
    ui.boxed(
        BoxWidth::Fill,
        BoxStyle::new(None, Some(border), 0.0),
        Edges::all(1.0),
        0.0,
        |ui| match model.mode {
            Mode::Read => read_view(ui, model),
            Mode::Edit => edit_view(ui, model),
            Mode::Source => source_view(ui, model),
        },
    );
}

fn read_view(ui: &mut Ui<'_, '_>, model: &NoteModel) {
    ui.scroll(BODY_SCROLL, 0.0, |ui| {
        bounded(ui, model, |ui| {
            ui.pad(16.0, 10.0, |ui| {
                if model.blocks.is_empty() {
                    ui.text("这篇笔记还没有正文。", TextRole::Caption);
                    return;
                }
                rows(ui, model, None);
            });
        });
    });
}

fn edit_view(ui: &mut Ui<'_, '_>, model: &NoteModel) {
    let body_size = ui.role_size(TextRole::Body);
    let measure = EditMeasure {
        size: body_size,
        line: body_size * PROSE_LINE,
        text: ui.text_height(TextRole::Body),
    };
    ui.scroll(BODY_SCROLL, 0.0, |ui| {
        bounded(ui, model, |ui| {
            ui.pad(16.0, 10.0, |ui| {
                if model.blocks.is_empty() {
                    ui.text_area_plain_auto(
                        EMPTY_EDIT,
                        model.body(),
                        measure.line,
                        600.0,
                        body_size,
                        Edges::symmetric(0.0, 0.0),
                    );
                    return;
                }
                rows(ui, model, Some(measure));
            });
        });
    });
}

/// 预览编辑的行高口径：正文源码行高 1.625 倍字号；输入框内部按实际字高排版，差额用上下内衬补齐。
#[derive(Clone, Copy)]
struct EditMeasure {
    size: f32,
    line: f32,
    text: f32,
}

/// 块序列：只读 / 预览编辑共用（预览编辑档的活动块换成源码输入面，空行各自成为输入行）。
fn rows(ui: &mut Ui<'_, '_>, model: &NoteModel, edit: Option<EditMeasure>) {
    let body = model.body();
    let line = ui.role_size(TextRole::Body) * PROSE_LINE;
    for (index, block) in model.blocks.iter().enumerate() {
        let gaps = model.gap_lines(index).len();
        if gaps > 0 {
            match edit {
                Some(measure) => {
                    // 上一块的活动编辑面已把块尾空行含在源码里，这里不重画（避免空行翻倍）
                    if index == 0 || model.active_block != Some(index - 1) {
                        for line_index in 0..gaps {
                            gap_line(ui, model, index, line_index, measure);
                        }
                    }
                }
                None => ui.spacer(gaps as f32 * line),
            }
        }
        if let (Some(measure), Some(index_active)) = (edit, model.active_block) {
            if index_active == index && block.items.is_empty() {
                let source = model.active_source().unwrap_or("");
                ui.text_area_plain_auto(
                    block_id(index),
                    source,
                    measure.line,
                    600.0,
                    measure.size,
                    Edges::symmetric(0.0, 2.0),
                );
                continue;
            }
        }
        if let Some(measure) = edit {
            if !block.items.is_empty() {
                list_rows(ui, model, index, block, measure);
                continue;
            }
        }
        match &block.image {
            Some(image) if image.height.is_some() => {
                ui.image_fit(image_source(image), image.height.unwrap_or(240.0), ImageFit::Contain);
            }
            _ => {
                ui.markdown_named(block_id(index), &body[block.from..block.to], Vec::new());
            }
        }
    }
    let tail = model.gap_lines(model.blocks.len()).len();
    if tail > 0 {
        match edit {
            Some(measure) => {
                let last = model.blocks.len().checked_sub(1);
                if model.active_block != last {
                    for line_index in 0..tail {
                        gap_line(ui, model, model.blocks.len(), line_index, measure);
                    }
                }
            }
            None => ui.spacer(tail as f32 * line),
        }
    }
}

/// 一根空行 = 一个无底色输入面：点它光标落在这根空行，键入内容就地插进正文，不揭示任何块。
fn gap_line(ui: &mut Ui<'_, '_>, model: &NoteModel, gap: usize, line_index: usize, measure: EditMeasure) {
    let Some((from, to)) = model.gap_line_range(gap, line_index) else { return };
    let value = model.text[from..to].to_string();
    let pad = ((measure.line - measure.text) / 2.0).max(0.0);
    ui.text_area_plain_fixed(
        gap_input_id(gap, line_index),
        value,
        measure.line,
        measure.size,
        Edges::symmetric(0.0, pad),
    );
}

/// 列表块逐项渲染：每项一个揭示单元，有序项按原序号绘制，项间空行折算为间距。
fn list_rows(
    ui: &mut Ui<'_, '_>,
    model: &NoteModel,
    index: usize,
    block: &BlockInfo,
    measure: EditMeasure,
) {
    let body = model.body();
    for (item, info) in block.items.iter().enumerate() {
        let gaps = model.item_gap_lines(index, item);
        if gaps > 0 {
            ui.spacer(gaps as f32 * measure.line);
        }
        let id = item_id(index, item);
        let source = &body[info.from..info.to];
        if model.active_item == Some((index, item)) {
            ui.text_area_plain_auto(
                id,
                source,
                measure.line,
                600.0,
                measure.size,
                Edges::symmetric(0.0, 2.0),
            );
        } else {
            ui.markdown_list_item_named(id, source, item, Vec::new());
        }
    }
}

fn source_view(ui: &mut Ui<'_, '_>, model: &NoteModel) {
    let body_size = ui.role_size(TextRole::Body);
    ui.scroll(SOURCE_SCROLL, 0.0, |ui| {
        bounded(ui, model, |ui| {
            ui.pad(16.0, 10.0, |ui| {
                ui.text_area_plain_auto(
                    SOURCE,
                    model.text.clone(),
                    40.0,
                    4000.0,
                    body_size,
                    Edges::symmetric(0.0, 0.0),
                );
            });
        });
    });
}

fn bounded(ui: &mut Ui<'_, '_>, model: &NoteModel, build: impl FnOnce(&mut Ui<'_, '_>)) {
    ui.bounded(model.prefs.content_width as f32, build);
}

fn backlinks(ui: &mut Ui<'_, '_>, model: &NoteModel) {
    ui.pad(16.0, 6.0, |ui| {
        ui.row_fit(6.0, |ui| {
            ui.icon_named("unlink", 12.0, ColorToken::TextMuted);
            ui.text(format!("反链 ({})", model.backlinks.len()), TextRole::Caption);
        });
        if model.backlinks.is_empty() {
            let style = TextStyle::new(TextRole::Micro).color(ColorToken::TextMuted);
            ui.text_styled_fit("暂无其他笔记引用当前笔记", style);
            return;
        }
        for (index, link) in model.backlinks.iter().enumerate() {
            ui.click_row(
                backlink_id(index),
                Edges::symmetric(4.0, 2.0),
                3.0,
                false,
                true,
                |ui| {
                    ui.row_fit(6.0, |ui| {
                        ui.icon_named("file-text", 12.0, ColorToken::TextMuted);
                        let style = TextStyle::new(TextRole::Caption).color(ColorToken::Text);
                        ui.text_styled_fit(link.title.clone(), style);
                    });
                },
            );
        }
    });
}

fn overlays(ui: &mut Ui<'_, '_>, model: &NoteModel) {
    if model.menu_open {
        let bounds = ui.bounds();
        // 背板承接外点：点面内其它位置即收起菜单
        ui.overlay(Rect::new(0.0, 0.0, bounds.w, bounds.h), |ui| {
            ui.hit_area("note:menu-backdrop");
        });
        let width = 168.0;
        let rows = 2.0;
        let height = 12.0 + rows * 28.0;
        let left = (bounds.w - width - 8.0).max(0.0);
        ui.overlay(Rect::new(left, 44.0, width, height), |ui| {
            ui.boxed(
                BoxWidth::Fixed(width),
                BoxStyle::new(Some(ColorToken::SurfaceOverlay), Some(ColorToken::Border), 6.0),
                Edges::all(6.0),
                0.0,
                |ui| {
                    ui.click_row(
                        "note:menu:add-prop",
                        Edges::symmetric(8.0, 4.0),
                        3.0,
                        false,
                        model.can_write,
                        |ui| {
                            ui.row_fit(6.0, |ui| {
                                ui.icon_named("plus", 13.0, ColorToken::TextMuted);
                                let style = TextStyle::new(TextRole::Caption).color(ColorToken::TextSecondary);
                                ui.text_styled_fit("添加笔记属性", style);
                            });
                        },
                    );
                    ui.click_row(
                        "note:menu:history",
                        Edges::symmetric(8.0, 4.0),
                        3.0,
                        false,
                        model.path.is_some(),
                        |ui| {
                            ui.row_fit(6.0, |ui| {
                                ui.icon_named("history", 13.0, ColorToken::TextMuted);
                                let style = TextStyle::new(TextRole::Caption).color(ColorToken::TextSecondary);
                                ui.text_styled_fit("历史记录", style);
                            });
                        },
                    );
                },
            );
        });
    }
    if let Some(index) = model.props.type_menu {
        let (x, y) = model.pointer;
        let left = x.min(ui.bounds().w - 140.0).max(4.0);
        props::type_menu(ui, model, index, left, y + 6.0);
    }
}

fn image_source(image: &crate::model::ImageBlock) -> ImageSource {
    if image.src.starts_with("http://") || image.src.starts_with("https://") {
        ImageSource::Url { url: image.src.clone(), local: None }
    } else {
        ImageSource::Vault { path: image.src.clone(), max_edge: None }
    }
}

fn parse_color(value: &str) -> [u8; 4] {
    let hex = value.trim_start_matches('#');
    if !hex.is_ascii() {
        return [0x80, 0x80, 0x80, 255];
    }
    let parse = |range: std::ops::Range<usize>| u8::from_str_radix(&hex[range], 16).unwrap_or(0x80);
    match hex.len() {
        6 => [parse(0..2), parse(2..4), parse(4..6), 255],
        8 => [parse(0..2), parse(2..4), parse(4..6), parse(6..8)],
        _ => [0x80, 0x80, 0x80, 255],
    }
}

fn short(text: &str, fallback: &str) -> String {
    if text.trim().is_empty() {
        fallback.to_string()
    } else if text.chars().count() > 40 {
        let mut out: String = text.chars().take(39).collect();
        out.push('…');
        out
    } else {
        text.to_string()
    }
}
