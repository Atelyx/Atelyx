/**
 * 图片放大预览（Lightbox）。
 *
 * 表格图片单元格点击缩略图弹出：全屏遮罩 + 居中大图（保持比例）+ 左右切换（循环）+
 * Esc/点击遮罩关闭。纯视觉组件，图片 dataURL 由调用方传入，无额外加载。
 *
 * 右键预览内任意处（图或遮罩）弹上下文菜单：复制图片（系统剪贴板）/ 下载图片（系统 Downloads 文件夹）——
 * 动作经 `onCopyImage`/`onDownloadImage` 回调（返回 Promise<boolean> 是否成功），
 * 结果在遮罩内底部提示（成功绿/失败红；全屏遮罩盖住面板 header，提示须就地可见）。
 * 菜单打开时 Esc 只关菜单不连关预览；左右切换图片自动关闭菜单（防菜单停留在旧图位置）。
 *
 * 保留原生 button 的三处（脱离基元的原因）：关闭（36px）与左右切换（40px）——灯箱是深色全屏
 * 遮罩上的大尺寸指针控件，边长落在 `lg`(32) 与 `touch`(44) 两档之间，归不进 `IconButton` 的
 * 正方形档位；且 hover 是 `white/10` 的浅色反馈、非主题色。三处只以 `Tooltip` 取代原生 `title`
 * 统一提示形态，可访问名由 `aria-label` 给出。
 */
import { Copy, Download, ChevronLeft, ChevronRight, X } from "lucide-react";
import { useEffect, useState } from "react";
import type { MouseEvent } from "react";
import { Menu, MenuItem } from "@/components/common/Menu";
import { Tooltip } from "@/components/common/Tooltip";

interface Props {
  /** 图片 dataURL 数组（当前单元格的全部图片）。 */
  images: string[];
  /** 初始显示下标。 */
  index: number;
  /** 切换图片（受控：父级更新 index）。 */
  onIndexChange: (i: number) => void;
  onClose: () => void;
  /** 右键「复制图片」：返回是否成功。 */
  onCopyImage: (dataUrl: string) => Promise<boolean>;
  /** 右键「下载图片」：返回是否成功。 */
  onDownloadImage: (dataUrl: string) => Promise<boolean>;
}

/** 操作结果提示（成功绿 / 失败红），定时自动消失。 */
interface Notice {
  text: string;
  kind: "success" | "error";
}

/** 提示自动消失时长。 */
const NOTICE_MS = 2500;

export function ImageLightbox({
  images,
  index,
  onIndexChange,
  onClose,
  onCopyImage,
  onDownloadImage,
}: Props) {
  const count = images.length;
  /** 右键菜单锚点（null = 关闭）。 */
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  /** 复制/下载结果提示（就地可见，定时自动清除）。 */
  const [notice, setNotice] = useState<Notice | null>(null);
  useEffect(() => {
    if (!notice) return;
    const t = setTimeout(() => setNotice(null), NOTICE_MS);
    return () => clearTimeout(t);
  }, [notice]);

  // Esc 关闭（监听挂载期间；菜单打开时 Esc 只关菜单，不连关预览）
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        if (menu) return;
        onClose();
      }
      if (e.key === "ArrowLeft" && count > 1) changeIndex((index - 1 + count) % count);
      if (e.key === "ArrowRight" && count > 1) changeIndex((index + 1) % count);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [onClose, index, count, menu]);

  /** 切换图片：受控转发 + 自动关闭右键菜单（防菜单停留在已换图片的旧位置）。 */
  const changeIndex = (i: number) => {
    setMenu(null);
    onIndexChange(i);
  };

  /** 执行菜单动作：关菜单 → await 回调 → 结果就地提示（成功/失败）。 */
  const runAction = async (
    action: (url: string) => Promise<boolean>,
    url: string,
    okMsg: string,
    failMsg: string,
  ) => {
    setMenu(null);
    const ok = await action(url);
    setNotice(ok ? { text: okMsg, kind: "success" } : { text: failMsg, kind: "error" });
  };

  // 挂在遮罩根：img 右键冒泡复用；遮罩空白处右键也弹菜单（不露浏览器默认菜单）
  const handleContextMenu = (e: MouseEvent<HTMLDivElement>) => {
    e.preventDefault();
    setMenu({ x: e.clientX, y: e.clientY });
  };

  return (
    <div
      data-lightbox
      className="fixed inset-0 z-[100] flex items-center justify-center"
      style={{ background: "rgba(0,0,0,0.85)" }}
      onClick={onClose}
      onContextMenu={handleContextMenu}
    >
      <img
        src={images[index]}
        alt="放大预览"
        className="max-w-[90vw] max-h-[90vh] object-contain select-none"
        draggable={false}
        onClick={(e) => e.stopPropagation()}
      />
      {/* 顶部关闭 */}
      <Tooltip content="关闭 (Esc)">
        <button
          onClick={onClose}
          className="absolute top-4 right-4 w-9 h-9 flex items-center justify-center rounded hover:bg-white/10"
          style={{ color: "var(--text-primary)" }}
          aria-label="关闭"
        >
          <X size={20} />
        </button>
      </Tooltip>
      {/* 左右切换（仅多图时显示；循环） */}
      {count > 1 && (
        <>
          <Tooltip content="上一张 (←)">
            <button
              onClick={(e) => {
                e.stopPropagation();
                changeIndex((index - 1 + count) % count);
              }}
              className="absolute left-4 top-1/2 -translate-y-1/2 w-10 h-10 flex items-center justify-center rounded-full hover:bg-white/10"
              style={{ color: "var(--text-primary)" }}
              aria-label="上一张"
            >
              <ChevronLeft size={24} />
            </button>
          </Tooltip>
          <Tooltip content="下一张 (→)">
            <button
              onClick={(e) => {
                e.stopPropagation();
                changeIndex((index + 1) % count);
              }}
              className="absolute right-4 top-1/2 -translate-y-1/2 w-10 h-10 flex items-center justify-center rounded-full hover:bg-white/10"
              style={{ color: "var(--text-primary)" }}
              aria-label="下一张"
            >
              <ChevronRight size={24} />
            </button>
          </Tooltip>
          <div
            className="absolute bottom-4 left-1/2 -translate-x-1/2 px-2 py-0.5 rounded-full text-xs"
            style={{ background: "rgba(0,0,0,0.6)", color: "var(--text-primary)" }}
          >
            {index + 1} / {count}
          </div>
        </>
      )}
      {/* 右键菜单：z-[110] 高过遮罩 z-[100]（全屏遮罩盖住面板 header，错误须就地可见） */}
      {menu && (
        <Menu x={menu.x} y={menu.y} onClose={() => setMenu(null)} zClass="z-[110]" widthClass="w-40">
          <MenuItem
            onClick={() =>
              void runAction(onCopyImage, images[index], "已复制到剪贴板", "复制图片失败，请重试")
            }
          >
            <span className="inline-flex items-center gap-1.5">
              <Copy size={14} />
              复制图片
            </span>
          </MenuItem>
          <MenuItem
            onClick={() =>
              void runAction(onDownloadImage, images[index], "已保存到 Downloads", "下载图片失败，请重试")
            }
          >
            <span className="inline-flex items-center gap-1.5">
              <Download size={14} />
              下载图片
            </span>
          </MenuItem>
        </Menu>
      )}
      {/* 操作结果提示（底部居中，成功绿/失败红，定时消失） */}
      {notice && (
        <div
          className="absolute bottom-8 left-1/2 -translate-x-1/2 px-3 py-1.5 rounded text-xs"
          style={{
            background: "rgba(0,0,0,0.75)",
            color: notice.kind === "success" ? "#4ade80" : "#f87171",
          }}
        >
          {notice.text}
        </div>
      )}
    </div>
  );
}
