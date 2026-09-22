//! 空间文件历史（版本侧文件）：与客户端 `services/history` 同一套 schema、同一套落点约定。
//!
//! 落点 = 空间内容根下 `.atelyx/history[/<kind>/]<最小编码文件名>.json`（隐藏目录，不进树 /
//! 索引 / 检索，与个人仓库同构）。真源仍是内容文件，历史是独立的审计轴；每个版本存全文快照
//! （回滚即时可靠）+ 客户端算好的改动摘要。
//!
//! 追加 = 同一路径串行锁内的「读 → 判重 / 同作者连续编辑合并 / 追加 → 版本数与字节预算剪枝 →
//! 原子写」。合并必须由服务端在锁内完成：客户端各自「读—改—写」整个侧文件，两端并发保存时
//! 后写者会覆盖先写者，版本静默丢失（客户端本地是单机场景，不存在该竞态）。
//! 判重/合并的**判据**（action、作者 id、时间戳、coalesce 窗口、上限、字节预算）由客户端表达进来，
//! 摘要按正文格式生成（行级 diff / 画布表格实体级人话）也由客户端算好传入——服务端不感知内容格式。
//!
//! 重命名 / 移动时由本模块迁移侧文件（服务端知道每次改名，比客户端 watcher 可靠）；
//! 聚合端点扫全量侧文件供「仓库历史」面板与日历活动密度（按日计数由客户端按本机时区归日，
//! 服务端不承担时区语义）。

use std::path::Path;

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::auth::AuthUser;
use crate::content::{join_err, write_root};
use crate::fsops::{atomic_write, SpaceRoot};
use crate::state::ServerState;
use crate::{ApiError, ApiResult};

/// 历史侧文件根与 kind 子目录（note 顶层，canvas/table 按 kind 分目录）。
/// 与客户端 `services/history/index.ts` 的 `historyPathFor` 逐字一致。
const HISTORY_DIR: &str = ".atelyx/history";
const HISTORY_KIND_SUBDIRS: [&str; 3] = ["", "canvas/", "table/"];

/// 单侧文件字节占位上限（客户端在写前按 byteBudget 剪枝；服务端兜底拒超限防写坏读路径）。
const MAX_HISTORY_FILE_BYTES: usize = 8 * 1024 * 1024;

/// 版本流上限（聚合响应防载荷爆炸；按日计数走 timestamps 不受此限）。
const HISTORY_FEED_CAP: usize = 300;

/// 侧文件名编码（最小百分号转义）：仅转义文件系统非法字符与 `%`/控制符。
/// 与客户端 `services/history/index.ts` 的 `encodeSideName` 及 `vault.rs::percent_encode`
/// 保持同一字符集——三处各自计算侧文件路径，字符集改动须同步，否则互不命中。
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let u = c as u32;
        if u < 0x20 || u == 0x7f || matches!(c, '%' | '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
            for b in c.to_string().bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// 百分号解码（`%XX`），非法序列原样保留（匹配不上自然不命中）。
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let hex = |b: u8| -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    };
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
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

/// kind 字符串校验（未知即拒，不落无意义侧文件）。
fn kind_of(kind: &str) -> Result<&'static str, ApiError> {
    match kind {
        "note" => Ok("note"),
        "canvas" => Ok("canvas"),
        "table" => Ok("table"),
        other => Err(ApiError(
            StatusCode::BAD_REQUEST,
            format!("未知历史类型：{other}"),
        )),
    }
}

/// 单文件侧文件相对路径（note 顶层；canvas/table 按 kind 分目录）。
fn side_rel(kind: &str, file: &str) -> String {
    let enc = percent_encode(file);
    match kind {
        "note" => format!("{HISTORY_DIR}/{enc}.json"),
        "canvas" => format!("{HISTORY_DIR}/canvas/{enc}.json"),
        _ => format!("{HISTORY_DIR}/table/{enc}.json"),
    }
}

/// 读侧文件版本列表（缺失/损坏 = 空；历史尽力而为，不阻塞写入）。
fn read_versions(path: &Path) -> Vec<Value> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return vec![];
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&raw) else {
        return vec![];
    };
    parsed
        .get("versions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct HistoryAuthor {
    pub id: String,
    pub name: String,
    pub device: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordBody {
    kind: String,
    file: String,
    content: String,
    action: String,
    author: HistoryAuthor,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    co_authors: Option<Vec<HistoryAuthor>>,
    #[serde(default)]
    coalesce_edit_ms: Option<i64>,
    #[serde(default)]
    max_versions: Option<usize>,
    #[serde(default)]
    byte_budget: Option<usize>,
}

/// 合并协作作者集合（按 id 去重，主作者之外）：coalesce 滑动更新时保留前序协作者。
fn merge_co_authors(existing: Option<&Value>, incoming: &[HistoryAuthor]) -> Vec<Value> {
    let mut out: Vec<Value> = vec![];
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Some(arr) = existing.and_then(|v| v.as_array()) {
        for a in arr {
            let id = a.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
            if seen.insert(id) {
                out.push(a.clone());
            }
        }
    }
    for a in incoming {
        if seen.insert(a.id.clone()) {
            out.push(serde_json::to_value(a).unwrap_or(Value::Null));
        }
    }
    out
}

/// 按字节预算剪枝（从最旧丢起；仅剩 1 版也超预算时保留 1 版兜底）。
fn prune_by_budget(mut versions: Vec<Value>, budget: usize) -> Vec<Value> {
    let size = |v: &[Value]| {
        serde_json::to_vec(&json!({ "versions": v }))
            .map(|b| b.len())
            .unwrap_or(0)
    };
    while versions.len() > 1 && size(&versions) > budget {
        versions.remove(0);
    }
    versions
}

/// 追加一个历史版本（判重 → 同作者连续编辑合并 → 追加 → 剪枝 → 原子写）。
/// 全程在每路径串行锁内完成，多端并发保存严格按到达序落地、不丢版本。
pub async fn record_history(
    State(state): State<ServerState>,
    user: AuthUser,
    AxumPath(space_id): AxumPath<String>,
    Json(body): Json<RecordBody>,
) -> ApiResult<impl IntoResponse> {
    let kind = kind_of(&body.kind)?;
    let root = write_root(&state, &space_id, &user)?;
    let rel = side_rel(kind, &body.file);
    let path = root.join(&rel, true).map_err(join_err)?;
    // 同路径串行：读—改—写在锁内完成，读到的即最新真源
    let _lock = state.path_lock(&space_id, &rel).await;

    let mut versions = read_versions(&path);
    let last = versions.last().cloned();
    // 与最新版本内容/动作相同 = no-op（防重复存档点）
    if let Some(l) = &last {
        if l.get("content").and_then(|c| c.as_str()) == Some(body.content.as_str())
            && l.get("action").and_then(|a| a.as_str()) == Some(body.action.as_str())
        {
            return Ok(Json(json!({ "skipped": true })).into_response());
        }
    }

    let author = serde_json::to_value(&body.author).unwrap_or(Value::Null);
    let now = now_millis();
    let window = body.coalesce_edit_ms.unwrap_or(0);
    // 连续编辑节流：上一版为 edit、action 为 edit、作者相同、在窗口内 → 就地滑动更新（seq 不变）
    let coalesce = window > 0
        && body.action == "edit"
        && last.as_ref().and_then(|l| l.get("action")).and_then(|a| a.as_str()) == Some("edit")
        && last
            .as_ref()
            .and_then(|l| l.get("author"))
            .and_then(|a| a.get("id"))
            .and_then(|i| i.as_str())
            == Some(body.author.id.as_str())
        && last
            .as_ref()
            .and_then(|l| l.get("ts"))
            .and_then(|t| t.as_i64())
            .map(|ts| now - ts < window)
            .unwrap_or(false);

    if coalesce {
        let l = versions.last_mut().expect("coalesce 分支必有上一版本");
        l["content"] = json!(body.content);
        l["ts"] = json!(now);
        l["author"] = author;
        if let Some(ca) = &body.co_authors {
            if !ca.is_empty() {
                let existing = l.get("coAuthors").cloned();
                l["coAuthors"] = Value::Array(merge_co_authors(existing.as_ref(), ca));
            }
        }
        if let Some(s) = &body.summary {
            l["summary"] = json!(s);
        }
        if let Some(n) = &body.note {
            l["note"] = json!(n);
        }
    } else {
        let seq = last
            .as_ref()
            .and_then(|l| l.get("seq"))
            .and_then(|s| s.as_i64())
            .unwrap_or(0)
            + 1;
        let mut version = json!({
            "seq": seq,
            "ts": now,
            "author": author,
            "action": body.action,
            "content": body.content,
        });
        if let Some(s) = &body.summary {
            version["summary"] = json!(s);
        }
        if let Some(ca) = &body.co_authors {
            if !ca.is_empty() {
                version["coAuthors"] = serde_json::to_value(ca).unwrap_or(Value::Null);
            }
        }
        if let Some(n) = &body.note {
            version["note"] = json!(n);
        }
        versions.push(version);
    }

    // 版本数上限（保留最近 N 版）→ 字节预算剪枝（叠加，优先保最新）
    let max = body.max_versions.unwrap_or(0);
    if max > 0 && versions.len() > max {
        let drop = versions.len() - max;
        versions.drain(0..drop);
    }
    let budget = body.byte_budget.unwrap_or(usize::MAX);
    let pruned = prune_by_budget(versions, budget);

    let bytes = serde_json::to_vec(&json!({ "versions": pruned }))
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("序列化失败：{e}")))?;
    if bytes.len() > MAX_HISTORY_FILE_BYTES {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "历史侧文件过大，已拒绝写入".to_string(),
        ));
    }
    atomic_write(&path, &bytes).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(json!({ "ok": true })).into_response())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RepoHistoryEntry {
    file: String,
    kind: String,
    ts: i64,
    author_id: String,
    author_name: String,
    author_device: String,
    action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

/// 递归收集侧文件版本（子目录白名单 canvas/table；根目录 = note）。
fn collect_history_dir(dir: &Path, kind: &str, entries: &mut Vec<RepoHistoryEntry>, timestamps: &mut Vec<i64>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if path.is_dir() && (name == "canvas" || name == "table") {
            collect_history_dir(&path, &name, entries, timestamps);
            continue;
        }
        if !name.ends_with(".json") {
            continue;
        }
        let file = percent_decode(&name[..name.len() - 5]);
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(parsed) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let Some(versions) = parsed.get("versions").and_then(|v| v.as_array()) else {
            continue;
        };
        for v in versions {
            // 缺 ts/非法 ts 的版本跳过（本服务写入必带 ts；文件损坏/手工改动时不臆造 1970 时刻，
            // 否则活动密度会多出一天虚假计数——本地实现遇必填字段缺失会整份跳过，同属「不臆造」）
            let Some(ts) = v.get("ts").and_then(|t| t.as_i64()) else {
                continue;
            };
            timestamps.push(ts);
            let author = v.get("author");
            entries.push(RepoHistoryEntry {
                file: file.clone(),
                kind: kind.to_string(),
                ts,
                author_id: author
                    .and_then(|a| a.get("id"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                author_name: author
                    .and_then(|a| a.get("name"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                author_device: author
                    .and_then(|a| a.get("device"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                action: v.get("action").and_then(|x| x.as_str()).unwrap_or("edit").to_string(),
                summary: v.get("summary").and_then(|x| x.as_str()).map(|s| s.to_string()),
                note: v.get("note").and_then(|x| x.as_str()).map(|s| s.to_string()),
            });
        }
    }
}

/// 聚合全空间历史：版本流（ts 倒序、上限）+ 全量版本时间戳（客户端按本机时区归日算活动密度）。
pub async fn aggregate_history(
    State(state): State<ServerState>,
    user: AuthUser,
    AxumPath(space_id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    let root = crate::content::member_root(&state, &space_id, &user)?;
    let mut entries: Vec<RepoHistoryEntry> = vec![];
    let mut timestamps: Vec<i64> = vec![];
    collect_history_dir(&root.0.join(HISTORY_DIR), "note", &mut entries, &mut timestamps);
    entries.sort_by(|a, b| b.ts.cmp(&a.ts));
    entries.truncate(HISTORY_FEED_CAP);
    Ok(Json(json!({
        "entries": entries,
        "timestamps": timestamps,
    })))
}

// ===== 重命名 / 移动时的侧文件迁移 =====
//
// 迁移与 record 必须在**同一把每侧文件锁**下交错，否则「record 已读到版本但写盘落在迁移之后」
// 会让旧编码名侧文件被重新创建（历史按新旧两个编码名分裂）。锁键 = 侧文件相对路径
// （record 用 `side_rel(kind, file)`；此处直接用枚举到的侧文件相对路径，两者同形）。
// 逐文件取锁、释放后再取下一个——任一时刻最多持一把，不会死锁。

/// 取锁后二次确认源仍在（等锁期间可能已被前一个持有者迁走）。
fn try_move_side_file(old_path: &std::path::Path, new_path: &std::path::Path) {
    if !old_path.is_file() || new_path.exists() {
        return;
    }
    let _ = std::fs::rename(old_path, new_path);
}

/// 单文件迁移：`.atelyx/history[/<kind>/]<enc(old)>.json` → 同结构新编码路径。
/// 源不存在静默跳过；目标已存在跳过不覆盖；单个失败不阻断其余（历史尽力而为）。
pub async fn remap_sideloads(state: &ServerState, space_id: &str, root: &SpaceRoot, old_file: &str, new_file: &str) {
    let old_enc = percent_encode(old_file);
    let new_enc = percent_encode(new_file);
    for sub in HISTORY_KIND_SUBDIRS {
        let old_rel = format!("{HISTORY_DIR}/{sub}{old_enc}.json");
        let new_rel = format!("{HISTORY_DIR}/{sub}{new_enc}.json");
        if old_rel == new_rel {
            continue;
        }
        // 与 record 同一把锁：迁移进行中不会有并发 record 重建旧名侧文件
        let _lock = state.path_lock(space_id, &old_rel).await;
        let Ok(old_path) = root.join(&old_rel, false) else {
            continue;
        };
        let Ok(new_path) = root.join(&new_rel, true) else {
            continue;
        };
        try_move_side_file(&old_path, &new_path);
    }
}

/// 文件夹重命名 / 移动后迁移其下全部侧文件：解码文件名，命中 `old_dir/` 前缀者改写到
/// `new_dir/` 前缀同结构新名。目标已存在跳过；单文件失败静默。
pub async fn remap_sideloads_by_dir(state: &ServerState, space_id: &str, root: &SpaceRoot, old_dir: &str, new_dir: &str) {
    let old_dir = old_dir.trim_end_matches('/');
    let new_dir = new_dir.trim_end_matches('/');
    if old_dir.is_empty() || new_dir.is_empty() {
        return;
    }
    let prefix = format!("{old_dir}/");
    for sub in HISTORY_KIND_SUBDIRS {
        let dir_rel = format!("{HISTORY_DIR}/{sub}");
        let Ok(dir) = root.join(&dir_rel, false) else {
            continue;
        };
        // 先收集候选名（读目录后即释放，不再持目录句柄），再逐个取锁迁移
        let mut candidates: Vec<String> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let name = match entry.file_name().into_string() {
                    Ok(n) => n,
                    Err(_) => continue,
                };
                if !name.ends_with(".json") {
                    continue;
                }
                let decoded = percent_decode(&name[..name.len() - 5]);
                if decoded.starts_with(&prefix) {
                    candidates.push(name);
                }
            }
        }
        for name in candidates {
            let old_rel = format!("{HISTORY_DIR}/{sub}{name}");
            let decoded = percent_decode(&name[..name.len() - 5]);
            let rest = match decoded.strip_prefix(&prefix) {
                Some(r) => r,
                None => continue,
            };
            let new_name = format!("{}.json", percent_encode(&format!("{new_dir}/{rest}")));
            // 与 record 同一把锁（逐文件取锁，不跨文件持有）
            let _lock = state.path_lock(space_id, &old_rel).await;
            let Ok(old_path) = root.join(&old_rel, false) else {
                continue;
            };
            let dst = dir.join(&new_name);
            try_move_side_file(&old_path, &dst);
        }
    }
}

/// 内容重命名后按「文件 / 文件夹」分派侧文件迁移（失败静默，不阻塞重命名主流程）。
/// `old_path`/`new_path` 为相对空间根路径；目标是否为目录由调用方在改名后判定。
pub async fn remap_after_rename(
    state: &ServerState,
    space_id: &str,
    root: &SpaceRoot,
    old_path: &str,
    new_path: &str,
    is_dir: bool,
) {
    if is_dir {
        remap_sideloads_by_dir(state, space_id, root, old_path, new_path).await;
    } else {
        remap_sideloads(state, space_id, root, old_path, new_path).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_rel_matches_client_layout() {
        assert_eq!(side_rel("note", "a/b.md"), ".atelyx/history/a%2Fb.md.json");
        assert_eq!(side_rel("canvas", "画布.atlx"), ".atelyx/history/canvas/画布.atlx.json");
        assert_eq!(side_rel("table", "t.atb"), ".atelyx/history/table/t.atb.json");
    }

    #[test]
    fn encode_decode_round_trip() {
        for s in ["a/b.md", "中文 名.md", "a%b", "a\\b:c"] {
            assert_eq!(percent_decode(&percent_encode(s)), s);
        }
    }

    #[test]
    fn prune_keeps_at_least_one() {
        let v: Vec<Value> = (0..5).map(|i| json!({ "seq": i, "content": "x".repeat(100) })).collect();
        let pruned = prune_by_budget(v, 10);
        assert_eq!(pruned.len(), 1);
    }
}
