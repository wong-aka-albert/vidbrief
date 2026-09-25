//! 进程内状态。
//!
//! MCP 走 stdio 时是 **1 会话 : 1 进程**(`docs/DESIGN.md` §4.7),所以这里不需要
//! 跨进程共享 —— 但**进程内**有三件事必须有人管:
//!
//! 1. **写锁**:首次要改某个项目时取 `project.lock`,一直拿到进程结束。
//!    这样第二个 agent 会话在**第一次动手**时就知道"这项目有主了",
//!    而不是改到一半才发现自己的改动会被覆盖。
//! 2. **每项目串行**:agent 可能在同一轮里并发发几个写工具。它们都是同步阻塞的
//!    (读 JSON → 改 → 原子写),两个同时跑就是"后写的赢",前一个的结论凭空消失。
//! 3. **见过的项目**:`resources/list` 需要一份可枚举的清单,而项目可以建在任何路径
//!    (给了 `-p` 就不在 `projects/` 下),没有别的办法知道。

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use vidbrief_ops::OpenProject;
use vidbrief_project::layout::identity;
use vidbrief_project::lock::{LockGuard, acquire};

/// 本进程的会话标识。
///
/// **不再写进 `project.lock`**(那是个空哨兵,见 `vidbrief_project::lock` 的模块文档),
/// 只用来在日志与诊断里区分进程 —— stdio 下 1 会话 = 1 进程,pid 就够了。
pub fn session_name() -> String {
    format!("mcp#{}", std::process::id())
}

pub struct State {
    /// 已持有的写锁:项目根 → guard。**只有 `Some` 一种状态** ——
    /// 没有"只读登记"这回事,拿不到锁就是失败。
    ///
    /// 这张表不能省:OS 锁**同进程跨 handle 也生效**,所以同一个项目在同一个进程里
    /// 第二次取锁同样会被自己挡住 —— 必须先在这里查一次(键是 [`identity`])。
    locks: Mutex<HashMap<PathBuf, Arc<LockGuard>>>,
    /// 每项目一把的串行锁(进程内)。
    busy: Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>,
    /// 本会话碰过的项目:项目根 → 项目 id。
    ///
    /// 存 id 是为了让资源 URI 能用 `vidbrief://project/<id>/...` 这种**稳定**写法 ——
    /// 项目可以建在任何路径(`-p`),用路径当 URI 会又长又带盘符。
    seen: Mutex<BTreeMap<PathBuf, String>>,
    /// 建立会话时的工作目录 —— `projects/<id>` 这种相对路径要相对它解析,
    /// 而 MCP 客户端拉进程时给的工作目录不一定是我们想要的那个。
    cwd: PathBuf,
}

impl State {
    pub fn new() -> Self {
        Self {
            locks: Mutex::new(HashMap::new()),
            busy: Mutex::new(HashMap::new()),
            seen: Mutex::new(BTreeMap::new()),
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// 记下"这个会话碰过这个项目"。
    pub fn note(&self, dir: &Path, id: &str) {
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        seen.insert(identity(dir), id.to_string());
    }

    /// 本会话碰过的项目:`(项目根, 项目 id)`。
    pub fn known_projects(&self) -> Vec<(PathBuf, String)> {
        let seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        seen.iter().map(|(d, id)| (d.clone(), id.clone())).collect()
    }

    /// 按项目 id 找回项目根(资源 URI 走这条路;没碰过的就找不到)。
    pub fn dir_of(&self, id: &str) -> Option<PathBuf> {
        let seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        seen.iter()
            .find(|(_, known)| known.as_str() == id)
            .map(|(dir, _)| dir.clone())
    }

    /// 确保本进程持有该项目的写锁(已持有则直接返回)。
    ///
    /// **这是阻塞调用**(要开文件、上锁),必须放在 `spawn_blocking` 里。
    ///
    /// 键一律走 [`identity`]:调用方给的是 `e2e`、`projects/e2e` 还是绝对路径,
    /// 在这里必须收敛成同一个值 —— 否则同一个项目在内存里会有好几把"锁",
    /// 缓存不命中就会去真加锁,而 OS 锁是同进程跨 handle 也生效的,
    /// 结果是自己报"项目正被另一个会话写入",那个"另一个会话"就是它自己。
    pub fn ensure_write_lock(&self, opened: &OpenProject) -> Result<()> {
        let dir = identity(opened.dir());
        {
            let locks = self.locks.lock().unwrap_or_else(|e| e.into_inner());
            if locks.contains_key(&dir) {
                return Ok(());
            }
        }
        // 注意:这里**不能**持着 locks 的锁去 acquire —— 那是文件 IO,
        // 而其它线程可能正等着登记表。窄化临界区,代价只是极端情况下多开一次文件。
        let guard = acquire(opened.layout())?;
        let mut locks = self.locks.lock().unwrap_or_else(|e| e.into_inner());
        // 竞态兜底:两个工具调用同时进来时,后来者复用先进去的那个 guard
        locks.entry(dir).or_insert_with(|| Arc::new(guard));
        Ok(())
    }

    /// 本进程是否持有该项目的写锁。
    pub fn holds_lock(&self, dir: &Path) -> bool {
        let locks = self.locks.lock().unwrap_or_else(|e| e.into_inner());
        locks.contains_key(&identity(dir))
    }

    /// 拿到"这个项目的写操作"名额。
    ///
    /// 用法:`let _slot = state.serialize(dir).await;` —— 直到 `_slot` 析构为止,
    /// 同一项目上的其它写操作都在排队。
    pub async fn serialize(&self, dir: &Path) -> tokio::sync::OwnedMutexGuard<()> {
        let key = identity(dir);
        let slot = {
            let mut busy = self.busy.lock().unwrap_or_else(|e| e.into_inner());
            busy.entry(key)
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        slot.lock_owned().await
    }
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 不打印锁的内部状态:它是给别人看的诊断信息,不是这里要展示的东西
        f.debug_struct("State")
            .field("session", &session_name())
            .field("cwd", &self.cwd)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_name_carries_the_pid() {
        // 多会话 = 多进程,能一眼看出是哪个进程占着项目
        assert!(session_name().starts_with("mcp#"));
        assert!(session_name().contains(&std::process::id().to_string()));
    }

    #[test]
    fn seen_projects_start_empty_and_remember() {
        let state = State::new();
        assert!(state.known_projects().is_empty());
        state.note(Path::new("E:/a"), "a");
        state.note(Path::new("E:/b"), "b");
        state.note(Path::new("E:/a"), "a");
        assert_eq!(state.known_projects().len(), 2, "同一个项目记两次只算一个");
        assert_eq!(state.dir_of("b"), Some(PathBuf::from("E:/b")));
        assert_eq!(state.dir_of("没有这个"), None);
    }

    #[test]
    fn serialize_is_per_project() {
        let state = State::new();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            let a = state.serialize(Path::new("E:/a")).await;
            // 另一个项目不该被挡住
            let b = state
                .serialize(Path::new("E:/b"))
                .await;
            drop((a, b));
        });
    }
}
