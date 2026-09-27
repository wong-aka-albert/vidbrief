//! 步 1 作业的「运行记录」—— 让一次跑几分钟的活**被别人看见**。
//!
//! # 为什么需要它
//!
//! 步 1 在长视频上要几分钟,而 MCP 的**一次调用**在宿主那边有上限(实测宿主给
//! 5 分钟,而真跑过一次 4 分 23 秒 —— 差 37 秒)。**作业的寿命不该由一次调用的寿命决定**:
//!
//! - 作业跑在**独立进程**里(与查看服务同一个道理:工具进程是会话的,作业是项目的);
//! - 会话想知道「跑到哪儿了」,读这份记录;会话断了,作业照样跑完。
//!
//! 记录写在 `01_preprocess/run.json`,**一个项目一份**。它不是材料(材料以
//! `state.bin` 与各材料文件为准,有内容底账管着),是一份**过程投影**:
//! 谁在跑、跑到第几步、跑完的结论或失败的原因。
//!
//! # 记录里写的 ≠ 读出来的判断
//!
//! 进程被 `kill`、断电、机器休眠,记录就永远停在「还在跑」那一行 —— 而写的那个进程
//! 早就没了。所以读完记录还要判一次活性:
//!
//! | 记录里 | 读的人看到 |
//! |--------|-----------|
//! | `stage = done` | [`RunOutcome::Done`] |
//! | `error` 有值 | [`RunOutcome::Failed`] |
//! | 还在跑、心跳新鲜(≤ [`STALE_AFTER_SECONDS`]) | [`RunOutcome::Running`] |
//! | 还在跑、**很久没动静** | [`RunOutcome::Interrupted`] —— 推断:那一趟断了 |
//!
//! 心跳是专门为这一格存在的:转录那几分钟没有任何细粒度进度可报,
//! 但「我还活着」得说出来(见 [`HEARTBEAT`])。
//!
//! # 写记录是**辅助**,不许拦作业
//!
//! 磁盘满、目录只读、文件被人改成乱码 —— 这些都不该让步 1 跑不成。
//! 所以入口那边是「写不动就退化成没有记录」,而不是「写不动就报错收工」。

use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use vidbrief_project::Layout;

use crate::preprocess::{PreprocessOptions, PreprocessReport};

/// 心跳:每隔这么久把 `updated_at` 与已用时推一次。
///
/// 为什么不能只靠进度回调:抽帧有回调(每张一次),转录**没有** ——
/// whisper 一跑几分钟不吭声,没有心跳的话,那几分钟里谁读都以为它断了。
pub const HEARTBEAT: Duration = Duration::from_secs(2);

/// 两次写盘之间至少隔这么久。**阶段变了就立刻写**(不受它限制)。
///
/// 抽帧一张几十毫秒,照单全写就是几十次 IO/秒 —— 记录是给人看的,不需要那么快。
pub const WRITE_INTERVAL: Duration = Duration::from_millis(300);

/// 多久没有心跳就当那一趟断了。取心跳的 60 倍:机器卡一下、视频在慢盘上都够宽,
/// 而真的断了也不必让人干等。
pub const STALE_AFTER_SECONDS: i64 = 120;

/// 作业走到哪一步了。**失败不在这个枚举里** —— 失败了 `error` 有值,
/// `stage` 保留它失败时那一步(「抽帧的时候失败的」比「失败了」有用)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStage {
    /// 取写锁、读项目(几毫秒,但「卡在锁上」是一种要能看见的状态)
    Starting,
    /// 算差帧图与变化密集区
    Analyzing,
    /// 抽关键截图
    Shots,
    /// 语音转文字(最久的一段)
    Transcribing,
    /// 渲染时间线
    Rendering,
    /// 跑完了
    Done,
}

impl RunStage {
    /// 给人看的一句话(记录里 `message` 之外的那半句)。
    pub fn describe(self) -> &'static str {
        match self {
            Self::Starting => "正在开工(取写锁、读项目)",
            Self::Analyzing => "算差帧图与变化密集区",
            Self::Shots => "抽关键截图",
            Self::Transcribing => "语音转文字",
            Self::Rendering => "渲染时间线",
            Self::Done => "跑完了",
        }
    }

    /// 五步制里的第几步(0 = 还在开工,5 = 跑完)。
    ///
    /// 进度条要一个单调递增的数,而阶段是离散的;转录那一步没有细粒度进度
    /// (whisper 不给),所以**只能按步报** —— 但至少每换一步都动一次。
    pub fn step(self) -> u8 {
        match self {
            Self::Starting => 0,
            Self::Analyzing => 1,
            Self::Shots => 2,
            Self::Transcribing => 3,
            Self::Rendering => 4,
            Self::Done => 5,
        }
    }
}

/// 这一趟用的是哪套参数(与数据契约同一套 snake_case 名字)。
///
/// 记它的用处:读记录的人能分清「这一趟是 1fps、带转录」和「那一趟跳过转录」——
/// 两趟的记录长得一样的话,「跑完了」会被当成同一件事。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunOptions {
    pub fps: f64,
    pub width: u32,
    pub transcribe: bool,
    pub shots: bool,
}

impl From<&PreprocessOptions> for RunOptions {
    fn from(opts: &PreprocessOptions) -> Self {
        Self {
            fps: opts.fps,
            width: opts.width,
            transcribe: opts.transcribe,
            shots: opts.shots,
        }
    }
}

/// 盘上那份记录。
///
/// `deny_unknown_fields`:手改出来的字段不会被当成没看见(与数据契约同一条规矩)。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunRecord {
    /// 这一趟的标识:`<年月日>-<时分秒>-<pid>`。用来分清「跑完了」说的是哪一趟。
    pub run_id: String,
    pub project_id: String,
    pub stage: RunStage,
    /// 具体在干什么(「抽关键截图 12/57」)。
    pub message: String,
    /// 这一趟失败了的话,失败原因;没失败就是 `None`。
    #[serde(default)]
    pub error: Option<String>,
    /// 写锁降级之类**必须在开工前说出去**的那句话(正常时是 `None`)。
    ///
    /// 为什么放进记录:作业跑在独立进程里,它的 stderr 被会话收走了 ——
    /// 只打在终端上的话,agent 永远看不到「这个文件系统不支持文件锁」。
    #[serde(default)]
    pub warning: Option<String>,
    pub options: RunOptions,
    /// 跑它的那个进程(诊断用:报得出「上一趟是 pid 1234 起的」)。
    pub pid: u32,
    pub started_at: DateTime<FixedOffset>,
    /// **心跳**:最后一次写盘的时刻。判活性看的是它,不是 `started_at`。
    pub updated_at: DateTime<FixedOffset>,
    pub elapsed_seconds: f64,
    /// 抽帧进度。还没到抽帧那一步时是 `0 / 0`(`shots: false` 时永远是 `0 / 0`)。
    pub shots_done: usize,
    pub shots_total: usize,
    /// 跑完时的完整交代 —— **与工具/命令直接返回的那份同源**
    /// (`PreprocessReport` 的 JSON),省得 agent 学两种结果形状。
    #[serde(default)]
    pub report: Option<serde_json::Value>,
}

/// 读完记录**推断**出来的结论(见模块文档那张表)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    /// 记录说还在跑,心跳也新鲜。
    Running,
    /// 记录说还在跑,但很久没动静了 —— 那一趟多半已经不在。
    Interrupted,
    Done,
    /// 失败了(`error` 里是原因)。
    Failed,
}

impl RunOutcome {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Running => "还在跑",
            Self::Interrupted => "断了",
            Self::Done => "跑完了",
            Self::Failed => "失败了",
        }
    }

    /// 要不要继续等。
    pub fn is_settled(self) -> bool {
        matches!(self, Self::Done | Self::Failed)
    }
}

impl RunRecord {
    /// 记录自己说跑完了/失败了没(不含任何推断)。
    pub fn is_finished(&self) -> bool {
        self.stage == RunStage::Done || self.error.is_some()
    }

    /// 判活性。`now` 由调用方给:同一个时刻判两份记录、测试里造「很久以前」的记录,
    /// 都要能传进来。
    pub fn outcome_at(&self, now: DateTime<FixedOffset>) -> RunOutcome {
        if self.error.is_some() {
            return RunOutcome::Failed;
        }
        if self.stage == RunStage::Done {
            return RunOutcome::Done;
        }
        let quiet = now.signed_duration_since(self.updated_at).num_seconds();
        if quiet <= STALE_AFTER_SECONDS {
            RunOutcome::Running
        } else {
            RunOutcome::Interrupted
        }
    }

    /// 同上,拿「现在」判。
    pub fn outcome(&self) -> RunOutcome {
        self.outcome_at(vidbrief_project::now())
    }

    /// 现在是「哪一步、在干什么」—— 记录里的 `message` 已经自带上下文,用它;
    /// 没有 message 才退回阶段名。
    fn where_now(&self) -> String {
        if self.message.is_empty() {
            self.stage.describe().to_string()
        } else {
            self.message.clone()
        }
    }

    /// 多久没动静了(秒)。判断"断了"看的就是它,报出去也要用它。
    pub fn quiet_seconds(&self) -> i64 {
        vidbrief_project::now()
            .signed_duration_since(self.updated_at)
            .num_seconds()
    }

    /// 一句话说清这一趟 —— 给工具与命令直接用。
    ///
    /// 一句话要能独立读懂:哪一趟、什么结局、现在/最后停在哪、多久了。
    /// 「这一趟跑完了」与「步 1 完成了」是两件事(转录失败时作业跑到了终点、
    /// 但步 1 没被标成完成),所以后半句也得说。
    pub fn summarize(&self) -> String {
        let mut text = match self.outcome() {
            RunOutcome::Running => format!(
                "{} 的步 1 还在跑:{}",
                self.run_id,
                self.where_now()
            ),
            RunOutcome::Interrupted => format!(
                "{} 的步 1 断了:记录停在「{}」,已经 {} 秒没有动静",
                self.run_id,
                self.where_now(),
                self.quiet_seconds()
            ),
            RunOutcome::Done => {
                format!("{} 的步 1 跑完了(用时 {:.0} 秒)", self.run_id, self.elapsed_seconds)
            }
            RunOutcome::Failed => format!(
                "{} 的步 1 失败了:停在「{}」",
                self.run_id,
                self.where_now()
            ),
        };
        if !matches!(self.outcome(), RunOutcome::Done) && self.elapsed_seconds > 0.0 {
            text.push_str(&format!("(已 {:.0} 秒)", self.elapsed_seconds));
        }
        if let Some(error) = &self.error {
            text.push_str(&format!(";原因:{error}"));
        }
        if self.report.as_ref().and_then(|r| r.get("done")) == Some(&serde_json::Value::Bool(false))
        {
            text.push_str(";**步 1 未标记完成**(看报告里的 `transcript`)");
        }
        text
    }
}

/// 记录在项目里的位置。
pub fn path(layout: &Layout) -> PathBuf {
    layout.preprocess_dir().join("run.json")
}

/// 读盘上的记录。
///
/// - **不在** → `Ok(None)`:这个项目还没跑过步 1。
/// - **读得动** → `Ok(Some(_))`。
/// - **读不动**(写了一半被打断 / 被人改成乱码)→ `Err`:不许当成「还没跑过」,
///   那种「静悄悄按最松的那头算」正是出过事的地方。
pub fn read(layout: &Layout) -> Result<Option<RunRecord>> {
    let file = path(layout);
    if !file.exists() {
        return Ok(None);
    }
    vidbrief_project::read(&file)
        .map(Some)
        .with_context(|| format!("运行记录读不动:{}", file.display()))
}

/// 开记录失败的两类原因。**分得很开**:一类是「这个项目上正有一趟在跑」(不许开工),
/// 一类是「记录写不下去」(作业照跑,只是没有记录可看)。
#[derive(Debug)]
pub enum StartError {
    /// 已经有一趟在跑 —— 消息里说清了是哪一趟([`RunRecord::summarize`] 那一路)。
    Busy(String),
    /// 记录落不下去(磁盘满、目录不可写……)。**这不是作业的错**,调用方该降级继续。
    Storage(anyhow::Error),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy(why) => write!(f, "{why}"),
            Self::Storage(e) => write!(f, "{e:#}"),
        }
    }
}

/// 同一秒里开的第二趟也得有个不一样的 id(进程内自增,从 1 起)。
static RUN_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 自己给自己起个名字:秒级时间戳 + pid + 序号(同一秒里开的第二趟也不撞名)。
fn generated_run_id() -> String {
    let seq = RUN_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    format!(
        "{}-{}-{seq}",
        vidbrief_project::now().format("%Y%m%d-%H%M%S"),
        std::process::id()
    )
}

/// 一边跑一边写记录的那只手。
///
/// 用法:`start` → 若干次 [`RunRecorder::stage`] → [`RunRecorder::finish_ok`] /
/// [`RunRecorder::finish_err`]。**中途退出也会收尾**(见 `Drop`):
/// 记录不许停在「还在跑」却没人跑的状态 —— 除非进程是**被杀**的,那种情况由
/// [`RunRecord::outcome_at`] 的陈旧判据接住。
pub struct RunRecorder {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    record: Mutex<RunRecord>,
    /// 上次写盘的时刻(节流用)。
    last_write: Mutex<Instant>,
    /// 开工时刻,算 `elapsed_seconds` 用。
    started: Instant,
    /// 心跳线程的停车牌:拿走它,线程立刻醒着退出。
    stop: Mutex<Option<mpsc::Sender<()>>>,
}

impl RunRecorder {
    /// 开一份记录。**已经有一趟在跑时拒绝开工**([`StartError::Busy`]) —— 两趟同时写
    /// 同一个项目,后写的会把先写的盖掉,而「哪一趟是哪一趟」谁都说不清。
    ///
    /// 唯一会撞上这条的入口是手打命令行(MCP 那边在起进程之前就先看过记录了)。
    /// 记录读不动时**允许**开工:那种残迹多半是上一次写了一半,新的一趟覆盖它最干净。
    pub fn start(layout: &Layout, opts: &PreprocessOptions) -> Result<Self, StartError> {
        Self::start_named(layout, opts, &generated_run_id())
    }

    /// 同上,但这一趟的名字由调用方给。
    ///
    /// 为什么需要:起了作业的那个人(比如 MCP 会话)要能**认出"就是这一趟"** ——
    /// 盘上还可能留着上一趟的记录,而「跑完了」说的到底是哪一趟,只能靠名字分。
    pub fn start_named(
        layout: &Layout,
        opts: &PreprocessOptions,
        run_id: &str,
    ) -> Result<Self, StartError> {
        if let Ok(Some(existing)) = read(layout) {
            if existing.outcome() == RunOutcome::Running {
                return Err(StartError::Busy(format!(
                    "这个项目上已经有一趟步 1 在跑:{}(pid {},已 {:.0} 秒,现在在{}:{})。\n\
                     等它跑完再开新的 —— 记录在 {}。它超过 {} 秒没有心跳就会被当成断了,\
                     那时重跑不会撞上这一条。",
                    existing.run_id,
                    existing.pid,
                    existing.elapsed_seconds,
                    existing.stage.describe(),
                    existing.message,
                    path(layout).display(),
                    STALE_AFTER_SECONDS
                )));
            }
        }

        let now = vidbrief_project::now();
        let record = RunRecord {
            run_id: run_id.to_string(),
            project_id: layout
                .root()
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            stage: RunStage::Starting,
            message: "开工".to_string(),
            error: None,
            warning: None,
            options: RunOptions::from(opts),
            pid: std::process::id(),
            started_at: now,
            updated_at: now,
            elapsed_seconds: 0.0,
            shots_done: 0,
            shots_total: 0,
            report: None,
        };

        let inner = Arc::new(Inner {
            path: path(layout),
            record: Mutex::new(record),
            last_write: Mutex::new(Instant::now()),
            started: Instant::now(),
            stop: Mutex::new(None),
        });
        let recorder = Self {
            inner: inner.clone(),
        };
        // 第一份要**立刻**落盘:起作业的人随后就来读它了(等不到会以为没起成)
        recorder
            .inner
            .flush(true)
            .map_err(StartError::Storage)?;

        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let beat = inner;
        std::thread::Builder::new()
            .name("vidbrief-run-heartbeat".into())
            .spawn(move || loop {
                match stop_rx.recv_timeout(HEARTBEAT) {
                    // 停车牌被拿走(正常收工):走人
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                    Err(RecvTimeoutError::Timeout) => {
                        let _ = beat.flush(true);
                    }
                }
            })
            .map_err(|e| StartError::Storage(anyhow::Error::new(e).context("起不了心跳线程")))?;
        *recorder
            .inner
            .stop
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(stop_tx);

        Ok(recorder)
    }

    pub fn run_id(&self) -> String {
        self.inner
            .record
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .run_id
            .clone()
    }

    /// 记一句「必须在开工前说出去」的话(写锁降级之类)。写盘由第一次 `stage` 带上。
    pub fn note_warning(&self, warning: &str) {
        let mut record = self.inner.record.lock().unwrap_or_else(|e| e.into_inner());
        record.warning = Some(warning.to_string());
    }

    /// 报一步。写不动就算了(**记录是辅助,不许拦作业**),所以不返回错误。
    pub fn stage(&self, stage: RunStage, message: impl Into<String>, shots: Option<(usize, usize)>) {        let changed = {
            let mut record = self.inner.record.lock().unwrap_or_else(|e| e.into_inner());
            let changed = record.stage != stage;
            record.stage = stage;
            record.message = message.into();
            if let Some((done, total)) = shots {
                record.shots_done = done;
                record.shots_total = total;
            }
            changed
        };
        let _ = self.inner.flush(changed);
    }

    /// 收尾:成功。
    pub fn finish_ok(&self, report: &PreprocessReport) {
        {
            let mut record = self.inner.record.lock().unwrap_or_else(|e| e.into_inner());
            record.stage = RunStage::Done;
            record.message = format!("跑完了,用时 {:.1} 秒", report.elapsed_seconds);
            record.report = serde_json::to_value(report).ok();
            record.error = None;
            if let Some(shots) = report.shots {
                record.shots_done = shots;
                record.shots_total = shots;
            }
        }
        let _ = self.inner.flush(true);
        self.stop_beat();
    }

    /// 收尾:失败。
    pub fn finish_err(&self, error: &str) {
        {
            let mut record = self.inner.record.lock().unwrap_or_else(|e| e.into_inner());
            record.message = format!("{}到一半失败了", record.stage.describe());
            record.error = Some(error.to_string());
        }
        let _ = self.inner.flush(true);
        self.stop_beat();
    }

    fn stop_beat(&self) {
        self.inner
            .stop
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
    }
}

impl Inner {
    /// 把内存里那份甩到盘上。`force` 之外还要过节流(见 [`WRITE_INTERVAL`])。
    fn flush(&self, force: bool) -> Result<()> {
        let mut record = self.record.lock().unwrap_or_else(|e| e.into_inner());
        if !force {
            let mut last = self.last_write.lock().unwrap_or_else(|e| e.into_inner());
            if last.elapsed() < WRITE_INTERVAL {
                return Ok(());
            }
            *last = Instant::now();
        }
        record.updated_at = vidbrief_project::now();
        record.elapsed_seconds = self.started.elapsed().as_secs_f64();
        vidbrief_project::write_atomic(&self.path, &*record)
    }
}

impl Drop for RunRecorder {
    fn drop(&mut self) {
        // 没写收尾就退出了(panic 展开、`?` 提前返回、进程自己跑到了尽头):
        // 得留一句实话,否则记录停在「还在跑」而没人跑。
        let unfinished = {
            let record = self.inner.record.lock().unwrap_or_else(|e| e.into_inner());
            !record.is_finished()
        };
        if unfinished {
            {
                let mut record = self.inner.record.lock().unwrap_or_else(|e| e.into_inner());
                record.message = format!("{}到一半就退出了", record.stage.describe());
                record.error = Some(
                    "跑它的进程没来得及收尾就退出了(中途出错、或者被杀)。重跑一次即可 —— \
                     材料是一遍遍覆盖写的,不会越跑越坏。"
                        .to_string(),
                );
            }
            let _ = self.inner.flush(true);
        }
        self.stop_beat();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::Fake;

    fn fake_report(fake: &Fake) -> PreprocessReport {
        PreprocessReport {
            project_id: fake
                .layout()
                .root()
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            source: vidbrief_project::RelPath::new("source/video.mp4").expect("合法相对路径"),
            duration: 60.0,
            analysis: crate::preprocess::AnalysisSummary {
                curve_points: 60,
                fps: 1.0,
                max_score: 0.9,
                regions: 3,
                mass_threshold: 0.6,
                mass_median: 0.01,
                mass_k: 3.0,
                mass_floor: 0.6,
            },
            shots: Some(10),
            shots_skipped: Vec::new(),
            transcript: crate::preprocess::TranscriptOutcome::NoAudio,
            shot_checks_cleared: 0,
            segment_checks_cleared: 0,
            timeline: None,
            timeline_error: None,
            reuse: crate::preprocess::ReuseOutcome {
                visual: false,
                transcript: false,
                visual_why_not: Some("测试夹具:没跑过真东西".into()),
                transcript_why_not: Some("测试夹具:没跑过真东西".into()),
                source_sha256: "0".repeat(64),
                source_changed: false,
            },
            done: true,
            elapsed_seconds: 12.5,
        }
    }

    /// 造一份「很久以前写的」记录:直接改盘上的 `updated_at`。
    fn backdate(fake: &Fake) {
        let file = path(fake.layout());
        let mut record: RunRecord = vidbrief_project::read(&file).expect("读不到记录");
        record.updated_at -= chrono::Duration::seconds(STALE_AFTER_SECONDS + 10);
        vidbrief_project::write_atomic(&file, &record).expect("写不回去");
    }

    #[test]
    fn a_run_writes_down_where_it_is_and_how_it_ended() {
        let fake = Fake::new("run-happy");
        let recorder =
            RunRecorder::start(fake.layout(), &PreprocessOptions::default()).expect("开一份记录");
        recorder.stage(RunStage::Analyzing, "差帧图算完了", None);
        recorder.stage(RunStage::Shots, "抽关键截图 3/10", Some((3, 10)));

        let record = read(fake.layout()).unwrap().expect("记录在");
        assert_eq!(record.stage, RunStage::Shots);
        assert_eq!(record.message, "抽关键截图 3/10");
        assert_eq!((record.shots_done, record.shots_total), (3, 10));
        assert_eq!(record.outcome(), RunOutcome::Running);
        assert_eq!(record.pid, std::process::id());
        assert!(record.report.is_none(), "还没跑完,不该有报告");
        assert!(record.run_id.contains(&std::process::id().to_string()));
        assert_eq!(record.options.fps, 1.0);

        // 收尾之后记录里要能读到完整交代
        recorder.finish_ok(&fake_report(&fake));
        let record = read(fake.layout()).unwrap().unwrap();
        assert_eq!(record.outcome(), RunOutcome::Done);
        assert!(record.is_finished());
        let stored = record.report.expect("报告要跟着记录一起留下");
        assert_eq!(stored["shots"], 10);
        assert_eq!(stored["transcript"]["outcome"], "no_audio");
    }

    #[test]
    fn a_failure_keeps_the_stage_it_died_on() {
        let fake = Fake::new("run-failed");
        let recorder = RunRecorder::start(fake.layout(), &PreprocessOptions::default()).unwrap();
        recorder.stage(RunStage::Transcribing, "语音转文字", None);
        recorder.finish_err("whisper 不见了");

        let record = read(fake.layout()).unwrap().unwrap();
        assert_eq!(record.outcome(), RunOutcome::Failed);
        assert_eq!(record.stage, RunStage::Transcribing, "留在失败的那一步");
        assert!(
            record.summarize().contains("whisper 不见了"),
            "{}",
            record.summarize()
        );
    }

    /// **断了要认出来** —— 记录停在「还在跑」而进程早没了,是这套机制唯一会骗人的地方。
    #[test]
    fn a_record_whose_heartbeat_stopped_reads_as_interrupted() {
        let fake = Fake::new("run-stale");
        let recorder = RunRecorder::start(fake.layout(), &PreprocessOptions::default()).unwrap();
        recorder.stage(RunStage::Transcribing, "语音转文字", None);
        backdate(&fake);
        // 记录对象还活着(它就在 recorder 手里),但盘上那一份已经很久没动静
        let record = read(fake.layout()).unwrap().unwrap();
        assert_eq!(record.outcome(), RunOutcome::Interrupted);
        assert!(!record.is_finished(), "断了 ≠ 跑完:记录里那一行没变");
        assert!(record.summarize().contains("断了"), "{}", record.summarize());
    }

    /// 心跳是**真的在跳**:转录那几分钟没有任何进度回调,全靠它。
    #[test]
    fn the_heartbeat_keeps_the_record_fresh_while_a_stage_says_nothing() {
        let fake = Fake::new("run-heartbeat");
        let recorder = RunRecorder::start(fake.layout(), &PreprocessOptions::default()).unwrap();
        recorder.stage(RunStage::Transcribing, "语音转文字", None);
        let before = read(fake.layout()).unwrap().unwrap().elapsed_seconds;

        std::thread::sleep(HEARTBEAT + Duration::from_millis(600));

        let after = read(fake.layout()).unwrap().unwrap();
        assert!(
            after.elapsed_seconds > before,
            "心跳没跳:盘上还是 {before}(现在 {})",
            after.elapsed_seconds
        );
        assert_eq!(after.outcome(), RunOutcome::Running, "还在跳就该读成还在跑");
    }

    /// 同一条记录上不许开第二趟 —— 两趟同时写,后写的把先写的盖掉。
    #[test]
    fn a_second_run_is_refused_while_the_first_is_alive() {
        let fake = Fake::new("run-two");
        let first = RunRecorder::start(fake.layout(), &PreprocessOptions::default()).unwrap();
        let first_id = first.run_id();

        let err = RunRecorder::start(fake.layout(), &PreprocessOptions::default())
            .err()
            .expect("第二趟该被拒");
        let why = match err {
            StartError::Busy(why) => why,
            StartError::Storage(e) => panic!("该是「有一趟在跑」,却报了这个:{e:#}"),
        };
        assert!(why.contains("已经有一趟"), "{why}");
        assert!(why.contains(&first_id), "报错要说清是哪一趟:{why}");
        assert_eq!(
            read(fake.layout()).unwrap().unwrap().run_id,
            first_id,
            "拒绝开工不许动第一条记录"
        );
    }

    /// 但**断了的不拦**:否则一次断电就把这个项目永久锁在「跑不了」上。
    #[test]
    fn an_interrupted_record_does_not_block_a_new_run() {
        let fake = Fake::new("run-after-stale");
        let first = RunRecorder::start(fake.layout(), &PreprocessOptions::default()).unwrap();
        let first_id = first.run_id();
        first.finish_err("假装上一趟跑到一半断了");
        backdate(&fake);

        let second = RunRecorder::start(fake.layout(), &PreprocessOptions::default())
            .expect("断了就该能重开");
        assert_ne!(second.run_id(), first_id);
        let record = read(fake.layout()).unwrap().unwrap();
        assert_eq!(record.run_id, second.run_id());
        assert_eq!(record.outcome(), RunOutcome::Running, "新的一趟从头算");
    }

    /// 提前退出(没写收尾)也要留一句实话 —— 记录不许停在「还在跑」而没人跑。
    #[test]
    fn leaving_without_finishing_is_recorded_as_a_failure() {
        let fake = Fake::new("run-dropped");
        {
            let recorder =
                RunRecorder::start(fake.layout(), &PreprocessOptions::default()).unwrap();
            recorder.stage(RunStage::Shots, "抽关键截图 1/10", Some((1, 10)));
            // 出了这个作用域,recorder 被 drop —— 等价于中途退出
        }
        let record = read(fake.layout()).unwrap().unwrap();
        assert_eq!(record.outcome(), RunOutcome::Failed);
        assert_eq!(record.stage, RunStage::Shots);
        assert!(
            record.error.unwrap().contains("没来得及收尾"),
            "要说清是中途断的"
        );
    }

    /// 读不动就是读不动 —— 不许当成「还没跑过」。
    #[test]
    fn a_record_that_cannot_be_read_is_an_error_not_an_empty_slot() {
        let fake = Fake::new("run-broken");
        std::fs::create_dir_all(fake.layout().preprocess_dir()).unwrap();
        std::fs::write(path(fake.layout()), "{ 这不是 JSON").unwrap();
        let err = read(fake.layout()).unwrap_err().to_string();
        assert!(err.contains("运行记录读不动"), "{err}");
    }
}
