//! 仓库存储唯一出口：路径校验、目录与文件名工具、笔记/临时文件读写、原子写、历史侧文件迁移。
//! 子域拆在 document / index / links / config / chat_store，公开项在此统一 re-export 保持既有调用路径。

mod chat_store;
mod config;
mod document;
mod index;
mod links;

pub use chat_store::*;
pub use config::*;
pub use document::*;
pub use index::*;
pub use links::*;

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use nanoid::nanoid;
use serde::Serialize;

/// 磁盘文件缓存条目：mtime（纳秒）+ 长度作失效指纹——外部编辑/同步盘改动命中指纹变化，
/// 命中克隆免每次保存重读重解析（createdAt 保留 + 补丁基底共用）。
pub struct CachedFile<T> {
    mtime_nanos: u128,
    len: u64,
    data: T,
}

/// 当前仓库会话状态，由 lib.rs app.manage 注入，命令通过 State<VaultState> 读取。
pub struct VaultState {
    pub session: Mutex<Option<VaultSession>>,
    /// 会话世代：每次 `set`（切换仓库）自增。索引在锁外构建，安装前必须比对世代——
    /// 否则构建期间切换了仓库，旧仓库的索引会被装进新仓库会话（同路径同指纹的条目会一直用错）。
    pub generation: std::sync::atomic::AtomicU64,
    /// 反链索引缓存：纯内存、磁盘为真相（每次查询按指纹 diff 自愈）；切换仓库时随 set 清空，查询时懒构建。
    pub wiki: Mutex<Option<WikiIndex>>,
    /// 标签索引缓存：同反链（纯内存、磁盘为真相、指纹增量刷新；切仓库随 set 清空、查询懒构建）。
    pub tags: Mutex<Option<TagIndex>>,
    /// 已解析 `.atlx` 缓存（key = 相对仓库根路径）：指纹校验失效，切仓库随 set 清空。
    pub canvas_cache: Mutex<HashMap<String, CachedFile<CanvasFile>>>,
    /// 已解析 `.atb` 缓存（同 canvas_cache）。
    pub table_cache: Mutex<HashMap<String, CachedFile<TableFile>>>,
    /// 仓库内容写盘串行队列：笔记等写盘统一经此提交，保证同路径写按提交序落地。
    pub writes: crate::write_queue::WriteQueue,
}

/// 一次仓库会话：根路径 + 该仓库生效的文件面板配置（open_vault 时从配置解析）。
pub struct VaultSession {
    pub root: PathBuf,
    /// 排除文件夹名列表（任何层级的同名文件夹不显示/不监听，`excludeFolders` 配置）。
    pub exclude_folders: Vec<String>,
}

impl Default for VaultState {
    fn default() -> Self {
        Self {
            session: Mutex::new(None),
            generation: std::sync::atomic::AtomicU64::new(0),
            wiki: Mutex::new(None),
            tags: Mutex::new(None),
            canvas_cache: Mutex::new(HashMap::new()),
            table_cache: Mutex::new(HashMap::new()),
            writes: crate::write_queue::WriteQueue::new(),
        }
    }
}

impl VaultState {
    /// 取当前仓库根路径，未打开仓库时返回错误。
    pub fn root(&self) -> Result<PathBuf, String> {
        self.session
            .lock()
            .map_err(|e| e.to_string())?
            .as_ref()
            .map(|s| s.root.clone())
            .ok_or_else(|| "未打开仓库".to_string())
    }

    /// 取当前仓库生效的排除文件夹列表（缺省 = 空），未打开仓库时返回错误。
    pub fn exclude_folders(&self) -> Result<Vec<String>, String> {
        self.session
            .lock()
            .map_err(|e| e.to_string())?
            .as_ref()
            .map(|s| s.exclude_folders.clone())
            .ok_or_else(|| "未打开仓库".to_string())
    }

    /// 会话快照：在**同一把 session 锁内**同时取 root / exclude / 世代。
    ///
    /// 索引在锁外构建、安装前用世代比对（见 commands::vault 的 `with_index`），分开取会出现
    /// 「读完 root 才读世代」的窗口——那期间切仓库，旧仓库的索引仍会被装进新会话。
    pub fn session_snapshot(&self) -> Result<(PathBuf, Vec<String>, u64), String> {
        let guard = self.session.lock().map_err(|e| e.to_string())?;
        let session = guard.as_ref().ok_or_else(|| "未打开仓库".to_string())?;
        let generation = self
            .generation
            .load(std::sync::atomic::Ordering::SeqCst);
        Ok((session.root.clone(), session.exclude_folders.clone(), generation))
    }

    /// 设置当前仓库会话（canonicalize 消除 `..`/符号链接，保证 safe_join 校验语义一致；
    /// 用 dunce 去除 Windows `\\?\` 长路径前缀，保证存/回传给调用方的路径格式统一）。
    /// 同时清空反链索引缓存并递增会话世代：不同仓库的索引不混用（查询时懒重建），
    /// 且锁外构建中的索引据世代判定作废（见 `generation`）。
    /// 返回 Result：Mutex poisoned 时向上传播而非静默丢弃（与 root() 策略一致）。
    pub fn set(&self, root: PathBuf, exclude_folders: Vec<String>) -> Result<(), String> {
        let canonical = dunce::canonicalize(&root).unwrap_or_else(|_| root.clone());
        let mut guard = self.session.lock().map_err(|e| e.to_string())?;
        *guard = Some(VaultSession {
            root: canonical,
            exclude_folders,
        });
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // 以下四个辅助锁（wiki/tags/canvas_cache/table_cache）poison 时清空动作无副作用，吞掉即可；
        // 读路径（query/read_cached）对 poison 走 map_err 传播（见 416/433 行），与本处策略一致。
        let _ = self.wiki.lock().map(|mut w| *w = None);
        let _ = self.tags.lock().map(|mut t| *t = None);
        // 切仓库清空文件缓存：不同仓库的同名相对路径不得混用
        let _ = self.canvas_cache.lock().map(|mut c| c.clear());
        let _ = self.table_cache.lock().map(|mut c| c.clear());
        Ok(())
    }

    /// 仅推进世代（不设仓库会话、不清缓存）：进入 / 离开协作空间时调用，
    /// 使在途的本地仓库异步回执按世代守卫过期，与 `set` 的切换语义对齐。
    pub fn bump_generation(&self) {
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// 仓库文件树节点（`list_vault_tree`，文件面板全仓库树）。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FileTreeNode {
    /// 文件名 / 文件夹名
    pub name: String,
    /// 相对仓库根路径（目录不带尾部分隔符；空串 = 仓库根自身）
    pub path: String,
    pub is_dir: bool,
    /// mtime unix 秒
    pub updated_at: i64,
    pub children: Vec<FileTreeNode>,
}

/// 相对路径分隔符归一：`\` → `/`（显式传入的相对路径可能带反斜杠；隐藏段/排除判定
/// 按统一分隔符切段，防反斜杠路径绕过过滤）。仅含 `\` 时分配，否则零开销借用。
fn normalize_rel_separators(rel: &str) -> std::borrow::Cow<'_, str> {
    if rel.contains('\\') {
        std::borrow::Cow::Owned(rel.replace('\\', "/"))
    } else {
        std::borrow::Cow::Borrowed(rel)
    }
}

/// 相对路径是否命中排除文件夹（按统一分隔符切段；`\` 与 `/` 同义）。
fn hits_exclude_folder(rel: &str, exclude: &[String]) -> bool {
    let rel = normalize_rel_separators(rel);
    rel.split('/').any(|seg| exclude.iter().any(|e| e.as_str() == seg))
}

/// 相对路径是否应被过滤：任一路径段以 `.` 开头（隐藏目录/文件，如 `.atelyx`/`.git`/`.obsidian`）
/// 或精确命中排除文件夹列表。文件树与全仓库扫描共用此判定，保证显示/扫描语义一致。
pub fn is_excluded_rel(rel: &str, exclude: &[String]) -> bool {
    let rel = normalize_rel_separators(rel);
    rel.split('/').any(|seg| seg.starts_with('.') || exclude.iter().any(|e| e.as_str() == seg))
}

/// 相对路径是否含隐藏段（任一段以 `.` 开头，如 `.atelyx/x`、`a/.git/y`）。AI 工具发现层
/// （glob/grep/list_dir）对隐藏目录完全屏蔽：遍历结果过滤 + 显式把 path/dir 指向隐藏目录时拒绝。
/// 排除 `..`（父目录段）——它由 safe_join 的越界校验拒绝，报错语义更准确。
pub fn has_hidden_segment(rel: &str) -> bool {
    let rel = normalize_rel_separators(rel);
    rel.split('/').any(|seg| {
        seg.starts_with('.') && !seg.is_empty() && seg != "." && seg != ".."
    })
}

/// 目录子项（文件面板懒加载子项；mtime 供「编辑时间」排序）。
#[derive(Clone, Debug, PartialEq)]
pub struct DirChild {
    pub rel: String,
    pub is_dir: bool,
    pub mtime: i64,
}

/// 读取 `root/rel` 目录条目（相对路径 + 是否目录 + mtime），应用全仓库统一过滤。
pub fn read_dir_entries(
    root: &Path,
    rel: &str,
    exclude: &[String],
) -> Result<Vec<DirChild>, String> {
    read_dir_entries_with(root, rel, exclude, false)
}

/// 目录条目（插件能力面）：与 [`read_dir_entries`] 同口径，但保留 `.` 开头的隐藏项
/// （插件按显式路径访问 `.atelyx/temp/` 时能列出自己写入的附件）。
pub fn read_dir_entries_all(
    root: &Path,
    rel: &str,
    exclude: &[String],
) -> Result<Vec<DirChild>, String> {
    read_dir_entries_with(root, rel, exclude, true)
}

fn read_dir_entries_with(
    root: &Path,
    rel: &str,
    exclude: &[String],
    include_hidden: bool,
) -> Result<Vec<DirChild>, String> {
    let dir = if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    };
    let mut out: Vec<DirChild> = vec![];
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = match entry.file_name().to_str() {
            Some(n) => n.to_string(),
            None => continue,
        };
        // 单段名：排除文件夹始终过滤；隐藏项只在文件树口径下过滤
        if hits_exclude_folder(&name, exclude)
            || (!include_hidden && has_hidden_segment(&name))
        {
            continue;
        }
        let child_rel = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if !is_dir && entry.path().extension().and_then(|s| s.to_str()) == Some("tmp") {
            // 跳过 atomic_write 的 `.tmp` 中间文件（原子写 `path.tmp` → rename 副产物）
            continue;
        }
        let mtime = entry.metadata().map(|md| mtime_of(&md)).unwrap_or(0);
        out.push(DirChild { rel: child_rel, is_dir, mtime });
    }
    Ok(out)
}

/// 读取 `root/rel` 目录条目（相对路径 + 是否目录），应用全仓库统一过滤：
/// 隐藏项（`.` 前缀）/ 排除文件夹 / `.tmp` 原子写副产物。递归由各 walker 自行组织
/// （list_tree_in / scan_canvases_in / walk_md_in / scan_atlx_in 共用，过滤规则只维护一处）。
pub fn read_dir_filtered(
    root: &Path,
    rel: &str,
    exclude: &[String],
) -> Result<Vec<(String, bool)>, String> {
    Ok(read_dir_entries(root, rel, exclude)?
        .into_iter()
        .map(|c| (c.rel, c.is_dir))
        .collect())
}

/// 名称自然比较：数字段按数值（`file2` < `file10`，前导零多者排后），其余按码点序。
/// 中文按码点不按拼音（Rust 侧无
/// ICU，不值得为树内排序引入 collation 依赖），英文大小写同码点序。
pub fn compare_natural(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    fn blocks(s: &str) -> Vec<&str> {
        if s.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut start = 0;
        let mut digit = s.as_bytes()[0].is_ascii_digit();
        for (i, b) in s.as_bytes().iter().enumerate() {
            let d = b.is_ascii_digit();
            if d != digit {
                out.push(&s[start..i]);
                start = i;
                digit = d;
            }
        }
        out.push(&s[start..]);
        out
    }
    let (pa, pb) = (blocks(a), blocks(b));
    for (sa, sb) in pa.iter().zip(pb.iter()) {
        let na = sa.as_bytes()[0].is_ascii_digit();
        let nb = sb.as_bytes()[0].is_ascii_digit();
        if na && nb {
            let ta = sa.trim_start_matches('0');
            let tb = sb.trim_start_matches('0');
            let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb)).then_with(|| sa.len().cmp(&sb.len()));
            if ord != Ordering::Equal {
                return ord;
            }
        } else if na != nb {
            // 数字段排前（"章2" < "章A"）
            return if na { Ordering::Less } else { Ordering::Greater };
        } else {
            let ord = sa.cmp(sb);
            if ord != Ordering::Equal {
                return ord;
            }
        }
    }
    pa.len().cmp(&pb.len())
}

/// 每层排序：同级文件夹恒在文件前，两组各自按 sortKey
/// （`name-asc` / `name-desc` / `mtime-desc` / `mtime-asc`；未知值按名称升序）。
pub fn sort_children(entries: &mut [DirChild], sort: &str) {
    let key = |a: &DirChild, b: &DirChild| -> std::cmp::Ordering {
        match sort {
            "name-desc" => compare_natural(&b.rel, &a.rel),
            "mtime-asc" => a.mtime.cmp(&b.mtime).then_with(|| compare_natural(&a.rel, &b.rel)),
            // mtime 相同回落名称序（read_dir 原始序随文件系统而异，不作为展示顺序）
            "mtime-desc" => b.mtime.cmp(&a.mtime).then_with(|| compare_natural(&a.rel, &b.rel)),
            _ => compare_natural(&a.rel, &b.rel),
        }
    };
    // is_dir 反向比较 = 文件夹（true）排前
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| key(a, b)));
}

#[cfg(test)]
mod sort_tests {
    use super::*;

    fn child(rel: &str, is_dir: bool, mtime: i64) -> DirChild {
        DirChild { rel: rel.to_string(), is_dir, mtime }
    }

    #[test]
    fn compare_natural_orders_numbers_by_value() {
        assert_eq!(compare_natural("file2", "file10"), std::cmp::Ordering::Less);
        assert_eq!(compare_natural("章2", "章10"), std::cmp::Ordering::Less);
        // 前导零多者排后（1 < 01）
        assert_eq!(compare_natural("1", "01"), std::cmp::Ordering::Less);
        // 数字段排前
        assert_eq!(compare_natural("章2", "章A"), std::cmp::Ordering::Less);
    }

    #[test]
    fn sort_children_groups_dirs_first_and_applies_key() {
        let mut entries = vec![
            child("b.md", false, 30),
            child("z", true, 1),
            child("a.md", false, 20),
            child("a", true, 2),
        ];
        // 名称升序：文件夹在前，两组各自按名称自然序
        sort_children(&mut entries, "name-asc");
        let rels: Vec<&str> = entries.iter().map(|c| c.rel.as_str()).collect();
        assert_eq!(rels, vec!["a", "z", "a.md", "b.md"]);
        // 名称降序：两组各自反转
        sort_children(&mut entries, "name-desc");
        let rels: Vec<&str> = entries.iter().map(|c| c.rel.as_str()).collect();
        assert_eq!(rels, vec!["z", "a", "b.md", "a.md"]);
        // mtime 从新到旧：两组各自按 mtime，文件夹仍在文件前
        sort_children(&mut entries, "mtime-desc");
        let rels: Vec<&str> = entries.iter().map(|c| c.rel.as_str()).collect();
        assert_eq!(rels, vec!["a", "z", "b.md", "a.md"]);
        // mtime 从旧到新
        sort_children(&mut entries, "mtime-asc");
        let rels: Vec<&str> = entries.iter().map(|c| c.rel.as_str()).collect();
        assert_eq!(rels, vec!["z", "a", "a.md", "b.md"]);
        // 未知值回落名称升序
        sort_children(&mut entries, "bogus");
        let rels: Vec<&str> = entries.iter().map(|c| c.rel.as_str()).collect();
        assert_eq!(rels, vec!["a", "z", "a.md", "b.md"]);
    }
}

/// 递归枚举仓库文件树（跳过隐藏 `.` 开头 / 排除列表 / `.tmp` 原子写副产物），按 name 升序。
pub fn list_vault_tree(root: &Path, exclude: &[String]) -> Result<Vec<FileTreeNode>, String> {
    list_tree_in(root, "", exclude)
}

fn list_tree_in(
    root: &Path,
    rel: &str,
    exclude: &[String],
) -> Result<Vec<FileTreeNode>, String> {
    let mut nodes: Vec<FileTreeNode> = vec![];
    for (child_rel, is_dir) in read_dir_filtered(root, rel, exclude)? {
        let path = root.join(&child_rel);
        let mtime = std::fs::metadata(&path)
            .ok()
            .and_then(|md| md.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let children = if is_dir {
            list_tree_in(root, &child_rel, exclude)?
        } else {
            vec![]
        };
        nodes.push(FileTreeNode {
            name: rel_last_seg(&child_rel).to_string(),
            path: child_rel,
            is_dir,
            updated_at: mtime,
            children,
        });
    }
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(nodes)
}

/// 相对路径末段（list_tree_in 展示名用；rel 非空时必有末段）。
fn rel_last_seg(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

/// 文件 mtime（Unix 秒；元数据不可得返回 0）。
pub fn mtime_secs(path: &Path) -> i64 {
    std::fs::metadata(path).map(|md| mtime_of(&md)).unwrap_or(0)
}

/// Metadata 的 mtime（Unix 秒；不可得返回 0）。DirEntry 与路径两处取 mtime 共用同一口径。
pub(crate) fn mtime_of(md: &std::fs::Metadata) -> i64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 当前 Unix 秒（引用同步等落盘时间戳用）。
pub fn now_secs() -> i64 {
    crate::time::now_secs()
}

/// 相对路径组件校验：拒绝空路径 / 绝对路径 / `..` / 根目录 / 盘符前缀，返回净化后的相对路径
/// （Windows 下 `PathBuf::join` 遇绝对路径会整体替换，仅拦 `..` 可被 `C:\x`、`\\server\x`、`\x` 绕过）。
fn validate_relative_path(file: &str) -> Result<PathBuf, String> {
    if file.is_empty() {
        return Err("非法路径：空路径".to_string());
    }
    let p = Path::new(file);
    if p.is_absolute() {
        return Err(format!("非法路径：绝对路径 ({})", file));
    }
    let mut clean = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(seg) => clean.push(seg),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!("非法路径：含越界段 ({})", file));
            }
        }
    }
    Ok(clean)
}

/// 跨平台相对路径归一（口径见 [crate::path::normalize]）。
pub fn normalize_relative_path(rel: &str) -> Result<String, String> {
    crate::path::normalize(rel)
}

/// AI 工具路径校验：在跨平台相对路径归一之上拒绝隐藏段（`.atelyx` / `.git` 等——仓库配置与
/// 密钥所在处不对 AI 工具开放），返回以 `/` 分隔的净化相对路径（空间端点直接使用该形态）。
pub fn validate_tool_path(rel: &str) -> Result<String, String> {
    let text = normalize_relative_path(rel)?;
    if has_hidden_segment(&text) {
        return Err(format!("路径不可访问（隐藏目录不对 AI 工具开放）：{rel}"));
    }
    Ok(text)
}

/// 插件能力面路径校验：只拦越界（绝对路径 / `..` / 盘符前缀），隐藏目录可访问——插件自管的
/// 临时附件就落在 `.atelyx/temp/` 下。返回以 `/` 分隔的净化相对路径（空间端点直接使用该形态）。
pub fn validate_plugin_path(rel: &str) -> Result<String, String> {
    normalize_relative_path(rel)
}

/// 校验相对路径安全并 join 仓库根，返回绝对路径（安全 12）。
/// join 后再 dunce::canonicalize 父目录校验落在仓库根内（同时覆盖符号链接逃逸）。
/// 文件可能不存在，故只校验父目录。
/// `create_parents`：写路径（write_note / rename 目标）父目录不存在时先建目录
/// （否则 canonicalize 父目录必失败，`write_note` 注释声明的「自动建父目录」不可达）。
pub fn safe_join(root: &Path, file: &str, create_parents: bool) -> Result<PathBuf, String> {
    let clean = validate_relative_path(file)?;
    let joined = root.join(clean);
    let root_canon = dunce::canonicalize(root).map_err(|_| "仓库根不可达".to_string())?;
    // 最终文件已存在时校验其真实路径仍在仓库根内：目录级穿越已由组件过滤 + 父目录
    // canonicalize 双保险，文件级符号链接（指向仓库外）是最后缺口——canonicalize 解出目标
    // 后 starts_with 拒绝；新建文件（不存在）跳过，父目录校验已覆盖中间目录符号链接。
    if joined.exists() {
        let joined_canon = dunce::canonicalize(&joined)
            .map_err(|e| format!("路径不可达：{} ({})", file, e))?;
        if !joined_canon.starts_with(&root_canon) {
            return Err(format!("路径越界：{}", file));
        }
    }
    let parent = joined.parent().ok_or_else(|| format!("非法路径：{}", file))?;
    if create_parents {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建目录失败：{} ({})", file, e))?;
    }
    let parent_canon = dunce::canonicalize(parent).map_err(|e| format!("路径不存在：{} ({})", file, e))?;
    if !parent_canon.starts_with(&root_canon) {
        return Err(format!("路径越界：{}", file));
    }
    Ok(joined)
}

/// 文件是否存在（仅元数据查询，不读内容；不限文件类型）：文件不存在（含所在目录被删）返回 false。
/// 不做父目录可达性校验（与 safe_join 的差异点）：存在性判定的对象是文件本身，
/// 父目录缺失本身就该回答「不存在」；路径组件过滤与 safe_join 同口径。
pub fn file_exists(root: &Path, file: &str) -> Result<bool, String> {
    let clean = validate_relative_path(file)?;
    Ok(root.join(clean).is_file())
}

/// 初始化仓库目录结构：仅 `.atelyx/`（无固定 画布/笔记/附件 目录，文件夹由用户自由创建）。
pub fn init_vault_dirs(root: &Path) -> Result<(), String> {
    std::fs::create_dir_all(root.join(".atelyx")).map_err(|e| e.to_string())
}

/// 两路径是否指向同一物理文件（case-only 重命名在大小写不敏感文件系统上的场景）。
/// 都 canonicalize 成功且相等才豁免；任一失败（身份不明）不豁免（宁可拒绝覆盖，不可删错文件）。
/// 画布/表格写路径共用：Windows NTFS 下 `Foo.atlx` → `foo.atlx` 新旧路径是同一文件，
/// 写新后删旧会删掉刚写入的文件（删即丢数据）。
pub fn same_physical_file(a: &Path, b: &Path) -> bool {
    match (dunce::canonicalize(a), dunce::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// 图片文件名 → 规范扩展名（白名单外拒绝；对话附件托盘与表格导入共用）。
pub fn image_ext_of(file_name: &str) -> Option<&'static str> {
    let ext = std::path::Path::new(file_name).extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("png"),
        "jpg" | "jpeg" => Some("jpg"),
        "webp" => Some("webp"),
        "gif" => Some("gif"),
        _ => None,
    }
}

/// 保存图片字节到系统 Downloads（取文件名主干净化 + 重名按 `-n` 顺延，不覆盖已有文件），
/// 返回落盘绝对路径。目录定位失败兜底用户主目录。
pub fn save_image_to_downloads(file_name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let ext = image_ext_of(file_name).ok_or_else(|| format!("非图片文件：{file_name}"))?;
    let dir = dirs::download_dir()
        .or_else(dirs::home_dir)
        .ok_or("无法定位下载目录")?;
    // 只取主干：调用方传入的附件名已带扩展名，落盘名由本函数统一拼 ext
    let stem = std::path::Path::new(file_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(file_name);
    let base = sanitize_filename(stem);
    let base = if base.is_empty() { "图片".to_string() } else { base };
    let ext_dotted = format!(".{ext}");
    let name = crate::path::unique_stem(&base, &ext_dotted, |candidate| {
        dir.join(candidate).exists()
    });
    let path = dir.join(name);
    std::fs::write(&path, bytes).map_err(|e| format!("写入失败：{e}"))?;
    Ok(path)
}

pub fn read_note(root: &Path, file: &str) -> Result<String, String> {
    let path = safe_join(root, file, false)?;
    std::fs::read_to_string(&path).map_err(|e| e.to_string())
}

/// 提示词笔记存在性（只回答「文件在不在」）：
/// 路径非法 = 不存在；读取权限/占用类失败不算，避免把瞬时读错误误判为已删除。
pub fn note_exists(root: &Path, file: &str) -> bool {
    match safe_join(root, file, false) {
        Ok(path) => path.is_file(),
        Err(_) => false,
    }
}

/// 原子写 .md（同时建父目录）。
pub fn write_note(root: &Path, file: &str, content: &str) -> Result<(), String> {
    let path = safe_join(root, file, true)?;
    atomic_write(&path, content)
}

/// 重命名 .md 文件（不更新 .atlx 引用，链接维护由 commands 层组合）。
/// 通用：附件重命名也复用此函数（路径校验与建父目录逻辑相同）。
pub fn rename_note_file(root: &Path, old_file: &str, new_file: &str) -> Result<(), String> {
    let old_path = safe_join(root, old_file, false)?;
    let new_path = safe_join(root, new_file, true)?;
    if !old_path.exists() {
        return Err(format!("源文件不存在：{}", old_file));
    }
    // 只允许重命名普通文件：目录名传入时 fs::rename 会整个移动目录（delete 因 remove_file 安全，rename 无此保护）
    if !old_path.is_file() {
        return Err(format!("源不是文件：{}", old_file));
    }
    // 目标已存在直接拒绝（Linux/macOS 的 fs::rename 会静默覆盖，Windows 会报错但语义不一）；
    // 调用方已防重名，此处兜底并发/外部创建。
    // 豁免 case-only 重命名（`note.md`→`Note.md`）：大小写不敏感文件系统上 exists() 解析到同一文件，
    // 两个 canonicalize 都成功且相等才豁免；任一失败（身份不明）不豁免，宁可拒绝不可覆盖
    let same_file = same_physical_file(&old_path, &new_path);
    if new_path.exists() && !same_file {
        return Err(format!("目标文件已存在：{}", new_file));
    }
    std::fs::rename(&old_path, &new_path).map_err(|e| e.to_string())
}

/// 删除仓库内文件（按相对路径，safe_join 防穿越）。用于 delete_note / delete_attachment。
pub fn delete_vault_file(root: &Path, file: &str) -> Result<(), String> {
    let path = safe_join(root, file, false)?;
    if !path.exists() {
        return Err(format!("文件不存在：{}", file));
    }
    // 目录误传守卫：remove_file 对目录只报系统级错误语义不明，显式拒绝并指因
    if path.is_dir() {
        return Err(format!("目标是目录，仅支持删除文件：{}", file));
    }
    std::fs::remove_file(&path).map_err(|e| e.to_string())
}

/// 侧文件名编码契约（最小百分号转义）：仅转义文件系统非法字符（`% / \ : * ? " < > |`）、
/// 控制符与 DEL（UTF-8 字节 `%XX`，大写十六进制），中文等合法字符保留原样。
/// 字符集是侧文件名的磁盘契约（`percent_decode` 对 `%XX` 解码，`%` 本身被转义故无歧义）。
/// 不能用 encodeURIComponent 式全量转义：CJK 字符会膨胀成 9 字符，中文长路径的侧文件名
/// 超出 NAS/SMB 服务端 ~260 字符路径上限（客户端 `\\?\UNC\` 扩展前缀无效），历史读写
/// 对这些笔记永久失败。最小转义下侧文件路径长度 ≈ 笔记自身路径 + 常数 overhead。
/// 注：`commands/search.rs` 另有一份私有编码（搜索请求用，保留字符集不同）——勿合并。
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let u = c as u32;
        if u < 0x20 || u == 0x7f || matches!(c, '%' | '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
            for b in c.to_string().bytes() {
                out.push_str(&format!("%{:02X}", b));
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// 存量侧文件名编码（encodeURIComponent 语义：仅字母数字与 `-_.!~*'()` 保留，其余全量 UTF-8 %XX）。
/// 仅供读取回退路径（新编码名缺失时按此名探查存量侧文件），新产出恒走 `percent_encode`。
pub fn percent_encode_legacy(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '!' | '~' | '*' | '\'' | '(' | ')') {
            out.push(c);
        } else {
            for b in c.to_string().bytes() {
                out.push_str(&format!("%{:02X}", b));
            }
        }
    }
    out
}

/// 历史侧文件根与 kind 子目录（note 保持旧路径 `.atelyx/history/<enc>.json` 向后兼容；
/// canvas/table 按 kind 分目录摊平）。
const HISTORY_DIR: &str = ".atelyx/history";

const HISTORY_KIND_SUBDIRS: [&str; 3] = ["", "canvas/", "table/"];

/// 迁移单个文件的全部候选历史侧文件（note/canvas/table 三处只命中存在的一处）：
/// `.atelyx/history[/<kind>/]<enc(old)>.json` → 同结构新编码路径。
/// 只认最小编码名——存量旧编码名须先迁至新名，
/// remap 才找得到源（by_dir 解码文件名，可直读旧名）。
/// 源不存在（无历史/已迁移过）静默跳过；目标已存在（罕见：新路径已累积历史）跳过不覆盖。
/// 单个失败不阻断其它（历史尽力而为，不阻塞重命名主流程）。
pub fn remap_sideloads(root: &Path, old_file: &str, new_file: &str) {
    let old_enc = percent_encode(old_file);
    let new_enc = percent_encode(new_file);
    for sub in HISTORY_KIND_SUBDIRS {
        let old_rel = format!("{HISTORY_DIR}/{sub}{old_enc}.json");
        let new_rel = format!("{HISTORY_DIR}/{sub}{new_enc}.json");
        if old_rel == new_rel {
            continue;
        }
        // 仅当源存在且为文件（safe_join 已防穿越）才迁移；失败静默降级
        let Ok(old_path) = safe_join(root, &old_rel, false) else {
            continue;
        };
        if !old_path.is_file() {
            continue;
        }
        let Ok(new_path) = safe_join(root, &new_rel, true) else {
            continue;
        };
        if new_path.exists() {
            continue;
        }
        let _ = std::fs::rename(&old_path, &new_path);
    }
}

/// 文件夹重命名后迁移其下全部历史侧文件：遍历 `.atelyx/history[/<kind>/]`，
/// 对每个 `.json` 侧文件解码文件名，命中 `old_dir/` 前缀者改写到 `new_dir/` 前缀同结构新名。
/// 目标已存在跳过（防覆盖）；单文件失败静默（历史尽力而为，不阻塞文件夹重命名主流程）。
/// 目录参数先剥尾随 `/`（与 rename_folder 主流程的宽容语义一致，防 `a//` 前缀永不命中）。
pub fn remap_sideloads_by_dir(root: &Path, old_dir: &str, new_dir: &str) {
    let old_dir = old_dir.trim_end_matches('/');
    let new_dir = new_dir.trim_end_matches('/');
    if old_dir.is_empty() || new_dir.is_empty() {
        return; // 空目录无意义（根目录不可重命名），静默返回
    }
    let prefix = format!("{old_dir}/");
    for sub in HISTORY_KIND_SUBDIRS {
        let dir_rel = format!("{HISTORY_DIR}/{sub}");
        let Ok(dir) = safe_join(root, &dir_rel, false) else {
            continue;
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = match entry.file_name().into_string() {
                Ok(n) => n,
                Err(_) => continue,
            };
            if !name.ends_with(".json") {
                continue;
            }
            let stem = &name[..name.len() - 5];
            let decoded = percent_decode(stem);
            let Some(rest) = decoded.strip_prefix(&prefix) else {
                continue;
            };
            let new_name = format!("{}.json", percent_encode(&format!("{new_dir}/{rest}")));
            let dst = dir.join(&new_name);
            if dst.exists() {
                continue;
            }
            let _ = std::fs::rename(entry.path(), dst);
        }
    }
}

/// 读仓库内文件字节（按相对路径，safe_join 防穿越）。用于 read_attachment_data_url。
pub fn read_file_bytes(root: &Path, file: &str) -> Result<Vec<u8>, String> {
    let path = safe_join(root, file, false)?;
    std::fs::read(&path).map_err(|e| e.to_string())
}

/// 读仓库内文件的前 `max_bytes` 字节（safe_join 防穿越）：
/// 图片固有尺寸等只落在起始段的读取，不必整读大文件。
pub fn read_file_head(root: &Path, file: &str, max_bytes: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let path = safe_join(root, file, false)?;
    let file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    file.take(max_bytes).read_to_end(&mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}

/// 原子写：写唯一临时文件 → fsync → rename 覆盖目标 → fsync 父目录。
/// - 临时名带纳秒时间戳 + 进程内序号：并发写同一目标不交叉同一 tmp。
/// - 写后 sync_all：崩溃/断电时 rename 已提交但数据未刷盘会丢最后一次保存。
/// - 任一步失败都清理临时文件，避免残留。
///
/// pub：commands/global.rs 的全局配置/UI 状态写盘复用（保证全项目同一 durability 语义）。
pub fn atomic_write(path: &Path, content: &str) -> Result<(), String> {
    atomic_write_bytes(path, content.as_bytes())
}

/// 原子写字节（atomic_write 的字节形态，同一套 tmp → rename + fsync 语义）。
/// pub：附件二进制与插件外部文件命令的写盘复用（保证全项目同一 durability 语义）。
pub fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = write_tmp(path, bytes)?;
    commit_tmp(&tmp, path)
}

/// 从既有文件原子落库（目标同目录生成 tmp 后 copy 再 rename，跨卷也不影响原子性；
/// 用于把插件暂存的大文件收进仓库）。`source` 必须在调用期间保持存在。
pub fn atomic_write_from_file(path: &Path, source: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = tmp_path(path);
    let copy = || -> std::io::Result<()> {
        let mut src = std::fs::File::open(source)?;
        let mut dst = std::fs::File::create(&tmp)?;
        std::io::copy(&mut src, &mut dst)?;
        dst.sync_all()
    };
    match copy() {
        Ok(()) => commit_tmp(&tmp, path),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e.to_string())
        }
    }
}

/// tmp 落正：rename 覆盖目标 + fsync 父目录；失败清理 tmp。
fn commit_tmp(tmp: &Path, path: &Path) -> Result<(), String> {
    std::fs::rename(tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(tmp);
        format!("写入失败：{e}")
    })?;
    sync_parent_dir(path);
    Ok(())
}

/// 进程内单调序号：SystemTime 不可用时（uniq 恒 0）仍避免并发写同一目标共用同一 tmp 名。
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// 目标同目录的唯一临时文件名（纳秒时间戳 + 进程内序号，并发写不交叉同一 tmp）。
fn tmp_path(path: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(format!("{}.{}.{}.tmp", path.display(), nanos, seq))
}

/// 写临时文件（建父目录 → 写入 → fsync），返回 tmp 路径；写入期任一步失败即清理 tmp。
pub(super) fn write_tmp(path: &Path, content: &[u8]) -> Result<PathBuf, String> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = tmp_path(path);
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(content)?;
        f.sync_all()
    };
    match write() {
        // f 在闭包结束时析构（Windows 上 rename 目标不能是打开的文件）
        Ok(()) => Ok(tmp),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e.to_string())
        }
    }
}

/// rename 后 fsync 父目录：只 fsync 文件本身时，断电可能让 rename 未持久化而回退旧内容。
/// 尽力而为——rename 已成功，把目录 fsync 的失败上报成「写失败」会误导调用方。
fn sync_parent_dir(path: &Path) {
    if let Some(parent) = path.parent() {
        sync_dir(parent);
    }
}

#[cfg(unix)]
pub(super) fn sync_dir(dir: &Path) {
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
}

#[cfg(not(unix))]
pub(super) fn sync_dir(_dir: &Path) {}

/// 文件名净化：替换 `/\:*?"<>|` 为 `_`，
/// 去首尾空白；Windows 保留名（CON/PRN/AUX/NUL/COM1-9/LPT1-9，含任意扩展名）与尾部点/空格
/// 补 `_` 前缀/后缀避免 I/O 失败（`CON.md`、`foo.` 在 Windows 会被拒绝或截断）。
pub fn sanitize_filename(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect();
    let trimmed = cleaned.trim();
    let stem = trimmed.split('.').next().unwrap_or("");
    if is_windows_reserved_name(stem) {
        format!("_{}", trimmed)
    } else if trimmed.ends_with(['.', ' ']) {
        format!("{}_", trimmed)
    } else {
        trimmed.to_string()
    }
}

/// 文件名的扩展名（不含点；无扩展名与仅前导点的隐藏名返回 None）。
pub fn file_extension(name: &str) -> Option<&str> {
    match name.rfind('.') {
        None | Some(0) => None,
        Some(i) => Some(&name[i + 1..]),
    }
}

/// 文件名的主名（去掉最后一个扩展名；无扩展名与隐藏名原样返回）。
pub fn file_stem(name: &str) -> &str {
    match file_extension(name) {
        Some(ext) => &name[..name.len() - ext.len() - 1],
        None => name,
    }
}

/// 仓库内临时附件区（插件写入的临时媒体、宿主托管的会话 / 表格附件都在其下）。
pub const TEMP_DIR: &str = ".atelyx/temp/";

/// 会话图片附件目录前缀（临时区内的宿主托管目录）。
pub const CHAT_TEMP_DIR: &str = ".atelyx/temp/chats/";

/// 表格图片附件目录前缀（临时区内的宿主托管目录）。
pub const TABLE_TEMP_DIR: &str = ".atelyx/temp/tables/";

/// 仓库相对路径的父目录（根 = 空串；末段为文件名或目录名）。
pub fn parent_rel(rel: &str) -> String {
    rel.rsplit_once('/').map(|(parent, _)| parent).unwrap_or("").to_string()
}

/// 仓库相对路径的显示标题：末段去掉扩展名（页面标题、标签与新建文档的标题同源）。
pub fn file_label(rel: &str) -> String {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    file_stem(name).to_string()
}

/// 名称尾部若恰好是本文件的扩展名（忽略大小写）则剥掉：重命名输入框只显示主名，
/// 用户补打后缀时不该落成 `名字.atb.atb`。
pub fn strip_own_extension<'a>(name: &'a str, ext: &str) -> &'a str {
    let suffix = ext.len() + 1;
    if name.len() < suffix || !name.is_char_boundary(name.len() - suffix) {
        return name;
    }
    let (head, tail) = name.split_at(name.len() - suffix);
    (tail.starts_with('.') && tail[1..].eq_ignore_ascii_case(ext)).then_some(head).unwrap_or(name)
}

/// 重命名提交：输入的主名 → 新文件名。扩展名恒取原名（重命名不改类型），
/// 输入尾部手打的同名后缀先剥掉。净化后为空返回 None。
pub fn rename_file_name(input: &str, old_name: &str) -> Option<String> {
    let input = input.trim();
    match file_extension(old_name) {
        Some(ext) => {
            // 输入即当前主名 = 未改动，原样返回（主名本身以同后缀结尾的文件不会被顺手削一段）
            if input == file_stem(old_name) {
                return Some(old_name.to_string());
            }
            let stem = sanitize_filename(strip_own_extension(input, ext));
            (!stem.is_empty()).then(|| format!("{stem}.{ext}"))
        }
        None => {
            let name = sanitize_filename(input);
            (!name.is_empty()).then_some(name)
        }
    }
}

/// 新建草稿名净化：与重命名同口径剥掉手打的同名后缀（新建表格输入「表.atb」不落成
/// 「表.atb.atb」）。`ext` = 该类条目的扩展名（文件夹为 None）。净化后为空返回 None。
pub fn draft_file_name(input: &str, ext: Option<&str>) -> Option<String> {
    let input = input.trim();
    let stem = sanitize_filename(match ext {
        Some(ext) => strip_own_extension(input, ext),
        None => input,
    });
    (!stem.is_empty()).then_some(stem)
}

/// Windows 保留设备名（不带扩展名时同样保留，如 `CON` / `CON.txt` 均非法）。
fn is_windows_reserved_name(stem: &str) -> bool {
    let up = stem.to_ascii_uppercase();
    matches!(
        up.as_str(),
        "CON" | "PRN" | "AUX" | "NUL"
            | "COM1" | "COM2" | "COM3" | "COM4" | "COM5" | "COM6" | "COM7" | "COM8" | "COM9"
            | "LPT1" | "LPT2" | "LPT3" | "LPT4" | "LPT5" | "LPT6" | "LPT7" | "LPT8" | "LPT9"
    )
}

/// 新建文件夹（相对仓库根路径，如 `项目A/素材`），自动建父目录；已存在则幂等返回。
/// dir 经 safe_join 校验（拒绝 `..`/绝对路径越界）并建父目录。
pub fn create_folder(root: &Path, dir: &str) -> Result<String, String> {
    let path = safe_join(root, dir, true)?;
    if !path.is_dir() {
        std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
    }
    Ok(dir.to_string())
}

/// 删除文件夹的结果（结构化返回：空目录直接删；非空且未带 force 时返回需确认信息供调用方确认）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteFolderResult {
    /// 本次是否已删除
    pub deleted: bool,
    /// 目录非空且未带 force：需用户确认后以 force=true 再次调用
    pub needs_confirm: bool,
    /// 目录内条目数（递归计数，含子目录与隐藏文件）
    pub item_count: usize,
}

/// 删除文件夹（相对仓库根路径）。force=false 只删空目录——非空返回
/// `needs_confirm`（含递归条目数）供调用方确认后以 force=true 重试；
/// force=true 递归删除全部内容。dir 为空（仓库根）拒绝，防误删整个仓库。
pub fn delete_folder(root: &Path, dir: &str, force: bool) -> Result<DeleteFolderResult, String> {
    if dir.is_empty() {
        return Err("非法路径：不能删除仓库根目录".to_string());
    }
    let path = safe_join(root, dir, false)?;
    if !path.is_dir() {
        return Err(format!("目录不存在：{}", dir));
    }
    let item_count = count_dir_items(&path)?;
    if !force && item_count > 0 {
        return Ok(DeleteFolderResult {
            deleted: false,
            needs_confirm: true,
            item_count,
        });
    }
    if force {
        std::fs::remove_dir_all(&path).map_err(|e| e.to_string())?;
    } else {
        std::fs::remove_dir(&path).map_err(|e| e.to_string())?;
    }
    Ok(DeleteFolderResult {
        deleted: true,
        needs_confirm: false,
        item_count,
    })
}

/// 递归统计目录内条目数（含隐藏文件与子目录；删除确认弹窗文案用）。
fn count_dir_items(dir: &Path) -> Result<usize, String> {
    let mut count = 0;
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        count += 1;
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            count += count_dir_items(&entry.path())?;
        }
    }
    Ok(count)
}

/// 移动整个目录到新相对路径（校验 + 防覆盖；`fs::rename` 对目录同样生效）。
/// 不更新 .atlx 引用——引用前缀维护由 commands 层组合（与 rename_note 同模式）。
pub fn rename_folder(root: &Path, old_dir: &str, new_dir: &str) -> Result<(), String> {
    if old_dir.is_empty() || new_dir.is_empty() {
        return Err("非法路径：目录为空".to_string());
    }
    let old_path = safe_join(root, old_dir, false)?;
    let new_path = safe_join(root, new_dir, true)?;
    if !old_path.is_dir() {
        return Err(format!("目录不存在：{}", old_dir));
    }
    // 目标已存在拒绝（防覆盖丢数据；调用方已防重名，此处兜底并发/外部创建）。
    // 豁免 case-only 重命名（`dir`→`Dir`）：大小写不敏感文件系统上 exists() 解析到同一目录，
    // 两个 canonicalize 都成功且相等才豁免；任一失败（身份不明）不豁免，宁可拒绝不可覆盖
    let same_file = same_physical_file(&old_path, &new_path);
    if new_path.exists() && !same_file {
        return Err(format!("目标目录已存在：{}", new_dir));
    }
    std::fs::rename(&old_path, &new_path).map_err(|e| e.to_string())
}

/// 复制整个目录到新相对路径（递归复制全部内容含隐藏文件；链接不复制；校验 + 防覆盖，与 rename_folder 对称）。
/// 副本是独立目录，内部 .atlx 引用仍是相对路径、随目录整体复制，无需链接维护；
/// 目录内画布/表格重新生成 id（防同 id 双文件歧义：画布标签按 id 去重、协作合并按 id 身份）。
pub fn copy_folder(root: &Path, old_dir: &str, new_dir: &str) -> Result<(), String> {
    if old_dir.is_empty() || new_dir.is_empty() {
        return Err("非法路径：目录为空".to_string());
    }
    let old_path = safe_join(root, old_dir, false)?;
    let new_path = safe_join(root, new_dir, true)?;
    if !old_path.is_dir() {
        return Err(format!("目录不存在：{}", old_dir));
    }
    // 目标已存在拒绝（防覆盖丢数据；调用方已防重名，此处兜底并发/外部创建）
    if new_path.exists() {
        return Err(format!("目标目录已存在：{}", new_dir));
    }
    if let Err(e) = copy_dir_all(&old_path, &new_path) {
        // 复制中途失败：清理残缺副本，防半成品目录残留在文件树
        let _ = std::fs::remove_dir_all(&new_path);
        return Err(format!("复制目录失败：{e}"));
    }
    if let Err(e) = regenerate_ids_in(&new_path) {
        let _ = std::fs::remove_dir_all(&new_path);
        return Err(format!("复制目录失败：{e}"));
    }
    Ok(())
}

/// 递归复制目录内容（含隐藏文件与子目录；链接一律跳过——Unix 符号链接与 Windows 符号链接/
/// 目录联接都由 `DirEntry::file_type`（不跟随链接）报为链接。`fs::copy` 会跟随链接，
/// 把仓库外文件的内容复制进仓库；链接到目录则整个复制失败）。
pub fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let target = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// 重写目录内全部 `.atlx`/`.atb` 的 id 为新值（目录副本不改文件名，内嵌标题本就与
/// 各自文件名一致）。读失败的文件视为损坏跳过（与画布扫描同策略）。
fn regenerate_ids_in(dir: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            regenerate_ids_in(&path)?;
        } else {
            regenerate_file_id(&path)?;
        }
    }
    Ok(())
}

/// 重写单个 `.atlx`/`.atb` 的 id 为新值，内嵌标题改为这份文件自己的主名
/// （单文件副本换了文件名：留着原件的标题会让副本被当成原件改名，见 `title` 字段语义）。
/// 防同 id 双文件歧义：画布标签按 id 去重、协作合并按 id 身份；
/// 非这两种扩展名/读失败（损坏文件）原样保留，不阻断复制。
pub fn regenerate_file_id(path: &Path) -> Result<(), String> {
    let ext = path.extension().and_then(|s| s.to_str());
    let title = path.file_stem().and_then(|s| s.to_str()).map(str::to_string);
    if ext == Some("atlx") {
        if let Ok(mut canvas) = document::read_canvas_file(path) {
            canvas.id = nanoid!();
            if let Some(title) = &title {
                canvas.title = title.clone();
            }
            document::write_canvas_file(path, &canvas)?;
        }
    } else if ext == Some("atb") {
        if let Ok(mut table) = document::read_table_file(path) {
            table.id = nanoid!();
            if let Some(title) = &title {
                table.title = title.clone();
            }
            document::write_table_file(path, &table)?;
        }
    }
    Ok(())
}

/// 新实体稳定 id（画布 / 表格 / 协作合并按 id 身份；与 regenerate_file_id 同源）。
pub fn new_entity_id() -> String {
    nanoid!()
}

/// 仓库显示名：优先取路径最后一段（文件夹名）；网络共享根（`\\server\share`，无更深子目录）
/// 与尾部带分隔符的路径 `file_name()` 为 None——回退为去尾部分隔符后取最后一段（UNC 根取 share 名）。
/// 本地盘根（`E:\`）回退为盘符（`E:`），少见但避免空白名。
pub fn vault_display_name(root: &Path) -> String {
    if let Some(n) = root.file_name() {
        if let Some(s) = n.to_str() {
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }
    root.to_string_lossy()
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// 在 `(相对路径, 文件名)` 清单上按查询词检索：大小写不敏感，文件名命中优先于仅路径命中
/// （前缀 > 包含 > 仅路径命中），同档保持传入顺序（清单本身即树序）。
/// 空白查询 = 全部命中、原序返回。返回下标，由调用方决定渲染上限。
pub fn search_file_hits(files: &[(String, String)], query: &str) -> Vec<usize> {
    /// 命中档位 = 排序键。
    #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    enum Rank {
        NamePrefix,
        NameContains,
        PathContains,
    }

    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return (0..files.len()).collect();
    }
    let mut ranked: Vec<(Rank, usize)> = Vec::new();
    for (i, (rel, name)) in files.iter().enumerate() {
        let name = name.to_lowercase();
        let rank = if name.starts_with(&q) {
            Rank::NamePrefix
        } else if name.contains(&q) {
            Rank::NameContains
        } else if rel.to_lowercase().contains(&q) {
            Rank::PathContains
        } else {
            continue;
        };
        ranked.push((rank, i));
    }
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    ranked.into_iter().map(|(_, i)| i).collect()
}

#[cfg(test)]
mod search_tests {
    use super::search_file_hits;

    fn files(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items.iter().map(|(rel, name)| (rel.to_string(), name.to_string())).collect()
    }

    #[test]
    fn blank_query_returns_all_in_tree_order() {
        let f = files(&[("b.md", "b.md"), ("a.md", "a.md")]);
        assert_eq!(search_file_hits(&f, ""), vec![0, 1]);
        // 只含空白的查询等同空查询（输入框里敲空格不该把结果清空）
        assert_eq!(search_file_hits(&f, "   "), vec![0, 1]);
    }

    #[test]
    fn ranks_name_prefix_then_name_contains_then_path_hit() {
        let f = files(&[
            ("归档/会议纪要.md", "会议纪要.md"),
            ("周报/2026-10.md", "2026-10.md"),
            ("项目/周报归档/2025.md", "2025.md"),
            ("周报汇总.md", "周报汇总.md"),
            ("杂项/关于周报的思考.md", "关于周报的思考.md"),
            ("其它/无关.md", "无关.md"),
        ]);
        // 前缀命中 → 名称包含 → 仅路径命中（同档保持树序），无关项剔除
        assert_eq!(search_file_hits(&f, "周报"), vec![3, 4, 1, 2]);
    }

    #[test]
    fn matches_case_insensitively() {
        let f = files(&[("Logs/Report.md", "Report.md")]);
        assert_eq!(search_file_hits(&f, "REPORT"), vec![0]);
        assert_eq!(search_file_hits(&f, "logs"), vec![0]);
    }

    #[test]
    fn path_hit_only_when_name_misses() {
        let f = files(&[("notes/todo.md", "todo.md")]);
        // 「notes」只在目录名里：仍命中，且不会因为匹配到路径而升级为名称档
        assert_eq!(search_file_hits(&f, "notes"), vec![0]);
    }
}

/// 简易 percent 解码（%XX）；非法序列原样保留（匹配不上自然不命中，不报错）。
/// 壳的链接 href 解码、服务端的仓库内链接解析都按这份语义走。
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod hidden_segment_tests {
    use super::*;

    #[test]
    fn detects_hidden_segments() {
        // 隐藏目录：根级与任意深度
        assert!(has_hidden_segment(".atelyx/config.json"));
        assert!(has_hidden_segment(".git/config"));
        assert!(has_hidden_segment("a/.hidden/b.md"));
        assert!(has_hidden_segment(".hidden"));
        // 隐藏文件（根级 . 开头文件）
        assert!(has_hidden_segment(".gitignore"));
        assert!(has_hidden_segment(".env"));
        // 普通路径不含隐藏段
        assert!(!has_hidden_segment(""));
        assert!(!has_hidden_segment("a/b.md"));
        assert!(!has_hidden_segment("笔记/方案.txt"));
        assert!(!has_hidden_segment("a"));
        // 当前目录段 `.` 与父目录段 `..` 不算隐藏（`..` 由 safe_join 越界校验拒绝）
        assert!(!has_hidden_segment("./a.md"));
        assert!(!has_hidden_segment(".."));
        assert!(!has_hidden_segment("../x"));
        // 反斜杠分隔符按统一分隔符切段（显式传入的相对路径可能带 `\`，防绕过隐藏判定）
        assert!(has_hidden_segment(r".atelyx\config.json"));
        assert!(has_hidden_segment(r"a\.git\config"));
        assert!(!has_hidden_segment(r"a\b.md"));
    }

    #[test]
    fn plugin_paths_allow_hidden_segments_but_reject_escapes() {
        assert_eq!(
            validate_plugin_path(".atelyx/temp/tables/t1/a.png").unwrap(),
            ".atelyx/temp/tables/t1/a.png"
        );
        assert_eq!(validate_plugin_path("./笔记/a.md").unwrap(), "笔记/a.md");
        assert!(validate_plugin_path("../x").is_err());
        assert!(validate_plugin_path(r"..\x").is_err(), "反斜杠形态的越界段同样拒绝");
        assert!(validate_plugin_path("/x").is_err());
        assert!(validate_plugin_path("").is_err());
        // 反斜杠按分隔符归一：同期路径形如 `附件\图.png` 与 `附件/图.png` 同义
        assert_eq!(validate_plugin_path(r"附件\图.png").unwrap(), "附件/图.png");
        // 盘符前缀在 Unix 上只是普通文件名，但共享内容到了 Windows 会被解析成绝对路径
        assert!(validate_plugin_path(r"C:\x.md").is_err(), "盘符前缀两端口径一致拒绝");
        assert!(validate_plugin_path("C:x.md").is_err());
        assert!(validate_plugin_path(r"a\C:\x.md").is_err(), "非首段的盘符形态同样拒绝");
    }

    #[test]
    fn plugin_listing_keeps_hidden_entries() {
        let dir = crate::vault::test_support::TempDir::new("plugin-list");
        std::fs::create_dir_all(dir.join(".atelyx/temp/tables/t1")).unwrap();
        std::fs::write(dir.join(".atelyx/temp/tables/t1/a.png"), b"x").unwrap();
        std::fs::write(dir.join(".secret"), b"x").unwrap();
        std::fs::create_dir_all(dir.join("private")).unwrap();
        std::fs::write(dir.join("note.md"), b"x").unwrap();

        let exclude = vec!["private".to_string()];
        let mut visible: Vec<String> = read_dir_entries(&dir, "", &exclude)
            .unwrap()
            .into_iter()
            .map(|child| child.rel)
            .collect();
        visible.sort();
        assert_eq!(visible, vec!["note.md"], "文件树口径过滤隐藏项");

        let mut all: Vec<String> = read_dir_entries_all(&dir, "", &exclude)
            .unwrap()
            .into_iter()
            .map(|child| child.rel)
            .collect();
        all.sort();
        assert_eq!(all, vec![".atelyx", ".secret", "note.md"], "插件口径保留隐藏项、仍过滤排除文件夹");

        let attachments = read_dir_entries_all(&dir, ".atelyx/temp/tables/t1", &[]).unwrap();
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].rel, ".atelyx/temp/tables/t1/a.png");
    }

    #[test]
    fn excludes_backslash_paths() {
        // 反斜杠路径同样命中隐藏/排除判定（与正斜杠同口径）
        assert!(is_excluded_rel(r".atelyx\config.json", &[]));
        assert!(is_excluded_rel(r"docs\.git\config", &[]));
        assert!(is_excluded_rel(r"a\private\f.md", &["private".to_string()]));
        assert!(!is_excluded_rel(r"a\public\f.md", &["private".to_string()]));
        // 正斜杠行为不变
        assert!(is_excluded_rel("a/.git/config", &[]));
        assert!(is_excluded_rel("private/f.md", &["private".to_string()]));
    }
}

#[cfg(test)]
mod sideload_remap_tests {
    use super::*;

    /// 临时目录（纳秒级命名防碰撞；残留于系统临时目录）。
    fn tmp_root(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("atelyx-remap-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 写一个历史侧文件（kind_sub = "" / "canvas/" / "table/"）。
    fn write_side(root: &Path, kind_sub: &str, file: &str, content: &str) {
        let rel = format!(
            "{HISTORY_DIR}/{kind_sub}{}.json",
            percent_encode(file)
        );
        let path = root.join(&rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// 断言某历史侧文件存在且内容匹配。
    fn assert_side(root: &Path, kind_sub: &str, file: &str, content: &str) {
        let rel = format!(
            "{HISTORY_DIR}/{kind_sub}{}.json",
            percent_encode(file)
        );
        assert_eq!(
            std::fs::read_to_string(root.join(&rel)).unwrap(),
            content,
            "侧文件 {} 不存在或内容不符",
            rel
        );
    }

    /// 断言某历史侧文件不存在。
    fn assert_side_gone(root: &Path, kind_sub: &str, file: &str) {
        let rel = format!(
            "{HISTORY_DIR}/{kind_sub}{}.json",
            percent_encode(file)
        );
        assert!(
            !root.join(&rel).exists(),
            "侧文件 {} 应已被迁移走",
            rel
        );
    }

    #[test]
    fn encode_decode_roundtrip() {
        // 最小编码契约：仅转义非法字符与 %（UTF-8 %XX），CJK 等合法字符保留原样——
        // 全量转义会让 CJK 膨胀 9 倍，长中文路径的侧文件名超 NAS 服务端路径上限
        let f = "笔记/我的 笔记.md";
        assert_eq!(percent_encode(f), "笔记%2F我的 笔记.md");
        assert_eq!(percent_decode(&percent_encode(f)), f);
        assert_eq!(percent_encode("a/b.md"), "a%2Fb.md");
        assert_eq!(percent_decode("a%2Fb.md"), "a/b.md");
        // 文件名含 %：自身被编码为 %25，往返一致且无歧义（空格合法保留原样）
        let pct = "a%20b 笔记.md";
        assert_eq!(percent_encode(pct), "a%2520b 笔记.md");
        assert_eq!(percent_decode(&percent_encode(pct)), pct);
        // 非法 % 序列不 panic、原样保留
        assert_eq!(percent_decode("a%zz"), "a%zz");
    }

    #[test]
    fn remap_single_file_all_kinds() {
        let root = tmp_root("single");
        write_side(&root, "", "笔记/旧.md", "v1");
        write_side(&root, "canvas/", "画布/旧.atlx", "cv1");
        write_side(&root, "table/", "表格/旧.atb", "tb1");
        remap_sideloads(&root, "笔记/旧.md", "笔记/新.md");
        remap_sideloads(&root, "画布/旧.atlx", "画布/新.atlx");
        remap_sideloads(&root, "表格/旧.atb", "表格/新.atb");
        assert_side(&root, "", "笔记/新.md", "v1");
        assert_side(&root, "canvas/", "画布/新.atlx", "cv1");
        assert_side(&root, "table/", "表格/新.atb", "tb1");
        assert_side_gone(&root, "", "笔记/旧.md");
        assert_side_gone(&root, "canvas/", "画布/旧.atlx");
        assert_side_gone(&root, "table/", "表格/旧.atb");
    }

    #[test]
    fn remap_missing_source_is_noop() {
        let root = tmp_root("missing");
        // 无历史侧文件：静默跳过不报错（首次重命名/从未保存过）
        remap_sideloads(&root, "无.md", "有.md");
        assert_side_gone(&root, "", "有.md");
    }

    #[test]
    fn remap_skips_existing_target() {
        let root = tmp_root("target");
        write_side(&root, "", "旧.md", "old");
        write_side(&root, "", "新.md", "newer");
        remap_sideloads(&root, "旧.md", "新.md");
        // 目标已存在：跳过不覆盖，旧文件保留
        assert_side(&root, "", "新.md", "newer");
        assert_side(&root, "", "旧.md", "old");
    }

    #[test]
    fn remap_by_dir_prefix() {
        let root = tmp_root("bydir");
        write_side(&root, "", "项目A/笔记1.md", "a1");
        write_side(&root, "", "项目A/子/笔记2.md", "a2");
        write_side(&root, "canvas/", "项目A/画布.atlx", "cv");
        write_side(&root, "canvas/", "其他/画布.atlx", "other");
        remap_sideloads_by_dir(&root, "项目A", "项目B");
        assert_side(&root, "", "项目B/笔记1.md", "a1");
        assert_side(&root, "", "项目B/子/笔记2.md", "a2");
        assert_side(&root, "canvas/", "项目B/画布.atlx", "cv");
        // 前缀外的侧文件不受影响
        assert_side(&root, "canvas/", "其他/画布.atlx", "other");
        assert_side_gone(&root, "", "项目A/笔记1.md");
        assert_side_gone(&root, "", "项目A/子/笔记2.md");
        assert_side_gone(&root, "canvas/", "项目A/画布.atlx");
    }

    #[test]
    fn remap_by_dir_empty_history_dir() {
        let root = tmp_root("empty");
        // .atelyx/history 不存在或为空：静默返回
        remap_sideloads_by_dir(&root, "a", "b");
    }

    #[test]
    fn remap_by_dir_skips_existing_target() {
        let root = tmp_root("bytarget");
        write_side(&root, "", "项目A/笔记.md", "old");
        write_side(&root, "", "项目B/笔记.md", "newer");
        remap_sideloads_by_dir(&root, "项目A", "项目B");
        // 目标已存在：跳过不覆盖，两边都保留
        assert_side(&root, "", "项目B/笔记.md", "newer");
        assert_side(&root, "", "项目A/笔记.md", "old");
    }

    #[test]
    fn remap_sideloads_by_dir_trims_trailing_slash() {
        let root = tmp_root("trailing");
        write_side(&root, "", "项目A/笔记.md", "v1");
        // 调用方带尾斜杠：须归一化后正常迁移（防 `a//` 前缀永不命中）
        remap_sideloads_by_dir(&root, "项目A/", "项目B/");
        assert_side(&root, "", "项目B/笔记.md", "v1");
        assert_side_gone(&root, "", "项目A/笔记.md");
    }

    #[test]
    fn remap_by_dir_migrates_legacy_encoded_names() {
        // 存量侧文件（旧 encodeURIComponent 名）随文件夹改名直接迁移：
        // by_dir 解码文件名（兼容 %XX 大写/小写 hex）后按前缀改写、再最小重编码。
        // 项/目/笔/记 = %E9%A1%B9 %E7%9B%AE %E7%AC%94 %E8%AE%B0（encodeURIComponent 全量转义）
        let root = tmp_root("legacy");
        let legacy = format!(
            "{HISTORY_DIR}/%E9%A1%B9%E7%9B%AEA%2F%E7%AC%94%E8%AE%B01.md.json"
        );
        std::fs::create_dir_all(root.join(HISTORY_DIR)).unwrap();
        std::fs::write(root.join(&legacy), "legacy-v1").unwrap();
        remap_sideloads_by_dir(&root, "项目A", "项目B");
        assert_side(&root, "", "项目B/笔记1.md", "legacy-v1");
        assert!(
            !root.join(&legacy).exists(),
            "旧 encodeURIComponent 名侧文件应已被迁移走"
        );
    }

    #[test]
    fn remap_by_dir_prefix_boundary() {
        // 前缀必须带 `/` 分隔：old_dir="项目A" 不得命中同级「项目A2/…」（否则迁移错位）
        let root = tmp_root("boundary");
        write_side(&root, "", "项目A/笔记.md", "a");
        write_side(&root, "", "项目A2/笔记.md", "a2");
        remap_sideloads_by_dir(&root, "项目A", "项目B");
        assert_side(&root, "", "项目B/笔记.md", "a");
        assert_side(&root, "", "项目A2/笔记.md", "a2"); // 近邻名不受影响
        assert_side_gone(&root, "", "项目A/笔记.md");
    }
}

#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    use std::path::{Path, PathBuf};

    /// 测试用临时目录（纳秒级命名防碰撞）；`Drop` 递归清理，测试失败也不留残留。
    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir().join(format!("atelyx-{tag}-{nanos}"));
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }

        pub fn join(&self, rel: &str) -> PathBuf {
            self.0.join(rel)
        }
    }

    impl std::ops::Deref for TempDir {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 目录树下残留的 `.tmp` 文件（相对路径，排序）。
    pub fn tmp_leftovers(dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(cur) = stack.pop() {
            for entry in std::fs::read_dir(&cur).unwrap().filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.to_string_lossy().ends_with(".tmp") {
                    out.push(path.to_string_lossy().into_owned());
                }
            }
        }
        out.sort();
        out
    }
}

#[cfg(test)]
mod safe_join_tests {
    use super::test_support::{tmp_leftovers, TempDir};
    use super::*;

    #[test]
    fn rejects_traversal_and_absolute() {
        let root = TempDir::new("safejoin-reject");
        for bad in [
            "",
            "..",
            "../escape.md",
            "a/..",
            "a/../../escape.md",
            "notes/../../outside/x.md",
            "/etc/passwd",
        ] {
            assert!(safe_join(&root, bad, false).is_err(), "应拒绝：{bad}");
        }
        assert!(tmp_leftovers(&root).is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn rejects_windows_prefix_paths() {
        let root = TempDir::new("safejoin-winprefix");
        for bad in [
            "C:\\Windows\\x.md",
            "C:/Windows/x.md",
            "\\\\server\\share\\x.md",
            "\\\\?\\C:\\x.md",
            "\\x.md",
        ] {
            assert!(safe_join(&root, bad, false).is_err(), "应拒绝：{bad}");
        }
    }

    #[test]
    fn accepts_plain_relative_paths() {
        let root = TempDir::new("safejoin-plain");
        std::fs::create_dir_all(root.join("子目录/深层")).unwrap();
        assert_eq!(safe_join(&root, "a.md", false).unwrap(), root.join("a.md"));
        assert_eq!(
            safe_join(&root, "子目录/深层/b.md", false).unwrap(),
            root.join("子目录/深层/b.md")
        );
        // `./` 段被归一化掉
        assert_eq!(safe_join(&root, "./a.md", false).unwrap(), root.join("a.md"));
        // 中文与 emoji 文件名
        assert_eq!(
            safe_join(&root, "子目录/📝笔记.md", false).unwrap(),
            root.join("子目录/📝笔记.md")
        );
    }

    #[test]
    fn missing_parent_rejected_unless_create_parents() {
        let root = TempDir::new("safejoin-parents");
        assert!(safe_join(&root, "新目录/x.md", false).is_err());
        let created = safe_join(&root, "新目录/深层/x.md", true).unwrap();
        assert!(created.parent().unwrap().is_dir());
        assert_eq!(created, root.join("新目录/深层/x.md"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escaping_root() {
        let root = TempDir::new("safejoin-symlink-out");
        let outside = TempDir::new("safejoin-symlink-target");
        std::fs::write(outside.join("secret.md"), "s").unwrap();
        std::os::unix::fs::symlink(outside.join("secret.md"), root.join("evil.md")).unwrap();
        assert!(safe_join(&root, "evil.md", false).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn accepts_symlink_inside_root() {
        let root = TempDir::new("safejoin-symlink-in");
        std::fs::write(root.join("real.md"), "s").unwrap();
        std::os::unix::fs::symlink(root.join("real.md"), root.join("link.md")).unwrap();
        assert_eq!(safe_join(&root, "link.md", false).unwrap(), root.join("link.md"));
    }

    #[cfg(unix)]
    #[test]
    fn accepts_root_that_is_itself_a_symlink() {
        // 仓库根经符号链接打开（网络盘/挂载点常见）：root 与 canonicalize 后的父目录必须一致，
        // 否则正常写入会被误判「路径越界」
        let real = TempDir::new("safejoin-real-root");
        let parent = TempDir::new("safejoin-link-parent");
        let link = parent.join("link-root");
        std::os::unix::fs::symlink(&*real, &link).unwrap();
        assert_eq!(safe_join(&link, "a.md", false).unwrap(), link.join("a.md"));
    }
}

#[cfg(test)]
mod copy_folder_tests {
    use super::copy_folder;
    use super::rename_folder;
    use super::test_support::TempDir;
    use std::path::Path;

    /// 目录 case-only 改名（`dir`→`Dir`）：大小写不敏感文件系统上 exists() 解析到自身，
    /// 须豁免占用检查而不是拒绝。改名结果按 read_dir 的真实目录名校验（exists 对
    /// 大小写变体恒真，判不出改名是否生效）。
    #[test]
    fn rename_folder_allows_case_only_rename() {
        let root = TempDir::new("rename-dir-case");
        std::fs::create_dir_all(root.join("dir/sub")).unwrap();
        std::fs::write(root.join("dir/sub/a.md"), "内容").unwrap();
        rename_folder(&root, "dir", "Dir").unwrap();
        assert!(root.join("Dir/sub/a.md").exists());
        let names: Vec<String> = std::fs::read_dir(&*root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"Dir".to_string()) && !names.contains(&"dir".to_string()));
        // 真占用仍拒绝：同名不同物不可覆盖
        std::fs::create_dir_all(root.join("other")).unwrap();
        assert!(rename_folder(&root, "Dir", "other").is_err());
        assert!(root.join("Dir/sub/a.md").exists());
    }

    #[test]
    fn copies_files_and_subdirs() {
        let root = TempDir::new("copyfolder-plain");
        std::fs::create_dir_all(root.join("源/子目录")).unwrap();
        std::fs::write(root.join("源/原件.md"), "content").unwrap();
        std::fs::write(root.join("源/子目录/内.md"), "inner").unwrap();

        copy_folder(&root, "源", "副本").unwrap();

        assert_eq!(std::fs::read_to_string(root.join("副本/原件.md")).unwrap(), "content");
        assert_eq!(std::fs::read_to_string(root.join("副本/子目录/内.md")).unwrap(), "inner");
        // 源目录内容不变
        assert_eq!(std::fs::read_to_string(root.join("源/原件.md")).unwrap(), "content");
    }

    /// 建文件/目录链接（Unix 符号链接；Windows 符号链接需开发者模式）。
    /// 建不了返回 false，平台不支持该用例时跳过。
    #[cfg(unix)]
    fn link(original: &Path, link_path: &Path, _dir: bool) -> bool {
        std::os::unix::fs::symlink(original, link_path).is_ok()
    }

    #[cfg(windows)]
    fn link(original: &Path, link_path: &Path, dir: bool) -> bool {
        let called = if dir {
            std::os::windows::fs::symlink_dir(original, link_path).is_ok()
        } else {
            std::os::windows::fs::symlink_file(original, link_path).is_ok()
        };
        // 调用成功不等于建成：收紧权限的机器上可能返回 Ok 却只落出普通文件/目录，
        // 甚至不产生条目。以磁盘事实为准，建不成即按平台不支持跳过。
        called
            && std::fs::symlink_metadata(link_path)
                .map(|meta| meta.file_type().is_symlink())
                .unwrap_or(false)
    }

    #[test]
    fn skips_links_to_file_and_directory() {
        let root = TempDir::new("copyfolder-link");
        let outside = TempDir::new("copyfolder-outside");
        std::fs::write(outside.join("secret.md"), "secret").unwrap();
        std::fs::create_dir_all(root.join("源")).unwrap();
        std::fs::write(root.join("源/原件.md"), "content").unwrap();
        // 链接到仓库外文件：不得把外部内容复制进仓库
        if !link(&outside.join("secret.md"), &root.join("源/外链.md"), false) {
            return;
        }
        // 链接到祖先目录（仓库根）：不得递归展开
        if !link(&root, &root.join("源/根链"), true) {
            return;
        }

        copy_folder(&root, "源", "副本").unwrap();

        assert!(root.join("副本/原件.md").is_file());
        assert!(!root.join("副本/外链.md").exists());
        assert!(!root.join("副本/根链").exists());
        // 源目录的链接仍在（复制不改动源）
        assert!(root
            .join("源/外链.md")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());
    }
}

#[cfg(test)]
mod duplicate_tests {
    use super::test_support::TempDir;
    use super::{read_table_file, regenerate_file_id, write_table_file, TableFile, TABLE_SCHEMA};

    /// 单文件副本换了文件名：id 换新，内嵌标题改成副本自己的主名
    /// （留着原件的标题会让副本被当成原件改名）。
    #[test]
    fn copy_takes_own_id_and_title() {
        let root = TempDir::new("duplicate-title");
        let src = root.join("动画分镜模板.atb");
        write_table_file(
            &src,
            &TableFile {
                schema: TABLE_SCHEMA.into(),
                id: "tb-1".into(),
                title: "动画分镜模板".into(),
                fields: vec![],
                rows: vec![],
                created_at: 0,
                updated_at: 0,
            },
        )
        .unwrap();

        let copy = root.join("动画分镜模板-2.atb");
        std::fs::copy(&src, &copy).unwrap();
        regenerate_file_id(&copy).unwrap();

        let copied = read_table_file(&copy).unwrap();
        assert_ne!(copied.id, "tb-1", "副本须换新 id");
        assert_eq!(copied.title, "动画分镜模板-2", "内嵌标题取副本自己的主名");
        // 原件不动
        assert_eq!(read_table_file(&src).unwrap().title, "动画分镜模板");
    }
}

#[cfg(test)]
mod read_file_head_tests {
    use super::test_support::TempDir;
    use super::read_file_head;

    #[test]
    fn head_reads_only_prefix_and_keeps_tail() {
        let root = TempDir::new("readhead-prefix");
        std::fs::write(root.join("a.bin"), b"0123456789").unwrap();
        assert_eq!(read_file_head(&root, "a.bin", 4).unwrap(), b"0123");
        // 限额大于文件长度时读到文件末尾
        assert_eq!(read_file_head(&root, "a.bin", 99).unwrap(), b"0123456789");
    }

    #[test]
    fn head_rejects_escape_and_missing() {
        let root = TempDir::new("readhead-reject");
        assert!(read_file_head(&root, "../out.bin", 8).is_err());
        assert!(read_file_head(&root, "missing.bin", 8).is_err());
    }
}

#[cfg(test)]
mod percent_decode_tests {
    use super::*;

    /// 与服务端 `fsops::percent_decode` 同口径（独立工程不共享代码，靠同一张期望表对拍）。
    #[test]
    fn keeps_invalid_sequences() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%2"), "%2");
        assert_eq!(percent_decode("%E7%AC%94%E8%AE%B0"), "笔记");
    }
}

#[cfg(test)]
mod atomic_write_tests {
    use super::test_support::{tmp_leftovers, TempDir};
    use super::*;

    #[test]
    fn writes_and_replaces_content() {
        let root = TempDir::new("atomic-replace");
        let target = root.join("a.md");
        atomic_write(&target, "第一版").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "第一版");
        atomic_write(&target, "第二版").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "第二版");
        assert!(tmp_leftovers(&root).is_empty(), "残留：{:?}", tmp_leftovers(&root));
    }

    #[test]
    fn creates_missing_parent_dirs() {
        let root = TempDir::new("atomic-mkdir");
        let target = root.join("新目录/深层/a.md");
        atomic_write(&target, "x").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "x");
        assert!(tmp_leftovers(&root).is_empty());
    }

    #[test]
    fn no_tmp_left_when_target_is_a_directory() {
        let root = TempDir::new("atomic-renfail");
        let target = root.join("adir");
        std::fs::create_dir(&target).unwrap();
        assert!(atomic_write(&target, "x").is_err());
        assert!(tmp_leftovers(&root).is_empty(), "残留：{:?}", tmp_leftovers(&root));
        // 目标目录未被动过
        assert!(target.is_dir());
    }

    #[test]
    fn no_tmp_created_when_parent_is_a_file() {
        // 父目录被占位 → 建目录阶段即失败，不产生任何 tmp
        let root = TempDir::new("atomic-noparent");
        std::fs::write(root.join("afile"), "x").unwrap();
        let target = root.join("afile/a.md");
        assert!(atomic_write(&target, "x").is_err());
        assert!(tmp_leftovers(&root).is_empty());
        assert_eq!(std::fs::read_to_string(root.join("afile")).unwrap(), "x");
    }

    #[test]
    fn concurrent_writes_same_target_yield_one_complete_version() {
        let root = TempDir::new("atomic-concurrent");
        let target = root.join("same.md");
        let versions: Vec<String> = (0..8).map(|i| format!("版本-{i}-{}", "x".repeat(4096))).collect();
        std::thread::scope(|scope| {
            for v in &versions {
                let target = &target;
                scope.spawn(move || atomic_write(target, v).unwrap());
            }
        });
        let final_content = std::fs::read_to_string(&target).unwrap();
        assert!(versions.contains(&final_content), "落盘内容不是任何一个完整版本");
        assert!(tmp_leftovers(&root).is_empty(), "残留：{:?}", tmp_leftovers(&root));
    }
}

#[cfg(test)]
mod sanitize_filename_tests {
    use super::sanitize_filename;

    /// 净化用例表：落盘名必须稳定，否则改名后的保存目标与列表路径会指错文件。
    /// 保留名一律只折叠 ASCII 字母，不做 Unicode 大小写。
    #[test]
    fn matches_frontend_sanitize_filename() {
        for (input, expected) in [
            // 非法字符替换 + trim
            ("a/b:c*d?\"e<f>g|h", "a_b_c_d__e_f_g_h"),
            ("  笔记  ", "笔记"),
            ("a\\b", "a_b"),
            // Windows 保留名按首个点前的 stem 判定（含扩展名同样非法）
            ("con.md", "_con.md"),
            ("CON", "_CON"),
            ("lpt9.txt", "_lpt9.txt"),
            ("com1", "_com1"),
            ("PRN", "_PRN"),
            ("icon.md", "icon.md"),
            // 尾部点补 `_`；尾空格先被 trim（故不补）
            ("笔记.", "笔记._"),
            ("笔记 ", "笔记"),
            ("笔记", "笔记"),
            // 多字节字符不受 ASCII 折叠影响
            ("第三季度.atlx", "第三季度.atlx"),
            ("📝笔记", "📝笔记"),
        ] {
            assert_eq!(sanitize_filename(input), expected, "输入 {input:?}");
        }
    }
}

#[cfg(test)]
mod file_name_tests {
    use super::{draft_file_name, file_extension, file_stem, rename_file_name};

    #[test]
    fn extension_and_stem_split_on_last_dot() {
        assert_eq!(file_extension("笔记.md"), Some("md"));
        assert_eq!(file_extension("v1.2.md"), Some("md"));
        assert_eq!(file_extension("readme"), None);
        // 仅前导点的隐藏名没有扩展名
        assert_eq!(file_extension(".env"), None);
        assert_eq!(file_stem("v1.2.md"), "v1.2");
        assert_eq!(file_stem("表.atb"), "表");
        assert_eq!(file_stem("readme"), "readme");
        assert_eq!(file_stem(".env"), ".env");
    }

    /// 输入框只显示主名：手打的同名后缀先剥掉，后缀恒取原名（不改类型）。
    #[test]
    fn rename_keeps_original_extension() {
        assert_eq!(rename_file_name("新名", "表.atb").as_deref(), Some("新名.atb"));
        assert_eq!(rename_file_name("新名.atb", "表.atb").as_deref(), Some("新名.atb"));
        assert_eq!(rename_file_name("新名.ATB", "表.atb").as_deref(), Some("新名.atb"));
        // 别的后缀不剥（是主名的一部分）
        assert_eq!(rename_file_name("新名.md", "表.atb").as_deref(), Some("新名.md.atb"));
        // 无扩展名与隐藏名按原名改名
        assert_eq!(rename_file_name("新名.md", "readme").as_deref(), Some("新名.md"));
        assert_eq!(rename_file_name("新名", ".env").as_deref(), Some("新名"));
        // 净化后为空 = 不提交
        assert_eq!(rename_file_name("   ", "表.atb"), None);
        assert_eq!(rename_file_name(".atb", "表.atb"), None);
        // 未改动（输入即当前主名）= 原样返回：主名自带同后缀的双后缀文件不被削一段
        assert_eq!(rename_file_name("表.md", "表.md.md").as_deref(), Some("表.md.md"));
    }

    #[test]
    fn draft_name_strips_typed_extension() {
        assert_eq!(draft_file_name("表", Some("atb")).as_deref(), Some("表"));
        assert_eq!(draft_file_name("表.atb", Some("atb")).as_deref(), Some("表"));
        assert_eq!(draft_file_name("笔记.md", Some("md")).as_deref(), Some("笔记"));
        assert_eq!(draft_file_name("素材", None).as_deref(), Some("素材"));
        assert_eq!(draft_file_name("  ", Some("md")), None);
    }
}
