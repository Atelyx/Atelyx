/**
 * 工具：搜索内容（grep）。用正则表达式搜索仓库内文件内容，返回匹配行（绝对行号、按文件分组），最多内联返回 GREP_MAX_MATCHES 条，超限附精确总数提示收窄。
 * 单行预览按字节截断（UTF-8 边界安全）；可选 path 限定文件/目录、include 限定单一正向 glob 过滤文件。
 * 依赖 `capabilities.grep`（Rust `grep_vault`）；只读，不建产物节点。
 */
import { ToolArgsError, errText, type GrepMatchRow } from "@/types";
import {
  GREP_MAX_LINE_BYTES,
  GREP_MAX_MATCHES,
  HIDDEN_PATH_ERROR,
  hasHiddenSegment,
} from "@/constants/tools";
import { defineTool } from "./defineTool";

export interface GrepArgs {
  pattern: string;
  /** 搜索的文件或目录（相对仓库根），缺省 = 整个仓库。 */
  path?: string;
  /** 单一正向 glob 文件过滤（如「*.ts」「*.{js,jsx}」）。 */
  include?: string;
}

/** include 须为单一正向 glob：空串 / `!` 否定 / 逗号列表（花括号内逗号除外）均拒绝。 */
function validateInclude(include: string): void {
  if (!include.trim()) throw new ToolArgsError("include 须为非空 glob 过滤");
  if (include.startsWith("!")) {
    throw new ToolArgsError("include 仅支持正向 glob，不支持否定（!）");
  }
  let braceDepth = 0;
  for (const ch of include) {
    if (ch === "{") braceDepth += 1;
    else if (ch === "}") braceDepth = Math.max(0, braceDepth - 1);
    else if (ch === "," && braceDepth === 0) {
      throw new ToolArgsError("include 须为单一 glob，不支持逗号列表（可用 {a,b} 并列）");
    }
  }
}

/** 匹配行按文件分组（首见顺序）→ 模型可读正文：每文件一行路径 + 逐行「Line N: text」。 */
function formatGrepMatches(matches: GrepMatchRow[]): string {
  const byFile = new Map<string, GrepMatchRow[]>();
  for (const m of matches) {
    const group = byFile.get(m.path);
    if (group !== undefined) group.push(m);
    else byFile.set(m.path, [m]);
  }
  const sections: string[] = [];
  for (const [path, rows] of byFile) {
    sections.push(`${path}\n${rows.map((r) => `Line ${r.lineNumber}: ${r.line}`).join("\n")}`);
  }
  return sections.join("\n\n");
}

export const GREP_TOOL = defineTool<GrepArgs>({
  name: "grep",
  parallelSafe: true,
  description:
    "用正则表达式搜索仓库内文件内容，返回匹配行（带行号、按文件分组）。" +
    `可选 path 限定搜索的文件或目录、include 限定文件（单一正向 glob，如「*.ts」「*.{js,jsx}」，不支持列表与否定）。` +
    `最多返回前 ${GREP_MAX_MATCHES} 条匹配、回填总量受字节预算约束，超限会在结果中说明总数并要求收窄；` +
    `单行预览按 ${GREP_MAX_LINE_BYTES} 字节截断。` +
    "对命中文件需要上下文时用 read_file 分页读取。",
  parameters: {
    type: "object",
    properties: {
      pattern: { type: "string", description: "正则表达式（搜索文件内容）" },
      path: {
        type: "string",
        description: "搜索的文件或目录（相对仓库根），缺省为整个仓库",
      },
      include: {
        type: "string",
        description: "文件过滤：单一正向 glob（如「*.ts」「*.{js,jsx}」），不支持列表与否定",
      },
    },
    required: ["pattern"],
  },
  validate: (args) => {
    const raw = (args as { pattern?: unknown; path?: unknown; include?: unknown } | undefined) ?? {};
    const p = raw.pattern;
    if (typeof p !== "string" || !p.trim()) throw new ToolArgsError("缺少搜索模式 pattern");
    const out: GrepArgs = { pattern: p };
    if (raw.path !== undefined) {
      if (typeof raw.path !== "string" || !raw.path.trim()) {
        throw new ToolArgsError("path 须为非空字符串");
      }
      if (hasHiddenSegment(raw.path)) {
        throw new ToolArgsError(HIDDEN_PATH_ERROR);
      }
      out.path = raw.path;
    }
    if (raw.include !== undefined) {
      if (typeof raw.include !== "string") throw new ToolArgsError("include 须为字符串");
      validateInclude(raw.include);
      out.include = raw.include;
    }
    return out;
  },
  summarize: (args) => {
    const p = args.pattern?.trim();
    return p ? `搜索 ${p.slice(0, 48)}` : "搜索内容";
  },
  execute: async (args, exec) => {
    const cap = exec.capabilities.grep;
    if (!cap) return { ok: false, summary: "搜索内容能力未启用" };
    try {
      const { matches, total, capped } = await cap(args.pattern, {
        ...(args.path !== undefined ? { path: args.path } : {}),
        ...(args.include !== undefined ? { include: args.include } : {}),
      });
      if (matches.length === 0) {
        return { ok: true, summary: "未找到匹配", content: "No matches found" };
      }
      const noun = total === 1 ? "match" : "matches";
      const header = capped
        ? `Found ${matches.length} of ${total} ${noun}`
        : `Found ${total} ${noun}`;
      const footer = capped ? "\n\n(Narrow pattern, path, or include to see more.)" : "";
      return {
        ok: true,
        summary: `找到 ${total} 处匹配`,
        content: `${header}\n\n${formatGrepMatches(matches)}${footer}`,
      };
    } catch (e) {
      return { ok: false, summary: `搜索失败：${errText(e)}` };
    }
  },
});
