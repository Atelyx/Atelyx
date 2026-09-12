/**
 * 槽位声明表：宿主实际渲染的扩展位置（唯一清单）。
 *
 * 固定具名槽（titlebar/toolbar/panelhead/statusbar/settings）与右键菜单目标（contextmenu/<target>）
 * 必须在表内登记——注册进未声明的槽即失败并给近似槽名提示（治「槽名拼错静默丢失」与「乱占位置」）；
 * 载荷须匹配声明的字段契约。
 * 开放 kind 槽（view/node/edge/tableview）按前缀放行：插件可注册任意 kind/type 出现在「添加视图」
 * 菜单与画布节点集合里，这是既有的开放语义，不做逐项白名单。
 * 新增宿主渲染位置时在此登记（缺登记 = 该位置无法被插件贡献）。
 * 表与元素均深冻结：`ctx.slots.list()` 暴露同一引用，冻结防插件运行时改写全局校验依据。
 */
import type { SlotCardinality } from "@/utils/cordis/slots";

/** 一条槽位声明。字段全部只读：声明表暴露给插件（`ctx.slots.list()`），与运行时深冻结一致。 */
export interface SlotDeclaration {
  /** 槽名；开放 kind 槽写前缀（如 "view"）。 */
  readonly key: string;
  /** 前缀匹配（开放 kind 槽：`<key>/<任意非空>` 均合法）。 */
  readonly prefix?: boolean;
  /** 基数（single 胜出 / list 多贡献有序）。 */
  readonly cardinality: SlotCardinality;
  /** 必需载荷字段。 */
  readonly required: readonly string[];
  /** 可选载荷字段。 */
  readonly optional?: readonly string[];
  /** 渲染位置。 */
  readonly scope: string;
  /** 用途（仅当比 scope 多出信息时才写：固定具名座位的 scope 已自解释）。 */
  readonly summary?: string;
}

/** 宿主渲染的具名座位载荷：一个渲染组件。 */
const COMPONENT_ONLY = ["component"] as const;

/** 固定具名槽（宿主渲染的座位）：键 = 槽名，值 = 渲染位置；全部 list 槽 + 单一 component 载荷。 */
const FIXED_SLOTS: Record<string, string> = {
  "titlebar/right": "标题栏右侧动作区",
  "panelhead/status": "面板标签头状态区",
  "statusbar/canvas": "画布底部状态栏",
  "toolbar/note/right": "笔记编辑器工具栏右侧",
  "toolbar/table/right": "表格工具栏右侧",
  "toolbar/files": "文件面板工具栏",
  "settings/general": "通用设置页追加区块",
  "settings/theme": "主题设置页追加区块",
  "settings/collab": "多人协作设置页追加区块",
  "settings/editor": "编辑器设置页追加区块",
  "settings/modelServices": "模型服务设置页追加区块",
  "settings/files": "文件与路径设置页追加区块",
  "settings/search": "联网搜索设置页追加区块",
};

const DECLARATIONS: readonly SlotDeclaration[] = [
  ...Object.entries(FIXED_SLOTS).map(([key, scope]) => ({
    key,
    cardinality: "list" as const,
    required: COMPONENT_ONLY,
    scope,
  })),

  // 右键菜单目标（宿主渲染的座位；载荷为 { label, onClick }，与普通 UI 槽不同故不并入 FIXED_SLOTS）
  {
    key: "contextmenu/canvas",
    cardinality: "list",
    required: ["label", "onClick"],
    scope: "画布空白右键菜单",
  },

  // ── 开放 kind 槽（前缀放行；插件自定 kind/type）─────────────────────────────
  {
    key: "view",
    prefix: true,
    cardinality: "single",
    required: ["label"],
    optional: ["component", "render"],
    scope: "面板视图",
    summary: "注册任意视图 kind，出现在「添加视图」菜单；重型视图用 render(hostId) 承载宿主面板 id。",
  },
  {
    key: "tableview",
    prefix: true,
    cardinality: "single",
    required: ["label", "component"],
    scope: "表格编辑器内视图",
    summary: "注册任意表格视图 kind。",
  },
  {
    key: "node",
    prefix: true,
    cardinality: "single",
    required: COMPONENT_ONLY,
    scope: "画布节点",
    summary: "注册任意画布节点 type（同 type 高 priority 覆盖内置基座）。",
  },
  {
    key: "edge",
    prefix: true,
    cardinality: "single",
    required: COMPONENT_ONLY,
    scope: "画布边",
    summary: "注册任意画布边 type。",
  },
];

/** 槽位声明清单（深冻结：`ctx.slots.list()` 暴露同一引用，冻结防运行时改写）。 */
export const SLOT_DECLARATIONS: readonly SlotDeclaration[] = Object.freeze(
  DECLARATIONS.map((decl) =>
    Object.freeze({
      ...decl,
      required: Object.freeze([...decl.required]),
      ...(decl.optional ? { optional: Object.freeze([...decl.optional]) } : {}),
    }),
  ),
);

/** 载荷形状（由字段契约派生，供文档与错误提示；不在声明里另存一份以免漂移）。 */
export function slotPayloadShape(decl: SlotDeclaration): string {
  const fields = [...decl.required, ...(decl.optional ?? []).map((f) => `${f}?`)];
  return `{ ${fields.join(", ")} }`;
}

/** 解析槽声明：固定槽精确匹配，开放槽按 `<前缀>/<非空>` 匹配（无匹配 = 未声明）。 */
export function findSlotDeclaration(slot: string): SlotDeclaration | undefined {
  const exact = SLOT_DECLARATIONS.find((d) => !d.prefix && d.key === slot);
  if (exact) return exact;
  // 裸前缀（"view"）与空 kind（"view/"）不算合法槽：宿主按 `<前缀>/<kind>` 查询，注册它们会静默不渲染。
  return SLOT_DECLARATIONS.find(
    (d) => d.prefix === true && slot.startsWith(`${d.key}/`) && slot.length > d.key.length + 1,
  );
}

/** 近似槽名提示（同族候选按与目标槽名的公共前缀长度降序；注册失败时的可读原因用）。 */
export function suggestSlotNames(slot: string, limit = 3): string[] {
  const head = slotHead(slot);
  const take = Math.max(0, Math.trunc(limit));
  return SLOT_DECLARATIONS.filter((d) => !d.prefix && d.key.startsWith(`${head}/`))
    .map((d) => d.key)
    .sort((a, b) => commonPrefixLength(b, slot) - commonPrefixLength(a, slot))
    .slice(0, take);
}

/** 槽名首段（`toolbar/note/right` → `toolbar`；无分隔符时取原名）。 */
function slotHead(slot: string): string {
  const at = slot.indexOf("/");
  return at === -1 ? slot : slot.slice(0, at);
}

/** 两串公共前缀长度。 */
function commonPrefixLength(a: string, b: string): number {
  const max = Math.min(a.length, b.length);
  let i = 0;
  while (i < max && a[i] === b[i]) i += 1;
  return i;
}
