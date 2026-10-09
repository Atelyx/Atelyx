//! Tauri 命令模块。
//! 每个命令对应前端 invoke 调用，统一在 lib.rs 的 generate_handler! 中注册。

pub mod assembly;
pub mod entity_txn;
pub mod external_fs;
pub mod filesearch;
pub mod chat_container;
pub mod global;
pub mod global_shortcut;
pub mod host_runtime;
pub mod home;
pub mod keychain;
pub mod mobile;
pub mod open_context;
pub mod plugin;
pub mod process;
pub mod search;
pub mod table;
pub mod temp_attachment;
pub mod update;
pub mod vault;
pub mod web;
pub mod windows;

pub(crate) mod content_broadcast;
