//! 重建内部链接：全库 `.md` 的双链/路径链接统一改写为标准 Markdown 链接（两步：计划纯读 + 应用落盘）。

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::vault;

/// 重建计划（纯读产物，供确认对话框审阅）。
pub struct RebuildPlan {
    /// 扫描的 `.md` 文件数（含无改动的）。
    pub scanned: usize,
    /// 计划改写的链接处数。
    pub links: usize,
    /// 有改动的文件（相对路径 → 改写后全文）。
    pub rewrites: Vec<(String, String)>,
}

/// 应用结果：`errors` 由调用方呈现（单文件失败不中断其余）。
pub struct RebuildOutcome {
    /// 本次写入的文件数。
    pub written: usize,
    /// 计划列出但本次未写入的文件数（按当前内容重算后已无差异，见 apply）。
    pub skipped: usize,
    /// (相对路径, 失败原因)。
    pub errors: Vec<(String, String)>,
}

/// 排除文件夹：读仓库配置（plan/apply 的唯一 exclude 来源；未配置 = 空）。
fn exclude_folders(root: &Path) -> Result<Vec<String>, String> {
    Ok(vault::read_vault_config(root)?.exclude_folders.unwrap_or_default())
}

/// 链接解析器：全库 .md 建精确路径 + basename 两级索引（同名歧义由 resolve_link_target 定序）。
fn link_resolver(root: &Path, exclude: &[String]) -> Result<impl Fn(&str) -> Option<String>, String> {
    let mut files: Vec<String> = Vec::new();
    vault::walk_md_in(root, "", exclude, &mut |rel, _| {
        files.push(rel.to_string());
        Ok(())
    })?;
    let exact: HashSet<String> = files.iter().cloned().collect();
    let mut by_basename: HashMap<String, Vec<String>> = HashMap::new();
    for rel in &files {
        let base = rel.rsplit('/').next().unwrap_or(rel).to_string();
        by_basename.entry(base).or_default().push(rel.clone());
    }
    Ok(move |name: &str| vault::resolve_link_target(name, &exact, &by_basename))
}

/// 全库扫描生成重建计划：纯读不落盘；不可读文件跳过不阻塞其余。
pub fn plan(root: &Path) -> Result<RebuildPlan, String> {
    let exclude = exclude_folders(root)?;
    let resolver = link_resolver(root, &exclude)?;
    let mut scanned = 0usize;
    let mut links = 0usize;
    let mut rewrites: Vec<(String, String)> = Vec::new();
    vault::walk_md_in(root, "", &exclude, &mut |rel, path| {
        scanned += 1;
        let Ok(content) = std::fs::read_to_string(path) else {
            return Ok(());
        };
        let (next, n) = vault::rewrite_internal_links(&content, &resolver);
        if n > 0 {
            links += n;
            rewrites.push((rel.to_string(), next));
        }
        Ok(())
    })?;
    Ok(RebuildPlan { scanned, links, rewrites })
}

/// 执行重建：计划与执行之间文件可能被编辑或外部修改，对计划列出的每个文件**按当前磁盘
/// 内容重算改写后写入**（不写计划文本，防旧结论覆盖新状态）；重算后无差异 → skipped 不写。
/// 逐文件原子写，单文件失败记入 errors 继续；全局失败（配置/扫描）返回 Err 且未写任何文件。
pub fn apply(root: &Path, plan: &RebuildPlan) -> Result<RebuildOutcome, String> {
    let exclude = exclude_folders(root)?;
    let resolver = link_resolver(root, &exclude)?;
    let mut outcome = RebuildOutcome { written: 0, skipped: 0, errors: Vec::new() };
    for (rel, _) in &plan.rewrites {
        let path = match vault::safe_join(root, rel, false) {
            Ok(p) => p,
            Err(e) => {
                outcome.errors.push((rel.clone(), e));
                continue;
            }
        };
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                outcome.errors.push((rel.clone(), format!("读取失败：{e}")));
                continue;
            }
        };
        let (next, _) = vault::rewrite_internal_links(&content, &resolver);
        if next == content {
            outcome.skipped += 1;
            continue;
        }
        match vault::atomic_write(&path, &next) {
            Ok(()) => outcome.written += 1,
            Err(e) => outcome.errors.push((rel.clone(), e)),
        }
    }
    Ok(outcome)
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

    #[test]
    fn plan_rewrites_wiki_alias_and_missing() {
        let tmp = TempDir::new("rebuild-basic");
        let root = &*tmp;
        write(root, "note-a.md", "A");
        write(root, "notes/note-b.md", "B");
        write(root, "doc.md", "见 [[note-a]] 与 [[note-b|别名]] 与 [[missing]]");
        let plan = plan(root).unwrap();
        assert_eq!(plan.scanned, 3);
        assert_eq!(plan.links, 3);
        assert_eq!(
            plan.rewrites,
            vec![(
                "doc.md".to_string(),
                "见 [note-a](note-a.md) 与 [别名](notes/note-b.md) 与 [missing]()".to_string()
            )]
        );
        // 纯读不落盘：文件内容未被触碰
        assert_eq!(
            std::fs::read_to_string(root.join("doc.md")).unwrap(),
            "见 [[note-a]] 与 [[note-b|别名]] 与 [[missing]]"
        );
    }

    #[test]
    fn plan_resolves_basename_with_disambiguation() {
        let tmp = TempDir::new("rebuild-basename");
        let root = &*tmp;
        write(root, "b.md", "根");
        write(root, "dir/b.md", "子");
        write(root, "doc.md", "见 [[b]] 与 [子](dir/b.md) 与 [唯一直链](b.md)");
        let plan = plan(root).unwrap();
        // 同名歧义取路径最短（根目录优先）；已命中规范路径的直链不改写
        assert_eq!(plan.links, 1);
        assert_eq!(
            plan.rewrites,
            vec![(
                "doc.md".to_string(),
                "见 [b](b.md) 与 [子](dir/b.md) 与 [唯一直链](b.md)".to_string()
            )]
        );
    }

    #[test]
    fn plan_normalizes_basename_and_encoded_paths() {
        let tmp = TempDir::new("rebuild-norm");
        let root = &*tmp;
        write(root, "notes/note-b.md", "B");
        write(root, "topic a.md", "T");
        write(root, "doc.md", "[note-b](note-b.md) 与 [doc](topic%20a.md)");
        let plan = plan(root).unwrap();
        // basename 形式按文件名命中补全；percent 编码路径解码后归一化
        assert_eq!(plan.links, 2);
        assert_eq!(
            plan.rewrites,
            vec![(
                "doc.md".to_string(),
                "[note-b](notes/note-b.md) 与 [doc](topic a.md)".to_string()
            )]
        );
    }

    #[test]
    fn plan_skips_frontmatter_code_image_external() {
        let tmp = TempDir::new("rebuild-skip");
        let root = &*tmp;
        write(root, "note-b.md", "B");
        let content = "---\ntags: [[note-b]]\n---\n\n```\n[[note-b]]\n```\n\n`[[note-b]]`\n\n![img](note-b.md)\n\n[ext](https://example.com)\n\n见 [[note-b]]";
        write(root, "doc.md", content);
        let plan = plan(root).unwrap();
        assert_eq!(plan.links, 1);
        assert_eq!(
            plan.rewrites,
            vec![(
                "doc.md".to_string(),
                "---\ntags: [[note-b]]\n---\n\n```\n[[note-b]]\n```\n\n`[[note-b]]`\n\n![img](note-b.md)\n\n[ext](https://example.com)\n\n见 [note-b](note-b.md)".to_string()
            )]
        );
    }

    #[test]
    fn plan_skips_hidden_and_excluded_dirs() {
        let tmp = TempDir::new("rebuild-exclude");
        let root = &*tmp;
        write(root, "note-a.md", "A");
        write(root, ".hidden/s.md", "见 [[note-a]]");
        write(root, "secret/s.md", "见 [[note-a]]");
        write(root, ".atelyx/config.json", r#"{"excludeFolders":["secret"]}"#);
        let plan = plan(root).unwrap();
        assert_eq!(plan.scanned, 1);
        assert!(plan.rewrites.is_empty());
    }

    #[test]
    fn apply_writes_exact_text_and_preserves_drifted_edits() {
        let tmp = TempDir::new("rebuild-apply");
        let root = &*tmp;
        write(root, "note-a.md", "A");
        write(root, "doc.md", "见 [[note-a]]");
        let plan = plan(root).unwrap();
        // plan 之后用户继续编辑（内容漂移）：apply 以当前内容重算，不得覆盖新编辑
        write(root, "doc.md", "开头新行\n见 [[note-a]]");
        let outcome = apply(root, &plan).unwrap();
        assert_eq!(outcome.written, 1);
        assert_eq!(outcome.skipped, 0);
        assert!(outcome.errors.is_empty());
        assert_eq!(
            std::fs::read_to_string(root.join("doc.md")).unwrap(),
            "开头新行\n见 [note-a](note-a.md)"
        );
    }

    #[test]
    fn apply_skips_files_already_consistent() {
        let tmp = TempDir::new("rebuild-skip-consistent");
        let root = &*tmp;
        write(root, "note-a.md", "A");
        write(root, "doc.md", "见 [[note-a]]");
        let plan = plan(root).unwrap();
        // plan 之后文件已被改写成目标形态：重算无差异 → 跳过不写
        write(root, "doc.md", "见 [note-a](note-a.md)");
        let outcome = apply(root, &plan).unwrap();
        assert_eq!(outcome.written, 0);
        assert_eq!(outcome.skipped, 1);
        assert_eq!(std::fs::read_to_string(root.join("doc.md")).unwrap(), "见 [note-a](note-a.md)");
    }

    #[test]
    fn apply_records_per_file_errors_without_stopping() {
        let tmp = TempDir::new("rebuild-errors");
        let root = &*tmp;
        write(root, "t.md", "T");
        write(root, "ok.md", "看 [[t]]");
        // 同名目录占位制造读取失败：记入 errors 且不中断其余文件
        std::fs::create_dir(root.join("dir.md")).unwrap();
        let plan = RebuildPlan {
            scanned: 2,
            links: 1,
            rewrites: vec![
                ("dir.md".to_string(), String::new()),
                ("ok.md".to_string(), "看 [t](t.md)".to_string()),
            ],
        };
        let outcome = apply(root, &plan).unwrap();
        assert_eq!(outcome.written, 1);
        assert_eq!(std::fs::read_to_string(root.join("ok.md")).unwrap(), "看 [t](t.md)");
        assert_eq!(outcome.errors.len(), 1);
        assert_eq!(outcome.errors[0].0, "dir.md");
        assert!(!outcome.errors[0].1.is_empty());
    }
}
