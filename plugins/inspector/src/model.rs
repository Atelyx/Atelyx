//! 属性面板模型：当前文档的属性区、标签词表与画布快照；属性编辑语义复用共用库。

use atelyx_format::frontmatter::{self, Yaml};
use atelyx_plugin_props::{PropsHost, PropsState};
use atelyx_plugin_protocol::method::CanvasSnapshotResult;

/// 面板视图贡献 id（也是整面帧的 surfaceId）。
pub const PANEL: &str = "inspector";

/// 当前文档的属性区（宿主已判定可解析性与可写性）。
#[derive(Clone, Default)]
pub struct DocProps {
    /// 含首尾 `---` 的块原文；无属性区为空串。
    pub block: String,
    /// 文档是否有属性区（含未闭合的起始行）。
    pub present: bool,
    pub parsed: bool,
    pub editable: bool,
}

/// 面板状态：属性草稿 + 最近的文档 / 画布数据。
#[derive(Default)]
pub struct InspectorModel {
    pub props: PropsState,
    pub doc: DocProps,
    pub open: bool,
    pub tags: Vec<String>,
    pub canvas: Option<CanvasSnapshotResult>,
    /// 画布是否在前台展示（后台标签页不算）；为真时属性面显示画布属性。
    pub canvas_visible: bool,
    /// 待提交的属性块（渲染期提交，落盘在渲染之后统一发起）。
    pub pending_write: Option<String>,
    /// 最近一次指针位置（类型菜单浮层锚点）。
    pub pointer: (f32, f32),
}

impl InspectorModel {
    /// 用宿主回执刷新属性区；键值对由块原文解析，避免与正文出现两份真源。
    pub fn set_doc(&mut self, doc: DocProps) {
        self.props.data = data_of(&doc);
        self.doc = doc;
        self.open = true;
    }

    pub fn clear_doc(&mut self) {
        self.doc = DocProps::default();
        self.props = PropsState::default();
        self.open = false;
        self.canvas = None;
    }

    /// 画布快照只在画布处于前台时成立（`path` 为空 = 画布不在前台）。
    pub fn set_canvas(&mut self, snapshot: CanvasSnapshotResult) {
        self.canvas = if snapshot.path.is_some() { Some(snapshot) } else { None };
    }
}

/// 无属性区 = 面板整块不显示；有块但解析失败 = 错误行；其余按键值对渲染。
fn data_of(doc: &DocProps) -> Option<(Vec<(String, Yaml)>, bool)> {
    if !doc.present {
        return None;
    }
    if !doc.parsed {
        return Some((Vec::new(), false));
    }
    let parsed = frontmatter::parse(&doc.block);
    let pairs = parsed.data.as_map().map(<[(String, Yaml)]>::to_vec).unwrap_or_default();
    Some((pairs, parsed.ok))
}

impl PropsHost for InspectorModel {
    fn props(&self) -> &PropsState {
        &self.props
    }

    fn props_mut(&mut self) -> &mut PropsState {
        &mut self.props
    }

    fn editable(&self) -> bool {
        self.doc.editable && self.canvas.is_none()
    }

    fn tags(&self) -> &[String] {
        &self.tags
    }

    /// 属性面板只提交属性块，正文由宿主拼接并走自己的保存链。
    fn write_pairs(&mut self, pairs: Vec<(String, Yaml)>) -> Option<String> {
        self.pending_write = Some(if pairs.is_empty() {
            String::new()
        } else {
            frontmatter::stringify(&Yaml::Map(pairs), "")
        });
        None
    }

    fn refresh(&mut self) {
        self.props.data = data_of(&self.doc);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(block: &str, present: bool, parsed: bool) -> DocProps {
        DocProps {
            block: block.into(),
            present,
            parsed,
            editable: true,
        }
    }

    #[test]
    fn absent_frontmatter_hides_panel() {
        let mut model = InspectorModel::default();
        model.set_doc(doc("", false, false));
        assert!(model.props.data.is_none());
    }

    #[test]
    fn unparsable_block_locks_editing() {
        let mut model = InspectorModel::default();
        model.set_doc(doc("---\n: :\n", true, false));
        assert_eq!(model.props.data, Some((Vec::new(), false)));
    }

    #[test]
    fn block_pairs_parse_in_order() {
        let mut model = InspectorModel::default();
        model.set_doc(doc("---\ntags:\n  - a\ntitle: x\n---\n", true, true));
        let (pairs, ok) = model.props.data.clone().unwrap();
        assert!(ok);
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].0, "tags");
        assert_eq!(pairs[1].0, "title");
    }

    #[test]
    fn write_pairs_emits_block_and_keeps_body_out() {
        let mut model = InspectorModel::default();
        model.set_doc(doc("---\ntitle: x\n---\n", true, true));
        model.write_pairs(vec![("title".into(), Yaml::Str("hello".into()))]);
        let block = model.pending_write.clone().unwrap();
        assert!(block.starts_with("---\n"));
        assert!(block.contains("title: hello"));
        assert!(!block.contains("body"));
    }

    #[test]
    fn empty_pairs_clear_the_block() {
        let mut model = InspectorModel::default();
        model.set_doc(doc("---\ntitle: x\n---\n", true, true));
        model.write_pairs(Vec::new());
        assert_eq!(model.pending_write.as_deref(), Some(""));
    }

    #[test]
    fn canvas_snapshot_without_path_is_ignored() {
        let mut model = InspectorModel::default();
        model.set_canvas(CanvasSnapshotResult::default());
        assert!(model.canvas.is_none());
    }
}
