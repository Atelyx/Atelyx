//! Atelyx 插件 SDK：插件侧的连接与能力面封装。
//! 插件以动态库交付：用 [`export_in_process_plugin`] 导出 C ABI 符号，由 [`in_process`] 完成握手与
//! 事件分发；[`Context`] 提供全部能力面代理调用，[`frame_builders`] 构建绘制指令，
//! [`frame_ui`] 提供 immediate-mode 控件层。协议类型统一从 `protocol` 模块取用。

pub mod connection;
pub mod context;
pub mod frame_builders;
pub mod frame_ui;
pub mod handler;
pub mod in_process;

/// 协议类型再导出：插件代码只依赖本 crate。
pub use atelyx_plugin_protocol as protocol;
/// 插件与宿主之间的 C ABI 契约（函数表 / 符号名 / 返回码）。
pub use atelyx_plugin_abi as abi;
pub use connection::{ConnectError, Connection};
pub use context::Context;
pub use handler::PluginHandler;
pub use protocol::handshake::PluginInfo;
