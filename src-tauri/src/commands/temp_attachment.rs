//! 附件临时区命令：未入库附件与表格图片统一落仓库内隐藏目录，实体文件只存路径引用。
//!
//! 目录分层 `.atelyx/temp/<组件>/<实例目录>/`：
//! - 组件 = 画布（`canvas`，画布对话节点附件）/ AI 会话（`sessions`，面板会话附件）/
//!   表格（`tables`，图片字段）；
//! - 画布与会话的实例目录 = 实例 id 的 FNV-1a 16 位哈希（id 来自实体文件内容、可被外部构造，
//!   哈希排除分隔符/`..`/保留名等路径语义），目录内标记文件记录实例 id 供回收反查；
//!   表格实例目录直接用 tableId（应用生成，写入前校验无路径语义）；
//! - 隐藏目录：文件树与全仓库扫描天然跳过；
//! - **放在仓库内**（而非应用数据目录）：路径校验直接复用 `safe_join`（越界一律拒绝），
//!   回收天然按仓库归属，读取复用仓库附件读命令——三件事都不需要另造一套边界；
//! - 未入库的内容会随 `.atelyx` 一起被 Git/云盘同步（不额外忽略）。
//!
//! 回收 = 引用收集器注册：各组件声明自己的引用来源（画布扫全仓 `.atlx`、会话扫 `.atelyx/对话历史/`
//! 的 `.jsonl`、表格扫全仓 `.atb` 的 image 单元格），统一收集「被引用的完整仓库相对路径」白名单，
//! 只删实例目录内不在白名单里的顶层文件（节点复制粘贴可把画布引用带到别的画布，白名单必须全仓收集）。
//! 「保存到仓库」= 把临时件复制进附件文件夹并换成普通仓库相对路径引用；临时目录的回收由
//! 实体关闭/删除时的清理命令（`cleanup_temp_attachments`，按组件 + 实例 id + 实体文件路径）
//! 与进仓兜底清扫负责。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use base64::Engine;
use serde::Serialize;
use tauri::State;

use super::table::image_cell_entries;
use crate::vault::{atomic_write, atomic_write_bytes, safe_join, VaultState, CHAT_HISTORY_DIR};

/// 未入库临时附件的根目录（相对仓库根）。
pub const TEMP_ATTACHMENT_DIR: &str = ".atelyx/temp";

/// 画布/会话实例目录的标记文件：内容 = 实例 id，防「实体文档恰好暂时读不到」与目录被
/// 同名/碰撞复用时把整目录清空。
const INSTANCE_MARKER_FILE: &str = ".instance-id";

/// 孤儿实例目录最大存活时长（秒）：未入库附件在仓库内，超龄且归属不在且无引用才回收。
const ORPHAN_TEMP_MAX_AGE_SECS: u64 = 24 * 60 * 60;

/// 临时区组件：一个组件 = 一类把附件落进临时区的实体（画布 / AI 会话 / 表格）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TempComponent {
    Canvas,
    Session,
    Table,
}

impl TempComponent {
    /// 前端传入的组件标识。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "canvas" => Some(Self::Canvas),
            "session" => Some(Self::Session),
            "table" => Some(Self::Table),
            _ => None,
        }
    }

    /// 组件目录名（`TEMP_ATTACHMENT_DIR` 下的固定一层）。
    fn dir_name(self) -> &'static str {
        match self {
            Self::Canvas => "canvas",
            Self::Session => "sessions",
            Self::Table => "tables",
        }
    }

    /// 组件目录名 → 组件（清扫器遍历 temp 根时判定目录归属）。
    fn from_dir_name(name: &str) -> Option<Self> {
        match name {
            "canvas" => Some(Self::Canvas),
            "sessions" => Some(Self::Session),
            "tables" => Some(Self::Table),
            _ => None,
        }
    }

    /// 画布/会话实例目录落标记（表格目录名即 tableId，无需标记）。
    fn uses_marker(self) -> bool {
        !matches!(self, Self::Table)
    }
}

/// 实例 id → 临时目录名：稳定、定长、无路径语义。
///
/// 为什么不直接用画布/会话 id：它来自实体文件内容（可被外部构造/同步），直接拼进路径就得再写一套
/// 穿越校验；派生成立即排除分隔符/`..`/保留名的十六进制串，`safe_join` 之外不再需要额外校验。
/// 碰撞由实例标记兜底：目录标记与当前实例 id 不一致即视为他人目录，不删。
pub(crate) fn instance_temp_key(instance_id: &str) -> String {
    // FNV-1a（64bit）：不需要加密强度，只要稳定且分布均匀
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in instance_id.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// 某组件某实例的临时目录（相对仓库根）。表格直接用 tableId（写入前经 `validate_table_id`）。
pub(crate) fn instance_temp_dir(component: TempComponent, instance_id: &str) -> String {
    let leaf = match component {
        TempComponent::Table => instance_id.to_string(),
        _ => instance_temp_key(instance_id),
    };
    format!("{}/{}/{}", TEMP_ATTACHMENT_DIR, component.dir_name(), leaf)
}

/// 表格 id 直接进路径（实例目录名），只接受应用生成器的字母表（字母/数字/`-`/`_`）。
pub(crate) fn validate_table_id(table_id: &str) -> Result<(), String> {
    let valid = !table_id.is_empty()
        && table_id.len() <= 128
        && table_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if valid {
        Ok(())
    } else {
        Err(format!("非法的表格 id：{table_id}"))
    }
}

/// 把附件字节写入临时区，返回仓库相对路径引用。前端粘贴/拖入附件时调用。
///
/// 走 base64 文本而不是字节数组：附件可达数十 MB，base64 让载荷形状在 IPC 两端只有一种解释
/// （JSON 字符串），不依赖字节数组的序列化细节。
#[tauri::command]
pub fn write_temp_attachment(
    component: String,
    instance_id: String,
    file_name: String,
    base64_data: String,
    state: State<'_, VaultState>,
) -> Result<String, String> {
    let comp =
        TempComponent::parse(&component).ok_or_else(|| format!("未知临时区组件：{component}"))?;
    if matches!(comp, TempComponent::Table) {
        validate_table_id(&instance_id)?;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64_data.as_bytes())
        .map_err(|e| format!("附件数据解码失败：{e}"))?;
    let root = state.root()?;
    let leaf = format!("att-{}-{}", nanoid::nanoid!(), sanitize_temp_file_name(&file_name));
    let rel = format!("{}/{}", instance_temp_dir(comp, &instance_id), leaf);
    let path = safe_join(&root, &rel, true)?;
    // 唯一名不可能已存在，但仍走原子写：写入完整性优先（与仓库其它写盘同一语义）
    ensure_instance_marker(&root, comp, &instance_id);
    atomic_write_bytes(&path, &bytes)?;
    Ok(rel)
}

/// 首次写入该实例目录时落标记（幂等；失败不阻断写入——标记只用于「不敢删」的保守判断）。
fn ensure_instance_marker(root: &Path, component: TempComponent, instance_id: &str) {
    if !component.uses_marker() {
        return;
    }
    let Ok(marker) = safe_join(
        root,
        &format!("{}/{INSTANCE_MARKER_FILE}", instance_temp_dir(component, instance_id)),
        true,
    ) else {
        return;
    };
    if marker.exists() {
        return;
    }
    let _ = atomic_write(&marker, instance_id);
}

/// 临时文件名净化：只保留叶子名（去掉调用方可能带的路径段），替换分隔符与非法字符。
fn sanitize_temp_file_name(file_name: &str) -> String {
    let leaf = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name).trim();
    let cleaned: String = leaf
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            _ => c,
        })
        .collect();
    let cleaned = cleaned.trim_matches('.').trim();
    // 空格/中文等原样保留：引用扫描按 JSON 字符串值整体判定，不靠空白判定
    if cleaned.is_empty() {
        "attachment".to_string()
    } else {
        cleaned.to_string()
    }
}

/// 把临时附件复制进仓库附件文件夹，返回仓库相对路径（画布改用该路径引用）。
/// 复制而非移动：临时件在画布上可能被多个节点引用，清理由引用回收统一负责。
/// `file_name` 由调用方给出（附件本就带着显示名）：叶子名里的内部前缀（`att-<随机串>-`）不可反解
/// ——随机串的字母表含 `-`，按分隔符切会把随机串尾段当成名字的一部分。
#[tauri::command]
pub fn import_vault_attachment(
    window: tauri::WebviewWindow,
    rel: String,
    file_name: String,
    state: State<'_, VaultState>,
) -> Result<AttachmentImportResult, String> {
    let root = state.root()?;
    let src = safe_join(&root, &rel, false)?;
    if !src.is_file() {
        return Err(format!("临时附件不存在：{rel}"));
    }
    let folder = crate::vault::read_vault_config(&root)
        .unwrap_or_default()
        .attachment_folder;
    let target_rel =
        unique_attachment_rel(&root, folder.as_deref(), &sanitize_temp_file_name(&file_name))?;
    let dest = safe_join(&root, &target_rel, true)?;
    std::fs::copy(&src, &dest).map_err(|e| format!("复制附件失败：{e}"))?;
    super::content_broadcast::broadcast_content_changes(
        &window,
        &root.to_string_lossy(),
        vec![super::content_broadcast::ContentChange::write(&target_rel)],
    );
    Ok(AttachmentImportResult { file: target_rel })
}

/// 生成仓库内唯一的落位相对路径：`<附件文件夹>/<基础名>.<ext>`，重名追加 ` (n)`。
/// `attachmentFolder` 配置缺省/空 = 仓库根目录（设置页「附件导入默认文件夹」）。
fn unique_attachment_rel(root: &Path, folder: Option<&str>, file_name: &str) -> Result<String, String> {
    let dir = folder.unwrap_or("").trim_matches('/').to_string();
    let (stem, ext) = match file_name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() && !e.is_empty() => (s.to_string(), Some(e.to_string())),
        _ => (file_name.to_string(), None),
    };
    let with_ext = |name: &str| match &ext {
        Some(e) => format!("{name}.{e}"),
        None => name.to_string(),
    };
    let join = |name: &str| {
        if dir.is_empty() {
            with_ext(name)
        } else {
            format!("{dir}/{}", with_ext(name))
        }
    };
    let mut candidate = join(&stem);
    let mut n = 1;
    // 目标已存在即换序号（不覆盖已有附件；同一附件重复「保存到仓库」也不会互相覆盖）
    while safe_join(root, &candidate, true).map(|p| p.exists()).unwrap_or(false) {
        candidate = join(&format!("{stem} ({n})"));
        n += 1;
    }
    Ok(candidate)
}

// ===== 引用收集（白名单）=====

/// 回收判定事实的完整收集：三类存活实例 id + 被引用的完整仓库相对路径白名单。
///
/// 任一来源读不到或解析不了即整体返回 `Err`：这些集合是**删除白名单**，残缺的集合会把别处
/// 仍在用的附件判成孤儿（NAS/云盘上短暂占用、权限不可读都会命中），调用方必须放弃本次删除。
struct TempUsage {
    /// 存活画布 id（全仓 `.atlx` 内 `id` 字段）。
    canvas_ids: HashSet<String>,
    /// 存活面板会话 id（`.atelyx/对话历史/` 下 `.jsonl` 文件名）。
    session_ids: HashSet<String>,
    /// 存活表格 id（全仓 `.atb` 内 `id` 字段）。
    table_ids: HashSet<String>,
    /// 被引用文件的完整仓库相对路径（临时区引用 + 表格 image 单元格路径引用）。
    refs: HashSet<String>,
}

impl TempUsage {
    fn new() -> Self {
        Self {
            canvas_ids: HashSet::new(),
            session_ids: HashSet::new(),
            table_ids: HashSet::new(),
            refs: HashSet::new(),
        }
    }

    /// 白名单里是否有引用落在给定目录（仓库相对路径）之下。
    fn refs_under(&self, dir_rel: &str) -> bool {
        let prefix = format!("{dir_rel}/");
        self.refs.iter().any(|r| r.starts_with(&prefix))
    }
}

/// 深度遍历 JSON，把以 `<前缀目录>/` 开头的字符串值收进白名单（完整仓库相对路径）。
///
/// 按 JSON 字符串值整体判定：文件名允许含空格、中文与任何需要 JSON 转义的字面（`\t`、
/// `\uXXXX`），按文本切分会在转义处截断，把仍被引用的附件误判成孤儿而删除。
fn collect_prefixed_refs_in(value: &serde_json::Value, prefix_dir: &str, out: &mut HashSet<String>) {
    let prefix = format!("{prefix_dir}/");
    collect_strings_matching(value, &|s| s.starts_with(&prefix), out);
}

/// 深度遍历 JSON，把命中判定的字符串值收进集合。
fn collect_strings_matching(
    value: &serde_json::Value,
    matches: &impl Fn(&str) -> bool,
    out: &mut HashSet<String>,
) {
    match value {
        serde_json::Value::String(s) => {
            if matches(s) {
                out.insert(s.to_string());
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_strings_matching(item, matches, out);
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values() {
                collect_strings_matching(item, matches, out);
            }
        }
        _ => {}
    }
}

/// 收集 `.atb` 全部 image 单元格的路径引用（遗留 `data:` 条目非路径，不计入）。
fn collect_table_image_refs(value: &serde_json::Value, out: &mut HashSet<String>) {
    let image_field_ids: HashSet<&str> = value
        .get("fields")
        .and_then(|f| f.as_array())
        .map(|fields| {
            fields
                .iter()
                .filter(|f| f.get("type").and_then(|t| t.as_str()) == Some("image"))
                .filter_map(|f| f.get("id").and_then(|i| i.as_str()))
                .collect()
        })
        .unwrap_or_default();
    if image_field_ids.is_empty() {
        return;
    }
    let Some(rows) = value.get("rows").and_then(|r| r.as_array()) else {
        return;
    };
    for row in rows {
        let Some(values) = row.get("values").and_then(|v| v.as_object()) else {
            continue;
        };
        for (field_id, cell) in values {
            if !image_field_ids.contains(field_id.as_str()) {
                continue;
            }
            for item in image_cell_entries(cell) {
                if !item.starts_with("data:") {
                    out.insert(item.to_string());
                }
            }
        }
    }
}

/// 递归扫仓库实体文件（跳过隐藏目录）：`.atlx` → 画布 id + 临时区引用；`.atb` → 表格 id +
/// image 单元格引用。
///
/// 目录判定走 `entry.file_type()`（不跟随链接）：`Path::is_dir()` 在 stat 失败时会返回 false，
/// 于是「有 r 无 x」的子目录会被当成普通文件跳过、其中的引用全部漏掉；链接目录不下钻（防出仓与成环）。
/// 名字以扩展名结尾的**文件链接跟随读取**：画布/表格列表按扩展名收录链接实体（文件树同样当普通
/// 文件），白名单必须与「UI 可见实体集合」一致，否则链接实体的引用看不到、其临时件会被误删。
fn collect_entity_usage(dir: &Path, usage: &mut TempUsage) -> Result<(), String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("读取目录失败（{}）：{e}", dir.display()))?;
    for entry in rd {
        let entry = entry.map_err(|e| format!("读取目录项失败（{}）：{e}", dir.display()))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let file_type = entry
            .file_type()
            .map_err(|e| format!("读取目录项类型失败（{}）：{e}", path.display()))?;
        if file_type.is_dir() {
            // 只跳过隐藏目录（`.atelyx` 等）：实体不会放在其中，进去只会白扫
            if !name.starts_with('.') {
                collect_entity_usage(&path, usage)?;
            }
            continue;
        }
        let is_canvas = name.ends_with(".atlx");
        let is_table = name.ends_with(".atb");
        if !is_canvas && !is_table {
            continue;
        }
        // 普通文件与文件链接都可读；FIFO/socket/设备文件一律跳过（`read_to_string` 会挂死）
        if !file_type.is_file() && !file_type.is_symlink() {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("读取实体失败（{}）：{e}", path.display()))?;
        let value: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| format!("解析实体失败（{}）：{e}", path.display()))?;
        if let Some(id) = value.get("id").and_then(|i| i.as_str()) {
            if is_canvas {
                usage.canvas_ids.insert(id.to_string());
            } else {
                usage.table_ids.insert(id.to_string());
            }
        }
        if is_canvas {
            collect_prefixed_refs_in(&value, TEMP_ATTACHMENT_DIR, &mut usage.refs);
        } else {
            collect_table_image_refs(&value, &mut usage.refs);
        }
    }
    Ok(())
}

/// 扫 `.atelyx/对话历史/`，收集面板会话的回收判定事实：存活会话 id（`.jsonl` 文件名）+
/// 消息里引用的临时附件（逐行 JSON 解析，损坏行跳过——与前端会话恢复语义一致：坏行不阻塞
/// 其余引用的收集）。读目录/读文件失败返回 `Err`（调用方放弃本次删除，理由同白名单完整性）。
fn collect_chat_session_usage(root: &Path, usage: &mut TempUsage) -> Result<(), String> {
    let Ok(dir) = safe_join(root, CHAT_HISTORY_DIR, false) else {
        return Ok(());
    };
    if !dir.is_dir() {
        return Ok(());
    }
    let rd = std::fs::read_dir(&dir).map_err(|e| format!("读取对话历史失败：{e}"))?;
    for entry in rd {
        let entry = entry.map_err(|e| format!("读取对话历史目录项失败：{e}"))?;
        let file_type = entry
            .file_type()
            .map_err(|e| format!("读取对话历史目录项类型失败：{e}"))?;
        if !file_type.is_file() && !file_type.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(session_id) = name.strip_suffix(".jsonl") else {
            continue;
        };
        usage.session_ids.insert(session_id.to_string());
        let text = std::fs::read_to_string(entry.path())
            .map_err(|e| format!("读取会话失败（{name}）：{e}"))?;
        for line in text.lines() {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            collect_prefixed_refs_in(&value, TEMP_ATTACHMENT_DIR, &mut usage.refs);
        }
    }
    Ok(())
}

/// 回收判定事实的完整收集 = 实体文件（画布 + 表格）+ 面板会话。
fn collect_all_temp_usage(root: &Path) -> Result<TempUsage, String> {
    let mut usage = TempUsage::new();
    collect_entity_usage(root, &mut usage)?;
    collect_chat_session_usage(root, &mut usage)?;
    Ok(usage)
}

// ===== 按实例回收（清理命令）=====

/// 实例回收目标目录的类别：标记目录删除前须核对实例标记，无标记目录（目录名即实例 id）直接判定。
#[derive(Clone, Copy, PartialEq)]
enum OwnDirKind {
    /// 目录内有实例标记文件，删除前须与实例 id 一致（防同名/碰撞复用）。
    Marked(&'static str),
    /// 无标记（表格目录名即 tableId）。
    Unmarked,
}

/// 某实例的回收目标目录：(仓库相对路径, 类别)。
fn instance_own_dirs(component: TempComponent, instance_id: &str) -> Vec<(String, OwnDirKind)> {
    vec![(
        instance_temp_dir(component, instance_id),
        match component {
            TempComponent::Canvas | TempComponent::Session => OwnDirKind::Marked(INSTANCE_MARKER_FILE),
            TempComponent::Table => OwnDirKind::Unmarked,
        },
    )]
}

/// 目录标记是否与给定实例 id 一致（标记缺失 = 无法确认归属，按「不一致」处理）。
fn marker_matches(dir: &Path, marker_file: &str, instance_id: &str) -> bool {
    std::fs::read_to_string(dir.join(marker_file))
        .map(|s| s.trim() == instance_id)
        .unwrap_or(false)
}

/// 回收某组件某实例的临时附件：删实例目录内未被任何引用来源认账的顶层文件。
///
/// 保守规则（宁可留垃圾，不可删用户附件）：
/// 1. 全仓引用扫描失败（实体读不到/解析不了）→ 放弃本次删除（残缺白名单当完整集会误删）；
/// 2. 源实体文件不存在（已删除）→ 带标记的目录须标记与实例 id 一致才清理（防同名/碰撞复用）；
/// 3. 只删实例目录顶层普通文件，不递归。
pub(crate) fn cleanup_instance_temp_attachments(
    root: &Path,
    component: TempComponent,
    instance_id: &str,
    source_file: &str,
) -> Result<usize, String> {
    let own_dirs = instance_own_dirs(component, instance_id);
    // (仓库相对目录, 磁盘路径, 类别)
    let mut existing: Vec<(&str, PathBuf, OwnDirKind)> = Vec::new();
    for (dir_rel, kind) in &own_dirs {
        let Ok(dir) = safe_join(root, dir_rel, false) else {
            continue;
        };
        if dir.is_dir() {
            existing.push((dir_rel, dir, *kind));
        }
    }
    if existing.is_empty() {
        return Ok(0);
    }
    let source_path = safe_join(root, source_file, false)?;
    let source_missing = !source_path.is_file();
    let mut candidate_rels: Vec<String> = Vec::new();
    let mut candidate_paths: Vec<PathBuf> = Vec::new();
    for (dir_rel, dir, kind) in &existing {
        if source_missing {
            if let OwnDirKind::Marked(marker) = kind {
                if !marker_matches(dir, marker, instance_id) {
                    continue;
                }
            }
        }
        let rd = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            // 标记文件不是附件，永不入候选
            if name == INSTANCE_MARKER_FILE {
                continue;
            }
            if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                candidate_rels.push(format!("{dir_rel}/{name}"));
                candidate_paths.push(entry.path());
            }
        }
    }
    // 无候选（实例引用完整）时不扫全仓：清理的常态是「无事可做」，全仓扫描只在真要删东西时付
    if candidate_rels.is_empty() {
        return Ok(0);
    }
    let usage = collect_all_temp_usage(root)?;
    let mut removed = 0usize;
    for (rel, path) in candidate_rels.iter().zip(&candidate_paths) {
        if usage.refs.contains(rel) {
            continue;
        }
        if std::fs::remove_file(path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

/// 回收某组件某实例的临时附件（前端内容面统一入口）。
#[tauri::command]
pub async fn cleanup_temp_attachments(
    component: String,
    instance_id: String,
    source_file: String,
    state: State<'_, VaultState>,
) -> Result<usize, String> {
    let comp =
        TempComponent::parse(&component).ok_or_else(|| format!("未知临时区组件：{component}"))?;
    let root = state.root()?;
    cleanup_instance_temp_attachments(&root, comp, &instance_id, &source_file)
}

// ===== 兜底清扫（进仓预热）=====

/// 兜底回收：临时区里「归属实例已不存在」的整目录（崩溃/强杀遗留，实体未走正常关闭路径），
/// 以及表格图片旧落盘根下已清空的目录壳。
///
/// 由 `open_vault` 后的预热线程调用，失败静默（非关键路径）。临时区在仓库内，白名单天然只来自
/// 本仓库（不存在跨仓库误删）。临时区目录按所属组件判定，三道保守闸（宁可留垃圾，不可删用户附件）：
/// 1. 超龄（刚写入、实体尚未保存时目录已存在而引用尚未落盘）；
/// 2. 无任何引用（目录内文件被任一来源认账即保留）；
/// 3. 归属确认：画布/会话实例目录须标记命中存活实例 id（标记缺失一律保留——可能属于尚未落盘的
///    实体）；表格实例目录名即 tableId，直接与存活表格核对；temp 根下的平面哈希目录沿用旧标记
///    与存活画布/会话核对。
pub fn sweep_orphan_temp_dirs(root: &Path) -> usize {
    sweep_orphan_temp_dirs_with_max_age(root, ORPHAN_TEMP_MAX_AGE_SECS)
}

/// 同 `sweep_orphan_temp_dirs`，超龄阈值由参数给定（测试用 0 免去等待真实文件年龄）。
fn sweep_orphan_temp_dirs_with_max_age(root: &Path, max_age_secs: u64) -> usize {
    let Ok(temp) = safe_join(root, TEMP_ATTACHMENT_DIR, false) else {
        return 0;
    };
    if !temp.is_dir() {
        return 0;
    }
    let usage = match collect_all_temp_usage(root) {
        Ok(u) => u,
        Err(e) => {
            eprintln!("[vault] 临时附件兜底回收跳过（引用扫描失败）：{e}");
            return 0;
        }
    };
    let now = SystemTime::now();
    let Ok(rd) = std::fs::read_dir(&temp) else {
        return 0;
    };
    let mut removed = 0usize;
    for entry in rd.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if let Some(component) = TempComponent::from_dir_name(&name) {
            removed += sweep_component_dir(&path, component, &usage, now, max_age_secs);
        }
    }
    removed
}

/// 超龄与无引用两道闸：任一命中即保留（返回 Some），都通过（None）才进入归属判定。
fn orphan_dir_kept(
    dir_rel: &str,
    path: &Path,
    usage: &TempUsage,
    now: SystemTime,
    max_age_secs: u64,
) -> Option<&'static str> {
    let age_secs = std::fs::symlink_metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|m| now.duration_since(m).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if age_secs < max_age_secs {
        return Some("未超龄");
    }
    if usage.refs_under(dir_rel) {
        return Some("仍被引用");
    }
    None
}

/// 清扫一个组件目录下的实例目录，返回删除数。
fn sweep_component_dir(
    component_dir: &Path,
    component: TempComponent,
    usage: &TempUsage,
    now: SystemTime,
    max_age_secs: u64,
) -> usize {
    let Ok(rd) = std::fs::read_dir(component_dir) else {
        return 0;
    };
    let mut removed = 0usize;
    for entry in rd.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        let leaf = entry.file_name().to_string_lossy().into_owned();
        let dir_rel = format!("{}/{}/{}", TEMP_ATTACHMENT_DIR, component.dir_name(), leaf);
        if orphan_dir_kept(&dir_rel, &path, usage, now, max_age_secs).is_some() {
            continue;
        }
        let alive = match component {
            TempComponent::Table => usage.table_ids.contains(&leaf),
            TempComponent::Canvas | TempComponent::Session => {
                let marker = std::fs::read_to_string(path.join(INSTANCE_MARKER_FILE))
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                // 标记缺失 = 无法确认归属（可能属于尚未落盘的实体）：保留
                marker.is_empty()
                    || match component {
                        TempComponent::Canvas => usage.canvas_ids.contains(&marker),
                        _ => usage.session_ids.contains(&marker),
                    }
            }
        };
        if alive {
            continue;
        }
        if std::fs::remove_dir_all(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// 「保存到仓库」的结果：落位后的仓库相对路径（供画布 media 节点 `file` 引用）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentImportResult {
    pub file: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::test_support::TempDir;

    #[test]
    fn instance_temp_key_is_fixed_hex_and_separator_free() {
        // 64bit FNV-1a 的十六进制串：恒 16 字符、只含十六进制字符（不含任何路径语义）
        let key = instance_temp_key("some-canvas-id");
        assert_eq!(key.len(), 16);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
        // 恶意/畸形 id 同样只产出十六进制串：不会带任何路径语义
        for evil in ["../../etc", "C:\\Windows", "..", "a/b"] {
            let k = instance_temp_key(evil);
            assert_eq!(k.len(), 16);
            assert!(k.chars().all(|c| c.is_ascii_hexdigit()));
        }
        // 稳定性：同 id 恒同 key，不同 id 不同 key
        assert_eq!(instance_temp_key("x"), instance_temp_key("x"));
        assert_ne!(instance_temp_key("x"), instance_temp_key("y"));
    }

    #[test]
    fn component_dir_layout_is_stable() {
        // 组件目录名一旦定下即进入引用路径，不可改动（parse/from_dir_name/dir_name 三方一致）
        assert_eq!(TempComponent::Canvas.dir_name(), "canvas");
        assert_eq!(TempComponent::Session.dir_name(), "sessions");
        assert_eq!(TempComponent::Table.dir_name(), "tables");
        for (value, comp) in [
            ("canvas", TempComponent::Canvas),
            ("session", TempComponent::Session),
            ("table", TempComponent::Table),
        ] {
            assert_eq!(TempComponent::parse(value), Some(comp));
            assert_eq!(TempComponent::from_dir_name(comp.dir_name()), Some(comp));
        }
        assert_eq!(TempComponent::parse("unknown"), None);
        assert_eq!(TempComponent::from_dir_name("random-dir"), None);
        // 画布/会话实例目录 = 哈希；表格 = 原始 tableId
        assert_eq!(
            instance_temp_dir(TempComponent::Canvas, "cid"),
            format!(".atelyx/temp/canvas/{}", instance_temp_key("cid"))
        );
        assert_eq!(
            instance_temp_dir(TempComponent::Table, "tid"),
            ".atelyx/temp/tables/tid"
        );
    }

    #[test]
    fn validate_table_id_rejects_path_semantics() {
        assert!(validate_table_id("abc123XYZ-_").is_ok());
        for evil in ["", "a/b", "a\\b", "..", ".hidden", "a:b", "x y z", "表格id"] {
            assert!(validate_table_id(evil).is_err(), "应拒绝：{evil}");
        }
    }

    #[test]
    fn refs_keep_spaces_and_unicode() {
        let text = r#"{"nodes":[{"data":{"file":".atelyx/temp/c1/att-x-my photo.png"}},{"data":{"file":".atelyx/temp/canvas/c1/att-y-截图 2024.png"}},{"data":{"file":".atelyx/temp/c1/att-z-plain.png"}}]}"#;
        let mut usage = TempUsage::new();
        let value: serde_json::Value = serde_json::from_str(text).unwrap();
        collect_prefixed_refs_in(&value, TEMP_ATTACHMENT_DIR, &mut usage.refs);
        assert_eq!(usage.refs.len(), 3);
        assert!(usage.refs.contains(".atelyx/temp/c1/att-x-my photo.png"));
        assert!(usage.refs.contains(".atelyx/temp/canvas/c1/att-y-截图 2024.png"));
        assert!(usage.refs.contains(".atelyx/temp/c1/att-z-plain.png"));
    }

    #[test]
    fn refs_survive_json_escapes() {
        // 名字含控制字符（JSON 写作 `\t`）或非 ASCII（部分序列化器写作 `\uXXXX`）：
        // JSON 解码后字符串值整体进白名单，不会被转义截断
        let text = r#"{"a":".atelyx/temp/c1/att-x-a\tb.png","b":".atelyx/temp/c1/att-x-\u62a5\u544a.pdf"}"#;
        let mut usage = TempUsage::new();
        let value: serde_json::Value = serde_json::from_str(text).unwrap();
        collect_prefixed_refs_in(&value, TEMP_ATTACHMENT_DIR, &mut usage.refs);
        assert!(usage.refs.contains(".atelyx/temp/c1/att-x-a\tb.png"));
        assert!(usage.refs.contains(".atelyx/temp/c1/att-x-报告.pdf"));
    }

    #[test]
    fn refs_are_scoped_to_temp_dir() {
        // 仓库内普通附件路径不含临时区前缀，不得进白名单
        let text = r#"{"file":"附件/att-a.png"}"#;
        let mut usage = TempUsage::new();
        let value: serde_json::Value = serde_json::from_str(text).unwrap();
        collect_prefixed_refs_in(&value, TEMP_ATTACHMENT_DIR, &mut usage.refs);
        assert!(usage.refs.is_empty());
    }

    #[test]
    fn table_image_refs_skip_data_urls_and_non_image_fields() {
        let text = r#"{
            "id":"t1",
            "fields":[{"id":"f1","type":"image"},{"id":"f2","type":"text"}],
            "rows":[
                {"values":{"f1":{"images":[".atelyx/temp/tables/t1/img-a.png","data:image/png;base64,xxx"]},"f2":".atelyx/temp/canvas/c1/att-x.png"}},
                {"values":{"f1":[".atelyx/temp/tables/t1/img-b.webp"]}}
            ]
        }"#;
        let mut usage = TempUsage::new();
        let value: serde_json::Value = serde_json::from_str(text).unwrap();
        collect_table_image_refs(&value, &mut usage.refs);
        assert_eq!(usage.refs.len(), 2);
        assert!(usage.refs.contains(".atelyx/temp/tables/t1/img-a.png"));
        assert!(usage.refs.contains(".atelyx/temp/tables/t1/img-b.webp"));
    }

    #[test]
    fn entity_usage_collects_ids_and_refs_across_nested_dirs() {
        let root = TempDir::new("temp-usage");
        std::fs::create_dir_all(root.join("项目A")).unwrap();
        let key_a = instance_temp_key("canvas-a");
        let key_b = instance_temp_key("canvas-b");
        // 项目A 的画布引用了自己目录的文件，同时粘贴过来的副本引用了 canvas-b 的文件
        std::fs::write(
            root.join("项目A/图.atlx"),
            format!(
                r#"{{"id":"canvas-a","nodes":[{{"file":".atelyx/temp/canvas/{key_a}/att-1.png"}},{{"file":".atelyx/temp/canvas/{key_b}/att-2.png"}}]}}"#
            ),
        )
        .unwrap();
        std::fs::write(
            root.join("项目A/表.atb"),
            r#"{"id":"table-a","fields":[{"id":"f1","type":"image"}],"rows":[{"values":{"f1":{"images":[".atelyx/temp/tables/table-a/img-1.png"]}}}]}"#,
        )
        .unwrap();

        let mut usage = TempUsage::new();
        collect_entity_usage(&root, &mut usage).unwrap();

        assert!(usage.canvas_ids.contains("canvas-a"));
        assert!(usage.table_ids.contains("table-a"));
        assert!(usage.refs.contains(&format!(".atelyx/temp/canvas/{key_a}/att-1.png")));
        assert!(usage.refs.contains(&format!(".atelyx/temp/canvas/{key_b}/att-2.png")));
        assert!(usage.refs.contains(".atelyx/temp/tables/table-a/img-1.png"));
    }

    #[test]
    fn entity_usage_reports_failure_so_callers_skip_deletion() {
        // 仓库里有一个解析不了的实体：不能只跳过它（残缺的引用集当完整白名单会误删附件）
        for (name, body) in [("坏.atlx", "{ 坏 JSON"), ("坏表.atb", "{ 坏 JSON")] {
            let root = TempDir::new("temp-usage-broken");
            std::fs::write(root.join(name), body).unwrap();
            let mut usage = TempUsage::new();
            assert!(collect_entity_usage(&root, &mut usage).is_err());
        }
    }

    #[test]
    fn session_usage_collects_session_ids_and_refs() {
        let root = TempDir::new("session-usage");
        let dir = root.join(".atelyx/对话历史");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("session-a.jsonl"),
            concat!(
                r#"{"attachments":[{"file":".atelyx/temp/sessions/x/att-1.png"}]}"#,
                "\n",
                "{ 损坏行\n",
                r#"{"attachments":[{"file":".atelyx/temp/sessions/x/att-2.png"}]}"#,
                "\n",
            ),
        )
        .unwrap();

        let mut usage = TempUsage::new();
        collect_chat_session_usage(&root, &mut usage).unwrap();

        assert!(usage.session_ids.contains("session-a"));
        assert!(usage.refs.contains(".atelyx/temp/sessions/x/att-1.png"));
        assert!(usage.refs.contains(".atelyx/temp/sessions/x/att-2.png"));
    }

    #[test]
    fn cleanup_canvas_keeps_cross_canvas_refs() {
        let root = TempDir::new("cleanup-canvas");
        let key_a = instance_temp_key("canvas-a");
        // canvas-a 自己的画布引用本目录文件；另一画布（粘贴副本）引用同一目录的另一文件
        let dir = root.join(&format!(".atelyx/temp/canvas/{key_a}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(root.join("图.atlx"), format!(r#"{{"id":"canvas-a","nodes":[{{"file":".atelyx/temp/canvas/{key_a}/att-live.png"}}]}}"#)).unwrap();
        std::fs::write(root.join("他.atlx"), format!(r#"{{"id":"canvas-b","nodes":[{{"file":".atelyx/temp/canvas/{key_a}/att-elsewhere.png"}}]}}"#)).unwrap();
        for name in ["att-live.png", "att-elsewhere.png", "att-orphan.png"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }

        let removed = cleanup_instance_temp_attachments(
            &root,
            TempComponent::Canvas,
            "canvas-a",
            "图.atlx",
        )
        .unwrap();
        // 只删孤儿：被本画布与别画布引用的文件都保留
        assert_eq!(removed, 1);
        assert!(dir.join("att-live.png").is_file());
        assert!(dir.join("att-elsewhere.png").is_file());
        assert!(!dir.join("att-orphan.png").exists());
    }

    #[test]
    fn cleanup_deleted_canvas_requires_matching_marker() {
        let root = TempDir::new("cleanup-deleted-canvas");
        let key = instance_temp_key("canvas-gone");
        let dir = root.join(&format!(".atelyx/temp/canvas/{key}"));
        std::fs::create_dir_all(&dir).unwrap();
        // 标记与实例 id 一致 + 画布文件已不存在：可清
        std::fs::write(dir.join(".instance-id"), "canvas-gone").unwrap();
        std::fs::write(dir.join("att-a.png"), b"x").unwrap();
        let removed =
            cleanup_instance_temp_attachments(&root, TempComponent::Canvas, "canvas-gone", "无.atlx")
                .unwrap();
        assert_eq!(removed, 1);
        // 标记不一致（他人目录/碰撞复用）：不删
        let dir2 = root.join(&format!(".atelyx/temp/canvas/{}", instance_temp_key("canvas-other")));
        std::fs::create_dir_all(&dir2).unwrap();
        std::fs::write(dir2.join(".instance-id"), "canvas-someone-else").unwrap();
        std::fs::write(dir2.join("att-b.png"), b"x").unwrap();
        let removed = cleanup_instance_temp_attachments(
            &root,
            TempComponent::Canvas,
            "canvas-other",
            "无.atlx",
        )
        .unwrap();
        assert_eq!(removed, 0);
        assert!(dir2.join("att-b.png").is_file());
    }

    #[test]
    fn cleanup_session_reads_jsonl_refs() {
        let root = TempDir::new("cleanup-session");
        let key = instance_temp_key("session-1");
        let dir = root.join(&format!(".atelyx/temp/sessions/{key}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".instance-id"), "session-1").unwrap();
        std::fs::write(dir.join("att-live.png"), b"x").unwrap();
        std::fs::write(dir.join("att-orphan.png"), b"x").unwrap();
        std::fs::create_dir_all(root.join(".atelyx/对话历史")).unwrap();
        std::fs::write(
            root.join(".atelyx/对话历史/session-1.jsonl"),
            r#"{"attachments":[{"file":".atelyx/temp/sessions/SESSIONKEY/att-live.png"}]}"#
                .replace("SESSIONKEY", &key),
        )
        .unwrap();

        let removed = cleanup_instance_temp_attachments(
            &root,
            TempComponent::Session,
            "session-1",
            ".atelyx/对话历史/session-1.jsonl",
        )
        .unwrap();
        assert_eq!(removed, 1);
        assert!(dir.join("att-live.png").is_file());
        assert!(!dir.join("att-orphan.png").exists());
    }

    #[test]
    fn cleanup_table_deletes_unreferenced_only() {
        let root = TempDir::new("cleanup-table");
        let dir = root.join(".atelyx/temp/tables/t1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("img-live.png"), b"x").unwrap();
        std::fs::write(dir.join("img-orphan.png"), b"x").unwrap();
        std::fs::write(
            root.join("表.atb"),
            r#"{"id":"t1","fields":[{"id":"f1","type":"image"}],"rows":[{"values":{"f1":{"images":[".atelyx/temp/tables/t1/img-live.png"]}}}]}"#,
        )
        .unwrap();

        let removed =
            cleanup_instance_temp_attachments(&root, TempComponent::Table, "t1", "表.atb").unwrap();
        assert_eq!(removed, 1);
        assert!(dir.join("img-live.png").is_file());
        assert!(!dir.join("img-orphan.png").exists());
    }

    #[test]
    fn cleanup_aborts_when_entity_scan_fails() {
        // 任一实体解析失败 = 白名单残缺：放弃整次删除（Err 上抛，调用方按 0 删除处理）
        let root = TempDir::new("cleanup-abort");
        let key = instance_temp_key("canvas-a");
        let dir = root.join(&format!(".atelyx/temp/canvas/{key}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("att-a.png"), b"x").unwrap();
        std::fs::write(root.join("图.atlx"), r#"{"id":"canvas-a"}"#).unwrap();
        std::fs::write(root.join("坏.atlx"), "{ 坏 JSON").unwrap();
        let result =
            cleanup_instance_temp_attachments(&root, TempComponent::Canvas, "canvas-a", "图.atlx");
        assert!(result.is_err());
        assert!(dir.join("att-a.png").is_file());
    }

    #[test]
    fn sweep_removes_orphan_component_dirs_and_keeps_live_or_referenced() {
        let root = TempDir::new("sweep-components");
        let live_key = instance_temp_key("canvas-live");
        let dead_key = instance_temp_key("canvas-dead");
        let session_key = instance_temp_key("session-live");
        // 存活画布（.atlx 在）+ 其新目录（标记一致）
        let live_dir = root.join(&format!(".atelyx/temp/canvas/{live_key}"));
        std::fs::create_dir_all(&live_dir).unwrap();
        std::fs::write(
            live_dir.join(".instance-id"),
            b"canvas-live",
        )
        .unwrap();
        std::fs::write(root.join("图.atlx"), r#"{"id":"canvas-live"}"#).unwrap();
        // 已删画布的超龄目录（标记在、引用无）
        let dead_dir = root.join(&format!(".atelyx/temp/canvas/{dead_key}"));
        std::fs::create_dir_all(&dead_dir).unwrap();
        std::fs::write(dead_dir.join(".instance-id"), b"canvas-dead").unwrap();
        // 存活会话目录 + 已删会话目录
        let s_live = root.join(&format!(".atelyx/temp/sessions/{session_key}"));
        std::fs::create_dir_all(&s_live).unwrap();
        std::fs::write(s_live.join(".instance-id"), b"session-live").unwrap();
        std::fs::create_dir_all(root.join(".atelyx/对话历史")).unwrap();
        std::fs::write(root.join(".atelyx/对话历史/session-live.jsonl"), "{}").unwrap();
        let s_dead = root.join(&format!(".atelyx/temp/sessions/{}", instance_temp_key("session-dead")));
        std::fs::create_dir_all(&s_dead).unwrap();
        std::fs::write(s_dead.join(".instance-id"), b"session-dead").unwrap();
        // 标记缺失目录：一律保留
        let no_marker = root.join(&format!(".atelyx/temp/canvas/{}", instance_temp_key("canvas-nomarker")));
        std::fs::create_dir_all(&no_marker).unwrap();

        let removed = sweep_orphan_temp_dirs_with_max_age(&root, 0);
        assert_eq!(removed, 2);
        assert!(!dead_dir.exists());
        assert!(!s_dead.exists());
        assert!(root.join(&format!(".atelyx/temp/canvas/{live_key}")).is_dir());
        assert!(s_live.is_dir());
        assert!(no_marker.is_dir());
    }

    #[test]
    fn sweep_keeps_young_or_referenced_dirs() {
        let root = TempDir::new("sweep-keep");
        // 超龄闸：max_age 极大时未超龄目录保留
        let dir = root.join(&format!(".atelyx/temp/tables/t-dead"));
        std::fs::create_dir_all(&dir).unwrap();
        let removed = sweep_orphan_temp_dirs_with_max_age(&root, ORPHAN_TEMP_MAX_AGE_SECS);
        assert_eq!(removed, 0);
        assert!(dir.is_dir());
        // 引用闸：表格已不在，但 .atb 引用仍认账（跨实体引用场景）
        std::fs::write(
            root.join("他表.atb"),
            r#"{"id":"t-other","fields":[{"id":"f1","type":"image"}],"rows":[{"values":{"f1":{"images":[".atelyx/temp/tables/t-dead/img-x.png"]}}}]}"#,
        )
        .unwrap();
        let removed = sweep_orphan_temp_dirs_with_max_age(&root, 0);
        assert_eq!(removed, 0);
        assert!(dir.is_dir());
    }

    #[test]
    fn sweep_ignores_non_component_dirs_under_temp_root() {
        // temp 根下非组件目录（外来/未知布局）一律不碰
        let root = TempDir::new("sweep-ignore-unknown");
        let unknown = root.join(".atelyx/temp/随机目录");
        std::fs::create_dir_all(&unknown).unwrap();
        std::fs::write(unknown.join("x.png"), b"x").unwrap();
        let removed = sweep_orphan_temp_dirs_with_max_age(&root, 0);
        assert_eq!(removed, 0);
        assert!(unknown.join("x.png").is_file());
    }

    #[test]
    fn sanitize_temp_file_name_keeps_leaf_only() {
        assert_eq!(sanitize_temp_file_name("C:\\Users\\a\\图 1.png"), "图 1.png");
        assert_eq!(sanitize_temp_file_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_temp_file_name(""), "attachment");
        assert_eq!(sanitize_temp_file_name(".."), "attachment");
        assert_eq!(sanitize_temp_file_name("a:b*c.png"), "a_b_c.png");
        // 空格与中文原样保留（回收扫描按 JSON 值整体判定，不依赖空白）
        assert_eq!(sanitize_temp_file_name("my photo.png"), "my photo.png");
    }
}
