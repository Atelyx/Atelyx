//! AI 笔记编辑的决策段（纯函数）：基线选择、三方合流前置判定与落盘后的编辑面结算。
//! 壳只做 I/O 编排，判定规则集中在此，便于用真实时序（脏 / 外部改动 / 在途保存）直测。

/// 本地编辑基线：打开且脏时以上次落盘正文为共同祖先（磁盘已变说明外部也改过，无法安全对齐）；
/// 未打开 / 未脏时以磁盘事实为源。
pub fn local_base(
    open: bool,
    dirty: bool,
    disk: &str,
    note_base: &str,
    path: &str,
) -> Result<String, String> {
    if open && dirty {
        if disk != note_base {
            return Err(format!(
                "文件已被外部修改，且你有未保存的改动；请先保存或重新打开 {path} 后再让 AI 修改"
            ));
        }
        return Ok(note_base.to_string());
    }
    Ok(disk.to_string())
}

/// 空间编辑的操作源：以工具线程取回的服务端正文为准（取不到回落本端落盘基线）。
pub fn space_source(server_text: Option<&str>, flushed: Option<&str>) -> String {
    match server_text {
        Some(text) => text.to_string(),
        None => flushed.unwrap_or_default().to_string(),
    }
}

/// 空间编辑的分歧判定：打开且本端有未落盘内容时，服务端既非落盘基线也非本端文本
/// = 双方都改过，无法安全对齐（拒绝写入、不动草稿）。
pub fn space_diverged_conflict(
    open: bool,
    note_text: &str,
    base: &str,
    source: &str,
    path: &str,
) -> Result<bool, String> {
    let diverged = open && note_text != base;
    if diverged && source != base && source != note_text {
        return Err(format!(
            "文件已被他人修改，且你有未保存的改动；请先保存或重新打开 {path} 后再让 AI 修改"
        ));
    }
    Ok(diverged)
}

/// 落盘回执对编辑面的结算：只有当前打开的笔记才回填脏标记与错误条（写别的文件失败不得污染本篇）。
#[derive(Debug, Default, PartialEq)]
pub struct NoteSettle {
    /// 新的落盘基线（None = 不更新）。
    pub note_base: Option<String>,
    /// 脏标记（None = 不动）。
    pub dirty: Option<bool>,
    /// 错误条（Some(None) = 清空；None = 不动）。
    pub save_error: Option<Option<String>>,
}

/// 按回执结算编辑面：成功推进基线并在正文未变时清脏；失败恢复脏标记并留错误原因。
pub fn settle_note_done(
    is_open: bool,
    content: &str,
    current_text: &str,
    err: Option<&str>,
) -> NoteSettle {
    if !is_open {
        return NoteSettle::default();
    }
    match err {
        None => NoteSettle {
            note_base: Some(content.to_string()),
            // 落盘正文即基线；期间新输入保持脏
            dirty: (current_text == content).then_some(false),
            save_error: Some(None),
        },
        Some(e) => NoteSettle {
            note_base: None,
            dirty: Some(true),
            save_error: Some(Some(e.to_string())),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_base_uses_disk_unless_open_and_dirty() {
        // 未打开：磁盘事实
        assert_eq!(local_base(false, false, "disk", "old", "a.md").unwrap(), "disk");
        // 打开但未脏：磁盘事实
        assert_eq!(local_base(true, false, "disk", "old", "a.md").unwrap(), "disk");
        // 打开且脏、磁盘仍是上次落盘：以上次落盘为共同祖先
        assert_eq!(local_base(true, true, "base", "base", "a.md").unwrap(), "base");
        // 打开且脏、磁盘已变：拒绝（外部也改过）
        let err = local_base(true, true, "外部改过", "base", "a.md").unwrap_err();
        assert!(err.contains("文件已被外部修改"), "{err}");
    }

    #[test]
    fn space_source_prefers_server_text() {
        assert_eq!(space_source(Some("服务端"), Some("基线")), "服务端");
        assert_eq!(space_source(None, Some("基线")), "基线");
        assert_eq!(space_source(None, None), "");
    }

    #[test]
    fn space_diverged_conflict_only_when_both_sides_changed() {
        // 未打开：无分歧概念
        assert!(!space_diverged_conflict(false, "本端", "基线", "服务端", "a.md").unwrap());
        // 打开但未脏：直接按服务端源写
        assert!(!space_diverged_conflict(true, "基线", "基线", "服务端", "a.md").unwrap());
        // 打开且脏、服务端仍是落盘基线：可合流
        assert!(space_diverged_conflict(true, "本端", "基线", "基线", "a.md").unwrap());
        // 打开且脏、服务端 = 本端文本：可合流
        assert!(space_diverged_conflict(true, "本端", "基线", "本端", "a.md").unwrap());
        // 打开且脏、服务端是第三方内容：拒绝
        let err = space_diverged_conflict(true, "本端", "基线", "别人的", "a.md").unwrap_err();
        assert!(err.contains("文件已被他人修改"), "{err}");
    }

    #[test]
    fn settle_updates_only_the_open_note() {
        // 写别的文件：不回填本篇状态
        assert_eq!(settle_note_done(false, "x", "y", None), NoteSettle::default());
        // 成功且正文未变：推进基线并清脏
        let settle = settle_note_done(true, "新", "新", None);
        assert_eq!(settle.note_base.as_deref(), Some("新"));
        assert_eq!(settle.dirty, Some(false));
        assert_eq!(settle.save_error, Some(None));
        // 成功但期间又有输入：保持脏
        let settle = settle_note_done(true, "落盘内容", "又改了", None);
        assert_eq!(settle.dirty, None);
        // 失败：恢复脏并留原因
        let settle = settle_note_done(true, "新", "新", Some("磁盘满"));
        assert_eq!(settle.dirty, Some(true));
        assert_eq!(settle.save_error, Some(Some("磁盘满".to_string())));
        assert_eq!(settle.note_base, None);
    }
}
