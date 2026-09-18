//! Atelyx 协作服务端（库入口）。
//!
//! 单进程承载两套面：
//! - 空间面：`/api/*`（HTTP JSON：账号 / 空间 / 成员 / 邀请 / 内容 / 索引）+ `/ws/space`
//!   （凭令牌订阅空间频道）。内容真源在服务器数据目录的文件树上，权限在服务端强制校验。
//! - 中转面：`/ws` 房间转发（presence / 补丁 / 笔记同步 / 插件消息，无鉴权，局域网信任）。
//!
//! 数据目录（`DATA_DIR`，默认 `./data`）：结构化元数据 JSON（accounts / sessions / spaces /
//! invites，每次变更原子写整文件）+ `spaces/<id>/` 内容文件树。备份 = 拷目录。
//! 日志只记元数据不记内容（密码 / 令牌 / 正文一律不入日志）。

pub mod auth;
pub mod content;
pub mod fsops;
pub mod index;
pub mod relay;
pub mod space_ws;
pub mod spaces;
pub mod state;
pub mod ws;

use std::net::SocketAddr;

use axum::routing::{delete, get, post};
use axum::Router;
use tokio::net::TcpListener;

pub use state::ServerState;

/// 可选 TLS 的证书与私钥路径（部署方提供 PEM 文件；缺省 = 明文 HTTP，供本地开发与测试）。
pub struct TlsPaths {
    pub cert: std::path::PathBuf,
    pub key: std::path::PathBuf,
}

pub fn build_app(state: ServerState) -> Router {
    Router::new()
        // 账号
        .route("/api/auth/register", post(auth::register))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/auth/devices", get(auth::list_devices))
        .route("/api/auth/devices/{session_id}", delete(auth::revoke_device))
        // 空间与成员
        .route("/api/spaces", get(spaces::list_spaces).post(spaces::create_space))
        .route("/api/spaces/{space_id}", axum::routing::patch(spaces::rename_space))
        .route("/api/spaces/{space_id}/transfer", post(spaces::transfer_ownership))
        .route("/api/spaces/{space_id}/members", get(spaces::list_members))
        .route("/api/spaces/{space_id}/members/{user_id}", delete(spaces::remove_member))
        .route(
            "/api/spaces/{space_id}/invites",
            get(spaces::list_invites).post(spaces::create_invite),
        )
        .route("/api/spaces/{space_id}/invites/{code}", delete(spaces::revoke_invite))
        .route("/api/invites/accept", post(spaces::accept_invite))
        // 内容（真源在服务器文件树）
        .route("/api/spaces/{space_id}/tree", get(content::tree))
        .route(
            "/api/spaces/{space_id}/file",
            get(content::read_file).put(content::write_file).delete(content::delete_file),
        )
        .route("/api/spaces/{space_id}/rename", post(content::rename))
        .route("/api/spaces/{space_id}/copy", post(content::copy))
        .route(
            "/api/spaces/{space_id}/folder",
            post(content::create_folder).delete(content::delete_folder),
        )
        // 派生索引与扫描
        .route("/api/spaces/{space_id}/backlinks", get(index::backlinks))
        .route("/api/spaces/{space_id}/tags", get(index::tags))
        .route("/api/spaces/{space_id}/glob", post(index::glob))
        .route("/api/spaces/{space_id}/grep", post(index::grep))
        // 实时：空间频道（带鉴权）与无鉴权中转
        .route("/ws/space", get(space_ws::upgrade))
        .route("/ws", get(relay::upgrade))
        .with_state(state)
}

/// 在已绑定的监听器上起服务（`tls` 缺省 = 明文 HTTP）。
pub async fn serve_on(
    listener: TcpListener,
    app: Router,
    tls: Option<TlsPaths>,
) -> Result<(), String> {
    match tls {
        None => axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
            .await
            .map_err(|e| e.to_string()),
        Some(paths) => {
            let _ = rustls::crypto::ring::default_provider().install_default();
            let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(paths.cert, paths.key)
                .await
                .map_err(|e| format!("加载 TLS 证书失败：{e}"))?;
            let std_listener = listener
                .into_std()
                .map_err(|e| format!("监听器转换失败：{e}"))?;
            axum_server::from_tcp_rustls(std_listener, config)
                .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                .await
                .map_err(|e| e.to_string())
        }
    }
}

/// 错误响应体（`{"error": "..."}` + HTTP 状态码）。
pub struct ApiError(pub axum::http::StatusCode, pub String);

impl axum::response::IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let body = serde_json::json!({ "error": self.1 }).to_string();
        (
            self.0,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            body,
        )
            .into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;
