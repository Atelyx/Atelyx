import { useState } from "react";
import { SettingCard } from "@/components/settings/SettingCard";
import { SlotListMount } from "@/components/plugins/SlotHost";
import { ConfirmDialog } from "@/components/common/ConfirmDialog";
import { useSettingsStore, selectVaultSettingsSession } from "@/stores/settingsStore";
import { useVaultStore } from "@/stores/vaultStore";

/** 编辑器面板（仓库级）：重建内部链接流程自持。
 *  经编辑目标选择器取会话——仓库设置弹窗可编辑非激活仓库。
 *  宽松换行/页面内标题是应用级显示偏好（「设置 → 编辑器」，落 global.json），不在此。 */
export function EditorSettingsTab() {
  const session = useSettingsStore(selectVaultSettingsSession);

  // 重建内部链接改写的是**仓库内容**（全仓 .md），依赖激活仓库的扫描与写盘链路：
  // 正在编辑非激活仓库时不可执行（先切换到该仓库）。
  const canRebuild = !session;

  // 重建内部链接：确认弹窗 / 执行中 / 内联结果
  const [rebuildConfirm, setRebuildConfirm] = useState(false);
  const [rebuilding, setRebuilding] = useState(false);
  const [rebuildState, setRebuildState] = useState<{ message: string; error?: string } | null>(
    null
  );
  const runRebuild = () => {
    if (!canRebuild) return;
    setRebuildConfirm(false);
    setRebuilding(true);
    setRebuildState(null);
    void useVaultStore
      .getState()
      .rebuildInternalLinks()
      .then((r) =>
        setRebuildState({
          message: `已扫描 ${r.scanned} 个文件，更新 ${r.modified} 个文件、${r.links} 处链接`,
        })
      )
      .catch((e) => setRebuildState({ message: "", error: `重建失败：${String(e)}` }))
      .finally(() => setRebuilding(false));
  };

  return (
    <>
      <section className="flex-1 p-5 overflow-auto space-y-4">
        {/* 内部链接：一键重建为标准 Markdown 写法（批量改写，需确认；仅对当前激活仓库可用） */}
        <SettingCard
          title="内部链接"
          description={
            <span>
              一键统一全仓库笔记的链接为标准 Markdown 写法；批量改写不可撤销。
              {!canRebuild && (
                <span className="block mt-1">
                  批量改写作用于当前激活仓库：先切换到该仓库再执行。
                </span>
              )}
              {rebuilding && <span className="block mt-1">重建中…</span>}
              {rebuildState && (
                <span
                  className="block mt-1"
                  style={{
                    color: rebuildState.error ? "#f87171" : undefined,
                  }}
                >
                  {rebuildState.error ?? rebuildState.message}
                </span>
              )}
            </span>
          }
        >
          <button
            className="px-3 py-1.5 text-xs rounded border flex-shrink-0 hover:opacity-80 disabled:opacity-50"
            style={{
              borderColor: "#f87171",
              color: "#f87171",
            }}
            disabled={rebuilding || !canRebuild}
            onClick={() => setRebuildConfirm(true)}
            title={canRebuild ? "批量改写仓库内全部 .md 的链接写法" : "先切换到该仓库再执行"}
          >
            重建内部链接
          </button>
        </SettingCard>

        {/* 插件贡献的设置区块（ctx.slots.registerUi 槽名 settings/editor） */}
        <SlotListMount slot="settings/editor" />
      </section>
      {rebuildConfirm && (
        <ConfirmDialog
          title="重建内部链接"
          description="将批量改写仓库内全部 .md 笔记的链接写法，统一为标准 Markdown「[名](基于仓库的路径)」。此操作不可撤销，建议先确认重要笔记已备份！"
          confirmText="开始重建"
          onConfirm={runRebuild}
          onCancel={() => setRebuildConfirm(false)}
        />
      )}
    </>
  );
}
