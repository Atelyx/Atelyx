/**
 * 工具：写入文件（write_file）。把内容按指定相对路径写入仓库（原子写、自动建父目录），
 * 像编程工具写文件那样直接落盘；data.path 供画布判断是否建产物节点。依赖 `capabilities.writeFile`。
 */
import { errText, ToolArgsError } from "@/utils/errors";
import { HIDDEN_PATH_ERROR, hasHiddenSegment } from "@/constants/tools";
import { defineTool } from "./defineTool";

export interface WriteFileArgs {
  path: string;
  content: string;
}

export const WRITE_FILE_TOOL = defineTool<WriteFileArgs>({
  name: "write_file",
  description:
    "把内容写入仓库中指定路径的文件（相对仓库根路径；不存在则新建，存在则覆盖），原子写并自动创建父目录，返回写入结果",
  parameters: {
    type: "object",
    properties: {
      path: { type: "string", description: "目标文件相对仓库根的路径，如「产物/总结.md」，勿含 / 开头或 ../ 越权路径" },
      content: { type: "string", description: "要写入的文件内容（纯文本，Markdown 或代码皆可）" },
    },
    required: ["path", "content"],
  },
  validate: (args) => {
    const raw = (args ?? {}) as { path?: unknown; content?: unknown };
    const path = typeof raw.path === "string" ? raw.path.trim() : "";
    const content = typeof raw.content === "string" ? raw.content : "";
    if (!path) throw new ToolArgsError("缺少文件路径 path");
    if (hasHiddenSegment(path)) throw new ToolArgsError(HIDDEN_PATH_ERROR);
    return { path, content };
  },
  summarize: (args) => {
    const p = args.path?.trim();
    return p ? `写入 ${p.slice(0, 48)}` : "写入文件";
  },
  execute: async (args, exec) => {
    const cap = exec.capabilities.writeFile;
    if (!cap) return { ok: false, summary: "写入文件能力未启用" };
    try {
      const res = await cap(args.path, args.content);
      return { ok: res.ok, summary: res.summary, data: { path: args.path } };
    } catch (e) {
      return { ok: false, summary: `写入失败：${errText(e)}` };
    }
  },
});
