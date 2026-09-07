/**
 * 插件隔离上下文（Web Worker）与桥代理。
 *
 * 插件入口代码与一段桥代理源码拼接后作为 blob Worker 执行：worker 内没有 window /
 * `__TAURI_INTERNALS__` / invoke，插件只能经 `self.bridge` 与主线程通信——
 * 桥是插件触达 App 能力的唯一通道（无裸 IPC）。
 *
 * 桥是「能力注册表」模型：宿主与插件在同一注册表里对等提供能力——
 * - `registerCapability({ namespace, methods })`：插件定义能力（新命名空间），
 *   宿主与其他插件经 `call(namespace, method, args)` 调用，路由由宿主完成。
 * - `call` 支持流式（`callStream`）：宿主能力与插件能力都以 chunk/end/error 帧推送，
 *   流一定以 end/error 收尾（宿主侧契约见 bridge.ts）。
 * - `registerContribution({ point, id, payload })`：通用扩展点注册（现有 register* 是其特化）。
 * - `on`/`emit`：命名空间事件总线（`<pluginId>:<topic>` 跨插件）。
 *
 * 代理协议：
 * - 插件 → 主线程：`{ kind: "call", seq, method, args }`（bridge 方法调用），主线程回 `reply`；
 *   流式调用时主线程不回 reply，改发 `{ kind: "stream", seq, event, data }` 帧。
 * - 主线程 → 插件：`{ kind: "invoke", seq, fnId, args, stream? }` 运行插件注册的函数；
 *   stream=true 时注入 `ctx.stream`（chunk/end/error）供插件回推；插件回 `reply` 或 `stream`。
 * - `{ kind: "event", event, payload }` 事件投递。
 *
 * 注册类方法（registerTool 等）参数中的函数会被代理存为 fnId 引用，序列化只过描述信息；
 * registerContribution 的 payload 深度序列化：函数 → `{ $fn: fnId }`，宿主据此回指可调用引用。
 */

/** 桥方法白名单（自动暴露给插件的 bridge.<method> 入口；简单透传型方法走这里）。
 * 承载逻辑平面可表达的方法；UI 类贡献（panel/node/setting/theme/app/command）
 * 走主线程平面，不在此列。 */
export const BRIDGE_METHODS = [
  "stateRead",
  "stateWrite",
  "ready",
] as const;

export type BridgeMethod = (typeof BRIDGE_METHODS)[number];

/**
 * 插件运行时传输层：worker 平面（postMessage）与子进程平面（stdio 桥）共用同一接口。
 * 桥宿主只认这个接口——能力路由、审计、生命周期与传输无关；`onCrash` 由各传输
 * 自行上报（worker 的 uncaught error / 子进程退出）。
 */
export interface PluginTransport {
  /** 向插件投递消息（reply/stream/event/invoke）。 */
  post(message: unknown): void;
  /** 订阅插件消息（call/reply/stream）；返回退订函数。 */
  onMessage(handler: (message: unknown) => void): () => void;
  /** 订阅插件异常终止；返回退订函数。 */
  onCrash?(handler: (message: string) => void): () => void;
  dispose(): void;
}

/** 主线程 → 插件：运行插件注册的函数（stream=true 时插件可经 ctx.stream 回推流帧）。 */
export interface WorkerInvokeMessage {
  kind: "invoke";
  seq: number;
  fnId: string;
  args: unknown[];
  stream?: boolean;
}

/** 主线程 → 插件：事件投递。 */
export interface WorkerEventMessage {
  kind: "event";
  event: string;
  payload: unknown;
}

/** 插件 → 主线程：桥调用。 */
export interface WorkerCallMessage {
  kind: "call";
  seq: number;
  method: string;
  args: unknown[];
}

/** 流式帧（双向）：chunk 推数据、end 正常收尾、error 失败收尾。 */
export interface WorkerStreamMessage {
  kind: "stream";
  seq: number;
  event: "chunk" | "end" | "error";
  data: unknown;
}

/** 插件 → 主线程：注册工具/命令/能力/贡献/调用/订阅 的消息载荷。 */
export interface PluginToolSpec {
  name: string;
  description: string;
  parameters: Record<string, unknown>;
  parallelSafe?: boolean;
  executeId: string;
}

export interface PluginCommandSpec {
  id: string;
  label: string;
  runId: string;
}

/** 插件定义能力：namespace 为反向域名（必含点），methodIds 为方法名 → fnId。 */
export interface PluginCapabilitySpec {
  namespace: string;
  methodIds: Record<string, string>;
}

/** 通用扩展点注册：payload 深度序列化，可调用函数为 `{ $fn: fnId }`。 */
export interface PluginContributionSpec {
  point: string;
  id?: string;
  payload: unknown;
}

/**
 * 桥代理源码（经典 worker 脚本，拼接在插件代码之前）。
 * 注：代码刻意保持无依赖、ES5 风格——它跑在无 bundler 的 worker 全局里。
 */
export function buildProxySource(): string {
  const generic = BRIDGE_METHODS.map(
    (m) =>
      `bridge[${JSON.stringify(m)}]=function(){return call(${JSON.stringify(m)},Array.prototype.slice.call(arguments));};`,
  ).join("\n");
  return `(function(){
var seq=0;var pending={};var fns={};var subs=[];var streams={};
self.addEventListener("message",function(e){
  var m=e.data;if(!m||typeof m!=="object")return;
  if(m.kind==="reply"){var p=pending[m.seq];if(!p)return;delete pending[m.seq];m.ok?p[0](m.result):p[1](new Error(m.error||"bridge error"));}
  else if(m.kind==="stream"){var s=streams[m.seq];if(!s)return;if(m.event==="chunk"){if(s.chunk)s.chunk(m.data);}
    else if(m.event==="end"){if(s.end)s.end(m.data);delete streams[m.seq];}
    else if(m.event==="error"){if(s.error)s.error(m.data);delete streams[m.seq];}}
  else if(m.kind==="event"){for(var i=0;i<subs.length;i++)subs[i](m.event,m.payload);}
  else if(m.kind==="invoke"){var f=fns[m.fnId];if(!f){self.postMessage({kind:"reply",seq:m.seq,ok:false,error:"unknown fn"});return;}
    var ctx={aborted:false};
    if(m.stream)ctx.stream={chunk:function(d){self.postMessage({kind:"stream",seq:m.seq,event:"chunk",data:d});},
      end:function(d){self.postMessage({kind:"stream",seq:m.seq,event:"end",data:d});},
      error:function(e){self.postMessage({kind:"stream",seq:m.seq,event:"error",data:(e&&e.message)?e.message:String(e)});}};
    Promise.resolve().then(function(){return f.apply(null,(m.args||[]).concat([ctx]));}).then(function(r){self.postMessage({kind:"reply",seq:m.seq,ok:true,result:r});},
      function(err){self.postMessage({kind:"reply",seq:m.seq,ok:false,error:err&&err.message?err.message:String(err)});});}
});
function call(method,args){var s=++seq;return new Promise(function(res,rej){pending[s]=[res,rej];self.postMessage({kind:"call",seq:s,method:method,args:args||[]});});}
function reg(fn){var id="f"+(++seq);fns[id]=fn;return id;}
function serialize(v){if(typeof v==="function")return {$fn:reg(v)};
  if(Array.isArray(v)){var a=[];for(var i=0;i<v.length;i++)a.push(serialize(v[i]));return a;}
  if(v&&typeof v==="object"){var o={};for(var k in v){if(Object.prototype.hasOwnProperty.call(v,k))o[k]=serialize(v[k]);}return o;}
  return v;}
var bridge={};
bridge.registerTool=function(def){
  if(!def||typeof def.execute!=="function")return Promise.reject(new Error("registerTool 需要 { name, description, parameters, execute }"));
  return call("registerTool",[{name:def.name,description:def.description,parameters:def.parameters||{},parallelSafe:!!def.parallelSafe,executeId:reg(def.execute)}]);
};
bridge.registerCommand=function(cmd){
  if(!cmd||typeof cmd.run!=="function")return Promise.reject(new Error("registerCommand 需要 { id, label, run }"));
  return call("registerCommand",[{id:cmd.id,label:cmd.label,runId:reg(cmd.run)}]);
};
bridge.registerCapability=function(def){
  if(!def||typeof def.namespace!=="string"||!def.methods||typeof def.methods!=="object")return Promise.reject(new Error("registerCapability 需要 { namespace, methods }"));
  var methodIds={};
  for(var k in def.methods){if(typeof def.methods[k]==="function")methodIds[k]=reg(def.methods[k]);}
  return call("registerCapability",[{namespace:def.namespace,methodIds:methodIds}]);
};
bridge.registerContribution=function(cont){
  if(!cont||typeof cont.point!=="string")return Promise.reject(new Error("registerContribution 需要 { point, id, payload }"));
  return call("registerContribution",[{point:cont.point,id:cont.id,payload:serialize(cont.payload||{})}]);
};
bridge.call=function(ns,method,args,opts){
  if(typeof ns!=="string"||typeof method!=="string")return Promise.reject(new Error("call 需要 (namespace, method, args)"));
  return call("call",[ns,method,args||[],opts||{}]);
};
bridge.callStream=function(ns,method,args,handlers){
  if(!handlers||typeof handlers!=="object")return Promise.reject(new Error("callStream 需要 handlers { chunk, end, error }"));
  var s=++seq;
  streams[s]={chunk:handlers.chunk,end:handlers.end,error:handlers.error};
  self.postMessage({kind:"call",seq:s,method:"call",args:[ns,method,args||[],{stream:true}]});
  return function(){delete streams[s];};
};
bridge.on=function(event,cb){if(typeof cb!=="function")return Promise.reject(new Error("on 需要回调函数"));return call("subscribe",[event]).then(function(){subs.push(cb);});};
bridge.emit=function(topic,payload){return call("emit",[topic,payload]);};
${generic}
self.bridge=bridge;
})();
`;
}

/** 创建插件隔离上下文（blob Worker）；dispose 终止 worker 并释放 blob URL。 */
export function createPluginWorker(code: string): PluginTransport {
  const source = `${buildProxySource()}\n;\n${code}`;
  const url = URL.createObjectURL(new Blob([source], { type: "text/javascript" }));
  const worker = new Worker(url);
  return {
    post: (message) => worker.postMessage(message),
    onMessage: (handler) => {
      const onMsg = (e: MessageEvent<unknown>) => handler(e.data);
      worker.onmessage = onMsg;
      return () => {
        worker.onmessage = null;
      };
    },
    onCrash: (handler) => {
      worker.onerror = (e) => handler(e.message || "插件执行出错");
      worker.onmessageerror = () => handler("插件消息解析失败");
      return () => {
        worker.onerror = null;
        worker.onmessageerror = null;
      };
    },
    dispose: () => {
      worker.terminate();
      URL.revokeObjectURL(url);
    },
  };
}
