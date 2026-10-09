/**
 * 工具：删除文件（delete_file）。受限删除：仅单个文件（目录不可删），须显式传 confirm: true 才执行，防误删。
 * 删除 .atb 会连带删除其私有图片附件目录；被画布/表格引用的文件删除后引用处降级为「文件缺失」。
 * 依赖 `capabilities.deleteFile`（vaultStore.deleteFile 按扩展名分发）。
 */
import { ToolArgsError } from "@/utils/errors";
import { HIDDEN_PATH_ERROR, hasHiddenSegment } from "@/constants/tools";
import { defineTool } from "./defineTool";

export interface DeleteFileArgs {
  path: string;
  confirm: true;
}

export const DELETE_FILE_TOOL = defineTool<DeleteFileArgs>({
  name: "delete_file",
  description:
    "删除仓库中的单个文件（相对仓库根路径）。仅支持文件，不支持文件夹；删除 .atb 表格会连带删除其私有图片附件目录。" +
    "被画布/表格引用的文件删除后引用处显示「文件缺失」断链降级，不会自动清理引用。此操作不可撤销，" +
    "调用时必须显式传 confirm: true 确认。",
  parameters: {
    type: "object",
    properties: {
      path: { type: "string", description: "目标文件相对仓库根的路径" },
      confirm: { type: "boolean", description: "必须显式传 true 才执行删除（防误删）" },
    },
    required: ["path", "confirm"],
  },
  validate: (args) => {
    const raw = (args as { path?: unknown; confirm?: unknown } | undefined) ?? {};
    if (typeof raw.path !== "string" || !raw.path.trim()) {
      throw new ToolArgsError("缺少文件路径 path");
    }
    if (hasHiddenSegment(raw.path)) {
      throw new ToolArgsError(HIDDEN_PATH_ERROR);
    }
    if (raw.confirm !== true) {
      throw new ToolArgsError("删除须显式传 confirm: true 以防误删");
    }
    return { path: raw.path, confirm: true };
  },
  summarize: (args) => `删除 ${args.path?.trim().slice(0, 48) ?? ""}`.trim(),
  execute: async (args, exec) => {
    const cap = exec.capabilities.deleteFile;
    if (!cap) return { ok: false, summary: "删除文件能力未启用" };
    return cap(args.path);
  },
});
