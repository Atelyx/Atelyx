//! 扩展打包与校验：把插件目录按清单的 `[build]` 规则压成可分发的 zip（`extension build`），
//! 以及在不安装的前提下校验一份清单与它的内容（`extension validate`）。
//!
//! 与安装链（`install`）共用同一套打包口径：`[build].paths` 限定纳入的文件，
//! `paths_exclude_pattern` 叠加排除；清单与当前平台入口必须落在结果里。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use atelyx_plugin_protocol::manifest::{current_platform, Manifest, MANIFEST_FILE};

use super::manifest as host_manifest;

/// 单次打包的条目数上限：正常扩展远低于此，超限多半是选错了目录。
const MAX_ENTRIES: usize = 10_000;
/// 打包跳过宿主在插件目录内保留的顶层名（那些是运行时状态，不是内容）。
const SKIPPED_TOP_LEVEL: [&str; 5] = ["data", "logs", "previous", "enabled", "source"];

/// 打包结果：产物路径与纳入的条目数。
#[derive(Debug)]
pub struct BuiltArchive {
    pub path: PathBuf,
    pub entries: usize,
}

/// 按清单把 `source_dir` 打成 `<id>-<version>.zip`，默认落在同一目录。
///
/// 返回可读错误：清单不合法、入口不在纳入范围、或写包失败。产物已存在时直接覆盖。
pub fn build(source_dir: &Path, output_dir: Option<&Path>, output_file: Option<&Path>) -> Result<BuiltArchive, String> {
    let manifest = host_manifest::read_manifest(source_dir)?;
    let files = collect(source_dir, &manifest)?;
    let path = match output_file {
        Some(path) => path.to_path_buf(),
        None => {
            let name = format!(
                "{}-{}-{}.zip",
                manifest.id,
                manifest.version,
                current_platform()
            );
            output_dir.unwrap_or(source_dir).join(name)
        }
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建输出目录失败：{e}"))?;
    }
    let file = std::fs::File::create(&path).map_err(|e| format!("创建压缩包失败：{e}"))?;
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for rel in &files {
        let full = source_dir.join(rel);
        let mut input =
            std::fs::File::open(&full).map_err(|e| format!("读取 {} 失败：{e}", rel.display()))?;
        let mut bytes = Vec::new();
        input
            .read_to_end(&mut bytes)
            .map_err(|e| format!("读取 {} 失败：{e}", rel.display()))?;
        writer
            .start_file(rel.to_string_lossy().replace('\\', "/"), options)
            .map_err(|e| format!("写入压缩包条目失败：{e}"))?;
        writer
            .write_all(&bytes)
            .map_err(|e| format!("写入压缩包条目失败：{e}"))?;
    }
    writer.finish().map_err(|e| format!("收尾压缩包失败：{e}"))?;
    Ok(BuiltArchive { path, entries: files.len() })
}

/// 校验一份待安装的扩展：清单结构、协议版本、当前平台入口存在且不越出目录。
/// 与安装链的装载校验同口径，只是不落位，供 `extension validate` 与提交前自检使用。
pub fn validate(source_dir: &Path) -> Result<Manifest, String> {
    host_manifest::read_manifest(source_dir)
}

/// 按清单的 `[build]` 规则枚举要打包的文件（相对路径，按字典序）。
fn collect(source_dir: &Path, manifest: &Manifest) -> Result<Vec<PathBuf>, String> {
    let include = Include::new(&manifest.build.paths);
    let exclude = Exclude::new(&manifest.build.paths_exclude_pattern);
    let mut files = Vec::new();
    walk(source_dir, source_dir, &include, &exclude, &mut files)?;
    files.sort();
    if files.len() > MAX_ENTRIES {
        return Err(format!("打包条目过多（{}），请检查所选目录", files.len()));
    }
    if !files.iter().any(|rel| rel == Path::new(MANIFEST_FILE)) {
        return Err(format!("清单 {MANIFEST_FILE} 不在打包范围内"));
    }
    let Some(entry) = manifest.entry.for_current_platform() else {
        return Err("未声明当前平台的插件入口".to_string());
    };
    if !files.iter().any(|rel| rel == Path::new(entry)) {
        return Err(format!("入口文件不在打包范围内：{entry}"));
    }
    Ok(files)
}

/// `[build].paths`：空 = 整个目录；否则只收列出的路径（目录前缀匹配）。
struct Include {
    prefixes: Vec<PathBuf>,
}

impl Include {
    fn new(paths: &[String]) -> Self {
        Self {
            prefixes: paths
                .iter()
                .map(|path| PathBuf::from(path.trim_end_matches('/')))
                .collect(),
        }
    }

    fn accepts(&self, rel: &Path) -> bool {
        // 清单是打包的骨架：它必须在包里，不受 `paths` 过滤
        if rel == Path::new(MANIFEST_FILE) {
            return true;
        }
        self.prefixes.is_empty()
            || self
                .prefixes
                .iter()
                .any(|prefix| rel == prefix || rel.starts_with(prefix))
    }
}

/// `[build].paths_exclude_pattern`：目录前缀或 `*` 通配（只支持后缀 `*`，够用且不引依赖）。
struct Exclude {
    patterns: Vec<String>,
}

impl Exclude {
    fn new(patterns: &[String]) -> Self {
        Self { patterns: patterns.to_vec() }
    }

    fn matches(&self, rel: &Path) -> bool {
        let text = rel.to_string_lossy().replace('\\', "/");
        self.patterns.iter().any(|pattern| {
            let pattern = pattern.trim_end_matches('/');
            if let Some(prefix) = pattern.strip_suffix('*') {
                text.starts_with(prefix)
            } else {
                text == pattern || text.starts_with(&format!("{pattern}/"))
            }
        })
    }
}

fn walk(
    root: &Path,
    dir: &Path,
    include: &Include,
    exclude: &Exclude,
    out: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("读取目录失败（{}）：{e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("读取目录项失败：{e}"))?;
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .map_err(|_| format!("路径越界：{}", path.display()))?;
        // 宿主保留的顶层名是运行时状态，不随内容分发
        if rel.components().count() == 1
            && rel
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| SKIPPED_TOP_LEVEL.contains(&name))
        {
            continue;
        }
        if exclude.matches(rel) {
            continue;
        }
        let metadata = entry
            .metadata()
            .map_err(|e| format!("读取文件信息失败（{}）：{e}", path.display()))?;
        if metadata.is_dir() {
            walk(root, &path, include, exclude, out)?;
        } else if metadata.is_file() && include.accepts(rel) {
            out.push(rel.to_path_buf());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_runtime::test_support::{entry_name, manifest_toml};
    use crate::vault::test_support::TempDir;

    fn seed(dir: &Path) {
        crate::plugin_runtime::test_support::seed_plugin(dir, "com.example.demo", "");
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("target").join("junk.bin"), "build").unwrap();
        std::fs::create_dir_all(dir.join("data")).unwrap();
        std::fs::write(dir.join("data").join("state.json"), "{}").unwrap();
    }

    fn write_manifest(dir: &Path, build: &str) {
        std::fs::write(dir.join(MANIFEST_FILE), manifest_toml("com.example.demo", build)).unwrap();
    }

    #[test]
    fn build_excludes_named_patterns_and_host_state() {
        let tmp = TempDir::new("plugin-package-build");
        let src = tmp.join("src");
        seed(&src);
        write_manifest(&src, "\n[build]\npaths_exclude_pattern = [\"target/\"]\n");
        let built = build(&src, None, None).expect("打包应成功");
        assert_eq!(built.entries, 2, "清单 + 入口，target/ 与 data/ 都不纳入");
        let file = std::fs::File::open(&built.path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let mut names: Vec<String> = (0..archive.len())
            .map(|index| archive.by_index(index).unwrap().name().to_string())
            .collect();
        names.sort();
        let mut expected = vec![MANIFEST_FILE.to_string(), entry_name().to_string()];
        expected.sort();
        assert_eq!(names, expected);
    }

    /// `paths` 限定纳入范围：没列出的文件一律不进包，清单本身恒纳入。
    #[test]
    fn build_honors_include_paths() {
        let tmp = TempDir::new("plugin-package-include");
        let src = tmp.join("src");
        seed(&src);
        std::fs::create_dir_all(src.join("lib")).unwrap();
        std::fs::write(src.join("lib").join("extra.txt"), "x").unwrap();
        write_manifest(&src, "\n[build]\npaths = [\"lib\"]\n");
        let error = build(&src, None, None).expect_err("入口不在纳入范围应拒");
        assert!(error.contains("入口文件不在打包范围内"), "{error}");
    }

    /// 清单不参与 `paths` 过滤：它是打包的骨架，缺了就不是可安装的包。
    #[test]
    fn manifest_itself_is_always_packed() {
        let tmp = TempDir::new("plugin-package-manifest");
        let src = tmp.join("src");
        seed(&src);
        write_manifest(&src, "\n[build]\npaths = [\"lib\"]\n");
        std::fs::create_dir_all(src.join("lib")).unwrap();
        std::fs::write(src.join("lib").join("placeholder.txt"), "x").unwrap();
        let built = build(&src, None, None).expect_err("入口不在 lib 下，仍应拒");
        assert!(built.contains("入口文件不在打包范围内"), "{built}");
    }

    #[test]
    fn validate_reports_unreadable_manifest() {
        let tmp = TempDir::new("plugin-package-validate");
        let src = tmp.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let error = validate(&src).expect_err("缺清单应拒");
        assert!(error.contains(MANIFEST_FILE), "{error}");
        seed(&src);
        write_manifest(&src, "");
        validate(&src).expect("合法清单应通过");
    }
}

