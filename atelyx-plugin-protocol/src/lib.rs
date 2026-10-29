//! Atelyx 插件协议：宿主与插件进程之间的对外契约。
//! 握手校验 API 版本；信封为 JSON-RPC 2.0 over NDJSON；绘制指令、输入事件、能力方法与事件
//! 均为封闭枚举 + 版本号，破坏性变更必须升 [`API_VERSION`]（握手失败即响亮拒绝）。

pub mod envelope;
pub mod event;
pub mod frame;
pub mod handshake;
pub mod layout;
pub mod manifest;
pub mod method;
pub mod paint;
pub mod service;

/// API 版本。破坏性变更（方法/指令/事件语义不兼容）递增后，握手时双方不一致即失败。
/// 协议冻结为稳定契约前不递增本号（同版本发布，避免每次扩容都要求重建在装插件）。
pub const API_VERSION: u32 = 1;

/// 方法名的方向约定：`host:` 前缀归宿主保留，只有宿主→插件方向可调用；
/// 插件→宿主只可调用无前缀方法。方向错用即协议违规。
pub fn is_host_method(method: &str) -> bool {
    method.starts_with("host:")
}
