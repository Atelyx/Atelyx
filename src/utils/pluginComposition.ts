/**
 * 组合配置（插件装配）视图推导纯函数。
 * 装配 = 期望清单的「默认值层」：官方默认集（内置插件清单）× 当前已装/启用 推导视图行，
 * 不新增持久化——enabled 仍是运行时唯一真相，装配只是默认值视角（非约束）。
 */
import type { PluginManifest } from "@/types";

/** 装配视图行的角色：官方默认集成员 / 用户安装的第三方。 */
export type CompositionRole = "default" | "third-party";

/** 装配视图行的状态（默认成员被停用/卸载、第三方启停等）。 */
export type CompositionStatus =
  | "default-on"
  | "default-off"
  | "uninstalled-default"
  | "user-on"
  | "user-off";

export interface CompositionRow {
  id: string;
  name: string;
  role: CompositionRole;
  /** 是否已安装（默认成员可能被卸载）。 */
  installed: boolean;
  /** 是否启用（运行时真相，来自已装行）。 */
  enabled: boolean;
  status: CompositionStatus;
}

/**
 * 装配视图推导：默认集（官方内置插件清单）× 当前已装/启用 推导行。
 * - 默认成员：已装启用 = default-on；已装停用 = default-off；未装（被卸载）= uninstalled-default。
 * - 第三方（已装但非默认成员）：启用 = user-on；停用 = user-off。
 * 畸形默认条目（缺 id/空 id）跳过；默认成员名字以默认集为准（已卸载成员只能来自默认集）。
 */
export function deriveComposition(
  defaults: PluginManifest[],
  installed: Record<string, { name: string; enabled: boolean }>,
): CompositionRow[] {
  const defaultIds = new Set<string>();
  const rows: CompositionRow[] = [];
  for (const d of defaults) {
    if (!d || typeof d.id !== "string" || d.id === "") continue;
    defaultIds.add(d.id);
    const info = installed[d.id];
    if (!info) {
      rows.push({
        id: d.id,
        name: d.name,
        role: "default",
        installed: false,
        enabled: false,
        status: "uninstalled-default",
      });
      continue;
    }
    rows.push({
      id: d.id,
      name: d.name,
      role: "default",
      installed: true,
      enabled: info.enabled,
      status: info.enabled ? "default-on" : "default-off",
    });
  }
  for (const [id, info] of Object.entries(installed)) {
    if (defaultIds.has(id)) continue;
    rows.push({
      id,
      name: info.name,
      role: "third-party",
      installed: true,
      enabled: info.enabled,
      status: info.enabled ? "user-on" : "user-off",
    });
  }
  return rows;
}
