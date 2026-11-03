//! 测试与冒烟产物的临时根：统一落点、唯一命名与 RAII 清理（失败重试并留日志）。

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// 显式临时根环境变量（仓库本地经 `.cargo` 配置指到 `target/tmp`）。
pub const ROOT_ENV: &str = "ATELYX_TMP_ROOT";

/// 临时根：显式配置优先；否则从产物路径能定位 `target/` 时用 `target/tmp`；再否则系统临时目录。
pub fn root() -> PathBuf {
    std::env::var_os(ROOT_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| target_dir().map(|dir| dir.join("tmp")))
        .unwrap_or_else(std::env::temp_dir)
}

/// 产物路径上的 `target/` 祖先（测试 / 示例产物都在其 `debug/{deps,examples}` 下）。
fn target_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors()
        .find(|path| path.file_name().is_some_and(|name| name == "target"))
        .map(Path::to_path_buf)
}

/// 测试用临时目录：创建即定位到 [`root`]，`Drop` 递归清理（panic / 提前返回同样触发）。
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = root();
        std::fs::create_dir_all(&base).expect("创建测试临时根");
        for _ in 0..32 {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0);
            let dir = base.join(format!(
                "atelyx-{tag}-{}-{nanos}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&dir) {
                Ok(()) => return TempDir(dir),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("创建测试临时目录失败（{}）：{error}", dir.display()),
            }
        }
        panic!("创建测试临时目录失败：命名连续冲突");
    }
}

impl std::ops::Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // 清理失败必须可见：被静默吞掉就会累积成磁盘垃圾
        if let Err(error) = remove_dir_all_with_retry(&self.0) {
            eprintln!("临时目录清理失败（{}）：{error}", self.0.display());
        }
    }
}

/// Windows 上动态库仍被装载时删除会短暂失败，按短间隔重试。
fn remove_dir_all_with_retry(path: &Path) -> io::Result<()> {
    const ATTEMPTS: usize = 8;
    const RETRY: Duration = Duration::from_millis(50);
    let mut last = None;
    for attempt in 0..ATTEMPTS {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                last = Some(error);
                if attempt + 1 < ATTEMPTS {
                    std::thread::sleep(RETRY);
                }
            }
        }
    }
    Err(last.expect("至少尝试一次删除"))
}

/// 目录树下残留的 `.tmp` 文件（绝对路径，排序）。
pub fn tmp_leftovers(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        for entry in std::fs::read_dir(&cur).unwrap().filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.to_string_lossy().ends_with(".tmp") {
                out.push(path.to_string_lossy().into_owned());
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use super::{root, TempDir, ROOT_ENV};

    #[test]
    fn temp_root_is_the_configured_location() {
        let location = root();
        assert!(location.is_absolute(), "临时根必须是绝对路径：{}", location.display());
        if let Some(configured) = std::env::var_os(ROOT_ENV) {
            assert_eq!(location, PathBuf::from(configured), "显式配置必须生效");
            return;
        }
        // 未显式配置时：产物在 target 下则落到 target/tmp，否则回退系统临时目录
        let expected = std::env::temp_dir();
        assert!(
            location == expected
                || (location.file_name().is_some_and(|name| name == "tmp")
                    && location.parent().is_some_and(|parent| {
                        parent.file_name().is_some_and(|name| name == "target")
                    })),
            "临时根落点不可辨认：{}",
            location.display()
        );
    }

    #[test]
    fn temp_dir_lives_under_root_and_cleans_up() {
        let tmp = TempDir::new("root-check");
        assert!(tmp.starts_with(root()));
        assert!(tmp.is_dir(), "临时目录应在配置的根下真实创建");
        let path = tmp.to_path_buf();
        drop(tmp);
        assert!(!path.exists(), "正常返回后临时目录应已删除");
    }

    #[test]
    fn panic_still_cleans_temp_dir() {
        let seen = Arc::new(Mutex::new(PathBuf::new()));
        let capture = seen.clone();
        let result = std::panic::catch_unwind(move || {
            let tmp = TempDir::new("panic-check");
            *capture.lock().unwrap() = tmp.to_path_buf();
            panic!("模拟断言失败");
        });
        assert!(result.is_err());
        let path = seen.lock().unwrap().clone();
        assert!(!path.exists(), "panic 展开时临时目录也必须清掉");
    }
}
