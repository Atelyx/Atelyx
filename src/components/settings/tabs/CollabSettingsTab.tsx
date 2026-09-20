import { RotateCcw } from "lucide-react";
import { ToggleSwitch } from "@/components/common/ToggleSwitch";
import { SettingCard } from "@/components/settings/SettingCard";
import { SpaceAccountSection } from "@/components/settings/SpaceAccountSection";
import { SlotListMount } from "@/components/plugins/SlotHost";
import { randomPeerColor, useCollabStore } from "@/stores/collabStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { useDraftSync, useDebouncedDraft } from "@/hooks/useDraftSync";

/** 多人协作面板（应用级）：协作空间账号 + 空间频道开关与身份（昵称/颜色作用于空间在线列表）。草稿自持，直接订阅 store。 */
export function CollabSettingsTab() {
  // 协作（应用级）：空间频道开关 + 昵称/颜色 + 常驻连接状态
  const collabEnabled = useSettingsStore((s) => s.collabEnabled);
  const collabNickname = useSettingsStore((s) => s.collabNickname);
  const collabColor = useSettingsStore((s) => s.collabColor);
  const setCollabConfig = useSettingsStore((s) => s.setCollabConfig);
  const collabConnected = useCollabStore((s) => s.connected);

  // 协作昵称草稿（blur/Enter 提交，避免每键一次 IPC）
  const [collabNicknameDraft, setCollabNicknameDraft] = useDraftSync(collabNickname);
  const commitCollabNickname = () => {
    void setCollabConfig({ collabNickname: collabNicknameDraft.trim() });
  };
  // 协作身份色草稿：取色器拖动连续触发 onChange，防抖 200ms 后落盘（同强调色模式）
  const [collabColorDraft, commitCollabColorDraft] = useDebouncedDraft(
    collabColor || "#e06c75",
    (v) => void setCollabConfig({ collabColor: v }),
  );

  return (
    <section className="flex-1 p-5 overflow-auto space-y-4">
      {/* 协作空间账号：登录/注册 + 已登录服务器与设备会话管理 */}
      <SpaceAccountSection />

      {/* 开关：开启后进入协作空间即连接（身份变化即时重建连接） */}
      <SettingCard
        title="多人协作"
        description="进入协作空间后与成员实时互见（选中高亮）"
      >
        <div className="flex items-center gap-3">
          <ToggleSwitch
            checked={collabEnabled}
            onChange={(v) => void setCollabConfig({ collabEnabled: v })}
            title="多人协作"
          />
          {collabEnabled && (
            <span
              className="flex items-center gap-1.5 text-xs"
              style={{ color: collabConnected ? "#22c55e" : "var(--text-muted)" }}
            >
              <span
                className="w-1.5 h-1.5 rounded-full"
                style={{ background: collabConnected ? "#22c55e" : "var(--text-muted)" }}
              />
              {collabConnected ? "已连接" : "未连接"}
            </span>
          )}
        </div>
      </SettingCard>

      {/* 身份：昵称 = 在线列表展示名；颜色 = 远端选中高亮描边色 */}
      <SettingCard
        title="昵称与颜色"
        description="空昵称 = 设备名；空颜色 = 随机分配"
      >
        <div className="flex items-center gap-2">
          <input
            value={collabNicknameDraft}
            onChange={(e) => setCollabNicknameDraft(e.target.value)}
            onBlur={commitCollabNickname}
            onKeyDown={(e) => {
              if (e.key === "Enter") e.currentTarget.blur();
            }}
            placeholder="设备名"
            className="text-sm rounded px-2 py-1 outline-none max-w-[140px]"
            style={{
              color: "var(--text-primary)",
              background: "var(--input-bg)",
              border: "1px solid var(--input-border)",
            }}
          />
          <input
            type="color"
            value={collabColorDraft}
            onChange={(e) => commitCollabColorDraft(e.target.value)}
            title="身份颜色"
            className="w-6 h-6 rounded cursor-pointer bg-transparent p-0 border-0"
          />
          <button
            onClick={() => {
              // 随机 = 提交一个新随机色存显式值（重启不变）；空色仅作未配置时的启动随机兜底
              commitCollabColorDraft(randomPeerColor());
            }}
            title="随机换色"
            className="flex items-center gap-1 text-xs rounded px-1.5 py-1 hover:bg-[var(--hover)] flex-shrink-0"
            style={{ color: "var(--text-secondary)" }}
          >
            <RotateCcw size={12} />
            随机
          </button>
        </div>
      </SettingCard>

      {/* 插件贡献的设置区块（ctx.slots.registerUi 槽名 settings/collab） */}
      <SlotListMount slot="settings/collab" />
    </section>
  );
}
