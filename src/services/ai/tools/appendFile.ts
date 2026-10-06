/**
 * 工具：追加内容（append_file）。把内容追加到仓库中**已存在**文本文件的末尾（原子写），免去「读到末尾 → 带全文替换」。
 * 文件不存在/不可读（含超 read_file 整读上限）拒绝——新建文件请用 write_file。
 * 依赖 `capabilities.appendFile`（aiFiles.appendVaultFile）。
 */
import { ToolArgsError, errText } from "@/types";
import { HIDDEN_PATH_ERROR, hasHiddenSegment } from "@/constants/tools";
import { defineTool } from "./defineTool";

export interface AppendFileArgs {
  path: string;
  content: string;
}

export const APPEND_FILE_TOOL = defineTool<AppendFileArgs>({
  name: "append_file",
  description:
    "把内容追加到仓库中已存在文本文件的末尾（相对仓库根路径；文件不存在或不可读则拒绝，新建请用 write_file），" +
    "原子写并返回追加结果",
  parameters: {
    type: "object",
    properties: {
      path: { type: "string", description: "目标文件相对仓库根的路径" },
      content: { type: "string", description: "要追加到文件末尾的内容（纯文本）" },
    },
    required: ["path", "content"],
  },
  validate: (args) => {
    const raw = (args ?? {}) as { path?: unknown; content?: unknown };
    const path = typeof raw.path === "string" ? raw.path.trim() : "";
    const content = typeof raw.content === "string" ? raw.content : "";
    if (!path) throw new ToolArgsError("缺少文件路径 path");
    if (hasHiddenSegment(path)) throw new ToolArgsError(HIDDEN_PATH_ERROR);
    if (content === "") throw new ToolArgsError("缺少要追加的内容 content");
    return { path, content };
  },
  summarize: (args) => {
    const p = args.path?.trim();
    return p ? `追加 ${p.slice(0, 40)}` : "追加内容";
  },
  execute: async (args, exec) => {
    const cap = exec.capabilities.appendFile;
    if (!cap) return { ok: false, summary: "追加内容能力未启用" };
    try {
      return await cap(args.path, args.content);
    } catch (e) {
      return { ok: false, summary: `追加失败：${errText(e)}` };
    }
  },
});
