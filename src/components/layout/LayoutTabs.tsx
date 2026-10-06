/**
 * 标题栏左侧：应用标识（图标 + 软件名，纯展示）+ 布局 tab 条（切换/右键/双击重命名/pointer 排序/新建）。
 * tab 交互细节见各处理函数；「+」新建布局 = 单个空面板占位，命名「布局 N」自动去重。
 */
import { Plus } from "lucide-react";
import { useRef, useState } from "react";
import { useUiStateStore } from "@/stores/uiStateStore";
import { Input } from "@/components/common/Input";
import { IconButton } from "@/components/common/Button";
import { Menu, MenuItem } from "@/components/common/Menu";
import { Tooltip } from "@/components/common/Tooltip";
import { MenuSlotList } from "@/components/plugins/MenuSlot";
import { HOME_LAYOUT_ID } from "@/types";
import appIcon from "@/assets/icon.svg";

/** 拖拽判定阈值（px）：低于视为点击，不进入拖动模式。 */
const DRAG_THRESHOLD = 4;

export function LayoutTabs() {
  const layouts = useUiStateStore((s) => s.workspaceLayouts);
  const activeLayoutId = useUiStateStore((s) => s.activeLayoutId);
  const activateLayout = useUiStateStore((s) => s.activateLayout);
  const addLayout = useUiStateStore((s) => s.addLayout);
  const renameLayout = useUiStateStore((s) => s.renameLayout);
  const deleteLayout = useUiStateStore((s) => s.deleteLayout);
  const moveLayout = useUiStateStore((s) => s.moveLayout);

  const [editingId, setEditingId] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  // Escape 取消重命名标记：拦截 input 卸载触发的 blur 误提交
  const cancelRef = useRef(false);

  // 右键菜单（重命名 / 删除）
  const [menu, setMenu] = useState<{ id: string; x: number; y: number } | null>(null);
  const closeMenu = () => setMenu(null);

  // ===== pointer 拖拽排序（不用 HTML5 DnD：WebView2 下不可靠）=====
  /** 位移超阈值进入拖动（setPointerCapture），松手按落点计算目标位置持久化。 */
  /** 拖动会话（pointerdown 时建立；moved = 是否越过阈值进入拖动）。 */
  const dragRef = useRef<{ index: number; startX: number; moved: boolean } | null>(null);
  /** 拖动视觉：被拖 tab 的水平位移（null = 未拖动）。 */
  const [dragOffset, setDragOffset] = useState<number | null>(null);
  /** 拖动结束后抑制本次点击的 tab 激活（click 在 pointerup 后触发）。 */
  const suppressClickRef = useRef(false);

  const onTabPointerDown = (e: React.PointerEvent, index: number) => {
    if (e.button !== 0) return;
    if (menu || editingId) return; // 菜单/重命名打开时不拖
    // 主页布局固定置顶：不可拖拽排序
    if (layouts[index]?.id === HOME_LAYOUT_ID) return;
    dragRef.current = { index, startX: e.clientX, moved: false };
    // 不在 pointerdown 时 capture：WebView2 中 pointerdown 即 setPointerCapture 会吞掉
    // 后续 click（单击切换/双击重命名失效）；只有真正进入拖动（位移超阈值）才 capture
  };

  const onTabPointerMove = (e: React.PointerEvent, index: number) => {
    const d = dragRef.current;
    if (!d || d.index !== index) return;
    const dx = e.clientX - d.startX;
    if (!d.moved && Math.abs(dx) > DRAG_THRESHOLD) {
      // 进入拖动：开始捕获（后续 move/up 发给本元素，鼠标移出仍可跟踪）
      d.moved = true;
      (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
      suppressClickRef.current = true;
      setDragOffset(dx);
      return;
    }
    if (d.moved) setDragOffset(dx);
  };

  const onTabPointerUp = (e: React.PointerEvent, index: number) => {
    const d = dragRef.current;
    dragRef.current = null;
    if (!d || !d.moved) return;
    setDragOffset(null);
    // 落位：被拖 tab 移除后，拖动中心越过几个剩余 tab 的中点就插到其后。
    // 只数 tab（data-layout-tab），「+」按钮不算落点——否则拖到最右端 to = layouts.length 越界
    const container = (e.currentTarget as HTMLElement).parentElement;
    if (!container) return;
    const rects = Array.from(container.querySelectorAll("[data-layout-tab]")).map((el) =>
      el.getBoundingClientRect()
    );
    // 拖动中心 = 原位中心 + 本次位移
    const draggedCenter = rects[index].left + rects[index].width / 2 + (e.clientX - d.startX);
    let to = 0;
    rects.forEach((r, i) => {
      if (i === index) return;
      if (draggedCenter > r.left + r.width / 2) to++;
    });
    moveLayout(index, to);
  };

  const onTabPointerCancel = () => {
    dragRef.current = null;
    setDragOffset(null);
    // 取消的拖拽不会派发 click，残留的抑制标记会误吞下一次单击
    suppressClickRef.current = false;
  };

  return (
    <div className="flex items-center gap-1 h-full flex-shrink-0 select-none" data-tauri-drag-region>
      {/* 应用标识（纯展示，不可点） */}
      <div className="flex items-center gap-1.5 px-2 h-8 flex-shrink-0" data-tauri-drag-region>
        <img src={appIcon} alt="" draggable={false} className="w-4 h-4 rounded select-none" />
        <span
          className="text-xs font-semibold tracking-wide"
          style={{ color: "var(--text-secondary)", fontFamily: "var(--font-display)" }}
        >
          Atelyx
        </span>
      </div>

      <div className="flex items-stretch gap-1" data-tauri-drag-region>
        {layouts.map((l, index) => {
          const active = l.id === activeLayoutId;
          const editing = editingId === l.id;
          const dragging = dragOffset !== null && dragRef.current?.index === index;
          const isHome = l.id === HOME_LAYOUT_ID;
          return (
            <div
              key={l.id}
              data-layout-tab
              className="group relative flex items-center h-7 rounded-sm text-xs min-w-0 flex-shrink-0 hover:bg-[var(--bg-tertiary)]"
              style={{
                // 弱层级：激活只抬一档底色 + 2px 强调下边（见下方条），不染强调底（重命名中同款高亮）；
                // 未激活不设内联底色，交给 hover 类，否则内联样式会压过 hover
                background: active || editing ? "var(--bg-tertiary)" : undefined,
                color: active ? "var(--text-primary)" : "var(--text-secondary)",
                // 拖动中：跟随水平位移 + 阴影提示，其他 tab 原位等待
                transform: dragging && dragOffset !== null ? `translateX(${dragOffset}px)` : undefined,
                opacity: dragging ? 0.85 : undefined,
                transition: dragging
                  ? "none"
                  : "transform 120ms ease, background-color 120ms ease, color 120ms ease",
                zIndex: dragging ? 10 : undefined,
              }}
              // 禁窗口拖动：tab 需独占 pointer 事件做排序拖拽
              data-tauri-drag-region="false"
              onClick={() => {
                // 拖动结束的 click 派发在捕获元素（tab 容器）上，button 的 onClick 不触发——
                // 在此消费抑制标记（与 button 内消费幂等，防残留误抑制下一次单击）
                if (suppressClickRef.current) suppressClickRef.current = false;
              }}
              onPointerDown={(e) => onTabPointerDown(e, index)}
              onPointerMove={(e) => onTabPointerMove(e, index)}
              onPointerUp={(e) => onTabPointerUp(e, index)}
              onPointerCancel={onTabPointerCancel}
              onContextMenu={(e) => {
                // 主页布局固定：不可重命名/删除/排序，不弹右键菜单
                if (isHome) return;
                e.preventDefault();
                e.stopPropagation();
                setMenu({ id: l.id, x: e.clientX, y: e.clientY });
              }}
            >
              {editing ? (
                <Input
                  borderless
                  value={draft}
                  onChange={(e) => setDraft(e.target.value)}
                  onBlur={() => {
                    if (cancelRef.current) {
                      cancelRef.current = false;
                      return;
                    }
                    renameLayout(l.id, draft);
                    setEditingId(null);
                  }}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      renameLayout(l.id, draft);
                      setEditingId(null);
                    }
                    if (e.key === "Escape") {
                      cancelRef.current = true;
                      setEditingId(null);
                    }
                  }}
                  autoFocus
                  className="!text-xs py-0.5 min-w-0"
                  style={{ paddingLeft: "0.75rem", color: "var(--text-primary)" }}
                  data-tauri-drag-region="false"
                />
              ) : (
                <Tooltip
                  content={
                    isHome
                      ? "固定布局：不可删除或重命名"
                      : "点击切换 · 双击重命名 · 右键更多"
                  }
                  placement="bottom"
                >
                  <button
                    onClick={() => {
                      // 拖拽结束的 pointerup 会紧随触发 click，抑制这次激活
                      if (suppressClickRef.current) {
                        suppressClickRef.current = false;
                        return;
                      }
                      activateLayout(l.id);
                    }}
                    onDoubleClick={() => {
                      // 主页布局固定：不可重命名
                      if (isHome) return;
                      cancelRef.current = false;
                      setDraft(l.name);
                      setEditingId(l.id);
                    }}
                    className="pl-3 pr-2 py-0.5 truncate max-w-[120px] min-w-[48px] text-left"
                    data-tauri-drag-region="false"
                  >
                    {l.name}
                  </button>
                </Tooltip>
              )}
              {/* 激活下边：走 --accent-grad 的短条（box-shadow 不能铺渐变，故用元素画） */}
              {(active || editing) && (
                <span
                  aria-hidden
                  className="absolute left-2 right-2 -bottom-px h-0.5 rounded-full pointer-events-none"
                  style={{ background: "var(--accent-grad)" }}
                />
              )}
            </div>
          );
        })}

        <IconButton
          icon={<Plus size={13} />}
          label="新建布局"
          onClick={addLayout}
          variant="subtle"
          size="md"
          className="hover:!bg-[var(--bg-tertiary)]"
          data-tauri-drag-region="false"
        />
      </div>

      {/* tab 右键菜单：重命名 / 删除 */}
      {menu && (
        <Menu x={menu.x} y={menu.y} onClose={closeMenu} widthClass="w-36">
          <MenuItem
            onClick={() => {
              cancelRef.current = false;
              setDraft(layouts.find((l) => l.id === menu.id)?.name ?? "");
              setEditingId(menu.id);
              closeMenu();
            }}
            className="text-xs"
          >
            重命名
          </MenuItem>
          {/* 场景内至少保留 1 个布局：合成序含主页（首位），可删前提 = 布局列表 ≥ 2 */}
          {layouts.length > 2 && (
            <MenuItem
              onClick={() => {
                deleteLayout(menu.id);
                closeMenu();
              }}
              danger
              className="text-xs"
            >
              删除
            </MenuItem>
          )}
          {/* 插件贡献区：布局标签右键菜单（list 槽，priority 降序） */}
          <MenuSlotList target="layout-tab" />
        </Menu>
      )}
    </div>
  );
}
