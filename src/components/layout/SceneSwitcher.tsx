/**
 * 标题栏右上角场景选择器：场景（布局之上的容器）的切换入口，布局切换在左侧布局 tab 条。
 *
 * 菜单：点击切换场景（整组替换面板网格并恢复该场景记忆的布局）、行内重命名、
 * 删除走确认面板（场景内布局一并删除，不可撤销）、底部新建场景。
 * 默认场景固定置顶、不可删除/重命名。
 * 全部变更经 uiStateStore 发命令，Rust 是唯一权威，本组件只渲染镜像。
 */
import { Check, ChevronDown, Pencil, Plus, Trash2 } from "lucide-react";
import { useRef, useState } from "react";
import { useUiStateStore } from "@/stores/uiStateStore";
import { Input } from "@/components/common/Input";
import { PopupLayer } from "@/components/common/PopupLayer";
import { usePopupAnchor } from "@/hooks/usePopupAnchor";
import { MenuItem } from "@/components/common/Menu";
import { DEFAULT_SCENE_ID } from "@/types";

/** 菜单面板：root = 场景列表；confirmDelete = 场景删除确认。 */
type Pane = "root" | { kind: "confirmDelete"; sceneId: string };

export function SceneSwitcher() {
  const scenes = useUiStateStore((s) => s.scenes);
  const activeSceneId = useUiStateStore((s) => s.activeSceneId);
  const activateScene = useUiStateStore((s) => s.activateScene);
  const addScene = useUiStateStore((s) => s.addScene);
  const renameScene = useUiStateStore((s) => s.renameScene);
  const deleteScene = useUiStateStore((s) => s.deleteScene);

  const triggerRef = useRef<HTMLButtonElement>(null);
  const { anchor, toggle, close } = usePopupAnchor(triggerRef);
  const [pane, setPane] = useState<Pane>("root");
  // 场景行内重命名（Escape 取消标记：拦截 input 卸载触发的 blur 误提交）
  const [editingId, setEditingId] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const cancelRef = useRef(false);

  const activeScene = scenes.find((s) => s.id === activeSceneId)!;

  const open = () => {
    if (!anchor) {
      setPane("root");
      setEditingId(null);
    }
    toggle();
  };

  const beginRename = (id: string, name: string) => {
    cancelRef.current = false;
    setDraft(name);
    setEditingId(id);
  };

  const commitRename = () => {
    if (cancelRef.current) {
      cancelRef.current = false;
      setEditingId(null);
      return;
    }
    if (editingId) renameScene(editingId, draft);
    setEditingId(null);
  };

  /** 场景行尾部的重命名/删除小按钮（默认场景无；统一 hover 显形由行容器 group 控制）。 */
  const rowAction = (icon: React.ReactNode, title: string, onClick: () => void, danger = false) => (
    <button
      onClick={(e) => {
        e.stopPropagation();
        onClick();
      }}
      className="flex-shrink-0 rounded p-0.5 opacity-0 group-hover/row:opacity-100 hover:bg-[var(--hover)]"
      style={{ color: danger ? "var(--danger)" : "var(--text-muted)" }}
      title={title}
      aria-label={title}
      data-tauri-drag-region="false"
    >
      {icon}
    </button>
  );

  // 场景行：点击切场景（重命名中不响应）；行尾重命名/删除。默认场景固定不可删/不可重命名。
  const sceneRow = (id: string, name: string) => {
    const active = id === activeSceneId;
    if (editingId === id) {
      return (
        <div key={id} className="px-2 py-0.5">
          <Input
            borderless
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onBlur={commitRename}
            onKeyDown={(e) => {
              if (e.key === "Enter") commitRename();
              if (e.key === "Escape") {
                cancelRef.current = true;
                setEditingId(null);
              }
            }}
            autoFocus
            className="!text-xs py-0.5"
            style={{ color: "var(--text-primary)" }}
            data-tauri-drag-region="false"
          />
        </div>
      );
    }
    return (
      <div key={id} className="group/row flex items-center gap-0.5 pl-1 pr-1">
        <button
          onClick={() => {
            activateScene(id);
            close();
          }}
          className="flex-1 min-w-0 text-left px-2 py-1.5 text-xs inline-flex items-center gap-1.5 hover:bg-[var(--hover)] rounded"
          style={{ color: active ? "var(--accent)" : "var(--text-primary)" }}
          title={id === DEFAULT_SCENE_ID ? "固定场景：不可删除或重命名" : name}
          data-tauri-drag-region="false"
        >
          {name}
          {active && <Check size={12} className="flex-shrink-0" />}
        </button>
        {id !== DEFAULT_SCENE_ID && (
          <>
            {rowAction(<Pencil size={11} />, "重命名场景", () => beginRename(id, name))}
            {rowAction(
              <Trash2 size={11} />,
              "删除场景",
              () => setPane({ kind: "confirmDelete", sceneId: id }),
              true,
            )}
          </>
        )}
      </div>
    );
  };

  const confirmTarget =
    typeof pane === "object" ? scenes.find((s) => s.id === pane.sceneId) : null;

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        onClick={open}
        className="flex items-center gap-1 h-7 px-2 rounded-sm text-xs hover:bg-[var(--bg-tertiary)] flex-shrink-0"
        style={{ color: "var(--text-secondary)" }}
        title="切换场景"
        aria-haspopup="menu"
        aria-expanded={!!anchor}
        data-tauri-drag-region="false"
      >
        <span className="max-w-[120px] truncate">{activeScene.name}</span>
        <ChevronDown size={12} className="flex-shrink-0" />
      </button>
      <PopupLayer
        anchor={anchor}
        onClose={close}
        triggerRef={triggerRef}
        widthClass="w-52"
        repositionDeps={[pane, editingId, scenes.length]}
      >
        {confirmTarget ? (
          <div className="p-3" data-tauri-drag-region="false">
            <p className="text-xs font-medium" style={{ color: "var(--text-primary)" }}>
              删除场景「{confirmTarget.name}」？
            </p>
            <p className="text-xs mt-1" style={{ color: "var(--text-muted)" }}>
              其下 {confirmTarget.layouts.length} 套布局将一并删除。
            </p>
            <div className="flex justify-end gap-2 mt-3">
              <button
                onClick={() => setPane("root")}
                className="px-2.5 py-1 text-xs rounded border hover:opacity-80"
                style={{ borderColor: "var(--border)", color: "var(--text-secondary)" }}
                data-tauri-drag-region="false"
              >
                取消
              </button>
              <button
                onClick={() => {
                  deleteScene(confirmTarget.id);
                  setPane("root");
                  close();
                }}
                className="px-2.5 py-1 text-xs rounded border hover:opacity-80"
                style={{ borderColor: "var(--danger)", color: "var(--danger)" }}
                data-tauri-drag-region="false"
              >
                删除
              </button>
            </div>
          </div>
        ) : (
          <>
            <div
              className="px-3 pt-1.5 pb-0.5 text-micro select-none"
              style={{ color: "var(--text-muted)" }}
            >
              场景
            </div>
            {scenes.map((s) => sceneRow(s.id, s.name))}
            <MenuItem
              onClick={() => {
                addScene();
                close();
              }}
              className="text-xs"
            >
              <Plus size={12} />
              新建场景
            </MenuItem>
          </>
        )}
      </PopupLayer>
    </>
  );
}
