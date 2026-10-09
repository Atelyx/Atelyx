/**
 * 工具：读取文件（read_file）。分页读取仓库内任意文本文件，返回带绝对行号的窗口并回填上下文。
 * 大文件按 offset/limit 分段读取（单行/单次字节受 Rust 侧截断约束），页脚引导模型继续翻页——不硬拒大文件，也不整文件回填撑爆上下文。
 * 依赖 `capabilities.readFile`；只读，不建产物节点。
 */
import { errText, ToolArgsError } from "@/utils/errors";
import { HIDDEN_PATH_ERROR, READ_WINDOW_DEFAULT_LINES, hasHiddenSegment } from "@/constants/tools";
import { defineTool } from "./defineTool";

export interface ReadFileArgs {
  path: string;
  /** 1-based 起始行（默认 1）。 */
  offset: number;
  /** 返回行数上限（默认 READ_WINDOW_DEFAULT_LINES）。 */
  limit: number;
}

function positiveInt(v: unknown, fallback: number, name: string): number {
  if (v === undefined) return fallback;
  if (typeof v !== "number" || !Number.isInteger(v) || v < 1) {
    throw new ToolArgsError(`${name} 须为正整数`);
  }
  return v;
}

export const READ_FILE_TOOL = defineTool<ReadFileArgs>({
  name: "read_file",
  parallelSafe: true,
  description:
    "分页读取仓库中指定文本文件（相对仓库根路径），返回带行号的内容。结果含总行数与页脚提示；大文件用 offset/limit 分段继续读取（offset=上一段页脚提示的行号）。",
  parameters: {
    type: "object",
    properties: {
      path: { type: "string", description: "目标文件相对仓库根的路径，如「产品需求.md」或「笔记/方案.txt」" },
      offset: { type: "number", description: "起始行号（1-based），默认 1；上一段页脚可给出继续的行号" },
      limit: { type: "number", description: `返回行数上限，默认 ${READ_WINDOW_DEFAULT_LINES}` },
    },
    required: ["path"],
  },
  validate: (args) => {
    const raw = (args as { path?: unknown; offset?: unknown; limit?: unknown } | undefined) ?? {};
    const p = raw.path;
    if (typeof p !== "string" || !p.trim()) throw new ToolArgsError("缺少文件路径 path");
    if (hasHiddenSegment(p)) throw new ToolArgsError(HIDDEN_PATH_ERROR);
    return {
      path: p,
      offset: positiveInt(raw.offset, 1, "offset"),
      limit: positiveInt(raw.limit, READ_WINDOW_DEFAULT_LINES, "limit"),
    };
  },
  summarize: (args) => {
    const p = args.path?.trim();
    return p ? `读取 ${p.slice(0, 48)}` : "读取文件";
  },
  execute: async (args, exec) => {
    const cap = exec.capabilities.readFile;
    if (!cap) return { ok: false, summary: "读取文件能力未启用" };
    try {
      const { lines, totalLines, truncated } = await cap(args.path, {
        offset: args.offset,
        limit: args.limit,
      });
      // 结果页脚：字节截断 / 尚有后续可翻页 / 已读完全部
      const endLine = lines.length ? lines[lines.length - 1].number : args.offset - 1;
      let footer: string;
      if (truncated) {
        footer = `(Output capped. Showing lines ${args.offset}-${endLine}. Use offset=${endLine + 1} to continue.)`;
      } else if (endLine < totalLines) {
        footer = `(Showing lines ${args.offset}-${endLine} of ${totalLines}. Use offset=${endLine + 1} to continue.)`;
      } else {
        footer = `(End of file - total ${totalLines} lines)`;
      }
      const body = lines.map((l) => `${l.number}: ${l.text}`).join("\n");
      return {
        ok: true,
        summary: `已读取「${args.path}」（${lines.length} 行 / 共 ${totalLines} 行）`,
        content: body ? `${body}\n\n${footer}` : footer,
      };
    } catch (e) {
      return { ok: false, summary: `读取失败：${errText(e)}` };
    }
  },
});
