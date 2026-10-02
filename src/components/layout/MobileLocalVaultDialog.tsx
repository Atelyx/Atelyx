/**
 * 移动端本地仓库入口：首次启动引导与「添加本地仓库」共用（`firstRun` 区分）。
 *
 * 安卓没有系统文件夹选择器，且访问设备任意文件夹需要「所有文件访问权限」（无法弹窗申请，
 * 只能引导用户去系统设置页手动开启）。未开启时回落应用内部私有目录：功能完整，但其他应用
 * 与电脑都看不到，无法导出——这一点必须在界面上明示。
 * 编排在 mobileVaultStore（组件层不 import services）。
 */
import { useEffect } from "react";
import { FolderOpen, HardDrive, Loader2, ShieldCheck, X } from "lucide-react";
import { MobileDirectoryBrowser } from "@/components/layout/MobileDirectoryBrowser";
import { useMobileVaultStore } from "@/stores/mobileVaultStore";
import { useBackHandler } from "@/hooks/useBackHandler";

export function MobileLocalVaultDialog({
  firstRun = false,
  onClose,
}: {
  /** 首次启动引导态：多一个「稍后再说」入口。 */
  firstRun?: boolean;
  onClose: () => void;
}) {
  const granted = useMobileVaultStore((s) => s.allFilesAccess);
  const browsing = useMobileVaultStore((s) => s.browsing);
  const listing = useMobileVaultStore((s) => s.listing);
  const listingLoading = useMobileVaultStore((s) => s.listingLoading);
  const error = useMobileVaultStore((s) => s.error);
  const busy = useMobileVaultStore((s) => s.busy);
  const refreshPermission = useMobileVaultStore((s) => s.refreshPermission);
  const requestPermission = useMobileVaultStore((s) => s.requestPermission);
  const openBrowser = useMobileVaultStore((s) => s.openBrowser);
  const enterDir = useMobileVaultStore((s) => s.enterDir);
  const closeBrowser = useMobileVaultStore((s) => s.closeBrowser);
  const openVaultAt = useMobileVaultStore((s) => s.openVaultAt);
  const openPrivateVault = useMobileVaultStore((s) => s.openPrivateVault);

  useEffect(() => {
    void refreshPermission();
  }, [refreshPermission]);

  // 关闭即清浏览态与错误：下次打开从入口态开始（不残留上次的目录或错误）
  useEffect(() => () => useMobileVaultStore.getState().reset(), []);

  // 用户去系统设置里开完开关返回应用时重查（切回前台即刷新，无需手动重进对话框）
  useEffect(() => {
    const onVisible = () => {
      if (!document.hidden) void refreshPermission();
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => document.removeEventListener("visibilitychange", onVisible);
  }, [refreshPermission]);

  useBackHandler(!browsing, () => {
    onClose();
    return true;
  });

  const pick = async (path: string) => {
    if (await openVaultAt(path)) onClose();
  };

  const enterPrivateVault = async () => {
    if (await openPrivateVault()) onClose();
  };

  if (browsing) {
    return (
      <MobileDirectoryBrowser
        listing={listing}
        loading={listingLoading}
        error={error}
        onEnter={(path) => void enterDir(path)}
        onPick={(path) => void pick(path)}
        onClose={() => closeBrowser()}
      />
    );
  }

  return (
    <div
      className="fixed inset-0 z-[185] flex items-center justify-center p-4"
      style={{ background: "var(--scrim)" }}
      role="dialog"
      aria-modal="true"
      aria-label="本地仓库"
    >
      <div
        className="w-[min(28rem,100%)] rounded-lg border p-4"
        style={{ background: "var(--bg-secondary)", borderColor: "var(--border)" }}
      >
        <div className="flex items-start gap-2">
          <h3 className="flex-1 text-sm font-medium" style={{ color: "var(--text-primary)" }}>
            {firstRun ? "本地仓库" : "添加本地仓库"}
          </h3>
          <button
            onClick={onClose}
            className="p-1 rounded"
            style={{ color: "var(--text-muted)" }}
            aria-label="关闭"
          >
            <X size={15} />
          </button>
        </div>

        <p className="mt-2 text-xs leading-relaxed" style={{ color: "var(--text-secondary)" }}>
          本地仓库是一个存放笔记、表格与画布的文件夹。要选择设备上的任意文件夹，需要先在系统设置里开启
          「所有文件访问权限」——该权限无法弹窗申请，只能手动打开开关。
        </p>

        <div className="mt-3 flex items-center gap-1.5 text-xs" style={{ color: "var(--text-muted)" }}>
          {granted === null ? (
            <>
              <Loader2 size={12} className="animate-spin" /> 正在检查权限…
            </>
          ) : granted ? (
            <>
              <ShieldCheck size={12} style={{ color: "var(--success)" }} /> 已开启「所有文件访问权限」
            </>
          ) : (
            <>未开启「所有文件访问权限」</>
          )}
        </div>

        <div className="mt-3 flex flex-col gap-2">
          {granted === true && (
            <button
              onClick={() => void openBrowser()}
              disabled={busy}
              className="flex items-center justify-center gap-1.5 h-10 rounded text-sm font-medium disabled:opacity-60"
              style={{ background: "var(--accent)", color: "var(--accent-fg)" }}
            >
              <FolderOpen size={14} /> 从设备选择文件夹
            </button>
          )}
          {granted === false && (
            <>
              <button
                onClick={() => void requestPermission()}
                className="flex items-center justify-center gap-1.5 h-10 rounded text-sm font-medium"
                style={{ background: "var(--accent)", color: "var(--accent-fg)" }}
              >
                <ShieldCheck size={14} /> 开启所有文件访问权限
              </button>
              <button
                onClick={() => void enterPrivateVault()}
                disabled={busy}
                className="flex items-center justify-center gap-1.5 h-10 rounded text-sm border disabled:opacity-60"
                style={{ borderColor: "var(--border)", color: "var(--text-secondary)" }}
              >
                <HardDrive size={14} /> 使用应用私有目录
              </button>
              <p className="text-[11px] leading-relaxed" style={{ color: "var(--text-muted)" }}>
                私有目录仅本应用可见，其他应用与电脑都无法访问，内容无法导出。
              </p>
            </>
          )}
        </div>

        {error && (
          <p className="mt-3 text-[11px] break-words" style={{ color: "var(--danger)" }}>
            {error}
          </p>
        )}

        {firstRun && (
          <button
            onClick={onClose}
            className="mt-4 w-full h-9 rounded text-xs"
            style={{ color: "var(--text-muted)" }}
          >
            稍后再说
          </button>
        )}
      </div>
    </div>
  );
}
