//! 写盘前合流本端正文与磁盘当前内容：纯函数，输入只有正文文本，不依赖会话与 I/O。

use super::merge3::{merge3, ConflictSide};

/// 写盘前合流本端正文与磁盘当前内容：`base` 为上次落盘基线，`None` = 首次保存。
/// 磁盘内容与基线相同（无外部修改）时直接取本端正文。
pub fn merge_before_write(base: Option<&str>, content: &str, disk: &str) -> String {
    match base {
        Some(base) if base != disk => merge3(base, content, disk, ConflictSide::Ours),
        _ => content.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_merges_only_when_disk_moved_from_base() {
        assert_eq!(merge_before_write(None, "new", "disk"), "new");
        assert_eq!(merge_before_write(Some("disk"), "new", "disk"), "new");
        assert_eq!(
            merge_before_write(Some("line1\nline2\n"), "line1\nline2\nlocal\n", "remote\nline2\n"),
            "remote\nline2\nlocal\n"
        );
    }
}
