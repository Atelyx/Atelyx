//! Atelyx 插件 SDK：插件侧的连接与能力面封装。
//! 插件作者引此包即可：[`run_stdio`] 完成握手与事件分发，[`Context`] 提供全部能力面代理调用，
//! [`frame_builders`] 构建绘制指令，[`frame_ui`] 提供 immediate-mode 控件层。协议类型统一从
//! `protocol` 模块取用。

pub mod connection;
pub mod context;
pub mod frame_builders;
pub mod frame_ui;
pub mod handler;

/// 协议类型再导出：插件代码只依赖本 crate。
pub use atelyx_plugin_protocol as protocol;
pub use connection::{run_stdio, ConnectError, Connection};
pub use context::Context;
pub use handler::PluginHandler;
pub use protocol::handshake::PluginInfo;
