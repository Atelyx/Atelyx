import {
  AlertCircle,
  CheckCircle2,
  Download,
  ExternalLink,
  Loader2,
  RefreshCw,
} from "lucide-react";
import { useEffect, useState } from "react";
import { useAppStore } from "@/stores/appStore";
// 应用图标（与 src-tauri/icons/icon.svg 同源，设置页「关于」Logo 展示）
import appIcon from "@/assets/icon.svg";

/** 项目主页（更新源为 GitHub Release，见 tauri.conf.json updater.endpoints）。 */
const REPO_URL = "https://github.com/Atelyx/Atelyx";

/**
 * 设置页「关于」tab：Logo + 版本号 + 手动检查更新（下载安装后 relaunch 重启）。
 * 更新状态机在 appStore（updateStatus）：idle → checking → upToDate / available / error。
 */
export function AboutSection() {
  const updateStatus = useAppStore((s) => s.updateStatus);
  const updateLatestVersion = useAppStore((s) => s.updateLatestVersion);
  const updateError = useAppStore((s) => s.updateError);
  const installing = useAppStore((s) => s.installing);
  const checkForUpdates = useAppStore((s) => s.checkForUpdates);
  const installUpdate = useAppStore((s) => s.installUpdate);
  const getAppVersion = useAppStore((s) => s.getAppVersion);
  const openUrl = useAppStore((s) => s.openUrl);

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
          className="relative w-16 h-16 rounded-xl shadow-lg ring-1 ring-white/10 select-none"
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
          <button
            onClick={() => void installUpdate()}
            disabled={installing}
            className="flex items-center gap-1.5 px-4 py-2 rounded text-sm font-medium bg-[var(--accent)] text-[var(--accent-fg)] hover:opacity-90 disabled:opacity-60"
          >
            {installing ? (
              <Loader2 size={14} className="animate-spin" />
            ) : (
              <Download size={14} />
            )}
            {installing ? "安装中…" : "下载并安装"}
          </button>
        ) : (
          <button
            onClick={() => void checkForUpdates()}
            disabled={updateStatus === "checking"}
            className="flex items-center gap-1.5 px-4 py-2 rounded text-sm font-medium bg-[var(--accent)] text-[var(--accent-fg)] hover:opacity-90 disabled:opacity-60"
          >
            {updateStatus === "checking" ? (
              <Loader2 size={14} className="animate-spin" />
            ) : (
              <RefreshCw size={14} />
            )}
            {updateStatus === "checking"
              ? "检查中…"
              : updateStatus === "upToDate"
                ? "重新检查"
                : updateStatus === "error"
                  ? "重试"
                  : "检查更新"}
          </button>
        )}
        {updateStatus === "upToDate" && (
          <p
            className="text-xs flex items-center gap-1"
            style={{ color: "#4ade80" }}
          >
            <CheckCircle2 size={12} className="flex-shrink-0" />
            已是最新版本
          </p>
        )}
        {updateStatus === "available" && (
          <p
            className="text-xs max-w-[420px] text-center"
            style={{ color: "var(--text-muted)" }}
          >
            发现新版本 {updateLatestVersion}；安装完成后将自动重启应用
          </p>
        )}
        {updateStatus === "error" && (
          <div className="flex flex-col items-center gap-1 w-full max-w-[480px]">
            <p
              className="text-xs flex items-center gap-1"
              style={{ color: "#f87171" }}
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
