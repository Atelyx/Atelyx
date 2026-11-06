//! `fs` 能力面：仓库外任意绝对路径与插件私有目录的文本 / 二进制读写、删除与列举。
//! 信任模型与进程托管一致（插件本就是任意权限的用户进程）——本面给的是统一出口与审计事实，不构成沙箱边界。

use std::path::Path;

use atelyx_plugin_protocol::method::{FsEntry, FsEntryKind};

/// 插件私有目录 `<插件目录>/data`：首次访问即建立，返回其绝对路径。
pub(crate) fn private_dir(dir: &Path) -> Result<String, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("创建插件私有目录失败：{e}"))?;
    Ok(dir.to_string_lossy().into_owned())
}

pub(crate) fn read_text(path: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("读取文件失败（{path}）：{e}"))
}

pub(crate) fn write_text(path: &str, content: &str) -> Result<(), String> {
    crate::vault::atomic_write(Path::new(path), content)
        .map_err(|e| format!("写入文件失败（{path}）：{e}"))
}

pub(crate) fn read_binary(path: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("读取文件失败（{path}）：{e}"))
}

pub(crate) fn write_binary(path: &str, bytes: &[u8]) -> Result<(), String> {
    crate::vault::atomic_write_bytes(Path::new(path), bytes)
        .map_err(|e| format!("写入文件失败（{path}）：{e}"))
}

/// 删除文件或目录（目录递归删除）；路径不存在即报错，不让调用方把「没删掉」当成「删成功」。
/// 类型按 `symlink_metadata` 判定：符号链接按文件删除，不跟随到目标去删它里面的内容。
pub(crate) fn remove(path: &str) -> Result<(), String> {
    let target = Path::new(path);
    let meta = std::fs::symlink_metadata(target)
        .map_err(|e| format!("路径不存在或不可访问（{path}）：{e}"))?;
    if meta.is_dir() {
        std::fs::remove_dir_all(target).map_err(|e| format!("删除目录失败（{path}）：{e}"))
    } else {
        std::fs::remove_file(target).map_err(|e| format!("删除文件失败（{path}）：{e}"))
    }
}

pub(crate) fn exists(path: &str) -> bool {
    Path::new(path).exists()
}

/// 列举目录条目（按名字排序，顺序稳定）；目录不存在即报错。
pub(crate) fn list(path: &str) -> Result<Vec<FsEntry>, String> {
    let entries = std::fs::read_dir(path).map_err(|e| format!("读取目录失败（{path}）：{e}"))?;
    let mut out: Vec<FsEntry> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("读取目录项失败：{e}"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = if entry.path().is_dir() { FsEntryKind::Dir } else { FsEntryKind::File };
        out.push(FsEntry { name, kind });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::test_support::TempDir;

    #[test]
    fn text_roundtrip_creates_parent_dirs() {
        let tmp = TempDir::new("plugin-fs-text");
        let nested = tmp.join("a").join("b").join("note.txt");
        let path = nested.to_string_lossy().into_owned();
        assert!(!exists(&path));
        write_text(&path, "内容").expect("写入应自动建父目录");
        assert!(exists(&path));
        assert_eq!(read_text(&path).unwrap(), "内容");
    }

    #[test]
    fn binary_roundtrip() {
        let tmp = TempDir::new("plugin-fs-bin");
        let path = tmp.join("blob.bin").to_string_lossy().into_owned();
        write_binary(&path, &[0, 159, 146, 150]).unwrap();
        assert_eq!(read_binary(&path).unwrap(), vec![0, 159, 146, 150]);
    }

    #[test]
    fn list_is_sorted_and_classifies_kind() {
        let tmp = TempDir::new("plugin-fs-list");
        std::fs::create_dir_all(tmp.join("dir")).unwrap();
        std::fs::write(tmp.join("b.txt"), b"x").unwrap();
        std::fs::write(tmp.join("a.txt"), b"x").unwrap();
        let entries = list(&tmp.to_string_lossy()).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a.txt", "b.txt", "dir"]);
        assert_eq!(entries[2].kind, FsEntryKind::Dir);
    }

    #[test]
    fn remove_handles_file_and_directory_and_reports_missing() {
        let tmp = TempDir::new("plugin-fs-remove");
        let file = tmp.join("f.txt");
        std::fs::write(&file, b"x").unwrap();
        remove(&file.to_string_lossy()).unwrap();
        assert!(!file.exists());
        let dir = tmp.join("d");
        std::fs::create_dir_all(dir.join("inner")).unwrap();
        remove(&dir.to_string_lossy()).unwrap();
        assert!(!dir.exists());
        // 不存在即报错（不把「没删掉」当成功）
        assert!(remove(&tmp.join("missing").to_string_lossy()).is_err());
    }

    #[test]
    fn private_dir_is_created_and_reported() {
        let tmp = TempDir::new("plugin-fs-private");
        let dir = tmp.join("plugins").join("com.a.one").join("data");
        let path = private_dir(&dir).unwrap();
        assert!(Path::new(&path).is_dir());
    }

    #[test]
    fn read_missing_file_fails_with_readable_reason() {
        let tmp = TempDir::new("plugin-fs-missing");
        let err = read_text(&tmp.join("nope.txt").to_string_lossy()).unwrap_err();
        assert!(err.contains("nope.txt"), "错误原因应指明路径：{err}");
    }
}
