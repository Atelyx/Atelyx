//! 属性面板绘制：笔记属性编辑器（共用库）+ 画布属性只读列表 + 空态占位。

use atelyx_plugin_protocol::method::{CanvasRefStatus, CanvasSnapshotResult};
use atelyx_plugin_props as props;
use atelyx_plugin_protocol::paint::{ColorToken, TextWeight};
use atelyx_plugin_sdk::frame_ui::{TextRole, TextStyle, Ui};

use crate::model::InspectorModel;

/// 属性行标签列宽（与面板其它键值行同宽）。
const LABEL_W: f32 = 64.0;
/// 属性行间距。
const ROW_GAP: f32 = 8.0;
pub fn frame(ui: &mut Ui<'_, '_>, model: &mut InspectorModel) {
    if model.canvas_visible {
        match model.canvas.clone() {
            Some(snapshot) => canvas_panel(ui, &snapshot),
            // 快照未到（画布刚切到前台）先给空态，避免整面空白
            None => placeholder(ui, "画布属性", "打开一篇画布后在此查看其属性。"),
        }
        return;
    }
    if !model.open {
        placeholder(ui, "笔记属性", "打开一篇笔记后在此查看与编辑其属性。");
        return;
    }
    match model.props.data.clone() {
        None => {
            ui.column(0.0, |ui| {
                ui.caption("当前笔记无属性");
            });
        }
        Some((_, false)) => error_row(ui),
        Some(_) => props::panel(ui, model),
    }
}

/// 属性浮层：类型菜单（点击行内类型徽标展开）。
pub fn overlays(ui: &mut Ui<'_, '_>, model: &InspectorModel, pointer: (f32, f32)) {
    let Some(index) = model.props.type_menu else { return };
    let left = pointer.0.min(ui.bounds().w - 140.0).max(4.0);
    props::type_menu(ui, model, index, left, pointer.1 + 6.0);
}

fn placeholder(ui: &mut Ui<'_, '_>, title: &str, desc: &str) {
    ui.fill_column(0.0, |ui| {
        ui.fill_spacer();
        ui.center_column(16.0, |ui| {
            ui.icon_named("info", 64.0, ColorToken::TextMuted);
            let title_style = TextStyle::new(TextRole::Title).weight(TextWeight::Bold);
            ui.text_styled(title.to_string(), title_style);
            let desc_style = TextStyle::new(TextRole::Body).color(ColorToken::TextMuted);
            ui.text_styled(desc.to_string(), desc_style);
        });
        ui.fill_spacer();
    });
}

fn error_row(ui: &mut Ui<'_, '_>) {
    ui.pad(16.0, 6.0, |ui| {
        ui.row(ROW_GAP, |row| {
            row.icon_named("triangle-alert", 13.0, ColorToken::Danger);
            let style = TextStyle::new(TextRole::Caption).color(ColorToken::TextSecondary);
            row.text_styled("YAML 格式错误，属性面板暂不可编辑", style);
        });
    });
}

/// 画布属性：有选中卡 → 卡字段与连线；无选中 → 画布统计。
fn canvas_panel(ui: &mut Ui<'_, '_>, snapshot: &CanvasSnapshotResult) {
    let Some(path) = snapshot.path.as_deref() else { return };
    ui.pad(12.0, 8.0, |ui| {
        ui.column(4.0, |ui| {
            match &snapshot.selection {
                Some(node) => {
                    ui.row(6.0, |row| {
                        row.icon_named("box", 12.0, ColorToken::TextMuted);
                        row.text_styled(
                            node.title.clone(),
                            TextStyle::new(TextRole::Caption).color(ColorToken::Text),
                        );
                    });
                    row(ui, "类型", &node.node_type);
                    row(ui, "位置", &format!("x {:.0} · y {:.0}", node.x, node.y));
                    row(ui, "尺寸", &format!("{:.0} × {:.0}", node.width, node.height));
                    if let Some(file) = &node.file {
                        row(ui, "引用来源", file);
                        let status = match node.ref_status {
                            Some(CanvasRefStatus::Loaded) => "已加载",
                            Some(CanvasRefStatus::Binary) => "二进制",
                            Some(CanvasRefStatus::Missing) => "缺失",
                            _ => "加载中",
                        };
                        row(ui, "引用状态", status);
                    }
                    if node.edges.is_empty() {
                        row(ui, "连线", "无");
                    } else {
                        for edge in &node.edges {
                            let dir = if edge.direction == "out" { "→" } else { "←" };
                            let tag = match edge.role.as_str() {
                                "association" => "关联",
                                "reference-consumed" => "数据流·已消费",
                                "reference-unconsumed" => "数据流·未消费",
                                "lineage" => "数据流·产出",
                                _ => "数据流",
                            };
                            row(ui, "连线", &format!("{tag} {dir} {}", edge.peer_title));
                        }
                    }
                }
                None => {
                    row(ui, "标题", &file_label(path));
                    row(ui, "卡", &snapshot.stats.cards.to_string());
                    row(ui, "分组", &snapshot.stats.groups.to_string());
                    row(ui, "连线", &snapshot.stats.edges.to_string());
                    ui.text("选中一张卡查看其属性", TextRole::Micro);
                }
            }
            ui.spacer(4.0);
        });
    });
}

fn row(ui: &mut Ui<'_, '_>, label: &'static str, value: &str) {
    ui.row(ROW_GAP, |row| {
        row.side(LABEL_W, |side| side.text(label, TextRole::Micro));
        row.text_styled(value.to_string(), TextStyle::new(TextRole::Caption).color(ColorToken::Text));
    });
}

/// 画布标题：取文件名去掉扩展名（与宿主视图标签同口径）。
fn file_label(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_string(),
        _ => name.to_string(),
    }
}
