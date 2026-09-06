/**
 * 多维表格（.atb）运行时状态：当前打开的表格内容 + 防抖保存 + 外部修改冲突。
 *
 * 保存管线对齐 canvasStore：debounce 500ms 原子写 + `markSelfSave` 抑制 watcher 回放 +
 * 乐观锁 `baseUpdatedAt`（磁盘版本更新则拒绝覆盖，冲突提示条由页面层展示）。
 * 窗口槽/恢复/重命名联动由页面层（ProjectWorkspacePage）编排，本 store 只管内容与持久化。
 * title 变更走 `vaultStore.renameTable`（Rust 改文件名 + 同步画布引用），本 store 不直接改 title。
 */
import { create } from "zustand";
import { CALC_TYPES_BY_FIELD, TABLE_SCHEMA } from "@/constants/table";
import {
  cleanupTableAttachments,
  exportTableXlsx,
  importTableImage,
  patchTableVault,
  readTableVault,
  saveImageToDownloads,
  writeTableVault,
} from "@/services/table";
import { copyImageToClipboard as copyImageSvc, readClipboardText, writeClipboardText } from "@/services/clipboard";
import { pickFile, saveFile } from "@/services/dialog";
import {
  loadHistory as loadTableHistory,
  recordHistoryVersion,
  versionContentAt,
  type HistoryVersion,
} from "@/services/history";
import { markSelfSave } from "@/utils/selfSave";
import { clearTableImageCache } from "@/services/tableImageCache";
import { useCollabStore } from "@/stores/collabStore";
import { createPersistController } from "@/utils/persist";
import { createUndoManager } from "@/utils/undoStack";
import {
  applyPasteGrid,
  buildRegionTsv,
  cellValueEqual,
  computeTablePatch,
  normalizeTableRow,
  parseTsv,
  reorderByRank,
  sameIdSequence,
  selectionRegion,
  styleEqual,
  summarizeTableSnapshot,
  type TableRegion,
} from "@/utils/table";
import type {
  CalcType,
  CellStyle,
  CellValue,
  FieldType,
  TableField,
  TableFile,
  TablePatch,
  TableRow,
  TableSelection,
} from "@/types";

/** 编辑器视图（内存态不持久化）："table" = 内置表格网格；其余 = 插件表格视图 kind（registerTableView）。
 * 判别统一用 `view !== "table"`（插件 kind 为任意字符串，无法静态收窄）。 */
type TableView = string;

interface TableStoreState {
  /** 当前打开的 .atb 相对仓库根路径（null = 未打开）。 */
  tableFile: string | null;
  id: string;
  title: string;
  fields: TableField[];
  rows: TableRow[];
  /** 乐观并发基准（加载时的磁盘 updatedAt；保存成功后同步为写入版本）。 */
  baseUpdatedAt: number;
  dirty: boolean;
  saving: boolean;
  /** 外部修改 + 本地有未保存改动：冲突提示条由用户决定（重载丢弃本地）。 */
  conflictPending: boolean;
  error: string | null;
  /** 选中行（表格/时间线视图联动；null = 无选中）。 */
  selectedRowId: string | null;
  /** 当前选中范围（单元格/框选区域/行/列/整表互斥；表格视图高亮用，时间线只读 selectedRowId）。 */
  selection: TableSelection;
  view: TableView;
  /** 最近一次撤销回退的编辑会话单元格（undo 弹掉会话入口时置位；TableCell 按布尔 selector 订阅回退草稿，无关撤销为 null）。 */
  undoResetCell: { rowId: string; fieldId: string } | null;

  /** 打开表格：读盘填充 + 同步乐观锁基准。失败时 error 提示（文件已删/损坏降级不崩溃）。 */
  load: (file: string) => Promise<void>;
  /** 外部修改后重载磁盘最新内容（无本地改动时 watcher 调用）。 */
  reloadFromDisk: () => Promise<void>;
  /** 冲突处理：keepLocal=true 保留本地并保存（绕过乐观锁覆盖磁盘）；false 重载丢弃本地。 */
  resolveConflict: (keepLocal: boolean) => Promise<void>;
  /** 清空运行时状态（切仓库/关窗口/删除文件时调用：取消保存定时器，防残留 timer 重写已删文件）。 */
  clear: () => void;
  /** 关闭错误提示。 */
  clearError: () => void;
  /** 立即落盘（卸载/切仓库前 flush；无改动不写）。force = 保留本地（绕过乐观锁强制覆盖，冲突条「保留本地并保存」用）。 */
  flush: (force?: boolean) => Promise<boolean>;

  updateCell: (rowId: string, fieldId: string, value: CellValue | undefined) => void;
  /** 对当前选中区域应用单元格样式（patch = 增量，undefined 键 = 清除该项；null = 清除全部格式）。
   *  布尔切换语义由调用方按汇总态决定传 true/undefined（mixed 一律传 true 拉齐）。一步撤销。 */
  applyCellStyle: (patch: Partial<CellStyle> | null) => void;
  /** 图片单元格追加图片（系统文件选择器 → 附件路径引用）。 */
  addImageToCell: (rowId: string, fieldId: string) => Promise<void>;
  /** 图片单元格移除指定下标图片。 */
  removeImageAt: (rowId: string, fieldId: string, index: number) => void;
  /** 图片单元格切换展示模式（单图轮播 ⇄ 九宫格，按单元格记忆；一次切换 = 一步撤销）。 */
  toggleImageDisplay: (rowId: string, fieldId: string) => void;
  /** 图片单元格重排（order[新位置] = 原下标；队列长按拖动/九宫格长按拖拽共用；一次重排 = 一步撤销）。 */
  reorderImages: (rowId: string, fieldId: string, order: number[]) => void;
  /** 放大预览右键「复制图片」：dataURL → 系统剪贴板（GIF 取首帧）。失败 error 提示，返回是否成功。 */
  copyImageToClipboard: (dataUrl: string) => Promise<boolean>;
  /** 放大预览右键「下载图片」：保存到系统 Downloads 文件夹（重名自动加序号）。失败 error 提示，返回是否成功。 */
  downloadImageToDownloads: (fileName: string, dataUrl: string) => Promise<boolean>;
  /** 导出 xlsx：系统保存对话框选目标路径 → 导出当前内容。成功返回 true。 */
  exportXlsx: () => Promise<boolean>;
  addField: (name: string, type: FieldType) => void;
  insertField: (index: number, name: string, type: FieldType) => void;
  renameField: (fieldId: string, name: string) => void;
  retypeField: (fieldId: string, type: FieldType) => void;
  setFieldOptions: (fieldId: string, options: string[]) => void;
  /** 拖拽调整列宽（px，随 .atb 持久化；缺省 = 按字段名自适应）。 */
  setFieldWidth: (fieldId: string, width: number) => void;
  /** 拖拽调整行高（px，随 .atb 持久化；缺省 = 内容自然撑开）。 */
  setRowHeight: (rowId: string, height: number) => void;
  /** 行高自适应：清除手动行高，恢复内容自然撑开。 */
  clearRowHeight: (rowId: string) => void;
  /** 整表行高自适应：全部行清除手动行高（单次撤销单元；无手动行高 no-op）。 */
  clearAllRowHeights: () => void;
  /** 设置状态栏列自动计算类型（缺省 = 无计算）。 */
  setCalcType: (fieldId: string, calcType: CalcType | undefined) => void;
  removeField: (fieldId: string) => void;
  addRow: () => void;
  duplicateRow: (rowId: string) => void;
  removeRow: (rowId: string) => void;
  /** 拖拽插入排序：把 dragId 行移动到 targetIndex 位置（targetIndex = 拖放时原始数组的插入位）。 */
  moveRow: (dragId: string, targetIndex: number) => void;
  /** 选中单元格（同时联动 selectedRowId 供时间线行高亮）。 */
  selectCell: (rowId: string, fieldId: string) => void;
  /** 选中整行（时间线/行首点击共用）。 */
  selectRow: (rowId: string | null) => void;
  /** 选中整列（表头点击）。 */
  selectField: (fieldId: string) => void;
  /** 选中整个表格（左上角点击）。 */
  selectAll: () => void;
  /** 拖拽框选多格（锚点 = 按下起点，终点 = 当前指针格；矩形范围由二者行/列推导）。 */
  selectRange: (anchorRowId: string, anchorFieldId: string, rowId: string, fieldId: string) => void;
  /** 复制当前选中区域为 TSV 到系统剪贴板（无选区/空区域 no-op；写失败置面板错误提示）。 */
  copySelection: () => void;
  /** 剪切当前选中区域：复制 TSV 到剪贴板，成功后才清空区域（一步撤销；图片格跳过；写失败不销毁数据）。 */
  cutSelection: () => Promise<void>;
  /** 从系统剪贴板读 TSV 并粘贴到当前选中锚点（空/无选区 no-op；一步撤销）。 */
  pasteFromClipboard: () => Promise<void>;
  /** 清空当前选中区域全部单元格（框选 Backspace/Delete 用；一步撤销，图片格跳过）。 */
  clearSelectionCells: () => void;
  setView: (view: TableView) => void;
  /** 保存当前快照到 undo 栈（清空 redo 栈）。结构变更方法内置；拖拽/编辑会话入口由调用方触发。 */
  pushUndo: () => void;
  undo: () => void;
  redo: () => void;
  /** 编辑会话开始（双击/覆盖进入时调用）：入栈 + 记录会话（提交确认/中止丢弃/撤销回退判定）。 */
  beginCellEdit: (rowId: string, fieldId: string) => void;
  /** 编辑会话提交（失焦有改动时调用）：撤销单元保留。 */
  commitCellEdit: () => void;
  /** 编辑会话中止（Esc/未改动退出时调用）：丢弃空撤销单元（恢复被清的 redo）。 */
  abortCellEdit: () => void;
  /** 协作实时广播钩子注入（collabStore init 时设置；null = 协作未启用，不广播）。 */
  setCollabBroadcast: (fn: ((file: string, patch: TablePatch) => void) | null) => void;
  /** 应用远端协作者的增量补丁（relay 收到 table-patch 时调用）：纯内存合并（与 Rust 合并同语义），
   * 不置脏/不入撤销栈/不触发保存——落盘由发送方负责，本端下次保存经 diff 幂等收敛。 */
  applyRemotePatch: (file: string, patch: TablePatch) => void;
  /** 读取表格历史版本列表（缺失/损坏 → 空数组，尽力而为）。 */
  tableHistoryLoad: (file: string) => Promise<HistoryVersion[]>;
  /** 回滚表格到指定版本：写回快照 + 重载内存 + 记 restore 版本；成功返回快照内容，失败返回 null。 */
  tableHistoryRollback: (file: string, seq: number) => Promise<string | null>;
}

/** 撤销快照：fields + rows（含 id/calcType/width/height/options/values，id 保真——撤销后选中引用不悬空）。 */
interface TableSnapshot {
  fields: TableField[];
  rows: TableRow[];
}

/** 撤销/重做栈（深 50，通用快照栈工具；与画布共用语义）。 */
const undoMgr = createUndoManager<TableSnapshot>({
  snapshot: () => {
    // 不可变更新保证引用即快照（零拷贝）：store 每次 set 生成新 fields/rows，
    // 历史引用不被污染；深拷贝会拖慢每次入栈/撤销（含 image dataURL 大字符串）
    const { fields, rows } = useTableStore.getState();
    return { fields, rows };
  },
  apply: (entry) =>
    useTableStore.setState((s) => {
      // 被撤销的选中目标可能已不存在：清理失效选择，防高亮残留失效引用；
      // 覆盖编辑意图同样清理（防 redo 恢复行后误进入编辑）
      const fieldIds = new Set(entry.fields.map((f) => f.id));
      const rowIds = new Set(entry.rows.map((r) => r.id));
      const sel = s.selection;
      const stale = selectionStaleForSets(sel, fieldIds, rowIds);
      return {
        fields: entry.fields,
        rows: entry.rows,
        selection: stale ? null : sel,
        selectedRowId:
          s.selectedRowId && !rowIds.has(s.selectedRowId) ? null : s.selectedRowId,
      };
    }),
});

/** 进行中的编辑会话（双击/覆盖进入；null = 无会话）。 */
let sessionCell: { rowId: string; fieldId: string } | null = null;
/** 会话入栈时的 undo 栈深：中止时栈深未变（期间无其他操作插入）才丢弃该会话的空撤销单元。 */
let sessionUndoDepth = -1;

/** 编辑会话开始：入栈（快照此刻旧值）+ 记录会话标识（提交确认/中止丢弃/撤销回退草稿判定用）。 */
function beginEditSession(rowId: string, fieldId: string): void {
  undoMgr.push();
  sessionUndoDepth = undoMgr.size;
  sessionCell = { rowId, fieldId };
}

/** 编辑会话提交：撤销单元保留生效。 */
function commitEditSession(): void {
  sessionUndoDepth = -1;
  sessionCell = null;
}

/** 编辑会话中止（Esc/未改动退出）：期间无其他操作插入则丢弃空撤销单元（恢复被清的 redo）。 */
function abortEditSession(): void {
  if (sessionUndoDepth >= 0 && undoMgr.size === sessionUndoDepth) undoMgr.dropTop();
  sessionUndoDepth = -1;
  sessionCell = null;
}

/** 选区是否引用指定字段（失效清理共用：单格/整列看 fieldId；框选看两端任一 fieldId）。 */
function selUsesField(sel: TableSelection, fieldId: string): boolean {
  if (!sel) return false;
  if (sel.kind === "cell" || sel.kind === "column") return sel.fieldId === fieldId;
  if (sel.kind === "range") return sel.fieldId === fieldId || sel.anchorFieldId === fieldId;
  return false;
}

/** 选区是否引用指定行（同 selUsesField，行侧）。 */
function selUsesRow(sel: TableSelection, rowId: string): boolean {
  if (!sel) return false;
  if (sel.kind === "cell" || sel.kind === "row") return sel.rowId === rowId;
  if (sel.kind === "range") return sel.rowId === rowId || sel.anchorRowId === rowId;
  return false;
}

/** 选区是否因目标集合缺失而失效（撤销/合并应用到快照后，两端任一 id 消失即清空）。 */
function selectionStaleForSets(
  sel: TableSelection,
  fieldIds: Set<string>,
  rowIds: Set<string>,
): boolean {
  if (!sel) return false;
  switch (sel.kind) {
    case "cell":
      return !fieldIds.has(sel.fieldId) || !rowIds.has(sel.rowId);
    case "column":
      return !fieldIds.has(sel.fieldId);
    case "row":
      return !rowIds.has(sel.rowId);
    case "range":
      return (
        !fieldIds.has(sel.fieldId) ||
        !fieldIds.has(sel.anchorFieldId) ||
        !rowIds.has(sel.rowId) ||
        !rowIds.has(sel.anchorRowId)
      );
    default:
      return false; // all / null
  }
}

/**
 * 合并产物按本地 id 序重排（本地相对「合并前基线」顺序变化时调用——排序 LWW 胜出；
 * order 未出现的实体（对端并发新增）保持相对顺序置尾）。本地未改顺序则原样返回（跟随磁盘排序）。
 */
function applyLocalOrder<T extends { id: string }>(merged: T[], local: T[], baseline: T[]): T[] {
  if (sameIdSequence(local, baseline)) return merged;
  return reorderByRank(merged, new Map(local.map((x, i) => [x.id, i] as const)));
}

/**
 * 乐观锁冲突（磁盘版本新于基准）自动合并重试：重读磁盘 → 以磁盘为基底应用本地增量
 * （同实体 = 本地后写者胜 LWW；本地删除生效；磁盘删除而本地未改 = 跟随磁盘不复活），
 * 合并结果写回内存并以磁盘为保存基线重发补丁——他人改动保留、本地改动落上，不再弹冲突条。
 * 未变实体直接引用磁盘对象，重发补丁引用 diff 天然只含真实增量。
 * 顺序变化（排序/插列）按本地 id 序重排后随补丁重发（同实体 LWW 语义）。
 * 重读失败/身份不匹配/再次并发冲突 → 抛错回落冲突提示（用户手动决策）。
 */
async function retryMergePersist(
  file: string,
  finish: (updatedAt: number | null, newFile?: string) => void,
): Promise<void> {
  const disk = await readTableVault(file);
  // 读盘期间可能已继续编辑/切换表格：以**最新**本地状态为合并基准（用旧引用会覆盖新编辑），
  // 且已不在原表则放弃合并（内存不得污染新表状态）
  const s = useTableStore.getState();
  if (s.tableFile !== file) return;
  if (disk.id !== s.id) throw new Error("磁盘表格身份不匹配，已中止合并保存");
  const localFields = new Map(s.fields.map((f) => [f.id, f]));
  const localRows = new Map(s.rows.map((r) => [r.id, r]));
  const changedFieldIds = new Set(
    s.fields
      .filter((f) => {
        const ls = lastSavedFields.find((x) => x.id === f.id);
        return !ls || ls !== f;
      })
      .map((f) => f.id),
  );
  const changedRowIds = new Set(
    s.rows
      .filter((r) => {
        const ls = lastSavedRows.find((x) => x.id === r.id);
        return !ls || ls !== r;
      })
      .map((r) => r.id),
  );
  const removedFieldIds = new Set(
    lastSavedFields.filter((f) => !localFields.has(f.id)).map((f) => f.id),
  );
  const removedRowIds = new Set(
    lastSavedRows.filter((r) => !localRows.has(r.id)).map((r) => r.id),
  );
  const mergedFields = disk.fields.filter((f) => !removedFieldIds.has(f.id));
  for (let i = 0; i < mergedFields.length; i++) {
    const f = mergedFields[i];
    const local = localFields.get(f.id);
    if (local && changedFieldIds.has(f.id)) mergedFields[i] = local;
  }
  const mergedRows = disk.rows.filter((r) => !removedRowIds.has(r.id));
  for (let i = 0; i < mergedRows.length; i++) {
    const r = mergedRows[i];
    const local = localRows.get(r.id);
    if (local && changedRowIds.has(r.id)) mergedRows[i] = local;
  }
  // 纯本地新增（磁盘无 + 基线无）补进末尾；磁盘删除的实体本地未删（含已修改）也跟随磁盘——
  // 删除冲突优先（防幽灵行复活），本地对已删实体的编辑视为随删除放弃
  for (const f of s.fields) {
    if (!disk.fields.some((x) => x.id === f.id) && !lastSavedFields.some((x) => x.id === f.id)) {
      mergedFields.push(f);
    }
  }
  for (const r of s.rows) {
    if (!disk.rows.some((x) => x.id === r.id) && !lastSavedRows.some((x) => x.id === r.id)) {
      mergedRows.push(r);
    }
  }
  // 顺序变化（排序/插列）：本地相对合并前基线改过顺序 → 合并产物按本地 id 序重排，
  // 重发补丁携带 order（本地排序 LWW 胜出，对端新增置尾）；未改顺序则跟随磁盘排序
  const mergedFieldsFinal = applyLocalOrder(mergedFields, s.fields, lastSavedFields);
  const mergedRowsFinal = applyLocalOrder(mergedRows, s.rows, lastSavedRows);
  // 写回内存前最后守卫：计算期间切表则放弃（补丁不重发，冲突留给下次保存）
  if (useTableStore.getState().tableFile !== file) return;
  useTableStore.setState({ fields: mergedFieldsFinal, rows: mergedRowsFinal });
  lastSavedFields = disk.fields;
  lastSavedRows = disk.rows;
  // 合并产物（远端磁盘内容 + 本端已广播增量）对房间已知：推进广播基线防其被重发全房
  syncBroadcastBaseline();
  const result = await patchTableVault({
    file,
    tableId: disk.id,
    fields: mergedFieldsFinal,
    rows: mergedRowsFinal,
    lastSaved: { fields: disk.fields, rows: disk.rows },
    baseUpdatedAt: disk.updatedAt,
    force: false,
  });
  finish(result ? result.updatedAt : null, result?.file);
}

/**
 * 从当前内存内容构建 `.atb` 全量快照（历史存档/磁盘全量重建/xlsx 导出三处收敛）：
 * schema/id/title/fields/rows 取当前 state，createdAt: 0 = 快照展示占位（磁盘真实 createdAt
 * 由 write_table_vault 落盘保留）；updatedAt 缺省 = 当前时间，extra 覆盖个别字段。
 */
function buildTableSnapshot(extra?: Partial<TableFile>): TableFile {
  const s = useTableStore.getState();
  return {
    schema: TABLE_SCHEMA,
    id: s.id,
    title: s.title,
    fields: s.fields,
    rows: s.rows,
    createdAt: 0,
    updatedAt: Date.now(),
    ...extra,
  };
}

/**
 * 记录表格历史版本（保存成功后的存档点）：以内存当前内容构建 `.atb` 格式快照 → 记一条 edit 版本
 * （60s 内连续编辑合并为一版，不逐键）。快照取内存（与保存同源，免每次存档多一次读盘 IPC——
 * 历史尽力而为，不阻塞保存流程）。
 */
function recordTableHistory(file: string): void {
  const s = useTableStore.getState();
  if (s.tableFile !== file) return; // 切表竞态：只记当前表格
  const snapshot = buildTableSnapshot();
  void recordHistoryVersion("table", file, {
    content: JSON.stringify(snapshot),
    action: "edit",
    coalesceEditMs: 60_000,
    // 人话摘要：新增/删除行 · 修改单元格 · 字段增删/改名（列表可读，替代 JSON 行级 diff）
    summarize: summarizeTableSnapshot,
  });
}

/**
 * 防抖持久化控制器：写盘期间又有新变更（persistCtl.version 已变）则保留 dirty，
 * 由下一轮 timer 再写，防写盘成功回调吞掉新编辑。force = 保留本地（绕过乐观锁强制覆盖）。
 * 增量保存：与 lastSaved 快照按引用 diff，只写变化实体（见 patchTableVault）；
 * 空补丁（撤销回退到已存状态等）跳过 IPC 仅清脏标志。
 */
const persistCtl = createPersistController<boolean>({
  persist: async (force = false) => {
    const versionAtStart = persistCtl.version;
    const { tableFile, id, fields, rows, baseUpdatedAt } = useTableStore.getState();
    if (!tableFile) return;
    // 写盘成功后的统一收尾（markSelfSave 先于守卫：写已发生，watcher 回放须抑制）
    const finish = (updatedAt: number | null, newFile?: string) => {
      if (updatedAt !== null) {
        markSelfSave(newFile && newFile !== tableFile ? [tableFile, newFile] : tableFile);
      }
      // 竞态守卫：await 期间可能已切换表格（load 替换了状态），旧表的写盘结果不得覆盖新表
      // 的乐观锁基准/脏标记（否则新表下次保存被误判冲突、脏编辑被吞）
      if (useTableStore.getState().tableFile !== tableFile) return;
      if (persistCtl.version !== versionAtStart) {
        // 写盘期间有新变更（已挂新 timer）：保留 dirty，由下一轮 timer 再写盘
        useTableStore.setState({ saving: false });
        return;
      }
      if (newFile && newFile !== tableFile) {
        useTableStore.setState({ tableFile: newFile });
      }
      useTableStore.setState({
        ...(updatedAt !== null ? { baseUpdatedAt: updatedAt } : {}),
        dirty: false,
        saving: false,
        error: null,
        // 保存/自动合并成功 = 冲突已解决：清掉此前合并失败置位的冲突条（自愈，
        // 用户无需手动点掉；真实合并失败时 reportError 会再次置位）
        conflictPending: false,
      });
      // 存档点：以当前内容快照记历史（60s 内连续编辑合并为一版；fire-and-forget 不阻塞保存流程）
      if (updatedAt !== null) recordTableHistory(newFile ?? tableFile);
      syncLastSaved();
    };
    const reportError = (e: unknown) => {
      const msg = typeof e === "string" ? e : e instanceof Error ? e.message : String(e);
      if (msg.includes("已被外部修改")) {
        // 自动合并兜底失败（重发补丁仍冲突的并发窗口）：提示用户重载/强制保存
        useTableStore.setState({ conflictPending: true, saving: false });
      } else {
        console.error("表格自动保存失败", e);
        useTableStore.setState({ error: "自动保存失败，请检查磁盘空间或权限", saving: false });
      }
    };
    /** 磁盘文件被外部删除：补丁只含变化实体，重建会丢未变化部分——回退全量写（与旧行为一致）。
     * 主补丁路径与乐观锁合并路径共用（合并路径读盘时同样可能遇到文件已删）。
     * 必须读最新 state：await 主补丁期间用户可能又有新编辑，写起始快照会把 stale 内容
     * 短暂覆盖磁盘；同时防切表后把新表内容写进旧文件（tableFile 变了即放弃，与 finish 守卫同语义）。 */
    const rewriteFull = async (): Promise<void> => {
      try {
        const latest = useTableStore.getState();
        if (latest.tableFile !== tableFile) return;
        const updatedAt = await writeTableVault(
          buildTableSnapshot(),
          tableFile,
          force ? undefined : latest.baseUpdatedAt,
        );
        finish(updatedAt);
      } catch (e2) {
        reportError(e2);
      }
    };
    try {
      const result = await patchTableVault({
        file: tableFile,
        tableId: id,
        fields,
        rows,
        lastSaved: { fields: lastSavedFields, rows: lastSavedRows },
        baseUpdatedAt,
        force,
      });
      finish(result ? result.updatedAt : null, result?.file);
    } catch (e) {
      if (typeof e === "string" && e.includes("表格文件不存在（已从磁盘删除）")) {
        await rewriteFull();
      } else if (typeof e === "string" && e.includes("已被外部修改")) {
        // 乐观锁冲突：自动三方合并（磁盘基底 + 本地增量 LWW）重发补丁；
        // 合并本身失败/再冲突（并发窗口）→ 回落冲突提示由用户决策
        try {
          await retryMergePersist(tableFile, finish);
        } catch (mergeErr) {
          if (
            typeof mergeErr === "string" &&
            mergeErr.includes("表格文件不存在（已从磁盘删除）")
          ) {
            // 合并读盘时磁盘文件已被外部删除：同样回退全量重建
            await rewriteFull();
          } else {
            reportError(mergeErr);
          }
        }
      } else {
        reportError(e);
      }
    }
  },
  beforeSchedule: () => useTableStore.setState({ saving: true, dirty: true }),
});

/** debounce 500ms 持久化（内容变更后调用；timer 回调里读最新 state）。 */
function schedulePersist(): void {
  const st = useTableStore.getState();
  if (!st.tableFile) return;
  // 协作实时广播：编辑即达（不等防抖落盘）。补丁 = 与广播基线的引用 diff（幂等 LWW，
  // 与磁盘合并语义一致，顺序变化经 order 携带）。广播基线独立于落盘基线：远端已应用内容
  // 推进广播基线后不再被重发全房；applyRemotePatch 路径不调本函数 → 无广播回环。
  if (collabBroadcast) {
    const patch = computeTablePatch({
      tableId: st.id,
      fields: st.fields,
      rows: st.rows,
      lastSaved: { fields: broadcastBaselineFields, rows: broadcastBaselineRows },
    });
    if (patch) collabBroadcast(st.tableFile, patch);
  }
  persistCtl.schedule();
}

/**
 * 增量保存基线快照：上次落盘时的 fields/rows 引用。
 * store 不可变更新——未变实体引用不变，保存时与快照按引用 diff，只序列化变化实体
 * （image dataURL 大字段不重传）。load/保存成功时同步；撤销不在此同步
 * （基线恒为「已写盘状态」，撤销产生的差异经 diff 自然落盘）。
 */
let lastSavedFields: TableField[] = [];
let lastSavedRows: TableRow[] = [];

/** 协作广播基线：房间已看到的最新状态（本端已广播 + 已从远端应用）。
 * 与落盘基线分离——远端已应用内容对房间是已知的，不得随本地增量重发（否则广播补丁持续膨胀）；
 * 无远端补丁时恒等于落盘基线（无回归）。 */
let broadcastBaselineFields: TableField[] = [];
let broadcastBaselineRows: TableRow[] = [];

/** 协作实时广播钩子（collabStore.init 注入，dispose 清空；null = 协作未启用）。 */
let collabBroadcast: ((file: string, patch: TablePatch) => void) | null = null;

/** 按 id 全序重排数组（应用远端补丁的 order；未出现 id 保持相对顺序置尾——与 Rust reorder_by 同语义）。 */
function reorderByIds<T extends { id: string }>(items: T[], order: string[]): T[] {
  return reorderByRank(items, new Map(order.map((id, i) => [id, i] as const)));
}

/** 把当前运行时 fields/rows 引用记为「已落盘基线」；同时按同一内存态推进广播基线
 * （加载/清空/保存成功三处收敛点共用，保证无远端补丁时广播基线恒等于落盘基线）。 */
function syncLastSaved(): void {
  const s = useTableStore.getState();
  lastSavedFields = s.fields;
  lastSavedRows = s.rows;
  syncBroadcastBaseline();
}

/** 把当前运行时 fields/rows 引用记为「协作广播基线」。应用远端补丁/冲突合并改写内存态后调用，
 * 使该内容不再被当作本地增量重发全房（远端内容对房间是已知的）。 */
function syncBroadcastBaseline(): void {
  const s = useTableStore.getState();
  broadcastBaselineFields = s.fields;
  broadcastBaselineRows = s.rows;
}

/**
 * 复位表格运行时态（load 失败降级 / clear 清空共用同一份 13 字段字面量，防两处漂移）：
 * tableFile 清空后不可写回，选中/冲突/编辑回退标记一并归零。error = 本次复位的原因文案（null = 正常清空）。
 */
function resetTableState(error: string | null): void {
  useTableStore.setState({
    tableFile: null,
    id: "",
    title: "",
    fields: [],
    rows: [],
    baseUpdatedAt: 0,
    dirty: false,
    saving: false,
    conflictPending: false,
    error,
    selectedRowId: null,
    selection: null,
    undoResetCell: null,
  });
}

/** 协作对端是否同表在线：其内存/撤销栈可能仍引用附件文件（共享盘文件多人共用），
 * 本端单方回收会使其破图——有对端在线时跳过回收（磁盘堆积可接受，数据安全优先）；
 * 亦用于 watcher 判别「磁盘写入是对端保存的广播回放（内容已应用，不得重载回退）」。 */
export function hasCollabPeerOnTable(file: string): boolean {
  return useCollabStore.getState().peers.some((p) => p.presence?.file === file);
}

/** 样式增量合并：patch 中**显式出现**的键才处理（值 undefined = 清除该项，真值 = 设置），
 * 未出现的键保持原样——同一单元格可叠加多种样式（如先设字号再点粗体，字号不被清掉）。
 * 用 `in` 判定「显式出现」而非 `patch.x !== undefined`：`Partial` 下缺省键取值为 undefined，
 * 会把「未触及」误判为「清除」。全空 → undefined（空样式 ≈ 缺省键，落盘保持文件干净）。
 * 布尔切换由调用方按汇总态决定传 true/undefined。 */
function mergeCellStyle(cur: CellStyle | undefined, patch: Partial<CellStyle>): CellStyle | undefined {
  const next: CellStyle = cur ? { ...cur } : {};
  const applyKey = <K extends keyof CellStyle>(key: K) => {
    if (key in patch) {
      const v = patch[key];
      if (v === undefined) delete next[key];
      else next[key] = v;
    }
  };
  applyKey("b");
  applyKey("i");
  applyKey("u");
  applyKey("s");
  applyKey("color");
  applyKey("bg");
  applyKey("font");
  applyKey("size");
  return Object.keys(next).length === 0 ? undefined : next;
}

/** 清空指定矩形区域全部单元格（一步撤销；图片格跳过——单格 Delete 同口径不清图片；
 *  无实际变化不入栈不落盘）。框选清空与剪切共用，区域均由 selectionRegion 产出（下标恒合法）。 */
function clearRegionCells(region: TableRegion): void {
  const s = useTableStore.getState();
  let anyChange = false;
  const outRows = s.rows.map((row, r) => {
    if (r < region.rowStart || r > region.rowEnd) return row;
    let changed = false;
    const values = { ...row.values };
    for (let c = region.colStart; c <= region.colEnd; c++) {
      // 图片格不清空（与单格 Delete 不清图片同口径）
      if (s.fields[c].type === "image") continue;
      const cur = values[s.fields[c].id];
      if (cellValueEqual(cur, undefined)) continue;
      values[s.fields[c].id] = undefined;
      changed = true;
    }
    if (!changed) return row;
    anyChange = true;
    return { ...row, values };
  });
  if (!anyChange) return;
  undoMgr.push();
  useTableStore.setState({ rows: outRows });
  schedulePersist();
}

export const useTableStore = create<TableStoreState>((set, get) => ({
  tableFile: null,
  id: "",
  title: "",
  fields: [],
  rows: [],
  baseUpdatedAt: 0,
  dirty: false,
  saving: false,
  conflictPending: false,
  error: null,
  selectedRowId: null,
  selection: null,
  view: "table",
  undoResetCell: null,

  load: async (file) => {
    // 切换表格前取消未落盘的保存定时器 + 清空撤销栈：快照含旧表内容，混用会串表污染撤销
    persistCtl.cancel();
    undoMgr.clear();
    abortEditSession();
    // 离开旧表：先落盘再回收其孤儿图片附件——防抖窗口内新导入的图片引用尚未写盘，
    // 直接按磁盘引用集合回收会把它误判孤儿删掉（丢图）；flush 失败（乐观锁冲突等）时
    // 磁盘集合同样可能缺本地引用，跳过回收留到下次切表；协作对端同表在线也跳过（见
    // hasCollabPeerOnTable）。切表即清空旧表撤销栈与显示缓存，会话内删除的图片此刻才
    // 没有恢复路径，回收安全（读盘失败保守跳过，不阻塞打开）
    const prevFile = get().tableFile;
    if (prevFile && prevFile !== file) {
      void (async () => {
        try {
          await get().flush();
        } catch {
          return;
        }
        const s = useTableStore.getState();
        if (s.tableFile !== prevFile && !s.dirty && !hasCollabPeerOnTable(prevFile)) {
          void cleanupTableAttachments(prevFile).catch((e) =>
            console.error("回收表格孤儿图片失败", e),
          );
        }
      })();
    }
    // 换表清上一张表图片显示缓存（路径含 tableId 目录段不会撞，防长会话内存累积）
    clearTableImageCache();
    try {
      const table = await readTableVault(file);
      set({
        tableFile: file,
        id: table.id,
        title: table.title,
        fields: table.fields,
        rows: table.rows,
        baseUpdatedAt: table.updatedAt,
        dirty: false,
        saving: false,
        conflictPending: false,
        error: null,
        selectedRowId: null,
        selection: null,
        undoResetCell: null,
      });
      // 已落盘基线 = 加载的磁盘状态（后续保存按引用 diff，未变实体不重写）
      syncLastSaved();
    } catch (e) {
      console.error("加载表格失败", e);
      // 复位文件态：读失败时上一张表的内容/路径不得残留——继续编辑会写进错误的表文件
      resetTableState(e instanceof Error ? e.message : String(e));
    }
  },

  reloadFromDisk: async () => {
    const file = get().tableFile;
    if (!file) return;
    // load 会清 dirty/冲突并同步乐观锁基准（磁盘即最新）
    await get().load(file);
  },

  resolveConflict: async (keepLocal) => {
    set({ conflictPending: false });
    if (keepLocal) {
      await get().flush(true);
    } else {
      await get().reloadFromDisk();
    }
  },

  clear: () => {
    // 取消保存定时器，防残留 timer 重写已删文件；清撤销栈（内容已不存在）
    persistCtl.cancel();
    undoMgr.clear();
    abortEditSession();
    // 关闭表格：回收其孤儿图片附件（撤销栈/显示缓存随 clear 清空，删除的图片无恢复路径；
    // 调用方契约已先 flush——closeTable 先落盘，closeTableSilent 文件已删读盘失败保守跳过）。
    // 协作对端同表在线跳过（见 hasCollabPeerOnTable）
    const prevFile = get().tableFile;
    if (prevFile && !hasCollabPeerOnTable(prevFile)) {
      void cleanupTableAttachments(prevFile).catch((e) =>
        console.error("回收表格孤儿图片失败", e),
      );
    }
    clearTableImageCache();
    resetTableState(null);
    syncLastSaved();
  },

  clearError: () => set({ error: null }),

  flush: async (force = false) => {
    const { tableFile, dirty } = get();
    // 无脏/未打开：复位 saving（schedulePersist 曾置位但 timer 内无改动可写，防状态卡死）
    if (!tableFile || !dirty) {
      set({ saving: false });
      return false;
    }
    await persistCtl.flush(force);
    return true;
  },

  updateCell: (rowId, fieldId, value) => {
    set((s) => ({
      rows: s.rows.map((r) =>
        r.id === rowId
          ? { ...r, values: { ...r.values, [fieldId]: value } }
          : r,
      ),
    }));
    // 编辑会话的提交（入栈点在会话入口：选中打字/双击）；任何新变更作废 redo（undo 后产生新变更，Ctrl+Y 不得恢复旧快照）
    undoMgr.touchRedo();
    schedulePersist();
  },

  applyCellStyle: (patch) => {
    const s = get();
    const region = selectionRegion(s.selection, s.fields, s.rows);
    if (!region) return;
    let anyChange = false;
    const outRows = s.rows.map((row, r) => {
      if (r < region.rowStart || r > region.rowEnd) return row;
      let styles = row.styles;
      let rowChanged = false;
      for (let c = region.colStart; c <= region.colEnd; c++) {
        const fieldId = s.fields[c].id;
        const next = patch === null ? undefined : mergeCellStyle(styles?.[fieldId], patch);
        if (styleEqual(styles?.[fieldId], next)) continue;
        // 惰性克隆：首次变更才复制映射，不污染原对象（不可变更新约定，防撤销快照被改）
        if (!styles) styles = {};
        else if (!rowChanged) styles = { ...styles };
        if (next === undefined) delete styles[fieldId];
        else styles[fieldId] = next;
        rowChanged = true;
      }
      if (!rowChanged) return row;
      // 整行样式清空 → 删 styles 字段（空映射 ≈ 缺省，落盘保持文件干净）
      const cleanStyles = styles && Object.keys(styles).length === 0 ? undefined : styles;
      anyChange = true;
      return { ...row, styles: cleanStyles };
    });
    if (!anyChange) return;
    undoMgr.push();
    set({ rows: outRows });
    schedulePersist();
  },

  addImageToCell: async (rowId, fieldId) => {
    const { id, tableFile } = get();
    if (!tableFile) return;
    try {
      const src = await pickFile([{ name: "图片", extensions: ["png", "jpg", "jpeg", "webp", "gif"] }]);
      if (!src) return;
      // 图片字节落 `.atelyx/attachments/<tableId>/`（隐藏目录：watcher/文件树零噪声），
      // 单元格只存唯一路径引用——保存补丁不含图片字节（大表保存提速）
      const rel = await importTableImage(src, id);
      // 图片增删 = 独立操作，一次一个撤销单元（读盘成功、变更前入栈）
      undoMgr.push();
      const current = get().rows.find((r) => r.id === rowId)?.values[fieldId];
      const prev = typeof current === "object" && current !== null ? current : undefined;
      const images = prev ? [...prev.images] : [];
      images.push(rel);
      // display 是按单元格记忆的展示偏好，增删图片不得重置
      get().updateCell(rowId, fieldId, { ...prev, images });
    } catch (e) {
      console.error("添加图片失败", e);
      set({ error: "添加图片失败，请重试" });
    }
  },

  removeImageAt: (rowId, fieldId, index) => {
    const current = get().rows.find((r) => r.id === rowId)?.values[fieldId];
    if (typeof current !== "object" || current === null) return;
    undoMgr.push();
    const images = [...current.images];
    images.splice(index, 1);
    get().updateCell(rowId, fieldId, { ...current, images }); // 保留 display（同 addImageToCell）
  },

  toggleImageDisplay: (rowId, fieldId) => {
    const current = get().rows.find((r) => r.id === rowId)?.values[fieldId];
    if (typeof current !== "object" || current === null) return;
    undoMgr.push();
    const display = current.display === "grid" ? undefined : "grid";
    get().updateCell(rowId, fieldId, { images: [...current.images], display });
  },

  reorderImages: (rowId, fieldId, order) => {
    const current = get().rows.find((r) => r.id === rowId)?.values[fieldId];
    if (typeof current !== "object" || current === null) return;
    // 写入边界校验：order 必须是 0..n-1 的排列（拖拽状态错乱时放弃，不写脏数据）
    const n = current.images.length;
    if (order.length !== n || new Set(order).size !== n || order.some((i) => i < 0 || i >= n)) return;
    undoMgr.push();
    get().updateCell(rowId, fieldId, { ...current, images: order.map((i) => current.images[i]) });
  },

  copyImageToClipboard: async (dataUrl) => {
    try {
      await copyImageSvc(dataUrl);
      return true;
    } catch (e) {
      console.error("复制图片失败", e);
      set({ error: "复制图片失败，请重试" });
      return false;
    }
  },

  downloadImageToDownloads: async (fileName, dataUrl) => {
    try {
      await saveImageToDownloads(fileName, dataUrl);
      return true;
    } catch (e) {
      console.error("下载图片失败", e);
      set({ error: "下载图片失败，请重试" });
      return false;
    }
  },

  exportXlsx: async () => {
    const { tableFile, id, title, fields, rows, baseUpdatedAt } = get();
    if (!tableFile) return false;
    try {
      const target = await saveFile({
        defaultPath: `${title}.xlsx`,
        filters: [{ name: "Excel 工作簿", extensions: ["xlsx"] }],
      });
      if (!target) return false;
      // 对话框等待期间内存可能被协作补丁改写：沿用弹出前捕获的内容导出
      await exportTableXlsx(
        buildTableSnapshot({ id, title, fields, rows, updatedAt: baseUpdatedAt }),
        target,
      );
      return true;
    } catch (e) {
      console.error("导出 xlsx 失败", e);
      set({ error: "导出 xlsx 失败，请重试" });
      return false;
    }
  },

  addField: (name, type) => {
    undoMgr.push();
    const field: TableField = { id: crypto.randomUUID(), name: name || "字段", type };
    set((s) => ({ fields: [...s.fields, field] }));
    schedulePersist();
  },

  insertField: (index, name, type) => {
    undoMgr.push();
    const field: TableField = { id: crypto.randomUUID(), name: name || "字段", type };
    set((s) => {
      const next = [...s.fields];
      next.splice(Math.max(0, Math.min(index, next.length)), 0, field);
      return { fields: next };
    });
    schedulePersist();
  },

  renameField: (fieldId, name) => {
    undoMgr.push();
    set((s) => ({
      fields: s.fields.map((f) => (f.id === fieldId ? { ...f, name: name || f.name } : f)),
    }));
    schedulePersist();
  },

  retypeField: (fieldId, type) => {
    undoMgr.push();
    set((s) => ({
      fields: s.fields.map((f) => {
        if (f.id !== fieldId) return f;
        // 改类型后原 calcType 可能不适用（如数字列求和 → 文本列），残留会产生误导统计，一并清除
        const calcType =
          f.calcType && CALC_TYPES_BY_FIELD[type].includes(f.calcType) ? f.calcType : undefined;
        return { ...f, type, calcType };
      }),
    }));
    schedulePersist();
  },

  setFieldOptions: (fieldId, options) => {
    undoMgr.push();
    set((s) => ({
      fields: s.fields.map((f) => (f.id === fieldId ? { ...f, options } : f)),
    }));
    schedulePersist();
  },

  setFieldWidth: (fieldId, width) => {
    // 拖拽高频：不入栈（TableEditor 拖拽开始时 pushUndo 一次）
    set((s) => ({
      fields: s.fields.map((f) => (f.id === fieldId ? { ...f, width } : f)),
    }));
    schedulePersist();
  },

  setRowHeight: (rowId, height) => {
    // 拖拽高频：不入栈（TableEditor 拖拽开始时 pushUndo 一次）
    set((s) => ({
      rows: s.rows.map((r) => (r.id === rowId ? { ...r, height } : r)),
    }));
    schedulePersist();
  },

  clearRowHeight: (rowId) => {
    // 无手动行高：no-op（不产生空撤销单元）
    if (!get().rows.some((r) => r.id === rowId && r.height !== undefined)) return;
    undoMgr.push();
    set((s) => ({
      rows: s.rows.map((r) => {
        if (r.id !== rowId) return r;
        const { height: _drop, ...rest } = r;
        return rest;
      }),
    }));
    schedulePersist();
  },

  /** 整表行高自适应：全部行清除手动行高（单次撤销单元；无手动行高 no-op）。 */
  clearAllRowHeights: () => {
    if (!get().rows.some((r) => r.height !== undefined)) return;
    undoMgr.push();
    set((s) => ({
      rows: s.rows.map(({ height: _drop, ...rest }) => rest),
    }));
    schedulePersist();
  },

  setCalcType: (fieldId, calcType) => {
    undoMgr.push();
    set((s) => ({
      fields: s.fields.map((f) => (f.id === fieldId ? { ...f, calcType } : f)),
    }));
    schedulePersist();
  },

  removeField: (fieldId) => {
    undoMgr.push();
    set((s) => {
      // 选中范围指向被删列（单元格/整列/框选两端）时清空，防高亮残留失效引用
      const stale = selUsesField(s.selection, fieldId);
      return {
        fields: s.fields.filter((f) => f.id !== fieldId),
        // 删除列同时清掉所有行的该列值（防 values 残留孤儿键）；样式同口径清理（防 styles 孤儿键）
        rows: s.rows.map((r) => {
          const { [fieldId]: _drop, ...rest } = r.values;
          if (!r.styles || r.styles[fieldId] === undefined) return { ...r, values: rest };
          const styles = { ...r.styles };
          delete styles[fieldId];
          const cleanStyles = Object.keys(styles).length === 0 ? undefined : styles;
          return { ...r, values: rest, styles: cleanStyles };
        }),
        selection: stale ? null : s.selection,
      };
    });
    schedulePersist();
  },

  addRow: () => {
    undoMgr.push();
    const row: TableRow = { id: crypto.randomUUID(), values: {} };
    set((s) => ({ rows: [...s.rows, row] }));
    schedulePersist();
  },

  duplicateRow: (rowId) => {
    undoMgr.push();
    set((s) => {
      const idx = s.rows.findIndex((r) => r.id === rowId);
      if (idx < 0) return {};
      const src = s.rows[idx];
      // 复制 = 得到一样的行：值 + 显示样式一并拷贝（styles 映射浅拷即可，CellStyle 不可变）
      const copy: TableRow = {
        id: crypto.randomUUID(),
        values: { ...src.values },
        ...(src.styles ? { styles: { ...src.styles } } : {}),
      };
      const next = [...s.rows];
      next.splice(idx + 1, 0, copy);
      return { rows: next };
    });
    schedulePersist();
  },

  removeRow: (rowId) => {
    undoMgr.push();
    set((s) => {
      // 选中范围指向被删行（单元格/整行/框选两端）时清空，防高亮残留失效引用
      const stale = selUsesRow(s.selection, rowId);
      return {
        rows: s.rows.filter((r) => r.id !== rowId),
        selectedRowId: s.selectedRowId === rowId ? null : s.selectedRowId,
        selection: stale ? null : s.selection,
      };
    });
    schedulePersist();
  },

  moveRow: (dragId, targetIndex) => {
    const rows = get().rows;
    const from = rows.findIndex((r) => r.id === dragId);
    if (from < 0) return;
    // targetIndex 是拖放时「原始数组」的插入位：移除拖拽行后校正下标
    let insertAt = from < targetIndex ? targetIndex - 1 : targetIndex;
    insertAt = Math.max(0, Math.min(insertAt, rows.length - 1));
    if (insertAt === from) return;
    undoMgr.push();
    const next = [...rows];
    const [moved] = next.splice(from, 1);
    next.splice(insertAt, 0, moved);
    set({ rows: next });
    schedulePersist();
  },

  selectCell: (rowId, fieldId) => {
    set({ selection: { kind: "cell", rowId, fieldId }, selectedRowId: rowId });
  },

  selectRow: (rowId) => {
    if (rowId === null) {
      set({ selection: null, selectedRowId: null });
    } else {
      set({ selection: { kind: "row", rowId }, selectedRowId: rowId });
    }
  },

  selectField: (fieldId) => set({ selection: { kind: "column", fieldId }, selectedRowId: null }),

  selectAll: () => set({ selection: { kind: "all" }, selectedRowId: null }),

  selectRange: (anchorRowId, anchorFieldId, rowId, fieldId) =>
    set({
      selection: { kind: "range", anchorRowId, anchorFieldId, rowId, fieldId },
      selectedRowId: anchorRowId,
    }),

  copySelection: () => {
    const s = get();
    const region = selectionRegion(s.selection, s.fields, s.rows);
    if (!region) return;
    const text = buildRegionTsv(s.fields, s.rows, region);
    if (text === "") return;
    // 剪贴板被占用等写失败：置面板错误提示（与复制图片失败同口径），不静默
    void writeClipboardText(text).catch((e) => {
      console.warn("复制单元格到剪贴板失败", e);
      set({ error: "复制失败，请重试" });
    });
  },

  cutSelection: async () => {
    const s = get();
    const sel = s.selection;
    const file = s.tableFile;
    const region = selectionRegion(sel, s.fields, s.rows);
    if (!region) return;
    const text = buildRegionTsv(s.fields, s.rows, region);
    // 先写剪贴板：成功才清空（写失败不销毁数据）
    if (text !== "") {
      try {
        await writeClipboardText(text);
      } catch (e) {
        console.warn("剪切单元格到剪贴板失败", e);
        set({ error: "剪切失败，请重试" });
        return;
      }
    }
    // await 窗口内可能切表/收远端补丁：同表校验后按捕获 selection（行/列以 id 定位）重解析区域，
    // 增删使 id 失效（region null）或已切表则跳过清空——剪贴板已写入，数据不丢
    const latest = get();
    if (latest.tableFile !== file) return;
    const regionNow = selectionRegion(sel, latest.fields, latest.rows);
    if (!regionNow) return;
    clearRegionCells(regionNow);
  },

  pasteFromClipboard: async () => {
    let text: string;
    try {
      text = await readClipboardText();
    } catch {
      return; // 剪贴板无文本（如图片）→ 非错误态，忽略
    }
    const grid = parseTsv(text);
    // 全空串网格：无内容可贴（不得补行补列制造空结构）
    if (grid.every((line) => line.every((c) => c === ""))) return;
    // await 期间可能改选中/切表：以最新 state 为准（防贴进旧锚点/旧表）
    const s = get();
    const region = selectionRegion(s.selection, s.fields, s.rows);
    if (!region) return;
    const { fields, rows } = applyPasteGrid(s.fields, s.rows, region.rowStart, region.colStart, grid);
    if (fields === s.fields && rows === s.rows) return; // 无实际变化：不置脏不入栈
    undoMgr.push();
    set({ fields, rows });
    schedulePersist();
  },

  clearSelectionCells: () => {
    const s = get();
    const region = selectionRegion(s.selection, s.fields, s.rows);
    if (!region) return;
    clearRegionCells(region);
  },

  setView: (view) => set({ view }),

  pushUndo: () => undoMgr.push(),

  undo: () => {
    if (undoMgr.undo()) {
      // 撤销弹掉的是进行中编辑会话的入口（栈顶即会话、无其他操作插入）→ 该单元格草稿须回退；
      // 弹掉的是无关条目（如会话期间 AI 填行）→ 不打断输入，草稿保留
      const sessionPopped =
        sessionCell !== null && undoMgr.size < sessionUndoDepth;
      set({ undoResetCell: sessionPopped ? sessionCell : null });
      commitEditSession();
      schedulePersist();
    }
  },

  redo: () => {
    if (undoMgr.redo()) {
      set({ undoResetCell: null });
      commitEditSession();
      schedulePersist();
    }
  },

  beginCellEdit: (rowId, fieldId) => beginEditSession(rowId, fieldId),
  commitCellEdit: () => commitEditSession(),
  abortCellEdit: () => abortEditSession(),

  setCollabBroadcast: (fn) => {
    collabBroadcast = fn;
  },

  applyRemotePatch: (file, patch) => {
    // 只应用当前打开的表格；补丁表格 id 不符（陈旧/串文件）拒绝——防污染本端状态
    const s = get();
    if (file !== s.tableFile || patch.id !== s.id) return;
    set((st) => {
      // 与 Rust patch_table_vault 同语义：removed 过滤 → upsert 按 id 覆盖/追加 → order 重排
      const removedFieldIds = new Set(patch.removedFieldIds);
      let fields = st.fields.filter((f) => !removedFieldIds.has(f.id));
      for (const f of patch.upsertFields) {
        const i = fields.findIndex((x) => x.id === f.id);
        if (i >= 0) fields[i] = f;
        else fields.push(f);
      }
      if (patch.fieldOrder) fields = reorderByIds(fields, patch.fieldOrder);
      const removedRowIds = new Set(patch.removedRowIds);
      let rows = st.rows.filter((r) => !removedRowIds.has(r.id));
      // 远端行归一化（旧版本对端可能仍广播 string[] 旧形态图片值，进内存前统一）
      for (const r of patch.upsertRows.map(normalizeTableRow)) {
        const i = rows.findIndex((x) => x.id === r.id);
        if (i >= 0) rows[i] = r;
        else rows.push(r);
      }
      if (patch.rowOrder) rows = reorderByIds(rows, patch.rowOrder);
      // 远端删除使本端选中失效（含框选两端任一 id）：清理（与 removeRow/removeField 同谓词）
      const sel = st.selection;
      const staleSel =
        sel &&
        (patch.removedFieldIds.some((id) => selUsesField(sel, id)) ||
          patch.removedRowIds.some((id) => selUsesRow(sel, id)));
      return {
        fields,
        rows,
        selection: staleSel ? null : sel,
        selectedRowId:
          st.selectedRowId && removedRowIds.has(st.selectedRowId) ? null : st.selectedRowId,
      };
    });
    // 核心：远端已应用内容对房间已知，推进广播基线，避免被当作本地增量重发全房
    syncBroadcastBaseline();
    // 版本守卫洞加固：本端有未保存改动（dirty）时重挂一次保存调度——在途保存捕获的是
    // 「未含远端补丁」的旧状态，写盘后 finish() 会因 persistCtl.version 未变而标干净并
    // 把 lastSaved 推进到磁盘之前（污染磁盘，经对端 watcher 重载使对端丢内容）。
    // 重挂使版本变化 → 保持 dirty，由下一轮 timer 带上远端内容重写。仅 dirty 时重挂：
    // 干净接收端不触发保存（无广播回环，与「接收端不落盘」约定一致）。
    if (get().dirty) persistCtl.schedule();
  },

  tableHistoryLoad: (file) => loadTableHistory("table", file),

  tableHistoryRollback: async (file, seq) => {
    // 仅当前打开表格可回滚（随后重载内存态）；file 须匹配当前表格
    if (get().tableFile !== file) return null;
    const versions = await loadTableHistory("table", file);
    const content = versionContentAt(versions, seq);
    if (content == null) return null;
    try {
      const snapshot = JSON.parse(content) as TableFile;
      if (!snapshot || snapshot.schema !== TABLE_SCHEMA) return null;
      // 取消挂起的防抖保存：回滚写盘后旧 timer 不得把恢复前内容写回
      persistCtl.cancel();
      // 全量写回快照（baseUpdatedAt 缺省 = 绕过乐观锁——回滚是显式覆盖，显式用户意图）
      await writeTableVault(snapshot, file);
      // 抑制 watcher 回波，然后重载内存（含乐观锁基准与撤销栈重置，同冲突「重新加载」语义）
      markSelfSave(file);
      set({ dirty: false });
      await get().reloadFromDisk();
      // 回滚记一条 restore 版本（滚动恢复点 + 审计「何时回滚到哪」）
      await recordHistoryVersion("table", file, {
        content,
        action: "restore",
        summarize: summarizeTableSnapshot,
      });
      return content;
    } catch (e) {
      console.error("表格回滚失败", e);
      return null;
    }
  },
}));
