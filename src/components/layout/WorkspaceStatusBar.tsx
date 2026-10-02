/**
 * 工作区状态栏（窗口底部常驻，只承载「全局/环境」事实）。
 *
 * 只放两件事：当前是哪个仓库/空间（含文件总数）、协作空间通道是否可用（在线设备数）。
 * 不放各视图自己的计数与保存态——画布/表格各有自己的底部条（节点·连线 + 缩放 / 列自动计算），
 * 保存态的唯一出处是面板头 `ViewStatusIndicator`：再放一处就会同时出现两条「保存中…」。
 */
import { Cloud, HardDrive } from "lucide-react";
import { useAppStore } from "@/stores/appStore";
import { useCollabStore } from "@/stores/collabStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { selectVaultFileCount, useVaultStore } from "@/stores/vaultStore";
import { useIsSpaceVault } from "@/hooks/useIsSpaceVault";

export function WorkspaceStatusBar() {
  const vaultName = useAppStore((s) => s.vaultName);
  const hasVault = useAppStore((s) => s.vaultIdentity !== null);
  const isSpace = useIsSpaceVault();
  const fileCount = useVaultStore(selectVaultFileCount);
  // 空间协作：开关关掉时通道本就不会建（`connected` 也为假），故二者分开取，别把「未开启」显示成「未连接」
  const collabEnabled = useSettingsStore((s) => s.collabEnabled);
  const connected = useCollabStore((s) => s.connected);
  // peers 已滤掉自己：加 1 = 含本机的在线设备数，「只有自己在线」时显示 1 而不是 0
  const deviceCount = useCollabStore((s) => s.peers.length) + 1;

  return (
    <div
      className="h-[26px] flex items-center gap-3 px-3 border-t flex-shrink-0 select-none text-[11px]"
      style={{
        background: "var(--bg-secondary)",
        borderColor: "var(--border-subtle)",
        color: "var(--text-muted)",
      }}
    >
      <span className="flex items-center gap-1.5 min-w-0">
        {isSpace ? (
          <Cloud size={12} className="flex-shrink-0" />
        ) : (
          <HardDrive size={12} className="flex-shrink-0" />
        )}
        <span className="truncate" style={{ color: "var(--text-secondary)" }}>
          {vaultName || "未打开仓库"}
        </span>
      </span>
      {hasVault && (
        <>
          <span className="w-px h-3 flex-shrink-0" style={{ background: "var(--border)" }} />
          <span className="font-mono flex-shrink-0">{fileCount} 文件</span>
        </>
      )}
      {isSpace && (
        <>
          <span className="ml-auto w-px h-3 flex-shrink-0" style={{ background: "var(--border)" }} />
          <span
            className="flex items-center gap-1.5 flex-shrink-0 font-mono"
            title={
              !collabEnabled
                ? "多人协作未开启（设置 → 多人协作）"
                : connected
                  ? `协作通道已连接：本空间共 ${deviceCount} 台设备在线`
                  : "协作通道未连接（连接中或已断开）"
            }
          >
            <span
              className="w-[5px] h-[5px] rounded-full flex-shrink-0"
              style={{ background: connected ? "var(--success)" : "var(--border-strong)" }}
            />
            {collabEnabled
              ? connected
                ? `协作中 ${deviceCount} 人`
                : "协作未连接"
              : "协作未开启"}
          </span>
        </>
      )}
    </div>
  );
}
