/**
 * 全局热键覆盖的改键执行：设置页改键后按声明定位当前注册，OS 层「先注册新键 → 成功后注销
 * 旧键」——新键被其他插件占用即失败、旧键保持不动（不回滚、不静默），成功后把本内核的
 * 登记（触发回调 + 声明元数据）迁到新键。
 *
 * 实际键解析与 ctx.shortcuts.registerDeclared 同源（用户覆盖 → 声明默认），声明缺失报错。
 * 只在发起改键的窗口执行 Rust 侧重注册：OS 登记是应用级的，注册成功即全局生效；触发回调
 * 由 Rust 固定转发主窗口分发，设置页在主窗口打开，本窗口登记迁移即 dispatch 生效所需。
 * 插件当前未注册（未挂载/未调 registerDeclared）时无事可做——挂载注册时按覆盖表解析。
 */
import {
  registerGlobalShortcut,
  registerWindowToggleShortcut,
  unregisterGlobalShortcut,
} from "@/services/globalShortcut";
import { getPluginShortcutAccess } from "./access";
import {
  declaredShortcutOf,
  moveShortcutRegistration,
  trackPendingShortcutRegister,
} from "./pluginShortcuts";

/** 改键执行结果。 */
export interface ApplyShortcutOverrideOutcome {
  ok: boolean;
  /** 本次是否发生了 OS 层重注册（false = 无既有注册或生效键未变）。 */
  changed: boolean;
  /** 解析出的实际生效键（ok 时有意义）。 */
  accelerator?: string;
  error?: string;
}

/** 执行一次覆盖变更（override = undefined 表示清除覆盖、回到声明默认键）。 */
export async function applyShortcutOverride(
  ctx: object,
  pluginId: string,
  declarationId: string,
  override: string | undefined,
): Promise<ApplyShortcutOverrideOutcome> {
  const access = getPluginShortcutAccess();
  const decl = access?.declarations(pluginId).find((d) => d.id === declarationId);
  if (!decl) {
    return { ok: false, changed: false, error: `热键声明 ${declarationId} 不存在` };
  }
  const newAccelerator = override || decl.key;
  const current = declaredShortcutOf(ctx, pluginId, declarationId);
  if (!current || current.accelerator === newAccelerator) {
    return { ok: true, changed: false, accelerator: newAccelerator };
  }
  // 在途注册进 pending 表：与「注册后立即停用」竞态对齐——插件停用释放会先等本注册落地，
  // 再按插件整体注销，避免旧键已释放、新键后落地留下的幽灵热键
  let register: Promise<void>;
  if (current.meta.windowToggle) {
    register = registerWindowToggleShortcut(newAccelerator, pluginId, current.meta.windowToggle);
  } else {
    register = registerGlobalShortcut(newAccelerator, pluginId);
  }
  trackPendingShortcutRegister(ctx, pluginId, register);
  try {
    await register;
  } catch (e) {
    return {
      ok: false,
      changed: false,
      accelerator: newAccelerator,
      error: e instanceof Error ? e.message : String(e),
    };
  }
  // 新键已就位才注销旧键：注销失败（罕见）不阻塞改键——旧键归属仍是本插件，
  // 随插件停用整体释放兜底；记可见日志
  try {
    await unregisterGlobalShortcut(current.accelerator, pluginId);
  } catch (e) {
    console.error(`注销旧全局快捷键 ${current.accelerator} 失败（插件停用时会整体释放）`, e);
  }
  // 等待期间插件可能已被停用（登记已随释放清空）或并发重注册过同一声明：仅当登记仍是
  // 本次的旧键时才迁移，避免把条目写回已清空的登记表
  const latest = declaredShortcutOf(ctx, pluginId, declarationId);
  if (latest?.accelerator === current.accelerator) {
    moveShortcutRegistration(ctx, pluginId, current.accelerator, newAccelerator, current);
  }
  return { ok: true, changed: true, accelerator: newAccelerator };
}
