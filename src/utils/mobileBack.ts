/**
 * 安卓返回键的逐层返回栈：浮层、弹窗、展开的侧边栏等在其存在期间登记处理器，
 * 返回键按后进先出调用；全部未处理时由壳层决定上一级（视图内上级 → 回主页 → 二次确认退出）。
 *
 * 处理器返回 true = 已消费本次返回，链停止。桌面端永不调用此入口，登记为空副作用。
 */
type BackHandler = () => boolean;

const handlers: BackHandler[] = [];

/** 登记一个返回处理器，返回注销函数（幂等；重复登记录入多次、注销各自移除一份）。 */
export function registerBackHandler(handler: BackHandler): () => void {
  handlers.push(handler);
  let active = true;
  return () => {
    if (!active) return;
    active = false;
    const i = handlers.lastIndexOf(handler);
    if (i >= 0) handlers.splice(i, 1);
  };
}

/** 从后往前调用处理器，返回是否有处理器消费了本次返回。
 *  快照迭代：处理器内部会注销自身（关闭浮层），避免迭代中被修改的数组漏项。
 *  单个处理器抛错按未消费处理并记日志：异常不得让上层把「返回」升级为退出应用。 */
export function runBackHandlers(): boolean {
  const snapshot = [...handlers];
  for (let i = snapshot.length - 1; i >= 0; i--) {
    try {
      if (snapshot[i]()) return true;
    } catch (e) {
      console.error("返回键处理失败", e);
    }
  }
  return false;
}
