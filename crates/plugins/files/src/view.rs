//! 文件面板整面帧：工具条 + 当前容器行 + 文件树（含就地输入行与拖拽落点高亮）。
//! 行的缩进、图标色、展开箭头与落点状态都由 [`crate::model`] 算好，这里只排布。

use atelyx_plugin_protocol::method::Radii;
use atelyx_plugin_protocol::paint::{ColorToken, PaintColor, TextWeight};
use atelyx_plugin_sdk::frame_ui::{BoxStyle, BoxWidth, Edges, TextRole, TextStyle, Ui};

use crate::model::{extension_of, stem_of, ContainerRow, InlineEdit, Row, View};

/// 行左右内衬。
const ROW_PAD_X: f32 = 8.0;
/// 每层缩进。
const INDENT: f32 = 16.0;
/// 行内元素间距。
const ROW_GAP: f32 = 6.0;
/// 行内容上下内衬（参考线要贯穿整行，行高由它定）。
const ROW_PAD_Y: f32 = 8.0;
/// 层级参考线宽。
const GUIDE_W: f32 = 1.0;
/// 图标边长。
const ICON: f32 = 14.0;
/// 工具条内衬。
const TOOLBAR_PAD_X: f32 = 8.0;
const TOOLBAR_PAD_Y: f32 = 6.0;
/// 树容器上下留白。
const TREE_PAD_TOP: f32 = 6.0;
const TREE_PAD_BOTTOM: f32 = 12.0;
/// 条目行高（容器行与文件行同高）。
const ROW_H: f32 = 32.0;
/// 行侧缝宽度：条目不满宽的留白，同时是「拖回仓库根」的落点。
const GUTTER: f32 = 8.0;
/// 列表下方空白落点的高度。
const DROP_ZONE_H: f32 = 120.0;

/// 非文件树状态下，容器行下方展示的内容。
#[derive(Clone, Copy)]
enum Pane<'a> {
    Loading,
    Failure(&'a str),
    NoRepo,
    EmptyRepo,
}

pub fn frame(ui: &mut Ui<'_, '_>, view: &View, radii: &Radii) {
    ui.fill_column(0.0, |ui| {
        toolbar(ui, view.all_expanded, view.in_repo);
        ui.rule();
        if let Some(notice) = view.rename_notice.as_deref() {
            rename_notice(ui, notice, radii.sm);
        }
        if let Some(notice) = view.notice.as_deref() {
            ui.notice(notice.to_string(), ColorToken::Danger);
        }
        let pane = if view.loading {
            Pane::Loading
        } else if let Some(error) = view.error.as_ref().filter(|_| view.rows.is_empty()) {
            Pane::Failure(error)
        } else if !view.in_repo {
            Pane::NoRepo
        } else if view.rows.is_empty() && view.inline.is_none() {
            Pane::EmptyRepo
        } else {
            list(ui, view, radii);
            return;
        };
        state_page(ui, view, radii, pane);
    });
}

/// 重名顺延提醒条：警示色边、文案占满、右侧关闭钮。
fn rename_notice(ui: &mut Ui<'_, '_>, text: &str, radius: f32) {
    ui.boxed(
        BoxWidth::Fill,
        BoxStyle::new(Some(ColorToken::Surface), Some(ColorToken::Warning), radius),
        Edges { left: ROW_PAD_X, right: 4.0, top: 4.0, bottom: 4.0 },
        0.0,
        |bar| {
            bar.row(0.0, |line| {
                line.text_styled(
                    text.to_string(),
                    TextStyle::new(TextRole::Caption).color(ColorToken::Warning),
                );
                line.icon_button_named(
                    crate::DISMISS_RENAME_NOTICE,
                    "x",
                    12.0,
                    18.0,
                    ColorToken::TextMuted,
                    true,
                );
            });
        },
    );
}

/// 工具条：打开仓库 / 协作空间 / 排序 / 刷新 / 展开或收起全部。
fn toolbar(ui: &mut Ui<'_, '_>, all_expanded: bool, in_repo: bool) {
    ui.boxed(
        BoxWidth::Fill,
        BoxStyle::new(Some(ColorToken::Surface), None, 0.0),
        Edges {
            left: TOOLBAR_PAD_X,
            right: TOOLBAR_PAD_X,
            top: TOOLBAR_PAD_Y,
            bottom: TOOLBAR_PAD_Y,
        },
        0.0,
        |bar| {
            bar.row(2.0, |line| {
                line.hint(crate::OPEN_VAULT, "打开文件夹为仓库");
                line.icon_button_named(
                    crate::OPEN_VAULT,
                    "folder-open",
                    15.0,
                    22.0,
                    ColorToken::TextMuted,
                    true,
                );
                line.hint(crate::SPACE_MENU, "协作空间");
                line.icon_button_named(
                    crate::SPACE_MENU,
                    "cloud",
                    15.0,
                    22.0,
                    ColorToken::TextMuted,
                    true,
                );
                line.hint(crate::SORT, "排序方式");
                line.icon_button_named(
                    crate::SORT,
                    "arrow-up-down",
                    14.0,
                    22.0,
                    ColorToken::TextMuted,
                    true,
                );
                line.grow();
                line.hint(crate::REFRESH, "重新读取文件树");
                // 未进仓库时没有可重读的树：与旧面板一致，刷新钮保持不可点
                line.icon_button_named(
                    crate::REFRESH,
                    "refresh-cw",
                    14.0,
                    22.0,
                    ColorToken::TextMuted,
                    in_repo,
                );
                line.hint(
                    crate::TOGGLE_ALL,
                    if all_expanded { "收起全部文件夹" } else { "展开全部文件夹" },
                );
                line.icon_button_named(
                    crate::TOGGLE_ALL,
                    if all_expanded { "chevrons-down-up" } else { "chevrons-up-down" },
                    14.0,
                    22.0,
                    ColorToken::TextMuted,
                    true,
                );
            });
        },
    );
}

/// 文件树：滚动区撑满剩余高度（容器列表在前，激活容器的文件树就地展开在其后）。
fn list(ui: &mut Ui<'_, '_>, view: &View, radii: &Radii) {
    let radius = radii.sm;
    let rows = view.rows.clone();
    let inline = view.inline.clone();
    let containers = view.containers.clone();
    let hint = view.drop_hint.clone();
    let dragging = view.dragging;
    let root_active = view.vault_drop_target;
    let root_rejected = view.vault_drop_rejected;
    let seam_base = containers.len();
    ui.stack(move |layer| {
        // 树视口整块也是根落点：内容不满一屏时，行下方的空白照样接收拖拽（行自身的命中区压在上面）
        layer.hit_area(crate::DROP_BOTTOM);
        layer.scroll("tree", 0.0, move |page| {
            page.column(0.0, move |col| {
                col.spacer(TREE_PAD_TOP);
                for (index, row) in containers.iter().enumerate() {
                    // 容器行也带两侧缝：缝隙与行体都指向仓库根（拖到行上 = 移入根）
                    col.row(0.0, |line| {
                        seam(line, format!("{}{index}:l", crate::DROP_ROOT));
                        container_row(line, row, radius);
                        seam(line, format!("{}{index}:r", crate::DROP_ROOT));
                    });
                    // 激活行就地展开文件树（其余行收起）
                    if row.active && row.open {
                        tree_block(col, &rows, &inline, radius, seam_base);
                    }
                }
                if let Some(why) = &hint {
                    col.notice(why.clone(), ColorToken::Danger);
                }
                // 内容末尾的根落点：拖拽期给出可见落位区（命中由树视口那块承担）
                if dragging {
                    root_drop_zone(col, radius, root_active, root_rejected);
                }
                col.spacer(TREE_PAD_BOTTOM);
            });
        });
    });
}

/// 非文件树状态页：容器行照常铺在最前，加载 / 失败 / 空态排在容器行下方。
/// 撑满列而非滚动页：容器行下方没有可撑高的余量，状态块居中不了。
fn state_page(ui: &mut Ui<'_, '_>, view: &View, radii: &Radii, pane: Pane<'_>) {
    let containers = view.containers.clone();
    let radius = radii.sm;
    let tile_radius = radii.md;
    ui.stack(move |layer| {
        // 容器行下方整块也是根落点：任何状态都能把文件拖进仓库根
        layer.hit_area(crate::DROP_BOTTOM);
        layer.fill_column(0.0, move |col| {
            col.spacer(TREE_PAD_TOP);
            for (index, row) in containers.iter().enumerate() {
                col.row(0.0, |line| {
                    seam(line, format!("{}{index}:l", crate::DROP_ROOT));
                    container_row(line, row, radius);
                    seam(line, format!("{}{index}:r", crate::DROP_ROOT));
                });
            }
            col.spacer(TREE_PAD_BOTTOM);
            match pane {
                Pane::Loading => col.loading_state(),
                Pane::Failure(error) => failure_block(col, error, tile_radius),
                Pane::NoRepo => col.empty_state_action(
                    "folder-open",
                    "打开仓库",
                    "用工具条打开一个文件夹作为仓库，或登录协作空间。",
                    crate::OPEN_VAULT,
                    "打开文件夹",
                ),
                Pane::EmptyRepo => {
                    col.empty_state("folder", "暂无文件", "右键新建笔记或文件夹。");
                }
            }
        });
    });
}

/// 激活容器的文件树（就地展开在该容器行之后）。
/// 重命名中的行由输入行替换；草稿行插在目标行之后（目录为空串时在首行）。
fn tree_block(
    ui: &mut Ui<'_, '_>,
    rows: &[Row],
    inline: &Option<(InlineEdit, usize)>,
    radius: f32,
    seam_base: usize,
) {
    let (renaming, draft_at) = match inline {
        Some((InlineEdit::Rename { path, .. }, _)) => (Some(path.clone()), None),
        Some((InlineEdit::Draft { dir, .. }, at)) => (None, Some((dir.clone(), *at))),
        None => (None, None),
    };
    if let Some((dir, at)) = &draft_at {
        if dir.is_empty() && *at == 0 {
            inline_row(ui, inline.as_ref().unwrap().0.clone(), 0, radius);
        }
    }
    for (index, row) in rows.iter().enumerate() {
        // 行与容器行共用缝序号区：按「容器数 + 行号」编，同一帧内不重复
        let base = seam_base + index;
        if renaming.as_deref() == Some(row.path.as_str()) {
            ui.row(0.0, |line| {
                seam(line, format!("{}{base}:l", crate::DROP_ROOT));
                inline_row(line, inline.as_ref().unwrap().0.clone(), row.depth, radius);
                seam(line, format!("{}{base}:r", crate::DROP_ROOT));
            });
            continue;
        }
        ui.row(0.0, |line| {
            seam(line, format!("{}{base}:l", crate::DROP_ROOT));
            tree_row(line, row, radius);
            seam(line, format!("{}{base}:r", crate::DROP_ROOT));
        });
        if let Some((_, at)) = &draft_at {
            if *at == index + 1 {
                inline_row(
                    ui,
                    inline.as_ref().unwrap().0.clone(),
                    row.depth + usize::from(row.is_dir),
                    radius,
                );
            }
        }
    }
}

/// 行侧缝（根落点）：行体两侧各一条，任意滚动位置都能放下。
fn seam(ui: &mut Ui<'_, '_>, id: String) {
    ui.hover_box(id, BoxWidth::Fixed(GUTTER), Edges::all(0.0), |line| {
        line.spacer(ROW_H);
    });
}

/// 列表下方空白的根落点（只在拖拽会话里出现；悬停 = 释放后移到仓库根）。
fn root_drop_zone(ui: &mut Ui<'_, '_>, radius: f32, active: bool, rejected: bool) {
    let (fill, border, color) = if rejected {
        (Some(ColorToken::DangerSoft), ColorToken::Danger, ColorToken::Danger)
    } else if active {
        (Some(ColorToken::AccentSoft), ColorToken::Accent, ColorToken::Accent)
    } else {
        (None, ColorToken::Border, ColorToken::TextMuted)
    };
    let label = if active { "松手移到根目录" } else { "移到根目录" };
    let pad = ((DROP_ZONE_H - 16.0) / 2.0).max(0.0);
    ui.boxed(
        BoxWidth::Fill,
        BoxStyle::new(fill, Some(border), radius),
        Edges::all(0.0),
        0.0,
        move |inside| {
            inside.spacer(pad);
            inside.row(0.0, move |line| {
                line.grow();
                line.text_styled(label, TextStyle::new(TextRole::Caption).color(color));
                line.grow();
            });
            inside.spacer(pad);
        },
    );
}

/// 一行：层级参考线 + 缩进 + 展开箭头（目录）+ 图标 + 主名 + 后缀徽标（文件）。
/// 主名吃剩余宽度并按尾部省略，悬停整行给出完整文件名；后缀徽标按固有宽留在行尾，恒显且右缘对齐。
fn tree_row(ui: &mut Ui<'_, '_>, row: &Row, radius: f32) {
    let id = crate::row_id(&row.path, row.is_dir);
    let name = row.name.clone();
    let is_dir = row.is_dir;
    let expanded = row.expanded;
    let color = row.color.clone();
    let doc_icon = row.icon.clone();
    let ext = if is_dir { String::new() } else { ext_label(&name) };
    let label = row_label(&name, is_dir);
    let depth = row.depth;
    let guides = guide_xs(depth);
    let drop_target = row.drop_target;
    let drop_rejected = row.drop_rejected;
    let active = row.active;
    let tint = if drop_rejected {
        Some(ColorToken::DangerSoft)
    } else if drop_target || active {
        Some(ColorToken::AccentSoft)
    } else {
        None
    };
    let name_color = if active { ColorToken::Accent } else { ColorToken::TextSecondary };
    let icon_color = if active { ColorToken::Accent } else { ColorToken::TextMuted };
    let ext_color = if active { ColorToken::Accent } else { ColorToken::TextMuted };
    ui.hint(&id, &name);
    ui.click_row_tinted(
        id,
        BoxWidth::Fill,
        Edges { left: 0.0, right: ROW_PAD_X, top: 0.0, bottom: 0.0 },
        tint,
        move |line| {
            line.row(0.0, move |whole| {
                // 层级参考线：每层一条，落在该层上级展开箭头的中心线上，贯穿整行高度
                let mut x = 0.0;
                for guide in guides.iter() {
                    if *guide > x {
                        whole.spacer_width(guide - x);
                    }
                    whole.bar(GUIDE_W, ColorToken::Border);
                    x = guide + GUIDE_W;
                }
                let content_x = ROW_PAD_X + depth as f32 * INDENT;
                if content_x > x {
                    whole.spacer_width(content_x - x);
                }
                whole.column(0.0, move |body| {
                    body.spacer(ROW_PAD_Y);
                    body.row(ROW_GAP, move |content| {
                        if is_dir {
                            content.icon_named(
                                if expanded { "chevron-down" } else { "chevron-right" },
                                ICON,
                                ColorToken::TextMuted,
                            );
                        }
                        let icon = match doc_icon.as_deref() {
                            Some(icon) => icon,
                            None if is_dir => "folder",
                            None => file_icon(&name),
                        };
                        match color.as_deref().and_then(parse_hex) {
                            Some((r, g, b, a)) => content
                                .icon_named_rgba(icon, ICON, PaintColor::Rgba { rgba: [r, g, b, a] }),
                            None => content.icon_named(icon, ICON, icon_color),
                        }
                        content.text_styled(
                            label,
                            TextStyle::new(TextRole::Caption).color(name_color),
                        );
                        if !ext.is_empty() {
                            content.text_styled_fit(
                                ext,
                                TextStyle::new(TextRole::Micro).mono().color(ext_color),
                            );
                        }
                    });
                    body.spacer(ROW_PAD_Y);
                });
            });
        },
    );
    let _ = radius;
}

/// 层级参考线的 x 坐标：每层一条，落在该层上级展开箭头的中心线上。
fn guide_xs(depth: usize) -> Vec<f32> {
    (1..=depth)
        .map(|level| ROW_PAD_X + ICON / 2.0 + (level - 1) as f32 * INDENT)
        .collect()
}



/// 容器行：激活行淡底 + 就地展开箭头；非激活行点击切过去。
fn container_row(ui: &mut Ui<'_, '_>, row: &ContainerRow, radius: f32) {
    let id = crate::container_id(&row.id);
    let label = row.label.clone();
    let hint = row.hint.clone();
    let is_local = row.is_local;
    let active = row.active;
    let open = row.open;
    // 激活行 = 强调淡底（拖拽落点更深 / 根被拒 = 警示色）；非激活行透明
    let tint = if row.drop_rejected {
        Some(ColorToken::DangerSoft)
    } else if row.drop_target {
        Some(ColorToken::Accent)
    } else if active {
        Some(ColorToken::AccentSoft)
    } else {
        None
    };
    let _ = radius;
    ui.hint(&id, &hint);
    ui.click_row_tinted(
        id,
        BoxWidth::Fill,
        Edges { left: ROW_PAD_X, right: ROW_PAD_X, top: 8.0, bottom: 8.0 },
        tint,
        move |line| {
            line.row(ROW_GAP, move |content| {
                content.icon_named(
                    if open { "chevron-down" } else { "chevron-right" },
                    ICON,
                    ColorToken::TextMuted,
                );
                content.icon_named(
                    if is_local { "hard-drive" } else { "cloud" },
                    ICON,
                    if active { ColorToken::Accent } else { ColorToken::TextMuted },
                );
                content.text_styled(
                    label.clone(),
                    TextStyle::new(TextRole::Caption).weight(TextWeight::SemiBold),
                );
            });
        },
    );
}

/// 就地输入行（重命名 / 新建草稿）：缩进到目标层级。
fn inline_row(ui: &mut Ui<'_, '_>, edit: InlineEdit, depth: usize, radius: f32) {
    let indent = ROW_PAD_X + depth as f32 * INDENT;
    let value = edit.value().to_string();
    let placeholder = match &edit {
        InlineEdit::Draft { kind, .. } => kind.label(),
        InlineEdit::Rename { .. } => "名称".to_string(),
    };
    ui.boxed(
        BoxWidth::Fill,
        BoxStyle::new(None, None, radius),
        Edges { left: indent, right: ROW_PAD_X, top: 2.0, bottom: 2.0 },
        0.0,
        move |line| {
            line.row(ROW_GAP, move |content| {
                content.icon_named("pencil", ICON, ColorToken::TextMuted);
                content.text_input_inline(crate::INLINE, value.clone(), placeholder.clone());
            });
        },
    );
}

/// 行上显示的名字：目录显示全名（`v1.2` 的点不是扩展名），文件只显示主名。
fn row_label(name: &str, is_dir: bool) -> String {
    if is_dir { name.to_string() } else { stem_of(name).to_string() }
}

/// 文件名的扩展名标签（`a.md` → `MD`；无扩展名 = 空）。
fn ext_label(name: &str) -> String {
    extension_of(name).map(str::to_uppercase).unwrap_or_default()
}

/// 文件名 → lucide 图标名（按扩展名分档）。
fn file_icon(name: &str) -> &'static str {
    let ext = extension_of(name).unwrap_or_default();
    match ext.to_ascii_lowercase().as_str() {
        "md" => "file-text",
        "atb" => "table",
        "atlx" => "canvas",
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "svg" => "image",
        "json" | "toml" | "yaml" | "yml" | "rs" | "ts" | "js" | "py" | "sh" => "file-code",
        _ => "file",
    }
}

/// `#rrggbb` / `#rrggbbaa` 解析成 RGBA（解析失败回落静默色）。
fn parse_hex(hex: &str) -> Option<(u8, u8, u8, u8)> {
    let body = hex.strip_prefix('#')?;
    let parse = |range: std::ops::Range<usize>| u8::from_str_radix(body.get(range)?, 16).ok();
    match body.len() {
        6 => Some((parse(0..2)?, parse(2..4)?, parse(4..6)?, 255)),
        8 => Some((parse(0..2)?, parse(2..4)?, parse(4..6)?, parse(6..8)?)),
        _ => None,
    }
}

/// 取数失败且清单为空：可读原因 + 重试（在撑满列里上下居中）。
fn failure_block(ui: &mut Ui<'_, '_>, error: &str, radius: f32) {
    let error = error.to_string();
    ui.fill_spacer();
    ui.center_column(8.0, move |center| {
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
            "文件树读取失败",
            TextStyle::new(TextRole::Body).weight(TextWeight::SemiBold),
        );
        center.text_styled(
            error.clone(),
            TextStyle::new(TextRole::Label).color(ColorToken::TextMuted),
        );
        center.button(crate::RETRY, "重试");
    });
    ui.fill_spacer();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 参考线落在上级箭头中心线上，每层一档。
    #[test]
    fn guides_follow_the_arrow_centers() {
        assert!(guide_xs(0).is_empty());
        assert_eq!(guide_xs(1), vec![ROW_PAD_X + ICON / 2.0]);
        assert_eq!(guide_xs(2), vec![15.0, 31.0]);
    }

    /// 主名去掉后缀；隐藏文件与无后缀名原样保留。
    #[test]
    fn stem_drops_the_extension() {
        assert_eq!(stem_of("plan.atb"), "plan");
        assert_eq!(stem_of("方案.md"), "方案");
        assert_eq!(stem_of(".gitignore"), ".gitignore");
        assert_eq!(stem_of("README"), "README");
        assert_eq!(stem_of("a.b.c"), "a.b");
    }

    /// 目录名不按扩展名截断：带点的目录名整名显示，文件才只留主名。
    #[test]
    fn directory_rows_keep_their_full_name() {
        assert_eq!(row_label("v1.2", true), "v1.2");
        assert_eq!(row_label("api.example.com", true), "api.example.com");
        assert_eq!(row_label("plan.atb", false), "plan");
        assert_eq!(row_label(".gitignore", false), ".gitignore");
    }
}
