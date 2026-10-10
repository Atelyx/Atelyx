//! 命令适配层：网页抓取与通用 HTTP 代理实现在 atelyx-core，此处保持 Tauri 命令面。
use atelyx_core::web::{FetchedWebPage, HttpRequest, HttpResponse};

/// 抓取网页正文（`https://`/`http://`）。
#[tauri::command]
pub async fn fetch_web(url: String) -> Result<FetchedWebPage, String> {
    atelyx_core::web::fetch_web(url).await
}

/// 通用 HTTP 请求（插件 `ctx.http`）。
#[tauri::command]
pub async fn http_request(req: HttpRequest) -> Result<HttpResponse, String> {
    atelyx_core::web::http_request(req).await
}
