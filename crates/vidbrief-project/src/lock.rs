//! `project.lock` —— 项目写锁。
//!
//! 格式见 `docs/schema/project_lock.schema.json`。并发规则见 `docs/DESIGN.md` §4.7:
//! 首个打开项目的会话取得**写锁**,其他会话只能**只读打开**。
//!
//! **M0 只定格式与类型**,"谁持有锁"的获取/释放/死锁检测随 M3(MCP 多会话真正出现时)一起实现。
//! 现在提前定格式,是为了让 M3 不必再改 schema。

use anyhow::Result;
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};

/// `project.lock`。存在即表示有会话正持有写锁。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectLock {
    /// 持有写锁的 MCP 进程 pid。进程已死 = 锁可回收(死锁检测)。
    pub pid: u32,
    /// 主机名。项目目录可能在共享盘上,pid 只在同一台机器上有意义。
    pub host: String,
    /// 会话标识,用于在提示信息里区分是"哪个会话"占着。
    pub session: String,
    pub opened_at: DateTime<FixedOffset>,
}

impl ProjectLock {
    pub fn new(session: impl Into<String>) -> Self {
        Self {
            pid: std::process::id(),
            host: hostname(),
            session: session.into(),
            opened_at: crate::now(),
        }
    }

    pub fn load(path: &std::path::Path) -> Result<Self> {
        crate::json::read(path)
    }

    pub fn save(&self, path: &std::path::Path) -> Result<()> {
        crate::json::write_atomic(path, self)
    }
}

/// 主机名。取不到时退回 `unknown`(锁的可用性不该因为拿不到主机名就失败)。
fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_lock_has_current_pid() {
        let lock = ProjectLock::new("mcp-test");
        assert_eq!(lock.pid, std::process::id());
        assert_eq!(lock.session, "mcp-test");
        assert!(!lock.host.is_empty());
    }

    #[test]
    fn round_trip() {
        let dir = std::env::temp_dir().join(format!("vidbrief-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("project.lock");

        let lock = ProjectLock::new("mcp-abc");
        lock.save(&path).unwrap();
        assert_eq!(ProjectLock::load(&path).unwrap(), lock);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
