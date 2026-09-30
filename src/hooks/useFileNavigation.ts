/**
 * 文件导航入口（打开画布 / 笔记 / 表格 / 外部白板）。
 *
 * 打开动作统一经 appStore（分层：组件 → store），导航回调不逐层透传。
 * 返回对象为模块级稳定引用（处理器内 `getState()` 调用，不引入渲染订阅）。
 */
import { useAppStore } from "@/stores/appStore";
import { baseName, noteTitleFromFile, stripExt, tableTitleFromFile } from "@/utils/filename";
import type { CanvasFileRow } from "@/types";

export interface FileNavigation {
  /** 打开已有画布行（列表命中 row；新建画布后传实际 row）。 */
  openCanvasRow: (row: CanvasFileRow) => void;
  /** 打开笔记（标题按路径推导）。 */
  openNote: (file: string) => void;
  /** 打开表格（标题按路径推导）。 */
  openTable: (file: string) => void;
  /** 打开外部 `.canvas` 白板（无画布列表条目，合成为只读查看行）。 */
  openWhiteboard: (path: string) => void;
}

const navigation: FileNavigation = {
  openCanvasRow: (row) => useAppStore.getState().openCanvas(row),
  openNote: (file) => useAppStore.getState().openNote(file, noteTitleFromFile(file)),
  openTable: (file) => useAppStore.getState().openTable(file, tableTitleFromFile(file)),
  openWhiteboard: (path) =>
    useAppStore.getState().openCanvas({
      id: path,
      title: stripExt(baseName(path)),
      file: path,
      updatedAt: 0,
    }),
};

export function useFileNavigation(): FileNavigation {
  return navigation;
}
