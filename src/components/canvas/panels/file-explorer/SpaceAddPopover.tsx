/**
 * 协作空间新增入口（文件面板工具条）：一个按钮收敛三条新增途径——在服务器上创建空间 /
 * 把服务器上已有文件夹就地纳管为空间 / 输邀请码加入。
 *
 * 点按钮弹出浮层：分段切换模式 + 该模式表单，确认后直接执行；创建与纳管成功后进入该空间，
 * 加入成功只提示（是否进入由用户在列表里决定）。无已登录服务器时浮层内引导前往设置。
 *
 * 可靠性约定：失败在浮层内显示且**不关浮层**（输入保留、可就地重试）；提交中确认按钮禁点
 * （防重复请求）；切换仓库进行中按钮禁用（与相邻的打开文件夹入口同一把闸）；目标服务器按
 * 当前登录态推导，选中的服务器中途登出不会拿旧地址发请求。
 *
 * 分层：只调 appStore / spaceAuthStore / spaceDirectoryStore，不直调 service。
 */
import { useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import { Cloud, FolderOpen, Loader2, Plus, Ticket } from "lucide-react";
import { PopupLayer } from "@/components/common/PopupLayer";
import { usePopupAnchor } from "@/hooks/usePopupAnchor";
import { useAppStore } from "@/stores/appStore";
import { useSpaceAuthStore } from "@/stores/spaceAuthStore";
import { useSpaceDirectoryStore } from "@/stores/spaceDirectoryStore";

/** 三种新增途径（顺序即分段标签顺序）。 */
const MODE_ORDER = ["create", "openFolder", "join"] as const;
type AddMode = (typeof MODE_ORDER)[number];

/** 一个模式的标签配置：短标签进分段条，说明与表单单列下方（标签条塞不下整句）。 */
interface ModeTab {
  label: string;
  icon: ReactNode;
  hint: string;
  placeholder: string;
  confirm: string;
}

const MODE_TABS: Record<AddMode, ModeTab> = {
  create: {
    label: "创建空间",
    icon: <Plus size={12} />,
    hint: "在服务器上新建一个空空间",
    placeholder: "空间名称",
    confirm: "创建并进入",
  },
  openFolder: {
    label: "打开文件夹",
    icon: <FolderOpen size={12} />,
    hint: "把服务器上已有文件夹就地纳管为空间（文件不搬移）",
    placeholder: "服务器上文件夹的绝对路径，如 /mnt/team-library",
    confirm: "打开并进入",
  },
  join: {
    label: "输入邀请码",
    icon: <Ticket size={12} />,
    hint: "用邀请码加入已有空间",
    placeholder: "邀请码",
    confirm: "加入",
  },
};

/** 默认模式：每次打开浮层都从这里开始（不留上一次的模式与输入）。 */
const DEFAULT_MODE: AddMode = "create";

/** 服务器上文件夹的绝对路径（纳管模式唯一接受的形态；服务端按此路径就地纳管）。 */
function isServerPath(value: string): boolean {
  return value.startsWith("/") && value !== "/";
}

export function SpaceAddPopover({ onNotice }: { onNotice: (message: string) => void }) {
  const selectSpace = useAppStore((s) => s.selectSpace);
  const openSettings = useAppStore((s) => s.openSettings);
  const switchingTo = useAppStore((s) => s.switchingVaultRoot);
  const authServers = useSpaceAuthStore((s) => s.servers);
  const createSpace = useSpaceDirectoryStore((s) => s.createSpace);
  const acceptInvite = useSpaceDirectoryStore((s) => s.acceptInvite);

  // 服务器地址清单（selector 只回原数组引用，派生在 useMemo 防每次渲染新引用）
  const servers = useMemo(() => authServers.map((e) => e.serverUrl), [authServers]);

  const [mode, setMode] = useState<AddMode>(DEFAULT_MODE);
  const [draft, setDraft] = useState("");
  const [serverPick, setServerPick] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const triggerRef = useRef<HTMLButtonElement>(null);
  const { anchor, toggle, close } = usePopupAnchor(triggerRef);

  // 目标服务器按当前登录态推导（而非留存选中值）：选中的服务器中途登出时回落首个可用服务器
  const targetServer = servers.includes(serverPick) ? serverPick : (servers[0] ?? "");
  const tab = MODE_TABS[mode];

  /** 打开浮层：回默认模式并清掉上一次的草稿与错误。busy 不动——在途请求的提交中必须保持真实。 */
  const open = () => {
    if (!anchor) {
      setMode(DEFAULT_MODE);
      setDraft("");
      setError(null);
    }
    toggle();
  };

  /** 切换模式：三种模式的输入语义不同，草稿不跨模式沿用。 */
  const switchMode = (next: AddMode) => {
    setMode(next);
    setDraft("");
    setError(null);
  };

  const submit = async () => {
    if (busy) return;
    const value = draft.trim();
    if (!value) return;
    if (mode === "openFolder" && !isServerPath(value)) {
      setError("请输入服务器上文件夹的绝对路径（以 / 开头）");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      if (mode === "join") {
        await acceptInvite(targetServer, value);
        close();
        onNotice("已加入协作空间");
        return;
      }
      // 纳管模式：空间名取路径末段，路径作内容根交给服务端就地纳管（不搬移文件）
      const opening = mode === "openFolder";
      const name = opening ? (value.split("/").filter(Boolean).pop() ?? value) : value;
      const created = await createSpace(targetServer, name, opening ? value : undefined);
      // 先收浮层再进空间：进入 = 整页切换，浮层不留在切换过程中
      close();
      await selectSpace({ serverUrl: targetServer, spaceId: created.spaceId, name: created.name });
    } catch (e) {
      console.error(mode === "join" ? "加入协作空间失败" : mode === "openFolder" ? "打开服务器文件夹失败" : "创建空间失败", e);
      const prefix = mode === "join" ? "加入失败" : mode === "openFolder" ? "打开文件夹失败" : "创建空间失败";
      setError(`${prefix}：${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <button
        ref={triggerRef}
        onClick={open}
        disabled={switchingTo !== null}
        className="relative flex items-center justify-center w-7 h-7 rounded hover:bg-[var(--hover)] disabled:opacity-40"
        style={{ color: "var(--text-muted)" }}
        title="新增协作空间"
      >
        <Cloud size={15} />
        {/* 云右下角叠加号：lucide 无 cloud-plus，空间在全应用的既有语汇就是云（列表层空间条目同图标） */}
        <span
          className="absolute flex items-center justify-center rounded-full"
          style={{ right: 4, bottom: 3, width: 10, height: 10, background: "var(--bg-secondary)" }}
        >
          <Plus size={8} strokeWidth={2.5} />
        </span>
      </button>

      <PopupLayer
        anchor={anchor}
        onClose={close}
        triggerRef={triggerRef}
        widthClass="w-[300px]"
        contentClassName="p-3"
        zClass="z-[1100]"
        repositionDeps={[mode, error]}
      >
        <div className="text-xs font-medium mb-2" style={{ color: "var(--text-primary)" }}>
          新增协作空间
        </div>

        {servers.length === 0 ? (
          <div className="flex flex-col items-start gap-2 text-xs" style={{ color: "var(--text-muted)" }}>
            <span className="flex items-center gap-1.5">
              <Cloud size={13} />
              尚未连接协作服务器
            </span>
            <button
              onClick={() => {
                close();
                openSettings("collab");
              }}
              className="px-2 py-1 rounded border hover:opacity-80"
              style={{ borderColor: "var(--border)", color: "var(--text-secondary)" }}
            >
              连接服务器
            </button>
          </div>
        ) : (
          <form
            onSubmit={(e) => {
              e.preventDefault();
              void submit();
            }}
          >
            <div className="flex items-center gap-0.5 p-0.5 rounded mb-2" style={{ background: "var(--bg-tertiary)" }}>
              {MODE_ORDER.map((m) => {
                const t = MODE_TABS[m];
                const active = m === mode;
                return (
                  <button
                    key={m}
                    type="button"
                    aria-pressed={active}
                    onClick={() => switchMode(m)}
                    className="flex-1 flex items-center justify-center gap-1 px-1 py-1 rounded text-[11px]"
                    style={active ? { background: "var(--accent)", color: "var(--accent-fg)" } : { color: "var(--text-muted)" }}
                  >
                    {t.icon}
                    {t.label}
                  </button>
                );
              })}
            </div>

            <p className="text-[11px] mb-2" style={{ color: "var(--text-muted)" }}>
              {tab.hint}
            </p>

            {servers.length > 1 && (
              // 原生 select：DropdownSelect 的弹层挂在 body 另一棵 portal 上，其内部点击会被本浮层的外点关闭吃掉
              <select
                value={targetServer}
                onChange={(e) => setServerPick(e.target.value)}
                className="w-full text-xs rounded px-2 py-1 mb-2 outline-none"
                style={{ background: "var(--input-bg)", color: "var(--text-primary)", border: "1px solid var(--input-border)" }}
                title="目标服务器"
              >
                {servers.map((s) => (
                  <option key={s} value={s}>
                    {s}
                  </option>
                ))}
              </select>
            )}

            <input
              autoFocus
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              placeholder={tab.placeholder}
              className="w-full text-xs rounded px-2 py-1.5 outline-none"
              style={{ background: "var(--input-bg)", color: "var(--text-primary)", border: "1px solid var(--input-border)" }}
            />

            {error && (
              <div className="mt-1.5 text-[11px] break-all" style={{ color: "#f87171" }}>
                {error}
              </div>
            )}

            <div className="mt-2.5 flex justify-end gap-2">
              <button
                type="button"
                onClick={close}
                className="px-2.5 py-1 text-xs rounded hover:bg-[var(--hover)]"
                style={{ color: "var(--text-secondary)" }}
              >
                取消
              </button>
              <button
                type="submit"
                disabled={busy || !draft.trim()}
                className="px-2.5 py-1 text-xs rounded flex items-center gap-1.5 disabled:opacity-50"
                style={{ background: "var(--accent)", color: "var(--accent-fg)" }}
              >
                {busy && <Loader2 size={12} className="animate-spin" />}
                {tab.confirm}
              </button>
            </div>
          </form>
        )}
      </PopupLayer>
    </>
  );
}
