//! Atelyx 协作服务端（库入口）。
//!
//! 实时协作统一走空间频道 `/ws/space`（凭令牌订阅空间房间），配合 `/api/*`
//! （HTTP JSON：账号 / 空间 / 成员 / 邀请 / 内容 / 索引）。内容真源在服务器数据目录的
//! 文件树上，权限在服务端强制校验。
//!
//! 数据目录（`DATA_DIR`，默认 `./data`）：结构化元数据 JSON（accounts / sessions / spaces /
//! invites，每次变更原子写整文件）+ `spaces/<id>/` 内容文件树。备份 = 拷目录。
//! 日志只记元数据不记内容（密码 / 令牌 / 正文一律不入日志）。

pub mod auth;
pub mod content;
pub mod fsops;
pub mod index;
pub mod meta;
pub mod patches;
pub mod space_ws;
pub mod spaces;
pub mod state;
pub mod ws;

use std::net::SocketAddr;

use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::routing::{delete, get, post};
use axum::Router;
use tokio::net::TcpListener;

pub use state::ServerState;
// 测试用：临时覆盖单文件 / 单键值字节上限
pub use state::set_size_limit_override;

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
            "/api/spaces/{space_id}/patches/canvas",
            post(patches::patch_canvas),
        )
        .route(
            "/api/spaces/{space_id}/patches/table",
            post(patches::patch_table),
        )
        .route(
            "/api/spaces/{space_id}/folder",
            post(content::create_folder).delete(content::delete_folder),
        )
        // 保留媒体目录枚举（客户端临时附件回收）
        .route("/api/spaces/{space_id}/media/list", get(content::media_list))
        // 派生索引与扫描
        .route("/api/spaces/{space_id}/backlinks", get(index::backlinks))
        .route("/api/spaces/{space_id}/tags", get(index::tags))
        .route("/api/spaces/{space_id}/glob", post(index::glob))
        .route("/api/spaces/{space_id}/grep", post(index::grep))
        // 空间配置落点（键值元数据）
        .route(
            "/api/spaces/{space_id}/meta",
            get(meta::get_space_meta).patch(meta::patch_space_meta).delete(meta::delete_space_meta),
        )
        .route(
            "/api/spaces/{space_id}/meta/me",
            get(meta::get_user_meta).patch(meta::patch_user_meta).delete(meta::delete_user_meta),
        )
        // 实时：空间频道（带鉴权）
        .route("/ws/space", get(space_ws::upgrade))
        // 请求体上限 = 50MB 单文件上限（content::MAX_FILE_BYTES）× base64 膨胀 4/3（约 66.7MB）
        // + JSON 字符串转义与请求包装余量，取 96MB，使自有大小校验（按解码后字节）先于框架拦截生效
        .layer(axum::extract::DefaultBodyLimit::max(96 * 1024 * 1024))
        // 桌面端 WebView（origin = tauri.localhost）跨源直连本服务，带 JSON 体 / Bearer 头的请求
        // 会先发 CORS 预检；无 CORS 应答时浏览器引擎直接中断请求（Failed to fetch）。
        // 客户端不用 cookie 凭据（鉴权走 Authorization 头），放开任意 origin / 方法即安全；
        // allow-headers 显式列出（`*` 通配在部分实现里不覆盖 Authorization）。
        .layer(
            tower_http::cors::CorsLayer::new()
                .allow_origin(tower_http::cors::Any)
                .allow_methods(tower_http::cors::Any)
                .allow_headers([CONTENT_TYPE, AUTHORIZATION]),
        )
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
