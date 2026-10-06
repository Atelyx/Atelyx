/**
 * 工具：移动文件（move_file）。把仓库内单个文件移动到目标文件夹，保持文件名；同目录改名请用 rename_file。
 * 目标重名自动加序号，结果以返回的实际路径为准。
 * 依赖 `capabilities.moveFile`（vaultStore.moveFile 按扩展名分发到对应动作）。
 */
import { ToolArgsError } from "@/types";
import { HIDDEN_PATH_ERROR, hasHiddenSegment } from "@/constants/tools";
import { defineTool } from "./defineTool";

export interface MoveFileArgs {
  oldPath: string;
  /** 目标文件夹（相对仓库根；空串 = 仓库根）。 */
  newDir: string;
}

export const MOVE_FILE_TOOL = defineTool<MoveFileArgs>({
  name: "move_file",
  description:
    "移动仓库中的单个文件到另一文件夹（相对仓库根路径），保持文件名；目标目录不存在时自动创建；引用该文件的画布引用自动更新。" +
    "目标重名时自动加序号（-2、-3），结果中的实际路径可能与预期不同，后续操作须以实际路径为准。" +
    "同目录改名请用 rename_file。",
  parameters: {
    type: "object",
    properties: {
      oldPath: { type: "string", description: "源文件相对仓库根的路径" },
      newDir: {
        type: "string",
        description: "目标文件夹相对仓库根的路径（仓库根传空串）",
      },
    },
    required: ["oldPath", "newDir"],
  },
  validate: (args) => {
    const raw = (args as { oldPath?: unknown; newDir?: unknown } | undefined) ?? {};
    if (typeof raw.oldPath !== "string" || !raw.oldPath.trim()) {
      throw new ToolArgsError("缺少源路径 oldPath");
    }
    if (typeof raw.newDir !== "string") {
      throw new ToolArgsError("缺少目标目录 newDir（仓库根传空串）");
    }
    if (hasHiddenSegment(raw.oldPath) || hasHiddenSegment(raw.newDir)) {
      throw new ToolArgsError(HIDDEN_PATH_ERROR);
    }
    return { oldPath: raw.oldPath, newDir: raw.newDir };
  },
  summarize: (args) => {
    const o = args.oldPath?.trim().slice(0, 40) ?? "";
    const d = args.newDir?.trim() || "仓库根";
    return o ? `移动 ${o} → ${d.slice(0, 40)}` : "移动文件";
  },
  execute: async (args, exec) => {
    const cap = exec.capabilities.moveFile;
    if (!cap) return { ok: false, summary: "移动文件能力未启用" };
    // 落盘事实（含重名加序号后的实际路径）由 capability 的 summary 如实回报
    return cap(args.oldPath, args.newDir);
  },
});
