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

use std::fs::{File, OpenOptions, TryLockError};

use anyhow::{Context, Error, Result, bail};

use crate::layout::Layout;

/// 写锁的持有凭证。**Drop 即释放**(RAII),不要手动调用释放。
///
/// ⚠️ 拿到之后一定要**留住**它:`let _guard = acquire(...)?;` 之后不绑定,
/// 锁会在那条语句结束时立刻被释放(guard 被 drop),写操作却以为项目归自己了。
#[derive(Debug)]
pub struct LockGuard {
    /// 加了独占锁的文件句柄。锁挂在它身上,句柄一关锁就没了,所以要留到 drop。
    file: File,
}

/// 取写锁。已经被别的会话持有时返回错误。
///
/// **不阻塞**:这个锁的持有单位是一场对话,等下去等于把调用方挂死(见模块文档)。
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
        Ok(()) => Ok(LockGuard { file }),
        Err(TryLockError::WouldBlock) => bail!(
            "项目正被另一个会话写入,拿不到写锁。\
             等这个会话结束再来(或让用户关掉那个会话),也可以只读打开该项目。\
             (锁文件:{})",
            path.display()
        ),
        Err(TryLockError::Error(e)) => {
            // 不是"别人占着"而是加锁本身出错(权限、文件系统不支持……),如实报出来别装成撞锁。
            Err(Error::new(e).context(format!("取项目写锁失败:{}", path.display())))
        }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // 显式解锁。失败也无所谓:句柄随 guard 一起关闭时 OS 会释放。
        // drop 里不能 panic,也没人接得住错误。
        let _ = self.file.unlock();
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
}
