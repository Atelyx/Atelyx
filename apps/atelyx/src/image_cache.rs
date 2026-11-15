//! 图片显示缓存的内容指纹：判断同一内容键背后的本地文件何时已被换掉。

use std::path::{Path, PathBuf};

/// 本地文件图片的内容指纹：修改时间 + 字节数（同路径被覆盖后必变，不需要读内容）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct FileStamp {
    mtime: std::time::SystemTime,
    len: u64,
}

impl FileStamp {
    fn of(meta: &std::fs::Metadata) -> Option<Self> {
        Some(Self { mtime: meta.modified().ok()?, len: meta.len() })
    }
}

/// 一次图片载入的结果：字节 + 本地来源的载入时指纹（远程与空间来源没有本地文件，为 None）。
#[derive(Clone, Debug)]
pub(crate) struct LoadedImage {
    pub bytes: Vec<u8>,
    pub stamp: Option<FileStamp>,
}

impl LoadedImage {
    /// 无本地文件的来源（远程下载 / 服务端字节）只带内容。
    pub(crate) fn remote(bytes: Vec<u8>) -> Self {
        Self { bytes, stamp: None }
    }
}

/// 内容键背后的本地文件路径。键的取值域见插件界面图片的 `image_key`：`file:` 前缀 =
/// 单机绝对路径，其余绝对路径 = 插件私有目录，相对路径按仓库根解析（空间仓库没有本地根，
/// 传 None 即拿不到路径）；远程地址（含本机策略的 `local:` 前缀）没有本地文件。
pub(crate) fn local_path(root: Option<&Path>, key: &str) -> Option<PathBuf> {
    if is_remote(key) {
        return None;
    }
    if let Some(rest) = key.strip_prefix("file:") {
        return (!rest.is_empty()).then(|| PathBuf::from(rest));
    }
    let path = Path::new(key);
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }
    atelyx_core::vault::safe_join(root?, key, false).ok()
}

/// 远程地址（含本机策略前缀）：没有本地文件，重载代价是网络下载。
pub(crate) fn is_remote(key: &str) -> bool {
    key.starts_with("http://") || key.starts_with("https://") || key.starts_with("local:")
}

/// 内容键当前对应的本地文件指纹（无本地文件 / 不可读 = None）。
pub(crate) fn stamp_for(root: Option<&Path>, key: &str) -> Option<FileStamp> {
    let path = local_path(root, key)?;
    let meta = std::fs::metadata(path).ok()?;
    FileStamp::of(&meta)
}

/// 读本地图片并带回载入时指纹：读前后指纹不一致（读期间被替换）按失败处理，
/// 交给既有退避重试再读一次，避免把旧字节记成新内容。
pub(crate) fn read_local(path: &Path) -> Result<LoadedImage, String> {
    let before = std::fs::metadata(path).ok().and_then(|m| FileStamp::of(&m));
    let bytes = std::fs::read(path)
        .map_err(|e| format!("读取图片失败：{}（{e}）", path.display()))?;
    let after = std::fs::metadata(path).ok().and_then(|m| FileStamp::of(&m));
    match (before, after) {
        (Some(before), Some(after)) if before == after => {
            Ok(LoadedImage { bytes, stamp: Some(before) })
        }
        // 首次 stat 未拿到（刚创建的瞬时窗口）：以读后指纹收下，下一次校验仍能判失效
        (None, Some(after)) => Ok(LoadedImage { bytes, stamp: Some(after) }),
        _ => Err("图片在读取期间被改动，未采用".to_string()),
    }
}

/// 读仓库相对路径的图片（路径经 safe_join 限制在仓库根内）。
pub(crate) fn read_vault(root: &Path, rel: &str) -> Result<LoadedImage, String> {
    let path = atelyx_core::vault::safe_join(root, rel, false)?;
    read_local(&path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str, bytes: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("atelyx-image-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn 远程与空间来源没有本地路径() {
        let root = std::env::temp_dir();
        assert_eq!(local_path(Some(&root), "https://example.com/a.png"), None);
        assert_eq!(local_path(Some(&root), "local:http://127.0.0.1/a.png"), None);
    }

    #[test]
    fn 单机路径与仓库相对路径分别解析() {
        let root = std::env::temp_dir().join("atelyx-image-cache-root");
        std::fs::create_dir_all(root.join("assets")).unwrap();
        let abs = temp_file("abs.png", b"x");
        assert_eq!(local_path(None, &abs.to_string_lossy()), Some(abs.clone()));
        assert_eq!(
            local_path(Some(&root), &format!("file:{}", abs.display())),
            Some(abs.clone())
        );
        let rel = local_path(Some(&root), "assets/a.png").unwrap();
        assert!(rel.starts_with(&root));
        // 穿越路径不落到仓库外
        assert_eq!(local_path(Some(&root), "../outside.png"), None);
    }

    #[test]
    fn 同路径覆盖后指纹变化() {
        let path = temp_file("stamp.png", b"a");
        let first = stamp_for(None, &path.to_string_lossy()).unwrap();
        std::fs::write(&path, b"ab").unwrap();
        let second = stamp_for(None, &path.to_string_lossy()).unwrap();
        assert_ne!(first, second, "同路径内容换掉后指纹必须变化");
    }

    #[test]
    fn 读完即记指纹() {
        let path = temp_file("read.png", b"payload");
        let loaded = read_local(&path).unwrap();
        assert_eq!(loaded.bytes, b"payload");
        assert_eq!(loaded.stamp, stamp_for(None, &path.to_string_lossy()));
    }
}
