//! 面板状态：宿主回执的版本分组副本 + 加载 / 失败态；插件不读侧文件、不落盘。

use atelyx_plugin_protocol::method::{HistoryFileGroup, HistoryGroupsResult, Radii};

/// 面板可见状态（每帧克隆一份给渲染，避免持有锁跨 await）。
#[derive(Clone)]
pub struct Model {
    /// `None` = 首份回执未到（聚合在途）：面板画加载态。
    pub ready: Option<bool>,
    pub groups: Vec<HistoryFileGroup>,
    /// 分组世代：每次整份替换 +1，卡片命中区 id 带着它，过期帧上的点击据此作废。
    pub gen: u64,
    /// 取数失败的原因（保留上次成功的分组，不清空已看着的内容）。
    pub error: Option<String>,
    /// 圆角档取自宿主主题（卡片与空态方形底共用）。
    pub radii: Radii,
}

impl Model {
    pub fn new() -> Self {
        Self { ready: None, groups: Vec::new(), gen: 0, error: None, radii: Radii::default() }
    }

    pub fn set_radii(&mut self, radii: Radii) {
        self.radii = radii;
    }

    /// 整份分组替换（宿主每次给的就是当前容器的完整聚合）。
    pub fn apply(&mut self, result: HistoryGroupsResult) {
        self.ready = Some(result.ready);
        self.groups = result.groups;
        self.gen = self.gen.wrapping_add(1);
        self.error = None;
    }

    pub fn fail(&mut self, error: String) {
        self.error = Some(error);
    }
}

impl Default for Model {
    fn default() -> Self {
        Self::new()
    }
}
