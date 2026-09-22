/**
 * 联网搜索 service（AI 自主决定搜索后的产物数据源）。
 *
 * 搜索请求统一走 Tauri Rust 代理（`search_web` 命令）：
 * - **SearXNG**：自建实例无内置 CORS，浏览器/WebView 前端直 fetch 必被拦截，须 Rust 侧请求；
 * - **Tavily**：key 由调用方传入——前端配置层已按仓库身份解析出 key（本地 keychain /
 *   `syncKeys` 落盘 / 协作空间团队元数据三态统一），Rust 侧只做请求与地址校验。
 *   命令不依赖本地仓库根（协作空间无 root，按 root 读会取到上一个仓库的残留）。
 *
 * 统一返回 `SearchResultItem[]`（title / url / snippet），供 SearchResultNode 渲染与
 * function calling 工具回填。边界捕获：搜索失败返回错误文本，不抛异常（失败降级不阻塞对话）。
 */
import { invoke } from "@tauri-apps/api/core";
import type { GlobalSearchConfig, SearchResultData, SearchResultItem } from "@/types";

/**
 * 执行搜索（Rust 代理）。边界捕获：失败返回 error 字段，不抛异常。
 * `apiKey` 仅 Tavily 用（前端配置层解析后的 key）；SearXNG 不需要。
 */
export async function runSearch(
  config: GlobalSearchConfig,
  query: string,
  apiKey: string,
): Promise<SearchResultData> {
  try {
    const results = await invoke<SearchResultItem[]>("search_web", {
      provider: config.provider,
      query,
      searxngUrl: config.provider === "searxng" ? config.searxngUrl : null,
      tavilyKey: config.provider === "tavily" ? apiKey || null : null,
    });
    return { query, results };
  } catch (e) {
    return { query, results: [], error: typeof e === "string" ? e : String(e) };
  }
}

/** 结果列表 → 注入上下文的文本摘要（勾选子集或全部，搜索节点注入）。 */
export function resultsToText(data: SearchResultData): string {
  const list = data.checked && data.checked.length > 0
    ? data.checked.map((i) => data.results[i]).filter((r): r is SearchResultItem => !!r)
    : data.results;
  return list.map((r) => `- ${r.title}：${r.snippet}\n  ${r.url}`).join("\n");
}
