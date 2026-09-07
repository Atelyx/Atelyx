#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Atelyx 插件 Python 运行时（宿主注入，由 Rust 写入临时目录后执行）。

作用：加载插件 main（sys.path 注入插件目录），并向插件提供全局 `bridge` 对象——
插件在 main.py 顶层直接使用 bridge，无需 import（bridge 由本运行时注入全局命名空间）。

协议：与浏览器 worker 平面同一套 JSON 消息（见 docs/plugins/bridge-protocol.md）：
- 插件 → 宿主：{"kind":"call","seq","method","args"}
- 宿主 → 插件：{"kind":"reply","seq","ok","result"|"error"}
               {"kind":"invoke","seq","fnId","args","stream"?}
               {"kind":"stream","seq","event":"chunk"|"end"|"error","data"}
               {"kind":"event","event","payload"}

执行模型：单线程 + 重入式 readloop。`bridge.call` 阻塞时进入 `_pump(seq)` 逐行读 stdin
分派消息，直到本调用的 reply 到达——因此插件函数内再调 bridge 也不会死锁（嵌套 pump）。
顶层 main 执行完后进入 `_pump(None)` 常驻等待 invoke/event（宿主卸载时 kill 进程）。
"""
import sys
import json

# 与宿主的通信固定 UTF-8：默认简体中文 Windows（ACP=cp936）下管道编码是 GBK，
# 非 ASCII 内容会输出非 UTF-8 字节、被宿主读线程丢弃（通道断裂）。
for _stream in (sys.stdout, sys.stderr):
    try:
        _stream.reconfigure(encoding="utf-8")
    except Exception:  # noqa: BLE001 —— 无 reconfigure（极老解释器）时靠宿主 PYTHONUTF8=1 兜底
        pass

plugin_dir = sys.argv[1]
main_file = sys.argv[2]
sys.path.insert(0, plugin_dir)

_seq = 0
_fns = {}
_subs = []
_streams = {}


def _post(message):
    sys.stdout.write(json.dumps(_finite(message), ensure_ascii=False, allow_nan=False) + "\n")
    sys.stdout.flush()


def _finite(v):
    """非有限浮点（NaN/Infinity）→ None：json.dumps(allow_nan=False) 会拒绝，转 null 保通道。"""
    if isinstance(v, float):
        return None if (v != v or v in (float("inf"), float("-inf"))) else v
    if isinstance(v, dict):
        return {k: _finite(x) for k, x in v.items()}
    if isinstance(v, (list, tuple)):
        return [_finite(x) for x in v]
    return v


def _reg(fn):
    global _seq
    _seq += 1
    _fns["f%d" % _seq] = fn
    return "f%d" % _seq


def _serialize(v):
    if callable(v):
        return {"$fn": _reg(v)}
    if isinstance(v, dict):
        return {k: _serialize(x) for k, x in v.items()}
    if isinstance(v, (list, tuple)):
        return [_serialize(x) for x in v]
    return v


def _handle_invoke(m):
    fn = _fns.get(m.get("fnId"))
    seq = m.get("seq")
    if fn is None:
        _post({"kind": "reply", "seq": seq, "ok": False, "error": "unknown fn"})
        return
    ctx = {"aborted": False}
    if m.get("stream"):
        ctx["stream"] = {
            "chunk": lambda d, s=seq: _post({"kind": "stream", "seq": s, "event": "chunk", "data": d}),
            "end": lambda d=None, s=seq: _post({"kind": "stream", "seq": s, "event": "end", "data": d}),
            "error": lambda e, s=seq: _post({"kind": "stream", "seq": s, "event": "error", "data": str(e)}),
        }
    args = list(m.get("args") or []) + [ctx]
    try:
        result = fn(*args)
        _post({"kind": "reply", "seq": seq, "ok": True, "result": result})
    except Exception as e:  # noqa: BLE001 —— 插件异常回包给宿主，不杀死进程
        _post({"kind": "reply", "seq": seq, "ok": False, "error": str(e)})


def _pump(until_seq):
    """重入式 readloop：读 stdin 分派消息；until_seq 非 None 时读到该 reply 即返回其 result。"""
    while True:
        line = sys.stdin.readline()
        if not line:
            sys.exit(0)  # stdin 关闭 = 宿主已终止进程
        line = line.strip()
        if not line:
            continue
        try:
            m = json.loads(line)
        except Exception:  # noqa: BLE001
            continue
        kind = m.get("kind")
        if kind == "reply":
            if m.get("seq") == until_seq:
                if m.get("ok"):
                    return m.get("result")
                raise RuntimeError(m.get("error", "bridge error"))
            # 其他 reply（如流式 invoke 的尾随回包）忽略
        elif kind == "invoke":
            _handle_invoke(m)
        elif kind == "stream":
            s = _streams.get(m.get("seq"))
            if s:
                ev = m.get("event")
                if ev == "chunk":
                    s[0](m.get("data"))
                elif ev == "end":
                    s[1](m.get("data"))
                    _streams.pop(m.get("seq"), None)
                elif ev == "error":
                    s[2](m.get("data"))
                    _streams.pop(m.get("seq"), None)
        elif kind == "event":
            for cb in list(_subs):
                try:
                    cb(m.get("event"), m.get("payload"))
                except Exception:  # noqa: BLE001 —— 回调异常不影响其他订阅者
                    pass


def _call(method, args):
    global _seq
    _seq += 1
    s = _seq
    _post({"kind": "call", "seq": s, "method": method, "args": args})
    return _pump(s)


class _Bridge:
    @staticmethod
    def registerTool(defn):
        return _call("registerTool", [{
            "name": defn["name"],
            "description": defn.get("description", ""),
            "parameters": defn.get("parameters", {}),
            "parallelSafe": bool(defn.get("parallelSafe")),
            "executeId": _reg(defn["execute"]),
        }])

    @staticmethod
    def registerCommand(cmd):
        return _call("registerCommand", [{"id": cmd["id"], "label": cmd["label"], "runId": _reg(cmd["run"])}])

    @staticmethod
    def registerCapability(defn):
        method_ids = {k: _reg(v) for k, v in defn["methods"].items()}
        return _call("registerCapability", [{"namespace": defn["namespace"], "methodIds": method_ids}])

    @staticmethod
    def registerContribution(cont):
        return _call("registerContribution", [{
            "point": cont["point"],
            "id": cont.get("id"),
            "payload": _serialize(cont.get("payload", {})),
        }])

    @staticmethod
    def call(ns, method, args=None, opts=None):
        return _call("call", [ns, method, args or [], opts or {}])

    @staticmethod
    def callStream(ns, method, args, handlers):
        global _seq
        _seq += 1
        s = _seq
        _streams[s] = (handlers["chunk"], handlers["end"], handlers["error"])
        _post({"kind": "call", "seq": s, "method": "call", "args": [ns, method, args or [], {"stream": True}]})
        return lambda: _streams.pop(s, None)

    @staticmethod
    def on(event, cb):
        _subs.append(cb)
        return _call("subscribe", [event])

    @staticmethod
    def emit(topic, payload=None):
        return _call("emit", [topic, payload])

    @staticmethod
    def stateRead():
        return _call("stateRead", [])

    @staticmethod
    def stateWrite(data):
        return _call("stateWrite", [data])

    @staticmethod
    def ready():
        return _call("ready", [])


bridge = _Bridge()

with open(main_file, encoding="utf-8") as f:
    _main_source = f.read()
_globals = {"bridge": bridge, "__file__": main_file, "__name__": "__main__"}
exec(compile(_main_source, main_file, "exec"), _globals)

# main 执行完进入常驻 readloop，等待宿主 invoke/event（宿主卸载时 kill 本进程）。
_pump(None)
