/**
 * 快捷键键串的序列化与展示（两套格式，互不混用）：
 * - 应用内命令格式（services/cordis/commandHotkeys 的匹配约定）：`mod+k`——mod = Ctrl/Cmd
 *   任一，主键 = `e.key` 小写，修饰键集合精确匹配；
 * - OS accelerator 格式（Rust 侧 `tauri_plugin_global_shortcut::Shortcut` 解析）：
 *   `CmdOrCtrl+Shift+KeyK`——主键用物理 Code 名（`e.code`），跨输入法布局稳定。
 */

/** 纯修饰键按下（无主键，不可绑定）。 */
const MODIFIER_ONLY_KEYS = new Set(["Control", "Meta", "Alt", "Shift", "CapsLock"]);

/** 键串展示文案的固定标签。 */
const MODIFIER_LABELS: Record<string, string> = {
  mod: "Ctrl/Cmd",
  ctrl: "Ctrl",
  meta: "Cmd",
  alt: "Alt",
  shift: "Shift",
  delete: "Del",
  backspace: "Backspace",
  escape: "Esc",
  enter: "Enter",
  tab: "Tab",
  space: "Space",
  arrowup: "↑",
  arrowdown: "↓",
  arrowleft: "←",
  arrowright: "→",
};

// ===== 应用内命令快捷键匹配（mod = Ctrl/Cmd 任一，修饰键集合精确相等） =====

/** 修饰键白名单（快捷键解析用）。 */
const COMMAND_MODIFIERS = new Set(["mod", "ctrl", "meta", "shift", "alt"]);

/** 从快捷键 token 中取主键（首个非修饰键 token）。 */
function keyOf(parts: string[]): string | undefined {
  return parts.find((p) => !COMMAND_MODIFIERS.has(p));
}

/** 快捷键声明的修饰键匹配集（物理修饰键的精确组合）：mod 声明 = 「其余键 + ctrl」或「其余键 + meta」两套替代。 */
function declaredModifierSets(parts: string[]): string[][] {
  const rest = parts.filter((p) => COMMAND_MODIFIERS.has(p) && p !== "mod");
  if (parts.includes("mod")) return [[...rest, "ctrl"], [...rest, "meta"]];
  return [rest];
}

/** 快捷键是否命中当前键盘事件（mod = Ctrl/Cmd 任一；主键大小写不敏感；修饰键集合精确匹配：
 *  多按的修饰键不算命中——注册 mod+k 时 Ctrl+Shift+K 不触发，防与用户刻意组合键冲突）。 */
export function matchesShortcut(shortcut: string, e: KeyboardEvent): boolean {
  const parts = shortcut.split("+").map((p) => p.trim().toLowerCase());
  const key = keyOf(parts);
  if (!key) return false;
  if (e.key.toLowerCase() !== key) return false;
  const pressed: string[] = [];
  if (e.ctrlKey) pressed.push("ctrl");
  if (e.metaKey) pressed.push("meta");
  if (e.shiftKey) pressed.push("shift");
  if (e.altKey) pressed.push("alt");
  return declaredModifierSets(parts).some(
    (alt) => alt.length === pressed.length && alt.every((m) => pressed.includes(m)),
  );
}

/**
 * 键盘事件 → 键串。纯修饰键按下返回 null（无可绑定主键）。
 * command 格式用 `e.key`（与事件时匹配同源），accelerator 格式用 `e.code`（物理键位）。
 */
export function serializeCommandEvent(
  e: KeyboardEvent,
  format: "command" | "accelerator",
): string | null {
  if (MODIFIER_ONLY_KEYS.has(e.key)) return null;
  const mods: string[] = [];
  if (e.ctrlKey || e.metaKey) mods.push(format === "command" ? "mod" : "CmdOrCtrl");
  if (e.altKey) mods.push(format === "command" ? "alt" : "Alt");
  if (e.shiftKey) mods.push(format === "command" ? "shift" : "Shift");
  const key = format === "command" ? e.key.toLowerCase() : e.code;
  // 命令格式以 "+" 分隔且无转义：主键为 "+" 时序列化结果无法被解析命中，拒绝录制
  if (!key || key === "+") return null;
  return [...mods, key].join("+");
}

/** 应用内命令键串 → 展示文案（"mod+shift+k" → "Ctrl/Cmd + Shift + K"）。 */
export function formatCommandShortcut(shortcut: string): string {
  return shortcut
    .split("+")
    .map((part) => {
      const label = MODIFIER_LABELS[part.toLowerCase()];
      if (label) return label;
      if (part === " ") return "Space";
      return part.toUpperCase();
    })
    .join(" + ");
}

/** OS accelerator → 展示文案（"CmdOrCtrl+Shift+KeyK" → "Ctrl/Cmd + Shift + K"）。 */
export function formatAccelerator(accelerator: string): string {
  return accelerator
    .split("+")
    .map((part) => {
      const lower = part.toLowerCase();
      if (MODIFIER_LABELS[lower]) return MODIFIER_LABELS[lower];
      if (lower === "cmdorctrl" || lower === "commandorcontrol") return "Ctrl/Cmd";
      if (lower === "option") return "Alt";
      if (part === "Space") return "Space";
      // 物理 Code 名收敛为可读主键：KeyK → K、Digit1 → 1；其余（F5/Minus/ArrowUp 等）原样
      if (/^Key[A-Z]$/.test(part)) return part.slice(3);
      if (/^Digit\d$/.test(part)) return part.slice(5);
      return part;
    })
    .join(" + ");
}

/**
 * 命令键串的冲突判定键：同键串（含书写差异 mod+K / MOD+k）归一到同一键。
 * 修饰键集合与主键大小写不参与区分——匹配语义本就精确相等。
 */
export function commandShortcutConflictKey(shortcut: string): string {
  const parts = shortcut.split("+").map((p) => p.trim().toLowerCase()).filter(Boolean);
  const isMod = (p: string) => ["mod", "ctrl", "meta", "shift", "alt"].includes(p);
  const key = parts.find((p) => !isMod(p)) ?? "";
  // ctrl / meta 书写与 mod 同义（mod = 两者任一，匹配语义下 ctrl+k 与 mod+k 都命中 Ctrl+K），
  // 归一为 mod 且不进修饰键集合，保证同键不同书写的撞键检测一致
  const mods = parts.filter((p) => isMod(p) && p !== "mod" && p !== "ctrl" && p !== "meta").sort();
  const hasMod = parts.some((p) => p === "mod" || p === "ctrl" || p === "meta");
  return [...(hasMod ? ["mod"] : []), ...mods, key].join("+");
}

/** 全局热键 accelerator 的冲突判定键：按 OS 语义修饰键与主键归一（书写顺序无关）。 */
export function acceleratorConflictKey(accelerator: string): string {
  const parts = accelerator.split("+").map((p) => p.trim()).filter(Boolean);
  const norm = parts.map((p) => {
    const lower = p.toLowerCase();
    if (lower === "cmdorctrl" || lower === "commandorcontrol") return "cmdorctrl";
    if (lower === "ctrl" || lower === "control") return "ctrl";
    if (lower === "cmd" || lower === "command" || lower === "meta" || lower === "super") return "cmd";
    if (lower === "alt" || lower === "option") return "alt";
    if (lower === "shift") return "shift";
    return p;
  });
  const key = norm.find((p) => !["cmdorctrl", "ctrl", "cmd", "alt", "shift"].includes(p)) ?? "";
  const mods = norm.filter((p) => p !== key).sort();
  return [...mods, key].join("+");
}
