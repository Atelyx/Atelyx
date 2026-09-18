//! 空间内容文件树的共享文件原语：路径安全校验、目录过滤遍历、原子写、时间戳。
//!
//! 路径安全与客户端仓库读写同口径：拒绝绝对路径 / `..` / 盘符前缀 / 越出空间根
//! （组件过滤 + 父目录 canonicalize 双保险，文件级符号链接由存在性检查 + canonicalize 兜底）。

use std::fmt;
use std::path::{Component, Path, PathBuf};

/// join 失败的两种性质：`Invalid` = 语义拒绝（穿越/绝对路径/越界，HTTP 400）；
/// `NotFound` = 父目录不存在（HTTP 404，与「文件不存在」同口径）。
pub enum JoinError {
    Invalid(String),
    NotFound(String),
}

impl fmt::Display for JoinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(m) | Self::NotFound(m) => f.write_str(m),
        }
    }
}

/// 空间根（内容文件树根）。
pub struct SpaceRoot(pub PathBuf);

impl SpaceRoot {
    /// 校验相对路径安全并 join 空间根。`create_parents`：写路径父目录不存在时先建
    /// （否则父目录 canonicalize 必失败，「自动建父目录」不可达）。
    pub fn join(&self, rel: &str, create_parents: bool) -> Result<PathBuf, JoinError> {
        let invalid = |m: String| JoinError::Invalid(m);
        if rel.is_empty() {
            return Err(invalid("非法路径：空路径".to_string()));
        }
        let p = Path::new(rel);
        if p.is_absolute() {
            return Err(invalid(format!("非法路径：绝对路径 ({rel})")));
        }
        let mut clean = PathBuf::new();
        for c in p.components() {
            match c {
                Component::Normal(seg) => clean.push(seg),
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(invalid(format!("非法路径：含越界段 ({rel})")));
                }
            }
        }
        let joined = self.0.join(&clean);
        let root_canon = dunce::canonicalize(&self.0).map_err(|_| invalid("空间根不可达".to_string()))?;
        // 已存在的最终路径解出真实落点后必须仍在空间根内（文件级符号链接是最后缺口）；
        // 新建文件（不存在）跳过，父目录校验已覆盖中间目录符号链接
        if joined.exists() {
            let joined_canon = dunce::canonicalize(&joined)
                .map_err(|e| JoinError::NotFound(format!("路径不可达：{rel} ({e})")))?;
            if !joined_canon.starts_with(&root_canon) {
                return Err(invalid(format!("路径越界：{rel}")));
            }
        }
        let parent = joined.parent().ok_or_else(|| invalid(format!("非法路径：{rel}")))?;
        if create_parents {
            std::fs::create_dir_all(parent).map_err(|e| invalid(format!("创建目录失败：{e}")))?;
        }
        let parent_canon = dunce::canonicalize(parent)
            .map_err(|e| JoinError::NotFound(format!("路径不存在：{rel} ({e})")))?;
        if !parent_canon.starts_with(&root_canon) {
            return Err(invalid(format!("路径越界：{rel}")));
        }
        Ok(joined)
    }
}

/// 相对路径是否含隐藏段（`.` 前缀段，`.`/`..` 不算——后者由路径安全校验按越界拒绝）。
pub fn has_hidden_segment(rel: &str) -> bool {
    rel.split('/').any(|seg| seg.starts_with('.') && !seg.is_empty() && seg != "." && seg != "..")
}

/// 相对路径（单段）是否被全树过滤：隐藏项与原子写 `.tmp` 中间产物。
pub fn is_excluded_name(name: &str) -> bool {
    (name.starts_with('.') && name.len() > 1) || name.ends_with(".tmp")
}

/// 读取目录条目（相对路径 + 是否目录），应用统一过滤（隐藏项 / `.tmp`）。
pub fn read_dir_filtered(dir: &Path, rel: &str) -> Result<Vec<(String, bool)>, String> {
    let mut out: Vec<(String, bool)> = vec![];
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = match entry.file_name().to_str() {
            Some(n) => n.to_string(),
            None => continue,
        };
        if is_excluded_name(&name) {
            continue;
        }
        let child_rel = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        out.push((child_rel, is_dir));
    }
    Ok(out)
}

/// 递归枚举文件（相对路径 + mtime unix 秒；过滤规则同上）。
pub fn walk_files(root: &Path, rel: &str, out: &mut Vec<(String, i64)>) -> Result<(), String> {
    let dir = if rel.is_empty() { root.to_path_buf() } else { root.join(rel) };
    let mut entries = read_dir_filtered(&dir, rel)?;
    // 按名排序保证遍历顺序确定（read_dir 顺序由文件系统决定）
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    for (child_rel, is_dir) in entries {
        if is_dir {
            walk_files(root, &child_rel, out)?;
        } else {
            // 先取 mtime 再 move：元组按序求值，先 move 会让 mtime 计算借用失效
            let mtime = file_mtime_secs(&root.join(&child_rel));
            out.push((child_rel, mtime));
        }
    }
    Ok(())
}

/// mtime unix 秒（失败回退 0；单文件失败不拖垮整轮）。
pub fn file_mtime_secs(path: &Path) -> i64 {
    std::fs::metadata(path)
        .ok()
        .and_then(|md| md.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 原子写：写 `<原名>.<随机>.tmp` → rename 覆盖（防半截文件；rename 在 Windows 上覆盖已存在文件）。
/// 临时名以 `.tmp` 结尾——与目录遍历的副产物过滤规则对齐（残留中间文件不进树/索引）；
/// 带随机段防同路径并发写互踩临时文件。
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use rand::RngCore;
    let mut suffix = [0u8; 4];
    rand::thread_rng().fill_bytes(&mut suffix);
    let hex: String = suffix.iter().map(|b| format!("{b:02x}")).collect();
    let tmp = path.with_extension(format!("{}.tmp", hex));
    std::fs::write(&tmp, bytes).map_err(|e| format!("写 {} 失败：{e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("落盘 {} 失败：{e}", path.display()))
}
