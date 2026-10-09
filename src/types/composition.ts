/**
 * 组合层契约：组合行「id → 实现来源」的 patch 声明与裁决结果。
 * 组合行 id 承担装配位置与顺序、插件自持数据归属、审计与启停真源，必须稳定；`impl` 决定
 * 装配时跑谁的代码（缺省 = 行自身），patch 只改 impl。声明分两层，后者赢：
 * 插件清单声明（`atelyx.compositionPatch`）→ 用户层（`global.json`）。
 */
/** 生效实现来源：default = 行自身；plugin = 插件清单声明；user = 用户层钉住。 */
type CompositionImplSource = "default" | "plugin" | "user";

/** 用户层「恢复该行默认实现」的取值（其余取值一律按插件 id 解释）。 */
export const COMPOSITION_IMPL_DEFAULT = "default";

/** 用户层 patch 表（`global.json` 的 `compositionPatches`）：目标行 id → 实现 id（或 `"default"`）。
 *  用户层条目恒胜插件声明，即「钉住」；删除键 = 解除钉住（回到插件声明层）。 */
export type CompositionUserPatches = Record<string, string>;

/** 插件清单的接管声明条目（`atelyx.compositionPatch` 数组项）：目标行由本插件实现装配。 */
export interface CompositionPatchDeclaration {
  /** 目标组合行 id（如 `builtin.chatcore`）。 */
  target: string;
  /** 多个声明方竞争同一目标行时的胜出序（缺省 0；高者胜，同值按声明方插件 id 升序取首个）。 */
  priority?: number;
}

/** 声明方（裁决后的展示形态；priority 降序）。 */
export interface CompositionDeclarer {
  pluginId: string;
  priority: number;
}

/** 单个组合行的裁决结果与治理展示字段。 */
export interface CompositionBinding {
  rowId: string;
  /** 该行自身实现是否可用（命中随应用编译的实现，或该插件已装且已启用且有入口）。 */
  selfAvailable: boolean;
  /** 声明接管本行的插件（priority 降序）。 */
  declarers: CompositionDeclarer[];
  /** 用户层条目（null = 未钉住）。 */
  userImpl: string | null;
  /** 实际装配的实现 id（声明的实现不可用时回退为 rowId）。 */
  implId: string;
  /** 生效来源。 */
  source: CompositionImplSource;
  /** 声明的实现不可用时的可读原因（非空 = implId 已回退为 rowId）。 */
  problem?: string;
}

/** 未命中任何组合行的插件声明（目标行不存在；不静默）。 */
export interface CompositionUnmatchedDeclaration {
  pluginId: string;
  target: string;
}

/** 组合层裁决结果。 */
export interface CompositionResolution {
  /** 装配计划（按装配顺序；已剔除被引用为实现提供者的插件行——它们只作为别处的实现出现）。 */
  mounts: CompositionMount[];
  /** 逐行裁决（键 = 行 id）；含未装/停用行，供管理页展示归属。 */
  bindings: Record<string, CompositionBinding>;
  /** 目标行不存在的声明（管理页提示用）。 */
  unmatched: CompositionUnmatchedDeclaration[];
}

/** 一条装配计划项：在某行的位置上跑某份实现。 */
export interface CompositionMount {
  rowId: string;
  /** 实际装配的实现 id；与 rowId 不同 = 该行已被接管。 */
  implId: string;
}
