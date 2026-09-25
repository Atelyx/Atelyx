//! 仓库外目录文件命令（插件 `ctx.fs` 面）。
//!
//! 与 `read_vault_file`/`write_vault_file` 等仓库命令同构，但安全根 = 该插件的
//! 「用户批准授权目录 ∪ 插件私有文件目录」（`fs_roots_for`：授权目录来自
//! `commands/plugin.rs::approved_dirs_for`；私有目录 = 插件根内 `data/files`，
//! 无需声明与批准）。独立命令族：模型工具（AI 文件工具）走仓库命令，结构性够不到本面
//! ——`safe_join` 与仓库命令一字未动。
//!
//! 授权目录每次调用实时查表（撤销立即失效，无缓存窗口）；私有目录按清单 id 扫描实时定位
//! 插件目录随之确定（插件不存在 = Err，与授权查表同口径）。路径入参一律绝对路径，
//! `resolve_under_roots` 把请求解析到某安全根之下（镜像 `safe_join` 的 canonicalize +
//! starts_with 语义：符号链接越出安全根即拒）。`plugin_id` 由宿主侧 tracker 绑定
//! （services/cordis/kernel.ts 的 `fs` 服务）——类型化 ctx.fs 面不暴露 id 参数，插件经它
//! 无法指定他人命名空间；原始命令逃生舱（ctx.native.invoke）为全量放行面、不在此约束内
//! （与 state/storage 同一暴露，完全信任模型下安全根不构成防插件边界）。

use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
use tauri::AppHandle;

use crate::commands::plugin::{approved_dirs_for, resolve_plugin_dir};
use crate::commands::vault::{list_dir_entries, mime_from_ext, ListDirResult};
use crate::vault::{atomic_write, atomic_write_bytes, same_physical_file};

/// 单层列目上限（与仓库 list_dir 对齐；超上限截断并标 capped）。
const EXTERNAL_LIST_DIR_MAX_ENTRIES: usize = 200;

/// 插件私有文件目录（插件根内 `data/files`）：ctx.fs 的隐式安全根之一，无需声明与批准。
/// 落在 data 目录内 = 复用宿主对插件数据的既有事务语义：更新/回退整目录搬运保留，
/// 卸载随插件目录清除。
pub(crate) fn private_files_dir_of(plugin_dir: &Path) -> PathBuf {
    plugin_dir.join("data").join("files")
}

/// ctx.fs 的安全根集合 = 用户批准的授权目录 ∪ 插件私有文件目录。授权表实时查
/// （撤销立即失效），私有目录按清单 id 扫描实时定位插件目录随之确定
/// （插件不存在 = Err，与授权查表同口径）。
fn fs_roots_for(app: &AppHandle, id: &str) -> Result<Vec<PathBuf>, String> {
    let mut roots = approved_dirs_for(app, id)?;
    roots.push(private_files_dir_of(&resolve_plugin_dir(app, id)?));
    Ok(roots)
}

/// 读仓库外文件全文（非 UTF-8 返回替换字符容错，与 read_vault_file 同口径）。
#[tauri::command]
pub fn external_read_file(plugin_id: String, path: String, app: AppHandle) -> Result<String, String> {
    let roots = fs_roots_for(&app, &plugin_id)?;
    let resolved = resolve_under_roots(&path, &roots, false)?;
    if !resolved.is_file() {
        return Err(format!("路径不存在或不是文件：{}", path));
    }
    let bytes = fs::read(&resolved).map_err(|e| format!("读取失败：{}", e))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// 写仓库外文件（原子写；补齐安全根内缺失的父目录，与 write_note 同口径）。
#[tauri::command]
pub fn external_write_file(plugin_id: String, path: String, content: String, app: AppHandle) -> Result<(), String> {
    let roots = fs_roots_for(&app, &plugin_id)?;
    let resolved = resolve_under_roots(&path, &roots, true)?;
    atomic_write(&resolved, &content)
}

/// 单层列出仓库外目录条目（目录在前、按名称升序）。不屏蔽 `.` 开头项——安全根是真实目录
/// （用户批准目录或插件私有目录），点文件（.gitignore 等）是正当内容，与仓库列表屏蔽模型的语义不同。
#[tauri::command]
pub fn external_list_dir(plugin_id: String, path: String, app: AppHandle) -> Result<ListDirResult, String> {
    let roots = fs_roots_for(&app, &plugin_id)?;
    let resolved = resolve_under_roots(&path, &roots, false)?;
    if !resolved.is_dir() {
        return Err(format!("路径不存在或不是目录：{}", path));
    }
    list_dir_entries(&resolved, EXTERNAL_LIST_DIR_MAX_ENTRIES, false)
}

/// 创建仓库外目录（补齐父目录；目标已存在且是目录 = 成功，与 create_dir_all 语义一致）。
#[tauri::command]
pub fn external_create_folder(plugin_id: String, path: String, app: AppHandle) -> Result<(), String> {
    let roots = fs_roots_for(&app, &plugin_id)?;
    let resolved = resolve_under_roots(&path, &roots, true)?;
    fs::create_dir_all(&resolved).map_err(|e| format!("创建目录失败：{}", e))
}

/// 仓库外文件同目录重命名（new_name 须为纯文件名；目标已存在拒绝，防静默覆盖）。
/// 返回实际落盘路径（绝对）。
#[tauri::command]
pub fn external_rename_file(
    plugin_id: String,
    path: String,
    new_name: String,
    app: AppHandle,
) -> Result<String, String> {
    let roots = fs_roots_for(&app, &plugin_id)?;
    let src = resolve_under_roots(&path, &roots, false)?;
    if !src.is_file() {
        return Err(format!("源不是文件：{}", path));
    }
    if new_name.trim().is_empty() || new_name.contains(['/', '\\']) {
        return Err("新名称必须是纯文件名".to_string());
    }
    let parent = src.parent().ok_or_else(|| "非法路径".to_string())?;
    let dst_raw = parent.join(&new_name);
    let dst = resolve_under_roots(&dst_raw.to_string_lossy(), &roots, true)?;
    // 目标已存在直接拒绝（fs::rename 在部分平台静默覆盖）；case-only 重命名豁免
    let same_file = same_physical_file(&src, &dst);
    if dst.exists() && !same_file {
        return Err(format!("目标文件已存在：{}", new_name));
    }
    fs::rename(&src, &dst).map_err(|e| e.to_string())?;
    Ok(dst.to_string_lossy().into_owned())
}

/// 把仓库外文件移动到另一安全根内目录（目标目录可尚不存在，在安全根内自动创建）。
/// 返回实际落盘路径（绝对）。
#[tauri::command]
pub fn external_move_file(
    plugin_id: String,
    path: String,
    target_dir: String,
    app: AppHandle,
) -> Result<String, String> {
    let roots = fs_roots_for(&app, &plugin_id)?;
    let src = resolve_under_roots(&path, &roots, false)?;
    if !src.is_file() {
        return Err(format!("源不是文件：{}", path));
    }
    let dir = resolve_under_roots(&target_dir, &roots, true)?;
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
pub fn external_delete_file(plugin_id: String, path: String, app: AppHandle) -> Result<(), String> {
    let roots = fs_roots_for(&app, &plugin_id)?;
    let resolved = resolve_under_roots(&path, &roots, false)?;
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
pub fn external_delete_dir(
    plugin_id: String,
    path: String,
    force: bool,
    app: AppHandle,
) -> Result<ExternalDeleteDirResult, String> {
    let roots = fs_roots_for(&app, &plugin_id)?;
    let resolved = resolve_under_roots(&path, &roots, false)?;
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
/// 插件运行时经此取值，再用取到的路径走本命令族读写——fs 面天然把它当安全根放行。
#[tauri::command]
pub fn external_private_dir(plugin_id: String, app: AppHandle) -> Result<String, String> {
    let root = private_files_dir_of(&resolve_plugin_dir(&app, &plugin_id)?);
    fs::create_dir_all(&root).map_err(|e| format!("创建插件私有目录失败：{e}"))?;
    Ok(root.to_string_lossy().into_owned())
}

/// 二进制写（base64 进，原子写）：私有目录与授权目录通用——授权目录此前只收文本，
/// 二进制落盘是两类安全根共同的缺口。路径须落在安全根集合内，`..` 段与越界符号链接同遭拒绝。
#[tauri::command]
pub fn external_write_file_base64(
    plugin_id: String,
    path: String,
    base64_data: String,
    app: AppHandle,
) -> Result<(), String> {
    let roots = fs_roots_for(&app, &plugin_id)?;
    let resolved = resolve_under_roots(&path, &roots, true)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64_data.as_bytes())
        .map_err(|e| format!("文件数据解码失败：{e}"))?;
    atomic_write_bytes(&resolved, &bytes)
}

/// 二进制读为 dataURL（`data:<mime>;base64,...`）：mime 按扩展名推（与仓库附件读同口径），
/// 其余 application/octet-stream。私有目录与授权目录通用。
#[tauri::command]
pub fn external_read_file_data_url(
    plugin_id: String,
    path: String,
    app: AppHandle,
) -> Result<String, String> {
    let roots = fs_roots_for(&app, &plugin_id)?;
    let resolved = resolve_under_roots(&path, &roots, false)?;
    if !resolved.is_file() {
        return Err(format!("路径不存在或不是文件：{}", path));
    }
    let bytes = fs::read(&resolved).map_err(|e| format!("读取失败：{e}"))?;
    let mime = mime_from_ext(&path).unwrap_or("application/octet-stream");
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:{};base64,{}", mime, b64))
}

/// 把绝对路径解析到安全根集合下（vault 外目录面的根集合校验器，镜像 safe_join 的
/// canonicalize + starts_with 语义；root 从单个仓库根换成安全根集合）。
///
/// - 强制绝对路径；`..`（ParentDir）段直接拒绝——canonicalize 虽能消解它，但词法前缀
///   判定与逐段创建会把越界目录建出来（canonicalize 后 starts_with 按组件比较，
///   `C:\a` 与 `C:\ab` 不会误判）。
/// - 目标已存在：canonicalize 目标须落在某安全根内——文件级符号链接指向安全根外即拒。
/// - 目标不存在且 `create`：补齐安全根到父目录之间缺失的目录（仅安全根之下，不越界
///   创建），中间符号链接段解析出安全根即拒；目标 = 安全根本身（插件首建工作目录）
///   直接放行，由调用方创建。
fn resolve_under_roots(path: &str, roots: &[PathBuf], create: bool) -> Result<PathBuf, String> {
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err("仅支持绝对路径".to_string());
    }
    // 拒绝 `..` 段：canonicalize 会消解它，但词法前缀判定（under）与逐段创建（ensure）会
    // 把它当作安全根内路径放行、把越界目录建出来——safe_join 同款，ParentDir 段直接拒绝。
    if p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err(format!("非法路径：含越界段 ({})", path));
    }
    // 安全根归一化：已存在则 canonicalize（符号链接根按真实落点比较），不存在保持原形
    //（插件可经批准在目标目录新建内容，安全根本身允许尚不存在）。
    let canon_roots: Vec<PathBuf> = roots
        .iter()
        .map(|r| dunce::canonicalize(r).unwrap_or_else(|_| r.clone()))
        .collect();
    let under = |t: &Path| {
        roots.iter().any(|r| t.starts_with(r)) || canon_roots.iter().any(|r| t.starts_with(r))
    };
    if !under(p) {
        return Err(format!("路径不在可访问的目录内：{}", path));
    }
    if p.exists() {
        let canon = dunce::canonicalize(p).map_err(|e| format!("路径不可达：{} ({})", path, e))?;
        if !canon_roots.iter().any(|r| canon.starts_with(r)) {
            return Err(format!("路径越出可访问目录：{}", path));
        }
        return Ok(canon);
    }
    if !create {
        return Err(format!("路径不存在：{}", path));
    }
    // 目标 = 某个安全根本身（如插件首次建自己的工作目录）：放行，由调用方创建
    if roots.iter().any(|r| p == r) {
        return Ok(p.to_path_buf());
    }
    ensure_dirs_from_root(p, roots)?;
    let parent = p.parent().ok_or_else(|| format!("非法路径：{}", path))?;
    let parent_canon =
        dunce::canonicalize(parent).map_err(|e| format!("路径不可达：{} ({})", path, e))?;
    if !canon_roots.iter().any(|r| parent_canon.starts_with(r)) {
        return Err(format!("路径越出可访问目录：{}", path));
    }
    Ok(p.to_path_buf())
}

/// 补齐「安全根（含）到目标父目录（含）」之间缺失的目录：只在该安全根之下创建，越出
/// 安全根的祖先（安全根本身不存在时的更上层）一律不建。中间符号链接段解析出安全根即拒
///（新建落到根外等于越权）。
fn ensure_dirs_from_root(target: &Path, roots: &[PathBuf]) -> Result<(), String> {
    let parent = target.parent().ok_or_else(|| "非法路径".to_string())?;
    let root = roots
        .iter()
        .find(|r| target.starts_with(r))
        .cloned()
        .ok_or_else(|| "路径不在可访问的目录内".to_string())?;
    // 安全根尚不存在（目标 = 根的直接子项等）：先建根，再校验真实落点
    let root_canon = match dunce::canonicalize(&root) {
        Ok(c) => c,
        Err(_) => {
            fs::create_dir_all(&root).map_err(|e| format!("创建根目录失败：{}", e))?;
            dunce::canonicalize(&root).map_err(|e| format!("根目录不可达：{}", e))?
        }
    };
    // 父目录不在安全根之下（目标 = 安全根本身或其上）：根已建好即算完成
    let Ok(rel) = parent.strip_prefix(&root) else {
        return Ok(());
    };
    let mut cur = root_canon.clone();
    for comp in rel.components() {
        cur.push(comp);
        match fs::symlink_metadata(&cur) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let canon = dunce::canonicalize(&cur).map_err(|e| format!("检查目录失败：{}", e))?;
                if !canon.starts_with(&root_canon) {
                    return Err("目标路径含越出可访问目录的符号链接".to_string());
                }
            }
            Ok(meta) if !meta.is_dir() => return Err("目标路径被非目录占用".to_string()),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&cur).map_err(|e| format!("创建目录失败：{}", e))?;
            }
            Err(e) => return Err(format!("检查目录失败：{}", e)),
        }
    }
    Ok(())
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

    /// 建文件符号链接（Unix 符号链接；Windows 需开发者模式/管理员）。建不了返回 false，
    /// 平台不支持该用例时跳过（与 vault 测试同口径，以磁盘事实为准——调用成功但没落出
    /// 链接条目同样视为建不成）。
    fn try_symlink_file(original: &Path, link_path: &Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(original, link_path).is_ok()
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(original, link_path).is_ok()
                && fs::symlink_metadata(link_path)
                    .map(|meta| meta.file_type().is_symlink())
                    .unwrap_or(false)
        }
    }

    #[test]
    fn resolve_rejects_relative_and_outside_roots() {
        let tmp = TempDir::new("rel");
        let root = tmp.path().join("grant");
        let outside = tmp.path().join("other");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let roots = vec![root.clone()];
        assert!(resolve_under_roots("relative/path", &roots, false).is_err());
        assert!(resolve_under_roots(&outside.to_string_lossy(), &roots, false).is_err());
        // 安全根内的现有文件解析成功且返回 canonical
        let f = root.join("a.txt");
        fs::write(&f, "x").unwrap();
        let resolved = resolve_under_roots(&f.to_string_lossy(), &roots, false).unwrap();
        assert!(resolved.starts_with(&root));
    }

    #[test]
    fn resolve_rejects_parent_dir_components_without_side_effect() {
        let tmp = TempDir::new("dotdot");
        let root = tmp.path().join("grant");
        fs::create_dir_all(&root).unwrap();
        let roots = vec![root.clone()];
        // `..` 段词法前缀判定会误放行（grant/../outside 前缀仍是 grant），但 canonicalize 会
        // 消解到根外——直接拒绝，且不得在根外留下被创建的目录副作用
        let escape = root.join("..").join("outside").join("newdir").join("file.txt");
        assert!(resolve_under_roots(&escape.to_string_lossy(), &roots, true).is_err());
        assert!(!tmp.path().join("outside").exists(), "越界目录不得被创建");
        // `.` 段无害（CurDir，消解后仍在安全根内），放行
        let dot = root.join("sub").join(".").join("a.txt");
        let r = resolve_under_roots(&dot.to_string_lossy(), &roots, true);
        assert!(r.is_ok(), "{dot:?} 应放行");
    }

    #[test]
    fn resolve_creates_parents_within_root_only() {
        let tmp = TempDir::new("create");
        let root = tmp.path().join("grant");
        let roots = vec![root.clone()];
        // 安全根尚不存在：写根直接子项会先建根
        let deep = root.join("sub/deep.txt");
        resolve_under_roots(&deep.to_string_lossy(), &roots, true).unwrap();
        assert!(root.join("sub").is_dir());
        // 越出安全根（根之上）拒绝，且不创建根外目录
        let above = tmp.path().join("above.txt");
        assert!(resolve_under_roots(&above.to_string_lossy(), &roots, true).is_err());
        assert!(!above.exists());
    }

    #[test]
    fn resolve_rejects_symlink_escape_outside_root() {
        let tmp = TempDir::new("sym");
        let root = tmp.path().join("grant");
        let outside = tmp.path().join("secret");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        // Windows 符号链接需开发者模式/管理员，建不了按磁盘事实跳过（与 vault 测试同口径）
        let real = outside.join("real.txt");
        fs::write(&real, "x").unwrap();
        let link = root.join("leak.txt");
        if !try_symlink_file(&real, &link) {
            return;
        }
        // 安全根内符号链接指向根外：读该链接目标拒绝
        assert!(resolve_under_roots(&link.to_string_lossy(), &[root.clone()], false).is_err());
        // 安全根内的普通文件正常
        let ok = root.join("ok.txt");
        fs::write(&ok, "x").unwrap();
        assert!(resolve_under_roots(&ok.to_string_lossy(), &[root.clone()], false).is_ok());
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
    fn resolve_with_multi_roots_accepts_private_root_and_confines_to_it() {
        // 根集合 = 授权目录 ∪ 私有目录 的代表性形状：目标落在第二个根（私有目录）内，
        // 逐段创建只发生在该根之下；data 目录里私有目录之外的文件（state.json 等）不在
        // 任何根内，fs 面结构性够不到。
        let tmp = TempDir::new("multi");
        let private = tmp.path().join("plugin").join("data").join("files");
        let roots = vec![tmp.path().join("approved"), private.clone()];
        let deep = private.join("2026").join("09").join("a.png");
        resolve_under_roots(&deep.to_string_lossy(), &roots, true).unwrap();
        assert!(private.join("2026").join("09").is_dir());
        let state = tmp.path().join("plugin").join("data").join("state.json");
        fs::write(&state, "{}").unwrap();
        assert!(resolve_under_roots(&state.to_string_lossy(), &roots, false).is_err());
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
