//! 插件默认布局规格：插件只描述面板结构与视图 kind，面板 / 标签 id 由宿主实例化时生成。
//! 形状上限与校验由宿主在应用前执行（超限即拒收，不落进用户布局）。

use serde::{Deserialize, Serialize};

/// 默认布局名的字节上限（多字节字符按字节计）。
pub const MAX_NAME_BYTES: usize = 64;
/// 规格树的深度上限。
pub const MAX_DEPTH: usize = 16;
/// 规格树的面板数上限。
pub const MAX_PANELS: usize = 32;
/// 单个面板的视图数上限。
pub const MAX_VIEWS_PER_PANEL: usize = 16;

/// 插件默认布局规格节点（`layout.declareDefaultLayout` 载荷）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum LayoutSpecNode {
    /// 分割节点：方向 + 子树 + 占比。
    Split {
        direction: crate::view::Axis,
        children: Vec<LayoutSpecNode>,
        sizes: Vec<f64>,
    },
    /// 面板节点：按顺序放置的视图 kind（首个为激活标签）。
    Panel { views: Vec<String> },
}
