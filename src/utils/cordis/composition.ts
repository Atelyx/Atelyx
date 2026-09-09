/**
 * 组合配置纯函数（profile 装配）。
 *
 * profile = 第一方插件装配的默认值层（存在/顺序/默认启用/默认 config）；
 * enabled 运行时真相由插件状态持久化（Rust plugin-state）提供，profile 只做顺序与默认值。
 * 纯函数无依赖，可直测；profile 行来源见 components/plugins/cordis/builtins.tsx。
 */
export interface ProfilePluginRow {
  id: string;
  /** 装配顺序（= 内置插件定义顺序；领域生命周期注册序依赖它）。 */
  order: number;
  /** 默认启用（组合 UI 默认值层用；运行时以持久化 enabled 为准）。 */
  defaultEnabled: boolean;
  /** 默认 config（当前第一方无 config；为组合补丁铺路）。 */
  defaultConfig?: Record<string, unknown>;
}

export interface Profile {
  name: string;
  plugins: ProfilePluginRow[];
}

/** 挂载清单解析：按给定启用 id 集合过滤，按 profile 顺序返回带 config 的挂载行。 */
export function resolveProfileMounts(
  profile: Profile,
  enabledIds: ReadonlySet<string>,
): Array<{ id: string; config: Record<string, unknown> | undefined }> {
  return profile.plugins
    .filter((p) => enabledIds.has(p.id))
    .sort((a, b) => a.order - b.order)
    .map((p) => ({ id: p.id, config: p.defaultConfig }));
}
