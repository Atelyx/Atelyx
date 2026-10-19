//! 插件安装 / 卸载 / 上一代轮换。目录安装与 zip 安装都先落到临时目录、校验通过后再落位：
//! 暂存与校验失败不动旧版本；轮换后的落位失败按回滚恢复旧内容（插件数据始终保留）。
//! 安装完成只落位内容，**不置启用标记**（是否启用由用户在设置页决定）；zip 解压带路径穿越、
//! 符号链接、压缩比与总量四道防御。

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use atelyx_plugin_protocol::manifest::{Manifest, MANIFEST_FILE};

use super::layout;
use super::manifest;

/// 单条目压缩比上限：正常插件二进制接近不可压缩（比值低），解压炸弹可达 1000:1 以上。
const MAX_ENTRY_RATIO: u64 = 100;
/// 压缩比判定的最小原始体积：小文件天然高比值，低于此值不参与比值判定。
const RATIO_MIN_SIZE: u64 = 64 * 1024;
/// 解压总量上限（字节）。绝对上限只作兜底，真正拦炸弹的是压缩比。
const MAX_TOTAL_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// 删除目录的重试次数与间隔：从容忍「刚被终止的进程尚未释放文件占用」的短暂失败。
const REMOVE_RETRY_ATTEMPTS: usize = 10;
const REMOVE_RETRY_INTERVAL: Duration = Duration::from_millis(100);
/// 目录安装的复制层级上限（正常插件目录远低于此）。
const MAX_COPY_DEPTH: usize = 32;

/// 从本地目录安装：目录根须含合法 `plugin.json`。返回安装的插件 id。
///
/// 记下源目录绝对路径（`source`），供「从来源更新」再次安装；内容先落临时目录，校验与入口
/// 权限就位后才动旧版本。
pub(crate) fn install_from_dir(root: &Path, src: &Path) -> Result<String, String> {
    let manifest = manifest::read_manifest(src)?;
    let id = manifest.id.clone();
    let source = source_outside_root(root, src)?;
    let temp = layout::temp_dir(root);
    let staged = (|| -> Result<(), String> {
        copy_tree(src, &temp)?;
        reject_reserved(&temp)?;
        grant_entry_execute(&temp, &manifest)?;
        // 来源记录写进暂存内容：与这一代内容一起落位，换代时随旧内容一起轮换掉
        let recorded = source.to_string_lossy();
        std::fs::write(temp.join(layout::SOURCE_NAME), recorded.as_bytes())
            .map_err(|e| format!("记录安装来源失败：{e}"))?;
        place(root, &id, &temp)
    })();
    if staged.is_err() {
        let _ = std::fs::remove_dir_all(&temp);
    }
    staged?;
    Ok(id)
}

/// 从 zip 安装：解压到临时目录（带防御），定位含 manifest 的根后落位。返回插件 id。
///
/// `expected_id` 为来源方声明的插件 id（市场条目）：与 zip 内 manifest 的 id 不一致即拒收——
/// 落位目标是 manifest 的 id，不校验就会让一份安装包覆盖掉另一个插件的目录与私有数据。
pub(crate) fn install_from_zip(
    root: &Path,
    zip_path: &Path,
    expected_id: Option<&str>,
) -> Result<String, String> {
    let temp = layout::temp_dir(root);
    let staged = (|| -> Result<String, String> {
        extract_zip(zip_path, &temp)?;
        let plugin_root = locate_plugin_root(&temp)?;
        reject_reserved(&plugin_root)?;
        let manifest = manifest::read_manifest(&plugin_root)?;
        if let Some(expected) = expected_id {
            if manifest.id != expected {
                return Err(format!(
                    "安装包清单 id（{}）与来源条目 id（{expected}）不一致，拒收",
                    manifest.id
                ));
            }
        }
        grant_entry_execute(&plugin_root, &manifest)?;
        let id = manifest.id.clone();
        // 来源记录不随压缩包带进来：旧记录随上一代轮换掉，避免「从来源更新」拿旧目录内容
        // 覆盖这次的压缩包内容
        place(root, &id, &plugin_root)?;
        Ok(id)
    })();
    let _ = std::fs::remove_dir_all(&temp);
    staged
}

/// 校验源目录与插件安装根不重叠，返回源目录的规范化绝对路径。
///
/// 安装临时目录建在插件根内：源目录若把插件根包在里面，`copy_tree` 会把正在写入的临时目录
/// 自己也拷进去，逐层加深直到路径超长或磁盘写满。源目录落在插件根内则是已安装副本，也不收。
fn source_outside_root(root: &Path, src: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(root).map_err(|e| format!("创建插件根目录失败：{e}"))?;
    let root_abs = dunce::canonicalize(root).map_err(|e| format!("解析插件根目录失败：{e}"))?;
    let src_abs = dunce::canonicalize(src).map_err(|e| format!("解析源目录失败：{e}"))?;
    if root_abs.starts_with(&src_abs) {
        return Err(format!(
            "所选目录包含插件安装目录，无法作为安装源：{}",
            src_abs.display()
        ));
    }
    if src_abs.starts_with(&root_abs) {
        return Err(format!(
            "所选目录位于插件安装目录内（已是安装后的副本）：{}",
            src_abs.display()
        ));
    }
    Ok(src_abs)
}

/// 入口补可执行位：zip 只是按字节落盘，Windows 打的包也不带 unix 权限位，装完启动会
/// Permission denied。入口是插件唯一要执行的路径，安装时统一补上（Windows 无此概念）。
fn grant_entry_execute(dir: &Path, manifest: &Manifest) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Some(entry) = manifest.entry.for_current_platform() else {
            return Ok(());
        };
        let path = dir.join(entry);
        let mut perms = std::fs::metadata(&path)
            .map_err(|e| format!("读取入口文件失败：{e}"))?
            .permissions();
        let mode = perms.mode();
        if mode & 0o111 == 0 {
            perms.set_mode(mode | 0o111);
            std::fs::set_permissions(&path, perms)
                .map_err(|e| format!("设置入口可执行权限失败：{e}"))?;
        }
    }
    #[cfg(not(unix))]
    let _ = (dir, manifest);
    Ok(())
}

/// 卸载：删整个插件目录（安装内容、私有数据、上一代与启用标记都在其内）。调用方须先停插件。
pub(crate) fn uninstall(root: &Path, id: &str) -> Result<(), String> {
    remove_dir_all_with_retry(&layout::plugin_dir(root, id))
}

/// 把已就绪的 `staged` 目录落位为 `<root>/<id>/`：旧内容轮换进 `previous/<ts>/`（只留一代），
/// 宿主状态项（`data/` `logs/` 与启用标记）原位保留。落位失败即回滚到轮换前状态。
fn place(root: &Path, id: &str, staged: &Path) -> Result<(), String> {
    let dest = layout::plugin_dir(root, id);
    if !dest.exists() {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建插件根目录失败：{e}"))?;
        }
        return std::fs::rename(staged, &dest).map_err(|e| format!("落位插件目录失败：{e}"));
    }
    // 先清掉上一代（只留一代），再把除宿主状态项外的旧内容搬进新一代码次
    let previous = layout::previous_dir(root, id);
    remove_dir_all_with_retry(&previous)?;
    let generation = previous.join(now_millis().to_string());
    std::fs::create_dir_all(&generation).map_err(|e| format!("创建上一代目录失败：{e}"))?;
    for entry in std::fs::read_dir(&dest).map_err(|e| format!("读取插件目录失败：{e}"))? {
        let entry = entry.map_err(|e| format!("读取目录项失败：{e}"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_preserved(&name) {
            continue;
        }
        std::fs::rename(entry.path(), generation.join(&name))
            .map_err(|e| format!("轮换旧版本失败：{e}"))?;
    }
    if let Err(e) = move_entries(staged, &dest) {
        rollback_placement(&dest, &generation);
        return Err(e);
    }
    let _ = std::fs::remove_dir_all(staged);
    Ok(())
}

/// 落位失败回滚：清掉已移入的新内容（旧内容已全部轮换出去，非宿主状态项即新内容），
/// 再把轮换出去的旧内容搬回原位——宁可退回旧版本，也不留「半新半旧」的插件目录。
fn rollback_placement(dest: &Path, generation: &Path) {
    if let Ok(entries) = std::fs::read_dir(dest) {
        for entry in entries.flatten() {
            if !is_preserved(&entry.file_name().to_string_lossy()) {
                let _ = remove_entry(&entry.path());
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(generation) {
        for entry in entries.flatten() {
            let _ = std::fs::rename(entry.path(), dest.join(entry.file_name()));
        }
    }
    let _ = std::fs::remove_dir_all(generation);
}

/// 删除一个路径（目录或文件）。
fn remove_entry(path: &Path) -> std::io::Result<()> {
    if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// 把 `from` 的顶层条目逐个移入已存在的 `to`（同卷 rename）。
fn move_entries(from: &Path, to: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(from).map_err(|e| format!("读取目录失败：{e}"))? {
        let entry = entry.map_err(|e| format!("读取目录项失败：{e}"))?;
        std::fs::rename(entry.path(), to.join(entry.file_name()))
            .map_err(|e| format!("落位插件内容失败：{e}"))?;
    }
    Ok(())
}

/// 拒收插件自带的宿主保留顶层名（否则会与宿主管理的 data / previous / enabled 撞名或合并）。
fn reject_reserved(staged: &Path) -> Result<(), String> {
    for name in layout::RESERVED_TOP_LEVEL {
        if staged.join(name).exists() {
            return Err(format!("插件内容含宿主保留路径，拒收：{name}"));
        }
    }
    Ok(())
}

/// 换代时原位保留的宿主管理项（见 [`layout::PRESERVED_ON_UPDATE`]）。
fn is_preserved(name: &str) -> bool {
    layout::PRESERVED_ON_UPDATE.contains(&name)
}

/// 解压目录中定位插件根：根含 manifest，或唯一顶层目录含 manifest。
fn locate_plugin_root(temp: &Path) -> Result<PathBuf, String> {
    if temp.join(MANIFEST_FILE).is_file() {
        return Ok(temp.to_path_buf());
    }
    let mut subdirs: Vec<PathBuf> = std::fs::read_dir(temp)
        .map_err(|e| format!("读取解压目录失败：{e}"))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    subdirs.sort();
    let mut with_manifest = subdirs.into_iter().filter(|p| p.join(MANIFEST_FILE).is_file());
    match (with_manifest.next(), with_manifest.next()) {
        (Some(found), None) => Ok(found),
        _ => Err(format!("压缩包根目录未找到唯一的 {MANIFEST_FILE}")),
    }
}

/// 递归复制目录；符号链接拒收（指向何处不可控，跟随复制会越出源目录）。
fn copy_tree(src: &Path, dest: &Path) -> Result<(), String> {
    copy_tree_inner(src, dest, 0)
}

/// 层级上限：目录联接（Windows junction 不被 `is_symlink` 识别）可以指回安装根，跟随复制会无限
/// 加深直到路径超长或磁盘写满；超过上限即拒收，不赌源目录里没有环。
fn copy_tree_inner(src: &Path, dest: &Path, depth: usize) -> Result<(), String> {
    if depth > MAX_COPY_DEPTH {
        return Err(format!("源目录层级过深，拒收：{}", src.display()));
    }
    std::fs::create_dir_all(dest).map_err(|e| format!("创建目录失败：{e}"))?;
    let entries = std::fs::read_dir(src).map_err(|e| format!("读取目录失败：{e}"))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("读取目录项失败：{e}"))?;
        let ty = entry.file_type().map_err(|e| format!("读取目录项类型失败：{e}"))?;
        let to = dest.join(entry.file_name());
        if ty.is_symlink() {
            return Err(format!("源目录含符号链接，拒收：{}", entry.path().display()));
        }
        if ty.is_dir() {
            copy_tree_inner(&entry.path(), &to, depth + 1)?;
        } else if ty.is_file() {
            std::fs::copy(entry.path(), &to).map_err(|e| format!("复制文件失败：{e}"))?;
        }
    }
    Ok(())
}

/// 解压 zip：先按中央目录声明做路径穿越 / 压缩比 / 总量预检，再逐条解压并实计数兜底
/// （压缩包里的声明值可以撒谎，实计数是最终阻止写满磁盘的闸门）。
fn extract_zip(zip_path: &Path, dest: &Path) -> Result<(), String> {
    let file = std::fs::File::open(zip_path).map_err(|e| format!("打开压缩包失败：{e}"))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("读取压缩包失败：{e}"))?;
    precheck(&mut archive)?;

    let mut written: u64 = 0;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|e| format!("读取压缩包条目失败：{e}"))?;
        let name = entry.name().to_string();
        let rel = safe_rel_path(&name)?;
        let out = dest.join(&rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| format!("创建目录失败：{e}"))?;
            continue;
        }
        if entry.is_symlink() {
            return Err(format!("压缩包含符号链接，拒收：{name}"));
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败：{e}"))?;
        }
        let mut sink = std::fs::File::create(&out).map_err(|e| format!("创建文件失败：{e}"))?;
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = entry.read(&mut buf).map_err(|e| format!("解压条目失败：{e}"))?;
            if n == 0 {
                break;
            }
            written = written
                .checked_add(n as u64)
                .ok_or_else(|| "解压总量溢出".to_string())?;
            if written > MAX_TOTAL_BYTES {
                return Err(total_exceeded());
            }
            std::io::Write::write_all(&mut sink, &buf[..n])
                .map_err(|e| format!("写入解压文件失败：{e}"))?;
        }
    }
    Ok(())
}

/// 中央目录预检：声明值即可拦下绝大多数炸弹与穿越（免去解压到磁盘才发现）。
fn precheck(archive: &mut zip::ZipArchive<std::fs::File>) -> Result<(), String> {
    let mut declared_total: u64 = 0;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|e| format!("读取压缩包条目失败：{e}"))?;
        let name = entry.name().to_string();
        safe_rel_path(&name)?;
        let size = entry.size();
        let compressed = entry.compressed_size();
        if size >= RATIO_MIN_SIZE && compressed > 0 && size / compressed > MAX_ENTRY_RATIO {
            return Err(format!("压缩比过高，疑似解压炸弹，拒收：{name}"));
        }
        declared_total = declared_total
            .checked_add(size)
            .ok_or_else(|| "压缩包声明总量溢出".to_string())?;
        if declared_total > MAX_TOTAL_BYTES {
            return Err(total_exceeded());
        }
    }
    Ok(())
}

fn total_exceeded() -> String {
    format!(
        "解压总量超过上限（{} GiB），拒收",
        MAX_TOTAL_BYTES / (1024 * 1024 * 1024)
    )
}

/// 条目名转仓库内安全相对路径：拒绝对路径、盘符前缀与 `..`（反斜杠按分隔符归一）。
fn safe_rel_path(name: &str) -> Result<PathBuf, String> {
    if name.trim().is_empty() {
        return Err("压缩包条目名为空".to_string());
    }
    if name.starts_with('/') || name.starts_with('\\') {
        return Err(format!("压缩包条目为绝对路径，拒收：{name}"));
    }
    let normalized = name.replace('\\', "/");
    if normalized.as_bytes().get(1) == Some(&b':') {
        return Err(format!("压缩包条目含盘符前缀，拒收：{name}"));
    }
    let mut rel = PathBuf::new();
    for part in normalized.split('/') {
        match part {
            "" | "." => continue,
            ".." => return Err(format!("压缩包条目含路径穿越，拒收：{name}")),
            _ => rel.push(part),
        }
    }
    if rel.as_os_str().is_empty() {
        return Err(format!("压缩包条目路径为空：{name}"));
    }
    Ok(rel)
}

/// 删除目录，容忍「刚被终止的进程尚未释放文件占用」的短暂失败（Windows 上常见）。
fn remove_dir_all_with_retry(dir: &Path) -> Result<(), String> {
    let mut last: Option<std::io::Error> = None;
    for _ in 0..REMOVE_RETRY_ATTEMPTS {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                if !dir.exists() {
                    return Ok(());
                }
                last = Some(e);
                std::thread::sleep(REMOVE_RETRY_INTERVAL);
            }
        }
    }
    Err(format!(
        "删除目录失败（{}）：{}",
        dir.display(),
        last.map(|e| e.to_string()).unwrap_or_default()
    ))
}

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::test_support::TempDir;
    use std::io::Write;

    fn manifest_json(id: &str) -> String {
        format!(
            r#"{{"id":"{id}","name":"Demo","version":"1.0.0","apiVersion":1,"entry":{{"windows":"app.exe","linux":"app"}},"permissions":["state"]}}"#
        )
    }

    fn entry_name() -> &'static str {
        if cfg!(windows) {
            "app.exe"
        } else {
            "app"
        }
    }

    /// 造一个可安装的插件目录（含平台入口占位文件）。
    fn make_plugin(dir: &Path, id: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(MANIFEST_FILE), manifest_json(id)).unwrap();
        std::fs::write(dir.join(entry_name()), b"stub").unwrap();
    }

    fn zip_entries(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        for (name, bytes) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap();
    }

    /// 插件安装根。源目录一律放在安装根之外：目录安装拒收与安装根重叠的源。
    fn plugin_root(tmp: &Path) -> PathBuf {
        tmp.join("plugins")
    }

    fn demo_zip(tmp: &Path, id: &str) -> PathBuf {
        let zip = tmp.join("demo.zip");
        zip_entries(
            &zip,
            &[(MANIFEST_FILE, manifest_json(id).as_bytes()), (entry_name(), b"stub")],
        );
        zip
    }

    #[test]
    fn safe_rel_path_rejects_escapes() {
        for bad in ["/abs.txt", "..\\evil", "a/../../b", "C:/x", "..", ""] {
            assert!(safe_rel_path(bad).is_err(), "应拒绝：{bad}");
        }
        assert_eq!(safe_rel_path("a/b/c.txt").unwrap(), PathBuf::from("a/b/c.txt"));
    }

    #[test]
    fn install_from_dir_places_without_enabling_and_records_source() {
        let tmp = TempDir::new("plugin-install-dir");
        let root = plugin_root(&tmp);
        let src = tmp.join("src");
        make_plugin(&src, "com.example.demo");
        let id = install_from_dir(&root, &src).expect("目录安装应成功");
        assert_eq!(id, "com.example.demo");
        assert!(layout::plugin_dir(&root, &id).join(MANIFEST_FILE).is_file());
        assert!(!layout::is_enabled(&root, &id), "安装只落位内容，不自动启用");
        // 来源记规范化绝对路径（相对路径在后续「从来源更新」时才不会随工作目录漂移）
        let recorded = layout::read_source(&root, &id).expect("应记录安装来源");
        assert_eq!(PathBuf::from(recorded), dunce::canonicalize(&src).unwrap());
    }

    #[test]
    fn dir_install_rejects_source_overlapping_root() {
        let tmp = TempDir::new("plugin-install-overlap");
        let root = plugin_root(&tmp);
        // 选插件根本身：临时目录建在其中，复制会把自己逐层拷进去
        make_plugin(&root, "com.example.root");
        let err = install_from_dir(&root, &root).expect_err("源目录 = 插件根应被拒");
        assert!(err.contains("包含插件安装目录"), "{err}");

        // 选已安装插件的目录：那是安装后的副本，不是可安装的内容
        let nested = layout::plugin_dir(&root, "com.example.target");
        make_plugin(&nested, "com.example.target");
        let err = install_from_dir(&root, &nested).expect_err("源目录在插件根内应被拒");
        assert!(err.contains("插件安装目录内"), "{err}");
        assert!(!layout::plugin_dir(&root, "com.example.copy").exists(), "拒收不动安装根");
    }

    #[test]
    fn reinstall_rotates_content_and_preserves_data() {
        let tmp = TempDir::new("plugin-install-previous");
        let root = plugin_root(&tmp);
        let src = tmp.join("src");
        make_plugin(&src, "com.example.demo");
        let id = install_from_dir(&root, &src).unwrap();
        let plugin = layout::plugin_dir(&root, &id);
        std::fs::write(plugin.join("marker"), "v1").unwrap();
        // 已启用的插件重装：启用标记属于宿主管理项，更新必须原位保留
        layout::set_enabled(&root, &id, true).unwrap();
        // 宿主私有数据：更新必须保留
        std::fs::create_dir_all(layout::data_dir(&root, &id)).unwrap();
        std::fs::write(layout::data_dir(&root, &id).join("state.json"), "{\"keep\":true}").unwrap();

        install_from_dir(&root, &src).unwrap();

        // 旧内容进上一代且只留一代；新内容就位（旧 marker 不在当前目录）
        let generations: Vec<_> = std::fs::read_dir(layout::previous_dir(&root, &id))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(generations.len(), 1);
        assert!(!plugin.join("marker").exists());
        assert!(plugin.join(MANIFEST_FILE).is_file());
        // 私有数据、启用标记与来源留在原位
        assert_eq!(
            std::fs::read_to_string(layout::data_dir(&root, &id).join("state.json")).unwrap(),
            "{\"keep\":true}"
        );
        assert!(layout::is_enabled(&root, &id));
        assert!(layout::read_source(&root, &id).is_some());
        // 上一代里不含宿主状态项；来源记录随旧内容一起轮换（它描述的是那一代内容从哪来）
        let generation = generations[0].path();
        for preserved in layout::PRESERVED_ON_UPDATE {
            assert!(
                !generation.join(preserved).exists(),
                "上一代不应含宿主状态项：{preserved}"
            );
        }
        assert!(generation.join(layout::SOURCE_NAME).is_file(), "旧来源应随旧内容轮换");
    }

    #[test]
    fn install_rejects_reserved_content() {
        let tmp = TempDir::new("plugin-install-reserved");
        let root = plugin_root(&tmp);
        for reserved in layout::RESERVED_TOP_LEVEL {
            let src = tmp.join(&format!("src-{reserved}"));
            make_plugin(&src, "com.example.demo");
            std::fs::create_dir_all(src.join(reserved)).unwrap();
            assert!(
                install_from_dir(&root, &src).is_err(),
                "插件自带保留路径 {reserved} 应被拒"
            );
        }
    }

    #[test]
    fn install_rejects_invalid_and_version_mismatch() {
        let tmp = TempDir::new("plugin-install-bad");
        let root = plugin_root(&tmp);
        let bad_id = tmp.join("bad-id");
        std::fs::create_dir_all(&bad_id).unwrap();
        std::fs::write(
            bad_id.join(MANIFEST_FILE),
            r#"{"id":"bad","name":"x","version":"1","apiVersion":1}"#,
        )
        .unwrap();
        assert!(install_from_dir(&root, &bad_id).is_err(), "非反域名 id 应被拒");

        let bad_version = tmp.join("bad-version");
        std::fs::create_dir_all(&bad_version).unwrap();
        std::fs::write(
            bad_version.join(MANIFEST_FILE),
            r#"{"id":"com.example.demo","name":"x","version":"1","apiVersion":99,"entry":{"windows":"a.exe","linux":"a"}}"#,
        )
        .unwrap();
        std::fs::write(bad_version.join(entry_name()), b"stub").unwrap();
        assert!(install_from_dir(&root, &bad_version).is_err(), "协议版本不一致应被拒");

        let no_entry = tmp.join("no-entry");
        std::fs::create_dir_all(&no_entry).unwrap();
        std::fs::write(
            no_entry.join(MANIFEST_FILE),
            r#"{"id":"com.example.demo","name":"x","version":"1","apiVersion":1,"entry":{"windows":"app.exe","linux":"app"}}"#,
        )
        .unwrap();
        assert!(install_from_dir(&root, &no_entry).is_err(), "入口缺失应被拒");
    }

    #[test]
    fn install_zip_and_uninstall() {
        let tmp = TempDir::new("plugin-install-zip");
        let root = plugin_root(&tmp);
        let zip = demo_zip(&tmp, "com.example.demo");
        let id = install_from_zip(&root, &zip, None).expect("zip 安装应成功");
        assert_eq!(id, "com.example.demo");
        assert!(!layout::is_enabled(&root, &id), "安装只落位内容，不自动启用");
        uninstall(&root, &id).unwrap();
        assert!(!layout::plugin_dir(&root, &id).exists());
    }

    #[test]
    fn zip_install_drops_stale_source_record() {
        let tmp = TempDir::new("plugin-zip-source");
        let root = plugin_root(&tmp);
        let src = tmp.join("src");
        make_plugin(&src, "com.example.demo");
        let id = install_from_dir(&root, &src).unwrap();
        assert!(layout::read_source(&root, &id).is_some());

        let zip = demo_zip(&tmp, "com.example.demo");
        install_from_zip(&root, &zip, None).unwrap();
        assert!(
            layout::read_source(&root, &id).is_none(),
            "内容改为来自压缩包后，本地目录不再代表当前内容"
        );
    }

    #[test]
    fn zip_install_checks_expected_id() {
        let tmp = TempDir::new("plugin-zip-expected-id");
        let root = plugin_root(&tmp);
        let zip = demo_zip(&tmp, "com.example.demo");
        // 落位目标是清单里的 id：不比对就会让一份安装包覆盖另一个插件的目录与私有数据
        let err = install_from_zip(&root, &zip, Some("com.example.other"))
            .expect_err("条目 id 与清单不一致应被拒");
        assert!(err.contains("不一致"), "{err}");
        assert!(!layout::plugin_dir(&root, "com.example.demo").exists());

        let id = install_from_zip(&root, &zip, Some("com.example.demo")).expect("id 一致应可安装");
        assert_eq!(id, "com.example.demo");
    }

    #[test]
    fn zip_install_locates_single_subdir_root() {
        let tmp = TempDir::new("plugin-zip-subdir");
        let root = plugin_root(&tmp);
        let zip = tmp.join("nested.zip");
        let entry = format!("pkg/{}", entry_name());
        zip_entries(
            &zip,
            &[
                ("pkg/plugin.json", manifest_json("com.example.demo").as_bytes()),
                (entry.as_str(), b"stub"),
            ],
        );
        let id = install_from_zip(&root, &zip, None).expect("唯一的顶层目录为插件根，应可安装");
        assert_eq!(id, "com.example.demo");
    }

    #[test]
    fn zip_rejects_path_traversal_and_absolute() {
        let tmp = TempDir::new("plugin-zip-traversal");
        let root = plugin_root(&tmp);
        for bad_name in ["../evil.txt", "/abs.txt", "a/../../b.txt"] {
            let zip = tmp.join("evil.zip");
            zip_entries(&zip, &[(bad_name, b"x")]);
            assert!(install_from_zip(&root, &zip, None).is_err(), "应拒绝条目：{bad_name}");
        }
    }

    #[test]
    fn zip_rejects_symlink() {
        let tmp = TempDir::new("plugin-zip-symlink");
        let root = plugin_root(&tmp);
        let zip = tmp.join("sym.zip");
        let file = std::fs::File::create(&zip).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default().unix_permissions(0o777);
        writer.start_file(MANIFEST_FILE, options).unwrap();
        writer.write_all(manifest_json("com.example.demo").as_bytes()).unwrap();
        writer.add_symlink("link", "target", options).unwrap();
        writer.finish().unwrap();
        let err = install_from_zip(&root, &zip, None).expect_err("符号链接应被拒");
        assert!(err.contains("符号链接"), "{err}");
    }

    #[test]
    fn zip_rejects_high_compression_ratio() {
        let tmp = TempDir::new("plugin-zip-ratio");
        let root = plugin_root(&tmp);
        let zip = tmp.join("bomb.zip");
        let file = std::fs::File::create(&zip).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        writer.start_file(MANIFEST_FILE, options).unwrap();
        writer.write_all(manifest_json("com.example.demo").as_bytes()).unwrap();
        // 1 MiB 全零：压缩比远超阈值
        writer.start_file("blob.bin", options).unwrap();
        writer.write_all(&vec![0u8; 1024 * 1024]).unwrap();
        writer.finish().unwrap();
        let err = install_from_zip(&root, &zip, None).expect_err("高压缩比应被拒");
        assert!(err.contains("压缩比"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn zip_install_grants_entry_execute() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new("plugin-zip-exec");
        let root = plugin_root(&tmp);
        let zip = tmp.join("demo.zip");
        // 只写清单与入口、不带 unix 权限位（等同 Windows 上打的包）：落盘后入口默认可执行位为 0
        zip_entries(
            &zip,
            &[(MANIFEST_FILE, manifest_json("com.example.demo").as_bytes()), ("app", b"#!/bin/sh\n")],
        );
        let id = install_from_zip(&root, &zip, None).unwrap();
        let mode = std::fs::metadata(layout::plugin_dir(&root, &id).join("app"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "入口应具备可执行位：{mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn dir_install_rejects_symlink() {
        let tmp = TempDir::new("plugin-install-symlink");
        let root = plugin_root(&tmp);
        let src = tmp.join("src");
        make_plugin(&src, "com.example.demo");
        std::os::unix::fs::symlink("/etc/hosts", src.join("escape")).unwrap();
        let err = install_from_dir(&root, &src).expect_err("源目录含符号链接应被拒");
        assert!(err.contains("符号链接"), "{err}");
    }
}
