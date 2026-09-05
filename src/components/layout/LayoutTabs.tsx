/**
 * 标题栏左侧：仓库切换按钮（`VaultSwitcher`，当前仓库名）+ 布局 tab 条（切换/右键/双击重命名/pointer 排序/新建）。
 *
 * - 布局 tab：点击切换；**右键菜单**（重命名 inline / 删除红字，最后一个布局不可删）；
 *   双击重命名保留；**pointer 模拟拖拽排序**（WebView2 HTML5 DnD 不可靠）——
 *   位移超阈值进入拖动（setPointerCapture），松手按落点计算目标位置持久化
 * - 「+」：新建布局（复制当前布局树，命名「布局 N」自动去重）
 */
import { Plus } from "lucide-react";
import { useRef, useState } from "react";
import { useUiStateStore } from "@/stores/uiStateStore";
import { Menu, MenuItem } from "@/components/common/Menu";
import { VaultSwitcher } from "@/components/layout/VaultSwitcher";
import { HOME_LAYOUT_ID } from "@/types";

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

  // ===== pointer 拖拽排序 =====
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
      {/* 仓库切换按钮（点击弹出已添加仓库列表 + 管理仓库） */}
      <VaultSwitcher />

      <div className="flex items-stretch gap-1" data-tauri-drag-region>
        {layouts.map((l, index) => {
          const active = l.id === activeLayoutId;
          const dragging = dragOffset !== null && dragRef.current?.index === index;
          const isHome = l.id === HOME_LAYOUT_ID;
          return (
            <div
              key={l.id}
              data-layout-tab
              className="group flex items-center rounded-t-md text-xs min-w-0 flex-shrink-0"
              style={{
                // 激活 tab 背景与编辑区同色（bg-primary）+ 同色底边框「顶开」标题栏底边线 → 与面板网格粘连
                background: active ? "var(--bg-primary)" : "transparent",
                borderBottom: active ? "1px solid var(--bg-primary)" : undefined,
                color: active ? "var(--text-primary)" : "var(--text-muted)",
                // 拖动中：跟随水平位移 + 阴影提示，其他 tab 原位等待
                transform: dragging && dragOffset !== null ? `translateX(${dragOffset}px)` : undefined,
                opacity: dragging ? 0.85 : undefined,
                transition: dragging ? "none" : "transform 120ms ease",
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
              {editingId === l.id ? (
                <input
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
                  className="pl-3 pr-1 py-0.5 bg-transparent border-b border-[var(--accent)] outline-none text-xs min-w-0"
                  style={{ color: "var(--text-primary)" }}
                  data-tauri-drag-region="false"
                />
              ) : (
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
                  title={
                    isHome
                      ? "主页（固定布局：不可删除/排序/重命名，面板可调整）"
                      : `${l.name}（点击切换 / 双击重命名 / 右键菜单 / 拖拽排序）`
                  }
                  data-tauri-drag-region="false"
                >
                  {l.name}
                </button>
              )}
            </div>
          );
        })}

        <button
          onClick={addLayout}
          className="flex-shrink-0 px-1.5 hover:opacity-80"
          title="新建布局（复制当前布局）"
          aria-label="新建布局"
          style={{ color: "var(--text-secondary)" }}
          data-tauri-drag-region="false"
        >
          <Plus size={13} />
        </button>
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
          {layouts.length > 1 && (
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
        </Menu>
      )}
    </div>
  );
}
