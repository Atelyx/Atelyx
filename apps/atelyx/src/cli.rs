//! `atelyx --command extension <子命令>`：扩展的打包、校验、列举、装卸。
//!
//! 在 iced 启动前分流：不建窗口、不进事件循环，跑完以退出码收场。
//! 参数解析与退出码留在壳，动作逻辑全部走 `atelyx_core::plugin_runtime`。

use std::path::{Path, PathBuf};

use atelyx_core::plugin_runtime::{package, PluginRuntime};

/// 退出码：成功。
const EXIT_OK: i32 = 0;
/// 退出码：执行失败（清单不合法、安装被拒等）。
const EXIT_FAILED: i32 = 1;
/// 退出码：用法错误（未知子命令 / 参数不成立）。
const EXIT_USAGE: i32 = 2;

const USAGE: &str = "用法：atelyx --command extension <子命令>

  list                              列出已安装扩展
  install-file <zip>                从压缩包安装
  remove <id[,id...]>               卸载扩展
  build [--source-dir DIR] [--output-dir DIR] [--output-file PATH]
                                    按清单打开发包
  validate [DIR]                    校验清单与内容（默认当前目录）";

/// 命令行分流：命中 `--command extension` 时执行并返回退出码；否则返回 `None` 交回正常启动。
pub fn dispatch(args: &[String]) -> Option<i32> {
    let mut iter = args.iter().skip(1);
    if iter.next().map(String::as_str) != Some("--command") {
        return None;
    }
    if iter.next().map(String::as_str) != Some("extension") {
        return None;
    }
    let rest: Vec<String> = iter.cloned().collect();
    Some(run(&rest))
}

/// 执行一个扩展子命令。失败原因打到 stderr，不吞错误。
fn run(args: &[String]) -> i32 {
    let Some(command) = args.first() else {
        eprintln!("{USAGE}");
        return EXIT_USAGE;
    };
    let rest = &args[1..];
    match command.as_str() {
        "list" => list(),
        "install-file" => match rest.first() {
            Some(path) => install_file(Path::new(path)),
            None => usage_error(),
        },
        "remove" => match rest.first() {
            Some(ids) => remove(ids),
            None => usage_error(),
        },
        "build" => build(rest),
        "validate" => validate(rest.first().map(Path::new).unwrap_or(Path::new("."))),
        other => {
            eprintln!("未知子命令：{other}\n\n{USAGE}");
            EXIT_USAGE
        }
    }
}

fn usage_error() -> i32 {
    eprintln!("{USAGE}");
    EXIT_USAGE
}

fn list() -> i32 {
    let runtime = PluginRuntime::new(plugins_root());
    runtime.scan();
    let plugins = runtime.list();
    if plugins.is_empty() {
        println!("未安装扩展");
        return EXIT_OK;
    }
    for plugin in plugins {
        let state = if plugin.enabled { "启用" } else { "停用" };
        println!("{} {}  {}  {}", plugin.id, plugin.version, state, plugin.name);
        if let Some(error) = plugin.load_error {
            println!("    无法装载：{error}");
        }
    }
    EXIT_OK
}

fn install_file(path: &Path) -> i32 {
    let runtime = PluginRuntime::new(plugins_root());
    match runtime.install_zip(path, None) {
        Ok(outcome) => {
            println!("已安装 {}", outcome.id);
            if let Some(error) = outcome.start_error {
                eprintln!("启动失败：{error}");
                return EXIT_FAILED;
            }
            EXIT_OK
        }
        Err(error) => {
            eprintln!("安装失败：{error}");
            EXIT_FAILED
        }
    }
}

fn remove(ids: &str) -> i32 {
    let ids: Vec<&str> = ids.split(',').map(str::trim).collect();
    if ids.iter().any(|id| id.is_empty()) {
        return usage_error();
    }
    let runtime = PluginRuntime::new(plugins_root());
    runtime.scan();
    let mut failed = false;
    for id in ids {
        match runtime.uninstall(id) {
            Ok(()) => println!("已卸载 {id}"),
            Err(error) => {
                eprintln!("卸载 {id} 失败：{error}");
                failed = true;
            }
        }
    }
    if failed { EXIT_FAILED } else { EXIT_OK }
}

fn build(args: &[String]) -> i32 {
    let mut source = PathBuf::from(".");
    let mut output_dir = None;
    let mut output_file = None;
    let mut index = 0;
    while index < args.len() {
        let Some(value) = args.get(index + 1) else {
            eprintln!("参数 {} 缺少取值", args[index]);
            return EXIT_USAGE;
        };
        match args[index].as_str() {
            "--source-dir" => source = PathBuf::from(value),
            "--output-dir" => output_dir = Some(PathBuf::from(value)),
            "--output-file" => output_file = Some(PathBuf::from(value)),
            other => {
                eprintln!("未知参数：{other}");
                return EXIT_USAGE;
            }
        }
        index += 2;
    }
    match package::build(&source, output_dir.as_deref(), output_file.as_deref()) {
        Ok(built) => {
            println!("已生成 {}（{} 个文件）", built.path.display(), built.entries);
            EXIT_OK
        }
        Err(error) => {
            eprintln!("打包失败：{error}");
            EXIT_FAILED
        }
    }
}

fn validate(dir: &Path) -> i32 {
    match package::validate(dir) {
        Ok(manifest) => {
            println!(
                "清单合法：{} {}（api_version {}）",
                manifest.id, manifest.version, manifest.api_version
            );
            EXIT_OK
        }
        Err(error) => {
            eprintln!("校验失败：{error}");
            EXIT_FAILED
        }
    }
}

/// 扩展安装根：与应用运行时同一个目录（应用数据目录下 `plugins/`）。
fn plugins_root() -> PathBuf {
    crate::app_data_dir()
        .map(|dir| dir.join("plugins"))
        .unwrap_or_else(|| std::env::temp_dir().join("atelyx-plugins"))
}
