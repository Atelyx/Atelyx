//! 写盘前合流本端正文与磁盘当前内容：纯函数，输入只有正文文本，不依赖会话与 I/O。

use super::merge3::{merge3_checked, ConflictSide, Unaligned};

/// 写盘前合流的结果：冲突 = 两侧改动无法安全合并，调用方不得自行选边覆盖磁盘。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    Merged(String),
    Conflict,
}

/// 写盘前合流本端正文与磁盘当前内容：`base` 为上次落盘基线，`None` = 首次保存。
/// 磁盘内容与基线相同（无外部修改）时直接取本端正文；无法安全合流时交回冲突，不产出半份正文。
pub fn merge_before_write(base: Option<&str>, content: &str, disk: &str) -> MergeOutcome {
    match base {
        Some(base) if base != disk => {
            match merge3_checked(base, content, disk, ConflictSide::Ours) {
                Ok(merged) => MergeOutcome::Merged(merged),
                Err(Unaligned) => MergeOutcome::Conflict,
            }
        }
        _ => MergeOutcome::Merged(content.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_merges_only_when_disk_moved_from_base() {
        assert_eq!(
            merge_before_write(None, "new", "disk"),
            MergeOutcome::Merged("new".into())
        );
        assert_eq!(
            merge_before_write(Some("disk"), "new", "disk"),
            MergeOutcome::Merged("new".into())
        );
        assert_eq!(
            merge_before_write(Some("line1\nline2\n"), "line1\nline2\nlocal\n", "remote\nline2\n"),
            MergeOutcome::Merged("remote\nline2\nlocal\n".into())
        );
    }

    /// 长文档两侧无法安全对齐时交回冲突：调用方据此保留两侧、不覆盖磁盘。
    #[test]
    fn unaligned_sides_report_conflict() {
        let base = "x\n".repeat(2000);
        let ours: String = "y\n".repeat(2000);
        let disk = format!("{}z\n", "x\n".repeat(1999));
        assert_eq!(merge_before_write(Some(&base), &ours, &disk), MergeOutcome::Conflict);
    }
}
