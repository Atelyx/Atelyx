//! 附件临时区命令：未入库附件先落仓库内隐藏目录，画布只存路径引用。
//!
//! 对话节点粘贴/拖入的附件**不再内嵌进 `.atlx`**（base64 图片会让画布文件涨到几十 MB），
//! 而是落进 `<仓库根>/.atelyx/temp/<canvasKey>/`，画布只存**仓库相对路径**引用：
//! - 隐藏目录：与表格图片（`.atelyx/attachments/<tableId>/`）同一族，文件树与 watcher 天然跳过；
//! - 一画布一目录（canvasKey = 画布 id 的稳定派生）：回收以该画布自己的 `.atlx` 引用为主；
//!   节点复制粘贴可把引用带到别的画布（副本仍指向原目录），故删除前还要确认仓库内无其它画布引用；
//! - **放在仓库内**（而非应用数据目录）：路径校验直接复用 `safe_join`（越界一律拒绝），
//!   回收天然按仓库归属，读取复用仓库附件读命令——三件事都不需要另造一套边界；
//! - 未入库的内容会随 `.atelyx` 一起被 Git/云盘同步（与表格附件同口径，不额外忽略）。
//!
//! 「保存到仓库」= 把临时件复制进附件文件夹并换成普通仓库相对路径引用；临时目录的回收
//! 由画布关闭/删除时的按引用清理负责（见 `cleanup_canvas_temp_attachments`）。

use std::path::{Path, PathBuf};

use base64::Engine;
use serde::Serialize;
use tauri::State;

use crate::vault::{atomic_write, safe_join, VaultState};

/// 未入库临时附件的目录（相对仓库根）。
pub const TEMP_ATTACHMENT_DIR: &str = ".atelyx/temp";

/// 删除某画布临时目录前的保护标记：防「画布文档恰好暂时读不到」时把整目录清空。
const TEMP_MARKER_FILE: &str = ".canvas-id";

/// 画布 id → 临时目录名：稳定、定长、无路径语义。
///
/// 为什么不直接用画布 id：它来自 `.atlx` 文件内容（可被外部构造/同步），直接拼进路径就得再写一套
/// 穿越校验；派生成立即排除分隔符/`..`/保留名的十六进制串，`safe_join` 之外不再需要额外校验。
/// 碰撞由 `TEMP_MARKER_FILE` 兜底：目录标记与当前画布 id 不一致即视为他人目录，不删。
pub fn canvas_temp_key(canvas_id: &str) -> String {
    // FNV-1a（64bit）：不需要加密强度，只要稳定且分布均匀
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in canvas_id.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// 某画布的临时附件目录（相对仓库根）。
fn canvas_temp_dir(canvas_id: &str) -> String {
    format!("{TEMP_ATTACHMENT_DIR}/{}", canvas_temp_key(canvas_id))
}

/// 首次写入该画布目录时落标记（幂等；失败不阻断写入——标记只用于「不敢删」的保守判断）。
fn ensure_canvas_marker(root: &Path, canvas_id: &str) {
    let Ok(marker) = safe_join(root, &format!("{}/{}", canvas_temp_dir(canvas_id), TEMP_MARKER_FILE), true)
    else {
        return;
    };
    if marker.exists() {
        return;
    }
    let _ = atomic_write(&marker, canvas_id);
}

/// 把附件字节写入临时区，返回仓库相对路径引用。前端粘贴/拖入附件时调用。
///
/// 走 base64 文本而不是字节数组：附件可达数十 MB，base64 让载荷形状在 IPC 两端只有一种解释
/// （JSON 字符串），不依赖字节数组的序列化细节。
#[tauri::command]
pub fn write_temp_attachment(
    canvas_id: String,
    file_name: String,
    base64_data: String,
    state: State<'_, VaultState>,
) -> Result<String, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64_data.as_bytes())
        .map_err(|e| format!("附件数据解码失败：{e}"))?;
    let root = state.root()?;
    let leaf = format!("att-{}-{}", nanoid::nanoid!(), sanitize_temp_file_name(&file_name));
    let rel = format!("{}/{}", canvas_temp_dir(&canvas_id), leaf);
    let path = safe_join(&root, &rel, true)?;
    // 唯一名不可能已存在，但仍走原子写：写入完整性优先（与仓库其它写盘同一语义）
    ensure_canvas_marker(&root, &canvas_id);
    atomic_write_bytes(&path, &bytes)?;
    Ok(rel)
}

/// 原子写二进制（`atomic_write` 面向文本；附件字节走同一套 tmp → rename + fsync 语义）。
fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = PathBuf::from(format!("{}.{}.tmp", path.display(), nanoid::nanoid!()));
    if let Some(parent) = tmp.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建附件目录失败：{e}"))?;
    }
    let written = std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, path));
    match written {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(format!("写入临时附件失败：{e}"))
        }
    }
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
    // 空格/中文等原样保留：引用扫描按 JSON 字符串边界切分，不靠空白判定
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

/// 回收某画布的临时附件：只删「仓库内任何画布都不引用」的文件。
///
/// 四条保守规则（宁可留垃圾，不可删用户附件）：
/// 1. 画布文档**读不到**时返回 0 不清理——引用集合未知，清即可能误删仍在用的附件
///    （切仓库/路径在途时也可能读不到，不能据此认定画布已删）；
/// 2. 画布已删（文档确实不存在）时也只删「标记与本次画布 id 一致」的目录；
/// 3. 只删顶层普通文件，不递归；
/// 4. 删除前扫全仓库其余画布：临时引用会随节点复制粘贴跨画布（副本仍指向本目录），
///    只看本画布引用会把别处仍在用的文件删掉——该扫描是「读全部画布」重活，故命令为 async
///    （Tauri 在异步运行时执行，不占主线程），且只在真有候选文件时才付这份代价。
#[tauri::command]
pub async fn cleanup_canvas_temp_attachments(
    canvas_id: String,
    canvas_file: String,
    state: State<'_, VaultState>,
) -> Result<usize, String> {
    let root = state.root()?;
    // 仓库从未写过临时附件时 `.atelyx/temp`（乃至该画布目录）不存在：无事可回收，不是错误
    let Ok(dir) = safe_join(&root, &canvas_temp_dir(&canvas_id), false) else {
        return Ok(0);
    };
    if !dir.is_dir() {
        return Ok(0);
    }
    let key = canvas_temp_key(&canvas_id);
    let canvas_path = safe_join(&root, &canvas_file, false)?;
    let referenced: std::collections::HashSet<String> = if canvas_path.is_file() {
        let text = std::fs::read_to_string(&canvas_path)
            .map_err(|e| format!("读取画布失败，未回收临时附件：{e}"))?;
        // 画布解析不了 = 引用集合未知：一律不清理（与「读不到画布」同口径）
        let Some(refs) = referenced_temp_refs(&text) else {
            return Ok(0);
        };
        refs.into_iter()
            .filter(|(k, _)| *k == key)
            .map(|(_, name)| name)
            .collect()
    } else {
        // 画布已删除：目录里确实没有该画布的引用了；但仍要认标记，防目录被同名/碰撞复用
        if !marker_matches(&dir, &canvas_id) {
            return Ok(0);
        }
        std::collections::HashSet::new()
    };
    let mut candidates: Vec<(String, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(&dir).map_err(|e| e.to_string())?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == TEMP_MARKER_FILE || referenced.contains(&name) {
            continue;
        }
        if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            candidates.push((name, entry.path()));
        }
    }
    // 无候选（本画布引用完整）时不扫仓库：清理的常态是「无事可做」，全仓扫描只在真要删东西时付
    if candidates.is_empty() {
        return Ok(0);
    }
    let mut live = std::collections::HashMap::new();
    let mut ids = std::collections::HashSet::new();
    // 扫描失败 = 引用集合残缺：残缺集合当成完整白名单会误删别处仍在用的附件，故放弃本次删除
    collect_temp_usage(&root, &mut ids, &mut live)?;
    let elsewhere = live.remove(&key).unwrap_or_default();
    let mut removed = 0usize;
    for (name, path) in candidates {
        if elsewhere.contains(&name) {
            continue;
        }
        if std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

/// 目录标记是否与给定画布 id 一致（标记缺失 = 无法确认归属，按「不一致」处理）。
fn marker_matches(dir: &Path, canvas_id: &str) -> bool {
    std::fs::read_to_string(dir.join(TEMP_MARKER_FILE))
        .map(|s| s.trim() == canvas_id)
        .unwrap_or(false)
}

/// 画布文本里引用的临时附件，逐条为 `(canvasKey, 文件名)`；文本不是合法 JSON 时返回 `None`。
///
/// 按 JSON 遍历字符串值取「整个值等于 `.atelyx/temp/<key>/<文件名>`」的引用：文件名允许含空格、
/// 中文与任何需要 JSON 转义的字面（`\t`、`\uXXXX`），按文本切分会在转义处截断，把仍被引用的
/// 附件误判成孤儿而删除。
fn referenced_temp_refs(canvas_text: &str) -> Option<Vec<(String, String)>> {
    let value: serde_json::Value = serde_json::from_str(canvas_text).ok()?;
    let mut out = Vec::new();
    collect_temp_refs_in(&value, &format!("{TEMP_ATTACHMENT_DIR}/"), &mut out);
    Some(out)
}

/// 深度遍历 JSON 收集临时附件引用（对象/数组全下钻；字符串值整体等于引用才算）。
fn collect_temp_refs_in(
    value: &serde_json::Value,
    prefix: &str,
    out: &mut Vec<(String, String)>,
) {
    match value {
        serde_json::Value::String(s) => {
            let Some(rest) = s.strip_prefix(prefix) else {
                return;
            };
            let Some((key, name)) = rest.split_once('/') else {
                return;
            };
            if !key.is_empty() && !name.is_empty() {
                out.push((key.to_string(), name.to_string()));
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_temp_refs_in(item, prefix, out);
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values() {
                collect_temp_refs_in(item, prefix, out);
            }
        }
        _ => {}
    }
}

/// 兜底回收：临时区里「画布已不存在」的整目录（崩溃/强杀遗留，画布未走正常关闭路径）。
///
/// 临时区在仓库内，白名单天然只来自本仓库（不存在跨仓库误删）；保留标记、引用与年龄三道保守闸：
/// 标记缺失或与目录名不符一律保留，被任一存活画布引用（节点复制粘贴可跨画布）也保留，
/// 且只清超龄目录（刚粘贴、画布尚未保存时目录已存在而 `.atlx` 里还没有引用）。
/// 由 `open_vault` 后的预热线程调用，失败静默（非关键路径）。
pub fn sweep_orphan_temp_dirs(root: &Path) -> usize {
    let Ok(temp) = safe_join(root, TEMP_ATTACHMENT_DIR, false) else {
        return 0;
    };
    if !temp.is_dir() {
        return 0;
    }
    let mut canvas_ids = std::collections::HashSet::new();
    let mut refs = std::collections::HashMap::new();
    // 扫描失败 = 引用集合残缺：整目录删除的风险更高（可能删掉别处仍在用的附件），本次跳过
    if let Err(e) = collect_temp_usage(root, &mut canvas_ids, &mut refs) {
        eprintln!("[vault] 临时附件兜底回收跳过（引用扫描失败）：{e}");
        return 0;
    }
    let now = std::time::SystemTime::now();
    let mut removed = 0usize;
    let Ok(rd) = std::fs::read_dir(&temp) else {
        return 0;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let key = entry.file_name().to_string_lossy().into_owned();
        // 标记里的画布 id 是本仓库仍在使用的凭据：任一存活画布的 key 命中即保留
        let marker_id = std::fs::read_to_string(path.join(TEMP_MARKER_FILE)).unwrap_or_default();
        let marker_id = marker_id.trim();
        if !marker_id.is_empty() && canvas_ids.contains(&canvas_temp_key(marker_id)) {
            continue;
        }
        // 归属画布已不在，但别的画布仍在引用（节点复制粘贴带来的跨画布引用）：删了就断链
        if refs.get(&key).is_some_and(|names| !names.is_empty()) {
            continue;
        }
        let age_secs = std::fs::symlink_metadata(&path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|m| now.duration_since(m).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if age_secs < ORPHAN_TEMP_MAX_AGE_SECS {
            continue;
        }
        // 标记缺失/无法解析（可能属于尚未落盘的画布）：只清超龄且目录名不是任何已知 key 的目录
        if marker_id.is_empty() {
            continue;
        }
        if std::fs::remove_dir_all(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// 递归扫仓库内全部 `.atlx`，收集两件回收判定用的事实：
/// - `ids`：画布 id 的临时目录 key（判断某目录的归属画布是否还在）；
/// - `refs`：临时目录 key → 被引用的文件名集合（判断某目录是否仍被任一画布引用）。
///
/// 任一目录项/文件读不到或解析不了即返回 `Err`：这些集合是**删除白名单**，残缺的集合会把别处仍在用的
/// 附件判成孤儿（NAS/云盘上短暂占用、权限不可读都会命中），故调用方必须放弃本次删除。
/// 目录判定走 `entry.file_type()`（不跟随链接）：`Path::is_dir()` 在 stat 失败时会返回 false，
/// 于是「有 r 无 x」的子目录会被当成普通文件跳过、其中的引用全部漏掉；链接目录不下钻（防出仓与成环）。
/// 名字以 `.atlx` 结尾的**文件链接跟随读取**：画布列表按扩展名收录链接画布（文件树同样当普通文件），
/// 白名单必须与「UI 可见画布集合」一致，否则链接画布的引用看不到、其临时件会被误删。
fn collect_temp_usage(
    dir: &Path,
    ids: &mut std::collections::HashSet<String>,
    refs: &mut std::collections::HashMap<String, std::collections::HashSet<String>>,
) -> Result<(), String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("读取目录失败（{}）：{e}", dir.display()))?;
    for entry in rd {
        let entry = entry.map_err(|e| format!("读取目录项失败（{}）：{e}", dir.display()))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let file_type = entry
            .file_type()
            .map_err(|e| format!("读取目录项类型失败（{}）：{e}", path.display()))?;
        if file_type.is_dir() {
            // 只跳过隐藏目录（`.atelyx` 等）：画布不会放在其中，进去只会白扫
            if !name.starts_with('.') {
                collect_temp_usage(&path, ids, refs)?;
            }
            continue;
        }
        if !name.ends_with(".atlx") {
            continue;
        }
        // 普通文件与文件链接都可读；FIFO/socket/设备文件一律跳过（`read_to_string` 会挂死）
        if !file_type.is_file() && !file_type.is_symlink() {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("读取画布失败（{}）：{e}", path.display()))?;
        let value: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| format!("解析画布失败（{}）：{e}", path.display()))?;
        if let Some(id) = value.get("id").and_then(|i| i.as_str()) {
            ids.insert(canvas_temp_key(id));
        }
        let mut found = Vec::new();
        collect_temp_refs_in(&value, &format!("{TEMP_ATTACHMENT_DIR}/"), &mut found);
        for (key, file) in found {
            refs.entry(key).or_default().insert(file);
        }
    }
    Ok(())
}

/// 孤儿目录最大存活时长（秒）：未入库附件在仓库内，超龄且画布确实不在才回收。
const ORPHAN_TEMP_MAX_AGE_SECS: u64 = 24 * 60 * 60;

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
    fn canvas_temp_key_is_fixed_hex_and_separator_free() {
        // 64bit FNV-1a 的十六进制串：恒 16 字符、只含十六进制字符（不含任何路径语义）
        let key = canvas_temp_key("some-canvas-id");
        assert_eq!(key.len(), 16);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
        // 恶意/畸形 id 同样只产出十六进制串：不会带任何路径语义
        for evil in ["../../etc", "C:\\Windows", "..", "a/b"] {
            let k = canvas_temp_key(evil);
            assert_eq!(k.len(), 16);
            assert!(k.chars().all(|c| c.is_ascii_hexdigit()));
        }
        // 稳定性：同 id 恒同 key，不同 id 不同 key
        assert_eq!(canvas_temp_key("x"), canvas_temp_key("x"));
        assert_ne!(canvas_temp_key("x"), canvas_temp_key("y"));
    }

    #[test]
    fn referenced_names_keep_spaces_and_unicode() {
        let text = r#"{"nodes":[{"data":{"file":".atelyx/temp/c1/att-x-my photo.png"}},{"data":{"file":".atelyx/temp/c1/att-y-截图 2024.png"}},{"data":{"file":".atelyx/temp/c1/att-z-plain.png"}}]}"#;
        let mut names: Vec<String> = referenced_temp_refs(text)
            .unwrap()
            .into_iter()
            .filter(|(key, _)| key == "c1")
            .map(|(_, name)| name)
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "att-x-my photo.png".to_string(),
                "att-y-截图 2024.png".to_string(),
                "att-z-plain.png".to_string(),
            ]
        );
    }

    #[test]
    fn referenced_names_survive_json_escapes() {
        // 名字含控制字符（JSON 写作 `\t`）或非 ASCII（部分序列化器写作 `\uXXXX`）：
        // 按文本切分会在转义处截断，把真实名字（`att-x-a\tb.png`）记成前缀而误判孤儿
        let text = r#"{"a":".atelyx/temp/c1/att-x-a\tb.png","b":".atelyx/temp/c1/att-x-\u62a5\u544a.pdf"}"#;
        let mut names: Vec<String> = referenced_temp_refs(text)
            .unwrap()
            .into_iter()
            .map(|(_, name)| name)
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec!["att-x-a\tb.png".to_string(), "att-x-报告.pdf".to_string()]
        );
    }

    #[test]
    fn referenced_names_are_scoped_to_temp_dir() {
        // 仓库内普通附件路径不含临时区前缀，不得被当成临时引用
        let text = r#"{"file":"附件/att-a.png"}"#;
        assert!(referenced_temp_refs(text).unwrap().is_empty());
    }

    #[test]
    fn referenced_names_reject_non_json() {
        // 解析不了 = 引用集合未知：返回 None 让调用方放弃清理，不得当成「无引用」
        assert!(referenced_temp_refs("{ 坏 JSON").is_none());
    }

    #[test]
    fn referenced_names_carry_canvas_key() {
        // 引用按画布目录分组：跨画布引用（节点复制粘贴）必须能与本画布引用区分开
        let text = r#"{"a":".atelyx/temp/c1/att-a.png","b":".atelyx/temp/c2/att-b.png"}"#;
        let mut refs = referenced_temp_refs(text).unwrap();
        refs.sort();
        assert_eq!(
            refs,
            vec![
                ("c1".to_string(), "att-a.png".to_string()),
                ("c2".to_string(), "att-b.png".to_string()),
            ]
        );
    }

    #[test]
    fn temp_usage_collects_ids_and_references_across_nested_dirs() {
        let root = TempDir::new("temp-usage");
        std::fs::create_dir_all(root.join("项目A")).unwrap();
        let key_a = canvas_temp_key("canvas-a");
        let key_b = canvas_temp_key("canvas-b");
        // 项目A 的画布引用了自己目录的文件，同时粘贴过来的副本引用了 canvas-b 的文件
        std::fs::write(
            root.join("项目A/图.atlx"),
            format!(
                r#"{{"id":"canvas-a","nodes":[{{"file":".atelyx/temp/{key_a}/att-1.png"}},{{"file":".atelyx/temp/{key_b}/att-2.png"}}]}}"#
            ),
        )
        .unwrap();

        let mut ids = std::collections::HashSet::new();
        let mut refs = std::collections::HashMap::new();
        collect_temp_usage(&root, &mut ids, &mut refs).unwrap();

        assert_eq!(ids.iter().cloned().collect::<Vec<_>>(), vec![key_a.clone()]);
        assert!(refs[&key_a].contains("att-1.png"));
        assert!(refs[&key_b].contains("att-2.png"));
    }

    #[test]
    fn temp_usage_reports_failure_so_callers_skip_deletion() {
        // 仓库里有一个解析不了的 `.atlx`：不能只跳过它（残缺的引用集当完整白名单会误删附件）
        let root = TempDir::new("temp-usage-broken");
        std::fs::write(root.join("坏.atlx"), "{ 坏 JSON").unwrap();

        let mut ids = std::collections::HashSet::new();
        let mut refs = std::collections::HashMap::new();
        assert!(collect_temp_usage(&root, &mut ids, &mut refs).is_err());
    }

    #[test]
    fn sanitize_temp_file_name_keeps_leaf_only() {
        assert_eq!(sanitize_temp_file_name("C:\\Users\\a\\图 1.png"), "图 1.png");
        assert_eq!(sanitize_temp_file_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_temp_file_name(""), "attachment");
        assert_eq!(sanitize_temp_file_name(".."), "attachment");
        assert_eq!(sanitize_temp_file_name("a:b*c.png"), "a_b_c.png");
        // 空格与中文原样保留（回收扫描按 JSON 值取引用，不依赖空白）
        assert_eq!(sanitize_temp_file_name("my photo.png"), "my photo.png");
    }
}
