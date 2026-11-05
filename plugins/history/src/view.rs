//! 仓库历史面板整面帧：空态与文件分组卡片列表；卡片点击让宿主打开该文件的历史弹层。

use atelyx_plugin_protocol::method::HistoryFileGroup;
use atelyx_plugin_protocol::paint::{ColorToken, TextWeight};
use atelyx_plugin_sdk::frame_ui::{BoxStyle, BoxWidth, Edges, TextRole, TextStyle, Ui};

use crate::model::Model;

/// 列表内衬（与宿主其它列表面板同档，纵向更紧：卡片本身已有内衬）。
const LIST_PAD_X: f32 = 12.0;
const LIST_PAD_Y: f32 = 4.0;
/// 卡片之间的间距。
const CARD_GAP: f32 = 6.0;
/// 卡片内衬。
const CARD_PAD_X: f32 = 10.0;
const CARD_PAD_Y: f32 = 8.0;
/// 图标与文字列的间距。
const ICON_GAP: f32 = 10.0;
/// 文件类型图标边长。
const ICON_PX: f32 = 14.0;

pub fn frame(ui: &mut Ui<'_, '_>, model: &Model) {
    // 失败态先于就绪门控：首份回执就失败时 `ready` 仍是 None，不能被吞掉
    if let Some(error) = &model.error {
        if model.groups.is_empty() {
            failure_state(ui, error, model.radii.md);
            return;
        }
    }
    let Some(ready) = model.ready else {
        // 首次聚合要扫全库侧文件或走服务端：这段时间给出加载态而不是空白
        ui.loading_state();
        return;
    };
    if !ready {
        ui.empty_state("history", "打开仓库", "打开仓库后显示。");
        return;
    }
    if model.groups.is_empty() {
        ui.empty_state("history", "暂无历史记录", "暂无版本记录。");
        return;
    }
    let groups = model.groups.clone();
    let failure = model.error.clone();
    let gen = model.gen;
    let radius = model.radii.md;
    ui.scroll("page", CARD_GAP, move |page| {
        page.pad(LIST_PAD_X, LIST_PAD_Y, move |inner| {
            inner.column(CARD_GAP, move |col| {
                // 列表在手时失败（打开历史被拒 / 重取失败）以提示行呈现，不当成空列表
                if let Some(error) = &failure {
                    col.notice(error.clone(), ColorToken::Danger);
                }
                for (index, group) in groups.iter().enumerate() {
                    card(col, index, gen, group, radius);
                }
            });
        });
    });
}

/// 一个文件的版本分组：类型图标 + 文件名 + 版本数徽标 + 完整路径。
/// 命中区 id 只带分组世代与行号（路径可能超出命中区 id 的字节上限）。
fn card(ui: &mut Ui<'_, '_>, index: usize, gen: u64, group: &HistoryFileGroup, radius: f32) {
    let name = group.file.rsplit('/').next().unwrap_or(&group.file).to_string();
    let path = group.file.clone();
    let count = format!("{} 版", group.count);
    let icon = kind_icon(&group.kind);
    let id = format!("open:{gen}:{index}");
    let hint_id = id.clone();
    ui.boxed(
        BoxWidth::Fill,
        BoxStyle::new(Some(ColorToken::Surface), None, radius),
        Edges::all(0.0),
        0.0,
        move |card| {
            card.click_box(
                id,
                BoxWidth::Fill,
                Edges::symmetric(CARD_PAD_X, CARD_PAD_Y),
                radius,
                false,
                true,
                move |row| {
                    row.row(ICON_GAP, move |line| {
                        line.icon_named(icon, ICON_PX, ColorToken::TextMuted);
                        line.column(2.0, move |text| {
                            let name_hint = hint_id.clone();
                            text.row(0.0, move |head| {
                                head.elide_with_hint(
                                    &name_hint,
                                    name.clone(),
                                    name.clone(),
                                    TextStyle::new(TextRole::Body),
                                );
                                head.chip(count.clone(), ColorToken::TextSecondary);
                            });
                            text.elide_with_hint(
                                &hint_id,
                                path.clone(),
                                path.clone(),
                                TextStyle::new(TextRole::Micro),
                            );
                        });
                    });
                },
            );
        },
    );
}

/// 文件类型 → 卡片图标（笔记 / 表格 / 画布与宿主同一套形状）。
fn kind_icon(kind: &str) -> &'static str {
    match kind {
        "note" => "file-text",
        "table" => "table",
        "canvas" => "pencil",
        _ => "file-text",
    }
}

/// 失败态：方形图标底 + 标题 + 错误文本 + 重试。
fn failure_state(ui: &mut Ui<'_, '_>, error: &str, radius: f32) {
    let error = error.to_string();
    ui.fill_column(0.0, move |outer| {
        outer.fill_spacer();
        outer.center_column(8.0, move |center| {
            center.boxed(
                BoxWidth::Fixed(40.0),
                BoxStyle::new(Some(ColorToken::Surface), None, radius),
                Edges::all(0.0),
                0.0,
                |tile| {
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
                "历史聚合失败",
                TextStyle::new(TextRole::Body).weight(TextWeight::SemiBold),
            );
            center.text_styled(
                error.clone(),
                TextStyle::new(TextRole::Label).color(ColorToken::TextMuted),
            );
            center.button("retry", "重试");
        });
        outer.fill_spacer();
    });
}
