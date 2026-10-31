//! 能力面的归类与 `state` 自持数据实现：方法 → 权限映射（公示与审计用），以及自持数据的读写。
//! 其余能力面的方法分派在 `instance`。
//!
//! 调用本身不设闸门（信任模型见协议 `PathParams` 的说明：插件本就是任意权限的用户进程，
//! 声明式权限是透明度机制而非沙箱边界）；映射只回答「这个方法属于哪个能力面」，供审计记录
//! 据此标出「声明过」与「实际用到但未声明」。

use std::path::Path;

use atelyx_plugin_protocol::manifest::Permission;
use atelyx_plugin_protocol::method;
use atelyx_plugin_protocol::service;
use serde_json::Value;

/// 方法所属能力面。`None` = 不占权限声明的方法：运行时时序方法（握手 / 事件订阅 / 视图提交 /
/// 贡献声明 / 接管请求）只编排插件自己的界面与声明，不触碰宿主资源；`markdown.parse` 是纯文本
/// 变换；能力总线自身的入口（`service.*`）也不属任何能力类——被调用的方法按其自身归类审计。
/// OS 资源消耗型方法（起进程 / 开窗等）一律占权限。
pub(crate) fn permission_for(name: &str) -> Option<Permission> {
    use Permission::*;
    use method::{
        ai, clipboard, collab, dialog, editor, fs, history, http, notification, process, shortcut,
        state, table, tray, vault, window,
    };
    match name {
        fs::PRIVATE_DIR | fs::READ_TEXT | fs::WRITE_TEXT | fs::READ_BINARY | fs::WRITE_BINARY
        | fs::REMOVE | fs::EXISTS | fs::LIST => Some(Fs),
        vault::READ_TEXT | vault::WRITE_TEXT | vault::READ_BINARY | vault::WRITE_BINARY
        | vault::IMPORT_FILE | vault::IMAGE_INFO | vault::LIST | vault::EXISTS | vault::REMOVE
        | vault::RENAME => Some(Vault),
        table::SNAPSHOT | table::UPDATE_CELL | table::ADD_ROW | table::REMOVE_ROW
        | table::SELECT_ROW | table::SET_CELL_IMAGE => Some(Table),
        process::SPAWN | process::WRITE_STDIN | process::CLOSE_STDIN | process::KILL => Some(Process),
        state::READ | state::WRITE => Some(State),
        notification::SHOW => Some(Notification),
        tray::SET_ITEMS => Some(Tray),
        shortcut::REGISTER | shortcut::UNREGISTER | shortcut::QUERY => Some(Shortcuts),
        clipboard::WRITE_TEXT | clipboard::READ_TEXT | clipboard::READ_IMAGE
        | clipboard::WRITE_IMAGE => Some(Clipboard),
        dialog::PICK_FILE | dialog::PICK_DIRECTORY => Some(Dialog),
        http::REQUEST => Some(Http),
        collab::PEERS | collab::SEND | collab::META_GET | collab::META_PATCH => Some(Collab),
        // 读写当前笔记的选区即读写用户正文，按仓库内容权限归类
        editor::SELECTION | editor::REPLACE_SELECTION => Some(Vault),
        // 活动统计取自当前仓库内容，同样按仓库内容权限归类
        history::ACTIVITY | history::DAY | history::OPEN => Some(Vault),
        window::OPEN | window::CLOSE | window::UPDATE | window::SHOW | window::BEGIN_DRAG
        | window::MINIMIZE | window::TOGGLE_MAXIMIZE => Some(Window),
        ai::CHAT_SEND | ai::CHAT_STOP | ai::CHAT_HISTORY | ai::CHAT_RENAME | ai::CHAT_DELETE
        | ai::CHAT_COMPACT | ai::MODELS | ai::AGENTS | ai::REGISTER_TOOL | ai::UNREGISTER_TOOL
        | ai::TOOL_RESULT => Some(Ai),
        // 解析只读插件递来的文本，不触碰宿主资源；外部提供者顶替后同理
        method::markdown::PARSE => None,
        // 能力总线入口：实际权限由目标方法自己承担
        service::method::REGISTER
        | service::method::UNREGISTER
        | service::method::LIST
        | service::method::CALL
        | service::method::RESULT => None,
        _ => None,
    }
}

/// 读插件自持数据：文件不存在返回 `null`；解析失败先备份原文再按 `null`（绝不静默清空用户数据）。
pub(crate) fn read_state(dir: &Path) -> Result<Value, String> {
    let path = dir.join("state.json");
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Value::Null),
        Err(e) => return Err(format!("读取插件数据失败：{e}")),
    };
    match serde_json::from_str::<Value>(&raw) {
        Ok(value) => Ok(value),
        Err(e) => {
            // 备份失败即报错：留不下原文还按空数据继续，后续写盘会覆盖掉它
            if crate::vault::backup_corrupt_config(&path, "plugin-state").is_none() {
                return Err(format!("插件数据损坏且原文备份失败，已中止读取以免覆盖原文：{e}"));
            }
            Ok(Value::Null)
        }
    }
}

/// 写插件自持数据（原子覆盖整文件；父目录由 `atomic_write` 建立）。
pub(crate) fn write_state(dir: &Path, json: &Value) -> Result<(), String> {
    let text =
        serde_json::to_string_pretty(json).map_err(|e| format!("序列化插件数据失败：{e}"))?;
    crate::vault::atomic_write(&dir.join("state.json"), &text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::test_support::TempDir;

    #[test]
    fn permission_map_covers_capability_faces() {
        assert_eq!(permission_for(method::fs::READ_TEXT), Some(Permission::Fs));
        assert_eq!(permission_for(method::process::SPAWN), Some(Permission::Process));
        assert_eq!(permission_for(method::state::WRITE), Some(Permission::State));
        assert_eq!(permission_for(method::notification::SHOW), Some(Permission::Notification));
        assert_eq!(permission_for(method::shortcut::REGISTER), Some(Permission::Shortcuts));
        assert_eq!(permission_for(method::clipboard::READ_TEXT), Some(Permission::Clipboard));
        assert_eq!(permission_for(method::clipboard::READ_IMAGE), Some(Permission::Clipboard));
        assert_eq!(permission_for(method::vault::WRITE_TEXT), Some(Permission::Vault));
        assert_eq!(permission_for(method::vault::READ_BINARY), Some(Permission::Vault));
        assert_eq!(permission_for(method::vault::WRITE_BINARY), Some(Permission::Vault));
        assert_eq!(permission_for(method::vault::REMOVE), Some(Permission::Vault));
        assert_eq!(permission_for(method::vault::RENAME), Some(Permission::Vault));
        assert_eq!(permission_for(method::clipboard::WRITE_IMAGE), Some(Permission::Clipboard));
        assert_eq!(permission_for(method::ai::MODELS), Some(Permission::Ai));
        assert_eq!(permission_for(method::ai::AGENTS), Some(Permission::Ai));
        assert_eq!(permission_for(method::table::SNAPSHOT), Some(Permission::Table));
        assert_eq!(permission_for(method::table::UPDATE_CELL), Some(Permission::Table));
        assert_eq!(permission_for(method::table::ADD_ROW), Some(Permission::Table));
        assert_eq!(permission_for(method::table::REMOVE_ROW), Some(Permission::Table));
        assert_eq!(permission_for(method::table::SELECT_ROW), Some(Permission::Table));
        assert_eq!(permission_for(method::http::REQUEST), Some(Permission::Http));
        assert_eq!(permission_for(method::ai::CHAT_SEND), Some(Permission::Ai));
        assert_eq!(permission_for(method::ai::CHAT_STOP), Some(Permission::Ai));
        assert_eq!(permission_for(method::ai::CHAT_HISTORY), Some(Permission::Ai));
        assert_eq!(permission_for(method::ai::REGISTER_TOOL), Some(Permission::Ai));
        assert_eq!(permission_for(method::ai::TOOL_RESULT), Some(Permission::Ai));
        // 运行时时序方法不占权限
        assert_eq!(permission_for("initialize"), None);
        assert_eq!(permission_for(method::view::REGISTER), None);
        assert_eq!(permission_for(method::frame::SUBMIT_SHARED), None, "整面帧是界面通道，不占权限声明");
        assert_eq!(permission_for(method::frame::REGION), None, "帧区域扩容是界面通道，不占权限声明");
        assert_eq!(permission_for(method::window::UPDATE), Some(Permission::Window));
        assert_eq!(permission_for(method::window::SHOW), Some(Permission::Window));
        assert_eq!(permission_for(method::shortcut::QUERY), Some(Permission::Shortcuts));
        assert_eq!(permission_for(method::table::SET_CELL_IMAGE), Some(Permission::Table));
        assert_eq!(permission_for(method::ai::CHAT_RENAME), Some(Permission::Ai));
        assert_eq!(permission_for(method::ai::CHAT_DELETE), Some(Permission::Ai));
        assert_eq!(permission_for(method::ai::CHAT_COMPACT), Some(Permission::Ai));
        assert_eq!(permission_for(method::vault::IMAGE_INFO), Some(Permission::Vault));
        assert_eq!(permission_for(method::tray::SET_ITEMS), Some(Permission::Tray));
        assert_eq!(permission_for(method::collab::PEERS), Some(Permission::Collab));
        assert_eq!(permission_for(method::collab::SEND), Some(Permission::Collab));
        assert_eq!(permission_for(method::collab::META_GET), Some(Permission::Collab));
        assert_eq!(permission_for(method::collab::META_PATCH), Some(Permission::Collab));
        assert_eq!(permission_for(method::editor::SELECTION), Some(Permission::Vault));
        assert_eq!(permission_for(method::editor::REPLACE_SELECTION), Some(Permission::Vault));
        assert_eq!(permission_for(method::history::ACTIVITY), Some(Permission::Vault));
        assert_eq!(permission_for(method::history::DAY), Some(Permission::Vault));
        assert_eq!(permission_for(method::history::OPEN), Some(Permission::Vault));
        // 纯文本变换与能力总线入口不占权限类（调用照常执行，审计按各自归类记录）
        assert_eq!(permission_for(method::markdown::PARSE), None);
        for name in [
            service::method::REGISTER,
            service::method::UNREGISTER,
            service::method::LIST,
            service::method::CALL,
            service::method::RESULT,
        ] {
            assert_eq!(permission_for(name), None, "{name} 不属任何能力类");
        }
    }

    #[test]
    fn state_roundtrip_and_missing_is_null() {
        let tmp = TempDir::new("plugin-state");
        let root: &Path = &tmp;
        assert_eq!(read_state(root).unwrap(), Value::Null);
        let value = serde_json::json!({"n": 3});
        write_state(root, &value).unwrap();
        assert_eq!(read_state(root).unwrap(), value);
    }

    #[test]
    fn corrupt_state_is_backed_up_and_reads_null() {
        let tmp = TempDir::new("plugin-state-corrupt");
        let root: &Path = &tmp;
        let path = root.join("state.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(read_state(root).unwrap(), Value::Null);
        // 原文已改名留档（原路径不再存在），内容与损坏原文一致
        assert!(!path.exists());
        let backup = std::fs::read_dir(root)
            .unwrap()
            .flatten()
            .find(|e| e.file_name().to_string_lossy().contains("corrupt"))
            .expect("应留档损坏原文");
        assert_eq!(std::fs::read_to_string(backup.path()).unwrap(), "{ not json");
    }
}
