/**
 * 工具：编辑文件（edit_file）。对仓库内任意文本文件做行级修改：每项 oldText 唯一精确匹配、
 * 块间不重叠，全部校验通过后统一替换（原子写）。依赖 `capabilities.editFile`。不建产物节点。
 */
import { errText, ToolArgsError } from "@/utils/errors";
import { HIDDEN_PATH_ERROR, hasHiddenSegment } from "@/constants/tools";
import { defineTool } from "./defineTool";

export interface EditEntry {
  oldText: string;
  newText: string;
}

export interface EditFileArgs {
  path: string;
  edits: EditEntry[];
}

export const EDIT_FILE_TOOL = defineTool<EditFileArgs>({
  name: "edit_file",
  description:
    "修改仓库中文件的指定文本：edits 每项 oldText 必须与文件现有文本精确匹配且唯一（不唯一请扩充上下文），全部匹配后统一替换，返回修改结果",
  parameters: {
    type: "object",
    properties: {
      path: { type: "string", description: "目标文件相对仓库根的路径，如「产品需求.md」或「笔记/方案.txt」" },
      edits: {
        type: "array",
        items: {
          type: "object",
          properties: {
            oldText: { type: "string", description: "要替换的原文（必须与文件中现有文本精确匹配且唯一）" },
            newText: { type: "string", description: "替换后的新文本" },
          },
          required: ["oldText", "newText"],
        },
        description: "替换列表（各块在原文上匹配，块间不得重叠）",
      },
    },
    required: ["path", "edits"],
  },
  validate: (args) => {
    const raw = (args ?? {}) as { path?: unknown; edits?: unknown };
    const path = typeof raw.path === "string" ? raw.path.trim() : "";
    const edits = Array.isArray(raw.edits)
      ? (raw.edits as unknown[]).filter(
          (e): e is EditEntry =>
            typeof e === "object" &&
            e !== null &&
            typeof (e as { oldText?: unknown }).oldText === "string" &&
            typeof (e as { newText?: unknown }).newText === "string",
        )
      : [];
    if (!path) throw new ToolArgsError("缺少文件路径 path");
    if (hasHiddenSegment(path)) throw new ToolArgsError(HIDDEN_PATH_ERROR);
    if (edits.length === 0) throw new ToolArgsError("缺少 edits 列表或格式非法（需 {oldText,newText}）");
    return { path, edits };
  },
  summarize: (args) => {
    const p = args.path?.trim();
    return p ? `编辑 ${p.slice(0, 40)}（${args.edits.length} 处）` : `编辑文件 ${args.edits.length} 处`;
  },
  execute: async (args, exec) => {
    const cap = exec.capabilities.editFile;
    if (!cap) return { ok: false, summary: "编辑文件能力未启用" };
    try {
      const res = await cap(args.path, args.edits);
      return { ok: res.ok, summary: res.summary };
    } catch (e) {
      return { ok: false, summary: `编辑失败：${errText(e)}` };
    }
  },
});
