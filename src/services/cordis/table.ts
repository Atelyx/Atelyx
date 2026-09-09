/**
 * 表格服务提供器（ctx.table）：由第一方 builtin.table 插件挂载时经 ctx.provide 提供。
 *
 * 实现 = 注入的表格访问对象（stores/tableStore 的 registerTablePluginWiring 填充）——
 * 停用/卸载 builtin.table 时 access 撤销、服务随之消失。
 * resolveImage 直连表格图片缓存（服务层函数，无需注入）。
 */
import { resolveTableImageUrl } from "@/services/tableImageCache";
import { getPluginTableRuntimeAccess } from "./access";
import type { TableService } from "./types";

/** 构造表格服务（要求访问已接线：builtin.table 挂载时经 registerTablePluginWiring 填充）。 */
export function createTableService(): TableService {
  const access = getPluginTableRuntimeAccess();
  if (!access) throw new Error("表格能力未就绪");
  return {
    snapshot: () => access.snapshot(),
    updateCell: (rowId, fieldId, value) => access.updateCell(rowId, fieldId, value),
    addRow: () => access.addRow(),
    removeRow: (rowId) => access.removeRow(rowId),
    selectRow: (rowId) => access.selectRow(rowId),
    resolveImage: (entry) => resolveTableImageUrl(entry),
  };
}
