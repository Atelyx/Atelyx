/**
 * 工具：读取历史（read_history）。读取仓库文件的版本历史（.md/.atlx/.atb，侧文件 `.atelyx/history/`）。
 * 不传 version 列出版本摘要（序号/时间/作者/行为/改动摘要/备注，**不含全文**防撑爆上下文）；传 version 返回该版全文快照（模型可用 write_file 写回恢复）。
 * 只读，不建产物节点；入参是普通仓库文件路径、内部直读 `.atelyx/history/`，属隐藏屏蔽的刻意豁免。
 */
import { ToolArgsError, errText } from "@/types";
import type { AgentHistoryReadResult } from "@/types";
import { HIDDEN_PATH_ERROR, hasHiddenSegment } from "@/constants/tools";
import { defineTool } from "./defineTool";

export interface ReadHistoryArgs {
  path: string;
  /** 要取快照的版本序号（1-based），不传则只列版本摘要。 */
  version?: number;
}

/** 把 capability 结果渲染为模型可读文本（导出供单测）。 */
export function renderHistoryResult(
  r: AgentHistoryReadResult,
): { ok: boolean; summary: string; content?: string } {
  if (!r.ok) return { ok: false, summary: r.summary };
  if (r.content !== undefined) {
    return { ok: true, summary: r.summary, content: r.content };
  }
  const versions = r.versions ?? [];
  if (versions.length === 0) {
    return { ok: true, summary: r.summary, content: "(no history versions)" };
  }
  const body = versions
    .map((v) => {
      const ts = new Date(v.ts).toLocaleString();
      const note = v.note ? ` [${v.note}]` : "";
      const change = v.summary ? ` — ${v.summary}` : "";
      return `v${v.seq} ${ts} ${v.authorName}(${v.action})${change}${note}`;
    })
    .join("\n");
  return { ok: true, summary: r.summary, content: body };
}

export const READ_HISTORY_TOOL = defineTool<ReadHistoryArgs>({
  name: "read_history",
  parallelSafe: true,
  description:
    "读取仓库中文件的版本历史：不传 version 列出各版本摘要（时间/作者/改动摘要/备注），" +
    "传 version 返回该版本全文快照（可用 write_file 写回以恢复到该版本）",
  parameters: {
    type: "object",
    properties: {
      path: { type: "string", description: "目标文件相对仓库根的路径（.md/.atlx/.atb）" },
      version: { type: "number", description: "要取快照的版本序号（不传则列出版本列表）" },
    },
    required: ["path"],
  },
  validate: (args) => {
    const raw = (args as { path?: unknown; version?: unknown } | undefined) ?? {};
    const p = raw.path;
    if (typeof p !== "string" || !p.trim()) throw new ToolArgsError("缺少文件路径 path");
    if (hasHiddenSegment(p)) throw new ToolArgsError(HIDDEN_PATH_ERROR);
    const out: ReadHistoryArgs = { path: p };
    if (raw.version !== undefined) {
      if (typeof raw.version !== "number" || !Number.isInteger(raw.version) || raw.version < 1) {
        throw new ToolArgsError("version 须为正整数");
      }
      out.version = raw.version;
    }
    return out;
  },
  summarize: (args) => {
    const p = args.path?.trim();
    return p ? `读取历史 ${p.slice(0, 40)}${args.version ? ` v${args.version}` : ""}` : "读取历史";
  },
  execute: async (args, exec) => {
    const cap = exec.capabilities.readHistory;
    if (!cap) return { ok: false, summary: "读取历史能力未启用" };
    try {
      const r = await cap(
        args.path,
        args.version !== undefined ? { version: args.version } : undefined,
      );
      return renderHistoryResult(r);
    } catch (e) {
      return { ok: false, summary: `读取历史失败：${errText(e)}` };
    }
  },
});
