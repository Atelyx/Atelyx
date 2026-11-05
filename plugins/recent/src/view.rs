//! 最近打开面板整面帧：空态与条目卡片列表；行点击交给宿主按扩展名打开文件。

use atelyx_plugin_protocol::method::RecentEntry;
use atelyx_plugin_protocol::paint::{ColorToken, TextWeight};
use atelyx_plugin_sdk::frame_ui::{BoxStyle, BoxWidth, Edges, TextRole, TextStyle, Ui};

use crate::model::Model;

/// 列表内衬（与宿主其它列表面板同档）。
const LIST_PAD_X: f32 = 12.0;
const LIST_PAD_Y: f32 = 10.0;
/// 卡片之间的间距。
const CARD_GAP: f32 = 6.0;
/// 卡片内衬（左内衬让图标与文字成组，右侧留出时间列的空隙）。
const CARD_PAD_X: f32 = 10.0;
const CARD_PAD_Y: f32 = 8.0;
/// 图标与文字列的间距。
const ICON_GAP: f32 = 10.0;
/// 条目类型图标（14px 与宿主行图标同档）。
const ICON_PX: f32 = 14.0;

pub fn frame(ui: &mut Ui<'_, '_>, model: &Model) {
    // 失败态先于就绪门控：首份回执就失败时 `ready` 仍是 None，不能被吞掉
    if let Some(error) = &model.error {
        if model.entries.is_empty() {
            failure_state(ui, error, model.radii.md);
            return;
        }
    }
    // 首份清单（或切容器后的首份）未到：先画加载态，绝不先露空态
    let Some(ready) = model.ready else {
        ui.loading_state();
        return;
    };
    if !ready {
        ui.empty_state("clock", "打开仓库", "打开仓库后显示。");
        return;
    }
    if model.entries.is_empty() {
        ui.empty_state("clock", "暂无最近打开", "打开文件后出现在这里。");
        return;
    }
    let now = crate::now_secs();
    let entries = model.entries.clone();
    let failure = model.error.clone();
    let gen = model.gen;
    let radius = model.radii.md;
    ui.scroll("page", CARD_GAP, move |page| {
        page.pad(LIST_PAD_X, LIST_PAD_Y, move |inner| {
            inner.column(CARD_GAP, move |col| {
                // 列表在手时失败（打开文件被拒 / 重取失败）以提示行呈现，不当成空列表
                if let Some(error) = &failure {
                    col.notice(error.clone(), ColorToken::Danger);
                }
                for (index, entry) in entries.iter().enumerate() {
                    card(col, index, gen, entry, now, radius);
                }
            });
        });
    });
}

/// 一条最近打开：类型图标 + 文件名 + 所在文件夹 / 相对时间。
/// 命中区 id 只带清单世代与行号（路径可能超出命中区 id 的字节上限）。
fn card(ui: &mut Ui<'_, '_>, index: usize, gen: u64, entry: &RecentEntry, now: i64, radius: f32) {
    let name = entry.file.rsplit('/').next().unwrap_or(&entry.file).to_string();
    let parent = entry.file.rsplit_once('/').map(|(dir, _)| dir.to_string()).unwrap_or_default();
    let time = crate::time::relative(entry.opened_at, now);
    let icon = kind_icon(entry.kind);
    let id = format!("open:{gen}:{index}");
    let hint_id = id.clone();
    let full = entry.file.clone();
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
                            text.elide_with_hint(
                                &hint_id,
                                name.clone(),
                                name.clone(),
                                TextStyle::new(TextRole::Body),
                            );
                            text.row(0.0, move |meta| {
                                meta.elide_with_hint(
                                    &hint_id,
                                    parent.clone(),
                                    full.clone(),
                                    TextStyle::new(TextRole::Micro),
                                );
                                meta.text_styled_fit(time.clone(), TextStyle::new(TextRole::Micro));
                            });
                        });
                    });
                },
            );
        },
    );
}

/// 条目类型 → 行首图标（笔记 / 表格 / 画布与宿主同一套形状）。
fn kind_icon(kind: atelyx_plugin_protocol::method::RecentKind) -> &'static str {
    use atelyx_plugin_protocol::method::RecentKind;
    match kind {
        RecentKind::Note => "file-text",
        RecentKind::Table => "table",
        RecentKind::Canvas => "palette",
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
                "最近打开加载失败",
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
