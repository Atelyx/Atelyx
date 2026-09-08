/**
 * 图片单元格（分派壳）：按单元格值 display 记忆两种展示模式——轮播（缺省）/
 * 九宫格，交互实现见 ImageCarouselMode/ImageGridMode 拆分件。
 *
 * - 布局：自适应行由「撑高层」（流内，只给高度）撑起固有高度（轮播 96px + 队列条；九宫格按
 *   列宽估算方块行高，单图按限宽修正），展示层 absolute inset-0 以 td 为定位上下文（td
 *   relative）铺满单元格并随行高伸展——行高手动调整/同行其他单元格撑高都不留白。不能依赖
 *   td 内百分比高度：td 无显式高度时 h-full 解析不可靠，flex-1 min-h-0 会塌陷成 0。
 * - 预载：多图/九宫格挂载即后台逐条解析全部条目（inflight 去重 + LRU 复用，单条失败不拖累
 *   其余），结果以**条目路径为键**共享给两个模式（重排后零重解析零错帧）；当前图另走
 *   useTableImageSrc 立即渲染。
 * - 放大预览：portal 到 body（逃离表格 CSS zoom 缩放包装层）；连续打开以序号守卫，
 *   仅最新请求落地（防并发解析互相覆盖）。
 * - hover 按钮组：左上角 = 展示模式切换（多图时）+ 追加图片；右上角 = 移除当前图（仅轮播，
 *   九宫格无移除入口）。
 * - 值读写经 store（addImageToCell/removeImageAt/toggleImageDisplay/reorderImages）；
 *   单元格值经 normalizeImageValue 读取（磁盘/远端旧形态与脏值统一归一，勿内联 typeof 判定）。
 */
import { GalleryHorizontal, ImagePlus, LayoutGrid, Plus, X } from "lucide-react";
import { memo, useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTableStore } from "@/stores/tableStore";
import { ImageLightbox } from "@/components/table/ImageLightbox";
import { ImageCarouselMode } from "@/components/table/ImageCarouselMode";
import { ImageGridMode } from "@/components/table/ImageGridMode";
import { resolveTableImageEntries, resolveTableImageEntry, useTableImageSrc } from "@/hooks/useTableImageSrc";
import { fieldDefaultWidth, normalizeImageValue } from "@/utils/table";
import {
  IMAGE_CAROUSEL_AREA_HEIGHT,
  IMAGE_GRID_GAP,
  IMAGE_GRID_SINGLE_MAX,
  IMAGE_GRID_WIDE_MIN,
  IMAGE_QUEUE_STRIP_HEIGHT,
} from "@/constants/table";
import type { TableField, TableRow } from "@/types";

interface Props {
  field: TableField;
  row: TableRow;
}

export const ImageCell = memo(function ImageCell({ field, row }: Props) {
  const cell = normalizeImageValue(row.values[field.id]);
  const images = cell.images;
  const gridMode = cell.display === "grid";
  const title = useTableStore((s) => s.title);
  const addImageToCell = useTableStore((s) => s.addImageToCell);
  const removeImageAt = useTableStore((s) => s.removeImageAt);
  const toggleImageDisplay = useTableStore((s) => s.toggleImageDisplay);

  const [idx, setIdx] = useState(0);
  // 图片数变化（增删）时钳制当前下标
  const cur = images.length === 0 ? 0 : Math.min(idx, images.length - 1);
  const [lightbox, setLightbox] = useState(false);
  const [lightboxUrls, setLightboxUrls] = useState<string[] | null>(null);
  // 预载：条目路径 → dataURL（以路径为键，重排/协作变更后元素按路径取图零错帧）
  const [srcMap, setSrcMap] = useState<Map<string, string>>(new Map());
  // 放大预览打开序号：仅最新一次解析落地（连点两格/解析竞态不互相覆盖）
  const lightboxSeqRef = useRef(0);

  const multi = images.length > 1;
  const fixedHeight = row.height !== undefined;

  useEffect(() => {
    // 九宫格所有格子与轮播跟手层/队列都只读 srcMap，必须预载；单图轮播走 useTableImageSrc 即可
    if (images.length === 0 || (!multi && !gridMode)) return;
    images.forEach((entry) => {
      resolveTableImageEntry(entry)
        .then((url) => setSrcMap((m) => (m.get(entry) === url ? m : new Map(m).set(entry, url))))
        .catch(() => {});
    });
  }, [images, multi, gridMode]);

  // 当前图即时渲染（未及预载/单图路径）；已预载则用 srcMap
  const currentSrc = useTableImageSrc(images[cur] ?? "");

  // setState 稳定引用：作为 onCurChange 下传给模式组件，供 document 级监听器长期持有
  const commitTo = useCallback((i: number) => setIdx(i), []);

  const openLightboxAt = async (i: number) => {
    setIdx(i);
    const seq = ++lightboxSeqRef.current;
    try {
      // 预览需要完整字节（大图）：一次性解析全部条目（data: 透传/路径走缓存）
      const urls = await resolveTableImageEntries(images);
      if (seq !== lightboxSeqRef.current) return; // 已有更新的打开请求，丢弃过期结果
      setLightboxUrls(urls);
      setLightbox(true);
    } catch {
      if (seq === lightboxSeqRef.current) setLightboxUrls(null);
    }
  };

  if (images.length === 0) {
    return (
      // 占位（流内 min-h-8）撑起 td 最小高度；按钮 absolute 铺满 td（td relative）垂直居中，
      // 行高更高时按钮随单元格整体居中
      <div className="group min-h-8 p-1">
        <button
          onClick={() => void addImageToCell(row.id, field.id)}
          className="absolute inset-0 w-full h-full flex items-center justify-center rounded opacity-0 group-hover:opacity-100 transition-opacity hover:bg-[var(--hover)]"
          style={{ color: "var(--text-muted)" }}
          title="添加图片"
        >
          <ImagePlus size={14} />
        </button>
      </div>
    );
  }

  // 九宫格列数：2 张 2 列；≥3 张按列宽自适应（宽列 3 列 / 窄列 2 列）；单图 1 个限宽方块
  const colWidth = field.width ?? fieldDefaultWidth(field.name);
  const gridCols = images.length <= 2 ? images.length : colWidth >= IMAGE_GRID_WIDE_MIN ? 3 : 2;

  // 撑高层高度（仅自适应行；固定行高由 tr height 保证）
  let sizerHeight = 0;
  if (!fixedHeight) {
    if (gridMode) {
      // 方块行高 = (列内容宽 - 间距) / 列数；td 内容宽 ≈ 列宽 - 2px 边框
      const innerW = colWidth - 2 - 8;
      const tileW = Math.max(24, (innerW - (gridCols - 1) * IMAGE_GRID_GAP) / gridCols);
      // 单图方块限宽，按实际渲染宽估算（否则撑出高瘦长条）
      const effTileW = images.length === 1 ? Math.min(tileW, IMAGE_GRID_SINGLE_MAX) : tileW;
      const nRows = Math.ceil(images.length / gridCols);
      sizerHeight = nRows * effTileW + (nRows - 1) * IMAGE_GRID_GAP + 8;
    } else {
      sizerHeight = IMAGE_CAROUSEL_AREA_HEIGHT + (multi ? IMAGE_QUEUE_STRIP_HEIGHT : 0);
    }
  }

  return (
    <>
      {/* 撑高层：只给自适应行一个固有高度（固定行高由 tr height 保证），不参与定位与交互 */}
      {!fixedHeight && <div style={{ height: sizerHeight }} />}
      {/* 展示层：absolute 以 td 为定位上下文（td relative），铺满单元格并随行高伸展 */}
      <div className="absolute inset-0 flex flex-col group">
        {gridMode ? (
          <ImageGridMode
            images={images}
            cur={cur}
            onCurChange={commitTo}
            onOpenAt={openLightboxAt}
            srcMap={srcMap}
            gridCols={gridCols}
            rowId={row.id}
            fieldId={field.id}
          />
        ) : (
          <ImageCarouselMode
            images={images}
            cur={cur}
            onCurChange={commitTo}
            onOpenAt={openLightboxAt}
            srcMap={srcMap}
            currentSrc={currentSrc}
            rowId={row.id}
            fieldId={field.id}
          />
        )}
        {/* 左上角 hover：模式切换（多图）/ 追加；右上角 hover：移除当前图（仅轮播，九宫格无移除入口） */}
        <div className="absolute top-1 left-1 flex items-center gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity">
          {multi && (
            <button
              onClick={(e) => {
                e.stopPropagation();
                toggleImageDisplay(row.id, field.id);
              }}
              className="w-5 h-5 flex items-center justify-center rounded-full"
              style={{ background: "rgba(0,0,0,0.6)", color: "#fff" }}
              title={gridMode ? "切换为单图轮播" : "切换为九宫格同显"}
            >
              {gridMode ? <GalleryHorizontal size={11} /> : <LayoutGrid size={11} />}
            </button>
          )}
          <button
            onClick={(e) => {
              e.stopPropagation();
              void addImageToCell(row.id, field.id);
            }}
            className="w-5 h-5 flex items-center justify-center rounded-full"
            style={{ background: "rgba(0,0,0,0.6)", color: "#fff" }}
            title="追加图片"
          >
            <Plus size={11} />
          </button>
        </div>
        {!gridMode && (
          <div className="absolute top-1 right-1 flex items-center gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity">
            <button
              onClick={(e) => {
                e.stopPropagation();
                removeImageAt(row.id, field.id, cur);
              }}
              className="w-5 h-5 flex items-center justify-center rounded-full"
              style={{ background: "rgba(0,0,0,0.6)", color: "#f87171" }}
              title="移除当前图片"
            >
              <X size={11} />
            </button>
          </div>
        )}
      </div>
      {/* 预览 portal 到 body：逃离表格缩放包装层（CSS zoom 会使 fixed 后代被缩放） */}
      {lightbox && lightboxUrls &&
        createPortal(
          <ImageLightbox
            images={lightboxUrls}
            index={cur}
            onIndexChange={commitTo}
            onClose={() => setLightbox(false)}
            onCopyImage={(url) => useTableStore.getState().copyImageToClipboard(url)}
            onDownloadImage={(url) =>
              useTableStore
                .getState()
                .downloadImageToDownloads(`${title || "图片"}-${field.name}-${cur + 1}`, url)
            }
          />,
          document.body,
        )}
    </>
  );
});
