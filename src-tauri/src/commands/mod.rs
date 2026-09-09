//! Tauri 命令模块。
//! 每个命令对应前端 invoke 调用，统一在 lib.rs 的 generate_handler! 中注册。

pub mod filesearch;
pub mod global;
pub mod home;
pub mod keychain;
pub mod plugin;
pub mod search;
pub mod table;
pub mod vault;
pub mod web;
pub mod windows;
