/**
 * 多维表格编辑器（表格视图）。
 *
 * 布局：工具条（视图切换 / 导出；冲突/错误/保存状态在面板 header）→ 表格主体（行号列 + 列头 + 类型化单元格）→
 * 行尾「+ 新行」→ 底部横向滑动条 → 状态栏（列自动计算，整格点击选类型 + 实时结果）。
 *
 * 交互要点：
 * - 选中体系（互斥）：左上角整格单击全选（角标示意）/ 右键全选并弹菜单；表头单击选整列；
 *   行首单击选整行；单元格按下不动（<5px）松手 = 选中（td 金色内描边，进入隐藏编辑态，
 *   编辑时保持单元格大小），拖动（>5px）= 拖拽框选多格（范围全体金色内描边）。
 *   选中后打字直达常驻输入框，首个字符/IME 组合覆盖原值；双击 = 取消覆盖（保留原值）。
 * - 复制/粘贴/剪切（选中区域 ↔ 系统剪贴板 TSV）：Ctrl+C 复制 / Ctrl+V 粘贴 / Ctrl+X 剪切
 *   （先写剪贴板成功再清空选中区域，一步撤销；编辑态输入框聚焦时放行原生行为）；
 *   数据单元格右键菜单「复制/剪切/粘贴」（点在当前选区内保留选区、否则落单格；
 *   整表选中时右键仍弹列宽/行高自适应菜单）。粘贴以选区左上角为锚点展开、越界自动补行/补列。
 * - 行拖拽为 pointer 模拟（HTML5 DnD 在 WebView2 不可靠，与文件面板同策略）：
 *   行首手柄按下 → 位移超 5px 激活 → 按行元素中点计算插入位（金色插入线指示）→ 松手 moveRow。
 * - 列宽：列头右缘拖拽（钳制 MIN/MAX）；行高：行首底缘拖拽。表头/行首/整表选中后右键菜单
 *   （`ColumnMenu`/`RowMenu`/`SelectAllMenu`）提供列宽/行高自适应与左右插入字段。
 * - 状态栏：每列整格 hover 高亮可点击（未设置留空），弹出计算类型菜单（固定向上弹出，
 *   底边贴点击位置不遮住点击处）；已设置列居中显示「类型 + 结果」。
 * - 冲突/错误/保存状态在面板 header 展示（`PanelFrame` 读 tableStore）。
 * - 弹层菜单（字段/列/行/整表/状态栏）见 `TableMenus.tsx`。
 */
import { GripVertical, MoreHorizontal, MoveDiagonal, Plus, Sigma } from "lucide-react";
import { Fragment, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTableStore } from "@/stores/tableStore";
import { useUiStateStore } from "@/stores/uiStateStore";
import { usePluginStore } from "@/stores/pluginStore";
import { TableCell, clearCell, navigateCell, navDirection } from "@/components/table/TableCell";
import { useCollabStore } from "@/stores/collabStore";
import type { CollabPeer } from "@/types";
import {
  AddFieldMenu,
  CellMenu,
  ColumnMenu,
  FieldMenu,
  RowMenu,
  SelectAllMenu,
  StatMenu,
} from "@/components/table/TableMenus";
import { computeColumnCalc, fieldDefaultWidth, selectionRegion, type TableRegion } from "@/utils/table";
import { HistoryModal } from "@/components/history/HistoryModal";
import { PopupLayer } from "@/components/common/PopupLayer";
import { SlotListMount } from "@/components/plugins/SlotHost";
import { DropdownSelect } from "@/components/common/DropdownSelect";
import { ErrorBoundary } from "@/components/common/ErrorBoundary";
import { usePopupAnchor } from "@/hooks/usePopupAnchor";
import { TableFormatToolbar } from "@/components/table/TableFormatToolbar";
import {
  ADD_FIELD_COL_WIDTH,
  CALC_TYPE_LABELS,
  MAX_COL_WIDTH,
  MAX_ROW_HEIGHT,
  MAX_TABLE_ZOOM,
  MIN_COL_WIDTH,
  MIN_ROW_HEIGHT,
  MIN_TABLE_ZOOM,
  ROW_NUM_COL_WIDTH,
} from "@/constants/table";
import type { TableField, TableRow } from "@/types";

/** Ctrl+滚轮缩放灵敏度（指数因子：deltaY px → zoom 倍率，负号 = 上滚放大）。 */
const ZOOM_SENSITIVITY = 0.0015;

/** 插件表格视图承载：按 kind 渲染注册的组件（缺注册空渲染；插件崩溃不影响 App）。
 * key=kind：错误态是 class 组件内部 state（不随 children 变化清除），切到另一插件视图须重建边界，
 * 防 A 崩溃后 B 复用 A 的错误面板。 */
function PluginTableViewMount({ kind }: { kind: string }) {
  usePluginStore((s) => s.uiRevision);
  const tv = usePluginStore.getState().pluginTableView(kind);
  if (!tv) return null;
  const Comp = tv.component;
  return (
    <ErrorBoundary key={kind}>
      <Comp />
    </ErrorBoundary>
  );
}

/** 表格编辑器：panelId 用于聚焦判定（撤销/重做快捷键门控，同画布快捷键惯例）。 */
export function TableEditor({ panelId }: { panelId: string }) {
  const fields = useTableStore((s) => s.fields);
  const rows = useTableStore((s) => s.rows);
  const selection = useTableStore((s) => s.selection);
  const selectRow = useTableStore((s) => s.selectRow);
  const selectCell = useTableStore((s) => s.selectCell);
  const selectField = useTableStore((s) => s.selectField);
  const selectAll = useTableStore((s) => s.selectAll);
  const focusedPanelId = useUiStateStore((s) => s.focusedPanelId);
  const addRow = useTableStore((s) => s.addRow);
  const view = useTableStore((s) => s.view);
  const setView = useTableStore((s) => s.setView);
  const exportXlsx = useTableStore((s) => s.exportXlsx);
  // 插件表格视图（工具条视图列表 + 内容区分派；订阅 uiRevision 随插件注册/卸载刷新——
  // 数组/注册经 getState 取，避免 selector 每次新引用致无限重渲染，同 PanelTabBar 惯例）
  usePluginStore((s) => s.uiRevision);
  const pluginTableViews = usePluginStore.getState().pluginTableViews();
  const currentTableView = view !== "table" ? usePluginStore.getState().pluginTableView(view) : undefined;
  // 当前视图 kind 无注册 → 回退表格视图（防无按钮高亮的悬空态；注册对象引用稳定，无重入）。
  // 注：插件更新/重装/启停的重载瞬态（先注销后重注册）也会短暂触发回退——内存态、一次点击可恢复，可接受。
  useEffect(() => {
    if (view !== "table" && !currentTableView) setView("table");
  }, [view, currentTableView, setView]);
  const [exported, setExported] = useState(false);
  useEffect(() => {
    if (!exported) return;
    const t = setTimeout(() => setExported(false), 1500);
    return () => clearTimeout(t);
  }, [exported]);

  // 工具条右上角「···」菜单（历史记录 + 导出 xlsx；统一 usePopupAnchor + PopupLayer 浮层）
  const moreTriggerRef = useRef<HTMLButtonElement>(null);
  const moreMenu = usePopupAnchor(moreTriggerRef);
  const [historyOpen, setHistoryOpen] = useState(false);

  // 字段菜单 / 行菜单 / 添加字段浮层 / 状态栏计算菜单
  const [fieldMenu, setFieldMenu] = useState<{ fieldId: string; x: number; y: number } | null>(null);
  const [columnMenu, setColumnMenu] = useState<{ fieldId: string; x: number; y: number } | null>(null);
  const [rowMenu, setRowMenu] = useState<{ rowId: string; x: number; y: number } | null>(null);
  const [addFieldMenu, setAddFieldMenu] = useState<{ x: number; y: number } | null>(null);
  const [statMenu, setStatMenu] = useState<{ fieldId: string; x: number; y: number } | null>(null);
  const [allMenu, setAllMenu] = useState<{ x: number; y: number } | null>(null);
  const [cellMenu, setCellMenu] = useState<{ x: number; y: number } | null>(null);
  // 气泡格式工具栏锚点（视口坐标；null = 关闭）。拖框选完成 / 再次点击已选中单元格 / 右键「格式」浮出
  const [formatBar, setFormatBar] = useState<{ x: number; y: number } | null>(null);
  /** 稳定 onClose：工具栏订阅 rows/selection 高频重渲染，内层 document 监听按此依赖避免反复重绑。 */
  const closeFormatBar = useCallback(() => setFormatBar(null), []);

  // ===== 视图缩放（Ctrl+滚轮，CSS zoom；纯视图状态，不持久化、不入撤销栈，切文件随重挂复位）=====
  const [zoom, setZoom] = useState(1);
  /** zoom 的 ref 镜像：wheel 监听常驻（deps 仅 view），回调内读最新值免重复绑定。 */
  const zoomRef = useRef(1);
  /** 缩放包装层（表格主体 + 状态栏 + 底部横向滑动条：同一缩放坐标系，滚动同步天然精确）。 */
  const zoomWrapRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = zoomWrapRef.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      if (!e.ctrlKey && !e.metaKey) return;
      // 拦截默认整页缩放（WebView2 默认 Ctrl+滚轮 = 页面整体缩放），钳制边界同样生效
      e.preventDefault();
      // deltaMode 1 = 行（部分 Linux 驱动），统一换算像素；指数曲线平滑且天然夹在正区间
      const dy = e.deltaMode === 1 ? e.deltaY * 16 : e.deltaY;
      const next = Math.min(MAX_TABLE_ZOOM, Math.max(MIN_TABLE_ZOOM, zoomRef.current * Math.exp(-dy * ZOOM_SENSITIVITY)));
      const rounded = Math.round(next * 100) / 100;
      zoomRef.current = rounded;
      setZoom(rounded);
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, [view]);

  // ===== 行拖拽插入排序（pointer 模拟）=====
  const dragRef = useRef<{ rowId: string; startX: number; startY: number; active: boolean } | null>(null);
  const [draggingRowId, setDraggingRowId] = useState<string | null>(null);
  const [dragInsertIndex, setDragInsertIndex] = useState<number | null>(null);
  const dragInsertIndexRef = useRef<number | null>(null);
  const rowsElRef = useRef<HTMLDivElement>(null);
  const moveRow = useTableStore((s) => s.moveRow);

  useEffect(() => {
    const onMove = (e: PointerEvent) => {
      const d = dragRef.current;
      if (!d) return;
      if (!d.active && Math.hypot(e.clientX - d.startX, e.clientY - d.startY) <= 5) return;
      dragRef.current = { ...d, active: true };
      setDraggingRowId(d.rowId);
      // 插入位 = 首个「中点低于光标」的行下标（纯几何，防元素遮挡干扰）；全高于 → 行尾
      const container = rowsElRef.current;
      let insertAt = 0;
      if (container) {
        const els = container.querySelectorAll<HTMLElement>("[data-row-id]");
        insertAt = els.length;
        for (let i = 0; i < els.length; i++) {
          const r = els[i].getBoundingClientRect();
          if (e.clientY < r.top + r.height / 2) {
            insertAt = i;
            break;
          }
        }
      }
      dragInsertIndexRef.current = insertAt;
      setDragInsertIndex(insertAt);
    };
    const onUp = () => {
      const d = dragRef.current;
      dragRef.current = null;
      const insertAt = dragInsertIndexRef.current;
      if (d?.active && insertAt !== null) moveRow(d.rowId, insertAt);
      dragInsertIndexRef.current = null;
      setDraggingRowId(null);
      setDragInsertIndex(null);
    };
    document.addEventListener("pointermove", onMove);
    document.addEventListener("pointerup", onUp);
    return () => {
      document.removeEventListener("pointermove", onMove);
      document.removeEventListener("pointerup", onUp);
    };
  }, [moveRow]);

  const startRowDrag = useCallback((e: React.PointerEvent, rowId: string) => {
    if (e.button !== 0) return;
    e.preventDefault(); // 阻止文本选择干扰
    dragRef.current = { rowId, startX: e.clientX, startY: e.clientY, active: false };
  }, []);

  // ===== Ctrl+Z/Y 撤销/重做 + Ctrl+C/V 复制/粘贴 + 选中态焦点兜底导航（全局键盘监听；
  // 选中单元格的覆盖输入由常驻输入框承接，不经此监听）=====
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      // 仅表格面板聚焦时生效（同画布快捷键门控惯例）；编辑框聚焦同样接管
      // （受控 input/textarea 原生撤销不可靠，整编辑会话一步撤销与 Esc 放弃编辑互补）
      const ctrl = e.ctrlKey || e.metaKey;
      if (ctrl && !e.altKey && focusedPanelId === panelId) {
        const key = e.key.toLowerCase();
        const active = document.activeElement as HTMLElement | null;
        const isCellEditor = !!active?.hasAttribute("data-cell-editor");
        // 焦点在弹层菜单/面板内其它输入框（字段重命名/单选选项/插入字段等非单元格编辑器）→
        // 放行原生 Ctrl+Z/Y/C/V；表格级接管只作用于单元格编辑器与空白选中态
        if (
          !isCellEditor &&
          (active instanceof HTMLInputElement ||
            active instanceof HTMLTextAreaElement ||
            active instanceof HTMLSelectElement ||
            !!active?.isContentEditable)
        ) {
          return;
        }
        if (key === "z" && !e.shiftKey) {
          e.preventDefault();
          useTableStore.getState().undo();
          return;
        }
        if (key === "y" || (key === "z" && e.shiftKey)) {
          e.preventDefault();
          useTableStore.getState().redo();
          return;
        }
        // 复制/粘贴/剪切：放大预览打开时归预览（Esc/方向键）；编辑态单元格输入框（data-editing）
        // 放行原生（复制草稿/贴入输入框/剪切草稿）；选中态才接管为「选中区域 ↔ 剪贴板 TSV」结构化复制粘贴
        if (key === "c" || key === "v" || key === "x") {
          if (document.querySelector("[data-lightbox]")) return;
          if (active?.hasAttribute("data-editing")) return;
          const st = useTableStore.getState();
          if (!st.selection || st.view !== "table") return;
          e.preventDefault();
          if (key === "c") st.copySelection();
          else if (key === "x") void st.cutSelection();
          else void st.pasteFromClipboard();
          return;
        }
      }
      if (ctrl || e.altKey) return;
      if (focusedPanelId !== panelId) return;
      // —— 焦点兜底导航 ——
      // 导航撞上非文本字段单元格（singleSelect/image 无常驻输入框）或点击空白后焦点落 body，
      // 方向键默认行为会滚动表格：此处按选中态语义接管导航；输入框聚焦（编辑态/选中态）时
      // 自身 keydown 已处理，不重复；lightbox 打开时方向键归其切图，不接管。
      const st = useTableStore.getState();
      if (st.view !== "table") return;
      const sel = st.selection;
      if (sel?.kind !== "cell" && sel?.kind !== "range") return;
      const el = document.activeElement;
      if (el && el !== document.body && el !== document.documentElement) return;
      if (document.querySelector("[data-lightbox]")) return;
      if (e.key === "Escape") {
        e.preventDefault();
        st.selectRow(null); // 取消并清选中
        return;
      }
      if (e.key === "Backspace" || e.key === "Delete") {
        e.preventDefault(); // 防浏览器默认（滚动/后退）
        // 框选 = 整块清空；单格 = 文本类字段清空（输入框失焦时语义照常），其余无操作
        if (sel.kind === "range") {
          st.clearSelectionCells();
        } else {
          const field = st.fields.find((f) => f.id === sel.fieldId);
          if (field && (field.type === "text" || field.type === "number" || field.type === "duration")) {
            clearCell(sel.rowId, sel.fieldId);
          }
        }
        return;
      }
      if (sel.kind !== "cell") return; // 框选无方向键导航/打字语义
      const dir = navDirection(e.key);
      if (dir) {
        e.preventDefault();
        navigateCell(sel.rowId, sel.fieldId, dir); // 移动选中（高亮跟随，输入框回到文本列自动聚焦）
        return;
      }
      // 可打印字符（焦点落空后打字）：重新聚焦常驻输入框，不 preventDefault——
      // 字符随 keydown 默认行为落入新焦点元素，覆盖编辑照常（同步聚焦机制）
      if (e.key.length === 1) {
        const field = st.fields.find((f) => f.id === sel.fieldId);
        if (field && (field.type === "text" || field.type === "number" || field.type === "duration")) {
          rowsElRef.current
            ?.querySelector<HTMLTextAreaElement | HTMLInputElement>("[data-cell-editor]")
            ?.focus();
        }
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [focusedPanelId, panelId]);

  // ===== 单元格按下手势：<5px 松手 = 选中（进入隐藏编辑态）；>5px 拖动 = 拖拽框选多格（selectRange）=====
  const cellPressRef = useRef<{
    x: number;
    y: number;
    active: boolean;
    rowId: string;
    fieldId: string;
    /** 图片字段格：自身滑动/排序手势，不参与框选。 */
    noRange: boolean;
    /** 按下前格式条是否已浮出（再点已选中单元格 = 切换开/关 的判定依据）。 */
    barWasOpen: boolean;
  } | null>(null);
  /** 框选拖拽最近一次落到的目标格（同格不重复 setState，防逐像素重渲染）。 */
  const rangeDragCellRef = useRef<string | null>(null);

  /** 区域 → 格式条锚点（选区左上角，视口坐标；CSS zoom 下 getBoundingClientRect 已是视口像素，
   *  格式条 fixed 定位天然不随表格缩放）。取不到区域 DOM（越界/未渲染）返回 null。 */
  const formatAnchorOfRegion = useCallback((region: TableRegion): { x: number; y: number } | null => {
    if (region.rowStart > region.rowEnd) return null;
    const st = useTableStore.getState();
    const sel = (rid: string, fid: string) =>
      rowsElRef.current?.querySelector<HTMLElement>(`[data-row-id="${rid}"] [data-field-id="${fid}"]`);
    const first = sel(st.rows[region.rowStart]?.id ?? "", st.fields[region.colStart]?.id ?? "");
    const last = sel(st.rows[region.rowEnd]?.id ?? "", st.fields[region.colEnd]?.id ?? "");
    if (!first || !last) return null;
    const r1 = first.getBoundingClientRect();
    return { x: r1.left, y: r1.top - 6 };
  }, []);

  useEffect(() => {
    const onMove = (e: PointerEvent) => {
      const p = cellPressRef.current;
      if (!p) return;
      if (!p.active) {
        if (Math.hypot(e.clientX - p.x, e.clientY - p.y) <= 5) return;
        cellPressRef.current = { ...p, active: true };
        rangeDragCellRef.current = null;
      }
      // 图片格自身手势（轮播滑动/长按排序）：拖拽不参与框选（active 已置位，释放不落单格选择）
      if (p.noRange) return;
      // 框选拖拽：定位指针下的数据格并更新范围（拖出表格/落在空白则保持上次目标格）
      const el = document.elementFromPoint(e.clientX, e.clientY) as HTMLElement | null;
      const cellEl = el?.closest<HTMLElement>("[data-field-id]");
      const fieldId = cellEl?.dataset.fieldId;
      const rowId = cellEl?.closest<HTMLElement>("[data-row-id]")?.dataset.rowId;
      if (!rowId || !fieldId) return;
      // 同窗口多表格面板：只认本实例容器内的格（防跨面板混入对端行列 id）
      if (!rowsElRef.current?.contains(cellEl)) return;
      const key = `${rowId}:${fieldId}`;
      if (key === rangeDragCellRef.current) return;
      // 指针首次离开锚点格前不建 range（防格内微拖产生 1×1 range 使打字/方向键失效）
      if (rangeDragCellRef.current === null && key === `${p.rowId}:${p.fieldId}`) return;
      rangeDragCellRef.current = key;
      useTableStore.getState().selectRange(p.rowId, p.fieldId, rowId, fieldId);
    };
    const onUp = () => {
      const wasDrag = cellPressRef.current?.active ?? false;
      // 本次拖拽是否真正创建/更新过 range（rangeDragCellRef 非空即 selectRange 被调用）。
      // 图片格长按滑动（noRange 路径）不建 range，若沿用旧选区会误弹格式条——用此守卫排除
      const didRange = rangeDragCellRef.current !== null;
      cellPressRef.current = null;
      rangeDragCellRef.current = null;
      // 拖框选完成 → 浮出格式条（锚定选区左上角）；单格点击由 releaseCellPress 处理
      if (wasDrag && didRange) {
        const st = useTableStore.getState();
        if (st.selection?.kind === "range") {
          const region = selectionRegion(st.selection, st.fields, st.rows);
          if (region) {
            const anchor = formatAnchorOfRegion(region);
            if (anchor) setFormatBar(anchor);
          }
        }
      }
    };
    document.addEventListener("pointermove", onMove);
    document.addEventListener("pointerup", onUp);
    return () => {
      document.removeEventListener("pointermove", onMove);
      document.removeEventListener("pointerup", onUp);
    };
  }, [formatAnchorOfRegion]);

  const startCellPress = useCallback(
    (e: React.PointerEvent, rowId: string, fieldId: string) => {
      if (e.button !== 0) return;
      // noRange：图片字段格有自身滑动/排序手势，按下即排除其参与框选（click 选择不受影响）
      const field = useTableStore.getState().fields.find((f) => f.id === fieldId);
      cellPressRef.current = {
        x: e.clientX,
        y: e.clientY,
        active: false,
        rowId,
        fieldId,
        noRange: field?.type === "image",
        // 按下时条是否已浮出（快照，供松手判定切换开/关；deps 随 formatBar 重建保证读到最新提交值）
        barWasOpen: formatBar !== null,
      };
    },
    [formatBar],
  );

  /** 单元格松手（未拖动）→ 选中 + 标记面板聚焦（点 td 不冒泡到面板层，快捷键门控需显式标记）+
   *  聚焦常驻输入框（已选中单元格的再次点击不触发重渲染，须直接聚焦防失焦后打字失效）。 */
  const releaseCellPress = useCallback(
    (rowId: string, fieldId: string) => {
      const p = cellPressRef.current;
      if (!p || p.active) return;
      cellPressRef.current = null;
      useUiStateStore.getState().setFocusedPanel(panelId);
      // 再次点击已选中单元格 = 切换格式条（按下前已浮出 → 本次点击关闭；未浮出 → 浮出）；
      // 输入框保持聚焦——开始键入时格式条自行关闭继续编辑。首次点击 = 选中即编辑（聚焦输入框）
      const st = useTableStore.getState();
      const prevRegion = selectionRegion(st.selection, st.fields, st.rows);
      const rIdx = st.rows.findIndex((r) => r.id === rowId);
      const cIdx = st.fields.findIndex((f) => f.id === fieldId);
      const wasSelected =
        prevRegion !== null &&
        rIdx >= prevRegion.rowStart &&
        rIdx <= prevRegion.rowEnd &&
        cIdx >= prevRegion.colStart &&
        cIdx <= prevRegion.colEnd;
      selectCell(rowId, fieldId);
      if (wasSelected && p.barWasOpen) {
        // 条按下时已浮出且点在已选区内 → 关闭（外点关闭已先行，此处兜底确保不重开）
        setFormatBar(null);
      } else if (wasSelected) {
        const anchor = formatAnchorOfRegion({
          rowStart: rIdx,
          rowEnd: rIdx,
          colStart: cIdx,
          colEnd: cIdx,
        });
        if (anchor) setFormatBar(anchor);
      }
      rowsElRef.current
        ?.querySelector<HTMLTextAreaElement | HTMLInputElement>("[data-cell-editor]")
        ?.focus();
    },
    [panelId, selectCell, formatAnchorOfRegion],
  );

  // ===== 列宽 / 行高拖拽调整（指针模拟，与行拖拽同策略）=====
  const resizeRef = useRef<
    | { kind: "col"; fieldId: string; startX: number; startWidth: number; zoom: number }
    | { kind: "row"; rowId: string; startY: number; startHeight: number; zoom: number }
    | null
  >(null);
  /** 拖拽是否已入栈（首次实际变化才 push，点击未拖动不产生空撤销单元）。 */
  const pushedRef = useRef(false);
  const setFieldWidth = useTableStore((s) => s.setFieldWidth);
  const setRowHeight = useTableStore((s) => s.setRowHeight);
  useEffect(() => {
    const onMove = (e: PointerEvent) => {
      const r = resizeRef.current;
      if (!r) return;
      if (r.kind === "col") {
        // 缩放补偿：视口像素 ÷ zoom 才是数据宽度增量（视觉 1:1 跟手）
        const width = Math.round(
          Math.max(MIN_COL_WIDTH, Math.min(MAX_COL_WIDTH, r.startWidth + (e.clientX - r.startX) / r.zoom)),
        );
        // 拖拽会话首次实际变化才入栈（期间连续调整合并为一步撤销）
        if (!pushedRef.current && width !== r.startWidth) {
          useTableStore.getState().pushUndo();
          pushedRef.current = true;
        }
        setFieldWidth(r.fieldId, width);
      } else {
        const height = Math.round(
          Math.max(MIN_ROW_HEIGHT, Math.min(MAX_ROW_HEIGHT, r.startHeight + (e.clientY - r.startY) / r.zoom)),
        );
        if (!pushedRef.current && height !== r.startHeight) {
          useTableStore.getState().pushUndo();
          pushedRef.current = true;
        }
        setRowHeight(r.rowId, height);
      }
    };
    const onUp = () => {
      resizeRef.current = null;
      pushedRef.current = false;
      document.body.style.cursor = "";
    };
    document.addEventListener("pointermove", onMove);
    document.addEventListener("pointerup", onUp);
    return () => {
      document.removeEventListener("pointermove", onMove);
      document.removeEventListener("pointerup", onUp);
      document.body.style.cursor = "";
    };
  }, [setFieldWidth, setRowHeight]);

  const startColResize = useCallback((e: React.PointerEvent, field: TableField) => {
    if (e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation();
    resizeRef.current = {
      kind: "col",
      fieldId: field.id,
      startX: e.clientX,
      startWidth: field.width ?? fieldDefaultWidth(field.name),
      zoom: zoomRef.current,
    };
    pushedRef.current = false;
    document.body.style.cursor = "col-resize";
  }, []);

  /** 行高拖拽起点：手动高度优先，缺省读当前 DOM 实际行高（内容撑开的值），防起点跳变。 */
  const startRowResize = useCallback((e: React.PointerEvent, row: TableRow) => {
    if (e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation();
    const el = rowsElRef.current?.querySelector<HTMLElement>(`[data-row-id="${row.id}"]`);
    resizeRef.current = {
      kind: "row",
      rowId: row.id,
      startY: e.clientY,
      startHeight: row.height ?? el?.offsetHeight ?? MIN_ROW_HEIGHT,
      zoom: zoomRef.current,
    };
    pushedRef.current = false;
    document.body.style.cursor = "row-resize";
  }, []);

  /** 列宽：手动调整值优先，缺省按字段名自适应。 */
  const widthOf = (f: TableField): number => f.width ?? fieldDefaultWidth(f.name);
  /** 表格总宽 = 行号列 + 各列宽 + 表头末尾「+」列：显式设给 table，拖宽一列只增总宽、
   *  其他列不挤压（border-collapse + table-fixed 无显式宽度时引擎会撑满容器并重分配）。 */
  const totalWidth = ROW_NUM_COL_WIDTH + fields.reduce((acc, f) => acc + widthOf(f), 0) + ADD_FIELD_COL_WIDTH;

  // ===== 协作 presence：同仓库看同一表格的在线用户（远端选中高亮 + 工具条胶囊）=====
  const tableFile = useTableStore((s) => s.tableFile);
  const collabPeers = useCollabStore((s) => s.peers);
  /** 本表格相关的远端用户（presence.file 匹配当前表格；未打开表格时无）。
   * useMemo 稳定引用：下游 peerShadowByCell 依赖它，新数组每渲染会致 memo 失效重建。 */
  const tablePeers = useMemo(
    () => (tableFile ? collabPeers.filter((p) => p.presence?.file === tableFile) : []),
    [tableFile, collabPeers],
  );
  /** 定位到远端用户选中位置（区域首行滚动到视口中央；插件表格视图卡片同样带 data-row-id，
   *  document 查询命中当前渲染的视图——任一布局内表格视图唯一，无歧义）。 */
  const focusPeer = (p: CollabPeer) => {
    const sel = p.presence?.selection;
    if (!sel || sel.kind === "all") return;
    const region = selectionRegion(sel, fields, rows);
    // 空表 + 列/全选选区：rowStart=0 而 rows 为空，须兜底防 rows[0] 越界
    if (!region || region.rowStart >= rows.length) return;
    const rowId = rows[region.rowStart].id;
    const el =
      view === "table"
        ? rowsElRef.current?.querySelector<HTMLElement>(`[data-row-id="${rowId}"]`)
        : document.querySelector<HTMLElement>(`[data-row-id="${rowId}"]`);
    el?.scrollIntoView({ block: "center", inline: "nearest" });
  };

  // ===== 选中高亮（互斥：单元格/框选 = td 内描边；行/列/整表 = 淡金背景）=====
  const rowHighlighted = (rowId: string): boolean =>
    selection?.kind === "row" ? selection.rowId === rowId : selection?.kind === "all";
  const colHighlighted = (fieldId: string): boolean =>
    selection?.kind === "column" ? selection.fieldId === fieldId : selection?.kind === "all";
  const cellSelected = (rowId: string, fieldId: string): boolean =>
    selection?.kind === "cell" && selection.rowId === rowId && selection.fieldId === fieldId;
  /** 本端框选区域格键集（每渲染构建一次，cellShadow 逐格 O(1) 查询——防大表逐格重算区域）。 */
  const localRangeCellKeys = useMemo(() => {
    if (selection?.kind !== "range") return null;
    const region = selectionRegion(selection, fields, rows);
    if (!region) return null;
    const set = new Set<string>();
    for (let r = region.rowStart; r <= region.rowEnd; r++) {
      for (let c = region.colStart; c <= region.colEnd; c++) {
        set.add(`${rows[r].id}:${fields[c].id}`);
      }
    }
    return set;
  }, [selection, fields, rows]);
  const rangeSelected = (rowId: string, fieldId: string): boolean =>
    localRangeCellKeys?.has(`${rowId}:${fieldId}`) ?? false;
  /** 行/列/整表的淡金背景（列选中作用于表头与数据单元格）。 */
  const highlightBg = (highlighted: boolean) => (highlighted ? "color-mix(in srgb, var(--accent) 8%, transparent)" : undefined);
  /** 远端选中覆盖映射：按区域展开一次构建（cell/range/row/column 统一经 selectionRegion；
   *  all 全表不渲染——满屏描边干扰协作），逐格查表 O(1)——替代每格 peersAtCell 的 filter
   * 分配（大表 + 多用户时每渲染 O(N·P) → O(N)）。 */
  const peerShadowByCell = useMemo(() => {
    const map = new Map<string, CollabPeer[]>();
    if (tablePeers.length === 0) return map;
    for (const p of tablePeers) {
      const sel = p.presence?.selection;
      if (!sel || sel.kind === "all") continue;
      const region = selectionRegion(sel, fields, rows);
      if (!region) continue;
      // 下标恒合法：region 产自 selectionRegion，退化区间（空表）循环不执行
      for (let r = region.rowStart; r <= region.rowEnd; r++) {
        const row = rows[r];
        for (let c = region.colStart; c <= region.colEnd; c++) {
          const k = `${row.id}:${fields[c].id}`;
          const arr = map.get(k) ?? [];
          arr.push(p);
          map.set(k, arr);
        }
      }
    }
    return map;
  }, [tablePeers, rows, fields]);
  /** 单元格描边：本端选中（单格/框选）= 金色 2px；远端覆盖 = 用户色 1px 同心叠加（多用户自动分层）。 */
  const cellShadow = (rowId: string, fieldId: string): string | undefined => {
    if (cellSelected(rowId, fieldId) || rangeSelected(rowId, fieldId)) return "inset 0 0 0 2px var(--accent)";
    const ps = peerShadowByCell.get(`${rowId}:${fieldId}`);
    if (!ps || ps.length === 0) return undefined;
    return ps.map((p, i) => `inset 0 0 0 ${i + 1}px ${p.color}`).join(", ");
  };

  // ===== 底部横向滑动条 + 状态栏：与表格横向滚动双向同步（常显；宽度 = 表格总宽 + 边框余量）=====
  const bottomScrollRef = useRef<HTMLDivElement>(null);
  const statBarRef = useRef<HTMLDivElement>(null);
  const syncScroll = (from: HTMLElement, to: HTMLElement) => {
    if (from.scrollLeft !== to.scrollLeft) to.scrollLeft = from.scrollLeft;
  };
  const onTableScroll = () => {
    // 滚动即关闭格式条（锚点随滚动失效；拖新选区/右键会重新浮出）
    setFormatBar(null);
    if (rowsElRef.current && bottomScrollRef.current) {
      syncScroll(rowsElRef.current, bottomScrollRef.current);
    }
    if (rowsElRef.current && statBarRef.current) {
      syncScroll(rowsElRef.current, statBarRef.current);
    }
  };
  const onBottomScroll = () => {
    // 底部/状态栏横向滚动（程序化设 scrollLeft 不触发 onTableScroll）也会使格式条锚点漂移，一并关闭
    setFormatBar(null);
    if (bottomScrollRef.current && rowsElRef.current) {
      syncScroll(bottomScrollRef.current, rowsElRef.current);
    }
  };
  const onStatBarScroll = () => {
    setFormatBar(null);
    if (statBarRef.current && rowsElRef.current) {
      syncScroll(statBarRef.current, rowsElRef.current);
    }
  };

  /** 插入位指示线（表格行之间渲染；colSpan 含行号列 + 字段列 + 表头「+」列）。 */
  const insertIndicator = (i: number) =>
    dragInsertIndex === i && draggingRowId ? (
      <tr className="pointer-events-none">
        <td colSpan={fields.length + 2} className="p-0" style={{ background: "var(--accent)", height: 2 }} />
      </tr>
    ) : null;

  return (
    <div className="h-full flex flex-col" style={{ background: "var(--bg-primary)" }}>
      {/* 工具条（冲突/错误/保存状态均在面板 header） */}
      <div
        className="flex items-center gap-2 px-3 py-1.5 border-b flex-shrink-0 text-xs"
        style={{ borderColor: "var(--border)", color: "var(--text-secondary)" }}
      >
        {/* 视图切换下拉：内置「表格」+ 插件表格视图（内存态不持久化；插件视图随 uiRevision 刷新） */}
        <div className="flex items-center gap-1 flex-shrink-0">
          <span>视图：</span>
          <DropdownSelect
            value={view}
            onChange={(v) => { setFormatBar(null); setView(v); }}
            options={[
              { value: "table", label: "表格" },
              ...pluginTableViews.map((tv) => ({ value: tv.kind, label: tv.label })),
            ]}
            className="px-2 py-1 text-xs rounded border flex-shrink-0"
            style={{ borderColor: "var(--border)", color: "var(--text-secondary)" }}
            title="切换视图"
          />
        </div>
        <span className="flex-1" />
        {/* 缩放百分比（Ctrl+滚轮缩放表格视图，纯视图状态；仅内置表格网格有效） */}
        {view === "table" && (
          <span
            className="flex-shrink-0 tabular-nums"
            style={{ color: "var(--text-muted)" }}
            title="Ctrl+滚轮缩放表格视图"
          >
            {Math.round(zoom * 100)}%
          </span>
        )}
        <span className="flex-shrink-0" style={{ color: "var(--text-muted)" }}>
          {rows.length} 行 · {fields.length} 列
        </span>
        {/* 协作：同看本表格的在线用户胶囊（点击定位其选中位置；断开连接自动消失） */}
        {tablePeers.length > 0 && (
          <div className="flex items-center gap-1 flex-shrink-0">
            {tablePeers.map((p) => (
              <button
                key={p.peerId}
                onClick={() => focusPeer(p)}
                className="flex items-center gap-1.5 px-1.5 py-0.5 rounded transition-colors hover:bg-[var(--hover)]"
                title={`${p.nickname}${p.deviceName ? `（${p.deviceName}）` : ""} · 点击定位到其选中`}
              >
                <span className="w-2 h-2 rounded-full flex-shrink-0" style={{ background: p.color }} />
                <span className="max-w-24 truncate" style={{ color: "var(--text-secondary)" }}>
                  {p.nickname}
                </span>
              </button>
            ))}
          </div>
        )}
        {/* 插件贡献区：表格工具条右侧（list 槽，priority 降序） */}
        <SlotListMount slot="toolbar/table/right" />
        {/* 「···」更多选项：历史记录 + 导出 xlsx（统一 usePopupAnchor + PopupLayer 浮层） */}
        <span className="flex-shrink-0">
          <button
            ref={moreTriggerRef}
            onClick={() => moreMenu.toggle()}
            title="更多选项"
            className="p-0.5 rounded hover:opacity-80"
            style={{ color: moreMenu.anchor ? "var(--accent)" : "var(--text-muted)" }}
          >
            <MoreHorizontal size={15} />
          </button>
          <PopupLayer
            anchor={moreMenu.anchor}
            onClose={moreMenu.close}
            triggerRef={moreTriggerRef}
            widthClass="w-36"
          >
            <button
              className="w-full flex items-center gap-2 px-2 py-1.5 text-xs hover:opacity-80"
              style={{ color: "var(--text-primary)" }}
              onClick={() => {
                moreMenu.close();
                setHistoryOpen(true);
              }}
              title="查看本表格的历史版本并回滚"
            >
              <span className="w-3.5 flex-shrink-0" />
              历史记录
            </button>
            <button
              className="w-full flex items-center gap-2 px-2 py-1.5 text-xs hover:opacity-80"
              style={{ color: exported ? "var(--accent)" : "var(--text-primary)" }}
              onClick={() => {
                moreMenu.close();
                void exportXlsx().then((ok) => {
                  if (ok) setExported(true);
                });
              }}
              title="导出为 xlsx"
            >
              <span className="w-3.5 flex-shrink-0" />
              {exported ? "已导出 xlsx" : "导出 xlsx"}
            </button>
          </PopupLayer>
        </span>
      </div>

      {/* 插件表格视图：整块替换表格主体（保留工具条；缺注册回退表格网格） */}
      {view !== "table" && currentTableView ? (
        <PluginTableViewMount kind={view} />
      ) : (
        <>
          {/* 缩放包装层：表格主体 + 状态栏 + 底部横向滑动条整体 CSS zoom（列/行/字体/表头随缩放比例；
              三者同一缩放坐标系，滚动同步 raw scrollLeft 与列对齐天然精确；底部条轨高经 8/zoom 补偿保持常 8px） */}
      <div
        ref={zoomWrapRef}
        className="table-zoom flex-1 min-h-0 flex flex-col"
        style={{ "--table-zoom": zoom } as React.CSSProperties}
      >
        {/* 表格主体（横向溢出滚动；onScroll 同步底部滑动条；原生横向滚动条隐藏避免双条） */}
      <div
        ref={rowsElRef}
        className="flex-1 min-h-0 overflow-auto relative no-horizontal-scrollbar"
        onScroll={onTableScroll}
        // 整表选中时右键任意数据单元格 → 列宽/行高自适应菜单（行首/表头各自处理；实时读 selection 防冒泡冲突）
        onContextMenu={(e) => {
          if (useTableStore.getState().selection?.kind !== "all") return;
          e.preventDefault();
          setAllMenu({ x: e.clientX, y: e.clientY });
        }}
      >
        <table className="border-collapse table-fixed select-none" style={{ width: totalWidth }}>
          <colgroup>
            <col style={{ width: ROW_NUM_COL_WIDTH }} />
            {fields.map((f) => (
              <col key={f.id} style={{ width: widthOf(f) }} />
            ))}
            <col style={{ width: ADD_FIELD_COL_WIDTH }} />
          </colgroup>
          <thead>
            <tr>
              <th
                className="relative border-b border-r text-center sticky top-0 z-10"
                style={{ background: "var(--bg-secondary)", borderColor: "var(--border)" }}
                onClick={selectAll}
                onContextMenu={(e) => {
                  e.preventDefault();
                  e.stopPropagation();
                  selectAll();
                  setAllMenu({ x: e.clientX, y: e.clientY });
                }}
                title="单击全选 · 右键全选并弹出菜单"
              >
                {/* 右下角角标（纯示意，事件穿透）：整格可点击全选 */}
                <div
                  className="absolute bottom-0.5 right-0.5 w-4 h-4 pointer-events-none"
                  style={{ color: "var(--text-muted)" }}
                >
                  <MoveDiagonal size={11} />
                </div>
              </th>
              {fields.map((f) => (
                <th
                  key={f.id}
                  className="border-b border-r align-middle px-1.5 py-1 sticky top-0 z-10 group relative"
                  style={{
                    background: colHighlighted(f.id) ? "color-mix(in srgb, var(--accent) 8%, transparent)" : "var(--bg-secondary)",
                    borderColor: "var(--border)",
                  }}
                  onClick={() => selectField(f.id)}
                  onContextMenu={(e) => {
                    e.preventDefault();
                    selectField(f.id);
                    setColumnMenu({ fieldId: f.id, x: e.clientX, y: e.clientY });
                  }}
                  title="单击选中整列"
                >
                  <div className="flex items-center gap-1 min-w-0">
                    <span className="truncate flex-1 font-normal cursor-default" title={f.name}>
                      {f.name}
                    </span>
                    <button
                      onClick={(e) => {
                        e.stopPropagation();
                        const rect = e.currentTarget.getBoundingClientRect();
                        setFieldMenu({ fieldId: f.id, x: rect.left, y: rect.bottom + 2 });
                      }}
                      className="w-5 h-5 flex items-center justify-center rounded hover:bg-[var(--hover)] flex-shrink-0"
                      style={{ color: "var(--text-muted)" }}
                      title="字段菜单"
                    >
                      <MoreHorizontal size={13} />
                    </button>
                  </div>
                  {/* 列宽拖拽手柄：悬停显示金色分隔线，按下拖拽调整（钳制 MIN/MAX） */}
                  <div
                    onPointerDown={(e) => startColResize(e, f)}
                    className="absolute top-0 bottom-0 right-0 w-2 cursor-col-resize group-hover:bg-[var(--accent)]/25 transition-colors"
                    title="拖拽调整列宽"
                  />
                </th>
              ))}
              {/* 表头末尾「+」列：添加字段入口（点击弹出名称 + 类型浮层） */}
              <th
                className="border-b border-r align-middle px-1.5 py-1 sticky top-0 z-10 text-center"
                style={{ background: "var(--bg-secondary)", borderColor: "var(--border)" }}
              >
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    const rect = e.currentTarget.getBoundingClientRect();
                    setAddFieldMenu({ x: rect.left, y: rect.bottom + 2 });
                  }}
                  className="w-6 h-6 flex items-center justify-center rounded hover:bg-[var(--hover)]"
                  style={{ color: "var(--text-muted)" }}
                  title="添加字段"
                >
                  <Plus size={14} />
                </button>
              </th>
            </tr>
          </thead>
          <tbody>
            {rows.map((row, i) => (
              <Fragment key={row.id}>
                {insertIndicator(i)}
                <tr
                  data-row-id={row.id}
                  onClick={() => selectRow(row.id)}
                  className="align-top"
                  style={{
                    height: row.height ?? undefined,
                    opacity: draggingRowId === row.id ? 0.45 : 1,
                    background: highlightBg(rowHighlighted(row.id)),
                    cursor: draggingRowId === row.id ? "grabbing" : undefined,
                  }}
                >
                  {/* 行首：行号 + 拖拽手柄 + 底部行高拖拽手柄（行菜单走右键） */}
                  <td
                    className="relative border-b border-r px-1 text-center"
                    style={{ borderColor: "var(--border)" }}
                    onContextMenu={(e) => {
                      e.preventDefault();
                      selectRow(row.id);
                      setRowMenu({ rowId: row.id, x: e.clientX, y: e.clientY });
                    }}
                    title="右键行菜单 · 底部拖拽调行高"
                  >
                    {/* absolute 铺满 td：序号 + 拖拽手柄随行高垂直居中（行高拖拽手柄在其下层仍可交互） */}
                    <div className="absolute inset-0 flex items-center justify-center gap-0.5">
                      <span className="text-[10px]" style={{ color: "var(--text-muted)" }}>
                        {i + 1}
                      </span>
                      <button
                        onPointerDown={(e) => startRowDrag(e, row.id)}
                        className="w-4 h-5 flex items-center justify-center rounded cursor-grab hover:bg-[var(--hover)]"
                        style={{ color: "var(--text-muted)" }}
                        title="拖动排序"
                      >
                        <GripVertical size={11} />
                      </button>
                    </div>
                    {/* 行高拖拽手柄：底部横条，按下拖拽调整（钳制 MIN/MAX） */}
                    <div
                      onPointerDown={(e) => startRowResize(e, row)}
                      className="absolute left-0 right-0 bottom-0 h-1.5 cursor-row-resize hover:bg-[var(--accent)]/25 transition-colors"
                      title="拖拽调整行高"
                    />
                  </td>
                  {fields.map((f) => (
                    <td
                      key={f.id}
                      // data-field-id：框选拖拽 elementFromPoint 定位目标格用（配合 tr 的 data-row-id）
                      data-field-id={f.id}
                      // relative：编辑态 textarea absolute inset-0 以此为定位上下文撑满单元格
                      className="border-b border-r relative"
                      style={{
                        borderColor: "var(--border)",
                        background: highlightBg(colHighlighted(f.id)),
                        boxShadow: cellShadow(row.id, f.id),
                      }}
                      onPointerDown={(e) => startCellPress(e, row.id, f.id)}
                      onPointerUp={() => releaseCellPress(row.id, f.id)}
                      onContextMenu={(e) => {
                        // 整表选中时容器层接管（列宽/行高自适应菜单）；其余弹出单元格复制/粘贴菜单
                        const st = useTableStore.getState();
                        if (st.selection?.kind === "all") return;
                        e.preventDefault();
                        e.stopPropagation();
                        // 右键点在当前选中区域内 → 保留选区（复制整个区域）；否则落单格
                        const region = selectionRegion(st.selection, st.fields, st.rows);
                        const rIdx = st.rows.findIndex((x) => x.id === row.id);
                        const cIdx = st.fields.findIndex((x) => x.id === f.id);
                        if (
                          !region ||
                          rIdx < region.rowStart ||
                          rIdx > region.rowEnd ||
                          cIdx < region.colStart ||
                          cIdx > region.colEnd
                        ) {
                          st.selectCell(row.id, f.id);
                        }
                        setCellMenu({ x: e.clientX, y: e.clientY });
                      }}
                      onClick={(e) => e.stopPropagation()}
                    >
                      <TableCell field={f} row={row} />
                    </td>
                  ))}
                  {/* 表头「+」列对应的空单元格：行尾横线延伸与表头对齐（border-r 与表头右缘对称） */}
                  <td className="border-b border-r" style={{ borderColor: "var(--border)" }} />
                </tr>
              </Fragment>
            ))}
            {insertIndicator(rows.length)}
          </tbody>
        </table>
        {/* 行尾「+ 新行」：整行可点击创建，底边线与表头对齐（宽度 = 表格总宽，随表格横向滚动） */}
        <div
          className="border-b cursor-pointer transition-colors hover:bg-[var(--hover)]"
          style={{ borderColor: "var(--border)", width: totalWidth }}
          onClick={addRow}
          title="添加新行"
        >
          <div className="flex items-center gap-1 px-2 py-1.5 text-xs" style={{ color: "var(--text-muted)" }}>
            <Plus size={13} /> 新行
          </div>
        </div>
        </div>

        {/* 状态栏：列自动计算（整格点击选择计算类型，hover 高亮，内容居中，横向滚动与表格同步） */}
        <div
          ref={statBarRef}
          onScroll={onStatBarScroll}
          className="flex-shrink-0 border-t overflow-x-auto no-horizontal-scrollbar h-8"
          style={{ borderColor: "var(--border)" }}
        >
          {/* h-full：容器 h-8 固定高，内部 flex 无此会塌陷为内容高（items-stretch 只管它的子项），内容贴顶留白 */}
          <div className="flex h-full items-stretch" style={{ width: totalWidth }}>
            <div
              className="flex-shrink-0 flex items-center justify-center border-r"
              style={{ width: ROW_NUM_COL_WIDTH, color: "var(--text-muted)", borderColor: "var(--border)" }}
              title="列自动计算"
            >
              <Sigma size={13} />
            </div>
            {fields.map((f) => {
              const result = computeColumnCalc(f, rows);
              return (
                <div
                  key={f.id}
                  onClick={(e) => {
                    e.stopPropagation();
                    const rect = e.currentTarget.getBoundingClientRect();
                    setStatMenu({ fieldId: f.id, x: rect.left, y: rect.top });
                  }}
                  className="flex-shrink-0 flex items-center justify-center px-1.5 border-r cursor-pointer transition-colors hover:bg-[var(--hover)]"
                  style={{ width: widthOf(f), borderColor: "var(--border)" }}
                  title={f.calcType ? `${CALC_TYPE_LABELS[f.calcType]}：${result ?? "—"}` : "选择计算类型"}
                >
                  {result !== null && (
                    <span className="truncate text-xs" style={{ color: "var(--text-primary)" }} title={result}>
                      {f.calcType ? `${CALC_TYPE_LABELS[f.calcType]} ${result}` : result}
                    </span>
                  )}
                </div>
              );
            })}
            <div className="flex-shrink-0" style={{ width: ADD_FIELD_COL_WIDTH }} />
          </div>
        </div>

        {/* 底部横向滑动条：与表格横向滚动双向同步（常显，防视图切换后消失）。
            放在缩放包装层内：与表格同一缩放坐标系，任意 zoom 下 raw scrollLeft 同步天然精确；
            高 = 8/zoom 抵消 zoom 让渲染高度恒为 8px（含边框；内容区渲染高 = 8-2·zoom ≥ 4px，
            满足 Chromium 水平滚动条内容高 ≥ 1px 防 scrollWidth 塌陷） */}
        <div
          ref={bottomScrollRef}
          onScroll={onBottomScroll}
          className="overflow-x-auto flex-shrink-0 border-t"
          style={{ borderColor: "var(--border)", height: 8 / zoom }}
        >
          <div style={{ width: totalWidth + 8, height: "100%" }} />
        </div>
        </div>
        </>
      )}

      {/* 字段菜单（⋮：字段修改） */}
      {fieldMenu && (
        <FieldMenu
          field={fields.find((f) => f.id === fieldMenu.fieldId)}
          x={fieldMenu.x}
          y={fieldMenu.y}
          onClose={() => setFieldMenu(null)}
        />
      )}
      {/* 表头右键菜单（列修改/调整） */}
      {columnMenu && (
        <ColumnMenu
          field={fields.find((f) => f.id === columnMenu.fieldId)}
          x={columnMenu.x}
          y={columnMenu.y}
          onClose={() => setColumnMenu(null)}
          onFormat={() => setFormatBar({ x: columnMenu.x, y: columnMenu.y })}
        />
      )}
      {/* 行菜单 */}
      {rowMenu && (
        <RowMenu
          rowId={rowMenu.rowId}
          x={rowMenu.x}
          y={rowMenu.y}
          onClose={() => setRowMenu(null)}
          onFormat={() => setFormatBar({ x: rowMenu.x, y: rowMenu.y })}
        />
      )}
      {/* 添加字段浮层 */}
      {addFieldMenu && (
        <AddFieldMenu x={addFieldMenu.x} y={addFieldMenu.y} onClose={() => setAddFieldMenu(null)} />
      )}
      {/* 状态栏计算类型菜单 */}
      {statMenu && (
        <StatMenu
          field={fields.find((f) => f.id === statMenu.fieldId)}
          x={statMenu.x}
          y={statMenu.y}
          onClose={() => setStatMenu(null)}
        />
      )}
      {/* 整表选中右键菜单：全部列宽 / 行高自适应 */}
      {allMenu && (
        <SelectAllMenu
          x={allMenu.x}
          y={allMenu.y}
          onClose={() => setAllMenu(null)}
          onFormat={() => setFormatBar({ x: allMenu.x, y: allMenu.y })}
        />
      )}
      {/* 数据单元格右键菜单：复制 / 剪切 / 粘贴 / 格式 */}
      {cellMenu && (
        <CellMenu
          x={cellMenu.x}
          y={cellMenu.y}
          onClose={() => setCellMenu(null)}
          onFormat={() => setFormatBar({ x: cellMenu.x, y: cellMenu.y })}
        />
      )}

      {/* 气泡格式工具栏（选中/框选/右键「格式」浮出；外点/Esc/滚动/键入自动关闭） */}
      {formatBar && <TableFormatToolbar anchor={formatBar} onClose={closeFormatBar} />}

      {/* 表格历史面板（工具条「···」→ 历史记录） */}
      <HistoryModal
        kind="table"
        file={tableFile ?? ""}
        open={historyOpen}
        onClose={() => setHistoryOpen(false)}
      />
    </div>
  );
}
