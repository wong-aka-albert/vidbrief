//! `project.lock` —— 项目写锁。
//!
//! 格式见 `docs/schema/project_lock.schema.json`。并发规则见 `docs/DESIGN.md` §4.7:
//! 首个打开项目的会话取得**写锁**,其他会话只能**只读打开**。
//!
//! ## 谁持有锁,由操作系统的文件锁说了算
//!
//! [`acquire`] 打开(不存在则创建)`project.lock` 并对它加**独占锁**
//! ([`File::try_lock`]:Windows 上落到 `LockFileEx`,Unix 上落到 `flock`),锁上了谁就是持有者;
//! [`LockGuard`] 被 drop 时解锁(RAII)。文件里那份 JSON([`ProjectLock`])只是**给人看的诊断信息**
//! —— 它记录的是"文件里写着谁",不等于"谁真的占着"。判据始终是"锁拿不拿得到",
//! 而不是"文件存不存在"。
//!
//! **为什么不去探测 pid 还活着没有**(M0 只定格式时设想过这么做):
//!
//! - 持有者进程崩溃时,**操作系统会自动释放**它持有的文件锁。于是根本不存在"陈旧的锁文件
//!   把项目永久锁死"这回事,也就没必要去猜"锁住的人还在不在"。
//! - pid 会被系统复用:同一个 pid 后来可能是一个完全无关的进程,拿它当判据既不可靠,
//!   又得为每个平台各写一套探测代码。
//!
//! 所以 JSON 里的 `pid` / `host` / `opened_at` 只用来回答人问的那句"这是谁开的"。
//!
//! ## 锁加在哪儿(Windows 上的一条硬经验)
//!
//! 锁不锁文件内容,而是锁**文件末尾之外的一个字节**。
//!
//! 起因是实测:Windows 的字节区间锁是**强制**的 —— 锁住哪一段,别的 handle 连**读**
//! 那一段都会被拒(`ERROR_LOCK_VIOLATION` 33)。而"谁占着这个项目"恰恰写在文件内容里,
//! **第二个会话最需要读到它**。锁整份文件 = 把最该被读到的东西锁得读不了,
//! 于是第二个会话只能干巴巴地说一句"占用者信息不可读"。
//!
//! 实测对比(仓库外的一次性探针):
//!
//! | | 锁整份文件(std `File::try_lock`) | 锁末尾之外的 1 字节 |
//! |---|---|---|
//! | 第二个进程抢锁 | 失败 ✅ | 失败 ✅ |
//! | 第二个进程**读内容** | `Os { code: 33 }` ❌ | `Ok("{\"session\":…}")` ✅ |
//! | 释放后再抢 | 成功 ✅ | 成功 ✅ |
//!
//! 所以 Windows 走 [`os_lock`] 里的 `LockFileEx`,锁在一个文件永远到不了的偏移上:
//! **互斥照旧,内容自由可读**。Unix 的 `flock` 是**咨询**锁,本来就不挡读取,照常用标准库。
//!
//! 代价是两边锁的**范围**不一样。同一份项目目录被两种平台的机器同时挂载着写,是病态用法,
//! 不值得为它牺牲"第二个人能看见是谁占着"这件天天发生的事。
//!
//! **为什么写 JSON 不能用 [`crate::json::write_atomic`]**:那是"写临时文件 → rename",
//! 而 rename 会让目录项指向**另一个文件标识**(inode / Windows 上的文件对象),
//! 锁恰恰是挂在文件标识上的 —— 一 rename,锁就留在没人再打开的那个旧文件上,互斥当场失效。
//! 所以这里必须**原地写**:`set_len(0)` → `seek(0)` → `write_all`。

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Error, Result, bail};
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};

use crate::layout::Layout;

/// `project.lock`。**正被某会话持有写锁时**里面写着它的信息;
/// 文件存在本身不代表锁还占着(见模块文档)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectLock {
    /// 持有写锁的 MCP 进程 pid。**只作诊断**:进程死没死由 OS 释放锁来体现,不靠探测它
    /// (见模块文档"为什么不去探测 pid")。
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

    /// 按 M0 的格式契约写一份 `project.lock`(原子写)。
    ///
    /// ⚠️ **不要拿它写正在持有的锁**:原子写靠 rename,会把锁留在旧文件标识上
    /// (见模块文档)。持锁期间的写入一律走 [`acquire`] 内部的原地写。
    /// 这里留着它,是为了"不持锁时造/改一份诊断文件"以及格式契约的往返测试。
    pub fn save(&self, path: &std::path::Path) -> Result<()> {
        crate::json::write_atomic(path, self)
    }
}

/// 写锁的持有凭证。**Drop 即释放**(RAII),不要手动调用释放。
///
/// 真正的互斥由操作系统的文件锁提供(见模块文档),`project.lock` 里的 JSON 只是给人看的诊断信息。
///
/// ⚠️ 拿到之后一定要**留住**它:`let _guard = acquire(...)?;` 之后不绑定,
/// 锁会在那条语句结束时立刻被释放(guard 被 drop),写操作却以为项目归自己了。
#[derive(Debug)]
pub struct LockGuard {
    /// 加了独占锁的文件句柄。锁挂在它身上,句柄一关锁就没了,所以要留到 drop。
    file: File,
    /// 进程内登记表的键(**项目身份**,不是调用方给的路径原样 —— 见 [`lock_key`])。
    key: PathBuf,
    /// 已经写进文件的那份信息。
    info: ProjectLock,
}

impl LockGuard {
    /// 当前持有的锁信息(写进文件里的那份)。
    pub fn info(&self) -> &ProjectLock {
        &self.info
    }
}

/// 取写锁。已经被别的会话持有时返回错误,错误信息里要**说清是谁占着**。
pub fn acquire(layout: &Layout, session: &str) -> Result<LockGuard> {
    let path = layout.lock_file();

    // 不替调用方建目录:锁属于一个已存在的项目。路径写错时就该在"打开锁文件失败"里
    // 原样暴露出来,而不是凭空造出一个看着像项目的空目录。
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true) // 不存在则创建:第一个会话就是靠这一步把锁文件立起来的
        .open(&path)
        .with_context(|| format!("打开锁文件失败:{}", path.display()))?;

    match os_lock::try_lock(&file) {
        os_lock::Outcome::Acquired => {}
        os_lock::Outcome::Busy => {
            // 有人占着。谁占着只能靠诊断信息问(见模块文档):问得出来就报出来,问不出来就说读不到。
            match peek(layout) {
                Some(holder) => bail!(
                    "项目正被另一个会话写入,拿不到写锁:{} (pid {},主机 {},打开于 {})。\
                     等这个会话结束再来,或只读打开该项目。(锁文件:{})",
                    holder.session,
                    holder.pid,
                    holder.host,
                    holder.opened_at.to_rfc3339(),
                    path.display()
                ),
                None => bail!(
                    "项目正被另一个会话写入,拿不到写锁:占用者信息读不出来\
                     (对方刚好处在写入的瞬间,或文件被截断了;锁文件:{})。\
                     等这个会话结束再来,或只读打开该项目。",
                    path.display()
                ),
            }
        }
        os_lock::Outcome::Failed(e) => {
            // 不是"别人占着"而是加锁本身出错(权限、文件系统不支持……),如实报出来别装成撞锁。
            return Err(Error::new(e).context(format!("取项目写锁失败:{}", path.display())));
        }
    }

    // 锁到手了,把"谁开着"原地写进文件(不能 rename,见模块文档)。
    let info = ProjectLock::new(session);
    if let Err(e) = write_in_place(&mut file, &info, &path) {
        // 写不进去就等于这个凭证不可信 —— 先解锁再报错,别让调用方拿着一个
        // "锁着但文件里没有自己"的 guard 去写项目。
        os_lock::unlock(&file);
        return Err(e);
    }

    // 登记到进程内表里:比读文件快,而且躲开了"正巧读到对方刚截断成 0 字节"的窗口。
    let key = lock_key(layout);
    if let Ok(mut held) = held_in_process().lock() {
        held.insert(key.clone(), info.clone());
    }

    Ok(LockGuard { file, key, info })
}

/// 不取锁,只读一眼当前持有者是谁(给状态展示与错误信息用)。
///
/// 文件不存在、为空、或内容读不出来时返回 `None` —— 这是诊断接口,不该因为它而失败。
/// 注意读到的可能是**上一个会话崩溃后残留**的内容,所以别拿它当判据(判据是 OS 锁)。
pub fn peek(layout: &Layout) -> Option<ProjectLock> {
    let path = layout.lock_file();

    // 先看进程内登记表:比读文件快,而且躲开了"正巧读到对方刚截断成 0 字节"的窗口。
    if let Some(info) = held_in_process().lock().ok()?.get(&lock_key(layout)) {
        return Some(info.clone());
    }

    // 再读文件:它能给出**本进程之外**的持有者,以及上个进程崩溃后残留的那一份。
    // 读不出来不算错误:文件还没建、刚被上一位持有者截断成空、JSON 坏掉 ——
    // 都只是"这一路问不出来"。
    //
    // 这里能读到内容,靠的是**锁加在文件之外**(见模块文档):换作锁整份文件,
    // Windows 上这一行会稳定地失败,而它恰恰是第二个会话唯一想问的东西。
    let text = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&text).ok()
}

/// 进程内登记表与 [`peek`] 共用的键:**项目目录的身份**,不是调用方给的路径原样。
///
/// 为什么必须规范化:`projects/e2e`、`./projects/e2e`、`E:\…\projects\e2e`、`PROJECTS/E2E`
/// 在 Windows 上是同一个目录,但作为字符串是四个不同的值。拿原样当键,"同一个项目"
/// 在内存里就变成四个 —— 于是**自己把自己挡在门外**:第二次取锁时这张表没命中,
/// 真去调 `try_lock`,而 OS 锁是同进程跨 handle 也生效的,直接报
/// "项目正被另一个会话写入",那个"另一个会话"就是它自己。
///
/// 这不是假想:`create_project` 回的是 `projects/<id>`,`list_projects` 回的是绝对路径,
/// 而工具描述里明确告诉 agent"两种写法都认"。
fn lock_key(layout: &Layout) -> PathBuf {
    crate::layout::identity(layout.root())
}

/// 把锁信息原地写进已加锁的文件。
///
/// 顺序不能换:`set_len(0)` 截断 → `seek(0)` 回到开头 → 写入。不截断会留下上一份的尾巴
/// (JSON 变大变小都可能),不 seek 就可能从旧位置往后写,两边都会写出解析不了的东西。
fn write_in_place(file: &mut File, info: &ProjectLock, path: &Path) -> Result<()> {
    let text = crate::json::to_pretty_string(info)?;
    file.set_len(0)
        .with_context(|| format!("截断锁文件失败:{}", path.display()))?;
    file.seek(SeekFrom::Start(0))
        .with_context(|| format!("定位到锁文件开头失败:{}", path.display()))?;
    file.write_all(text.as_bytes())
        .with_context(|| format!("写入锁文件失败:{}", path.display()))?;
    // 标准库的 File 是无缓冲的,这步其实什么都不做 —— 写在这里是为了把
    // "写完必须对读者可见"的意图钉住(将来若换成 BufWriter 也不会漏)。
    // 不做 sync_all:锁信息是诊断用的,为它多花一次落盘代价不值。
    file.flush()
        .with_context(|| format!("刷新锁文件失败:{}", path.display()))?;
    Ok(())
}

/// 本进程内正持有的锁:**项目身份** → 锁信息。
///
/// 为什么要有这张表:它比读文件快一步,而且躲开了"正巧读到对方刚 `set_len(0)`"的窗口
/// (持有者换手时会先截断再写)。键用 [`lock_key`],不用调用方给的路径原样。
fn held_in_process() -> &'static Mutex<HashMap<PathBuf, ProjectLock>> {
    static HELD: OnceLock<Mutex<HashMap<PathBuf, ProjectLock>>> = OnceLock::new();
    HELD.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 加锁 / 解锁的**平台差异都收在这里**。
///
/// 对外只有三件事:拿到了、别人占着、加锁本身出错。调用方不需要知道
/// Windows 锁的是文件之外的一个字节、Unix 锁的是整份文件。
mod os_lock {
    #[cfg(not(windows))]
    use std::fs::File;
    use std::io;

    pub enum Outcome {
        Acquired,
        /// 别人占着。**这不是错误**,是"等会儿再来"。
        Busy,
        /// 加锁这个动作本身失败了(权限、文件系统不支持……)。
        Failed(io::Error),
    }

    // ── Unix / 其它:标准库的 flock 就够 ──────────────────────────────
    //
    // `flock` 是**咨询**锁:它只约束同样来加锁的进程,文件该读照样能读。
    // 所以"第二个会话读不到持有者"这件事在 Unix 上根本不存在,不需要特殊处理。
    #[cfg(not(windows))]
    pub fn try_lock(file: &File) -> Outcome {
        use std::fs::TryLockError;
        match file.try_lock() {
            Ok(()) => Outcome::Acquired,
            Err(TryLockError::WouldBlock) => Outcome::Busy,
            Err(TryLockError::Error(e)) => Outcome::Failed(e),
        }
    }

    #[cfg(not(windows))]
    pub fn unlock(file: &File) {
        // 失败也无所谓:句柄随 guard 一起关闭时 OS 会释放。drop 里不能 panic。
        let _ = file.unlock();
    }

    // ── Windows:锁文件**之外**的一个字节 ────────────────────────────
    //
    // 为什么不用标准库的 `File::try_lock`:`LockFileEx` 是**强制**锁 ——
    // 锁住哪一段,别的 handle 连读那一段都会被拒(实测 `ERROR_LOCK_VIOLATION` 33)。
    // 而锁整份文件就等于把"谁占着"这句话锁得没人读得到。
    // 锁在一个文件永远到不了的偏移上,互斥照旧而内容自由可读(实测见模块文档)。
    #[cfg(windows)]
    pub use windows_impl::{try_lock, unlock};

    #[cfg(windows)]
    mod windows_impl {
        use std::ffi::c_void;
        use std::fs::File;
        use std::io;
        use std::os::windows::io::AsRawHandle;

        use super::Outcome;

        /// 锁的落点:**远超任何真实文件长度**的一个字节。
        ///
        /// `LockFileEx` 明确允许锁定超出文件末尾的范围("Locking a region that goes beyond
        /// the current end-of-file position is not an error"),所以这一格永远不会与内容相交。
        /// 取 2^63 附近是因为它离任何可能的视频/JSON 都足够远,又不会溢出 64 位偏移。
        const LOCK_OFFSET_LOW: u32 = 0xFFFF_FF00;
        const LOCK_OFFSET_HIGH: u32 = 0x7FFF_FFFF;

        const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x0000_0001;
        const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x0000_0002;
        /// `ERROR_LOCK_VIOLATION` —— 撞上别人持有的锁。
        const ERROR_LOCK_VIOLATION: u32 = 33;

        /// `OVERLAPPED`。这里只用得到偏移那两格与事件句柄,但要**按原样排布**:
        /// 前两个字段由系统填写,漏了它们后面全部错位。
        #[repr(C)]
        struct Overlapped {
            internal: usize,
            internal_high: usize,
            offset: u32,
            offset_high: u32,
            h_event: *mut c_void,
        }

        impl Overlapped {
            fn at_lock_offset() -> Self {
                Self {
                    internal: 0,
                    internal_high: 0,
                    offset: LOCK_OFFSET_LOW,
                    offset_high: LOCK_OFFSET_HIGH,
                    h_event: std::ptr::null_mut(),
                }
            }
        }

        // 手写 FFI 而不是拉 windows-sys:用到的只有这两个函数加一个 GetLastError,
        // 而 windows-sys 是几千个绑定的宏生成物。代价是 OVERLAPPED 的排布得自己保证 ——
        // 所以 `lock_and_read_content_are_independent` 那条测试是有意写死的:
        // 排布错了它就会因为读不到内容或抢不到锁而失败。
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn LockFileEx(
                handle: *mut c_void,
                flags: u32,
                reserved: u32,
                bytes_low: u32,
                bytes_high: u32,
                overlapped: *mut Overlapped,
            ) -> i32;
            fn UnlockFileEx(
                handle: *mut c_void,
                reserved: u32,
                bytes_low: u32,
                bytes_high: u32,
                overlapped: *mut Overlapped,
            ) -> i32;
            fn GetLastError() -> u32;
        }

        pub fn try_lock(file: &File) -> Outcome {
            let mut ov = Overlapped::at_lock_offset();
            // SAFETY:句柄来自还活着的 `File`;`ov` 在整个调用期间有效且排布正确;
            // 锁的范围是常数,与文件内容不相交。
            let ok = unsafe {
                LockFileEx(
                    file.as_raw_handle() as *mut c_void,
                    LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                    0,
                    1,
                    0,
                    &mut ov,
                )
            };
            if ok != 0 {
                return Outcome::Acquired;
            }
            let code = unsafe { GetLastError() };
            if code == ERROR_LOCK_VIOLATION {
                Outcome::Busy
            } else {
                Outcome::Failed(io::Error::from_raw_os_error(code as i32))
            }
        }

        pub fn unlock(file: &File) {
            let mut ov = Overlapped::at_lock_offset();
            // SAFETY:同 `try_lock`。失败也无所谓,见 `unlock` 的通用说明。
            let _ = unsafe {
                UnlockFileEx(
                    file.as_raw_handle() as *mut c_void,
                    0,
                    1,
                    0,
                    &mut ov,
                )
            };
        }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // 截断成 0 字节:给"文件非空 = 有会话开着项目"这个**人眼判据**留一个尽量准的痕迹。
        // 它不准也没关系(判据是 OS 锁),但能少让一个人白等。
        // 顺序是先截断再解锁:反过来的话,解锁后到截断前有个窗口,别人会读到旧内容。
        let _ = self.file.set_len(0);
        // 之后所有错误一律忽略:drop 里不能 panic,也没人接得住错误。解锁失败最坏也只是
        // 句柄随 guard 一起关闭时由 OS 顺带释放,不影响互斥的正确性。
        os_lock::unlock(&self.file);
        if let Ok(mut held) = held_in_process().lock() {
            held.remove(&self.key);
        }
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
    use std::io::Read;

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

    /// 建一个干净的临时项目目录。
    ///
    /// **每个测试必须给不同的 `tag`**:共用目录的测试会真的去抢同一把锁
    /// (测的就是互斥),结果变成随机失败。
    fn tmp_project(tag: &str) -> (PathBuf, Layout) {
        let dir = std::env::temp_dir().join(format!("vidbrief-lock-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); // 清掉上一轮跑挂留下的残骸
        std::fs::create_dir_all(&dir).unwrap();
        (dir.clone(), Layout::new(dir))
    }

    /// 借 guard **自己那个** handle 读锁文件。
    ///
    /// Windows 上锁是强制的:持有期间换个 handle 读同一段会拿到 `ERROR_LOCK_VIOLATION`(33),
    /// 用 `std::fs::read_to_string` 根本读不到。想验证"内容真的写进文件了"只能这样读,
    /// 这也顺带证明了"谁拿到锁谁独占这个文件"。
    fn read_via_guard(guard: &LockGuard) -> String {
        let mut text = String::new();
        (&guard.file).seek(SeekFrom::Start(0)).unwrap();
        (&guard.file).read_to_string(&mut text).unwrap();
        text
    }

    #[test]
    fn second_acquire_is_rejected_and_names_the_holder() {
        let (dir, layout) = tmp_project("exclusive");

        let first = acquire(&layout, "session-a").unwrap();
        let err = acquire(&layout, "session-b").unwrap_err();
        // `{:#}` 打出整条因果链,只 to_string() 会丢底层原因
        let msg = format!("{err:#}");
        assert!(msg.contains("拿不到写锁"), "报错该说明是撞锁而不是别的错:{msg}");
        assert!(msg.contains("session-a"), "报错必须说清是谁占着:{msg}");

        // 撞锁的那次不该动文件:持有者写的诊断内容还在。
        // (只能看长度 —— 内容在 Windows 上被强制锁挡着读不了,长度是元数据不受影响。)
        assert!(
            std::fs::metadata(layout.lock_file()).unwrap().len() > 0,
            "撞锁失败不该把持有者写的内容截掉"
        );

        drop(first);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn can_acquire_again_after_drop() {
        let (dir, layout) = tmp_project("reacquire");

        drop(acquire(&layout, "session-a").unwrap()); // drop 即释放(RAII)

        // 上面那次是老老实实解锁了,所以这里必须能拿到 ——
        // 否则就成了"文件存在即锁着"的误判(正是"不由文件是否存在决定"要避免的)。
        let again = acquire(&layout, "session-b").unwrap();
        assert_eq!(again.info().session, "session-b");

        drop(again);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn acquire_writes_canonical_json_into_the_file() {
        let (dir, layout) = tmp_project("content");
        let guard = acquire(&layout, "session-w").unwrap();

        let text = read_via_guard(&guard);
        let on_disk: ProjectLock = serde_json::from_str(&text).expect("锁文件里应是合法 JSON");
        assert_eq!(on_disk.session, "session-w");
        assert_eq!(on_disk, *guard.info(), "文件里那份应与 guard 手上那份一致");

        // 形状也得是契约里那个:逐字节等于规范序列化(否则 "project.lock 的 JSON 只是
        // 诊断信息" 就会变成 "它跟 schema 分家了")
        assert_eq!(
            text,
            crate::json::to_pretty_string(&on_disk).unwrap(),
            "写进文件的应是规范 JSON(2 空格缩进 + 结尾换行)"
        );

        // 原地写没留旧尾巴:文件长度 = 写进去的字节数
        assert_eq!(
            std::fs::metadata(layout.lock_file()).unwrap().len() as usize,
            text.len(),
            "文件长度应与写进去的 JSON 一致"
        );

        drop(guard);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn peek_is_soft_and_never_fails() {
        let (dir, layout) = tmp_project("peek");

        assert!(peek(&layout).is_none(), "锁文件还不存在时应是 None");

        std::fs::write(layout.lock_file(), "").unwrap();
        assert!(peek(&layout).is_none(), "空文件应是 None");

        // 垃圾内容、半截 JSON、缺字段、多字段、时间戳不合契约 —— 一律 None,不 panic 不报错
        for junk in [
            "这不是 JSON",
            "{\"session\":",
            "{\"pid\":1,\"host\":\"h\",\"session\":\"s\"}",
            "{\"pid\":1,\"host\":\"h\",\"session\":\"s\",\
             \"opened_at\":\"2026-09-24T21:00:00+08:00\",\"oops\":1}",
            "{\"pid\":1,\"host\":\"h\",\"session\":\"s\",\"opened_at\":\"不是时间\"}",
        ] {
            std::fs::write(layout.lock_file(), junk).unwrap();
            assert!(peek(&layout).is_none(), "内容不可解读时应是 None:{junk}");
        }

        // 没人持锁时(典型场景:上一个会话崩了,OS 收回了锁、JSON 留在文件里),
        // peek 才真能从文件里读出"上一个占着的人是谁" —— 这正是它作为诊断接口的用处。
        ProjectLock::new("session-dead").save(&layout.lock_file()).unwrap();
        assert_eq!(
            peek(&layout).map(|l| l.session),
            Some("session-dead".to_string()),
            "无锁持有者时应能读到文件里残留的那份信息"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn peek_reports_the_in_process_holder() {
        let (dir, layout) = tmp_project("peek-held");

        // 本进程持有期间也要问得出是谁(Windows 上文件那一路读不到,靠进程内登记表)
        let guard = acquire(&layout, "session-held").unwrap();
        assert_eq!(
            peek(&layout).map(|l| l.session),
            Some("session-held".to_string())
        );

        drop(guard);
        assert!(
            peek(&layout).is_none(),
            "释放后登记表与文件都该是干净的,一个都不该漏"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 读不到持有者信息时,报错也要**说得出话**(不能哑着,更不能说成别的错)。
    ///
    /// 这条走的就是真·跨进程撞锁在 Windows 上的路径:对方持有 OS 锁 → 文件读不了,
    /// 而进程内登记表里也没有它(它是别的进程)。这里用"抹掉登记表"来模拟那一步:
    /// 手上没有别的办法在单进程测试里制造出"文件读不了"+"表里没有"的组合。
    #[test]
    fn unreadable_holder_falls_back_to_a_clear_message() {
        let (dir, layout) = tmp_project("unreadable");

        let holder = acquire(&layout, "session-x").unwrap();

        // 模拟"这一路问不出来":持有者正卡在 `set_len(0)` 与 `write_all` 之间,
        // 或者文件被别的进程写坏了。**内容区不受锁保护**(锁加在文件之外,见模块文档),
        // 所以另开一个 handle 截断它是做得到的 —— 换作锁整份文件,这一步会直接失败。
        std::fs::OpenOptions::new()
            .write(true)
            .open(layout.lock_file())
            .expect("另开一个 handle 写内容区")
            .set_len(0)
            .expect("截断内容区");

        // 进程内那张表也得清掉,否则 peek 走的是内存那一路(它比读文件快,也优先)
        held_in_process()
            .lock()
            .unwrap()
            .remove(&lock_key(&layout));

        let err = acquire(&layout, "session-y").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("拿不到写锁"), "报错该说明是撞锁:{msg}");
        assert!(
            msg.contains("占用者信息读不出来"),
            "读不到持有者时应如实说读不出来,而不是编一个:{msg}"
        );

        drop(holder);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// **锁加在文件之外**这件事的验收:持有锁期间,另一个句柄必须能读到文件内容。
    ///
    /// 这是 Windows 上的关键差异 —— `LockFileEx` 是强制锁,锁住哪一段别人就连读都读不了。
    /// 一旦有人把实现改回"锁整份文件"(比如图省事用回 `File::try_lock`),
    /// 这条测试会立刻红:那样第二个会话永远说不出"是谁占着"。
    #[test]
    fn another_handle_can_read_the_content_while_locked() {
        let (dir, layout) = tmp_project("readable");

        let holder = acquire(&layout, "session-readable").unwrap();
        let text = std::fs::read_to_string(layout.lock_file())
            .expect("持锁期间内容必须读得到 —— 否则第二个会话问不出是谁占着");
        assert!(
            text.contains("session-readable"),
            "读到的应当是持有者信息:{text}"
        );

        drop(holder);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn drop_truncates_the_lock_file() {
        let (dir, layout) = tmp_project("truncate");

        let held_len = {
            let guard = acquire(&layout, "session-t").unwrap();
            let len = std::fs::metadata(layout.lock_file()).unwrap().len();
            assert!(len > 0, "持有期间锁文件应非空(人眼判据靠它)");
            drop(guard);
            len
        };
        assert!(held_len > 0);

        assert_eq!(
            std::fs::metadata(layout.lock_file()).unwrap().len(),
            0,
            "释放后应截断成 0 字节,否则人眼会把空闲的项目看成还开着"
        );
        assert!(peek(&layout).is_none());

        std::fs::remove_dir_all(&dir).ok();
    }
}
