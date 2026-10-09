/**
 * 工具：查找文件（glob）。按 glob 模式枚举仓库内文件路径（只返回文件、不含目录），按修改时间升序，最多内联返回 GLOB_MAX_RESULTS 条，超限附精确总数提示收窄。
 * 模式不含「/」时匹配任意深度的文件名（「*」即整棵树），含「/」才锚定层级。
 * 依赖 `capabilities.glob`（Rust `glob_vault`）；只读，不建产物节点。
 */
import { errText, ToolArgsError } from "@/utils/errors";
import { GLOB_MAX_RESULTS, HIDDEN_PATH_ERROR, hasHiddenSegment } from "@/constants/tools";
import { defineTool } from "./defineTool";

export interface GlobArgs {
  pattern: string;
  /** 搜索目录（相对仓库根），缺省 = 仓库根。 */
  path?: string;
}

export const GLOB_TOOL = defineTool<GlobArgs>({
  name: "glob",
  parallelSafe: true,
  description:
    "按 glob 模式查找仓库内文件路径（相对仓库根；只返回文件、不含目录；跳过隐藏目录与排除文件夹）。" +
    `模式不含「/」时匹配任意深度的文件名（「*」匹配整棵树的全部文件），含「/」才锚定层级（如「src/**/*.ts」）。` +
    `结果按修改时间升序，最多返回前 ${GLOB_MAX_RESULTS} 条；超限会在结果中说明总数并要求收窄模式或 path。`,
  parameters: {
    type: "object",
    properties: {
      pattern: {
        type: "string",
        description:
          "glob 模式（匹配相对仓库根的路径，如「**/*.md」「笔记/*.txt」；不含「/」时匹配任意深度文件名，如「*.ts」）",
      },
      path: { type: "string", description: "搜索目录（相对仓库根），缺省为仓库根" },
    },
    required: ["pattern"],
  },
  validate: (args) => {
    const raw = (args as { pattern?: unknown; path?: unknown } | undefined) ?? {};
    const p = raw.pattern;
    if (typeof p !== "string" || !p.trim()) throw new ToolArgsError("缺少 glob 模式 pattern");
    const out: GlobArgs = { pattern: p };
    if (raw.path !== undefined) {
      if (typeof raw.path !== "string" || !raw.path.trim()) {
        throw new ToolArgsError("path 须为非空字符串");
      }
      if (hasHiddenSegment(raw.path)) {
        throw new ToolArgsError(HIDDEN_PATH_ERROR);
      }
      out.path = raw.path;
    }
    return out;
  },
  summarize: (args) => {
    const p = args.pattern?.trim();
    return p ? `查找 ${p.slice(0, 48)}` : "查找文件";
  },
  execute: async (args, exec) => {
    const cap = exec.capabilities.glob;
    if (!cap) return { ok: false, summary: "查找文件能力未启用" };
    try {
      const { paths, total, capped } = await cap(
        args.pattern,
        args.path !== undefined ? { path: args.path } : undefined,
      );
      if (paths.length === 0) {
        return { ok: true, summary: "未找到匹配文件", content: "No files found" };
      }
      const footer = capped
        ? `\n\n(Showing ${paths.length} of ${total} paths; narrow pattern or path to see more.)`
        : "";
      return {
        ok: true,
        summary: `找到 ${total} 个匹配文件`,
        content: paths.join("\n") + footer,
      };
    } catch (e) {
      return { ok: false, summary: `查找失败：${errText(e)}` };
    }
  },
});
