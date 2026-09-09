import { EllipsisVertical, Folder, FolderOpen, Loader2 } from "lucide-react";
import { useEffect, useState } from "react";
import { useAppStore } from "@/stores/appStore";
import { TitleBarControls } from "@/components/common/TitleBarControls";
// 应用图标（与 src-tauri/icons/icon.svg 同源，启动页 Logo 展示）
import appIcon from "@/assets/icon.svg";

/**
 * 仓库选择页（启动页）。
 * 极简居中卡片式：Logo + 新建/打开仓库大卡片 + 最近仓库横排卡片。
 * 选中仓库 → selectVault → 直达画布工作区（占位引导）。
 * 窗口固定 960×640 不可调整（App view effect → services/window 切换）。
 * 背景固定深色（独立于主题，主题作用于工作区）。
 */
const C = {
  bg: "#1e1e1e",
  card: "#252526",
  cardBorder: "rgba(255,255,255,0.08)",
  hover: "rgba(255,255,255,0.06)",
  textPrimary: "#f5f5f5",
  textSecondary: "rgba(255,255,255,0.55)",
  textMuted: "rgba(255,255,255,0.4)",
  divider: "rgba(255,255,255,0.08)",
  accent: "#D4AF37",
  accentHover: "#B8962E",
  danger: "#f87171",
};

interface VaultMenuState {
  root: string;
  x: number;
  y: number;
}

export function VaultSelectPage() {
  const recentVaults = useAppStore((s) => s.recentVaults);
  const selectVault = useAppStore((s) => s.selectVault);
  const removeRecentVault = useAppStore((s) => s.removeRecentVault);
  const pickVaultDirectory = useAppStore((s) => s.pickVaultDirectory);
  const openInExplorer = useAppStore((s) => s.openInExplorer);
  const getAppVersion = useAppStore((s) => s.getAppVersion);
  const minimizeWindow = useAppStore((s) => s.minimizeWindow);
  const closeWindow = useAppStore((s) => s.closeWindow);

  const [busy, setBusy] = useState(false);
  const [version, setVersion] = useState("");
  const [menu, setMenu] = useState<VaultMenuState | null>(null);
  const [confirmingRoot, setConfirmingRoot] = useState<string | null>(null);

  useEffect(() => {
    void getAppVersion().then(setVersion).catch(() => {});
  }, [getAppVersion]);

  // 点击菜单/按钮外部关闭 ⋮ 菜单（data-vault-menu 内不关）
  useEffect(() => {
    if (!menu) return;
    const close = (e: MouseEvent) => {
      if (!(e.target as HTMLElement).closest("[data-vault-menu]")) setMenu(null);
    };
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [menu]);

  /** 调系统目录选择器，选中后打开为仓库（open_vault 会建 .atelyx/）。 */
  const pickAndOpenVault = async () => {
    if (busy) return;
    setBusy(true);
    try {
      const path = await pickVaultDirectory();
      if (path) {
        await selectVault(path);
      }
    } catch (e) {
      console.error("选择文件夹失败", e);
    } finally {
      setBusy(false);
    }
  };

  const openMenu = (e: React.MouseEvent, root: string) => {
    const rect = e.currentTarget.getBoundingClientRect();
    setMenu({ root, x: rect.right, y: rect.bottom });
  };

  const handleRemove = async (root: string) => {
    await removeRecentVault(root);
    setConfirmingRoot(null);
  };

  return (
    <div className="h-full flex flex-col" style={{ background: C.bg }}>
      {/* ===== 标题栏：可拖拽移动窗口，右侧窗口控制按钮 ===== */}
      <header
        className="h-9 flex items-center flex-shrink-0 px-3 select-none"
        style={
          {
            borderBottom: `1px solid ${C.cardBorder}`,
            // 启动页固定深色（独立于主题），标题栏按钮变量直接覆盖为夜间规范值
            "--titlebar-icon": "#cccccc",
            "--titlebar-hover": "#333333",
            "--titlebar-close-hover": "#e81123",
          } as React.CSSProperties
        }
        data-tauri-drag-region
      >
        <span className="text-xs" style={{ color: C.textMuted }} data-tauri-drag-region>
          Atelyx
        </span>
        <div className="ml-auto h-full">
          <TitleBarControls
            onMinimize={() => void minimizeWindow()}
            onClose={() => void closeWindow()}
          />
        </div>
      </header>

      {/* ===== 主体：居中品牌区 + 操作卡片 + 最近仓库 ===== */}
      <main className="flex-1 flex flex-col items-center justify-center min-h-0 px-8 py-6 overflow-y-auto">
        {/* Logo + 金色光晕（呼应应用图标金色调） */}
        <div className="relative">
          <div
            className="absolute -inset-8 rounded-full"
            style={{ background: "radial-gradient(circle, rgba(212,175,55,0.10), transparent 65%)" }}
          />
          <img
            src={appIcon}
            alt="Atelyx"
            draggable={false}
            className="relative w-20 h-20 rounded-2xl shadow-lg ring-1 ring-white/10 select-none"
          />
        </div>
        <h1 className="mt-4 text-3xl font-semibold tracking-wide" style={{ color: C.textPrimary }}>
          Atelyx
        </h1>
        <p className="mt-2 text-xs" style={{ color: C.textMuted }}>
          版本 {version || "0.1.0"}
        </p>

        {/* 操作卡片：打开仓库（整卡可点） */}
        <div className="mt-8">
          <ActionCard
            icon={<FolderOpen size={18} />}
            title="打开仓库"
            desc="选择一个文件夹作为仓库打开。"
            onAction={() => void pickAndOpenVault()}
            busy={busy}
          />
        </div>

        {/* 最近打开：横排卡片（hover 浮现 ⋮ 菜单，多仓库限高滚动） */}
        <section className="mt-10 w-full max-w-[720px]">
          <p className="text-xs" style={{ color: C.textMuted }}>
            最近打开
          </p>
          {recentVaults.length === 0 ? (
            <p className="mt-3 text-xs" style={{ color: C.textMuted }}>
              暂无最近仓库，可打开一个文件夹作为仓库。
            </p>
          ) : (
            <ul className="mt-3 flex flex-wrap gap-3 max-h-[190px] overflow-y-auto">
              {recentVaults.map((v) => (
                <li
                  key={v.root}
                  className="group w-[220px] flex-shrink-0 rounded-lg px-3 py-2.5 transition-colors hover:bg-white/5"
                  style={{ background: C.card, border: `1px solid ${C.cardBorder}` }}
                  data-vault-menu
                >
                  <div className="flex items-center gap-2">
                    <button
                      onClick={() => void selectVault(v.root)}
                      disabled={busy}
                      className="flex-1 min-w-0 flex items-center gap-2 text-left"
                      title={v.name}
                    >
                      <Folder size={14} className="flex-shrink-0" style={{ color: C.accent }} />
                      <span
                        className="min-w-0 truncate text-sm font-medium"
                        style={{ color: C.textPrimary }}
                      >
                        {v.name}
                      </span>
                    </button>
                    {confirmingRoot === v.root ? (
                      <div
                        className="flex-shrink-0 flex items-center gap-1 text-xs"
                        style={{ color: C.textSecondary }}
                      >
                        <span>移除？</span>
                        <button
                          onClick={() => void handleRemove(v.root)}
                          className="hover:opacity-80"
                          style={{ color: C.danger }}
                        >
                          移除
                        </button>
                        <button onClick={() => setConfirmingRoot(null)} className="hover:opacity-80">
                          取消
                        </button>
                      </div>
                    ) : (
                      <button
                        onClick={(e) => openMenu(e, v.root)}
                        aria-label="仓库操作"
                        title="仓库操作"
                        className="flex-shrink-0 p-0.5 rounded opacity-0 group-hover:opacity-100 hover:bg-white/10"
                        style={{ color: C.textSecondary }}
                      >
                        <EllipsisVertical size={14} />
                      </button>
                    )}
                  </div>
                  <button
                    onClick={() => void selectVault(v.root)}
                    disabled={busy}
                    className="block w-full mt-1.5 text-left"
                    title={v.root}
                  >
                    <span className="block text-xs truncate" style={{ color: C.textSecondary }}>
                      {v.root}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </section>
      </main>

      {/* 更多操作下拉菜单 */}
      {menu && (
        <div
          className="fixed z-50 rounded-md shadow-2xl py-1 min-w-[180px]"
          style={{ left: menu.x - 180, top: menu.y + 4, background: "#2b2b2b" }}
          data-vault-menu
        >
          <button
            onClick={() => {
              void openInExplorer(menu.root);
              setMenu(null);
            }}
            className="w-full text-left px-3 py-1.5 text-sm hover:bg-white/10"
            style={{ color: C.textPrimary }}
          >
            在文件管理器中打开
          </button>
          <button
            onClick={() => {
              setConfirmingRoot(menu.root);
              setMenu(null);
            }}
            className="w-full text-left px-3 py-1.5 text-sm hover:bg-white/10"
            style={{ color: C.danger }}
          >
            从列表移除
          </button>
        </div>
      )}
    </div>
  );
}

interface ActionCardProps {
  icon: React.ReactNode;
  title: string;
  desc: string;
  onAction: () => void;
  busy: boolean;
}

function ActionCard({ icon, title, desc, onAction, busy }: ActionCardProps) {
  return (
    <button
      onClick={busy ? undefined : onAction}
      disabled={busy}
      className="w-[340px] h-[112px] flex items-center gap-4 px-5 rounded-xl text-left transition-all duration-150 hover:-translate-y-0.5 hover:shadow-[0_0_0_1px_#D4AF37] disabled:opacity-60 disabled:hover:translate-y-0 disabled:hover:shadow-none"
      style={{ background: C.card, border: `1px solid ${C.cardBorder}` }}
    >
      <span
        className="w-10 h-10 rounded-lg flex items-center justify-center flex-shrink-0"
        style={{ background: "rgba(212,175,55,0.18)", color: C.accent }}
      >
        {busy ? <Loader2 size={18} className="animate-spin" /> : icon}
      </span>
      <span className="min-w-0">
        <span className="block text-sm font-medium truncate" style={{ color: C.textPrimary }}>
          {title}
        </span>
        <span className="block text-xs mt-1 truncate" style={{ color: C.textSecondary }}>
          {desc}
        </span>
      </span>
    </button>
  );
}
