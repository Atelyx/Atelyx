/**
 * 画布相关常量。
 */

/** 新建对话节点的默认尺寸（px）。较大便于直接输入多轮对话；用户可手动 resize 覆盖。 */
export const DEFAULT_CONVERSATION_WIDTH = 480;
export const DEFAULT_CONVERSATION_HEIGHT = 420;
/** 文本节点默认宽度（拖拽建节点/提取时初始宽度，创建时显式写入节点数据）。 */
export const DEFAULT_TEXT_NODE_WIDTH = 450;
/** 文本节点默认高度（拖拽建节点/提取时初始高度；resize 后随 .atlx 持久化覆盖）。 */
export const DEFAULT_TEXT_NODE_HEIGHT = 420;

/** 分组节点默认尺寸（空白右键「添加分组」创建时写入节点数据）。 */
export const DEFAULT_GROUP_WIDTH = 480;
export const DEFAULT_GROUP_HEIGHT = 320;

/** 链接节点默认尺寸（空白右键「添加链接」创建时写入节点数据）。 */
export const DEFAULT_LINK_WIDTH = 240;
export const DEFAULT_LINK_HEIGHT = 96;

/** 表格节点默认尺寸（文件面板拖 .atb 建节点时写入节点数据）。 */
export const DEFAULT_TABLE_NODE_WIDTH = 360;
export const DEFAULT_TABLE_NODE_HEIGHT = 280;

/**
 * 分组节点色板（对齐外部白板格式的颜色下标 "1"-"5"）。
 * 分组节点 header 色块按钮弹出选择（1-5 或默认）；缺省 = 默认中性灰（与 "6" 同色故不提供）。
 */
export const GROUP_COLORS: Record<string, string> = {
  "1": "#e05252",
  "2": "#e07b39",
  "3": "#4fae6a",
  "4": "#4f8fd0",
  "5": "#9b6fd0",
};

/**
 * 画布节点类型标识色（标题栏左侧 3px 色条）：对话 = 品牌金、文本 = 紫、搜索 = 蓝、媒体 = 绿、表格 = 中性。
 * 固定调色板（不随主题翻转），与 GROUP_COLORS 同属画布域语义色，不是主题 token。
 */
export const NODE_TYPE_COLORS: Record<string, string> = {
  conversation: "var(--accent)",
  text: "#c98be0",
  search: "var(--info)",
  media: "var(--success)",
  table: "var(--text-muted)",
};

/** 类型色条外形：各节点共用同一常量，尺寸/圆角改动只在此处。 */
export const NODE_TYPE_BAR_CLASS = "w-[3px] h-3 rounded-full flex-shrink-0";

/** `.atlx` 文件 schema 版本号（Rust 侧 `vault.rs` 有同名常量，两端须保持一致）。 */
export const CANVAS_SCHEMA = "atelyx-canvas/v1" as const;
