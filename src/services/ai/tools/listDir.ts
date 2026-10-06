/**
 * 工具：列出目录（list_dir）。单层列出目录条目：目录在前、按名称升序，子目录带直接子项数，文件带字节大小。
 * 不含 `.` 开头隐藏项（.atelyx 等对 AI 完全屏蔽）；想看子目录内容需再对其调用本工具，按模式检索文件请用 glob。
 * 依赖 `capabilities.listDir`（Rust `list_vault_dir`）；只读，不建产物节点。
 */
import { ToolArgsError, errText } from "@/types";
import { HIDDEN_PATH_ERROR, LIST_DIR_MAX_ENTRIES, hasHiddenSegment } from "@/constants/tools";
import { defineTool } from "./defineTool";

export interface ListDirArgs {
  /** 目标目录（相对仓库根），缺省 = 仓库根。 */
  dir?: string;
}

export const LIST_DIR_TOOL = defineTool<ListDirArgs>({
  name: "list_dir",
  parallelSafe: true,
  description:
    "单层列出仓库中指定目录的内容（相对仓库根路径，缺省 = 仓库根）。目录条目在前、按名称升序，" +
    `子目录标注直接子项数、文件标注字节大小；不含 . 开头隐藏项。最多返回前 ${LIST_DIR_MAX_ENTRIES} 条，` +
    "超限说明总数并要求改列子目录。查看子目录内容需再对其调用本工具；按模式检索文件请用 glob。",
  parameters: {
    type: "object",
    properties: {
      dir: { type: "string", description: "目标目录相对仓库根的路径，缺省为仓库根" },
    },
  },
  validate: (args) => {
    const raw = (args as { dir?: unknown } | undefined) ?? {};
    if (raw.dir === undefined) return {};
    if (typeof raw.dir !== "string") {
      throw new ToolArgsError("dir 须为字符串");
    }
    // 空串 = 显式列仓库根（与 Rust 侧「空 = 仓库根」约定对齐），归一为缺省
    const dir = raw.dir.trim();
    if (dir && hasHiddenSegment(dir)) {
      throw new ToolArgsError(HIDDEN_PATH_ERROR);
    }
    return dir ? { dir } : {};
  },
  summarize: (args) => `列出 ${args.dir?.trim() || "仓库根"}`,
  execute: async (args, exec) => {
    const cap = exec.capabilities.listDir;
    if (!cap) return { ok: false, summary: "列目录能力未启用" };
    try {
      const { entries, total, capped } = await cap(args.dir);
      if (entries.length === 0) {
        return { ok: true, summary: "目录为空", content: "(empty directory)" };
      }
      const body = entries
        .map((e) => (e.kind === "dir" ? `${e.name}/ (${e.children ?? 0} 项)` : `${e.name} (${e.size ?? 0} B)`))
        .join("\n");
      const footer = capped
        ? `\n\n(Showing ${entries.length} of ${total} entries; list a subdirectory to narrow.)`
        : "";
      return {
        ok: true,
        summary: `已列出「${args.dir?.trim() || "仓库根"}」（${total} 项）`,
        content: body + footer,
      };
    } catch (e) {
      return { ok: false, summary: `列目录失败：${errText(e)}` };
    }
  },
});
