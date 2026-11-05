//! 笔记面板的帧声明：工具条 + 页面内标题 + 属性面板 + 三档内容面 + 反链区 + 浮层。

use atelyx_plugin_protocol::paint::{
    ColorToken, DrawCommand, ImageFit, ImageSource, PaintColor, TextWeight,
};
use atelyx_plugin_sdk::frame_ui::{BoxStyle, BoxWidth, Edges, Rect, TextRole, TextStyle, Ui};

use crate::model::{
    image_layout, BlockInfo, CursorSpot, Mode, NoteModel, UnitKind, BODY_SCROLL, EMPTY_EDIT, SOURCE,
    SOURCE_SCROLL, TITLE,
};
use atelyx_plugin_props as props;

/// 正文源码行高倍数（与阅读面同一口径）。
const PROSE_LINE: f32 = 1.625;
/// 列表嵌套一级的水平缩进（与 Markdown 版面的标号列同宽）。
const LIST_STEP: f32 = crate::model::LIST_TEXT_X;
/// 远端光标名签的字号 / 水平内边距 / 高度。
const TAG_SIZE: f32 = 10.0;
const TAG_PAD_X: f32 = 4.0;
const TAG_H: f32 = 14.0;

/// 预览编辑的块 id；双击 / 单击回发同一个 id。
pub fn block_id(index: usize) -> String {
    format!("note:block:{index}")
}

/// 空行输入面 id：`gap` = 其后块序（末块之后用块数），`line` = 空行区内行序。
pub fn gap_input_id(gap: usize, line: usize) -> String {
    format!("note:gap:{gap}:{line}")
}

/// 揭示单元输入面 / 命中区 id：`block` = 块序，`item` = 单元序。
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
    let hint = match &model.open_error {
        Some(error) => short(error, "打开失败"),
        None => "在文件面板或搜索面板单击一个 .md 笔记开始编辑。".to_string(),
    };
    ui.empty_state("file-text", "打开笔记", &hint);
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
    ui.markdown_soft_wrap(model.prefs.soft_wrap);
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
    let measure = EditMeasure { size: body_size, line: body_size * PROSE_LINE };
    // 未揭示的块与只读面同口径：换行设置一致，行高按 1.625 倍字号排进输入面
    ui.markdown_soft_wrap(model.prefs.soft_wrap);
    ui.input_line_height(Some(measure.line));
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

/// 预览编辑的行高口径：正文源码行高 1.625 倍字号。
#[derive(Clone, Copy)]
struct EditMeasure {
    size: f32,
    line: f32,
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
                with_cursors(ui, model, index, None, 0.0, measure.line, false, |ui| {
                    ui.text_area_plain_auto(
                        block_id(index),
                        source,
                        measure.line,
                        600.0,
                        measure.size,
                        Edges::symmetric(0.0, 0.0),
                    );
                });
                continue;
            }
        }
        if !block.items.is_empty() {
            unit_rows(ui, model, index, block, line, edit);
            continue;
        }
        match &block.image {
            Some(image) => {
                let failed = model.image_probe.get(&image.src) == Some(&None);
                if failed {
                    // 载入失败：按替代文字占位，不给正文留空洞
                    let name = if image.alt.is_empty() { &image.src } else { &image.alt };
                    ui.markdown_named(block_id(index), &format!("[图片：{name}]"), Vec::new());
                } else {
                    let natural = model.image_probe.get(&image.src).copied().flatten();
                    match image_layout(image, natural, model.prefs.content_width as f32) {
                        Some(size) => {
                            ui.image_fit_sized(image_source(image), size.width, size.height, ImageFit::Contain);
                        }
                        // 原图尺寸还没探测回来且没有声明尺寸：先按正文渲染，探测完成后重排成图片
                        None => {
                            let source = &body[block.from..block.to];
                            with_cursors(ui, model, index, None, 0.0, line, true, |ui| {
                                ui.markdown_named(block_id(index), source, Vec::new());
                            });
                        }
                    }
                }
            }
            None => {
                let source = &body[block.from..block.to];
                with_cursors(ui, model, index, None, 0.0, line, true, |ui| {
                    ui.markdown_named(block_id(index), source, Vec::new());
                });
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
    ui.text_area_plain_fixed(
        gap_input_id(gap, line_index),
        value,
        measure.line,
        measure.size,
        Edges::symmetric(0.0, 0.0),
    );
}

/// 揭示单元逐项渲染：列表项按嵌套层级缩进，引用子块按块渲染；每项一个独立揭示单元。
fn unit_rows(
    ui: &mut Ui<'_, '_>,
    model: &NoteModel,
    index: usize,
    block: &BlockInfo,
    line: f32,
    edit: Option<EditMeasure>,
) {
    let body = model.body();
    for (unit, info) in block.items.iter().enumerate() {
        let gaps = model.item_gap_lines(index, unit);
        if gaps > 0 {
            ui.spacer(gaps as f32 * line);
        }
        let id = item_id(index, unit);
        if let Some(measure) = edit {
            if model.active_item == Some((index, unit)) {
                // 编辑框的文本必须与编辑值同源（含行首缩进 / 容器前缀），否则光标与点击落点整体偏移
                let Some(editing) = model.item_source(index, unit).map(|(from, to)| &model.text[from..to])
                else {
                    continue;
                };
                with_cursors(ui, model, index, Some(unit), 0.0, measure.line, false, |ui| {
                    ui.text_area_plain_auto(
                        id,
                        editing,
                        measure.line,
                        600.0,
                        measure.size,
                        Edges::symmetric(0.0, 0.0),
                    );
                });
                continue;
            }
        }
        let source = &body[info.from..info.to];
        match info.kind {
            UnitKind::Item { depth, ordinal } => {
                // 首行缩进由层级缩进代替，避免 4 空格以上被 Markdown 当成代码块
                let shown = source.trim_start_matches([' ', '\t']);
                let indent = depth as f32 * LIST_STEP;
                with_cursors(ui, model, index, Some(unit), indent, line, true, |ui| {
                    if depth == 0 {
                        ui.markdown_list_item_named(&id, shown, ordinal, Vec::new());
                    } else {
                        ui.row(0.0, |row| {
                            row.spacer_width(indent);
                            row.markdown_list_item_named(&id, shown, ordinal, Vec::new());
                        });
                    }
                });
            }
            UnitKind::Block { depth } => {
                let indent = depth as f32 * LIST_STEP;
                let shown = unindent(source, depth);
                with_cursors(ui, model, index, Some(unit), indent, line, true, |ui| {
                    if depth == 0 {
                        ui.markdown_named(&id, &shown, Vec::new());
                    } else {
                        ui.row(0.0, |row| {
                            row.spacer_width(indent);
                            row.markdown_named(&id, &shown, Vec::new());
                        });
                    }
                });
            }
        }
    }
}

/// 容器子块去掉所属列表的基准缩进（每层两级空格），余下的额外缩进仍按 Markdown 语义（如代码块）。
fn unindent(source: &str, depth: usize) -> String {
    if depth == 0 {
        return source.to_string();
    }
    let limit = depth * crate::edit::INDENT_UNIT.len();
    source
        .split('\n')
        .map(|line| {
            let mut cut = 0;
            while cut < limit && matches!(line.as_bytes().get(cut), Some(b' ') | Some(b'\t')) {
                cut += 1;
            }
            &line[cut..]
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 单元内的远端光标：同一矩形里叠在内容之上画竖线与名签。
/// `rendered` = 内容按 Markdown 渲染（列表标记 / 引用前缀不进量宽）；活动编辑框传 false（画的是源码）。
fn with_cursors(
    ui: &mut Ui<'_, '_>,
    model: &NoteModel,
    block: usize,
    unit: Option<usize>,
    indent: f32,
    line: f32,
    rendered: bool,
    content: impl FnOnce(&mut Ui<'_, '_>),
) {
    if model.remote.is_empty() {
        content(ui);
        return;
    }
    let spots: Vec<(String, [u8; 4], CursorSpot)> = model
        .remote
        .iter()
        .filter_map(|cursor| {
            let offset = cursor.head.or(cursor.anchor)?;
            // 先按块区间筛掉别的行，避免逐行重扫全文块表；块区间相对正文，偏移先减 frontmatter
            let rel = offset.checked_sub(model.body_start)?;
            let in_block =
                model.blocks.get(block).is_some_and(|info| rel >= info.from && rel <= info.to);
            if !in_block {
                return None;
            }
            let spot = if rendered {
                model.cursor_spot(offset)?
            } else {
                model.source_cursor_spot(offset)?
            };
            (spot.block == block && spot.unit == unit).then(|| (cursor.name.clone(), parse_color(&cursor.color), spot))
        })
        .collect();
    if spots.is_empty() {
        content(ui);
        return;
    }
    let size = ui.role_size(TextRole::Body);
    let need = spots.iter().map(|(_, _, spot)| spot.line).max().unwrap_or(0) as f32 * line + line;
    ui.column(0.0, |outer| {
        outer.stack(|layer| {
            content(layer);
            layer.canvas(need, move |canvas| {
                let area = canvas.area();
                for (name, color, spot) in &spots {
                    let x = area.x + indent + spot.base_x + canvas.measure(&spot.line_prefix, size).0;
                    let y = area.y + spot.line as f32 * line;
                    canvas.draw(DrawCommand::Rect {
                        x,
                        y,
                        w: 2.0,
                        h: line,
                        fill: Some(PaintColor::Rgba { rgba: *color }),
                        stroke: None,
                        stroke_width: None,
                        corner_radius: None,
                        shadow: None,
                    });
                    let tag_w = canvas.measure(name, TAG_SIZE).0 + TAG_PAD_X * 2.0;
                    let tag_x = (x + 3.0).min(area.x + area.w - tag_w).max(area.x);
                    let tag_y = (y - TAG_H - 1.0).max(area.y);
                    canvas.draw(DrawCommand::Rect {
                        x: tag_x,
                        y: tag_y,
                        w: tag_w,
                        h: TAG_H,
                        fill: Some(PaintColor::Rgba { rgba: *color }),
                        stroke: None,
                        stroke_width: None,
                        corner_radius: Some(3.0),
                        shadow: None,
                    });
                    canvas.text(name, tag_x + TAG_PAD_X, tag_y + 1.0, TAG_SIZE, PaintColor::Rgba { rgba: [255, 255, 255, 255] });
                }
            });
        });
    });
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
    wiki_menu(ui, model);
}

/// `[[` 候选：贴着当前光标弹出，优先放在光标下沿，越界时翻到上沿 / 左移。
fn wiki_menu(ui: &mut Ui<'_, '_>, model: &NoteModel) {
    let Some(wiki) = &model.wiki else { return };
    let Some(caret) = wiki.caret_rect else { return };
    let bounds = ui.bounds();
    const ROW_H: f32 = 26.0;
    let width = 260.0;
    let height = 8.0 + wiki.candidates.len() as f32 * ROW_H;
    let left = if caret.x + 6.0 + width <= bounds.w - 4.0 {
        caret.x + 6.0
    } else {
        (caret.x - width - 6.0).max(4.0)
    };
    let below = caret.y + caret.h + 4.0;
    let top = if below + height <= bounds.h - 4.0 { below } else { (caret.y - height - 4.0).max(4.0) };
    ui.overlay(Rect::new(left, top, width, height), |ui| {
        ui.boxed(
            BoxWidth::Fixed(width),
            BoxStyle::new(Some(ColorToken::SurfaceOverlay), Some(ColorToken::Border), 6.0),
            Edges::all(4.0),
            0.0,
            |ui| {
                for (index, path) in wiki.candidates.iter().enumerate() {
                    let name = path.rsplit('/').next().unwrap_or(path);
                    let name = name
                        .strip_suffix(".md")
                        .or_else(|| name.strip_suffix(".MD"))
                        .unwrap_or(name);
                    let dir = path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("");
                    ui.click_row(
                        &format!("note:wiki:{index}"),
                        Edges::symmetric(6.0, 3.0),
                        3.0,
                        false,
                        true,
                        |ui| {
                            ui.row_fit(6.0, |ui| {
                                ui.icon_named("file-text", 13.0, ColorToken::TextMuted);
                                let style = TextStyle::new(TextRole::Caption).color(ColorToken::TextSecondary);
                                ui.text_styled_fit(name, style);
                                if !dir.is_empty() {
                                    let dim = TextStyle::new(TextRole::Micro).color(ColorToken::TextMuted);
                                    ui.text_styled_fit(dir, dim);
                                }
                            });
                        },
                    );
                }
            },
        );
    });
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
