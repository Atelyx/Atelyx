import { useEffect, useState } from "react";
import { DropdownSelect } from "@/components/common/DropdownSelect";
import { SettingCard } from "@/components/settings/SettingCard";
import { SlotListMount } from "@/components/plugins/SlotHost";
import { useSettingsStore } from "@/stores/settingsStore";

/** 联网搜索面板（仓库级）。 */
export function SearchSettingsTab() {
  const searchConfig = useSettingsStore((s) => s.searchConfig);
  const tavilyKey = useSettingsStore((s) => s.tavilyKey);
  const setSearchConfig = useSettingsStore((s) => s.setSearchConfig);
  const setTavilyKey = useSettingsStore((s) => s.setTavilyKey);
  // Tavily key 用本地草稿 + blur 提交（受控输入避免每键一次 keychain 写入）
  const [keyDraft, setKeyDraft] = useState(tavilyKey);
  useEffect(() => setKeyDraft(tavilyKey), [tavilyKey]);

  return (
    <section className="flex-1 p-5 overflow-auto space-y-4">
      {/* 搜索服务：AI 联网搜索使用的服务商 */}
      <SettingCard title="搜索服务" description="AI 联网搜索使用的服务商">
        <DropdownSelect
          value={searchConfig.provider}
          onChange={(v) =>
            void setSearchConfig({
              provider: v as "tavily" | "searxng",
            })
          }
          options={[
            { value: "tavily", label: "Tavily API" },
            { value: "searxng", label: "SearXNG 自建实例" },
          ]}
          className="text-sm rounded px-2 py-1"
          style={{
            color: "var(--text-primary)",
            background: "var(--input-bg)",
            border: "1px solid var(--input-border)",
          }}
        />
      </SettingCard>
      {searchConfig.provider === "tavily" ? (
        <SettingCard
          title="Tavily API Key"
          description="默认存本机钥匙串，不进仓库文件"
        >
          <input
            type="password"
            value={keyDraft}
            onChange={(e) => setKeyDraft(e.target.value)}
            onBlur={() => void setTavilyKey(keyDraft.trim())}
            placeholder="tvly-..."
            className="text-sm rounded px-2 py-1 outline-none w-[260px]"
            style={{
              color: "var(--text-primary)",
              background: "var(--input-bg)",
              border: "1px solid var(--input-border)",
            }}
          />
        </SettingCard>
      ) : (
        <SettingCard title="SearXNG URL" description="自建实例的访问地址">
          <input
            type="url"
            value={searchConfig.searxngUrl}
            onChange={(e) =>
              void setSearchConfig({ searxngUrl: e.target.value })
            }
            placeholder="https://searx.example.com"
            className="text-sm rounded px-2 py-1 outline-none w-[260px]"
            style={{
              color: "var(--text-primary)",
              background: "var(--input-bg)",
              border: "1px solid var(--input-border)",
            }}
          />
        </SettingCard>
      )}

      {/* 插件贡献的设置区块（ctx.slots.registerUi 槽名 settings/search） */}
      <SlotListMount slot="settings/search" />
    </section>
  );
}
