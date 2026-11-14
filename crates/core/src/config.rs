//! 配置文件读写的唯一出口：缺失 / 损坏策略显式声明，损坏必然留档。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde::de::DeserializeOwned;
use serde::Serialize;

/// 文件缺失时的行为。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OnMissing {
    /// 按 `Default` 继续（尚无配置）。
    Default,
    /// 报错。
    Error,
}

/// 内容损坏（读不出或解析失败）时的行为。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OnCorrupt {
    /// 原文改名备份后按 `Default` 继续，备份名回传调用方。
    BackupDefault,
    /// 原文改名备份后报错（数据在备份里，本体待用户处理后重写）。
    BackupError,
}

/// 把损坏的配置文件改名备份（原文完整保留供排查/人工取回），返回备份路径（失败 = None）。
///
/// 名字带随机后缀：同名目标在 Windows 上会被 rename 静默替换，固定名会让新的损坏冲掉旧备份。
/// `tag` 只用于日志前缀（便于按模块定位）。
pub fn backup(path: &Path, tag: &str) -> Option<PathBuf> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    let backup = path.with_file_name(format!("{name}.corrupt-{}", nanoid::nanoid!()));
    match std::fs::rename(path, &backup) {
        Ok(()) => Some(backup),
        Err(e) => {
            eprintln!("[{tag}] 损坏配置备份失败：{e}");
            None
        }
    }
}

/// 读并解析 JSON 配置，返回 `(值, 损坏备份文件名)`（`None` = 未发生损坏）。
///
/// 策略必须由调用方写明：这些文件都走「读 → 改 → 写」整文件覆盖，把损坏读成空状态
/// 会被下一次保存写回磁盘，等于一次手编辑失误就清空用户配置。损坏一律先备份，
/// 备份失败即 `Err`——留不下原文还继续，调用方紧接着就会把它覆盖掉。
/// 首字节 UTF-8 BOM 不算内容（见 `strip_bom`）。
pub fn read<T: DeserializeOwned + Default>(
    path: &Path,
    tag: &str,
    missing: OnMissing,
    corrupt: OnCorrupt,
) -> Result<(T, Option<String>), String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => match missing {
            OnMissing::Default => return Ok((T::default(), None)),
            OnMissing::Error => return Err(format!("缺少配置文件：{}", path.display())),
        },
        Err(e) => return Err(format!("读取配置失败（{}）：{e}", path.display())),
    };
    match serde_json::from_str::<T>(strip_bom(&raw)) {
        Ok(value) => Ok((value, None)),
        Err(e) => {
            let Some(backup) = backup(path, tag) else {
                return Err(format!(
                    "配置已损坏且原文备份失败（{}），已中止读取以免覆盖原文：{e}",
                    path.display()
                ));
            };
            let name = backup.file_name().map(|n| n.to_string_lossy().into_owned());
            match corrupt {
                OnCorrupt::BackupDefault => {
                    eprintln!(
                        "[{tag}] 配置损坏，已备份为 {}（按默认值继续）：{e}",
                        backup.display()
                    );
                    Ok((T::default(), name))
                }
                OnCorrupt::BackupError => Err(format!(
                    "配置已损坏，原文已备份为 {}，修复或删除后重试：{e}",
                    name.as_deref().unwrap_or_default()
                )),
            }
        }
    }
}

/// 缺失按默认值、损坏则备份原文并报错（读到空状态就会被写回的数据用这个）。
pub fn read_or_error<T: DeserializeOwned + Default>(path: &Path, tag: &str) -> Result<T, String> {
    read::<T>(path, tag, OnMissing::Default, OnCorrupt::BackupError).map(|(value, _)| value)
}

/// 缺失与损坏都按默认值继续（偏好类、可重建的清单用这个；损坏仍留档并记日志）。
pub fn read_or_default<T: DeserializeOwned + Default>(path: &Path, tag: &str) -> Result<T, String> {
    read::<T>(path, tag, OnMissing::Default, OnCorrupt::BackupDefault).map(|(value, _)| value)
}

/// 缺一不可：文件不存在或内容损坏都报错。
pub fn read_required<T: DeserializeOwned + Default>(path: &Path, tag: &str) -> Result<T, String> {
    read::<T>(path, tag, OnMissing::Error, OnCorrupt::BackupError).map(|(value, _)| value)
}

/// 原子写 JSON（pretty 覆盖整文件）。
pub fn write<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let json = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    crate::vault::atomic_write(path, &json)
}

/// 配置文件的按路径互斥锁表：读—改—写三步交错会让先写者的字段被后写者的旧基线覆盖。
/// 锁只覆盖该文件的读改写过程，不进任何长期状态；条目按路径保留（每条仅一把空锁）。
static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();

/// 取某个配置文件的写锁（同一路径共享一把）。
pub fn lock(path: &Path) -> Arc<Mutex<()>> {
    let table = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    // 锁表自身的锁必须短暂：中毒（持锁者 panic）时取回内层继续，不让一次 panic 永久拒绝写盘
    let mut table = table.lock().unwrap_or_else(|e| e.into_inner());
    table
        .entry(path.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// 持锁读改写：`f` 返回 true 才写盘。返回 `(是否写入, 损坏备份文件名)`。
///
/// 锁覆盖整个读改周期，不可在持锁期间 await。
pub fn update<T, F>(path: &Path, tag: &str, f: F) -> Result<(bool, Option<String>), String>
where
    T: DeserializeOwned + Default + Serialize,
    F: FnOnce(&mut T) -> bool,
{
    let guard = lock(path);
    let _held = guard.lock().unwrap_or_else(|e| e.into_inner());
    let (mut value, backup) =
        read::<T>(path, tag, OnMissing::Default, OnCorrupt::BackupDefault)?;
    if !f(&mut value) {
        return Ok((false, backup));
    }
    write(path, &value)?;
    Ok((true, backup))
}

/// 剥掉开头 UTF-8 BOM：它是编辑器写出的编码标记，不是 JSON 内容（留着会让解析首字符失败）。
fn strip_bom(raw: &str) -> &str {
    raw.strip_prefix('\u{feff}').unwrap_or(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::test_support::TempDir;

    fn backups(dir: &Path, name: &str) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(&format!("{name}.corrupt-")))
            .collect()
    }

    #[test]
    fn missing_file_follows_declared_policy() {
        let tmp = TempDir::new("cfg-missing");
        let path = tmp.join("cfg.json");
        let (value, backup): (Vec<String>, _) =
            read(&path, "cfg", OnMissing::Default, OnCorrupt::BackupError).unwrap();
        assert!(value.is_empty());
        assert!(backup.is_none());
        assert!(!path.exists(), "读不落盘");
        assert!(read::<Vec<String>>(&path, "cfg", OnMissing::Error, OnCorrupt::BackupError).is_err());
    }

    #[test]
    fn corrupt_file_is_backed_up_then_defaulted_or_reported() {
        let tmp = TempDir::new("cfg-corrupt");
        let path = tmp.join("cfg.json");
        std::fs::write(&path, "{ 坏 JSON").unwrap();

        let (value, backup): (Vec<String>, _) =
            read(&path, "cfg", OnMissing::Default, OnCorrupt::BackupDefault).unwrap();
        assert!(value.is_empty());
        assert!(!path.exists(), "损坏原文已让开，后续写盘不会覆盖它");
        assert_eq!(backup.as_deref(), Some(backups(&tmp, "cfg.json")[0].as_str()));
        assert_eq!(std::fs::read_to_string(tmp.join(&backups(&tmp, "cfg.json")[0])).unwrap(), "{ 坏 JSON");
    }

    #[test]
    fn corrupt_file_can_be_reported_after_backup() {
        let tmp = TempDir::new("cfg-strict");
        let path = tmp.join("cfg.json");
        std::fs::write(&path, "[1,").unwrap();
        let err = read::<Vec<String>>(&path, "cfg", OnMissing::Default, OnCorrupt::BackupError);
        assert!(err.is_err());
        assert_eq!(backups(&tmp, "cfg.json").len(), 1, "报错路径同样必须留档");
    }

    #[test]
    fn bom_prefixed_json_parses() {
        let tmp = TempDir::new("cfg-bom");
        let path = tmp.join("cfg.json");
        std::fs::write(&path, "\u{feff}[\"a\"]").unwrap();
        assert_eq!(read_or_default::<Vec<String>>(&path, "cfg").unwrap(), vec!["a".to_string()]);
    }

    #[test]
    fn update_holds_lock_across_read_modify_write() {
        let tmp = TempDir::new("cfg-update");
        let path = tmp.join("cfg.json");
        std::fs::write(&path, r#"{"n":0}"#).unwrap();
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for _ in 0..10 {
                        update::<HashMap<String, u32>, _>(&path, "cfg", |cfg| {
                            let n = cfg.entry("n".into()).or_insert(0);
                            *n += 1;
                            true
                        })
                        .unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let cfg: HashMap<String, u32> = read_or_default(&path, "cfg").unwrap();
        assert_eq!(cfg["n"], 40, "并发读改写不得丢改动");
    }

    #[test]
    fn update_skips_write_when_unchanged() {
        let tmp = TempDir::new("cfg-nochange");
        let path = tmp.join("cfg.json");
        std::fs::write(&path, r#"["a"]"#).unwrap();
        let (wrote, _) = update::<Vec<String>, _>(&path, "cfg", |_| false).unwrap();
        assert!(!wrote);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"["a"]"#,
            "无改动不留写盘痕迹"
        );
    }
}
