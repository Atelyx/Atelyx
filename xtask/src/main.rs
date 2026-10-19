//! 文档任务：`cargo run -p xtask -- docs` 重生成对外 API 参考。
//! `cargo test --workspace` 会跑本 crate 的检查测试——协议改动后忘记重生成即失败。

mod api_doc;

use std::path::{Path, PathBuf};

/// 生成物（相对仓库根）。
const OUTPUT: &str = "docs/plugin-api.md";

/// 仓库根 = 本 crate 目录的上一级（编译期定位，不依赖当前目录）。
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask 应位于仓库根下")
        .to_path_buf()
}

/// 生成 `docs/plugin-api.md` 的完整内容。
fn render_docs(root: &Path) -> String {
    api_doc::render(root).unwrap_or_else(|e| panic!("生成 API 参考失败：{e}"))
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("docs") => {
            let root = repo_root();
            let path = root.join(OUTPUT);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("创建文档目录失败");
            }
            std::fs::write(&path, render_docs(&root)).expect("写入文档失败");
            println!("已生成 {OUTPUT}");
        }
        _ => {
            eprintln!("用法：cargo run -p xtask -- docs");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 提交物必须是当前源码的产物：协议加了控件 / 方法 / 字段而文档没重生成，门禁在这里失败。
    #[test]
    fn api_doc_is_current() {
        let root = repo_root();
        let expected = render_docs(&root);
        let actual = std::fs::read_to_string(root.join(OUTPUT)).unwrap_or_default();
        assert_eq!(
            actual, expected,
            "文档与协议源码不一致：运行 `cargo run -p xtask -- docs` 并提交结果"
        );
    }
}
