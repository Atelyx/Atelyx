/**
 * 联网搜索 service（AI 搜索工具的产物数据源）：统一返回 `SearchResultItem[]`（title / url / snippet），
 * 供 SearchResultNode 渲染与 function calling 工具回填。
 */
import { invoke } from "@tauri-apps/api/core";
import type { GlobalSearchConfig, SearchResultData, SearchResultItem } from "@/types";

/**
 * 执行搜索（Rust 代理）。边界捕获：失败返回 error 字段，不抛异常（失败降级不阻塞对话）。
 * `apiKey` 仅 Tavily 用（前端配置层已按仓库身份解析出 key）；SearXNG 不需要。
 * 走代理而非前端直 fetch：SearXNG 自建实例无内置 CORS，前端直连必被拦截。
 * 命令不依赖本地仓库根——协作空间无 root，按 root 读会取到上一个仓库的残留。
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
