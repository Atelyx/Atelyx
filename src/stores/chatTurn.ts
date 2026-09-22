/**
 * AI 对话核心运行时：一轮对话的公共编排（AI 对话面板、画布对话节点与插件共用）。
 *
 * 引擎（`stores/streaming.ts`）负责流式与工具循环，本模块负责「请求怎么组、结果怎么收」：
 * 解析 Agent 的提示词与工具名册 → 组装请求历史（压缩切分 + 尾部上下文块）→ 跑一轮 →
 * 收尾判定（空回复移除 / 超时降级 / 叙述提升 / 截断提示）→ 话题命名。
 *
 * 不持有消息容器、不落盘：产出经调用方传入的 `ChatTurnSink` 交回，容器与落盘留在消费方
 * （面板会话数组 / 画布节点消息表 / 插件自有容器），故本模块不感知容器形状——
 * 存在性守卫、协作与落盘调度都在消费方的写入器里。会话压缩同样只出摘要，注解写回由消费方负责。
 */
import { toLlmMessages } from "@/services/ai/client";
import { abortAutoTitle } from "@/services/ai/autoTitle";
import { runCompaction } from "@/services/ai/compaction";
import { fetchWeb } from "@/services/web";
import { emitPluginEvent } from "@/services/cordis/events";
import { runAgentTools, assembleAgentSystemPrompt } from "@/services/ai/tools";
import { runSearch } from "@/services/search";
import { readHistoryForAgent, recordAgentFileWrite } from "@/services/history";
import {
  appendVaultFile,
  editVaultFile,
  globVault,
  grepVault,
  listVaultDir,
  readVaultFileWindow,
  writeVaultFile,
} from "@/services/vault/aiFiles";
import { currentTodosBlock, readAgentTodos, writeAgentTodos } from "@/services/vault/agentTodos";
import {
  appendNarration,
  appendReasoning,
  fillAssistantReplyText,
  finalizeReplyText,
  mergeToolRuns,
} from "@/utils/agentSteps";
import { splitByCompaction } from "@/utils/compaction";
import { frameCompactionSummary } from "@/constants/compaction";
import { ERROR_PREFIX, TIMEOUT_ERROR_TEXT } from "@/constants/chat";
import { runStreamExchange, decideCleanup, runAutoNaming } from "./streaming";
import { useAppStore } from "./appStore";
import { useSettingsStore } from "./settingsStore";
import { useVaultStore } from "./vaultStore";
import type {
  AgentStep,
  ChatAutoNameOptions,
  ChatAutoNameResult,
  ChatCompactRequest,
  ChatCompactResult,
  ChatNamingTarget,
  ChatRuntime,
  ChatTargetResult,
  ChatTargetSelection,
  ChatTurnRequest,
  LlmMessage,
  ToolCapabilities,
} from "@/types";

/**
 * 当前打开笔记的尾部上下文块：随请求折叠进末条 user 消息线文（ephemeral，不入会话存储）。
 * 引导模型视相关性用 read_file 读取（read_file 随 Agent 勾选在名册内时生效；关闭后模型无读取能力）。
 */
function currentNoteContextBlock(file: string, title: string): string {
  const label = title ? `（${title}）` : "";
  return [
    "<context>",
    `用户当前打开的笔记：\`${file}\`${label}。若与本次对话相关，用 read_file 工具按此路径读取正文；读取之前不要声称已查看过该文件。`,
    "</context>",
  ].join("\n");
}

/** 尾部上下文块折叠进末条 user 消息（末条非 user 时另起一条，防空正文请求）。 */
function appendTailBlock(messages: LlmMessage[], block: string): void {
  const last = messages[messages.length - 1];
  if (last?.role === "user") last.text += `\n\n${block}`;
  else messages.push({ role: "user", text: block });
}

/**
 * 标准工具能力集：走宿主既有服务链（搜索 / 仓库文件读写 / 历史 / 任务清单 / 抓网页），
 * `targetId` 绑定任务清单侧车（清单随会话/对话节点各存一份）。
 * 消费方经 `hooks.capabilities` 覆盖需要改的方法（同名覆盖），不必重写整套能力。
 */
function standardToolCapabilities(targetId: string): ToolCapabilities {
  return {
    search: (query) => {
      const s = useSettingsStore.getState();
      return runSearch(s.searchConfig, query, s.tavilyKey);
    },
    readFile: (path, opts) => readVaultFileWindow(path, opts),
    glob: (pattern, opts) => globVault(pattern, opts),
    grep: (pattern, opts) => grepVault(pattern, opts),
    listDir: (dir) => listVaultDir(dir),
    renameFile: (oldPath, newName) => useVaultStore.getState().renameFile(oldPath, newName),
    moveFile: (oldPath, targetDir) => useVaultStore.getState().moveFile(oldPath, targetDir),
    deleteFile: (path) => useVaultStore.getState().deleteFile(path),
    deleteDir: (dir, force) =>
      useVaultStore.getState().deleteFolder(dir, force).then((r) => ({
        ok: r.deleted,
        summary: r.deleted
          ? `已删除目录「${dir}」`
          : r.needsConfirm
            ? `目录非空（${r.itemCount} 项）`
            : "删除目录失败",
        needsConfirm: r.needsConfirm,
        itemCount: r.itemCount,
      })),
    readHistory: (path, opts) => readHistoryForAgent(path, opts),
    appendFile: (path, content) =>
      appendVaultFile(path, content).then((res) => {
        if (res.ok) void recordAgentFileWrite(path);
        return res;
      }),
    writeTodos: (todos) => writeAgentTodos(targetId, todos),
    writeFile: (path, content) =>
      writeVaultFile(path, content).then(() => {
        // Agent 协作历史：AI 写文件以 Agent 身份记入对应 kind 的历史（fire-and-forget）
        void recordAgentFileWrite(path, content);
        return { ok: true, summary: `已写入「${path}」` };
      }),
    editFile: (path, edits) =>
      editVaultFile(path, edits).then((res) => {
        if (res.ok) void recordAgentFileWrite(path);
        return res;
      }),
    fetchUrl: fetchWeb,
  };
}

/**
 * 跑一轮对话：追加历史由消费方完成，本函数只组请求、跑引擎、收尾、命名。
 * 流式增量与收尾结果经 `req.sink` 交回消费方容器；命名在轮末 fire-and-forget（不阻塞返回）。
 * 出口唯一：**要么 finish 要么 fail**——引擎内部失败经 onError 上报，编排侧失败（Agent 提示词与
 * 任务清单读盘、请求组装等）由本函数的兜底一并交回，消费方的流式态与错误占位必有归宿。
 */
export async function runChatTurn(req: ChatTurnRequest): Promise<void> {
  // 让路：中止同目标的在途命名请求（防其占用后端槽位与新消息排队；不误伤其他目标）
  abortAutoTitle(req.targetId);
  // 本轮是否已经写入器交回结果：兜底只在「什么都没交回」时补一次，不覆盖已上报的结论
  const flow = { settled: false };
  try {
    await performTurn(req, flow);
  } catch (e) {
    if (flow.settled) return;
    // 失败不得静默：编排侧异常同样如实交回（消费方据此写错误占位并复位流式态）
    req.sink.fail(e instanceof Error ? e : new Error(String(e)));
    emitPluginEvent("chat:finished", { targetId: req.targetId });
  }
}

/** 一轮对话的实际编排（异常统一交 runChatTurn 的兜底出口）。 */
async function performTurn(req: ChatTurnRequest, flow: { settled: boolean }): Promise<void> {
  const { targetId, history, sink, naming } = req;

  // 系统提示词 + 工具名册：按 Agent 实时解析（配置在 设置 → Agent，引用提示词笔记实时读正文注入）。
  // 缺省（未选 Agent）= 预置「对话」；Agent 缺失（已删）降级为普通对话。
  const agentReq = await useSettingsStore.getState().resolveAgentRequest(req.agentId);
  const tools = agentReq?.tools ?? [];
  if (agentReq?.skippedWebSearch) {
    sink.notice("未配置搜索源（设置 → 联网搜索），本次对话未启用联网搜索");
  }

  // 历史：叙述-only 消息（content 为空、正文在 steps）先回填 content（空 content 会被部分端点判 400）；
  // 过滤 system 与错误占位 assistant（[错误] 不进 API 历史，避免污染上下文），空正文且无步骤的占位也剔除。
  // 压缩注解按原始列表定位锚点（与标记行同口径）：锚点及其之前不进请求。
  const { kept, checkpoint } = splitByCompaction(history, req.compaction);
  const apiHistory = kept
    .map(fillAssistantReplyText)
    .filter(
      (m) =>
        m.role !== "system" &&
        !(
          m.role === "assistant" &&
          (m.content.startsWith(ERROR_PREFIX) ||
            (m.content === "" && !m.steps?.length))
        ),
    );

  // 系统提示词：Agent 提示词 + 引用文件读取引导（工具含 read_file 时追加「@引用 文件用 read_file 读取」）。
  // 易变上下文（当前笔记/任务清单）走尾部 user 消息块，不进系统提示词（保前缀缓存命中）。
  const systemText = assembleAgentSystemPrompt(agentReq?.systemPrompt, tools);
  const apiMessages: LlmMessage[] = [
    ...(systemText ? [{ role: "system" as const, text: systemText }] : []),
    ...(checkpoint
      ? [{ role: "user" as const, text: frameCompactionSummary(checkpoint.summary) }]
      : []),
    ...toLlmMessages(apiHistory),
  ];
  // 当前打开的笔记（消费方声明生效时）：仅当名册含 read_file 才注入（模型才有能力读取）。
  // 块不落历史（ephemeral），历史逐字节稳定复现 → 前缀缓存命中至该消息原文。
  if (req.includeCurrentNote) {
    const { currentNoteFile, currentNoteTitle } = useAppStore.getState();
    if (currentNoteFile && tools.some((t) => t.name === "read_file")) {
      appendTailBlock(apiMessages, currentNoteContextBlock(currentNoteFile, currentNoteTitle));
    }
  }
  // 当前任务清单（todo_write 工具开启时）同以尾部上下文块带出：清单不变则逐字稳定 → 前缀缓存命中；
  // 清单变化（模型更新过 todo）只影响尾块 token，不打断系统前缀缓存
  if (tools.some((t) => t.name === "todo_write")) {
    const block = currentTodosBlock(await readAgentTodos(targetId));
    if (block) appendTailBlock(apiMessages, block);
  }

  // 工具能力：标准集 + 消费方覆盖（同名覆盖，其余保留标准实现）
  const standard = standardToolCapabilities(targetId);
  const overrides = req.hooks?.capabilities?.(standard);
  const capabilities: ToolCapabilities = overrides ? { ...standard, ...overrides } : standard;

  // 领域事件（状态已由消费方提交，订阅方读到的是含本轮 user 消息的最新状态）：轮次开始 + 本轮 user 消息
  emitPluginEvent("chat:started", { targetId });
  const lastUser = [...history].reverse().find((m) => m.role === "user");
  if (lastUser) {
    emitPluginEvent("chat:message", {
      targetId,
      role: "user",
      content: lastUser.displayContent ?? lastUser.content,
    });
  }

  // 步骤装配在核心完成（思考/叙述/工具交错，快照交回消费方）：消费方不重复实现合并规则，
  // 收尾判定读到的 steps 与消费方容器里的完全一致
  let steps: AgentStep[] = [];
  let content = "";

  await runStreamExchange({
    provider: req.target.provider,
    model: req.target.model,
    ...(req.reasoningEffort ? { reasoningEffort: req.reasoningEffort } : {}),
    apiMessages,
    ...(tools.length ? { tools } : {}),
    signal: req.signal,
    applyBatch: ({ content: delta, reasoning }) => {
      if (delta) content += delta;
      // 思考增量流入 steps（最后思考步拼接 / 工具轮之间自然分隔）
      if (reasoning) steps = appendReasoning(steps, reasoning);
      sink.update({ content, steps });
    },
    // 工具调用过程可视化：全量累积 runs 合并进 steps（思考→工具交错）
    onToolRuns: (runs) => {
      steps = mergeToolRuns(steps, runs);
      sink.update({ content, steps });
    },
    // 工具轮叙述正文进 steps（渲染为该步的叙述行）
    onNarration: (text) => {
      steps = appendNarration(steps, text);
      sink.update({ content, steps });
    },
    onError: (err) => {
      // 不静默降级：消费方写 [错误] 占位（下次请求历史过滤，不污染上下文）
      flow.settled = true;
      sink.fail(err);
      emitPluginEvent("chat:finished", { targetId });
    },
    onDone: ({ reasoning, timedOut, truncated, promoteNarration }) => {
      // onDone 最终化：最终回答轮叙述提升进 content + 输出上限截断提示
      flow.settled = true;
      const finalized = finalizeReplyText({ content, steps, promoteNarration, truncated });
      const decision = decideCleanup(
        finalized.content,
        reasoning,
        timedOut,
        finalized.steps.length > 0,
      );
      const removed = decision.kind === "remove";
      sink.finish({
        content:
          decision.kind === "timeout-error"
            ? `${ERROR_PREFIX} ${TIMEOUT_ERROR_TEXT}`
            : finalized.content,
        steps: finalized.steps,
        removed,
        timedOut,
        aborted: req.signal.aborted,
      });
      // 领域事件：assistant 完成消息（保留分支才有最终内容）+ 轮次结束
      if (!removed && decision.kind !== "timeout-error" && finalized.content) {
        emitPluginEvent("chat:message", {
          targetId,
          role: "assistant",
          content: finalized.content,
        });
      }
      emitPluginEvent("chat:finished", { targetId });
    },
    executeTools: (calls) =>
      // 公共工具执行器；产物节点差异经 hooks.onToolResult 交消费方
      runAgentTools(
        calls,
        { signal: req.signal, capabilities },
        req.hooks?.onToolResult ? { onToolResult: req.hooks.onToolResult } : undefined,
      ),
  });

  // 轮末话题命名（成功与否都不阻塞返回；已命名的目标自动跳过）
  void runAutoNaming(naming, { key: targetId });
}

/**
 * 生成压缩摘要：把当前模型可见历史（旧摘要 + 未被旧注解覆盖的保留段）截到新边界重新总结。
 * 只出文本——注解写回容器、锚点复核都由消费方负责。
 */
export async function compactChatTurn(req: ChatCompactRequest): Promise<ChatCompactResult> {
  const { kept, checkpoint } = splitByCompaction(req.messages, req.compaction);
  const cutIdx = kept.findIndex((m) => m.id === req.upToMessageId);
  if (cutIdx < 0) {
    // 锚点已被回滚/分支丢弃：此时压缩出的摘要无处安放，如实报错不写死注解
    return { ok: false, aborted: false, message: "压缩失败：对话已被改动，请重试" };
  }
  const toSummarize: LlmMessage[] = [
    ...(checkpoint
      ? [{ role: "user" as const, text: frameCompactionSummary(checkpoint.summary) }]
      : []),
    ...toLlmMessages(kept.slice(0, cutIdx + 1)),
  ];
  // 与当前会话同源的工具名册：历史含工具消息时带上，请求结构才与最近一次真实请求一致
  const agentReq = await useSettingsStore.getState().resolveAgentRequest(req.agentId);
  const result = await runCompaction({
    baseUrl: req.target.provider.baseUrl,
    apiKey: req.target.provider.apiKey,
    model: req.target.model,
    messages: toSummarize,
    ...(agentReq?.tools.length ? { tools: agentReq.tools } : {}),
    signal: req.signal,
  });
  if (result.ok) {
    return {
      ok: true,
      summary: result.summary,
      providerId: req.target.provider.id,
      model: req.target.model,
    };
  }
  // aborted = 用户主动停止，调用方静默收尾；其余给出可重试提示
  if (result.reason === "aborted") return { ok: false, aborted: true, message: "压缩已中止" };
  return {
    ok: false,
    aborted: false,
    message:
      result.reason === "truncated"
        ? "压缩失败：摘要超出输出上限被截断，请重试"
        : result.reason === "empty"
          ? "压缩失败：模型未返回摘要内容，请重试"
          : `压缩失败：${result.error?.message ?? "请求出错"}`,
  };
}

/** 解析对话目标（未指定 = 跟随仓库默认；与画布/面板/插件同源，避免各自读配置）。 */
function resolveTarget(selection?: ChatTargetSelection | null): ChatTargetResult {
  return useSettingsStore.getState().resolveChatTarget(selection ?? null);
}

/** 话题命名（轮末自动命名与手动重新命名共用）。 */
function autoName(
  naming: ChatNamingTarget,
  targetId: string,
  opts?: ChatAutoNameOptions,
): Promise<ChatAutoNameResult> {
  // key = 目标标识：发送新消息/手动接管时只中止本目标的命名请求（不误伤其他目标）
  return runAutoNaming(naming, { ...opts, key: targetId });
}

/** 构造核心能力面（由对话核心插件在挂载时注册；停用/卸载即随 fiber 撤销）。 */
export function createChatRuntime(): ChatRuntime {
  return { resolveTarget, runTurn: runChatTurn, compact: compactChatTurn, autoName };
}
