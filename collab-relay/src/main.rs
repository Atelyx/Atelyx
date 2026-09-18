//! Atelyx 协作服务端入口：读环境变量并起服务。
//!
//! 环境变量：
//! - `PORT`：监听端口（默认 11224）
//! - `DATA_DIR`：数据目录（默认 `./data`；账号 / 空间名册 / 空间内容都在这里，备份 = 拷目录）
//! - `TLS_CERT` + `TLS_KEY`：PEM 证书与私钥路径，二者齐备即启 HTTPS/WSS（缺省明文 HTTP，
//!   供本地开发与测试；局域网跨进程传密码应启用 TLS）
//!
//! 日志：tracing 结构化输出（stderr / `docker logs`）。级别由 `RUST_LOG` 控制（默认 info），
//! `LOG_FORMAT=json` 切 JSON 行输出便于采集。

use std::path::PathBuf;

use collab_relay::{build_app, serve_on, ServerState, TlsPaths};
use tracing::info;

fn main() {
    init_logging();
    let port = std::env::var("PORT").unwrap_or_else(|_| "11224".to_string());
    let data_dir =
        PathBuf::from(std::env::var("DATA_DIR").unwrap_or_else(|_| "./data".to_string()));
    let tls = match (
        std::env::var("TLS_CERT").ok().filter(|s| !s.is_empty()),
        std::env::var("TLS_KEY").ok().filter(|s| !s.is_empty()),
    ) {
        (Some(cert), Some(key)) => Some(TlsPaths { cert: PathBuf::from(cert), key: PathBuf::from(key) }),
        _ => None,
    };
    let state = ServerState::open(&data_dir);
    let runtime = tokio::runtime::Runtime::new().expect("创建 tokio 运行时失败");
    runtime.block_on(async move {
        let addr = format!("0.0.0.0:{port}");
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .unwrap_or_else(|e| panic!("bind {addr} 失败：{e}"));
        let app = build_app(state);
        info!(addr = %addr, data_dir = %data_dir.display(), tls = tls.is_some(), "协作服务端已启动");
        if let Err(e) = serve_on(listener, app, tls).await {
            panic!("服务器错误：{e}");
        }
    });
}

/// 日志初始化：RUST_LOG 控制级别（默认 info），非 TTY（Docker）自动去 ANSI 颜色，
/// `LOG_FORMAT=json` 切 JSON 行输出便于采集。
fn init_logging() {
    use std::io::IsTerminal;
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    if std::env::var("LOG_FORMAT").as_deref() == Ok("json") {
        tracing_subscriber::fmt().json().with_env_filter(filter).init();
    } else {
        tracing_subscriber::fmt()
            .with_ansi(std::io::stderr().is_terminal())
            .with_env_filter(filter)
            .init();
    }
}
