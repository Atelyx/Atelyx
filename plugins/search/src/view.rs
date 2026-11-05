//! 搜索面板整面帧：模式胶囊 + 查询输入（固定在顶部）+ 结果列表（滚动区在下方）。
//! 命中排序、计数行与空态都由 [`crate::model`] 算好，这里只排布。

use atelyx_plugin_protocol::method::Radii;
use atelyx_plugin_protocol::paint::{ColorToken, TextWeight};
use atelyx_plugin_sdk::frame_ui::{BoxStyle, BoxWidth, Edges, TextRole, TextStyle, Ui};

use crate::model::{Empty, View};

/// 结果行左右内衬（行体本身满宽，悬停底一直铺到滚动条内缘）。
const ROW_PAD_X: f32 = 8.0;
/// 结果行纵向内衬。
const ROW_PAD_Y: f32 = 5.0;
/// 结果行主次行间距。
const ROW_LINE_GAP: f32 = 1.0;
/// 结果行图标边长与图标到文字的间距（次行缩进量由此派生，与主行首字对齐）。
const ROW_ICON: f32 = 13.0;
const ROW_GAP: f32 = 6.0;
/// 顶部区（模式 / 输入 / 计数）的内衬：下缘比上缘紧一档，紧贴分隔线。
const HEADER_PAD_X: f32 = 8.0;
const HEADER_PAD_TOP: f32 = 8.0;
const HEADER_PAD_BOTTOM: f32 = 6.0;
/// 顶部区内各行的间距。
const HEADER_GAP: f32 = 6.0;
/// 模式胶囊之间与输入行内元素之间的间隙。
const INLINE_GAP: f32 = 4.0;
/// 模式胶囊内衬。
const MODE_PAD_X: f32 = 8.0;
const MODE_PAD_Y: f32 = 3.0;
/// 清空钮边长（空查询时同一尺寸占位，输入框宽度不随清空钮显隐变化）。
const CLEAR_BOX: f32 = 20.0;
/// 列表底部留白：末行不贴面板下缘。
const LIST_PAD_BOTTOM: f32 = 8.0;

pub fn frame(ui: &mut Ui<'_, '_>, view: &View, query: &str, radii: &Radii) {
    ui.fill_column(0.0, |ui| {
        header(ui, view, query, radii);
        ui.rule();
        if let Some(error) = view.error.as_ref().filter(|_| view.rows.is_empty()) {
            failure_state(ui, error, radii);
            return;
        }
        if view.loading {
            ui.loading_state();
            return;
        }
        match view.empty {
            Empty::NoFiles => {
                ui.empty_state("folder-open", "打开仓库", "在文件面板打开仓库后，在这里按名称检索。");
            }
            Empty::NoMatch => {
                ui.empty_state("search", "无匹配文件", "换个关键词，或输入所在文件夹的名字。");
            }
            Empty::None => list(ui, view, radii),
        }
    });
}

/// 顶部区：模式胶囊 / 输入行 / 计数行。
fn header(ui: &mut Ui<'_, '_>, view: &View, query: &str, radii: &Radii) {
    let count = view.count.clone();
    let empty_query = query.is_empty();
    let query = query.to_string();
    let radius = radii.sm;
    ui.boxed(
        BoxWidth::Fill,
        BoxStyle::new(None, None, 0.0),
        Edges {
            left: HEADER_PAD_X,
            right: HEADER_PAD_X,
            top: HEADER_PAD_TOP,
            bottom: HEADER_PAD_BOTTOM,
        },
        0.0,
        move |area| {
            area.column(HEADER_GAP, move |col| {
                mode_row(col, radius);
                col.row(INLINE_GAP, move |line| {
                    line.text_input(crate::INPUT, query.clone(), "输入文件名过滤…");
                    if empty_query {
                        line.spacer_width(CLEAR_BOX);
                    } else {
                        line.hint(crate::CLEAR, "清空");
                        line.icon_button_named(
                            crate::CLEAR,
                            "x",
                            12.0,
                            CLEAR_BOX,
                            ColorToken::TextMuted,
                            true,
                        );
                    }
                });
                if let Some(count) = count {
                    col.text_styled(count, TextStyle::new(TextRole::Micro));
                }
            });
        },
    );
}

/// 模式胶囊：当前档位实底；未实现档位只有文字（不可点，悬停说明原因）。
fn mode_row(ui: &mut Ui<'_, '_>, radius: f32) {
    ui.row(INLINE_GAP, move |line| {
        active_chip(line, "按文件名", radius);
        for (label, tip) in [
            ("全局搜索", "暂未实现：搜索正文内容"),
            ("按标签搜索", "暂未实现：按标签筛选文件"),
        ] {
            let id = format!("mode:{label}");
            line.hint(&id, tip);
            let width = line.text_width(label, TextRole::Micro) + MODE_PAD_X * 2.0;
            line.hover_box(
                id,
                BoxWidth::Fixed(width),
                Edges::symmetric(MODE_PAD_X, MODE_PAD_Y),
                move |chip| {
                    chip.text_styled(label, TextStyle::new(TextRole::Micro));
                },
            );
        }
    });
}

/// 当前档位胶囊：实底 + 主字色。
fn active_chip(ui: &mut Ui<'_, '_>, label: &str, radius: f32) {
    let width = ui.text_width(label, TextRole::Micro) + MODE_PAD_X * 2.0;
    ui.boxed(
        BoxWidth::Fixed(width),
        BoxStyle::new(Some(ColorToken::Surface), None, radius),
        Edges::symmetric(MODE_PAD_X, MODE_PAD_Y),
        radius,
        move |chip| {
            chip.text_styled(
                label,
                TextStyle::new(TextRole::Micro).color(ColorToken::Text),
            );
        },
    );
}

/// 结果列表：滚动区撑满剩余高度，行按命中序排列；取数失败时先给一条提示行。
fn list(ui: &mut Ui<'_, '_>, view: &View, radii: &Radii) {
    let gen = view.gen;
    let radius = radii.sm;
    let failure = view.error.clone();
    ui.scroll("results", 0.0, move |page| {
        page.column(0.0, move |col| {
            if let Some(error) = failure.as_ref() {
                col.notice(error.clone(), ColorToken::Danger);
            }
            for row in &view.rows {
                search_row(col, gen, row, radius);
            }
            col.spacer(LIST_PAD_BOTTOM);
        });
    });
}

/// 取数失败且清单为空：可读原因 + 重试（保留「没有可搜索的文件」的语义不被误当成空仓库）。
fn failure_state(ui: &mut Ui<'_, '_>, error: &str, radii: &Radii) {
    let error = error.to_string();
    let radius = radii.md;
    ui.fill_column(0.0, move |outer| {
        outer.fill_spacer();
        outer.center_column(8.0, move |center| {
            center.boxed(
                BoxWidth::Fixed(40.0),
                BoxStyle::new(Some(ColorToken::Surface), None, radius),
                Edges::all(0.0),
                0.0,
                move |tile| {
                    tile.spacer(11.0);
                    tile.row(0.0, move |line| {
                        line.grow();
                        line.icon_named("triangle-alert", 18.0, ColorToken::TextMuted);
                        line.grow();
                    });
                    tile.spacer(11.0);
                },
            );
            center.text_styled_fit(
                "搜索清单读取失败",
                TextStyle::new(TextRole::Body).weight(TextWeight::SemiBold),
            );
            center.text_styled(
                error.clone(),
                TextStyle::new(TextRole::Label).color(ColorToken::TextMuted),
            );
            center.button(crate::RETRY, "重试");
        });
        outer.fill_spacer();
    });
}

/// 结果行：主行文件名 + 次行所在文件夹（缩进到文件名首字）；可打开者整行可点，
/// 附件没有面板内打开面，只呈现不响应点击。
fn search_row(ui: &mut Ui<'_, '_>, gen: u64, row: &crate::model::Row, radius: f32) {
    let id = crate::row_id(gen, row.index);
    let (name, parent, icon) = (row.name.clone(), row.parent.clone(), row.icon.clone());
    let openable = row.openable;
    let pad = Edges {
        left: ROW_PAD_X,
        right: ROW_PAD_X,
        top: ROW_PAD_Y,
        bottom: ROW_PAD_Y,
    };
    let content = move |ui: &mut Ui<'_, '_>| {
        let (name, parent, icon) = (name.clone(), parent.clone(), icon.clone());
        ui.column(ROW_LINE_GAP, move |lines| {
            lines.row(ROW_GAP, move |line| {
                line.icon_named(&icon, ROW_ICON, ColorToken::TextMuted);
                line.text_styled(
                    name.clone(),
                    TextStyle::new(TextRole::Caption).color(if openable {
                        ColorToken::Text
                    } else {
                        ColorToken::TextMuted
                    }),
                );
            });
            if !parent.is_empty() {
                // 次行与主行首字对齐：左内衬让出图标列
                lines.boxed(
                    BoxWidth::Fill,
                    BoxStyle::new(None, None, 0.0),
                    Edges {
                        left: ROW_ICON + ROW_GAP,
                        right: 0.0,
                        top: 0.0,
                        bottom: 0.0,
                    },
                    0.0,
                    move |inset| inset.text_styled(parent.clone(), TextStyle::new(TextRole::Micro)),
                );
            }
        });
    };
    // 悬停给完整相对路径：行内文件名与所在文件夹都可能被省略号截断
    let path = row_path(row);
    ui.hint(&id, &path);
    if openable {
        ui.click_row(id, pad, radius, false, true, content);
    } else {
        ui.hover_row(id, pad, content);
    }
}

/// 行的完整仓库相对路径（悬停提示用）。
fn row_path(row: &crate::model::Row) -> String {
    if row.parent.is_empty() {
        row.name.clone()
    } else {
        format!("{}/{}", row.parent, row.name)
    }
}
