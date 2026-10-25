//! 重命名/移动的引用同步事务：画布节点引用（.atlx）+ 全库笔记内部链接（.md）+ 路径类元数据
//! （prompt-notes / agents / folder-colors）+ 历史侧文件迁移。先改名后统一写回，写回失败回滚改名。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::vault::{self, collect_md_link_updates, CanvasFile};

/// 同步结果：`rewritten` = 内部链接被代写的 `.md` 相对路径清单（调用方据此刷新对应笔记）。
pub struct SyncResult {
    pub rewritten: Vec<String>,
}

/// 入参路径归一化：剥尾部 `/` `\`，空串拒绝——引用匹配按字符串比较，非规范形态会
/// 静默零命中（底层 rename 照常成功），必须在入口挡掉。
fn normalize_rel(p: &str) -> Result<String, String> {
    let trimmed = p.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return Err(format!("非法路径：{p:?}"));
    }
    Ok(trimmed.to_string())
}

/// 重命名/移动 `.md` 笔记并同步引用（画布精确匹配 + `.md` 链接精确匹配）。
/// `exclude` = .md 链接扫描的排除目录；画布引用扫描不过滤（排除目录内的画布也可能引用）。
pub fn rename_note_synced(
    root: &Path,
    exclude: &[String],
    old_file: &str,
    new_file: &str,
) -> Result<SyncResult, String> {
    let old_file = normalize_rel(old_file)?;
    let new_file = normalize_rel(new_file)?;
    let (old_file, new_file) = (old_file.as_str(), new_file.as_str());
    let pending = collect_canvas_updates(root, &mut |canvas| {
        remap_canvas_refs(canvas, old_file, new_file)
    })?;
    let pending_md = collect_md_link_updates(root, exclude, &mut |span: &str| {
        let Some(path) = vault::markdown_link_path(span) else {
            return span.to_string();
        };
        if path != old_file {
            return span.to_string();
        }
        replace_link_target(span, new_file)
    })?;
    // 收集发生在重命名前：被重命名笔记自身的旧路径自引用条目，写回目标必须是新路径（防旧路径重建文件）
    let pending_md: Vec<(String, String)> = pending_md
        .into_iter()
        .map(|(rel, content)| {
            if rel == old_file {
                (new_file.to_string(), content)
            } else {
                (rel, content)
            }
        })
        .collect();
    vault::rename_note_file(root, old_file, new_file)?;
    if let Err(e) = flush_canvas_updates(&pending) {
        let _ = vault::rename_note_file(root, new_file, old_file);
        return Err(format!("更新画布引用失败，重命名已回滚（请重试）：{e}"));
    }
    if let Err(e) = vault::flush_md_updates(root, &pending_md) {
        let _ = vault::rename_note_file(root, new_file, old_file);
        return Err(format!("更新笔记内部链接失败，重命名已回滚（请重试）：{e}"));
    }
    remap_note_paths(root, &[(old_file.to_string(), new_file.to_string())]);
    vault::remap_sideloads(root, old_file, new_file);
    Ok(SyncResult { rewritten: pending_md.into_iter().map(|(rel, _)| rel).collect() })
}

/// 重命名/移动文件夹并同步引用（画布与 `.md` 链接按 `old/` 前缀替换）。
/// 先移动目录（旧路径已不存在），再扫描新树收集引用更新并统一写回；失败回滚移动。
pub fn rename_folder_synced(
    root: &Path,
    exclude: &[String],
    old_dir: &str,
    new_dir: &str,
) -> Result<SyncResult, String> {
    let old_dir = normalize_rel(old_dir)?;
    let new_dir = normalize_rel(new_dir)?;
    let (old_dir, new_dir) = (old_dir.as_str(), new_dir.as_str());
    vault::rename_folder(root, old_dir, new_dir)?;
    let pending = match collect_canvas_updates(root, &mut |canvas| {
        remap_canvas_refs(canvas, old_dir, new_dir)
    }) {
        Ok(p) => p,
        Err(e) => {
            let _ = vault::rename_folder(root, new_dir, old_dir);
            return Err(format!("扫描画布引用失败，重命名已回滚（请重试）：{e}"));
        }
    };
    let old_prefix = format!("{old_dir}/");
    let new_prefix = format!("{new_dir}/");
    let pending_md = match collect_md_link_updates(root, exclude, &mut |span: &str| {
        let Some(path) = vault::markdown_link_path(span) else {
            return span.to_string();
        };
        let Some(rest) = path.strip_prefix(&old_prefix) else {
            return span.to_string();
        };
        replace_link_target(span, &format!("{new_prefix}{rest}"))
    }) {
        Ok(p) => p,
        Err(e) => {
            let _ = vault::rename_folder(root, new_dir, old_dir);
            return Err(format!("扫描笔记内部链接失败，重命名已回滚（请重试）：{e}"));
        }
    };
    if let Err(e) = flush_canvas_updates(&pending) {
        let _ = vault::rename_folder(root, new_dir, old_dir);
        return Err(format!("更新画布引用失败，重命名已回滚（请重试）：{e}"));
    }
    if let Err(e) = vault::flush_md_updates(root, &pending_md) {
        let _ = vault::rename_folder(root, new_dir, old_dir);
        return Err(format!("更新笔记内部链接失败，重命名已回滚（请重试）：{e}"));
    }
    remap_dir_paths(root, old_dir, new_dir);
    vault::remap_sideloads_by_dir(root, old_dir, new_dir);
    Ok(SyncResult { rewritten: pending_md.into_iter().map(|(rel, _)| rel).collect() })
}

/// `[label](target)` 只换 target，label 原样（span 形如 `[..](..)`，取首个 `](` 前的标签段）。
fn replace_link_target(span: &str, new_target: &str) -> String {
    let label_end = span.find("](").unwrap_or(span.len() - 1);
    format!("{}]({new_target})", &span[..label_end])
}

// ===== 画布引用扫描与写回 =====

/// 递归扫描仓库内全部 `.atlx`，命中更新条件的收集（不写盘，统一写回）。
/// 全仓库扫描：被引用文件与画布可能位于任意文件夹（含排除/隐藏目录），链接维护不过滤。
fn collect_canvas_updates(
    root: &Path,
    update: &mut dyn FnMut(&mut CanvasFile) -> bool,
) -> Result<Vec<(PathBuf, CanvasFile)>, String> {
    let mut updates = Vec::new();
    scan_atlx_in(root, "", update, &mut updates)?;
    Ok(updates)
}

fn scan_atlx_in(
    root: &Path,
    rel: &str,
    update: &mut dyn FnMut(&mut CanvasFile) -> bool,
    updates: &mut Vec<(PathBuf, CanvasFile)>,
) -> Result<(), String> {
    if !root.join(rel).exists() {
        return Ok(());
    }
    for (child_rel, is_dir) in vault::read_dir_filtered(root, rel, &[])? {
        if is_dir {
            scan_atlx_in(root, &child_rel, update, updates)?;
        } else if child_rel.ends_with(".atlx") {
            let path = root.join(&child_rel);
            let Ok(mut canvas) = vault::read_canvas_file(&path) else {
                continue; // 无法解析的文件跳过，不阻塞其他画布的链接维护
            };
            if update(&mut canvas) {
                canvas.updated_at = vault::now_secs();
                updates.push((path, canvas));
            }
        }
    }
    Ok(())
}

/// 统一写回收集的画布更新（改名成功后才调用；失败由调用方回滚改名）。
fn flush_canvas_updates(updates: &[(PathBuf, CanvasFile)]) -> Result<(), String> {
    for (path, canvas) in updates {
        vault::write_canvas_file(path, canvas)?;
    }
    Ok(())
}

/// 节点类型 → 引用字段名（text/media/table 节点引用独立文件用 `file`；
/// conversation 节点系统提示词引用 .md 用 `systemPromptFile`）。
fn ref_field_of(node_type: &str) -> Option<&'static str> {
    match node_type {
        "text" | "media" | "table" => Some("file"),
        "conversation" => Some("systemPromptFile"),
        _ => None,
    }
}

/// 把一处改名 / 移动反映到画布节点引用（内存中）：精确路径与目录前缀两种匹配都试，
/// 每个引用只改写一次（`new_rel` 落在 `old_rel` 内部时不会被前缀匹配二次改写）。
/// 返回是否有变更；改名事务（笔记 / 文件夹）与协作空间的链接改写共用。
pub fn remap_canvas_refs(canvas: &mut CanvasFile, old_rel: &str, new_rel: &str) -> bool {
    let (Ok(old_rel), Ok(new_rel)) = (normalize_rel(old_rel), normalize_rel(new_rel)) else {
        return false;
    };
    let old_prefix = format!("{old_rel}/");
    let mut changed = false;
    for node in &mut canvas.nodes {
        let Some(field) = ref_field_of(&node.node_type) else { continue };
        let Some(obj) = node.data.as_object_mut() else { continue };
        let Some(path) = obj.get(field).and_then(|v| v.as_str()).map(str::to_string) else {
            continue;
        };
        let mapped = if path == old_rel {
            new_rel.clone()
        } else if let Some(rest) = path.strip_prefix(&old_prefix) {
            format!("{new_rel}/{rest}")
        } else {
            continue;
        };
        obj.insert(field.to_string(), serde_json::Value::String(mapped));
        changed = true;
    }
    changed
}

// ===== 路径元数据同步（尽力而为：失败不回滚主流程） =====

/// 单文件路径类元数据同步：提示词标记与 Agent 的提示词引用按精确路径替换。
fn remap_note_paths(root: &Path, pairs: &[(String, String)]) {
    if let Ok(mut notes) = vault::read_prompt_notes_file(root) {
        let mut changed = false;
        for note in &mut notes {
            if let Some((_, new)) = pairs.iter().find(|(old, _)| old == note) {
                *note = new.clone();
                changed = true;
            }
        }
        if changed {
            let _ = vault::write_prompt_notes_file(root, &notes);
        }
    }
    if let Ok(mut agents) = vault::read_agents_file(root) {
        let mut changed = false;
        for agent in &mut agents {
            if let Some(f) = &agent.system_prompt_file {
                if let Some((_, new)) = pairs.iter().find(|(old, _)| old == f) {
                    agent.system_prompt_file = Some(new.clone());
                    changed = true;
                }
            }
        }
        if changed {
            let _ = vault::write_agents_file(root, &agents);
        }
    }
}

/// 目录前缀类元数据同步：提示词标记 / Agent 提示词引用 / 文件夹颜色键按前缀替换
/// （颜色键可为目录本身，精确匹配也替换）。
fn remap_dir_paths(root: &Path, old_dir: &str, new_dir: &str) {
    let old_prefix = format!("{old_dir}/");
    let remap = |p: &str| -> Option<String> {
        if p == old_dir {
            return Some(new_dir.to_string());
        }
        p.strip_prefix(&old_prefix).map(|rest| format!("{new_dir}/{rest}"))
    };
    if let Ok(mut notes) = vault::read_prompt_notes_file(root) {
        let mut changed = false;
        for note in &mut notes {
            if let Some(next) = remap(note) {
                *note = next;
                changed = true;
            }
        }
        if changed {
            let _ = vault::write_prompt_notes_file(root, &notes);
        }
    }
    if let Ok(mut agents) = vault::read_agents_file(root) {
        let mut changed = false;
        for agent in &mut agents {
            if let Some(f) = &agent.system_prompt_file {
                if let Some(next) = remap(f) {
                    agent.system_prompt_file = Some(next);
                    changed = true;
                }
            }
        }
        if changed {
            let _ = vault::write_agents_file(root, &agents);
        }
    }
    if let Ok(colors) = vault::read_folder_colors_file(root) {
        let mut next: HashMap<String, String> = HashMap::new();
        let mut changed = false;
        for (dir, color) in &colors {
            match remap(dir) {
                Some(new_dir) => {
                    next.insert(new_dir, color.clone());
                    changed = true;
                }
                None => {
                    next.insert(dir.clone(), color.clone());
                }
            }
        }
        if changed {
            let _ = vault::write_folder_colors_file(root, &next);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::test_support::TempDir;

    fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    fn canvas_json(file_field: &str) -> String {
        // text 节点引用 + conversation 节点提示词引用
        format!(
            r#"{{"schema":"atelyx-canvas/v1","id":"c1","title":"c","nodes":[
                {{"id":"n1","type":"text","x":0,"y":0,"width":200,"height":120,"data":{{"file":"{file_field}"}}}},
                {{"id":"n2","type":"conversation","x":0,"y":200,"width":200,"height":120,"data":{{"systemPromptFile":"{file_field}"}}}}
            ],"edges":[]}}"#
        )
    }

    #[test]
    fn rename_note_updates_canvas_and_md_refs() {
        let tmp = TempDir::new("refs-note");
        let root = &*tmp;
        write(root, "a.md", "正文");
        write(root, "画布.atlx", &canvas_json("a.md"));
        write(root, "b.md", "看 [它](a.md) 与 [别的](other.md)");
        let result = rename_note_synced(root, &[], "a.md", "dir/a.md").unwrap();
        // 画布引用同步（write_canvas_file 落盘为 pretty JSON）
        let canvas = std::fs::read_to_string(root.join("画布.atlx")).unwrap();
        assert!(canvas.contains(r#""file": "dir/a.md""#), "{canvas}");
        assert!(canvas.contains(r#""systemPromptFile": "dir/a.md""#));
        // .md 链接同步（b.md 被代写，other.md 链接原样）；被改名文件自身的自引用写回新路径
        let b = std::fs::read_to_string(root.join("b.md")).unwrap();
        assert_eq!(b, "看 [它](dir/a.md) 与 [别的](other.md)");
        assert_eq!(result.rewritten, vec!["b.md".to_string()]);
        assert!(root.join("dir/a.md").exists());
        assert!(!root.join("a.md").exists());
    }

    #[test]
    fn rename_failure_keeps_original_state() {
        let tmp = TempDir::new("refs-rollback");
        let root = &*tmp;
        write(root, "a.md", "正文");
        // 目标已存在 → rename 拒绝，源保留
        write(root, "dir/a.md", "既有");
        assert!(rename_note_synced(root, &[], "a.md", "dir/a.md").is_err());
        assert!(root.join("a.md").exists());
        assert_eq!(std::fs::read_to_string(root.join("dir/a.md")).unwrap(), "既有");
    }

    #[test]
    fn rename_folder_remaps_prefix_refs() {
        let tmp = TempDir::new("refs-dir");
        let root = &*tmp;
        write(root, "old/a.md", "正文");
        write(root, "old/sub/b.md", "内容");
        write(root, "画布.atlx", &canvas_json("old/a.md"));
        write(root, "c.md", "看 [它](old/a.md)");
        let _ = rename_folder_synced(root, &[], "old", "new").unwrap();
        let canvas = std::fs::read_to_string(root.join("画布.atlx")).unwrap();
        assert!(canvas.contains(r#""file": "new/a.md""#), "{canvas}");
        let c = std::fs::read_to_string(root.join("c.md")).unwrap();
        assert_eq!(c, "看 [它](new/a.md)");
        assert!(root.join("new/sub/b.md").exists());
        assert!(!root.join("old").exists());
    }

    #[test]
    fn remap_canvas_refs_matches_exact_and_dir_prefix() {
        let mut canvas: CanvasFile = serde_json::from_str(
            r#"{"schema":"atelyx-canvas/v1","id":"c1","title":"c","nodes":[
                {"id":"n1","type":"text","x":0,"y":0,"data":{"file":"old/a.md"}},
                {"id":"n2","type":"media","x":0,"y":0,"data":{"file":"old"}},
                {"id":"n3","type":"conversation","x":0,"y":0,"data":{"systemPromptFile":"older/a.md"}}
            ],"edges":[]}"#,
        )
        .unwrap();
        assert!(remap_canvas_refs(&mut canvas, "old", "new"));
        assert_eq!(canvas.nodes[0].data["file"], "new/a.md");
        // 目录自身被直接引用（如媒体目录）也命中
        assert_eq!(canvas.nodes[1].data["file"], "new");
        // 前缀含尾斜杠：`older/` 不被 `old` 误改
        assert_eq!(canvas.nodes[2].data["systemPromptFile"], "older/a.md");
        assert!(!remap_canvas_refs(&mut canvas, "old", "new"), "二次改写应无变更");
    }

    #[test]
    fn remap_canvas_refs_does_not_double_apply_into_itself() {
        let mut canvas: CanvasFile = serde_json::from_str(
            r#"{"schema":"atelyx-canvas/v1","id":"c1","title":"c","nodes":[
                {"id":"n1","type":"media","x":0,"y":0,"data":{"file":"old"}},
                {"id":"n2","type":"text","x":0,"y":0,"data":{"file":"old/a.md"}}
            ],"edges":[]}"#,
        )
        .unwrap();
        assert!(remap_canvas_refs(&mut canvas, "old/", "old/sub"));
        // 目录自身引用命中精确匹配后，不再被前缀匹配追加一层
        assert_eq!(canvas.nodes[0].data["file"], "old/sub");
        assert_eq!(canvas.nodes[1].data["file"], "old/sub/a.md");
    }

    #[test]
    fn metadata_paths_follow_renames() {
        let tmp = TempDir::new("refs-meta");
        let root = &*tmp;
        write(root, "old/p.md", "提示词");
        // 预置元数据
        write(root, ".atelyx/prompt-notes.json", "[\"old/p.md\"]");
        write(
            root,
            ".atelyx/agents.json",
            r#"{"schema":"atelyx-agents/v1","agents":[{"id":"a","name":"A","systemPromptFile":"old/p.md"}]}"#,
        );
        write(root, ".atelyx/folder-colors.json", r##"{"old":"#ff0000"}"##);
        rename_folder_synced(root, &[], "old", "new").unwrap();
        assert!(std::fs::read_to_string(root.join(".atelyx/prompt-notes.json")).unwrap().contains("new/p.md"));
        assert!(std::fs::read_to_string(root.join(".atelyx/agents.json")).unwrap().contains("new/p.md"));
        assert!(std::fs::read_to_string(root.join(".atelyx/folder-colors.json")).unwrap().contains("new"));
    }
}
