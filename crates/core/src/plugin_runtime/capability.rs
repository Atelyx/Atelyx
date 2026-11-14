//! 能力面的归类与 `state` 自持数据实现：方法 → 权限键映射（公示与审计用），以及自持数据的读写。
//! 其余能力面的方法分派在 `instance`。
//!
//! 调用本身不设闸门（信任模型见协议 `PathParams` 的说明：插件本就是任意权限的用户进程，
//! 声明式权限是透明度机制而非沙箱边界）；映射只回答「这个方法属于哪个能力面」，供审计记录
//! 据此标出「声明过」与「实际用到但未声明」。

use std::path::Path;

use atelyx_plugin_protocol::manifest::PermissionKey;
use atelyx_plugin_protocol::method;
use serde_json::Value;

/// 方法所属能力面。`None` = 不占权限声明的方法：运行时时序方法（握手 / 事件订阅 / 视图提交 /
/// 贡献声明）只编排插件自己的界面与声明，不触碰宿主资源；`markdown.parse` 是纯文本
/// 变换；能力总线自身的入口（`service.*`）也不属任何能力类——被调用的方法按其自身归类审计。
/// OS 资源消耗型方法（起进程 / 开窗等）一律占权限。
pub(crate) fn permission_for(name: &str) -> Option<PermissionKey> {
    method::method_spec(name).and_then(|spec| spec.permission)
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
            if crate::config::backup(&path, "plugin-state").is_none() {
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
    use atelyx_plugin_protocol::service;
    use crate::vault::test_support::TempDir;

    #[test]
    fn permission_map_covers_capability_faces() {
        use PermissionKey::*;
        for (name, expected) in [
            (method::fs::READ_TEXT, Files),
            (method::vault::WRITE_TEXT, Files),
            (method::table::SNAPSHOT, Files),
            (method::editor::SELECTION, Files),
            (method::history::ACTIVITY, Files),
            (method::process::SPAWN, Process),
            (method::state::WRITE, State),
            (method::notification::SHOW, Notification),
            (method::shortcut::REGISTER, Shortcut),
            (method::clipboard::READ_IMAGE, Clipboard),
            (method::dialog::PICK_FILE, Dialog),
            (method::http::REQUEST, Network),
            (method::collab::SEND, Collab),
            (method::window::OPEN, Window),
            (method::tray::SET_ITEMS, Tray),
            (method::ai::CHAT_SEND, Ai),
            (method::ai::TOOL_RESULT, Ai),
        ] {
            assert_eq!(permission_for(name), Some(expected), "{name}");
        }
        // 运行时时序方法不占权限
        assert_eq!(permission_for("initialize"), None);
        assert_eq!(permission_for(method::view::REGISTER), None);
        assert_eq!(permission_for(method::frame::REQUEST_REDRAW), None, "重绘请求是界面通道，不占权限声明");
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


    /// 权限归类与协议方法表逐条一致（含不在目录里的宿主自持方法）。
    #[test]
    fn permission_lookup_covers_every_tabled_method() {
        for spec in method::method_specs() {
            assert_eq!(permission_for(spec.name), spec.permission, "{}", spec.name);
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
