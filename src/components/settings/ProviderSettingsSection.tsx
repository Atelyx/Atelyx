import {
  CheckCircle2,
  Loader2,
  Plus,
  RefreshCw,
  Unplug,
  X,
  XCircle,
} from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { PROVIDER_PRESETS } from "@/constants/providers";
import { SPACE_TEAM_AI_NOTICE, SPACE_VIEWER_NOTICE } from "@/constants/space";
import type { ProviderConfig } from "@/types";
import { SettingCard } from "@/components/settings/SettingCard";
import { ToggleSwitch } from "@/components/common/ToggleSwitch";
import { useEditingTargetFlags } from "@/hooks/useEditingTarget";
import { useSettingsStore, selectEditingProviders, selectEditingVaultConfig } from "@/stores/settingsStore";

/** 测试连通性结果（idle = 未测试；testing = 请求中）。 */
interface TestState {
  status: "idle" | "testing" | "ok" | "fail";
  message?: string;
  latencyMs?: number;
}

/**
 * 设置页「模型供应商」面板：左侧供应商卡片列表（+ 快速添加），右侧表单
 * （名称/Base URL/API Key + 测试连通性 + 多模型管理（获取列表/复选/昵称/手动添加））。
 * 供应商列表与写操作均作用于当前编辑目标（仓库设置弹窗可编辑非激活仓库）。
 *
 * 空间下这份配置（含 API key）整体存在服务端团队元数据里、全员共用一份，因此没有
 * 「API key 随仓库保存」开关（那是本地仓库的多设备同步选项）。
 */
export function ProviderSettingsSection() {
  const providers = useSettingsStore(selectEditingProviders);
  const syncKeys = useSettingsStore((s) => !!selectEditingVaultConfig(s)?.syncKeys);
  const setSyncKeys = useSettingsStore((s) => s.setSyncKeys);
  const addProvider = useSettingsStore((s) => s.addProvider);
  const updateProvider = useSettingsStore((s) => s.updateProvider);
  const removeProvider = useSettingsStore((s) => s.removeProvider);
  const { isSpace, viewerOnly } = useEditingTargetFlags();
  const [editingId, setEditingId] = useState<string | null>(
    providers[0]?.id ?? null,
  );
  const editing = providers.find((p) => p.id === editingId) ?? null;

  // 当前编辑的供应商被删除后自动选中剩余第一个
  useEffect(() => {
    if (editingId && !providers.some((p) => p.id === editingId)) {
      setEditingId(providers[0]?.id ?? null);
    }
  }, [providers, editingId]);

  return (
    <div className="flex-1 flex flex-col overflow-hidden">
      <div className="flex flex-1 overflow-hidden">
        <aside
          className="w-52 overflow-auto p-3 flex flex-col gap-1.5"
          style={{ borderRight: "1px solid var(--border)" }}
        >
        {providers.map((p) => (
          <ProviderCard
            key={p.id}
            provider={p}
            active={p.id === editingId}
            onClick={() => setEditingId(p.id)}
          />
        ))}
        {providers.length === 0 && (
          <p className="text-xs px-1" style={{ color: "var(--text-muted)" }}>
            {isSpace ? "该空间还没有配置模型供应商" : "还没有供应商，从下方添加"}
          </p>
        )}
        <div
          className="mt-2 pt-3 border-t"
          style={{ borderColor: "var(--border)" }}
        >
          <div
            className="text-xs px-1 mb-1.5"
            style={{ color: "var(--text-muted)" }}
          >
            快速添加
          </div>
          {PROVIDER_PRESETS.map((preset) => (
            <button
              key={preset.name}
              onClick={() => addProvider(preset).then(setEditingId)}
              className="w-full flex items-center gap-1.5 px-2 py-1.5 rounded text-xs text-[var(--text-secondary)] hover:bg-[var(--hover)]"
            >
              <Plus size={12} /> {preset.name}
            </button>
          ))}
          <button
            onClick={() => addProvider().then(setEditingId)}
            className="w-full flex items-center gap-1.5 px-2 py-1.5 rounded text-xs text-[var(--text-secondary)] hover:bg-[var(--hover)]"
          >
            <Plus size={12} /> 自定义
          </button>
        </div>
      </aside>
      <section className="flex-1 p-5 overflow-auto">
        {editing ? (
          <ProviderForm
            key={editing.id}
            provider={editing}
            onChange={(patch) => void updateProvider(editing.id, patch)}
            onRemove={() => void removeProvider(editing.id)}
          />
        ) : (
          <div className="h-full flex items-center justify-center">
            <p
              className="text-sm"
              style={{ color: "var(--text-muted)" }}
            >
              从左侧选择或添加一个供应商
            </p>
          </div>
        )}
      </section>
      </div>
      {/* 底部说明：本地仓库 = key 落盘策略开关；空间 = 团队共享说明（key 由服务端统一承载，无开关） */}
      <div className="px-5 py-3 border-t" style={{ borderColor: "var(--border)" }}>
        {isSpace ? (
          <p
            className="text-xs py-1"
            style={{ color: viewerOnly ? "#f59e0b" : "var(--text-muted)" }}
          >
            {viewerOnly ? SPACE_VIEWER_NOTICE : SPACE_TEAM_AI_NOTICE}
          </p>
        ) : (
          <SettingCard
            title="API key 随仓库保存"
            description="key 随仓库同步共用；仓库公开/共享时可能泄露"
          >
            <ToggleSwitch
              checked={syncKeys}
              onChange={(v) => void setSyncKeys(v)}
              title="API key 随仓库保存"
            />
          </SettingCard>
        )}
      </div>
    </div>
  );
}

function ProviderCard({
  provider,
  active,
  onClick,
}: {
  provider: ProviderConfig;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      className={`w-full text-left px-3 py-2 rounded-lg border transition ${
        active ? "" : "hover:bg-[var(--hover)]"
      }`}
      style={{
        background: active ? "color-mix(in srgb, var(--accent) 12%, transparent)" : "var(--bg-primary)",
        borderColor: active ? "var(--accent)" : "var(--border)",
      }}
    >
      <div className="flex items-center justify-between gap-1">
        <span
          className="text-sm font-medium truncate"
          style={{
            color: active ? "var(--accent)" : "var(--text-primary)",
          }}
        >
          {provider.name}
        </span>
        <span
          className="text-[10px] px-1 py-0.5 rounded flex-shrink-0"
          style={{
            background: "var(--bg-tertiary)",
            color: "var(--text-muted)",
          }}
        >
          {provider.models.length} 模型
        </span>
      </div>
      <div
        className="text-[11px] mt-0.5 truncate"
        style={{ color: "var(--text-muted)" }}
        title={provider.baseUrl}
      >
        {provider.baseUrl.replace(/^https?:\/\//, "") || "未设置地址"}
      </div>
    </button>
  );
}

/** 表单输入统一样式（与设置页其他 tab 一致）。 */
const INPUT_CLASS =
  "text-sm rounded px-2 py-1.5 outline-none w-full focus:ring-1 focus:ring-[var(--accent)]";
const INPUT_STYLE: React.CSSProperties = {
  color: "var(--text-primary)",
  background: "var(--input-bg)",
  border: "1px solid var(--input-border)",
};

function ProviderForm({
  provider,
  onChange,
  onRemove,
}: {
  provider: ProviderConfig;
  onChange: (patch: Partial<ProviderConfig>) => void;
  onRemove: () => void;
}) {
  const fetchProviderModelIds = useSettingsStore((s) => s.fetchProviderModelIds);
  /** 从供应商拉取的模型 ID 列表（null = 未获取/获取失败）。 */
  const [fetched, setFetched] = useState<string[] | null>(null);
  const [fetching, setFetching] = useState(false);
  const [fetchError, setFetchError] = useState<string | null>(null);
  const [test, setTest] = useState<TestState>({ status: "idle" });
  const [manualDraft, setManualDraft] = useState("");
  /** 测试/拉取互斥：同一时刻只允许一个在途请求（两按钮共享）。 */
  const [busy, setBusy] = useState(false);
  /** 卸载竞态：ProviderForm 以 key={editing.id} 重挂载，切换供应商会卸载旧实例，
   * await 返回后不再 setState（防陈旧结果覆盖新实例状态）。 */
  const aliveRef = useRef(true);
  useEffect(() => {
    aliveRef.current = true;
    return () => {
      aliveRef.current = false;
    };
  }, []);

  /** 测试连通性：GET /models 验证端点可达 + key 有效（免费，不触发模型计费）。 */
  const runTest = async () => {
    if (busy) return;
    setBusy(true);
    setTest({ status: "testing" });
    const start = performance.now();
    try {
      await fetchProviderModelIds(provider.id);
      if (!aliveRef.current) return;
      setTest({
        status: "ok",
        latencyMs: Math.round(performance.now() - start),
      });
    } catch (e) {
      if (!aliveRef.current) return;
      setTest({
        status: "fail",
        message: e instanceof Error ? e.message : String(e),
      });
    } finally {
      if (aliveRef.current) setBusy(false);
    }
  };

  /** 拉取供应商模型列表（成功即展示复选列表，失败降级为手动添加）。 */
  const fetchModels = async () => {
    if (busy) return;
    setBusy(true);
    setFetching(true);
    setFetchError(null);
    try {
      const ids = await fetchProviderModelIds(provider.id);
      if (!aliveRef.current) return;
      setFetched(ids.sort((a, b) => a.localeCompare(b)));
    } catch (e) {
      if (!aliveRef.current) return;
      setFetched(null);
      setFetchError(e instanceof Error ? e.message : String(e));
    } finally {
      if (aliveRef.current) {
        setFetching(false);
        setBusy(false);
      }
    }
  };

  // 行 = 服务端模型（可复选）∪ 已添加但不在服务端列表的手动模型（固定选中，标记「手动」）
  const rows: { id: string; fromServer: boolean }[] = [
    ...(fetched ?? []).map((id) => ({ id, fromServer: true })),
    ...provider.models
      .filter((m) => !fetched?.includes(m.id))
      .map((m) => ({ id: m.id, fromServer: false })),
  ];

  const toggleModel = (id: string) => {
    const has = provider.models.some((m) => m.id === id);
    onChange({
      models: has
        ? provider.models.filter((m) => m.id !== id)
        : [...provider.models, { id }],
    });
  };

  const setNickname = (id: string, nickname: string) => {
    onChange({
      models: provider.models.map((m) =>
        m.id === id ? { ...m, nickname: nickname || undefined } : m,
      ),
    });
  };

  const removeModel = (id: string) => {
    onChange({ models: provider.models.filter((m) => m.id !== id) });
  };

  const addManual = () => {
    const v = manualDraft.trim();
    if (!v || provider.models.some((m) => m.id === v)) {
      setManualDraft("");
      return;
    }
    onChange({ models: [...provider.models, { id: v }] });
    setManualDraft("");
  };

  return (
    <div className="space-y-4">
      <Field label="名称">
        <input
          type="text"
          value={provider.name}
          onChange={(e) => onChange({ name: e.target.value })}
          placeholder="My Provider"
          className={INPUT_CLASS}
          style={INPUT_STYLE}
        />
      </Field>
      <Field label="Base URL">
        <input
          type="url"
          value={provider.baseUrl}
          onChange={(e) => onChange({ baseUrl: e.target.value })}
          placeholder="https://api.openai.com/v1"
          className={INPUT_CLASS}
          style={INPUT_STYLE}
        />
      </Field>
      <Field label="API Key">
        <div className="flex items-center gap-2">
          <div className="flex-1">
            <input
              type="password"
              value={provider.apiKey}
              onChange={(e) => onChange({ apiKey: e.target.value })}
              placeholder="sk-..."
              className={INPUT_CLASS}
              style={INPUT_STYLE}
            />
          </div>
          <button
            onClick={() => void runTest()}
            disabled={busy || test.status === "testing"}
            className="flex-shrink-0 px-2.5 py-1.5 rounded text-xs flex items-center gap-1.5 border hover:opacity-90 disabled:opacity-60"
            style={{
              color: "var(--text-secondary)",
              background: "var(--bg-primary)",
              borderColor: "var(--border)",
            }}
            title="验证 Base URL 与 API Key 是否可用（GET /models，免费）"
          >
            {test.status === "testing" ? (
              <Loader2 size={12} className="animate-spin" />
            ) : (
              <Unplug size={12} />
            )}
            测试连通性
          </button>
        </div>
        {test.status === "ok" && (
          <p
            className="text-xs mt-1.5 flex items-center gap-1"
            style={{ color: "#4ade80" }}
          >
            <CheckCircle2 size={12} className="flex-shrink-0" />
            连接成功 · {test.latencyMs}ms
          </p>
        )}
        {test.status === "fail" && (
          <p
            className="text-xs mt-1.5 flex items-start gap-1 max-h-20 overflow-auto break-all"
            style={{ color: "#fca5a5" }}
          >
            <XCircle size={12} className="mt-0.5 flex-shrink-0" />
            <span>{test.message}</span>
          </p>
        )}
      </Field>

      <Field label={`模型（已选 ${provider.models.length} 个）`}>
        <div className="flex items-center gap-2">
          <button
            onClick={() => void fetchModels()}
            disabled={busy || fetching}
            className="px-2.5 py-1 rounded text-xs flex items-center gap-1.5 border hover:opacity-90 disabled:opacity-60"
            style={{
              color: "var(--text-secondary)",
              background: "var(--bg-primary)",
              borderColor: "var(--border)",
            }}
            title="从供应商拉取可用模型列表（GET /models）"
          >
            {fetching ? (
              <Loader2 size={12} className="animate-spin" />
            ) : (
              <RefreshCw size={12} />
            )}
            获取模型列表
          </button>
          <span
            className="text-[11px]"
            style={{ color: "var(--text-muted)" }}
          >
            勾选要使用的模型，可设置昵称
          </span>
        </div>
        {fetchError && (
          <p
            className="text-xs mt-1.5 flex items-start gap-1 max-h-20 overflow-auto break-all"
            style={{ color: "#fca5a5" }}
          >
            <XCircle size={12} className="mt-0.5 flex-shrink-0" />
            <span>获取失败：{fetchError}</span>
          </p>
        )}
        {rows.length === 0 && !fetchError && (
          <p
            className="text-xs mt-2"
            style={{ color: "var(--text-muted)" }}
          >
            点击「获取模型列表」拉取，或手动添加模型 ID
          </p>
        )}
        <div className="mt-2 space-y-1">
          {rows.map(({ id, fromServer }) => {
            const sel = provider.models.find((m) => m.id === id);
            return (
              <div
                key={id}
                className="flex items-center gap-2 px-2 py-1.5 rounded border"
                style={{
                  background: "var(--bg-primary)",
                  borderColor: "var(--border)",
                }}
              >
                {fromServer ? (
                  <input
                    type="checkbox"
                    checked={!!sel}
                    onChange={() => toggleModel(id)}
                    className="w-3.5 h-3.5 flex-shrink-0 accent-[var(--accent)]"
                    title="勾选 = 使用该模型"
                  />
                ) : (
                  <span
                    className="text-[10px] px-1 py-0.5 rounded flex-shrink-0"
                    style={{
                      background: "var(--bg-tertiary)",
                      color: "var(--text-muted)",
                    }}
                  >
                    手动
                  </span>
                )}
                <span
                  className="text-xs flex-1 min-w-0 truncate"
                  style={{ color: "var(--text-primary)" }}
                  title={id}
                >
                  {id}
                </span>
                {sel && (
                  <input
                    value={sel.nickname ?? ""}
                    onChange={(e) => setNickname(id, e.target.value)}
                    placeholder="昵称（可选）"
                    className="text-xs rounded px-1.5 py-0.5 outline-none w-32 flex-shrink-0 focus:ring-1 focus:ring-[var(--accent)]"
                    style={{
                      color: "var(--text-primary)",
                      background: "var(--input-bg)",
                      border: "1px solid var(--input-border)",
                    }}
                    title="显示昵称，替代长模型 ID"
                  />
                )}
                {sel && (
                  <button
                    onClick={() => removeModel(id)}
                    className="flex-shrink-0 hover:opacity-80"
                    style={{ color: "var(--text-muted)" }}
                    title="移除模型"
                  >
                    <X size={12} />
                  </button>
                )}
              </div>
            );
          })}
        </div>
        <div className="mt-2 flex items-center gap-1.5">
          <input
            type="text"
            value={manualDraft}
            onChange={(e) => setManualDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") addManual();
            }}
            placeholder="手动添加模型 ID（如模型名不在列表中）"
            className="flex-1 text-xs rounded px-2 py-1 outline-none focus:ring-1 focus:ring-[var(--accent)]"
            style={{
              color: "var(--text-secondary)",
              background: "var(--input-bg)",
              border: "1px solid var(--input-border)",
            }}
          />
          <button
            onClick={addManual}
            className="flex-shrink-0 px-2 py-1 rounded text-xs flex items-center gap-1 border hover:opacity-90"
            style={{
              color: "var(--text-secondary)",
              background: "var(--bg-primary)",
              borderColor: "var(--border)",
            }}
          >
            <Plus size={12} /> 添加
          </button>
        </div>
      </Field>

      <div
        className="flex items-center justify-end pt-3 border-t"
        style={{ borderColor: "var(--border)" }}
      >
        <button
          onClick={onRemove}
          className="px-3 py-1.5 rounded text-red-400 text-sm hover:bg-red-900/30"
        >
          删除
        </button>
      </div>
    </div>
  );
}

function Field({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <label className="block">
      <span className="text-xs" style={{ color: "var(--text-muted)" }}>
        {label}
      </span>
      <div className="mt-1">{children}</div>
    </label>
  );
}
