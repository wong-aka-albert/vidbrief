//! 写操作的事务壳:**开 → (必要时取锁) → 做**。
//!
//! # 为什么要有它
//!
//! 这条序列在四个地方各写过一遍(CLI 两条命令、MCP 工具、页面按钮),
//! 而**抄的时候漏了两样** —— 按钮那条既没走进程内排队、也没带降级警告
//! (devlog [260925-21] 的修复)。两次都是"改一处、忘了另一处",
//! 而 Cargo 和编译器都不会提醒。
//!
//! 所以把它收成一个函数:以后往序列里加任何一步(检查 rev、记审计、……
//! 都自动落在所有入口上。
//!
//! # 锁为什么是参数,不是这里自己取
//!
//! **锁的持有期是入口的事**(`DESIGN` §4.7):
//!
//! - CLI 是"一次命令一把"——命令结束就放;
//! - MCP 是"会话级长持"——第一次写时取到进程结束,这样第二个会话在**第一次动手**时
//!   就知道这个项目有主了,而不是改到一半才发现。
//!
//! 所以这里不去猜,让入口把决定权交进来:
//!
//! | 传什么 | 谁用 | 效果 |
//! |--------|------|------|
//! | [`None`] | CLI | 这里现取一把,函数返回时释放 |
//! | [`Some`] `&LockGuard` | MCP | 调用方已经持有,这里**不再取** |
//!
//! `Some` 收的是**真凭证**而不是一个布尔 —— 想走这条支路,你手里必须真有一把
//! `LockGuard`。这不只是好看:MCP 那条路**不能**在这里重复取锁,
//! 同进程再取一次会被自己挡住(见 `vidbrief_project::lock` 的模块文档),
//! 而那种错看着像"被别的会话占着",极难查。

use anyhow::Result;
use vidbrief_project::LockGuard;

use crate::project::OpenProject;

/// 对项目做一次写操作。
///
/// `lock` 见模块文档:CLI 传 [`None`],会话级持锁的入口传 `Some(&guard)`。
///
/// 闭包的第二个参数是**写锁降级放行时的说明**(正常时 `None`)。它交给闭包而不是
/// 当返回值,是因为**该说的时机各入口不同**:`preprocess` 要跑几分钟,
/// 那句话必须在**开工前**说出去;等函数返回才说,用户已经白等了一场。
pub fn with_write<T>(
    project_id: &str,
    lock: Option<&LockGuard>,
    act: impl FnOnce(&mut OpenProject, Option<&str>) -> Result<T>,
) -> Result<T> {
    let mut opened = OpenProject::open(project_id)?;

    // 自己取的那把要活到函数结束 —— 绑定在这里,别让它被提前 drop
    let owned;
    let warning = match lock {
        Some(_) => None,
        None => {
            owned = vidbrief_project::acquire(opened.layout())?;
            owned.degraded().map(str::to_string)
        }
    };

    act(&mut opened, warning.as_deref())
}

/// 这个项目的写锁**现在**在别人手里吗?拿到就返回 `None`,拿不到就把原因交回来。
///
/// 只探一下、**立刻放掉**(`LockGuard` 在这里就 drop 了)。用途是给「起作业」那种
/// 入口在**开工前**说清楚:现在开不了工,是因为别的会话正写着这个项目 ——
/// 而不是让作业进程自己撞在锁上、退得一声不吭(它的输出没人看得到)。
///
/// 探与真取之间有一个很窄的窗口,所以它**只是把话说得更好听**,不是判据:
/// 真正的互斥始终在 [`vidbrief_project::acquire`] 那里。
pub fn lock_note_if_taken(layout: &vidbrief_project::Layout) -> Option<String> {
    match vidbrief_project::acquire(layout) {
        Ok(_free) => None,
        Err(e) => Some(format!("{e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::Fake;

    /// 探锁:**空着**说空着,**被别人拿着**就把原因交回来(不是吞掉)。
    ///
    /// 「起作业」那种入口靠它把话说清楚 —— 从前作业撞在锁上只会退得一声不吭。
    #[test]
    fn the_lock_probe_says_whether_someone_else_holds_the_project() {
        let fake = Fake::new("lock-probe");
        assert_eq!(
            lock_note_if_taken(fake.layout()),
            None,
            "没人拿着的时候该说空着"
        );
        let held = vidbrief_project::acquire(fake.layout()).expect("拿一把");
        let note = lock_note_if_taken(fake.layout()).expect("别人拿着时要给出原因");
        assert!(!note.is_empty(), "原因不能是空话");
        drop(held);
        assert_eq!(lock_note_if_taken(fake.layout()), None, "放掉之后又是空着");
    }

    #[test]
    fn it_opens_locks_acts_and_reports_no_warning_on_a_normal_fs() {
        let fake = Fake::new("write-happy");
        let id = with_write(
            &fake.layout().root().display().to_string(),
            None,
            |opened, warning| {
                assert_eq!(warning, None, "正常文件系统不该有降级警告");
                Ok(opened.id().to_string())
            },
        )
        .unwrap();
        assert_eq!(id, "demo");
    }

    #[test]
    fn a_bad_project_id_fails_before_acting() {
        let mut acted = false;
        let err = with_write("没有这个项目", None, |_, _| {
            acted = true;
            Ok(())
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("找不到项目"), "{err}");
        assert!(!acted, "开不出来就不该执行动作");
    }

    /// **已经持有锁时不再取** —— 这是 MCP 那条路的前提。
    ///
    /// 不这么做的话,同一个进程第二次取锁会被自己挡住,而那个报错说的是
    /// "项目正被另一个会话写入" —— 那个"另一个会话"就是它自己。
    #[test]
    fn a_held_guard_is_not_taken_again() {
        let fake = Fake::new("write-held");
        let dir = fake.layout().root().display().to_string();

        let guard = vidbrief_project::acquire(fake.layout()).expect("先取一把");
        let id = with_write(&dir, Some(&guard), |opened, warning| {
            assert_eq!(warning, None, "锁是调用方取的,那句话也该由它说");
            Ok(opened.id().to_string())
        })
        .expect("已经持有时不该再取,更不该失败");
        assert_eq!(id, "demo");

        // 反证:不传凭证的话,这里就会撞上自己
        let err = with_write(&dir, None, |_, _| Ok(()))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("拿不到写锁"),
            "同一进程第二次取锁本来就该撞 —— 这正是凭证存在的理由:{err}"
        );
        drop(guard);
    }

    /// 动作失败时锁要照常释放(RAII),不留悬空。
    #[test]
    fn a_failing_action_still_releases_the_lock() {
        let fake = Fake::new("write-fail");
        let dir = fake.layout().root().display().to_string();

        let err = with_write(&dir, None, |_, _| -> Result<()> { anyhow::bail!("动作炸了") })
            .unwrap_err();
        assert!(err.to_string().contains("动作炸了"), "{err}");

        // 还能再取,说明上一把确实放了
        with_write(&dir, None, |_, _| Ok(())).expect("上一把锁该已经释放");
    }
    /// **写回之前比 `rev`**:你读到的那份被别人改过 → 这次写不许落盘。
    ///
    /// 这条债挂了一整个里程碑(`check_rev` 从没有调用点)。它和"锁改成按次取放"
    /// 是一对:锁只保证"同时只有一个人在写",管不了"两次调用之间被别人改过"——
    /// 而 `preprocess` 那种"开头读、跑几分钟、最后整份写回"的活正好落在缝里。
    #[test]
    fn a_write_based_on_a_stale_read_is_refused() {
        let fake = Fake::new("rev-stale");
        // 两个会话同时打开:都读到同一个 rev
        let mut older = fake.open();
        let mut newer = fake.open();

        newer.state_mut().progress.preprocess.mark_done();
        newer.save().expect("第一个写的人不该被拦");

        older.state_mut().progress.summarize.mark_done();
        let err = older.save().unwrap_err().to_string();
        assert!(err.contains("rev="), "报错要点出两个版本号:{err}");
        assert!(err.contains("没有落盘"), "要说清这次写没落地:{err}");

        // 重新打开 = 读到最新那份,照常能写
        let mut fresh = fake.open();
        fresh.state_mut().progress.summarize.mark_done();
        fresh
            .save()
            .expect("读到最新的一份就不该被拦(修法只有一条:重来一遍)");
    }
}
