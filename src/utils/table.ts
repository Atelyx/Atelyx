/**
 * 表格工具纯函数：列宽自适应、内容快照注入、状态栏列自动计算、增量补丁计算、磁盘/内存内容比对、
 * 图片值归一化（磁盘/远端旧形态 → ImageCellValue，内存恒新形态）、选中区域归约与复制粘贴（TSV）。
 *
 * - `fieldDefaultWidth`：列宽按字段名自适应（CJK 双宽，钳制 [MIN_COL_WIDTH, MAX_COL_WIDTH]）。
 * - `tableToSnapshotText`：表格 → 注入文本快照（行限 `MAX_TABLE_INJECT_ROWS`；image → `[图 N 张]`；
 *   多行文本压单行空格；超行数截断标注）。入参须为归一化内存态，勿直接喂磁盘原始 JSON。
 * - `computeColumnCalc`：按字段 calcType 统计全列，返回显示文本（数字统计 / 非空计数）。
 * - `computeTablePatch`：增量补丁计算（保存写盘与协作实时广播共用）。
 * - `tablesEqual`：磁盘表格与内存内容比对（watcher 回放判别）。
 * - `selectionRegion`：选中范围 → 矩形区域（复制/粘贴/清空/协作高亮/右键命中统一复用）。
 * - `buildRegionTsv`/`parseTsv`/`applyPasteGrid`：选中区域 ↔ 剪贴板 TSV ↔ 网格回写。
 */
import { MAX_TABLE_INJECT_ROWS, MAX_COL_WIDTH, MIN_COL_WIDTH, fontFamilyOf } from "@/constants/table";
import type { CSSProperties } from "react";
import type {
  CellStyle,
  CellValue,
  CollabPeer,
  CollabSelection,
  ImageCellValue,
  PluginTableSnapshot,
  TableField,
  TableFile,
  TablePatch,
  TableRow,
} from "@/types";

// ===== 图片单元格值归一化（磁盘/远端旧形态 string[] → ImageCellValue；内存恒为新形态）=====

/** 图片值归一化：旧形态字符串数组 → `{ images }`；对象形态缺 images 补空；其余 → 空图片值。
 * 两种形态同口径剔除非字符串条目（磁盘/远端脏数据不得进内存）。 */
export function normalizeImageValue(v: unknown): ImageCellValue {
  if (Array.isArray(v)) return { images: v.filter((x): x is string => typeof x === "string") };
  if (typeof v === "object" && v !== null) {
    const o = v as Partial<ImageCellValue>;
    if (!Array.isArray(o.images)) return { images: [] };
    // 正常新形态原引用返回（零拷贝）；含脏条目才重建
    if (o.images.every((x) => typeof x === "string")) return o as ImageCellValue;
    return { images: o.images.filter((x): x is string => typeof x === "string") };
  }
  return { images: [] };
}

/**
 * 行值归一化：值映射里的旧形态图片值（string[]，仅 image 字段会出现）归一化为 ImageCellValue，
 * 其余原样。已归一化的行返回原引用（零拷贝，load 大表免全量重建）。
 */
export function normalizeTableRow(row: TableRow): TableRow {
  let changed = false;
  const values: Record<string, CellValue | undefined> = {};
  for (const [k, v] of Object.entries(row.values)) {
    if (Array.isArray(v)) {
      values[k] = normalizeImageValue(v);
      changed = true;
    } else {
      values[k] = v;
    }
  }
  return changed ? { ...row, values } : row;
}

/** CJK 全角字符（列宽/单元格宽度估算共用口径，双宽判定）。 */
const CJK_WIDTH_RE = /[\u3000-\u303f\u4e00-\u9fff\uff00-\uffef]/;

/** 字符显示宽度单位：CJK 全角 = 2，其余 = 1。 */
function charUnits(ch: string): number {
  return CJK_WIDTH_RE.test(ch) ? 2 : 1;
}

/**
 * 列宽默认值：按字段名称字数自适应（CJK 字符按双宽，每单位 7px + 边距 24px），
 * 钳制在 [MIN_COL_WIDTH, MAX_COL_WIDTH]；用户拖拽调整后存字段 `width` 覆盖。
 */
export function fieldDefaultWidth(name: string): number {
  let units = 0;
  for (const ch of name) {
    units += charUnits(ch);
  }
  return Math.max(MIN_COL_WIDTH, Math.min(MAX_COL_WIDTH, units * 7 + 24));
}

/** 单个单元格内容的估算宽度（与 fieldDefaultWidth 同口径：CJK 双宽 × 7px + 边距 24px）。 */
function cellContentWidth(v: CellValue | undefined): number {
  if (typeof v === "object" && v !== null) return 72; // image 缩略图 64px + 边距
  const text = v === undefined || v === null ? "" : String(v);
  let maxUnits = 0;
  for (const line of text.split("\n")) {
    let units = 0;
    for (const ch of line) {
      units += charUnits(ch);
    }
    maxUnits = Math.max(maxUnits, units);
  }
  return maxUnits * 7 + 24;
}

/**
 * 列宽自适应：按该列全部单元格内容估算最大宽度，且不小于字段名宽度（名称自适应为下限），
 * 钳制 [MIN_COL_WIDTH, MAX_COL_WIDTH]；空列结果为字段名宽度（与清除手动宽度等效）。
 * 返回最终宽度，由调用方 `setFieldWidth` 写死持久化（table-fixed 布局需显式宽度）。
 */
export function columnAutoWidth(field: TableField, rows: TableRow[]): number {
  const nameWidth = fieldDefaultWidth(field.name);
  const contentWidth = rows.reduce(
    (acc, r) => Math.max(acc, cellContentWidth(r.values[field.id])),
    0,
  );
  return Math.max(MIN_COL_WIDTH, Math.min(MAX_COL_WIDTH, Math.max(nameWidth, contentWidth)));
}

export function tableToSnapshotText(table: Pick<TableFile, "fields" | "rows">): string {
  const header = `字段：${table.fields.map((f) => f.name).join(" | ")}`;
  const lines: string[] = [header];
  const shown = table.rows.slice(0, MAX_TABLE_INJECT_ROWS);
  for (let i = 0; i < shown.length; i++) {
    const row = shown[i];
    const cells = table.fields.map((f) => {
      const v = row.values[f.id];
      if (typeof v === "object" && v !== null) {
        return v.images.length > 0 ? `[图 ${v.images.length} 张]` : "";
      }
      if (typeof v === "number") return String(v);
      return (v ?? "").toString().replace(/\s+/g, " ").trim();
    });
    lines.push(`行${i + 1}：${cells.join(" | ")}`);
  }
  if (table.rows.length > MAX_TABLE_INJECT_ROWS) {
    lines.push(`…（共 ${table.rows.length} 行，已截断）`);
  }
  return lines.join("\n");
}

/** 单元格是否为「非空值」（count 计算口径；image 按图片数非空，text/singleSelect 按非空串）。 */
function isNonEmptyValue(v: CellValue): boolean {
  if (typeof v === "number") return true;
  if (typeof v === "string") return v.trim() !== "";
  return v.images.length > 0;
}

/** 数值显示：整数原样，小数保留两位去尾 0。 */
function formatCalcNumber(n: number): string {
  return Number.isInteger(n) ? String(n) : String(Math.round(n * 100) / 100);
}

/**
 * 状态栏列自动计算：按字段 calcType 对全列取值统计，返回显示文本（未设 calcType 返回 null）。
 * sum/count 无数据时为 0；avg/max/min 无数据时为「—」（避免误导性平均值）。
 */
export function computeColumnCalc(field: TableField, rows: TableRow[]): string | null {
  const type = field.calcType;
  if (!type) return null;
  const values = rows
    .map((r) => r.values[field.id])
    .filter((v): v is CellValue => v !== undefined && v !== null);
  if (type === "count") return formatCalcNumber(values.filter(isNonEmptyValue).length);
  const nums = values.filter((v): v is number => typeof v === "number");
  switch (type) {
    case "sum":
      return formatCalcNumber(nums.reduce((acc, n) => acc + n, 0));
    case "avg":
      return nums.length > 0 ? formatCalcNumber(nums.reduce((a, b) => a + b, 0) / nums.length) : "—";
    case "max":
      return nums.length > 0 ? formatCalcNumber(Math.max(...nums)) : "—";
    case "min":
      return nums.length > 0 ? formatCalcNumber(Math.min(...nums)) : "—";
  }
  return null;
}

// ===== 增量补丁计算（保存写盘 / 协作实时广播共用）=====

/** id 序列是否逐位一致（长度不同即不等）。数组顺序是数组属性，引用 diff 看不见，必须显式比对。 */
export function sameIdSequence(a: { id: string }[], b: { id: string }[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    if (a[i].id !== b[i].id) return false;
  }
  return true;
}

/** 按 id 序重排（rank = id → 位置）：未出现在 rank 中的实体（对端并发新增）保持相对顺序置尾。
 * 与 Rust `reorder_by` 同语义；远端补丁 order 应用与导出快照排序共用。 */
export function reorderByRank<T extends { id: string }>(items: T[], rank: ReadonlyMap<string, number>): T[] {
  const known: T[] = [];
  const unknown: T[] = [];
  for (const x of items) {
    (rank.has(x.id) ? known : unknown).push(x);
  }
  known.sort((a, b) => (rank.get(a.id) ?? 0) - (rank.get(b.id) ?? 0));
  return [...known, ...unknown];
}

/**
 * 计算表格增量补丁：与「上次落盘快照」按引用 diff（store 不可变更新，未变实体引用相同），
 * 只产出变化/新增/删除的字段与行 + 顺序变化（id 序列不同 = 排序变更，须经 order 显式携带，
 * 否则拖拽排序/复制行/左右插列不落盘、协作者不可见）。空补丁返回 null（调用方跳过 IPC/广播）。
 */
export function computeTablePatch(opts: {
  tableId: string;
  fields: TableField[];
  rows: TableRow[];
  lastSaved: { fields: TableField[]; rows: TableRow[] };
}): TablePatch | null {
  const { tableId, fields, rows, lastSaved } = opts;
  // 预建 id → 实体索引代替循环内 .find()（大表 O(R²) → O(N)，每次按键/每次保存都会跑）
  const lastFieldsById = new Map(lastSaved.fields.map((f) => [f.id, f]));
  const lastRowsById = new Map(lastSaved.rows.map((r) => [r.id, r]));
  const upsertFieldIds = new Set<string>();
  for (const f of fields) {
    const ls = lastFieldsById.get(f.id);
    if (!ls || ls !== f) upsertFieldIds.add(f.id);
  }
  const currentFieldIds = new Set(fields.map((f) => f.id));
  const removedFieldIds = lastSaved.fields
    .filter((f) => !currentFieldIds.has(f.id))
    .map((f) => f.id);
  const upsertRowIds = new Set<string>();
  for (const r of rows) {
    const ls = lastRowsById.get(r.id);
    if (!ls || ls !== r) upsertRowIds.add(r.id);
  }
  const currentRowIds = new Set(rows.map((r) => r.id));
  const removedRowIds = lastSaved.rows
    .filter((r) => !currentRowIds.has(r.id))
    .map((r) => r.id);
  const fieldOrderChanged = !sameIdSequence(fields, lastSaved.fields);
  const rowOrderChanged = !sameIdSequence(rows, lastSaved.rows);
  const upsertFields = fields.filter((f) => upsertFieldIds.has(f.id));
  const upsertRows = rows.filter((r) => upsertRowIds.has(r.id));
  if (
    upsertFields.length === 0 &&
    upsertRows.length === 0 &&
    removedFieldIds.length === 0 &&
    removedRowIds.length === 0 &&
    !fieldOrderChanged &&
    !rowOrderChanged
  ) {
    return null;
  }
  return {
    id: tableId,
    upsertFields,
    removedFieldIds,
    upsertRows,
    removedRowIds,
    ...(fieldOrderChanged ? { fieldOrder: fields.map((f) => f.id) } : {}),
    ...(rowOrderChanged ? { rowOrder: rows.map((r) => r.id) } : {}),
  };
}

// ===== 磁盘/内存内容比对（watcher 回放判别）=====

/** 字符串数组相等；空数组与 undefined 视为等价（序列化丢空/缺省键不误判为外部修改）。 */
function stringArrayEqual(a: string[] | undefined, b: string[] | undefined): boolean {
  const na = a && a.length > 0 ? a : undefined;
  const nb = b && b.length > 0 ? b : undefined;
  if (na === undefined || nb === undefined) return na === nb;
  if (na.length !== nb.length) return false;
  for (let i = 0; i < na.length; i++) {
    if (na[i] !== nb[i]) return false;
  }
  return true;
}

/** 图片值视图：任意形态（旧 string[] / ImageCellValue）→ 归一化对象；非图片值 → null。 */
function imageViewOf(v: CellValue): ImageCellValue | null {
  if (Array.isArray(v)) return { images: v };
  if (typeof v === "object" && v !== null && Array.isArray((v as Partial<ImageCellValue>).images)) {
    return v as ImageCellValue;
  }
  return null;
}

/**
 * 单元格值相等（数组按项比较；空值 ≈ undefined，同 stringArrayEqual 口径）。
 * 图片值两侧各自归一化后比较——磁盘旧行（string[]）与内存新行（{images}）判定相等，
 * 防 watcher 自写回波被误判为外部修改触发重载。
 */
export function cellValueEqual(a: CellValue | undefined, b: CellValue | undefined): boolean {
  const ia = a === undefined ? null : imageViewOf(a);
  const ib = b === undefined ? null : imageViewOf(b);
  if (ia || ib) {
    // 空图无模式 ≈ undefined（延续「空 ≈ 缺省键」口径，序列化丢空不误判）
    const emptyA = ia === null || (ia.images.length === 0 && ia.display === undefined);
    const emptyB = ib === null || (ib.images.length === 0 && ib.display === undefined);
    if (emptyA || emptyB) return emptyA && emptyB;
    if (ia === null || ib === null) return false;
    if ((ia.display ?? undefined) !== (ib.display ?? undefined)) return false;
    return stringArrayEqual(ia.images, ib.images);
  }
  return a === b;
}

// ===== 单元格显示样式（随行 `styles[fieldId]`，与值正交）=====

/** 单元格样式 → 行内 CSS（粗体/斜体/下划线/删除线/字色/底色/字体/字号；缺省返回空对象）。
 *  TextCell/NumberCell/单选渲染与时间线文本卡片共用。 */
export function styleToCss(st: CellStyle | undefined): CSSProperties {
  if (!st) return {};
  const decoration = [st.u ? "underline" : "", st.s ? "line-through" : ""]
    .filter(Boolean)
    .join(" ");
  return {
    ...(st.b ? { fontWeight: 700 } : {}),
    ...(st.i ? { fontStyle: "italic" } : {}),
    ...(decoration ? { textDecorationLine: decoration } : {}),
    ...(st.color ? { color: st.color } : {}),
    ...(st.bg ? { backgroundColor: st.bg } : {}),
    ...(st.font ? { fontFamily: fontFamilyOf(st.font) } : {}),
    ...(st.size ? { fontSize: st.size } : {}),
  };
}

/** 单元格样式相等（缺省键 ≈ undefined：序列化丢空/缺省键不误判为差异，与值侧口径一致）。 */
export function styleEqual(a: CellStyle | undefined, b: CellStyle | undefined): boolean {
  const na = a ?? {};
  const nb = b ?? {};
  return (
    (na.b ?? undefined) === (nb.b ?? undefined) &&
    (na.i ?? undefined) === (nb.i ?? undefined) &&
    (na.u ?? undefined) === (nb.u ?? undefined) &&
    (na.s ?? undefined) === (nb.s ?? undefined) &&
    (na.color ?? undefined) === (nb.color ?? undefined) &&
    (na.bg ?? undefined) === (nb.bg ?? undefined) &&
    (na.font ?? undefined) === (nb.font ?? undefined) &&
    (na.size ?? undefined) === (nb.size ?? undefined)
  );
}

/** 行样式映射相等（缺 key ≈ undefined；空映射 ≈ undefined，序列化丢空不误判）。 */
function styleMapEqual(
  a: Record<string, CellStyle> | undefined,
  b: Record<string, CellStyle> | undefined,
): boolean {
  const keys = new Set([...Object.keys(a ?? {}), ...Object.keys(b ?? {})]);
  for (const k of keys) {
    if (!styleEqual(a?.[k], b?.[k])) return false;
  }
  return true;
}

/** 选区样式汇总（气泡格式工具栏三态来源）：逐属性判定——某属性在选区全体同值 → 该值
 *  （布尔 = all-on/off；颜色/字体/字号 undefined = 全体默认），不一致 → "mixed"。
 *  逐属性而非整样式：避免「某属性其实全体一致」仍被其它属性的差异拖成半选。选区为空返回全默认态。 */
export interface CellStyleSummary {
  bold: boolean | "mixed";
  italic: boolean | "mixed";
  underline: boolean | "mixed";
  strike: boolean | "mixed";
  color: string | undefined | "mixed";
  bg: string | undefined | "mixed";
  font: string | undefined | "mixed";
  size: number | undefined | "mixed";
}

export function selectionStyleSummary(
  sel: CollabSelection,
  fields: TableField[],
  rows: TableRow[],
): CellStyleSummary {
  const region = selectionRegion(sel, fields, rows);
  const firstStyle =
    region && region.rowStart <= region.rowEnd
      ? rows[region.rowStart]?.styles?.[fields[region.colStart].id]
      : undefined;
  /** 某属性在选区全部单元格取值是否一致（pick 取该属性；空选区视为一致）。 */
  const uniform = (pick: (st: CellStyle | undefined) => unknown): boolean => {
    if (!region) return true;
    const base = pick(firstStyle);
    for (let r = region.rowStart; r <= region.rowEnd; r++) {
      const rowStyles = rows[r]?.styles;
      for (let c = region.colStart; c <= region.colEnd; c++) {
        if (pick(rowStyles?.[fields[c].id]) !== base) return false;
      }
    }
    return true;
  };
  const boolState = (k: "b" | "i" | "u" | "s"): boolean | "mixed" =>
    uniform((st) => st?.[k]) ? firstStyle?.[k] === true : "mixed";
  const valueState = <T>(
    pick: (st: CellStyle | undefined) => T | undefined,
  ): T | undefined | "mixed" => (uniform((st) => pick(st)) ? pick(firstStyle) : "mixed");
  return {
    bold: boolState("b"),
    italic: boolState("i"),
    underline: boolState("u"),
    strike: boolState("s"),
    color: valueState((st) => st?.color),
    bg: valueState((st) => st?.bg),
    font: valueState((st) => st?.font),
    size: valueState((st) => st?.size),
  };
}

/** 行的「已定义值」映射（显式 undefined 键 ≈ 缺失——序列化时被丢弃，不能当作差异）。 */
function definedValues(r: TableRow): Map<string, CellValue> {
  const m = new Map<string, CellValue>();
  for (const [k, v] of Object.entries(r.values)) {
    if (v !== undefined) m.set(k, v);
  }
  return m;
}

function fieldEqual(a: TableField, b: TableField): boolean {
  return (
    a.id === b.id &&
    a.name === b.name &&
    a.type === b.type &&
    (a.width ?? undefined) === (b.width ?? undefined) &&
    (a.calcType ?? undefined) === (b.calcType ?? undefined) &&
    stringArrayEqual(a.options, b.options)
  );
}

function rowEqual(a: TableRow, b: TableRow): boolean {
  if (
    a.id !== b.id ||
    (a.height ?? undefined) !== (b.height ?? undefined) ||
    !styleMapEqual(a.styles, b.styles)
  ) {
    return false;
  }
  const am = definedValues(a);
  const bm = definedValues(b);
  if (am.size !== bm.size) return false;
  for (const [k, v] of am) {
    if (!bm.has(k) || !cellValueEqual(v, bm.get(k))) return false;
  }
  return true;
}

/**
 * 磁盘表格与内存运行时内容比对（watcher 回放判别）：id 序列逐位一致 + 逐实体结构相等
 * （undefined ≈ 缺省键、options/values 逐项深比，不依赖键序）。一致 = 自写回放或已广播
 * 应用的对端写入 → 跳过重载（保护撤销栈/选中态）；不一致 = 真实外部修改。
 */
export function tablesEqual(
  disk: { fields: TableField[]; rows: TableRow[] },
  memory: { fields: TableField[]; rows: TableRow[] },
): boolean {
  if (!sameIdSequence(disk.fields, memory.fields) || !sameIdSequence(disk.rows, memory.rows)) {
    return false;
  }
  return (
    disk.fields.every((f, i) => fieldEqual(f, memory.fields[i])) &&
    disk.rows.every((r, i) => rowEqual(r, memory.rows[i]))
  );
}

// ===== 历史版本摘要 / diff（表格历史面板可读化）=====

/** 单元格值展示文本（图片值 = 图片数，历史旧快照 string[] 形态同样识别；空 → 空；超长截断）。 */
function formatCellValue(v: CellValue | undefined): string {
  if (v === undefined) return "空";
  if (typeof v === "string") return v.length > 24 ? `${v.slice(0, 24)}…` : v;
  if (typeof v === "number") return String(v);
  const n = normalizeImageValue(v).images.length;
  return n > 0 ? `图片 ×${n}` : "空";
}

/** 行展示名：首个非空文本字段值；无 → 空串（调用方兜底「第 N 行」）。 */
function rowLabelOf(row: TableRow, fields: TableField[]): string {
  for (const f of fields) {
    const v = row.values[f.id];
    if (typeof v === "string" && v.trim() !== "") {
      const s = v.trim();
      return s.length > 16 ? `${s.slice(0, 16)}…` : s;
    }
  }
  return "";
}

/** 解析历史快照为 TableFile；空串/损坏 → null。 */
function parseTableSnapshot(raw: string): TableFile | null {
  if (!raw) return null;
  try {
    const t = JSON.parse(raw) as TableFile;
    return t && Array.isArray(t.fields) && Array.isArray(t.rows) ? t : null;
  } catch {
    return null;
  }
}

/** 表格历史版本 diff（相对上一版本；首版 prev 为空 → 全部字段/行计为新增）。 */
export interface TableVersionDiff {
  addedFields: { name: string }[];
  removedFields: { name: string }[];
  renamedFields: { from: string; to: string }[];
  addedRows: string[];
  removedRows: string[];
  cellChanges: { rowIndex: number; fieldName: string; from: string; to: string }[];
  fieldOrderChanged: boolean;
  rowOrderChanged: boolean;
}

/** 对比两个历史快照：字段增删/改名 + 行增删 + 单元格修改 + 顺序变化（纯函数，历史面板懒计算）。 */
export function diffTableVersions(prevRaw: string, nextRaw: string): TableVersionDiff {
  const prev = parseTableSnapshot(prevRaw);
  const next = parseTableSnapshot(nextRaw);
  if (!next) {
    return {
      addedFields: [],
      removedFields: [],
      renamedFields: [],
      addedRows: [],
      removedRows: [],
      cellChanges: [],
      fieldOrderChanged: false,
      rowOrderChanged: false,
    };
  }
  const prevFields = prev?.fields ?? [];
  const prevRows = prev?.rows ?? [];
  const prevFieldsById = new Map(prevFields.map((f) => [f.id, f]));
  const prevRowsById = new Map(prevRows.map((r) => [r.id, r]));
  const nextFieldIds = new Set(next.fields.map((f) => f.id));
  const nextRowIds = new Set(next.rows.map((r) => r.id));
  // 行号按 next 版本的实际位置（1-based）；预建 index map 免每行 indexOf（大表 O(R²) → O(N)）
  const nextRowIndex = new Map(next.rows.map((r, i) => [r.id, i]));
  const prevRowIndex = new Map(prevRows.map((r, i) => [r.id, i]));

  const addedFields = next.fields.filter((f) => !prevFieldsById.has(f.id)).map((f) => ({ name: f.name }));
  const removedFields = prevFields.filter((f) => !nextFieldIds.has(f.id)).map((f) => ({ name: f.name }));
  const renamedFields = next.fields
    .filter((f) => {
      const p = prevFieldsById.get(f.id);
      return p && p.name !== f.name;
    })
    .map((f) => ({ from: prevFieldsById.get(f.id)!.name, to: f.name }));

  const addedRows = next.rows
    .filter((r) => !prevRowsById.has(r.id))
    .map((r) => rowLabelOf(r, next.fields) || `第 ${(nextRowIndex.get(r.id) ?? 0) + 1} 行`);
  const removedRows = prevRows
    .filter((r) => !nextRowIds.has(r.id))
    .map((r) => rowLabelOf(r, prevFields) || `第 ${(prevRowIndex.get(r.id) ?? 0) + 1} 行`);

  const cellChanges: TableVersionDiff["cellChanges"] = [];
  for (const r of next.rows) {
    const p = prevRowsById.get(r.id);
    if (!p) continue;
    const rowIndex = (nextRowIndex.get(r.id) ?? 0) + 1;
    for (const f of next.fields) {
      if (!cellValueEqual(p.values[f.id], r.values[f.id])) {
        cellChanges.push({
          rowIndex,
          fieldName: f.name,
          from: formatCellValue(p.values[f.id]),
          to: formatCellValue(r.values[f.id]),
        });
      }
    }
  }

  return {
    addedFields,
    removedFields,
    renamedFields,
    addedRows,
    removedRows,
    cellChanges,
    fieldOrderChanged: !sameIdSequence(next.fields, prevFields),
    rowOrderChanged: !sameIdSequence(next.rows, prevRows),
  };
}

/**
 * 历史版本人话摘要（记录时生成，列表展示）：对比上一版本快照输出「新增/删除 N 行 · 修改 N 个
 * 单元格 · 字段增删/改名 · 调整顺序」；无上一版本 = 新建统计。空表/损坏快照 → 空串。
 */
export function summarizeTableSnapshot(prevRaw: string, nextRaw: string): string {
  const next = parseTableSnapshot(nextRaw);
  if (!next) return "";
  if (!parseTableSnapshot(prevRaw)) return `新建 · ${next.fields.length} 字段 · ${next.rows.length} 行`;
  const diff = diffTableVersions(prevRaw, nextRaw);
  const parts: string[] = [];
  if (diff.addedRows.length) parts.push(`新增 ${diff.addedRows.length} 行`);
  if (diff.removedRows.length) parts.push(`删除 ${diff.removedRows.length} 行`);
  if (diff.cellChanges.length) parts.push(`修改 ${diff.cellChanges.length} 个单元格`);
  if (diff.addedFields.length) parts.push(`新增字段 ${diff.addedFields.map((f) => `「${f.name}」`).join("、")}`);
  if (diff.removedFields.length) parts.push(`删除字段 ${diff.removedFields.map((f) => `「${f.name}」`).join("、")}`);
  if (diff.renamedFields.length) parts.push(`字段改名 ${diff.renamedFields.map((f) => `「${f.from}」→「${f.to}」`).join("、")}`);
  if (diff.fieldOrderChanged) parts.push("调整列顺序");
  if (diff.rowOrderChanged) parts.push("调整行顺序");
  return parts.length ? parts.join(" · ") : "未改动";
}

// ===== 选中区域归约 / 复制粘贴（剪贴板 TSV）=====

/** 区域（行/列下标区间）。 */
export interface TableRegion {
  rowStart: number;
  rowEnd: number;
  colStart: number;
  colEnd: number;
}

/**
 * 选中范围 → 矩形区域（行/列下标区间）：单格 = 单点；拖拽框选 = 两端点行/列 min/max；
 * 整行 = 该行全列；整列 = 全行该列；整表 = 全表；null / 画布 node 选中 = null（无表格区域）。
 * 复制/粘贴/清空/协作高亮/右键命中判定统一复用。入参兼容本端选中与远端 presence 选中。
 */
export function selectionRegion(
  sel: CollabSelection,
  fields: TableField[],
  rows: TableRow[],
): TableRegion | null {
  if (!sel) return null;
  const rowIndex = (id: string) => rows.findIndex((r) => r.id === id);
  const colIndex = (id: string) => fields.findIndex((f) => f.id === id);
  switch (sel.kind) {
    case "cell": {
      const r = rowIndex(sel.rowId);
      const c = colIndex(sel.fieldId);
      if (r < 0 || c < 0) return null;
      return { rowStart: r, rowEnd: r, colStart: c, colEnd: c };
    }
    case "range": {
      const r1 = rowIndex(sel.anchorRowId);
      const r2 = rowIndex(sel.rowId);
      const c1 = colIndex(sel.anchorFieldId);
      const c2 = colIndex(sel.fieldId);
      if (r1 < 0 || r2 < 0 || c1 < 0 || c2 < 0) return null;
      return {
        rowStart: Math.min(r1, r2),
        rowEnd: Math.max(r1, r2),
        colStart: Math.min(c1, c2),
        colEnd: Math.max(c1, c2),
      };
    }
    case "row": {
      const r = rowIndex(sel.rowId);
      if (r < 0) return null;
      return { rowStart: r, rowEnd: r, colStart: 0, colEnd: fields.length - 1 };
    }
    case "column": {
      const c = colIndex(sel.fieldId);
      if (c < 0) return null;
      return { rowStart: 0, rowEnd: rows.length - 1, colStart: c, colEnd: c };
    }
    case "all":
      return { rowStart: 0, rowEnd: rows.length - 1, colStart: 0, colEnd: fields.length - 1 };
    default:
      return null; // node / 未知
  }
}

// ===== 插件表格数据快照（插件平台 facade subscribeTableData 推送）=====

/** 构建插件表格数据快照：协作远端选中行 → 用户色归约（cell/range/row 区域染行、column/all 忽略、
 *  首个匹配 peer 优先；空表越界兜底）。rows/fields 直传 store 不可变引用（选中变化不重建，插件可 memo 隔离）。 */
export function buildPluginTableSnapshot(
  tableFile: string | null,
  fields: TableField[],
  rows: TableRow[],
  selectedRowId: string | null,
  peers: CollabPeer[],
): PluginTableSnapshot {
  const peerColorByRowId: Record<string, string> = {};
  if (tableFile) {
    for (const p of peers) {
      const sel = p.presence?.selection;
      if (p.presence?.file !== tableFile || !sel || sel.kind === "all" || sel.kind === "column") {
        continue;
      }
      const region = selectionRegion(sel, fields, rows);
      if (!region) continue;
      // 下标恒合法：region 产自 selectionRegion，退化区间（空表）循环不执行
      for (let r = region.rowStart; r <= region.rowEnd; r++) {
        if (!peerColorByRowId[rows[r].id]) peerColorByRowId[rows[r].id] = p.color;
      }
    }
  }
  return { tableFile, fields, rows, selectedRowId, peerColorByRowId };
}

/** 单元格值 → 剪贴板文本（image = 空；number/duration = 数值字符串；text/singleSelect = 原串）。 */
export function cellToClipboardText(v: CellValue | undefined): string {
  if (typeof v === "string") return v;
  if (typeof v === "number") return String(v);
  return ""; // undefined / image
}

/**
 * 区域 → TSV 文本（行 = \n、列 = \t）：单格 = 原值（多行文本原样，1×1 无损）；
 * 多格 = 值内嵌 \t/\n 压成空格保 TSV 结构完整。空区域 → 空串。
 */
export function buildRegionTsv(
  fields: TableField[],
  rows: TableRow[],
  region: TableRegion,
): string {
  const multi = region.rowStart !== region.rowEnd || region.colStart !== region.colEnd;
  // 下标恒合法：region 一律产自 selectionRegion（区间内保证有效，退化区间循环不执行）
  const cellText = (r: number, c: number): string => {
    const t = cellToClipboardText(rows[r].values[fields[c].id]);
    return multi ? t.replace(/[\t\n]/g, " ") : t;
  };
  const lines: string[] = [];
  for (let r = region.rowStart; r <= region.rowEnd; r++) {
    const cells: string[] = [];
    for (let c = region.colStart; c <= region.colEnd; c++) cells.push(cellText(r, c));
    lines.push(cells.join("\t"));
  }
  return lines.join("\n");
}

/**
 * 剪贴板 TSV 文本 → 值网格（string[][]）：\r\n 归一、制表符分列，末尾换行的空尾行剔除。
 * 纯 TSV 不处理引号/内嵌换行（外部 Excel 带内嵌换行的格会拆行，已知限制）。
 */
export function parseTsv(text: string): string[][] {
  const lines = text.replace(/\r\n?/g, "\n").split("\n");
  if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();
  return lines.map((line) => line.split("\t"));
}

/** 粘贴值强转（text/number/duration；singleSelect/image 由调用方分派）：text = 原串（空 → undefined）；
 *  number/duration = 数值化（非有限数/空 → undefined——非有限数含 NaN 与 Infinity，落盘会变 null 丢值）。 */
function coercePasteValue(raw: string, field: TableField): CellValue | undefined {
  if (field.type === "text") return raw === "" ? undefined : raw;
  const t = raw.trim();
  if (t === "") return undefined;
  const n = Number(t);
  return Number.isFinite(n) ? n : undefined;
}

/**
 * 把粘贴网格应用到锚点（anchorRow, anchorCol）右下展开：越界自动补行（空行）/补列
 * （text 字段「字段N」）；按目标字段类型强转（text 原样/空清空、number/duration 数值化、
 * singleSelect 命中选项才写否则跳过、image 跳过）；空格清空目标格。
 * 用 `cellValueEqual` 判定实际变化：无变化且无补行/补列返回原引用（调用方不置脏不入栈）。
 */
export function applyPasteGrid(
  fields: TableField[],
  rows: TableRow[],
  anchorRow: number,
  anchorCol: number,
  grid: string[][],
): { fields: TableField[]; rows: TableRow[] } {
  const needRows = anchorRow + grid.length;
  const needCols = anchorCol + grid.reduce((max, line) => Math.max(max, line.length), 0);
  let newFields = fields;
  if (needCols > fields.length) {
    newFields = [...fields];
    for (let i = fields.length; i < needCols; i++) {
      newFields.push({ id: crypto.randomUUID(), name: `字段${i + 1}`, type: "text" });
    }
  }
  let newRows = rows;
  if (needRows > rows.length) {
    newRows = [...rows];
    for (let i = rows.length; i < needRows; i++) newRows.push({ id: crypto.randomUUID(), values: {} });
  }
  let anyChanged = false;
  const outRows = newRows.map((row, r) => {
    const g = r - anchorRow;
    if (g < 0 || g >= grid.length) return row;
    const line = grid[g];
    if (line.length === 0) return row;
    let rowChanged = false;
    const values = { ...row.values };
    for (let c = 0; c < line.length; c++) {
      // newFields 已按 needCols 补足，anchorCol + c 恒有字段
      const field = newFields[anchorCol + c];
      const raw = line[c];
      let next: CellValue | undefined;
      if (field.type === "singleSelect") {
        if (raw === "") next = undefined;
        else if (field.options?.includes(raw)) next = raw;
        else continue; // 非选项值：不覆盖不清空
      } else if (field.type === "image") {
        continue; // 图片格不支持贴文本，跳过
      } else {
        next = coercePasteValue(raw, field);
      }
      if (!cellValueEqual(values[field.id], next)) {
        values[field.id] = next;
        rowChanged = true;
      }
    }
    if (!rowChanged) return row;
    anyChanged = true;
    return { ...row, values };
  });
  // 无实际写入（全跳过/全空）：丢弃补出的空行/空列，返回原引用（调用方不置脏不入栈）
  if (!anyChanged) return { fields, rows };
  return { fields: newFields, rows: outRows };
}
