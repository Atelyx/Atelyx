//! `process` 能力面的宿主侧支撑：插件托管子进程的记账与事件形态转换。
//! `process.spawn` 起的是**宿主进程**的子进程（不在插件进程树下），故必须按插件记账：
//! 插件停用 / 失败 / 卸载时只靠结束插件进程树收不到它们，得按 pid 逐个收尾。

use std::collections::HashSet;
use std::sync::Mutex;

use atelyx_plugin_protocol::method as proto;

/// 某插件托管的子进程 pid 集合（`kill_process_tree` 按 pid 收尾，pid 即记账的全部凭据）。
#[derive(Default)]
pub(crate) struct Spawned(Mutex<HashSet<u32>>);

impl Spawned {
    pub(crate) fn add(&self, pid: u32) {
        self.0.lock().unwrap().insert(pid);
    }

    pub(crate) fn contains(&self, pid: u32) -> bool {
        self.0.lock().unwrap().contains(&pid)
    }

    /// 摘除一个 pid；返回它此前是否在册（`process.kill` 只认本插件起过的进程）。
    pub(crate) fn remove(&self, pid: u32) -> bool {
        self.0.lock().unwrap().remove(&pid)
    }

    /// 取走全部在册 pid（停用 / 失败收尾用：取走即清空，重复收尾不会重复动作）。
    pub(crate) fn take_all(&self) -> Vec<u32> {
        self.0.lock().unwrap().drain().collect()
    }
}

/// 宿主进程事件 → 协议负载（两种形态一一对应，仅字段名按协议 camelCase）。
pub(crate) fn to_protocol_event(event: crate::plugin_process::ProcessEvent) -> proto::ProcessEvent {
    match event {
        crate::plugin_process::ProcessEvent::Stdout { data } => proto::ProcessEvent::Stdout { data },
        crate::plugin_process::ProcessEvent::Stderr { data } => proto::ProcessEvent::Stderr { data },
        crate::plugin_process::ProcessEvent::Terminated { code } => {
            proto::ProcessEvent::Terminated { code }
        }
        crate::plugin_process::ProcessEvent::Error { message } => {
            proto::ProcessEvent::Error { message }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_process::ProcessEvent as HostEvent;

    #[test]
    fn bookkeeping_add_remove_and_take_all() {
        let spawned = Spawned::default();
        spawned.add(11);
        spawned.add(22);
        assert!(spawned.contains(11));
        assert!(spawned.remove(11), "在册 pid 应可摘除");
        assert!(!spawned.remove(11), "重复摘除应报否（kill 只认在册 pid）");
        let mut rest = spawned.take_all();
        rest.sort();
        assert_eq!(rest, vec![22]);
        assert!(spawned.take_all().is_empty(), "取走即清空");
    }

    #[test]
    fn event_mapping_is_one_to_one() {
        assert_eq!(
            to_protocol_event(HostEvent::Stdout { data: "a\n".into() }),
            proto::ProcessEvent::Stdout { data: "a\n".into() }
        );
        assert_eq!(
            to_protocol_event(HostEvent::Stderr { data: "e".into() }),
            proto::ProcessEvent::Stderr { data: "e".into() }
        );
        assert_eq!(
            to_protocol_event(HostEvent::Terminated { code: Some(3) }),
            proto::ProcessEvent::Terminated { code: Some(3) }
        );
        assert_eq!(
            to_protocol_event(HostEvent::Error { message: "x".into() }),
            proto::ProcessEvent::Error { message: "x".into() }
        );
    }
}
