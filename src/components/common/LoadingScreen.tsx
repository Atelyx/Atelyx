/**
 * 启动/进仓加载屏：窗口创建即显示（init 完成前）与仓库加载期间（selectVault 全程）。
 *
 * 全屏 + Logo + 循环扫光进度条（indeterminate）+ 加载步骤清单；步骤区布局见下方锚点注释，
 * 步骤由 appStore 的加载会话上报（beginLoad/reportLoad/endLoad），清单为空时只留 Logo + 扫光。
 */
// 应用图标（与 src-tauri/icons/icon.svg 同源，加载屏 Logo 展示）
import { Check, Loader2 } from "lucide-react";
import { useAppStore } from "@/stores/appStore";
import appIcon from "@/assets/icon.svg";

export function LoadingScreen() {
  const loadSteps = useAppStore((s) => s.loadSteps);

  return (
    // 配色全部走主题变量：index.html 首屏预置 .dark，深色默认下无闪变
    <div
      className="h-full flex flex-col items-center justify-center select-none"
      style={{ background: "var(--bg-primary)" }}
    >
      <img
        src={appIcon}
        alt="Atelyx"
        draggable={false}
        className="w-16 h-16 rounded-2xl shadow-lg ring-1 ring-[var(--border)]"
      />
      {/* 扫光动画（.sweep-track/.sweep-bar）定义在 styles/index.css */}
      <div className="sweep-track mt-8">
        <div className="sweep-bar" />
      </div>

      {/* 步骤可视窗：固定高度 + overflow-hidden（无滚动条），justify-end 底部锚定——
          条目从下向上顶替，超出顶部者被遮罩渐变淡出；上下留白不顶格 */}
      {loadSteps.length > 0 && (
        <div className="relative mt-7 w-[460px] h-[156px] overflow-hidden">
          <div
            className="pointer-events-none absolute inset-x-0 top-0 h-10 z-10"
            style={{
              background:
                "linear-gradient(to bottom, var(--bg-primary) 0%, transparent 100%)",
            }}
          />
          <ul className="h-full flex flex-col justify-end gap-1.5 px-1 py-1 text-ui">
            {loadSteps.map((step, i) => {
              // 最后一项 = 当前进行中（转圈 + 高亮），其余已完成项打勾
              const current = i === loadSteps.length - 1;
              return (
                <li
                  key={i}
                  className="flex items-center gap-2 min-w-0"
                  style={{ animation: "load-step-in var(--dur-slow) var(--ease)" }}
                >
                  {current ? (
                    <Loader2
                      size={12}
                      className="animate-spin flex-shrink-0"
                      style={{ color: "var(--accent)" }}
                    />
                  ) : (
                    <Check
                      size={12}
                      className="flex-shrink-0"
                      style={{ color: "var(--accent)" }}
                    />
                  )}
                  <span
                    className="truncate"
                    style={{
                      color: current ? "var(--text-primary)" : "var(--text-muted)",
                    }}
                  >
                    {step}
                  </span>
                </li>
              );
            })}
          </ul>
        </div>
      )}
    </div>
  );
}
