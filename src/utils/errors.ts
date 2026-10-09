/** 错误工具：工具层错误摘要与参数校验错误（宿主内通用，与具体领域无关）。 */

/** 工具参数校验失败（由各工具的 validate 抛出）。 */
export class ToolArgsError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ToolArgsError";
  }
}

/** 从 catch 到的未知抛出值提取可读消息（工具层错误摘要统一走此收敛）。 */
export function errText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}
