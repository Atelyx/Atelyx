/**
 * 网页抓取 service（AI `fetch_url` 工具的产物数据源）：经 Rust `fetch_web` 代理抓 title + 正文纯文本。
 * 走代理而非前端直 fetch：WebView 直连受 CORS 拦截，且超时与大小上限统一在 Rust 侧。
 */
import { invoke } from "@tauri-apps/api/core";

export interface FetchedWebPage {
  url: string;
  title?: string;
  content: string;
  /** 正文是否命中大小上限被截断（工具据此提示内容不完整）。 */
  truncated?: boolean;
}

/**
 * 抓取网页正文（Rust 代理）。边界捕获：失败抛出，由调用方（工具 execute）降级为失败结果。
 */
export async function fetchWeb(url: string): Promise<FetchedWebPage> {
  return invoke<FetchedWebPage>("fetch_web", { url });
}
