//! 属性面板绘制：笔记属性编辑器（共用库）+ 画布属性只读列表 + 空态占位。

use atelyx_plugin_protocol::method::{CanvasRefStatus, CanvasSnapshotResult};
use atelyx_plugin_props as props;
use atelyx_plugin_protocol::paint::ColorToken;
use atelyx_plugin_sdk::frame_ui::{TextRole, TextStyle, Ui};

use crate::model::InspectorModel;

/// 属性行标签列宽（与面板其它键值行同宽）。
const LABEL_W: f32 = 64.0;
/// 属性行间距。
const ROW_GAP: f32 = 8.0;
pub fn frame(ui: &mut Ui<'_, '_>, model: &mut InspectorModel) {
    // 首份属性回执未到（首次挂面 / 切仓库）：只画加载态，不提前给空态
    if !model.loaded {
        ui.loading_state();
        return;
    }
    if model.canvas_visible {
        match model.canvas.clone() {
            Some(snapshot) => canvas_panel(ui, &snapshot),
            // 无快照 = 当前没有打开的画布：给说明文字，首份属性回执的等待已由上方 `loaded` 门覆盖
            None => ui.empty_state("info", "画布属性", "打开一篇画布后在此查看其属性。"),
        }
        return;
    }
    // 读失败时旧数据照常显示，错误行留在顶上并可重试
    if let Some(error) = model.load_error.clone() {
        load_error_row(ui, &error);
    }
    if !model.open {
        if model.load_error.is_none() {
            ui.empty_state("info", "笔记属性", "打开一篇笔记后在此查看与编辑其属性。");
        }
        return;
    }
    match model.props.data.clone() {
        None => ui.empty_state("info", "笔记属性", "当前笔记无属性。"),
        Some((_, false)) => error_row(ui),
        Some(_) => props::panel(ui, model),
    }
}

/// 属性浮层：类型菜单（点击行内类型徽标展开）。
pub fn overlays(ui: &mut Ui<'_, '_>, model: &InspectorModel, pointer: (f32, f32)) {
    let Some(index) = model.props.type_menu else { return };
    props::type_menu(ui, model, index, pointer.0, pointer.1 + 6.0);
}

/// 读取失败：原因 + 重试，下方继续显示上次读到的属性。
fn load_error_row(ui: &mut Ui<'_, '_>, error: &str) {
    ui.pad(16.0, 6.0, |ui| {
        ui.row(ROW_GAP, |row| {
            row.icon_named("triangle-alert", 13.0, ColorToken::Danger);
            row.text_styled(
                error.to_string(),
                TextStyle::new(TextRole::Caption).color(ColorToken::Danger),
            );
            row.grow();
            row.button("props:retry", "重试");
        });
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
