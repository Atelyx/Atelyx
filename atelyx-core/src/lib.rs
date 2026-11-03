//! Atelyx UI 无关核心层：壳之下的存储、会话容器、补丁合并、AI 客户端、协作帧泵与插件进程托管。
//!
//! 依赖约束：禁止依赖 iced 等任何 GUI / 壳层 crate；仅允许平台无关的通用依赖
//! （序列化、异步运行时、时间与标识、正则 / glob、网络客户端）。

pub mod ai;
pub mod collab;
pub mod credentials;
pub mod canvas_collab;
pub mod ephemeral;
pub mod filesearch;
pub mod global_config;
pub mod history;
pub mod history_diff;
pub mod image_thumb;
pub mod layout;
pub mod merge3;
pub mod net_guard;
pub mod note_collab;
pub mod plugin_process;
pub mod plugin_runtime;
pub mod remote_image;
pub mod rebuild_links;
pub mod search;
pub mod space_client;
pub mod space_frame;
/// 表格纯逻辑（模型与运算在格式库，宿主经此路径沿用）。
pub use atelyx_format::table::ops as table_ops;
pub mod updater;
pub mod vault;
pub mod vault_refs;
pub mod web;
pub mod write_queue;

/// 格式语义库重导出：Markdown 与 frontmatter 两个模块，宿主与插件共用同一份解析与序列化语义。
pub use atelyx_format::{frontmatter, markdown};
