//! `initialize` 握手：插件启动后第一个请求，校验 API 版本；失败即断链，不残留半活连接。

use serde::{Deserialize, Serialize};

use crate::frame_region::FrameRegionInfo;

pub const INITIALIZE: &str = "initialize";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub api_version: u32,
    pub plugin: PluginInfo,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub api_version: u32,
    pub host: HostInfo,
    /// 整面帧共享内存区域：插件按它映射，此后每次提交只走这条通路。
    pub frame_region: FrameRegionInfo,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostInfo {
    pub name: String,
    pub version: String,
}

/// 版本兼容判定：版本必须相等。语义级破坏性变更都伴随版本递增，宽容 major-only 匹配
/// 会让旧插件带着未知语义跑起来——握手就要响亮失败。
pub fn compatible(plugin_version: u32, host_version: u32) -> bool {
    plugin_version == host_version
}
