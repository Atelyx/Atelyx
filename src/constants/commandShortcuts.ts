/**
 * 内置命令快捷键清单（默认键与作用域的唯一事实源）：builtins.tsx 据此注册命令，各作用域
 * 监听点据此取默认键并叠加用户覆盖（settingsStore.commandShortcuts）得生效键；
 * globalId = `<pluginId>:<id>`。键串格式见 utils/shortcutKeys；编辑语义键与 Esc 不开放自定义。
 */
import type { CommandShortcutScope } from "@/types";

/** 单条内置命令快捷键定义。 */
export interface BuiltinCommandShortcutDef {
  pluginId: string;
  id: string;
  label: string;
  /** 默认快捷键（undefined = 未绑定，仅可经设置页赋予）。 */
  shortcut?: string;
  scope: CommandShortcutScope;
  /**
   * 录制约束：require-mod = 须含 Ctrl/Cmd（监听点在 mod 分支内匹配，裸键永不生效）；
   * no-mod = 不得含 Ctrl/Cmd/Alt（监听点在 mod/alt 早退之后匹配）；缺省 = 无约束。
   * 设置页录制时据此校验，防录出永不生效的死绑定。
   */
  bindingRule?: "require-mod" | "no-mod";
}

export const BUILTIN_CANVAS_PLUGIN_ID = "builtin.canvas";
export const BUILTIN_NOTE_PLUGIN_ID = "builtin.note";
export const BUILTIN_TABLE_PLUGIN_ID = "builtin.table";

export const BUILTIN_COMMAND_SHORTCUTS: readonly BuiltinCommandShortcutDef[] = [
  // 画布（生效条件 = 画布面板聚焦且非只读，由 useCanvasHotkeys 门控）
  { pluginId: BUILTIN_CANVAS_PLUGIN_ID, id: "delete", label: "删除选中", shortcut: "delete", scope: "canvas" },
  { pluginId: BUILTIN_CANVAS_PLUGIN_ID, id: "copy", label: "复制节点", shortcut: "mod+c", scope: "canvas" },
  { pluginId: BUILTIN_CANVAS_PLUGIN_ID, id: "paste", label: "粘贴节点", shortcut: "mod+v", scope: "canvas" },
  { pluginId: BUILTIN_CANVAS_PLUGIN_ID, id: "undo", label: "撤销", shortcut: "mod+z", scope: "canvas" },
  { pluginId: BUILTIN_CANVAS_PLUGIN_ID, id: "redo", label: "重做", shortcut: "mod+y", scope: "canvas" },
  { pluginId: BUILTIN_CANVAS_PLUGIN_ID, id: "selectAll", label: "全选", shortcut: "mod+a", scope: "canvas" },
  // 笔记（生效条件 = 焦点在笔记编辑面，由 useNoteUndoRouting 按编辑面路由；监听点仅在 mod 组合内匹配）
  { pluginId: BUILTIN_NOTE_PLUGIN_ID, id: "undo", label: "撤销", shortcut: "mod+z", scope: "note-editing", bindingRule: "require-mod" },
  { pluginId: BUILTIN_NOTE_PLUGIN_ID, id: "redo", label: "重做", shortcut: "mod+y", scope: "note-editing", bindingRule: "require-mod" },
  // 表格（生效条件 = 表格面板聚焦，由 TableEditor 门控；撤销重做与剪贴板键在 mod 分支内匹配，
  // 清空选中在 mod/alt 早退之后匹配）
  { pluginId: BUILTIN_TABLE_PLUGIN_ID, id: "undo", label: "撤销", shortcut: "mod+z", scope: "table", bindingRule: "require-mod" },
  { pluginId: BUILTIN_TABLE_PLUGIN_ID, id: "redo", label: "重做", shortcut: "mod+y", scope: "table", bindingRule: "require-mod" },
  { pluginId: BUILTIN_TABLE_PLUGIN_ID, id: "copy", label: "复制", shortcut: "mod+c", scope: "table", bindingRule: "require-mod" },
  { pluginId: BUILTIN_TABLE_PLUGIN_ID, id: "cut", label: "剪切", shortcut: "mod+x", scope: "table", bindingRule: "require-mod" },
  { pluginId: BUILTIN_TABLE_PLUGIN_ID, id: "paste", label: "粘贴", shortcut: "mod+v", scope: "table", bindingRule: "require-mod" },
  { pluginId: BUILTIN_TABLE_PLUGIN_ID, id: "clear", label: "清空选中", shortcut: "delete", scope: "table", bindingRule: "no-mod" },
];

/** globalId → 默认快捷键（监听点解析生效键用；无默认 = undefined）。 */
export const BUILTIN_COMMAND_SHORTCUT_DEFAULTS: Record<string, string | undefined> =
  Object.fromEntries(
    BUILTIN_COMMAND_SHORTCUTS.map((c) => [`${c.pluginId}:${c.id}`, c.shortcut]),
  );

/** 录制键是否满足该命令的修饰键约束（不满足返回可读原因，满足返回 null）。 */
export function commandBindingError(
  def: Pick<BuiltinCommandShortcutDef, "bindingRule">,
  shortcut: string,
): string | null {
  const parts = shortcut.split("+").map((p) => p.trim().toLowerCase());
  const hasMod = parts.includes("mod") || parts.includes("ctrl") || parts.includes("meta");
  if (def.bindingRule === "require-mod" && !hasMod) {
    return "该命令在编辑/面板上下文中生效，组合键需包含 Ctrl/Cmd";
  }
  if (def.bindingRule === "no-mod" && (hasMod || parts.includes("alt"))) {
    return "该命令只在无 Ctrl/Cmd/Alt 组合时触发，请使用单键或仅 Shift 组合";
  }
  return null;
}
