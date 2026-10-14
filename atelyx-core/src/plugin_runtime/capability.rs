//! 能力面闸门与 `state` 自持数据实现：方法 → 权限映射（未声明一律 fail-closed 拒绝），
//! 以及自持数据的读写。其余能力面的方法分派在 `instance`，尚未实现的面（如 `clipboard`）由运行时
//! 按「已声明但未实现」返回可读失败。
//!
//! 未声明被拒的判定只认 manifest 声明（不看插件自报），故 API 不暴露插件 id 参数。

use std::path::Path;

use atelyx_plugin_protocol::manifest::Permission;
use atelyx_plugin_protocol::method;
use serde_json::Value;

/// 方法所属能力面。`None` = 运行时时序方法（握手 / 事件订阅 / 视图提交），不占权限声明。
pub(crate) fn permission_for(name: &str) -> Option<Permission> {
    use Permission::*;
    use method::{clipboard, dialog, fs, http, notification, process, shortcut, state};
    match name {
        fs::PRIVATE_DIR | fs::READ_TEXT | fs::WRITE_TEXT | fs::READ_BINARY | fs::WRITE_BINARY
        | fs::REMOVE | fs::EXISTS | fs::LIST => Some(Fs),
        process::SPAWN | process::WRITE_STDIN | process::CLOSE_STDIN | process::KILL => Some(Process),
        state::READ | state::WRITE => Some(State),
        notification::SHOW => Some(Notification),
        shortcut::REGISTER | shortcut::UNREGISTER => Some(Shortcuts),
        clipboard::WRITE_TEXT | clipboard::READ_TEXT => Some(Clipboard),
        dialog::PICK_FILE | dialog::PICK_DIRECTORY => Some(Dialog),
        http::REQUEST => Some(Http),
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
        assert_eq!(permission_for(method::http::REQUEST), Some(Permission::Http));
        // 运行时时序方法不占权限
        assert_eq!(permission_for("initialize"), None);
        assert_eq!(permission_for(method::view::SUBMIT), None);
        assert_eq!(permission_for(method::canvas::SUBMIT), None, "画布清单是界面通道，不占权限声明");
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
