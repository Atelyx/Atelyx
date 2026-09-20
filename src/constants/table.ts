/**
 * 多维表格（.atb）相关常量。
 */
import type { CalcType, FieldType } from "@/types/table";

/** `.atb` 文件 schema 版本号（Rust 侧 `vault.rs` 有同名常量，两端须保持一致）。 */
export const TABLE_SCHEMA = "atelyx-table/v1" as const;

/** 字段类型显示名（字段管理菜单 / 类型切换用）。 */
export const FIELD_TYPE_LABELS: Record<FieldType, string> = {
  text: "文本",
  number: "数字",
  duration: "时长（秒）",
  singleSelect: "单选",
  image: "图片",
};

/** 状态栏自动计算类型显示名。 */
export const CALC_TYPE_LABELS: Record<CalcType, string> = {
  sum: "求和",
  avg: "平均",
  max: "最大",
  min: "最小",
  count: "计数",
};

/** 各字段类型可用的自动计算（数字类统计，其余计数非空值）。 */
export const CALC_TYPES_BY_FIELD: Record<FieldType, CalcType[]> = {
  number: ["sum", "avg", "max", "min", "count"],
  duration: ["sum", "avg", "max", "min", "count"],
  text: ["count"],
  singleSelect: ["count"],
  image: ["count"],
};

/** 注入对话上下文的表格行数上限（防超长上下文）。 */
export const MAX_TABLE_INJECT_ROWS = 50;
/** 列宽拖拽调整的下限（px）。 */
export const MIN_COL_WIDTH = 120;
/** 列宽拖拽调整的上限（px）。 */
export const MAX_COL_WIDTH = 360;
/** 行高拖拽调整的下限（px；行首操作区高 32px）。 */
export const MIN_ROW_HEIGHT = 32;
/** 行高拖拽调整的上限（px）。 */
export const MAX_ROW_HEIGHT = 600;
/** 行号列固定宽度（px）。 */
export const ROW_NUM_COL_WIDTH = 40;
/** 表头末尾「+」添加字段列固定宽度（px）。 */
export const ADD_FIELD_COL_WIDTH = 48;
/** 表格视图缩放下限 / 上限（Ctrl+滚轮缩放，纯视图状态不持久化）。 */
export const MIN_TABLE_ZOOM = 0.5;
export const MAX_TABLE_ZOOM = 2;

// ===== 单元格格式（样式随行 `styles[fieldId]` 持久化，见 types/table.ts CellStyle）=====

/** 字体预设：`key` 落盘（CellStyle.font，紧凑跨平台），`fontFamily` 仅渲染用 CSS 栈。
 *  缺省（不设） = 表格基准字体；存「默认」项供工具栏选择清除。 */
export interface FontPreset {
  key: string;
  label: string;
  /** CSS font-family 栈（含跨平台回退）；缺省 = 不设。 */
  fontFamily?: string;
}

export const FONT_PRESETS: FontPreset[] = [
  { key: "default", label: "默认" },
  { key: "sans", label: "无衬线", fontFamily: "'Segoe UI', 'Microsoft YaHei', 'PingFang SC', 'Noto Sans CJK SC', sans-serif" },
  { key: "serif", label: "衬线", fontFamily: "Georgia, 'Times New Roman', 'Songti SC', 'SimSun', serif" },
  { key: "mono", label: "等宽", fontFamily: "'Cascadia Code', Consolas, 'Courier New', 'Noto Sans Mono CJK SC', monospace" },
  { key: "hei", label: "黑体", fontFamily: "'Microsoft YaHei', 'PingFang SC', 'Noto Sans CJK SC', sans-serif" },
  { key: "song", label: "宋体", fontFamily: "'SimSun', 'Songti SC', 'Noto Serif CJK SC', serif" },
  { key: "kai", label: "楷体", fontFamily: "'KaiTi', 'Kaiti SC', 'STKaiti', 'Noto Serif CJK SC', serif" },
  { key: "fang", label: "仿宋", fontFamily: "'FangSong', 'FangSong_GB2312', 'STFangsong', serif" },
];

/** 字体预设键 → CSS font-family（缺省 = undefined）；渲染/工具栏共用。 */
export function fontFamilyOf(key: string | undefined): string | undefined {
  return FONT_PRESETS.find((f) => f.key === key)?.fontFamily;
}

/** 字号选项（px；12 = 表格基准字号 text-xs；缺省 = 不设）。 */
export const FONT_SIZE_OPTIONS = [12, 13, 14, 16, 18, 20, 24] as const;

/** 文字颜色预设色板（hex）；「默认」= 清除（不设 key）。 */
export const TEXT_COLOR_PRESETS = [
  "#e05252",
  "#e07b39",
  "#c99a16",
  "#4fae6a",
  "#2fa3b3",
  "#4f8fd0",
  "#8a5ad0",
  "#d94f9c",
  "#8a6d4a",
  "#8a8a8a",
];

/** 单元格背景色预设色板（hex 浅色系）；「默认」= 清除（不设 key）。 */
export const BG_COLOR_PRESETS = [
  "#fde2e2",
  "#fde7d8",
  "#fdf0cf",
  "#e3f3e5",
  "#dcf1f4",
  "#e0ecfb",
  "#ece3fa",
  "#fbe3f0",
  "#f0e6d8",
  "#e8e8e8",
];

// ===== 图片单元格展示 =====

/** 轮播模式图片区默认高度（自适应行；固定行高时填满行高剩余空间）。 */
export const IMAGE_CAROUSEL_AREA_HEIGHT = 96;
/** 底部图片队列的缩略图边长（px）。 */
export const IMAGE_QUEUE_THUMB_SIZE = 28;
/** 队列相邻缩略图间距（px）。 */
export const IMAGE_QUEUE_GAP = 4;
/** 九宫格方块间距（px）。 */
export const IMAGE_GRID_GAP = 4;
/** 底部图片队列条高度（缩略图 28 + 描边 4 + 上下内衬 4）。 */
export const IMAGE_QUEUE_STRIP_HEIGHT = 36;
/** 长按进入拖动排序的等待时长（ms；先于长按移动 = 滑动切换/取消按压）。 */
export const IMAGE_LONG_PRESS_MS = 400;
/** 长按激活前允许的位移（视口 px；超过 = 取消按压：轮播转滑动、拖拽放弃）。 */
export const IMAGE_PRESS_CANCEL_PX = 6;
/** 滑动翻页阈值（视口 px；松手时绝对位移超过即翻页，否则回弹）。 */
export const IMAGE_SWIPE_THRESHOLD = 48;
/** 两端阻尼回弹系数（第一张左拖 / 最后一张右拖的位移衰减）。 */
export const IMAGE_SWIPE_EDGE_DAMPING = 0.35;
/** 九宫格宽列阈值（px；≥3 张图时列宽 ≥ 此值 3 列，否则 2 列）。 */
export const IMAGE_GRID_WIDE_MIN = 200;
/** 九宫格单图方块最大宽度（px）。 */
export const IMAGE_GRID_SINGLE_MAX = 160;
/** 单个图片文件导入上限（字节）：与服务端单文件写限额（按解码后字节计）一致；
 * 前端在读取/传输前预检拒绝，超限文件不发起网络传输。 */
export const TABLE_IMAGE_MAX_BYTES = 50 * 1024 * 1024;
