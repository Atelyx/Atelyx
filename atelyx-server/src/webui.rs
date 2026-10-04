//! 内置网页管理台：单文件页面编译期嵌入二进制，`GET /` 返回（浏览器打开服务器
//! 地址即达）。页面与桌面客户端调用同一批 API（Bearer 令牌鉴权），服务端不另设
//! 页面专用通道。

use axum::http::header::{
    CONTENT_SECURITY_POLICY, REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use axum::response::{Html, IntoResponse, Response};

/// 页面源文件（编译期嵌入，运行时零文件依赖，部署方式不变）。
const PAGE: &str = include_str!("../assets/webui.html");

/// 返回管理台页面。附最小安全响应头：页面脚本与样式均为内联（'unsafe-inline' 仅此），
/// 外部脚本不加载、不可被内嵌、表单不外跳；API 鉴权靠令牌本身，不依赖页面防御。
pub async fn page() -> Response {
    (
        [
            (
                CONTENT_SECURITY_POLICY,
                "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
                 connect-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'none'; \
                 frame-ancestors 'none'",
            ),
            (X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (X_FRAME_OPTIONS, "DENY"),
            (REFERRER_POLICY, "no-referrer"),
        ],
        Html(PAGE),
    )
        .into_response()
}
