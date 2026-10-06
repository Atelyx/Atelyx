/**
 * 工具：删除目录（delete_dir）。删除仓库中的文件夹（相对仓库根路径），须显式传 confirm: true 防误删。
 * 空目录直接删除，非空目录首次调用提示项数、再传 force: true 才递归删除；隐藏目录（. 开头段）与仓库根不可删除。
 * 删除后自动清理目录内画布引用/上次打开等既有联动；依赖 `capabilities.deleteDir`。
 */
import { ToolArgsError } from "@/types";
import { HIDDEN_PATH_ERROR, hasHiddenSegment } from "@/constants/tools";
import { defineTool } from "./defineTool";

export interface DeleteDirArgs {
  path: string;
  confirm: true;
  /** 非空目录递归删除确认（首次调用不传会提示项数，模型据此重试）。 */
  force?: boolean;
}

export const DELETE_DIR_TOOL = defineTool<DeleteDirArgs>({
  name: "delete_dir",
  description:
    "删除仓库中的文件夹（相对仓库根路径）。空目录直接删除；非空目录首次调用会提示项数，" +
    "须再传 force: true 才递归删除。须显式传 confirm: true 防误删；隐藏目录（. 开头段）与仓库根不可删除。",
  parameters: {
    type: "object",
    properties: {
      path: { type: "string", description: "目标文件夹相对仓库根的路径" },
      confirm: { type: "boolean", description: "必须显式传 true 才执行删除（防误删）" },
      force: {
        type: "boolean",
        description: "非空目录须传 true 才递归删除（首次调用不传会提示项数）",
      },
    },
    required: ["path", "confirm"],
  },
  validate: (args) => {
    const raw = (args as { path?: unknown; confirm?: unknown; force?: unknown } | undefined) ?? {};
    if (typeof raw.path !== "string" || !raw.path.trim()) {
      throw new ToolArgsError("缺少目录路径 path");
    }
    if (raw.confirm !== true) {
      throw new ToolArgsError("删除须显式传 confirm: true 以防误删");
    }
    const path = raw.path.trim().replace(/[\\/]+$/, "");
    if (hasHiddenSegment(path)) throw new ToolArgsError(HIDDEN_PATH_ERROR);
    // 仓库根 / 父目录段（..）不可删：父目录穿越由 safe_join 拒绝，这里提前给明确文案
    // （`/` 与 `\` 均识别，与 hasHiddenSegment 口径一致）
    if (!path || path === "." || path.split(/[\\/]+/).includes("..")) {
      throw new ToolArgsError("不可删除仓库根或父目录（..）");
    }
    return { path, confirm: true, force: raw.force === true };
  },
  summarize: (args) => `删除目录 ${args.path?.trim().slice(0, 40) ?? ""}`.trim(),
  execute: async (args, exec) => {
    const cap = exec.capabilities.deleteDir;
    if (!cap) return { ok: false, summary: "删除目录能力未启用" };
    const r = await cap(args.path, args.force);
    if (r.needsConfirm) {
      return {
        ok: false,
        summary: `目录非空（${r.itemCount} 项），需再传 force: true 确认递归删除`,
      };
    }
    return { ok: r.ok, summary: r.summary };
  },
});
