/**
 * 工具：重命名文件（rename_file）。同目录改名：newName 为新文件名（含扩展名，扩展名不可变更），.md/.atb/.atlx 标题随文件名同步。
 * 目标重名自动加序号，结果以返回的实际路径为准；跨目录移动请用 move_file。
 * 依赖 `capabilities.renameFile`（vaultStore.renameFile 按扩展名分发）。
 */
import { ToolArgsError } from "@/types";
import { HIDDEN_PATH_ERROR, hasHiddenSegment } from "@/constants/tools";
import { defineTool } from "./defineTool";

export interface RenameFileArgs {
  oldPath: string;
  newName: string;
}

export const RENAME_FILE_TOOL = defineTool<RenameFileArgs>({
  name: "rename_file",
  description:
    "重命名仓库中的单个文件（相对仓库根路径；仅限同目录）。newName 为新文件名（含扩展名，扩展名不可变更）；" +
    ".md/.atb/.atlx 的标题随新文件名同步，引用该文件的画布/笔记链接自动更新。" +
    "目标重名时自动加序号（-2、-3），结果中的实际路径可能与 newName 不同，后续操作须以实际路径为准。" +
    "跨目录移动请用 move_file。",
  parameters: {
    type: "object",
    properties: {
      oldPath: { type: "string", description: "源文件相对仓库根的路径" },
      newName: { type: "string", description: "新文件名（含扩展名；纯文件名，不含目录）" },
    },
    required: ["oldPath", "newName"],
  },
  validate: (args) => {
    const raw = (args as { oldPath?: unknown; newName?: unknown } | undefined) ?? {};
    if (typeof raw.oldPath !== "string" || !raw.oldPath.trim()) {
      throw new ToolArgsError("缺少源路径 oldPath");
    }
    if (typeof raw.newName !== "string" || !raw.newName.trim()) {
      throw new ToolArgsError("缺少新文件名 newName");
    }
    if (hasHiddenSegment(raw.oldPath) || hasHiddenSegment(raw.newName)) {
      throw new ToolArgsError(HIDDEN_PATH_ERROR);
    }
    return { oldPath: raw.oldPath, newName: raw.newName };
  },
  summarize: (args) => {
    const o = args.oldPath?.trim().slice(0, 40) ?? "";
    const n = args.newName?.trim().slice(0, 40) ?? "";
    return o && n ? `重命名 ${o} → ${n}` : "重命名文件";
  },
  execute: async (args, exec) => {
    const cap = exec.capabilities.renameFile;
    if (!cap) return { ok: false, summary: "重命名文件能力未启用" };
    // 落盘事实（含重名加序号后的实际路径）由 capability 的 summary 如实回报
    return cap(args.oldPath, args.newName);
  },
});
