/**
 * 工具注册表：把自包含 `ToolDefinition` 列表组织成「名册 → 模型」「参数 → 执行」的按名分发器。
 * 加工具 = 加一个 defineTool 模块 + 注册进列表，分发逻辑不动。
 */
import type {
  ToolDefinition,
  ToolExecContext,
  ToolExecResult,
  ToolResult,
  LlmMessage,
  LlmToolCall,
} from "@/types";
import { UNKNOWN_TOOL_MSG_PREFIX, MAX_PARALLEL_TOOL_CALLS } from "@/constants/tools";
import { errText } from "@/utils/errors";

/** 一次执行结束后的回填工具消息 + 可视化结果 + 原始结果（供调用方 hooks 消费 data 建产物）。 */
interface ToolDispatchResult {
  messages: LlmMessage[];
  results: ToolExecResult[];
  /** 已成功执行（未被中止跳过）的原始结果，含 data（画布据此建产物节点）。 */
  outcomes: Array<{ name: string; id: string; result: ToolResult }>;
}

/** 解析工具调用参数 JSON（非法抛 ToolArgsError，由 validate 决定更具体语义）。 */
function parseArgs(json: string): unknown {
  try {
    return JSON.parse(json);
  } catch {
    throw new Error("参数不是合法 JSON");
  }
}

export interface ToolRegistry {
  /** 运行前摘要（气泡工具块展示，容忍残缺参数）。 */
  summarize(name: string, argsJson: string): string;
  /**
   * 执行一轮工具调用：按 `parallelSafe` 切「连续并行段 + 有界滚动池」，非并行安全调用单独成段（屏障），
   * 结果按原调用顺序回填；中止后不再启动新调用（在飞者收敛）、跳过结果回填。
   * @param maxParallel 段内最大在飞数，缺省 `MAX_PARALLEL_TOOL_CALLS`。
   */
  dispatch(
    calls: LlmToolCall[],
    exec: ToolExecContext,
    maxParallel?: number,
  ): Promise<ToolDispatchResult>;
}

/** 由一组工具定义构建按名分发注册表。 */
export function createToolRegistry(defs: ToolDefinition[]): ToolRegistry {
  const byName = new Map(defs.map((d) => [d.name, d]));

  const summarize = (name: string, argsJson: string): string => {
    const def = byName.get(name);
    if (!def) return name;
    let args: unknown;
    try {
      args = def.validate(parseArgs(argsJson));
    } catch {
      // 残缺参数：降级为工具的通用摘要（工具块仍可见调用事实）
      args = {};
    }
    return def.summarize(args as Record<string, unknown>);
  };

  const dispatch = async (
    calls: LlmToolCall[],
    exec: ToolExecContext,
    maxParallel = MAX_PARALLEL_TOOL_CALLS,
  ): Promise<ToolDispatchResult> => {
    // 每个调用一个槽位：执行结果按下标回填，保证与模型 tool_calls 顺序一致（段序 + 段内序）。
    const slots: Array<
      | { message: LlmMessage; result: ToolExecResult; outcome: { name: string; id: string; result: ToolResult } }
      | undefined
    > = new Array(calls.length);

    const runCall = async (call: LlmToolCall, index: number): Promise<void> => {
      // 已中止：不启动（副作用不应在用户停止后再发生）；在飞调用收敛后跳过回填
      if (exec.signal.aborted) return;
      const def = byName.get(call.name);
      if (!def) {
        const msg = `${UNKNOWN_TOOL_MSG_PREFIX}${call.name}`;
        slots[index] = {
          message: { role: "tool", text: msg, toolCallId: call.id },
          result: { id: call.id, ok: false, summary: msg, detail: msg },
          outcome: { name: call.name, id: call.id, result: { ok: false, summary: msg } },
        };
        return;
      }
      // 参数校验（失败给错误 tool 消息，不执行）
      let args: unknown;
      try {
        args = def.validate(parseArgs(call.arguments));
      } catch (e) {
        const msg = `工具参数错误：${errText(e)}`;
        slots[index] = {
          message: { role: "tool", text: msg, toolCallId: call.id },
          result: { id: call.id, ok: false, summary: msg, detail: msg },
          outcome: { name: call.name, id: call.id, result: { ok: false, summary: msg } },
        };
        return;
      }
      // 执行（边界捕获：执行器异常降级为失败结果，不抛断整批）
      let result: ToolResult;
      try {
        result = await def.execute(args as Record<string, unknown>, exec);
      } catch (e) {
        result = { ok: false, summary: errText(e) };
      }
      // 用户已中止：副作用可能已发生，但结果不回填（引擎下一轮携已 abort 的 signal 收敛）
      if (exec.signal.aborted) return;
      slots[index] = {
        message: {
          role: "tool",
          text: (def.renderResult ?? ((r) => r.content ?? r.summary))(result),
          toolCallId: call.id,
        },
        result: {
          id: call.id,
          ok: result.ok,
          summary: result.summary,
          detail: result.content ?? result.summary,
        },
        outcome: { name: call.name, id: call.id, result },
      };
    };

    // 连续 parallelSafe 的调用成段（段内并发）；任一非 parallelSafe（含未知工具）单独成段（屏障）。
    const segments: Array<{ start: number; end: number; parallel: boolean }> = [];
    let i = 0;
    while (i < calls.length) {
      const parallel = byName.get(calls[i].name)?.parallelSafe ?? false;
      let j = i + 1;
      while (j < calls.length && (byName.get(calls[j].name)?.parallelSafe ?? false) === parallel) j++;
      segments.push({ start: i, end: j, parallel });
      i = j;
    }

    const cap = Math.max(1, maxParallel);
    for (const seg of segments) {
      // 非并行安全段：串行屏障，段内逐个执行（不可只跑第一个——连续多个读写/未知工具调用都要回填）
      if (!seg.parallel) {
        for (let k = seg.start; k < seg.end; k++) await runCall(calls[k], k);
        continue;
      }
      if (seg.end - seg.start <= 1) {
        await runCall(calls[seg.start], seg.start);
        continue;
      }
      // 有界滚动池（worker 模式）：中止后不再取新调用，在飞者自然收敛
      let next = seg.start;
      const workers = Array.from(
        { length: Math.min(cap, seg.end - seg.start) },
        async () => {
          while (next < seg.end && !exec.signal.aborted) {
            const idx = next++;
            await runCall(calls[idx], idx);
          }
        },
      );
      await Promise.all(workers);
    }

    const messages: LlmMessage[] = [];
    const results: ToolExecResult[] = [];
    const outcomes: ToolDispatchResult["outcomes"] = [];
    for (const slot of slots) {
      if (!slot) continue; // 中止跳过的调用不回填
      messages.push(slot.message);
      results.push(slot.result);
      outcomes.push(slot.outcome);
    }
    return { messages, results, outcomes };
  };

  return { summarize, dispatch };
}
