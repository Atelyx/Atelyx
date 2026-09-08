/**
 * 插件启用依赖解析（requires 启动前校验）。
 *
 * requires 在启动前校验：依赖不满足的插件拒绝启用并给出可读原因。纯函数无 I/O，便于单测。
 *
 * 解析规则：
 * - 可用集合 = 宿主能力命名空间 ∪ 已启用且已解析插件的 provides 声明；
 * - 固定点迭代（天然终止：每轮至少移除一个插件，无满足者即 break）：
 *   每轮把 requires 全部满足的插件加入可启动集合，并将其 provides 并入可用集合；
 * - 迭代后仍未解析的插件 = 依赖缺失（提供者未安装/未启用）或依赖成环（相互依赖且
 *   环内无任何插件可先启动），附可读原因。
 * - 用 manifest.provides 声明做解析（提供方自身运行时注册失败属该插件自己的问题，
 *   与本解析无关）；requires/provides 非数组（畸形清单）按空处理——坏清单不让
 *   App 内部功能崩溃，只让该插件自身依赖解析失败。
 */
export interface PluginDependencyInput {
  id: string;
  provides?: string[];
  requires?: string[];
}

export interface PluginDependencyFailure {
  id: string;
  reason: string;
}

export interface PluginDependencyResult {
  /** 依赖满足、可启动的插件 id（顺序 = 解析成功顺序：提供者先于依赖者）。 */
  spawnable: string[];
  failed: PluginDependencyFailure[];
}

export function resolveEnabledDeps(
  plugins: PluginDependencyInput[],
  hostNamespaces: string[],
): PluginDependencyResult {
  const available = new Set(hostNamespaces);
  const remaining = new Map(plugins.map((p) => [p.id, p]));
  const spawnable: string[] = [];

  for (;;) {
    const satisfied: string[] = [];
    for (const [id, p] of remaining) {
      // 畸形清单守卫：非数组按无依赖处理（防坏清单拖垮整个加载）
      const reqs = Array.isArray(p.requires) ? p.requires : [];
      if (reqs.every((r) => available.has(r))) satisfied.push(id);
    }
    if (satisfied.length === 0) break;
    for (const id of satisfied) {
      const p = remaining.get(id)!;
      remaining.delete(id);
      spawnable.push(id);
      const provs = Array.isArray(p.provides) ? p.provides : [];
      for (const ns of provs) available.add(ns);
    }
  }

  // 残留 = 依赖缺失或成环：区分「环内互供」与「真的缺失」（缺失优先报告真实缺项）
  const remainingProvides = new Set(
    [...remaining.values()].flatMap((p) => (Array.isArray(p.provides) ? p.provides : [])),
  );
  const failed: PluginDependencyFailure[] = [];
  for (const [id, p] of remaining) {
    const reqs = Array.isArray(p.requires) ? p.requires : [];
    const missing = reqs.filter((r) => !available.has(r));
    const ring = missing.length > 0 && missing.every((r) => remainingProvides.has(r));
    failed.push({
      id,
      reason: ring
        ? `依赖成环：${missing.join("、")} 由同样无法启用的插件提供`
        : `依赖能力 ${missing.join("、")} 缺失（提供者未安装/未启用）`,
    });
  }
  return { spawnable, failed };
}
