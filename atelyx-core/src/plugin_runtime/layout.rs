//! 插件目录布局与发现。根 = 应用数据目录下 `plugins/`。
//!
//! ```text
//! plugins/
//!   <id>/                 插件目录：安装内容（plugin.json + 入口 + 插件自带文件）
//!     data/               宿主管理：插件私有数据（state.json 与私有文件；更新保留、卸载清除）
//!     previous/<ts>/      宿主管理：上一代内容（只留一代，供回退）
//!     enabled             宿主管理：启用标记（空文件；存在即启用）
//!   _tmp-<唯一名>/        安装临时目录（下划线开头，反域名 id 不可能与之相撞；安装结束即清）
//! ```
//! 顶层只有插件目录，无隐藏目录约定；宿主保留插件目录内的顶层名 `data` / `previous` / `enabled`
//! （安装时拒收插件自带同名内容），其余空间归插件自由使用。

use std::path::{Path, PathBuf};

use atelyx_plugin_protocol::manifest::MANIFEST_FILE;

/// 宿主在插件目录内保留的顶层名。
pub(crate) const RESERVED_TOP_LEVEL: [&str; 3] = ["data", "previous", "enabled"];
/// 安装临时目录前缀。用下划线开头：插件 id 为反域名（每段仅字母数字与 `-`），不可能以此开头。
pub(crate) const TEMP_PREFIX: &str = "_tmp-";

/// 插件目录 `<root>/<id>`。
pub(crate) fn plugin_dir(root: &Path, id: &str) -> PathBuf {
    root.join(id)
}

/// 插件私有数据目录 `<root>/<id>/data`。
pub(crate) fn data_dir(root: &Path, id: &str) -> PathBuf {
    plugin_dir(root, id).join("data")
}

/// 上一代内容根 `<root>/<id>/previous`（其下按代次分子目录，只保留一代）。
pub(crate) fn previous_dir(root: &Path, id: &str) -> PathBuf {
    plugin_dir(root, id).join("previous")
}

/// 启用标记 `<root>/<id>/enabled`。
pub(crate) fn enabled_marker(root: &Path, id: &str) -> PathBuf {
    plugin_dir(root, id).join("enabled")
}

/// 安装临时目录 `<root>/tmp-<唯一名>`。
pub(crate) fn temp_dir(root: &Path) -> PathBuf {
    root.join(format!("{TEMP_PREFIX}{}", nanoid::nanoid!()))
}

/// 插件当前是否启用（启用标记存在）。
pub(crate) fn is_enabled(root: &Path, id: &str) -> bool {
    enabled_marker(root, id).is_file()
}

/// 写 / 删启用标记。插件目录不存在即拒绝（未安装不得置启用）。
pub(crate) fn set_enabled(root: &Path, id: &str, on: bool) -> Result<(), String> {
    let marker = enabled_marker(root, id);
    if on {
        if !plugin_dir(root, id).is_dir() {
            return Err(format!("插件未安装：{id}"));
        }
        std::fs::write(&marker, b"").map_err(|e| format!("写入启用标记失败：{e}"))
    } else {
        match std::fs::remove_file(&marker) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("删除启用标记失败：{e}")),
        }
    }
}

/// 发现已安装插件：`<root>` 下根含 `plugin.json` 的顶层目录（安装临时目录跳过）。
/// 只做目录识别，manifest 合法性由上层逐个读取校验（非法项以可读错误呈现，不静默跳过）。
pub(crate) fn discover(root: &Path) -> Vec<(String, PathBuf)> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return found;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(TEMP_PREFIX) {
            continue;
        }
        let path = entry.path();
        if path.is_dir() && path.join(MANIFEST_FILE).is_file() {
            found.push((name, path));
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

/// 清掉遗留的安装临时目录（上次安装中途退出时可能残留）。
pub(crate) fn sweep_temp(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(TEMP_PREFIX) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::test_support::TempDir;

    fn seed(root: &Path, id: &str) {
        let dir = plugin_dir(root, id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(MANIFEST_FILE), "{}").unwrap();
    }

    #[test]
    fn discovery_skips_temp_and_manifestless() {
        let tmp = TempDir::new("plugins-layout");
        let root: &Path = &tmp;
        seed(root, "com.a.one");
        seed(root, "com.a.two");
        // 安装临时目录即便含 manifest 也不算插件；无 manifest 的普通目录也不算
        let temp = temp_dir(root);
        std::fs::create_dir_all(&temp).unwrap();
        std::fs::write(temp.join(MANIFEST_FILE), "{}").unwrap();
        std::fs::create_dir_all(plugin_dir(root, "com.a.none")).unwrap();
        let found: Vec<String> = discover(root).into_iter().map(|(id, _)| id).collect();
        assert_eq!(found, vec!["com.a.one", "com.a.two"]);
    }

    #[test]
    fn enabled_marker_lives_inside_plugin_dir() {
        let tmp = TempDir::new("plugins-enabled");
        let root: &Path = &tmp;
        // 未安装不得置启用
        assert!(set_enabled(root, "com.a.one", true).is_err());
        seed(root, "com.a.one");
        assert!(!is_enabled(root, "com.a.one"));
        set_enabled(root, "com.a.one", true).unwrap();
        assert!(is_enabled(root, "com.a.one"));
        assert!(enabled_marker(root, "com.a.one").is_file());
        set_enabled(root, "com.a.one", false).unwrap();
        assert!(!is_enabled(root, "com.a.one"));
        // 删除不存在的标记是空操作
        set_enabled(root, "com.a.missing", false).unwrap();
    }

    #[test]
    fn sweep_temp_removes_only_temp_dirs() {
        let tmp = TempDir::new("plugins-sweep");
        let root: &Path = &tmp;
        std::fs::create_dir_all(temp_dir(root)).unwrap();
        seed(root, "com.a.one");
        sweep_temp(root);
        assert_eq!(discover(root).len(), 1);
        assert!(plugin_dir(root, "com.a.one").exists());
    }
}
