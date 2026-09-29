//! 仓库外文件命令（插件 `ctx.fs` 面）。
//!
//! 与 `read_vault_file`/`write_vault_file` 等仓库命令同构，但作用域为任意绝对路径——
//! 本面无目录授权门槛：插件与宿主同 realm、事前授权不构成可信边界，取向为
//! 「无门槛 + 调用全量审计」（审计在前端 audit 层按调用方插件记录方法与路径，详情页可见）。
//! 独立命令族：模型工具（AI 文件工具）走仓库命令、限仓库根内，结构性够不到本面
//! ——`safe_join` 与仓库命令一字未动。
//!
//! 路径入参一律绝对路径且不含 `..` 段（须规范化，创建语义不做词法消解）。
//! `external_private_dir` 按 plugin_id 定位插件私有文件目录（插件根内 `data/files`）。
//! plugin_id 由宿主侧 tracker 绑定（services/cordis/kernel.ts 的 `fs` 服务）——类型化
//! ctx.fs 面只在调用边界校验插件上下文，不把 id 交给插件。

use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
use tauri::AppHandle;

use crate::commands::plugin::resolve_plugin_dir;
use crate::commands::vault::{list_dir_entries, mime_from_ext, ListDirResult};
use crate::vault::{atomic_write, atomic_write_bytes, same_physical_file};

/// 单层列目上限（与仓库 list_dir 对齐；超上限截断并标 capped）。
const EXTERNAL_LIST_DIR_MAX_ENTRIES: usize = 200;

/// 插件私有文件目录（插件根内 `data/files`）。落在 data 目录内 = 复用宿主对插件数据的
/// 既有事务语义：更新/回退整目录搬运保留，卸载随插件目录清除。
pub(crate) fn private_files_dir_of(plugin_dir: &Path) -> PathBuf {
    plugin_dir.join("data").join("files")
}

/// 读仓库外文件全文（非 UTF-8 返回替换字符容错，与 read_vault_file 同口径）。
#[tauri::command]
pub fn external_read_file(path: String) -> Result<String, String> {
    let resolved = resolve_path(&path, false)?;
    if !resolved.is_file() {
        return Err(format!("路径不存在或不是文件：{}", path));
    }
    let bytes = fs::read(&resolved).map_err(|e| format!("读取失败：{}", e))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// 写仓库外文件（原子写；缺失的父目录自动补齐）。
#[tauri::command]
pub fn external_write_file(path: String, content: String) -> Result<(), String> {
    let resolved = resolve_path(&path, true)?;
    atomic_write(&resolved, &content)
}

/// 单层列出仓库外目录条目（目录在前、按名称升序）。不屏蔽 `.` 开头项——目标是真实
/// 目录，点文件（.gitignore 等）是正当内容，与仓库列表屏蔽模型的语义不同。
#[tauri::command]
pub fn external_list_dir(path: String) -> Result<ListDirResult, String> {
    let resolved = resolve_path(&path, false)?;
    if !resolved.is_dir() {
        return Err(format!("路径不存在或不是目录：{}", path));
    }
    list_dir_entries(&resolved, EXTERNAL_LIST_DIR_MAX_ENTRIES, false)
}

/// 创建仓库外目录（补齐父目录；目标已存在且是目录 = 成功，与 create_dir_all 语义一致）。
#[tauri::command]
pub fn external_create_folder(path: String) -> Result<(), String> {
    let resolved = resolve_path(&path, true)?;
    fs::create_dir_all(&resolved).map_err(|e| format!("创建目录失败：{}", e))
}

/// 仓库外文件同目录重命名（new_name 须为纯文件名；目标已存在拒绝，防静默覆盖）。
/// 返回实际落盘路径（绝对）。
#[tauri::command]
pub fn external_rename_file(path: String, new_name: String) -> Result<String, String> {
    let src = resolve_path(&path, false)?;
    if !src.is_file() {
        return Err(format!("源不是文件：{}", path));
    }
    if new_name.trim().is_empty() || new_name.contains(['/', '\\']) {
        return Err("新名称必须是纯文件名".to_string());
    }
    let parent = src.parent().ok_or_else(|| "非法路径".to_string())?;
    let dst_raw = parent.join(&new_name);
    let dst = resolve_path(&dst_raw.to_string_lossy(), true)?;
    // 目标已存在直接拒绝（fs::rename 在部分平台静默覆盖）；case-only 重命名豁免
    let same_file = same_physical_file(&src, &dst);
    if dst.exists() && !same_file {
        return Err(format!("目标文件已存在：{}", new_name));
    }
    fs::rename(&src, &dst).map_err(|e| e.to_string())?;
    Ok(dst.to_string_lossy().into_owned())
}

/// 把仓库外文件移动到另一目录（目标目录可尚不存在，自动创建）。返回实际落盘路径（绝对）。
#[tauri::command]
pub fn external_move_file(path: String, target_dir: String) -> Result<String, String> {
    let src = resolve_path(&path, false)?;
    if !src.is_file() {
        return Err(format!("源不是文件：{}", path));
    }
    let dir = resolve_path(&target_dir, true)?;
    let file_name = src.file_name().ok_or_else(|| "非法路径".to_string())?;
    let dst = dir.join(file_name);
    let same_file = same_physical_file(&src, &dst);
    if dst.exists() && !same_file {
        return Err(format!("目标文件已存在：{}", dst.display()));
    }
    fs::rename(&src, &dst).map_err(|e| e.to_string())?;
    Ok(dst.to_string_lossy().into_owned())
}

/// 删除仓库外文件（目录误传拒绝，与仓库 delete 同口径）。
#[tauri::command]
pub fn external_delete_file(path: String) -> Result<(), String> {
    let resolved = resolve_path(&path, false)?;
    if !resolved.exists() {
        return Err(format!("文件不存在：{}", path));
    }
    if resolved.is_dir() {
        return Err("目标是目录，仅支持删除文件".to_string());
    }
    fs::remove_file(&resolved).map_err(|e| e.to_string())
}

/// 删除目录结果（非空且未 force = needs_confirm，与仓库 delete_folder 同语义）。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalDeleteDirResult {
    pub deleted: bool,
    pub needs_confirm: bool,
    pub item_count: usize,
}

/// 删除仓库外目录（非空且未 force 时返回 needs_confirm，不删除——镜像仓库
/// delete_folder 的确认语义，前端据此提示）。
#[tauri::command]
pub fn external_delete_dir(path: String, force: bool) -> Result<ExternalDeleteDirResult, String> {
    let resolved = resolve_path(&path, false)?;
    if !resolved.is_dir() {
        return Err(format!("路径不存在或不是目录：{}", path));
    }
    if !force {
        let count = fs::read_dir(&resolved).map_err(|e| e.to_string())?.count();
        if count > 0 {
            return Ok(ExternalDeleteDirResult {
                deleted: false,
                needs_confirm: true,
                item_count: count,
            });
        }
    }
    fs::remove_dir_all(&resolved).map_err(|e| e.to_string())?;
    Ok(ExternalDeleteDirResult {
        deleted: true,
        needs_confirm: false,
        item_count: 0,
    })
}

/// 返回插件私有文件目录（不存在则创建）的绝对路径。目录位置因机器而异、清单无法声明，
/// 插件运行时经此取值，再用取到的路径走本命令族读写。
#[tauri::command]
pub fn external_private_dir(plugin_id: String, app: AppHandle) -> Result<String, String> {
    let root = private_files_dir_of(&resolve_plugin_dir(&app, &plugin_id)?);
    fs::create_dir_all(&root).map_err(|e| format!("创建插件私有目录失败：{e}"))?;
    Ok(root.to_string_lossy().into_owned())
}

/// 二进制写（base64 进，原子写）：字节原样落盘，与编码无关。
#[tauri::command]
pub fn external_write_file_base64(path: String, base64_data: String) -> Result<(), String> {
    let resolved = resolve_path(&path, true)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64_data.as_bytes())
        .map_err(|e| format!("文件数据解码失败：{e}"))?;
    atomic_write_bytes(&resolved, &bytes)
}

/// 二进制读为 dataURL（`data:<mime>;base64,...`）：mime 按扩展名推（与仓库附件读同口径），
/// 其余 application/octet-stream。
#[tauri::command]
pub fn external_read_file_data_url(path: String) -> Result<String, String> {
    let resolved = resolve_path(&path, false)?;
    if !resolved.is_file() {
        return Err(format!("路径不存在或不是文件：{}", path));
    }
    let bytes = fs::read(&resolved).map_err(|e| format!("读取失败：{e}"))?;
    let mime = mime_from_ext(&path).unwrap_or("application/octet-stream");
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:{};base64,{}", mime, b64))
}

/// 仓库外路径解析：强制绝对路径且不含 `..` 段（插件须传规范化路径——创建语义不做词法
/// 消解，`..` 输入直接拒绝）；已存在的路径 canonicalize 归一（符号链接按真实落点，
/// dunce 防 verbatim 前缀）；不存在且 create 时补齐父目录后原样返回。
fn resolve_path(path: &str, create: bool) -> Result<PathBuf, String> {
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err("仅支持绝对路径".to_string());
    }
    if p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err(format!("非法路径：含越界段 ({})", path));
    }
    if p.exists() {
        return dunce::canonicalize(p).map_err(|e| format!("路径不可达：{} ({})", path, e));
    }
    if !create {
        return Err(format!("路径不存在：{}", path));
    }
    let parent = p.parent().ok_or_else(|| format!("非法路径：{}", path))?;
    fs::create_dir_all(parent).map_err(|e| format!("创建目录失败：{}", e))?;
    Ok(p.to_path_buf())
}

#[cfg(test)]
mod external_fs_tests {
    use super::*;

    /// 唯一临时目录（并发测试互不干扰；测试结束时清理）。
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir().join(format!("atelyx-external-fs-{tag}-{nanos}"));
            fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn resolve_rejects_relative_and_dotdot() {
        let tmp = TempDir::new("rel");
        // 相对路径与 `..` 段拒绝（不创建任何目录）
        assert!(resolve_path("relative/path", false).is_err());
        let escape = tmp.path().join("sub").join("..").join("other.txt");
        assert!(resolve_path(&escape.to_string_lossy(), true).is_err());
        assert!(!tmp.path().join("sub").exists(), "拒绝的路径不得产生创建副作用");
    }

    #[test]
    fn resolve_creates_missing_parents_and_canonicalizes_existing() {
        let tmp = TempDir::new("create");
        // 不存在且 create：补齐父目录后返回
        let deep = tmp.path().join("a/b/deep.txt");
        let resolved = resolve_path(&deep.to_string_lossy(), true).unwrap();
        assert_eq!(resolved, deep);
        assert!(tmp.path().join("a").is_dir());
        // 已存在路径：返回 canonical 形态
        let f = tmp.path().join("a/b/deep.txt");
        fs::write(&f, "x").unwrap();
        let canon = resolve_path(&f.to_string_lossy(), false).unwrap();
        assert_eq!(canon, dunce::canonicalize(&f).unwrap());
        // 不存在且非 create：报不存在
        assert!(resolve_path(&tmp.path().join("gone.txt").to_string_lossy(), false).is_err());
    }

    #[test]
    fn list_dir_includes_hidden_entries() {
        let tmp = TempDir::new("list");
        fs::create_dir_all(tmp.path().join("sub")).unwrap();
        fs::write(tmp.path().join(".gitignore"), "x").unwrap();
        fs::write(tmp.path().join("a.txt"), "x").unwrap();
        let r = list_dir_entries(tmp.path(), 100, false).unwrap();
        let names: Vec<&str> = r.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&".gitignore"));
        assert!(names.contains(&"a.txt"));
        // 目录在前
        assert_eq!(r.entries[0].kind, "dir");
        assert_eq!(r.entries[0].name, "sub");
    }

    #[test]
    fn private_files_dir_is_plugin_data_subdir() {
        let tmp = TempDir::new("priv");
        assert_eq!(
            private_files_dir_of(tmp.path()),
            tmp.path().join("data").join("files")
        );
    }

    #[test]
    fn write_bytes_atomic_creates_file_with_content() {
        // atomic_write_bytes 与文本 atomic_write 同一套 tmp → rename 语义（幂等覆盖）
        let tmp = TempDir::new("bytes");
        let target = tmp.path().join("nested").join("img.bin");
        atomic_write_bytes(&target, &[0, 159, 146, 255]).unwrap();
        assert_eq!(fs::read(&target).unwrap(), vec![0, 159, 146, 255]);
        atomic_write_bytes(&target, &[1]).unwrap();
        assert_eq!(fs::read(&target).unwrap(), vec![1]);
        // 同目录不留 .tmp 残留
        let leftovers: Vec<_> = fs::read_dir(target.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, vec!["img.bin".to_string()]);
    }
}
