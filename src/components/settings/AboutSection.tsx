import { ProgressBar } from "@/components/common/primitives";
import { Button } from "@/components/common/Button";
import {
  AlertCircle,
  CheckCircle2,
  Download,
  ExternalLink,
  RefreshCw,
} from "lucide-react";
import { useEffect, useState } from "react";
import { useAppStore } from "@/stores/appStore";
// 应用图标（与 src-tauri/icons/icon.svg 同源，设置页「关于」Logo 展示）
import appIcon from "@/assets/icon.svg";

/** 项目主页（更新源为 GitHub Release）。 */
const REPO_URL = "https://github.com/Atelyx/Atelyx";

/** 字节转 MB（更新包量级为十几到几十 MB，固定用 MB 足够读）。 */
function toMb(bytes: number): string {
  return (bytes / 1024 / 1024).toFixed(1);
}

/**
 * 设置页「关于」tab：Logo + 版本号 + 手动检查更新（应用内下载并启动安装程序）。
 * 更新状态机在 appStore（updateStatus）：idle → checking → upToDate / available / error。
 */
export function AboutSection() {
  const updateStatus = useAppStore((s) => s.updateStatus);
  const updateLatestVersion = useAppStore((s) => s.updateLatestVersion);
  const updateError = useAppStore((s) => s.updateError);
  const updatePhase = useAppStore((s) => s.updatePhase);
  const updateReceived = useAppStore((s) => s.updateReceived);
  const updateTotal = useAppStore((s) => s.updateTotal);
  const checkForUpdates = useAppStore((s) => s.checkForUpdates);
  const installUpdate = useAppStore((s) => s.installUpdate);
  const cancelUpdate = useAppStore((s) => s.cancelUpdate);
  const getAppVersion = useAppStore((s) => s.getAppVersion);
  const openUrl = useAppStore((s) => s.openUrl);
  /** 应用内更新（Windows/安卓）；Linux 只打开下载页（平台事实经 store 读取）。 */
  const inAppUpdate = useAppStore((s) => s.platform.capabilities.inAppUpdate);

  const [version, setVersion] = useState("");
  useEffect(() => {
    void getAppVersion().then(setVersion).catch(() => {});
  }, [getAppVersion]);

  /** 错误详情行点击复制完整错误（截断显示 + 悬停可看全文，复制便于排查）。 */
  const [copied, setCopied] = useState(false);
  const copyError = async () => {
    try {
      await navigator.clipboard.writeText(updateError);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // 剪贴板不可用时静默忽略，不影响其他操作
    }
  };

  /** 下载安装流水线进行中：按钮禁用并显示进度区。 */
  const busy = updatePhase !== "idle";
  const percent =
    updateTotal > 0 ? Math.min(100, Math.round((updateReceived / updateTotal) * 100)) : 0;
  const phaseLabel =
    updatePhase === "downloading" ? "下载中…" : updatePhase === "verifying" ? "校验中…" : "正在安装…";
  const progressText =
    updateTotal > 0
      ? `${percent}% · ${toMb(updateReceived)} / ${toMb(updateTotal)} MB`
      : `${toMb(updateReceived)} MB`;

  return (
    <section className="flex-1 overflow-auto flex flex-col items-center justify-center px-8">
      {/* Logo + 名称 + 版本（版本号动态读取，不硬编码） */}
      <div className="relative">
        <div
          className="absolute -inset-8 rounded-full"
          style={{
            background:
              "radial-gradient(circle, color-mix(in srgb, var(--accent) 10%, transparent), transparent 65%)",
          }}
        />
        <img
          src={appIcon}
          alt="Atelyx"
          draggable={false}
          className="relative w-16 h-16 rounded-xl shadow-[var(--shadow-pop)] ring-1 ring-white/10 select-none"
        />
      </div>
      <h3
        className="mt-4 text-xl font-semibold"
        style={{ color: "var(--text-primary)" }}
      >
        Atelyx
      </h3>
      {version && (
        <p className="mt-1 text-xs" style={{ color: "var(--text-muted)" }}>
          版本 {version}
        </p>
      )}

      {/* 检查更新 / 下载并安装（状态流：idle → checking → upToDate / available / error） */}
      <div className="mt-8 flex flex-col items-center gap-2">
        {updateStatus === "available" ? (
          <Button
            variant="primary"
            size="lg"
            onClick={() => void installUpdate()}
            disabled={busy}
            loading={busy}
            icon={busy ? undefined : <Download size={14} />}
          >
            {busy ? phaseLabel : inAppUpdate ? "下载并安装" : "前往下载页"}
          </Button>
        ) : (
          <Button
            variant="primary"
            size="lg"
            onClick={() => void checkForUpdates()}
            disabled={updateStatus === "checking"}
            loading={updateStatus === "checking"}
            icon={updateStatus === "checking" ? undefined : <RefreshCw size={14} />}
          >
            {updateStatus === "checking"
              ? "检查中…"
              : updateStatus === "upToDate"
                ? "重新检查"
                : updateStatus === "error"
                  ? "重试"
                  : "检查更新"}
          </Button>
        )}

        {busy && (
          <div className="flex flex-col items-center gap-1.5 w-full max-w-[320px]">
            <ProgressBar value={percent} className="w-full" label={phaseLabel} />
            <p className="text-xs" style={{ color: "var(--text-muted)" }}>
              {progressText}
            </p>
            {updatePhase === "downloading" && (
              <Button variant="subtle" size="sm" onClick={() => void cancelUpdate()}>
                取消下载
              </Button>
            )}
          </div>
        )}

        {updateStatus === "upToDate" && (
          <p
            className="text-xs flex items-center gap-1"
            style={{ color: "var(--success)" }}
          >
            <CheckCircle2 size={12} className="flex-shrink-0" />
            已是最新版本
          </p>
        )}
        {updateStatus === "available" && !busy && (
          <p
            className="text-xs max-w-[420px] text-center"
            style={{ color: "var(--text-muted)" }}
          >
            发现新版本 {updateLatestVersion}
            {inAppUpdate ? "；下载完成后将启动安装程序" : "；将打开下载页，安装包需自行安装"}
          </p>
        )}
        {updateStatus === "error" && (
          <div className="flex flex-col items-center gap-1 w-full max-w-[480px]">
            <p
              className="text-xs flex items-center gap-1"
              style={{ color: "var(--danger)" }}
            >
              <AlertCircle size={12} className="flex-shrink-0" />
              检查更新失败
            </p>
            {/* 原始错误单行截断（完整内容悬停查看），点击复制完整错误；防长 URL 撑出容器横向滚动 */}
            <p
              title={copied ? "已复制" : updateError}
              onClick={() => void copyError()}
              className="text-xs w-full max-w-[480px] truncate cursor-pointer hover:opacity-80"
              style={{ color: "var(--text-muted)" }}
            >
              {copied ? "已复制" : updateError}
            </p>
          </div>
        )}
      </div>

      {/* 项目主页（系统默认浏览器打开） */}
      <button
        onClick={() => void openUrl(REPO_URL)}
        title={REPO_URL}
        className="mt-8 flex items-center gap-1 text-xs hover:opacity-80"
        style={{ color: "var(--text-muted)" }}
      >
        GitHub 项目主页
        <ExternalLink size={11} />
      </button>
    </section>
  );
}
