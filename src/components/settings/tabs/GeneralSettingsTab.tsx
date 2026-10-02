import { Fragment } from "react";
import { ChevronDown, ChevronUp } from "lucide-react";
import { ToggleSwitch } from "@/components/common/ToggleSwitch";
import { DropdownSelect } from "@/components/common/DropdownSelect";
import { SettingCard } from "@/components/settings/SettingCard";
import { SlotListMount } from "@/components/plugins/SlotHost";
import { useSettingsStore } from "@/stores/settingsStore";
import { useAppStore } from "@/stores/appStore";
import { usePluginStore } from "@/stores/pluginStore";
import { viewMetaFor } from "@/components/layout/ViewHost";
import { useDraftSync } from "@/hooks/useDraftSync";
import { MOBILE_NAV_BAR_SIZE, orderMobileViews, swapInOrder } from "@/utils/mobileNav";

/** 界面字体选项（value = CSS font-family；空串 = 跟随系统默认）。 */
const FONT_OPTIONS: { label: string; value: string }[] = [
  { label: "跟随系统", value: "" },
  {
    label: "无衬线",
    value: "system-ui, -apple-system, 'Segoe UI', sans-serif",
  },
  { label: "衬线", value: "Georgia, 'Times New Roman', serif" },
  { label: "等宽", value: "Consolas, 'Courier New', monospace" },
];

/** 通用面板（应用级外观）：草稿与状态自持，直接订阅 store。
 * 主题模式与强调色在「主题」tab（主题插件 + 设置项），仓库级开关在「仓库设置」里。
 * 此处只放跨仓库共享的应用级项：字号/字体/自动恢复/主页布局/自动更新。 */
export function GeneralSettingsTab() {
  // 应用级外观（跨仓库共享，global.json）：字号 / 字体 / 自动恢复 / 主页布局 / 自动更新
  const fontSize = useSettingsStore((s) => s.fontSize);
  const setFontSize = useSettingsStore((s) => s.setFontSize);
  const fontFamily = useSettingsStore((s) => s.fontFamily);
  const setFontFamily = useSettingsStore((s) => s.setFontFamily);
  const autoRestoreFiles = useSettingsStore((s) => s.autoRestoreFiles);
  const setAutoRestoreFiles = useSettingsStore((s) => s.setAutoRestoreFiles);
  const defaultHomeLayout = useSettingsStore((s) => s.defaultHomeLayout);
  const setDefaultHomeLayout = useSettingsStore((s) => s.setDefaultHomeLayout);
  const autoUpdate = useAppStore((s) => s.autoUpdate);
  const setAutoUpdate = useAppStore((s) => s.setAutoUpdate);
  /** 自动更新能力（桌面有 / 移动端无；经 store 读取，守「组件不 import services」分层）。 */
  const autoUpdateSupported = useAppStore((s) => s.platform.capabilities.autoUpdate);

  // 移动端底部导航栏顺序（应用级；桌面无此栏，故仅移动端渲染本区块）
  const isAndroid = useAppStore((s) => s.platform.isAndroid);
  const mobileNavOrder = useSettingsStore((s) => s.mobileNavOrder);
  const setMobileNavOrder = useSettingsStore((s) => s.setMobileNavOrder);
  // 订阅插件 UI 注册变化：视图槽增删（插件启停/挂载）后可用视图集合随之变化
  usePluginStore((s) => s.uiRevision);
  const mobileViews = orderMobileViews(usePluginStore.getState().pluginViewKinds(), mobileNavOrder);

  /** 相邻互换后整表提交（顺序是「用户自定义顺序」，首项缺失即回落到内建常用序）。 */
  const moveMobileView = (from: number, to: number) => {
    void setMobileNavOrder(swapInOrder(mobileViews, from, to));
  };

  // 字号用本地草稿 + blur 提交：受控 + 范围校验会拒绝输入中间态（如敲 "1" 准备输 15）导致无法输入
  const [fontSizeDraft, setFontSizeDraft] = useDraftSync(
    fontSize !== undefined ? String(fontSize) : "",
  );

  /** blur/Enter 提交字号；非法值回滚为当前配置值。 */
  const commitFontSize = () => {
    const v = fontSizeDraft.trim();
    if (v === "") {
      void setFontSize(undefined);
      return;
    }
    const n = Number(v);
    if (n >= 12 && n <= 20) {
      void setFontSize(n);
    } else {
      setFontSizeDraft(fontSize !== undefined ? String(fontSize) : ""); // 非法值回滚
    }
  };

  return (
    <section className="flex-1 p-5 overflow-auto space-y-4">
      {/* 字体大小（应用级） */}
      <SettingCard title="字体大小" description="界面字号；留空 = 16">
        <input
          type="number"
          min={12}
          max={20}
          step={1}
          value={fontSizeDraft}
          onChange={(e) => setFontSizeDraft(e.target.value)}
          onBlur={commitFontSize}
          onKeyDown={(e) => {
            if (e.key === "Enter") e.currentTarget.blur();
          }}
          placeholder="16"
          className="text-sm rounded px-2 py-1 outline-none max-w-[90px]"
          style={{
            color: "var(--text-primary)",
            background: "var(--input-bg)",
            border: "1px solid var(--input-border)",
          }}
        />
      </SettingCard>

      {/* 字体（应用级） */}
      <SettingCard title="字体" description="界面字体">
        <DropdownSelect
          value={fontFamily ?? ""}
          onChange={(v) => void setFontFamily(v || undefined)}
          options={FONT_OPTIONS}
          className="text-sm rounded px-2 py-1 max-w-[220px]"
          style={{
            color: "var(--text-secondary)",
            background: "var(--input-bg)",
            border: "1px solid var(--input-border)",
          }}
        />
      </SettingCard>

      {/* 自动恢复上次打开的文件（应用级）：进入仓库时恢复上次打开的画布/笔记窗口 */}
      <SettingCard
        title="自动恢复上次打开的文件"
        description="进入仓库时恢复上次打开的文件"
      >
        <ToggleSwitch
          checked={autoRestoreFiles}
          onChange={(v) => void setAutoRestoreFiles(v)}
          title="自动恢复上次打开的文件"
        />
      </SettingCard>

      {/* 进仓库时打开主页（应用级）：开启后进入仓库自动切到主页布局；关闭 = 保持恢复上次界面 */}
      <SettingCard
        title="进仓库时打开主页"
        description="进入仓库自动切到主页布局；关闭则恢复上次界面"
      >
        <ToggleSwitch
          checked={defaultHomeLayout}
          onChange={(v) => void setDefaultHomeLayout(v)}
          title="进仓库时打开主页"
        />
      </SettingCard>

      {/* 自动更新（应用级，global.json）：开启后启动时静默检查新版本并自动安装。
          移动端无 updater（不支持自动安装），开关置禁用，启动改为检查并提示下载。 */}
      <SettingCard
        title="自动更新"
        description={
          autoUpdateSupported
            ? "启动时自动检查新版本并安装；关闭 = 不联网检查"
            : "当前平台不支持自动安装，启动时只检查并提示下载"
        }
      >
        <ToggleSwitch
          checked={autoUpdate}
          onChange={(v) => void setAutoUpdate(v)}
          title={autoUpdateSupported ? "自动更新" : "当前平台不支持自动更新"}
          disabled={!autoUpdateSupported}
        />
      </SettingCard>

      {/* 移动端底部导航栏顺序（应用级）：前 5 个上底栏，其余收进「更多」 */}
      {isAndroid && (
        <SettingCard
          title="移动端导航栏"
          description={`底部导航栏显示前 ${MOBILE_NAV_BAR_SIZE} 个视图，其余收进「更多」；上下移动即调整底栏顺序`}
        >
          <div className="flex flex-col gap-0.5 w-56 max-h-64 overflow-y-auto">
            {mobileViews.map((kind, i) => (
              <Fragment key={kind}>
                {i === MOBILE_NAV_BAR_SIZE && (
                  <div
                    className="mt-1.5 pt-1.5 text-[11px]"
                    style={{ borderTop: "1px solid var(--border-subtle)", color: "var(--text-muted)" }}
                  >
                    以下收进「更多」
                  </div>
                )}
                <div className="flex items-center gap-1">
                  <span
                    className="flex-1 min-w-0 truncate text-xs"
                    style={{ color: "var(--text-primary)" }}
                  >
                    {viewMetaFor(kind).label}
                  </span>
                  {/* 触控目标按移动端档 44px（本区块仅移动端渲染） */}
                  <button
                    onClick={() => moveMobileView(i, i - 1)}
                    disabled={i === 0}
                    title="上移"
                    aria-label={`上移 ${viewMetaFor(kind).label}`}
                    className="w-11 h-11 flex items-center justify-center rounded-[var(--radius-sm)] hover:bg-[var(--hover)] disabled:opacity-30 disabled:hover:bg-transparent"
                    style={{ color: "var(--text-muted)" }}
                  >
                    <ChevronUp size={14} />
                  </button>
                  <button
                    onClick={() => moveMobileView(i, i + 1)}
                    disabled={i === mobileViews.length - 1}
                    title="下移"
                    aria-label={`下移 ${viewMetaFor(kind).label}`}
                    className="w-11 h-11 flex items-center justify-center rounded-[var(--radius-sm)] hover:bg-[var(--hover)] disabled:opacity-30 disabled:hover:bg-transparent"
                    style={{ color: "var(--text-muted)" }}
                  >
                    <ChevronDown size={14} />
                  </button>
                </div>
              </Fragment>
            ))}
          </div>
        </SettingCard>
      )}

      {/* 插件贡献的设置区块（ctx.slots.registerUi 槽名 settings/general） */}
      <SlotListMount slot="settings/general" />
    </section>
  );
}
