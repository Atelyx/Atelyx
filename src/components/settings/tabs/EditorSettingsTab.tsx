import { useState } from "react";
import { ToggleSwitch } from "@/components/common/ToggleSwitch";
import { SettingCard } from "@/components/settings/SettingCard";
import { SlotListMount } from "@/components/plugins/SlotHost";
import { ConfirmDialog } from "@/components/common/ConfirmDialog";
import { useSettingsStore } from "@/stores/settingsStore";
import { useVaultStore } from "@/stores/vaultStore";

/** 编辑器面板（仓库级）：宽松换行直接订阅 store，重建内部链接流程自持。 */
export function EditorSettingsTab() {
  const vaultConfig = useSettingsStore((s) => s.vaultConfig);
  const setSoftLineBreak = useSettingsStore((s) => s.setSoftLineBreak);
  /** 宽松换行：缺省开启。 */
  const softLineBreak = vaultConfig?.softLineBreak ?? true;

  // 重建内部链接：确认弹窗 / 执行中 / 内联结果
  const [rebuildConfirm, setRebuildConfirm] = useState(false);
  const [rebuilding, setRebuilding] = useState(false);
  const [rebuildState, setRebuildState] = useState<{ message: string; error?: string } | null>(
    null
  );
  const runRebuild = () => {
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
        {/* 宽松换行 */}
        <SettingCard
          title="宽松换行"
          description="单个换行显示为换行；关闭 = 按 Markdown 标准需空行换行"
        >
          <ToggleSwitch
            checked={softLineBreak}
            onChange={(v) => void setSoftLineBreak(v)}
            title="宽松换行"
          />
        </SettingCard>

        {/* 内部链接：一键重建为标准 Markdown 写法（批量改写，需确认） */}
        <SettingCard
          title="内部链接"
          description={
            <span>
              一键统一全仓库笔记的链接为标准 Markdown 写法；批量改写不可撤销。
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
            disabled={rebuilding}
            onClick={() => setRebuildConfirm(true)}
            title="批量改写仓库内全部 .md 的链接写法"
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
