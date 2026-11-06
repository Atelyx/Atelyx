//! 画布服务面（`canvas.snapshot`）：向属性面板等第三方视图提供与画布同源的属性快照，
//! 并负责把同一份快照随 `canvas:changed` 广播出去。

use atelyx_format::canvas::geometry::{EdgeRole, edge_role};
use atelyx_plugin_protocol::event::{self, CanvasChangedPayload};
use atelyx_plugin_protocol::method::{
    CanvasEdgeInfo, CanvasRefStatus, CanvasSelectionInfo, CanvasSnapshotResult, CanvasStats,
};
use atelyx_plugin_protocol::service::HostCallParams;
use atelyx_plugin_sdk::Context;

use crate::PluginState;
use crate::model::{RefContent, Session};

/// 服务名（归属本插件）。
pub const SERVICE: &str = "canvas";

/// 服务调用分发：只读快照。
pub async fn handle(plugin: &std::sync::Arc<PluginState>, ctx: &Context, params: HostCallParams) {
    let result = match params.method.as_str() {
        "snapshot" => serde_json::to_value(snapshot(plugin)).map_err(|error| error.to_string()),
        other => Err(format!("canvas 没有方法 {other}")),
    };
    match result {
        Ok(value) => {
            let _ = ctx.service_result(&params.call_id, value).await;
        }
        Err(error) => {
            let _ = ctx.service_error(&params.call_id, &error).await;
        }
    }
}

/// 当前画布快照：路径、落盘状态、选中卡属性与统计。
pub fn snapshot(plugin: &PluginState) -> CanvasSnapshotResult {
    plugin
        .with_session(|session| CanvasSnapshotResult {
            path: Some(session.file.clone()),
            dirty: session.dirty,
            selection: selection_of(session),
            stats: stats_of(&session.data),
        })
        .unwrap_or_default()
}

fn stats_of(data: &atelyx_format::canvas::CanvasFile) -> CanvasStats {
    let groups = data.nodes.iter().filter(|n| n.node_type == "group").count();
    CanvasStats {
        cards: data.nodes.len().saturating_sub(groups),
        groups,
        edges: data.edges.len(),
    }
}

fn selection_of(session: &Session) -> Option<CanvasSelectionInfo> {
    let node = session
        .view
        .selection
        .first()
        .and_then(|id| session.data.nodes.iter().find(|n| &n.id == id))?;
    let (width, height) = atelyx_format::canvas::geometry::display_size(node);
    let (_, title) = crate::model::card_head(node);
    let file = node.data.get("file").and_then(serde_json::Value::as_str);
    let edges = session
        .data
        .edges
        .iter()
        .filter_map(|edge| {
            let (direction, other) = if edge.source == node.id {
                ("out", edge.target.as_str())
            } else if edge.target == node.id {
                ("in", edge.source.as_str())
            } else {
                return None;
            };
            let peer = session.data.nodes.iter().find(|n| n.id == other)?;
            let (_, peer_title) = crate::model::card_head(peer);
            Some(CanvasEdgeInfo {
                role: role_key(edge_role(&session.data, edge)),
                direction: direction.to_string(),
                peer_title,
            })
        })
        .collect();
    Some(CanvasSelectionInfo {
        title,
        node_type: node.node_type.clone(),
        x: node.x,
        y: node.y,
        width,
        height,
        file: file.map(str::to_string),
        ref_status: file.map(|file| match session.view.refs.get(file) {
            Some(RefContent::Text(_) | RefContent::Image | RefContent::Table { .. }) => {
                CanvasRefStatus::Loaded
            }
            Some(RefContent::Binary) => CanvasRefStatus::Binary,
            Some(RefContent::Missing) => CanvasRefStatus::Missing,
            None => CanvasRefStatus::Loading,
        }),
        edges,
    })
}

fn role_key(role: EdgeRole) -> String {
    match role {
        EdgeRole::Association => "association",
        EdgeRole::Reference { consumed: true } => "reference-consumed",
        EdgeRole::Reference { consumed: false } => "reference-unconsumed",
        EdgeRole::Lineage => "lineage",
        EdgeRole::Flow => "flow",
    }
    .to_string()
}

/// 把当前快照作为 `canvas:changed` 广播（无画布时发关闭态）。
pub async fn emit_changed(plugin: &PluginState, ctx: &Context) {
    let payload = plugin
        .with_session(|session| CanvasChangedPayload {
            path: Some(session.file.clone()),
            dirty: session.dirty,
            selection: selection_of(session),
            stats: stats_of(&session.data),
        })
        .unwrap_or_default();
    let value = match serde_json::to_value(payload) {
        Ok(value) => value,
        Err(error) => {
            ctx.log_error(&format!("序列化画布快照失败：{error}"));
            return;
        }
    };
    if let Err(error) = ctx.event_emit(event::name::CANVAS_CHANGED, value).await {
        ctx.log_error(&format!("发布画布变更失败：{error}"));
    }
}
