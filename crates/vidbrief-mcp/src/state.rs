//! 进程内状态。
//!
//! MCP 走 stdio 时是 **1 会话 : 1 进程**(`docs/DESIGN.md` §4.7),所以这里不需要
//! 跨进程共享 —— 但**进程内**有三件事必须有人管:
//!
//! 1. **写锁**:**按次取放** —— 一次写调用取、这次调用结束就放(`WriteHold` 的 Drop)。
//!    进程还握着的时候,别的会话动手会立刻拿到"这项目有主了",而不是改到一半才发现;
//!    而**空转的会话不再把项目占死**(从前是取到进程结束,一个 idle 会话能让别人一直写不进去)。
//!    两次调用之间被别人改过怎么办:靠 `rev` 乐观并发(`OpenProject::save` 会拦),见 DESIGN §4.7。
//! 2. **每项目串行**:agent 可能在同一轮里并发发几个写工具。它们都是同步阻塞的
//!    (读 JSON → 改 → 原子写),两个同时跑就是"后写的赢",前一个的结论凭空消失。
//! 3. **见过的项目**:`resources/list` 需要一份可枚举的清单,而项目可以建在任何路径
//!    (给了 `-p` 就不在 `projects/` 下),没有别的办法知道。

use std::collections::{BTreeMap, BTreeSet, HashMap};
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

/// 表里那一项:锁 + 有几个调用正拿它当凭证。
struct Held {
    guard: Arc<LockGuard>,
    users: usize,
}

/// **一次写调用的持锁凭证。** Drop 时把这一份引用计数交回去,数到 0 就释放 OS 锁。
///
/// 为什么要 RAII 而不是"记得调 `exit_write`":入口有两条(MCP 工具与页面按钮),
/// 而"改一处、忘了另一处"在这个仓库里发生过([260925-21],那时漏的是排队与警告)。
/// 凭证拿在手上,编译器就替我们盯着了。
pub struct WriteHold {
    state: Arc<State>,
    dir: PathBuf,
}

impl Drop for WriteHold {
    fn drop(&mut self) {
        self.state.release_write(&self.dir);
    }
}

pub struct State {
    /// 本进程**正在**持有的写锁:项目根 → (guard, 还有几个调用在用)。
    ///
    /// 这张表不能省:OS 锁**同进程跨 handle 也生效**,所以同一个项目在同一个进程里
    /// 第二次取锁同样会被自己挡住 —— 必须先在这里查一次(键是 [`identity`])。
    ///
    /// **引用计数是为了"按次取放"**:同一项目上可能有两次写调用重叠(一个正在写、
    /// 另一个已经进场在排队),谁先走完都不能把别人还攥着的那把锁放掉 ——
    /// 数到 0 才真放。
    locks: Mutex<HashMap<PathBuf, Held>>,
    /// 每项目一把的串行锁(进程内)。
    busy: Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>,
    /// 已经提醒过"写锁是降级放行"的项目。见 [`State::take_lock_warning`]。
    warned: Mutex<BTreeSet<PathBuf>>,
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
            warned: Mutex::new(BTreeSet::new()),
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
            let mut locks = self.locks.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(held) = locks.get_mut(&dir) {
                held.users += 1;
                return Ok(());
            }
        }
        // 注意:这里**不能**持着 locks 的锁去 acquire —— 那是文件 IO,
        // 而其它线程可能正等着登记表。窄化临界区,代价只是极端情况下多开一次文件。
        let guard = acquire(opened.layout())?;
        let mut locks = self.locks.lock().unwrap_or_else(|e| e.into_inner());
        // 竞态兜底:两个工具调用同时进来时,后来者复用先进去的那个 guard
        match locks.get_mut(&dir) {
            Some(held) => held.users += 1,
            None => {
                locks.insert(dir, Held { guard: Arc::new(guard), users: 1 });
            }
        }
        Ok(())
    }

    /// 一次写调用结束:交回一份引用计数,数到 0 就把锁放掉。
    ///
    /// **不是谁先走完谁放** —— 同一项目上两次调用可能重叠(一次在写、一次在排队),
    /// 先走的不能把后者的锁放掉。这也是 `Held::users` 存在的唯一理由。
    pub fn release_write(&self, dir: &Path) {
        let key = identity(dir);
        let mut locks = self.locks.lock().unwrap_or_else(|e| e.into_inner());
        let Some(held) = locks.get_mut(&key) else {
            return;
        };
        held.users = held.users.saturating_sub(1);
        if held.users == 0 {
            locks.remove(&key);
        }
    }

    /// 写操作的**入场**:确保持锁 + 记账,返回**这一趟的持锁凭证**。
    ///
    /// **两个入口都走它**:MCP 工具(见 `tools::open_write`)和页面按钮
    /// (见 `action_handler`)。理由和 `ops::with_write` 一样 ——
    /// 这条序列被漏掉过一次(按钮那条既没排队也没带警告,devlog [260925-21]),
    /// 而编译器不会提醒。
    ///
    /// 拿到的 [`WriteHold`] 必须**活到这次写做完**:中途 drop 就等于提前放锁。
    ///
    /// 排队(`serialize`)不在这里:它是 async 的,由调用方自己 await 或 block_on。
    pub fn enter_write(self: &Arc<Self>, opened: &OpenProject) -> Result<WriteHold> {
        self.ensure_write_lock(opened)?;
        self.note(opened.dir(), opened.id());
        Ok(WriteHold {
            state: Arc::clone(self),
            dir: opened.dir().to_path_buf(),
        })
    }

    /// 本进程持有的那把锁。
    ///
    /// 给 `vidbrief_ops::with_write` 当**凭证**用 —— 会话级持锁的入口不能在那里
    /// 重复取锁(同进程再取会被自己挡住)。
    pub fn guard_of(&self, dir: &Path) -> Option<Arc<LockGuard>> {
        let locks = self.locks.lock().unwrap_or_else(|e| e.into_inner());
        locks.get(&identity(dir)).map(|held| Arc::clone(&held.guard))
    }

    /// 本进程是否持有该项目的写锁。
    pub fn holds_lock(&self, dir: &Path) -> bool {
        let locks = self.locks.lock().unwrap_or_else(|e| e.into_inner());
        locks.contains_key(&identity(dir))
    }

    /// 若该项目的写锁是**降级放行**的(文件系统不支持锁),取走那句说明。
    ///
    /// **只给一次**:同一项目在同一个进程里第二次来问就是 `None`。
    /// 理由 —— 每个工具调用都刷一遍"你可能在覆盖别人的数据",用户会当耳边风;
    /// 而这句话恰恰是要被当真的。
    pub fn take_lock_warning(&self, dir: &Path) -> Option<String> {
        let key = identity(dir);
        let reason = {
            let locks = self.locks.lock().unwrap_or_else(|e| e.into_inner());
            locks.get(&key)?.guard.degraded().map(str::to_string)
        };
        let mut warned = self.warned.lock().unwrap_or_else(|e| e.into_inner());
        warned.insert(key).then_some(reason).flatten()
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
    use vidbrief_ops::testkit::Fake;

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


    /// **按次取放**:凭证 drop 之后锁就放了。
    ///
    /// 从前的口径是"首次取到进程结束",代价实测过:一个 **idle 会话**能让别的会话
    /// 一直写不进去,而 agent 自己无路可走。现在锁只活到这次写调用结束 ——
    /// "两次调用之间被别人改过"由 `rev` 乐观并发兜(`OpenProject::save`)。
    #[test]
    fn the_write_lock_lives_only_as_long_as_the_hold() {
        let fake = Fake::new("state-hold");
        let state = Arc::new(State::new());
        let opened = OpenProject::open(&fake.layout().root().display().to_string()).unwrap();
        let dir = opened.dir().to_path_buf();

        {
            let _hold = state.enter_write(&opened).expect("第一趟该拿到锁");
            assert!(state.holds_lock(&dir), "持有期间该查得到");
            // 同一项目上第二趟重叠(一个在写、一个在排队):不许互相把锁放掉
            let _second = state
                .enter_write(&opened)
                .expect("同进程再来一趟该复用,不该撞自己");
        }

        assert!(
            !state.holds_lock(&dir),
            "两趟都结束了,锁该放掉 —— 空转的会话不许把项目占死"
        );
        // 真放了吗:再取一次(同进程跨 handle 也会被自己挡,所以这能验出来)
        vidbrief_project::acquire(opened.layout()).expect("锁该已经放了,能重新取到");
    }

    /// 一个凭证还在手上时,锁**不许**被别人放掉。
    #[test]
    fn an_overlapping_hold_keeps_the_lock_alive() {
        let fake = Fake::new("state-overlap");
        let state = Arc::new(State::new());
        let opened = OpenProject::open(&fake.layout().root().display().to_string()).unwrap();
        let dir = opened.dir().to_path_buf();

        let first = state.enter_write(&opened).unwrap();
        let _second = state.enter_write(&opened).unwrap();
        drop(first);
        assert!(
            state.holds_lock(&dir),
            "还有一趟在写,锁就不能放 —— 计数到 0 才放"
        );
    }
}
