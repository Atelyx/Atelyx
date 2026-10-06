/**
 * 工具：抓取网页（web_fetch）。抓取指定 URL 的正文回填上下文，不建画布产物节点；依赖 `capabilities.fetchUrl`（后端代理）。
 * 结果自描述：正文为空时显式报「可能为动态渲染/需登录」（AI 可判别抓取失败 vs 页面无内容），非空时附字符数与截断标记。
 */
import { ToolArgsError, errText } from "@/types";
import { WEB_FETCH_TITLE_PREVIEW } from "@/constants/tools";
import { defineTool } from "./defineTool";

export interface WebFetchArgs {
  url: string;
}

export const WEB_FETCH_TOOL = defineTool<WebFetchArgs>({
  name: "web_fetch",
  parallelSafe: true,
  description: "抓取指定网页（URL）的正文内容，获取最新信息作为回答依据",
  parameters: {
    type: "object",
    properties: { url: { type: "string", description: "要抓取的网页完整 URL（https 开头）" } },
    required: ["url"],
  },
  validate: (args) => {
    const u = (args as { url?: unknown } | undefined)?.url;
    if (typeof u !== "string" || !/^https?:\/\//i.test(u.trim())) {
      throw new ToolArgsError("缺少合法的网页地址 url（需 https:// 开头）");
    }
    return { url: u.trim() };
  },
  summarize: (args) => {
    const u = args.url?.trim();
    return u ? `抓取 ${u.slice(0, 48)}` : "抓取网页";
  },
  execute: async (args, exec) => {
    const cap = exec.capabilities.fetchUrl;
    if (!cap) return { ok: false, summary: "抓取网页能力未启用" };
    try {
      const r = await cap(args.url);
      const body = (r.content ?? "").trim();
      if (!body) {
        const titled = r.title
          ? `，仅获取到标题「${r.title.slice(0, WEB_FETCH_TITLE_PREVIEW)}」`
          : "";
        return {
          ok: false,
          summary: `页面正文为空（可能为动态渲染或需登录）${titled}`,
          content: r.title
            ? `页面标题：${r.title}\n（正文为空，可能为 JS 渲染或需登录；可改用 web_search 获取信息）`
            : "(empty body)",
        };
      }
      // 码点计数（与 Rust 侧 truncate_chars 的 chars().count() 同口径，避免 CJK/emoji 虚高）
      const chars = [...body].length;
      const suffix = r.truncated === true
        ? `（正文 ${chars} 字符，已截断）`
        : `（正文 ${chars} 字符）`;
      const summary = `已抓取 ${r.title || args.url}${suffix}`;
      return { ok: true, summary, content: `${body}\n\n${suffix}` };
    } catch (e) {
      return { ok: false, summary: `抓取失败：${errText(e)}` };
    }
  },
});
