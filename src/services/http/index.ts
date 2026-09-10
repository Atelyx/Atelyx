/**
 * 通用 HTTP 请求 service（插件 `ctx.http` 面）。
 *
 * 走 Rust 代理（`http_request` 命令）：WebView 前端直连受 CORS 限制，且便于统一超时/响应上限
 * 与 SSRF 防护（内网/回环地址拒绝，见 commands/web.rs）。边界捕获：失败抛错由调用方降级。
 */
import { invoke } from "@tauri-apps/api/core";

/** 请求输入（method 缺省 GET，白名单 GET/POST/PUT/PATCH/DELETE/HEAD）。 */
export interface HttpRequestInput {
  url: string;
  method?: string;
  headers?: Record<string, string>;
  body?: string;
}

/** 响应结果（正文为文本；truncated = 命中响应上限被截断）。 */
export interface HttpResponseResult {
  status: number;
  headers: Record<string, string>;
  body: string;
  truncated: boolean;
}

/** 发起 HTTP 请求（Rust 代理，20s 超时 + 1MB 响应上限）。 */
export function httpRequest(req: HttpRequestInput): Promise<HttpResponseResult> {
  return invoke<HttpResponseResult>("http_request", { req });
}
