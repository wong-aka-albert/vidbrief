//! `project.lock` —— 项目写锁。
//!
//! 设计见 `docs/DESIGN.md` §4.7:首个要**改**项目的会话取得写锁,其他会话只能只读打开。
//!
//! # 这个文件是空的,而且**故意**是空的
//!
//! 它是个**哨兵**,不是数据文件:里面不写任何东西,存在的唯一意义是"有个东西可以加锁"。
//!
//! M0 曾经给它定过格式(pid / host / session / opened_at),还配了一份 schema —— 那是错的,
//! 错在把两件事合进了一个文件。代价是现成的:
//!
//! Windows 的 `LockFileEx` 是**强制**锁 —— 锁住哪一段,别的进程连**读**那一段都会被拒
//! (`ERROR_LOCK_VIOLATION` 33)。锁整份文件就等于把"谁占着"这句话锁得**恰恰在需要它的时候读不到**。
//! 绕开它有两条路:锁到文件末尾之外的一个字节(要手写不安全的平台 FFI),
//! 或者另起一个不被锁住的旁路文件(要给项目目录加文件、动格式)。
//! 两条都做得到,但收益只有一个:**一句读了也不改变任何人动作的诊断信息**
//! (能锁住你的必然是活进程,而 pid 没法映射回"是哪个窗口")。
//!
//! 空文件把这个问题连同它的两种解法一起消掉:**没有内容,就没有"内容读不到"**。
//! 参考 cargo:`$CARGO_HOME/.package-cache`、`target/<profile>/.cargo-lock` 也都是空哨兵,
//! 用的是同一个 `std::fs::File::try_lock`。
//!
//! **代价要说清**:拿不到锁时只能说"被另一个会话占用",说不出是哪一个。
//! 真要找回这句话,正道是另起一个不被锁住的旁路文件 —— **等真有这个需求再说**,
//! 别为了补一句现在没人用的诊断信息,提前把格式和不安全代码引进来。
//!
//! # 判据是"锁拿不拿得到",**不是"文件存不存在"**
//!
//! 文件会一直留在项目里(0 字节),空闲时也在。想知道项目有没有被占,只能去**试着加锁**:
//!
//! - 加得上 → 没人占;
//! - `WouldBlock` → 有人占着;
//! - 进程崩溃 → **操作系统自动释放**,所以不存在"陈旧的锁文件把项目永久锁死",
//!   也就没必要去探测 pid 还活着没有(何况 pid 会被复用)。
//!
//! 因为从不往文件里写字节,那些"原地写不能 rename"的讲究在这里**根本不存在** ——
//! 没有内容,就没有 `write_atomic` 的取舍。
//!
//! # 加锁的粒度
//!
//! [`acquire`] 用的是标准库的 `File::try_lock`:**整份文件、独占**
//! (Windows 落 `LockFileEx`,Unix 落 `flock`)。抢不到就**立刻失败**,不阻塞 ——
//! 这个锁的持有单位是**一场对话**(分钟到小时),等着等于把调用方挂死;
//! cargo 敢阻塞,是因为它的锁只跨一次构建。
//!
//! 只读的活**不需要**这个锁:材料是文件,读的一方写不坏任何东西。
//!
//! # 各平台底下到底是什么(源自 `std` 源码,不是猜的)
//!
//! | 平台 | `File::try_lock` 落到哪 | 强制还是咨询 | 同进程第二个 handle 会不会撞 |
//! |------|------------------------|-------------|---------------------------|
//! | Windows | `LockFileEx(0, u32::MAX, u32::MAX)` = 整份文件 | **强制** | **会撞** |
//! | Linux / Android / macOS / *BSD / illumos / AIX | `flock(LOCK_EX \| LOCK_NB)` | 咨询 | **会撞**(两次 `open` 是两个 open file description) |
//! | Solaris | `fcntl(F_SETLK)` | 咨询 | **不撞** —— fcntl 记录锁属于**进程**,第二次 `F_SETLK` 只是替换自己那把 |
//!
//! 最后一行是这张表里唯一要小心的地方:在 Solaris 上 [`acquire`] **本身不构成互斥**,
//! 真正挡住"同一个进程开两把锁"的是调用方那张按 [`crate::layout::identity`] 索引的进程内表
//! (见 `vidbrief_mcp::state`)。所以在 Unix 与 Windows 上它是**加速**,在 Solaris 上它是**必需**。
//! 无论如何都不能把那张表删掉。
//!
//! ## 文件系统说"不支持锁"时:**放行,但说清楚**
//!
//! `Err(TryLockError::Error(..))` 里有一类不是"别人占着",而是"这个文件系统压根不支持锁" ——
//! 网络盘、NFS、某些 FUSE 都会。这类**不再当成失败**:拿不到锁也放行,把原因挂在
//! [`LockGuard::degraded`] 上让调用方去说。
//!
//! cargo 在这里的选择是**静默放行**(它的文档原话:保证不了,请用户自己别并发);
//! 而且它在 **NFS 上根本不尝试加锁**,因为 `flock` 在 NFS 上即使给了非阻塞标志
//! 也可能**永久阻塞**(rust-lang/cargo#2615)—— 那是挂死,比失败更糟。
//!
//! 我们没有照抄"静默":工具撞上"没有锁"不是构建冲突重跑一次,而是**静默丢结论**。
//! 所以是**放行 + 明确警告**,由入口层把话说到用户耳朵里(CLI 打 stderr、
//! MCP 把这句话挂在第一次写操作的结果里)。
//!
//! **只放行"不支持",不放行"没权限"** —— 后者是真错误,该原样报出来。

use std::fs::{File, OpenOptions, TryLockError};
use std::io;

use anyhow::{Context, Error, Result, bail};

use crate::layout::Layout;

/// 写锁的持有凭证。**Drop 即释放**(RAII),不要手动调用释放。
///
/// ⚠️ 拿到之后一定要**留住**它:`let _guard = acquire(...)?;` 之后不绑定,
/// 锁会在那条语句结束时立刻被释放(guard 被 drop),写操作却以为项目归自己了。
#[derive(Debug)]
pub struct LockGuard {
    /// 真正加上了锁时为 `Some`;降级放行时没有锁可放,为 `None`。
    file: Option<File>,
    /// 降级的原因。`Some` = **没锁上,但放行了**。
    degraded: Option<String>,
}

impl LockGuard {
    /// 降级放行的原因。`None` 表示真的拿到锁了。
    ///
    /// **拿到 `Some` 的调用方必须把这句话说给用户听** —— 这是"放行不静默"的全部代价
    /// (见模块文档)。说不出话的调用方等于把"可能互相覆盖"这件事藏了起来。
    pub fn degraded(&self) -> Option<&str> {
        self.degraded.as_deref()
    }
}

/// 取写锁。已经被别的会话持有时返回错误。
///
/// **不阻塞**:这个锁的持有单位是一场对话,等下去等于把调用方挂死(见模块文档)。
/// **文件系统不支持锁时不失败**:返回一个 [`LockGuard::degraded`] 为 `Some` 的凭证,
/// 由调用方去说明。
pub fn acquire(layout: &Layout) -> Result<LockGuard> {
    let path = layout.lock_file();

    // 不替调用方建目录:锁属于一个已存在的项目。路径写错时就该在"打开锁文件失败"里
    // 原样暴露出来,而不是凭空造出一个看着像项目的空目录。
    //
    // 也不往里写任何东西 —— 它就是空的(见模块文档)。
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true) // 不存在则创建:第一个会话就是靠这一步把哨兵立起来的
        .open(&path)
        .with_context(|| format!("打开锁文件失败:{}", path.display()))?;

    match file.try_lock() {
        Ok(()) => Ok(LockGuard {
            file: Some(file),
            degraded: None,
        }),
        Err(TryLockError::WouldBlock) => bail!(
            "项目正被另一个会话写入,拿不到写锁。\
             等这个会话结束再来(或让用户关掉那个会话),也可以只读打开该项目。\
             (锁文件:{})",
            path.display()
        ),
        Err(TryLockError::Error(e)) if locking_unsupported(&e) => Ok(LockGuard {
            // 没锁上就别留着句柄 —— 留着会让人以为"它正持有"
            file: None,
            degraded: Some(format!(
                "这个文件系统不支持文件锁({e}),已跳过互斥。\
                 只要**没有第二个会话**同时改这个项目,一切照常;\
                 有的话,两边可能互相覆盖。请自己保证不要同时开两个会话改同一个项目。"
            )),
        }),
        Err(TryLockError::Error(e)) => {
            // 不是"别人占着"、也不是"不支持",而是真出了错(权限、坏盘……),
            // 如实报出来别装成撞锁。
            Err(Error::new(e).context(format!("取项目写锁失败:{}", path.display())))
        }
    }
}

/// 这个错误是不是在说"**这个文件系统/平台不支持文件锁**"?
///
/// 判据照抄 cargo(`src/util/flock.rs` 的 `error_unsupported`),只放行"不支持",
/// **不放行"没权限"** —— 后者是真错误,该报出来。
fn locking_unsupported(e: &io::Error) -> bool {
    // std 已经替我们归一了一部分:EOPNOTSUPP(Linux 上 ENOTSUP 与它同值)、
    // Windows 的 ERROR_CALL_NOT_IMPLEMENTED,以及 std 自己没实现锁的平台。
    if e.kind() == io::ErrorKind::Unsupported {
        return true;
    }

    // 剩下的 std 没归一,只能认原始错误码。
    #[cfg(unix)]
    {
        // 数字按平台变(ENOLCK 在 Linux 是 37、macOS 是 77),所以用 libc 的常量。
        matches!(e.raw_os_error(), Some(libc::ENOLCK) | Some(libc::ENOSYS))
    }
    #[cfg(windows)]
    {
        // Windows 的错误码是 API 契约的一部分、永不改值,所以这里直接写字面量:
        // 1 = ERROR_INVALID_FUNCTION(cargo 用的就是它),50 = ERROR_NOT_SUPPORTED。
        // 为两个常量拉 windows-sys(几千个绑定的宏生成物)不划算。
        matches!(e.raw_os_error(), Some(1) | Some(50))
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // 显式解锁。失败也无所谓:句柄随 guard 一起关闭时 OS 会释放。
        // drop 里不能 panic,也没人接得住错误。
        if let Some(file) = &self.file {
            let _ = file.unlock();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

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

    #[test]
    fn second_acquire_is_rejected() {
        let (dir, layout) = tmp_project("exclusive");

        let first = acquire(&layout).unwrap();
        let err = acquire(&layout).unwrap_err();
        // `{:#}` 打出整条因果链,只 to_string() 会丢底层原因
        let msg = format!("{err:#}");
        assert!(msg.contains("拿不到写锁"), "报错该说明是撞锁而不是别的错:{msg}");
        assert!(msg.contains("只读"), "还要说清哪些还能用:{msg}");

        drop(first);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn can_acquire_again_after_drop() {
        let (dir, layout) = tmp_project("reacquire");

        drop(acquire(&layout).unwrap()); // drop 即释放(RAII)

        // 上面那次是老老实实解锁了,所以这里必须能拿到 ——
        // 否则就成了"文件存在即锁着"的误判(正是"不由文件是否存在决定"要避免的)。
        assert!(acquire(&layout).is_ok());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 哨兵就是哨兵:**从生到死都是 0 字节**。
    ///
    /// 这条测试存在的意义,是拦住"顺手往里写点诊断信息"的冲动 ——
    /// 一旦文件里有了内容,Windows 上的强制锁就会让那句话**恰恰在需要它的时候读不到**,
    /// 于是又得引入"锁到文件之外"或旁路文件。模块文档里那笔账就是这么算出来的。
    #[test]
    fn the_lock_file_is_and_stays_empty() {
        let (dir, layout) = tmp_project("empty");

        assert!(!layout.lock_file().exists(), "还没人取锁时不该有哨兵");

        let guard = acquire(&layout).unwrap();
        assert_eq!(
            std::fs::metadata(layout.lock_file()).unwrap().len(),
            0,
            "持锁期间锁文件必须是空的"
        );

        drop(guard);
        assert_eq!(
            std::fs::metadata(layout.lock_file()).unwrap().len(),
            0,
            "释放后仍然是空的(哨兵留着,下次直接在它上面加锁)"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// **0 字节的文件也必须锁得住。**
    ///
    /// 这条不是废话:整文件锁在空文件上的行为是要验证过才敢依赖的
    /// (锁一个超出 EOF 的范围是合法的,但那属于"知道它能用"而不是"想当然")。
    #[test]
    fn an_empty_file_is_still_lockable() {
        let (dir, layout) = tmp_project("empty-lock");

        let guard = acquire(&layout).unwrap();
        assert!(layout.lock_file().is_file(), "取锁顺带把哨兵立起来");
        let err = acquire(&layout).unwrap_err();
        assert!(
            format!("{err:#}").contains("拿不到写锁"),
            "空文件也要挡得住第二次"
        );

        drop(guard);
        assert!(acquire(&layout).is_ok(), "释放后立刻能再取");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 认得出"文件系统不支持锁"这一类错误,**且只认这一类**。
    ///
    /// 这条判断的两个方向都不能错:
    /// - 漏认 → 网络盘上工具直接不可用(就是要修的那个毛病);
    /// - 多认 → 把"没权限"当成"不支持",于是静默地不锁,丢数据还找不到原因。
    ///
    /// 真正的降级分支只在"文件系统不支持锁"时才走到,本机造不出那种文件系统,
    /// 所以这里测的是**判据**本身 —— 分支逻辑只有三行,判据才是会错的地方。
    #[test]
    fn only_unsupported_errors_count_as_unsupported() {
        // std 归一过的那一类
        assert!(locking_unsupported(&io::Error::from(io::ErrorKind::Unsupported)));

        assert!(
            !locking_unsupported(&io::Error::from(io::ErrorKind::PermissionDenied)),
            "没权限不是不支持"
        );
        assert!(
            !locking_unsupported(&io::Error::from(io::ErrorKind::NotFound)),
            "文件不在不是不支持"
        );

        #[cfg(unix)]
        {
            // ENOLCK:没有可用锁 —— NFS 上锁守护进程没起来时就是它
            assert!(locking_unsupported(&io::Error::from_raw_os_error(libc::ENOLCK)));
            // ENOSYS:内核没实现
            assert!(locking_unsupported(&io::Error::from_raw_os_error(libc::ENOSYS)));
            // EACCES:真错误,不能吞
            assert!(!locking_unsupported(&io::Error::from_raw_os_error(
                libc::EACCES
            )));
        }
        #[cfg(windows)]
        {
            // ERROR_INVALID_FUNCTION —— cargo 的判据之一
            assert!(locking_unsupported(&io::Error::from_raw_os_error(1)));
            // ERROR_NOT_SUPPORTED
            assert!(locking_unsupported(&io::Error::from_raw_os_error(50)));
            // ERROR_ACCESS_DENIED:真错误,不能吞
            assert!(!locking_unsupported(&io::Error::from_raw_os_error(5)));
        }
    }

    #[test]
    fn a_normal_acquire_is_not_degraded() {
        let (dir, layout) = tmp_project("not-degraded");
        let guard = acquire(&layout).unwrap();
        assert!(
            guard.degraded().is_none(),
            "正常拿到锁时不该带降级说明"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// **子进程不该继承锁句柄。**
    ///
    /// `vidbrief mcp` 会在**持有锁期间**拉 ffmpeg / ffprobe / whisper。那些子进程要是把锁句柄
    /// 一起继承了,父进程被杀之后锁就会留在孤儿子进程手里,直到它退出 —— 长视频上那是几分钟,
    /// 期间项目显示"被另一个会话占用",而那个会话早就不在了。
    ///
    /// Rust 在所有平台上都以"不继承"的方式打开文件(Unix `O_CLOEXEC`、Windows 不设
    /// `HANDLE_FLAG_INHERIT`),所以理论上安全。**这条测试就是去验那句话** ——
    /// 判据是行为:父进程放开锁之后,一个还活着的子进程不该让这把锁继续被占着。
    #[test]
    fn a_live_child_does_not_hold_the_lock() {
        let (dir, layout) = tmp_project("child");
        let guard = acquire(&layout).unwrap();
        let mut child = spawn_sleeper();

        drop(guard);

        assert!(
            acquire(&layout).is_ok(),
            "锁没被真正释放 —— 子进程是不是继承了锁句柄?"
        );

        let _ = child.kill();
        let _ = child.wait();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 拉一个活几秒、什么都不干的子进程。
    fn spawn_sleeper() -> std::process::Child {
        #[cfg(windows)]
        let mut cmd = {
            // 用 `ping` 而不是 `timeout`:后者在 stdin 被重定向时会直接拒绝工作
            let mut c = std::process::Command::new("ping");
            c.args(["-n", "6", "127.0.0.1"]);
            c
        };
        #[cfg(unix)]
        let mut cmd = {
            let mut c = std::process::Command::new("sleep");
            c.arg("5");
            c
        };
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("拉不起子进程")
    }
}
