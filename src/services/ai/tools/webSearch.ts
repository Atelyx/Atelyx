/**
 * 工具：联网搜索（web_search）。结果回填上下文，data 供画布建搜索结果节点。
 * 依赖 `capabilities.search`；依赖搜索源配置（needsSearch，未配置时由调用方剔除）。
 */
import { ToolArgsError } from "@/utils/errors";
import { defineTool } from "./defineTool";

export interface WebSearchArgs {
  query: string;
}

export const WEB_SEARCH_TOOL = defineTool<WebSearchArgs>({
  name: "web_search",
  parallelSafe: true,
  description: "联网搜索获取最新信息，返回网页标题、摘要与链接（可作为回答依据）",
  parameters: {
    type: "object",
    properties: { query: { type: "string", description: "搜索关键词" } },
    required: ["query"],
  },
  validate: (args) => {
    const q = (args as { query?: unknown } | undefined)?.query;
    if (typeof q !== "string" || !q.trim()) throw new ToolArgsError("缺少搜索关键词 query");
    return { query: q };
  },
  summarize: (args) => {
    const q = args.query?.trim();
    return q ? `搜索「${q.slice(0, 40)}」` : "联网搜索";
  },
  execute: async (args, exec) => {
    const search = exec.capabilities.search;
    if (!search) return { ok: false, summary: "搜索能力未启用" };
    const data = await search(args.query);
    if (data.error) return { ok: false, summary: `搜索失败：${data.error}` };
    return {
      ok: true,
      summary: `找到 ${data.results.length} 条结果`,
      content: JSON.stringify(data.results),
      data,
    };
  },
});
