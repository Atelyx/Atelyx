/**
 * 标题栏仓库切换按钮（工作区左上角，显示当前仓库名）。
 * 点击向下弹出菜单：
 * - 已添加（最近打开）仓库列表：点击切换（当前仓库高亮禁用）
 * - 分隔线下方「管理仓库」→ 返回启动页（VaultSelectPage）
 * 弹层 = `PopupLayer` 统一壳（向下弹出 + 下方空间不足向上翻转，minWidth = 按钮宽）。
 * 切换仓库期间工作区被全屏加载屏覆盖（appStore.entryLoading），无重复切换入口；
 * 点击当前仓库由 root === vaultRoot 守卫拦截。
 * 分层：只读 appStore + 调 selectVault / backToVaultSelect（store 内完成 openVault/watcher/登记）。
 */
import { Check, ChevronDown, Library } from "lucide-react";
import { useRef } from "react";
import { useAppStore } from "@/stores/appStore";
import { PopupLayer } from "@/components/common/PopupLayer";
import { usePopupAnchor } from "@/hooks/usePopupAnchor";

export function VaultSwitcher() {
  const recentVaults = useAppStore((s) => s.recentVaults);
  const vaultRoot = useAppStore((s) => s.vaultRoot);
  const vaultName = useAppStore((s) => s.vaultName);
  const selectVault = useAppStore((s) => s.selectVault);
  const backToVaultSelect = useAppStore((s) => s.backToVaultSelect);

  const barRef = useRef<HTMLButtonElement>(null);
  const { anchor, toggle, close } = usePopupAnchor(barRef);

  const switchTo = async (root: string) => {
    close();
    // 点击当前仓库 = no-op（防重复进入；切换中由全屏加载屏兜底）
    if (root === vaultRoot) return;
    await selectVault(root);
  };

  return (
    <>
      <button
        ref={barRef}
        onClick={toggle}
        className="flex items-center gap-1.5 px-2 h-8 rounded-md hover:bg-[var(--hover)] flex-shrink-0 min-w-0"
        style={{ color: "var(--text-secondary)" }}
        title="切换仓库"
        data-tauri-drag-region="false"
      >
        <Library size={13} className="flex-shrink-0" />
        <span
          className="flex-1 min-w-0 truncate text-xs text-left max-w-[140px]"
          style={{ color: "var(--text-primary)" }}
        >
          {vaultName}
        </span>
        <ChevronDown size={12} className="flex-shrink-0" />
      </button>

      <PopupLayer anchor={anchor} onClose={close} triggerRef={barRef}>
        {/* 已添加仓库列表（当前仓库高亮禁用）；max-w 限制长路径撑宽，行内 truncate 截断 */}
        <div className="max-h-[40vh] overflow-y-auto py-0.5 max-w-[380px]">
          {recentVaults.length === 0 ? (
            <p className="px-3 py-2 text-xs" style={{ color: "var(--text-muted)" }}>
              暂无已添加的仓库
            </p>
          ) : (
            recentVaults.map((v) => {
              const current = v.root === vaultRoot;
              return (
                <button
                  key={v.root}
                  onClick={() => void switchTo(v.root)}
                  disabled={current}
                  title={v.root}
                  className="w-full text-left px-3 py-1.5 hover:bg-[var(--hover)] disabled:hover:bg-transparent disabled:cursor-default"
                >
                  <span
                    className="block text-[13px] truncate"
                    style={{ color: current ? "var(--accent)" : "var(--text-primary)" }}
                  >
                    {v.name}
                    {current && <Check size={12} className="ml-1 inline" />}
                  </span>
                  <span className="block text-[11px] mt-0.5 truncate" style={{ color: "var(--text-muted)" }}>
                    {v.root}
                  </span>
                </button>
              );
            })
          )}
        </div>

        <div className="mx-2 my-1" style={{ borderTop: "1px solid var(--border)" }} />

        <button
          onClick={() => {
            close();
            backToVaultSelect();
          }}
          className="w-full text-left px-3 py-1.5 text-[13px] hover:bg-[var(--hover)]"
          style={{ color: "var(--text-primary)" }}
        >
          管理仓库
        </button>
      </PopupLayer>
    </>
  );
}
