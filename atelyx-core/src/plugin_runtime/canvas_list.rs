//! 画布清单存档：整表提交与按 id 增量维护两种写入，渲染侧按序取指令。
//! 清单不可变——每次写入生成一份新的，渲染侧凭 `Arc` 标识判定是否需要重画。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use atelyx_plugin_protocol::canvas::{CanvasUpsert, DrawCommand};
use atelyx_plugin_protocol::view::{NodeBody, ViewNode};

/// 一条清单条目。`slot` 是插件自定的稳定 id（整表提交的条目没有，只能按序定位）；
/// 指令共享持有——增量写入只克隆指针，不重复拷贝文本与路径。
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    slot: Option<String>,
    command: Arc<DrawCommand>,
}

/// 某画布节点的指令清单：条目按绘制顺序排列，后出现的画在上层。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CanvasList {
    entries: Vec<Entry>,
}

impl CanvasList {
    /// 整表重建：`canvas.submit` 的语义，此前经增量建立的所有 id 槽一并丢弃。
    pub fn from_commands(commands: Vec<DrawCommand>) -> Self {
        Self {
            entries: commands
                .into_iter()
                .map(|command| Entry { slot: None, command: Arc::new(command) })
                .collect(),
        }
    }

    /// 按 id 增量维护：`upsert` 命中已有 id 即原位替换、未命中即追加到末尾，最后按 `remove` 删除。
    /// `remove` 里不存在的 id 视为已删除（幂等）；同一 id 既 upsert 又 remove、或同一请求内重复出现即拒收。
    pub fn patched(&self, upsert: &[CanvasUpsert], remove: &[String]) -> Result<Self, String> {
        for id in remove {
            if upsert.iter().any(|item| &item.id == id) {
                return Err(format!("同一 id 不能既写入又删除：{id}"));
            }
        }
        for (index, item) in upsert.iter().enumerate() {
            if upsert[index + 1..].iter().any(|other| other.id == item.id) {
                return Err(format!("同一请求内重复写入同一 id：{}", item.id));
            }
        }
        for (index, id) in remove.iter().enumerate() {
            if remove[index + 1..].contains(id) {
                return Err(format!("同一请求内重复删除同一 id：{id}"));
            }
        }
        let mut entries = self.entries.clone();
        // 一次建位置表，避免逐条 upsert 都全表扫
        let positions: HashMap<&str, usize> = entries
            .iter()
            .enumerate()
            .filter_map(|(position, entry)| entry.slot.as_deref().map(|slot| (slot, position)))
            .collect();
        let mut replaced = Vec::new();
        let mut appended = Vec::new();
        for item in upsert {
            match positions.get(item.id.as_str()) {
                Some(position) => replaced.push((*position, item)),
                None => appended.push(item),
            }
        }
        drop(positions);
        for (position, item) in replaced {
            entries[position].command = Arc::new(item.command.clone());
        }
        for item in appended {
            entries.push(Entry {
                slot: Some(item.id.clone()),
                command: Arc::new(item.command.clone()),
            });
        }
        if !remove.is_empty() {
            let dropped: HashSet<&str> = remove.iter().map(String::as_str).collect();
            entries.retain(|entry| entry.slot.as_deref().is_none_or(|slot| !dropped.contains(slot)));
        }
        Ok(Self { entries })
    }

    /// 按绘制顺序取指令。
    pub fn commands(&self) -> impl Iterator<Item = &DrawCommand> {
        self.entries.iter().map(|entry| entry.command.as_ref())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// 树里声明了帧率的画布节点（节点 id, 帧率）。宿主据此只给要动画的画布挂节拍。
pub fn animated_canvases(tree: &ViewNode) -> Vec<(String, u16)> {
    fn walk(node: &ViewNode, out: &mut Vec<(String, u16)>) {
        if let NodeBody::Canvas { fps: Some(fps), .. } = &node.body {
            out.push((node.id.clone(), *fps));
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(tree, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(y: f32) -> DrawCommand {
        DrawCommand::Line {
            x1: 0.0,
            y1: y,
            x2: 10.0,
            y2: y,
            stroke: atelyx_plugin_protocol::canvas::CanvasColor::Rgba { rgba: [0, 0, 0, 255] },
            width: 1.0,
        }
    }

    fn item(id: &str, y: f32) -> CanvasUpsert {
        CanvasUpsert { id: id.into(), command: line(y) }
    }

    #[test]
    fn submit_rebuilds_whole_list_and_drops_slots() {
        let submitted = CanvasList::from_commands(vec![line(1.0), line(2.0)]);
        assert_eq!(submitted.len(), 2);

        let patched = submitted.patched(&[item("a", 5.0)], &[]).unwrap();
        assert_eq!(patched.len(), 3, "增量未命中的 id 追加到末尾");

        let rebuilt = CanvasList::from_commands(vec![line(9.0)]);
        assert_eq!(rebuilt.len(), 1, "整表重建只留本次提交的条目");
        let after = rebuilt.patched(&[], &["a".into()]).unwrap();
        assert_eq!(after.len(), 1, "重建后旧的 id 槽已不存在，删除幂等");
    }

    #[test]
    fn patch_updates_in_place_and_keeps_order() {
        let list = CanvasList::from_commands(vec![]).patched(&[item("a", 1.0), item("b", 2.0)], &[]).unwrap();
        let updated = list.patched(&[item("a", 7.0)], &[]).unwrap();
        let commands: Vec<&DrawCommand> = updated.commands().collect();
        assert_eq!(commands, vec![&line(7.0), &line(2.0)], "原位替换不打乱绘制顺序");

        let removed = updated.patched(&[], &["a".into()]).unwrap();
        assert_eq!(removed.commands().collect::<Vec<_>>(), vec![&line(2.0)]);
        assert_eq!(updated.len(), 2, "增量写入不改原清单");
    }

    #[test]
    fn patch_rejects_conflicting_ids() {
        let list = CanvasList::from_commands(vec![]);
        assert!(list.patched(&[item("a", 1.0)], &["a".into()]).is_err());
        assert!(list.patched(&[item("a", 1.0), item("a", 2.0)], &[]).is_err());
        assert!(list.patched(&[], &["x".into(), "x".into()]).is_err());
        assert!(list.patched(&[], &["missing".into()]).is_ok(), "删除不存在的 id 幂等");
    }

    #[test]
    fn animated_canvases_reports_declared_fps_only() {
        let tree = ViewNode {
            id: "root".into(),
            body: NodeBody::Column { gap: None, padding: None, cross_align: None, align: None, size: None },
            interaction: None,
            grow: None,
            children: vec![
                ViewNode {
                    id: "a".into(),
                    body: NodeBody::Canvas { mouse_events: vec![], height: None, fill: None, fps: Some(30) },
                    interaction: None,
                    grow: None,
                    children: vec![],
                    events: vec![],
                },
                ViewNode {
                    id: "b".into(),
                    body: NodeBody::Canvas { mouse_events: vec![], height: None, fill: None, fps: None },
                    interaction: None,
                    grow: None,
                    children: vec![],
                    events: vec![],
                },
            ],
            events: vec![],
        };
        assert_eq!(animated_canvases(&tree), vec![("a".to_string(), 30)]);
    }
}
