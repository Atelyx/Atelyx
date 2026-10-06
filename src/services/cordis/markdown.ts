/**
 * Markdown 渲染服务提供器（ctx.markdown）：内核平台能力，root 作用域、无条件挂载，
 * 停用插件后仍可用（契约见 kernel 的 provide 处与 cordis/types）。
 * 实现 = 适配 utils/markdownCore 的纯函数（解析 / HTML 序列化 / DOM 片段）——与编辑器视图同一内核，插件渲染结果与应用内展示一致。
 */
import { parseMarkdown, renderMarkdownToHtml } from "@/utils/markdownCore";
import type { PluginMarkdownOptions, MarkdownService } from "./types";

function toRenderOptions(options: PluginMarkdownOptions | undefined) {
  return { katex: options?.katex, mentions: options?.mentions };
}

/** 构造 Markdown 渲染服务（无宿主访问依赖，内核能力纯函数直出）。 */
export function createMarkdownService(): MarkdownService {
  return {
    renderHtml: (markdown, options) => renderMarkdownToHtml(markdown, toRenderOptions(options)),
    parse: (markdown, options) => parseMarkdown(markdown, { mentions: options?.mentions }),
    renderToFragment: (markdown, options) => {
      // 无 DOM 环境（如纯逻辑测试）下没有片段可给
      if (typeof document === "undefined") return null;
      const template = document.createElement("template");
      template.innerHTML = renderMarkdownToHtml(markdown, toRenderOptions(options));
      return template.content;
    },
  };
}