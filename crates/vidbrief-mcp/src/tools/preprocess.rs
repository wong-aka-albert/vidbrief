//! B 组:步 1 前处理(`preprocess`)—— **一件作业,三个入口**。
//!
//! 设计见 `docs/DESIGN.md` §5。
//!
//! # 为什么拆成三件
//!
//! 从前是「一次调用做完」,而实测:宿主给每次调用 5 分钟上限,步 1 真跑过一次 **4 分 23 秒**
//! —— 差 37 秒就被掐,而一旦被掐,那次活干到哪儿了谁都不知道。所以:
//!
//! | 工具 | 干什么 | 什么时候用 |
//! |------|--------|-----------|
//! | `preprocess` | **起作业**(没在跑的话)并**等到跑完**(默认最多等 180 秒) | 开工;等够了就再调一次接着等 |
//! | `preprocess_progress` | 只看一眼跑到哪儿了 | 想知道还要多久;不想占着一次调用 |
//! | `preprocess_result` | 取结果 | 跑完之后 |
//!
//! **作业跑在独立进程里**(`vidbrief preprocess <项目>`),调用只是「看着它」。
//! 所以调用被宿主掐了、会话断了,作业照样跑完 —— 与查看服务独立成进程同一个道理
//! (`vidbrief-viz` 的模块头有那一条的来龙去脉)。
//!
//! 作业的进度落在 `01_preprocess/run.json`(`vidbrief_ops::run`),三个入口读的都是它。
//!
//! # 进度通知
//!
//! 客户端在请求的 `_meta.progressToken` 里给了 token 时,等待期间发
//! `notifications/progress`:五步制(开工 / 差帧图 / 抽帧 / 转录 / 渲染 / 完),
//! 抽帧那一段带着细粒度张数。**转录那几分钟没有细粒度进度可报**(whisper 不给),
//! 所以那一段只在换步时报一次,配一句说清「这一步最久」的话 ——
//! 假装有进度比没有进度更坏。
//!
//! 没给 token 的客户端(比如手上那个宿主)照常工作,只是看不到进度。

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use rmcp::Peer;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ProgressNotificationParam, ProgressToken};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::json::{blocking, failed, json as json_result, text};
use crate::server::VidbriefServer;
use crate::tools::attach_warning;
use vidbrief_ops::{PreprocessOptions, RunOutcome, RunRecord, RunStage, read_run};

/// 一次 `preprocess` 调用默认等多久。
///
/// 宿主给的调用上限实测是 5 分钟,这里留 2 分钟余量:等够了**不重开**,
/// 只是把「还在跑」如实报回去,让 agent 再调一次接着等。
const DEFAULT_WAIT_SECONDS: u32 = 180;

/// 盯记录的间隔。比记录本身的写盘节流(300ms)略慢一点就够。
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// **等第一份记录**的宽限:子进程起来、取到写锁、写下第一笔,都要一点时间。
///
/// `wait_seconds: 0` 说的是「不等它跑完」,不是「连它起没起都不看」——
/// 返回里必须带着那一趟的名字(`run_id`),否则后面两次调用就没有把手了。
const FIRST_RECORD_GRACE: Duration = Duration::from_secs(5);

/// 多久没发过进度就补一条 —— 给「客户端设了进度超时」的那种宿主留的活口。
const KEEPALIVE: Duration = Duration::from_secs(30);

/// 五步制的总数(与 [`RunStage::step`] 配套):0 开工 / 1 差帧图 / 2 抽帧 / 3 转录 / 4 渲染 / 5 完。
const TOTAL_STEPS: f64 = 5.0;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreprocessParams {
    #[schemars(description = "项目:项目目录的路径,或 `projects/` 下的项目名")]
    pub project_id: String,
    /// 采样帧率(每秒几个点)。默认 1.0。调高更细但更慢。
    #[serde(default)]
    pub fps: Option<f64>,
    /// 参与打分的缩小宽度。默认 480。
    #[serde(default)]
    pub width: Option<u32>,
    /// 要不要跑语音转文字。默认 true。
    ///
    /// **只在明确不需要转录时才传 false** —— 「转录失败」与「我选择跳过」是两回事,
    /// 拿它掩盖失败会让项目状态说谎(那样步 1 会被标成完成)。
    #[serde(default)]
    pub transcribe: Option<bool>,
    /// 要不要抽关键截图。默认 true。
    #[serde(default)]
    pub shots: Option<bool>,
    /// **无条件重跑**:不沿用上次的任何成果。
    ///
    /// 默认相反 —— 输入没变(源视频指纹 + `fps`/`width` + 模型 + 口径版本逐项相同、
    /// 材料还在且是工具写出去的那一份)时,画面与转录各自沿用上次的成果:
    /// 不重算,**也不作废**那批看图结论与核验戳。怀疑上次那批材料有问题时传 true。
    #[serde(default)]
    pub force: Option<bool>,
    /// **这次调用最多等多久**(秒)。不给 = 180;给 0 = 起了就回,自己用
    /// `preprocess_progress` 看进度。
    ///
    /// 等多久都**不影响作业**:作业跑在独立进程里,这次调用结束了它照跑。
    #[serde(default)]
    pub wait_seconds: Option<u32>,
}

/// 只看一个项目上的步 1 作业(`preprocess_progress` / `preprocess_result` 共用)。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WatchParams {
    #[schemars(description = "项目:项目目录的路径,或 `projects/` 下的项目名")]
    pub project_id: String,
}

#[tool_router(router = preprocess_tools, vis = "pub")]
impl VidbriefServer {
    #[tool(
        description = "步 1 前处理(**起作业并等它**):抽帧算差帧图 → 找变化密集区 → \
                        抽关键截图 → 语音转文字 → 渲染时间线。**纯算法,不调任何模型。**\
                        作业跑在**独立进程**里,这次调用只是看着它:\
                        默认最多等 180 秒,等够了它还在跑就把当前进度如实返回(**不重开**);\
                        再调一次 `preprocess` 是接着等,`preprocess_progress` 只看一眼,\
                        跑完了用 `preprocess_result` 取结果。\
                        **输入没变就不重算**:源视频指纹、`fps`/`width`、模型逐项相同,\
                        材料还在(且是工具写出去的那一份)时,画面与转录各自**沿用上次的成果** ——\
                        不重跑、也不作废那批看图结论与核验戳;返回里 `reuse` 说清沿用了哪半、\
                        没沿用的为什么。要无条件重来一遍就传 `force: true`。\
                        返回的 `done` 为 false 表示转录失败(材料保留、未标记完成),\
                        此时应把 `transcript.error` 转告用户,而不是继续往下走。",
        // **不是幂等的**:跑完了再调就是**重跑**。输入没变时它不重算(见返回里的 `reuse`),
        // 但输入变过(换了视频 / 改了 fps / 换了模型)就会重算,并作废那一半的成果
        //(`shot_checks_cleared` / `segment_checks_cleared` 报的就是作废掉的条数)。
        // 还在跑时它只是搭车、不重开;而客户端照着 idempotentHint 自动重试的话,
        // 正好会在"跑完了且输入变过"那一侧把人的校准成果抹掉 —— 所以照旧标 false。
        annotations(title = "跑前处理", read_only_hint = false, idempotent_hint = false)
    )]
    async fn preprocess(
        &self,
        Parameters(p): Parameters<PreprocessParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, CallToolResult> {
        let opened = self.open_read(&p.project_id).await?;
        let dir = opened.dir().to_path_buf();
        let layout = opened.layout().clone();
        drop(opened);

        let mut options = PreprocessOptions::default();
        if let Some(fps) = p.fps {
            options.fps = fps;
        }
        if let Some(width) = p.width {
            options.width = width;
        }
        if let Some(transcribe) = p.transcribe {
            options.transcribe = transcribe;
        }
        if let Some(shots) = p.shots {
            options.shots = shots;
        }
        if let Some(force) = p.force {
            options.force = force;
        }

        // 先说清「有没有人在跑、我等着谁」。这一步必须在起进程**之前**做:
        // 两个会话同时开工的话,后到的那个会撞在写锁上(它那一趟压根动不了),
        // 而它本该做的是**搭前一趟的车**。
        let before = {
            let layout = layout.clone();
            blocking(move || read_run(&layout)).await?
        };
        let live = before
            .as_ref()
            .filter(|record| record.outcome() == RunOutcome::Running)
            .map(|record| record.run_id.clone());

        let (job, mine) = match live {
            // 已经有一趟在跑:不重开,等它
            Some(run_id) => (None, run_id),
            None => {
                // 开工前先看一眼写锁:在别人手里就**别起作业**了 ——
                // 起了也只会撞在锁上、退得一声不吭(它的输出没人看得到),
                // 而调用方至少该知道"现在开不了工"是因为别人正写着这个项目。
                let taken = {
                    let layout = layout.clone();
                    blocking(move || Ok(vidbrief_ops::lock_note_if_taken(&layout))).await?
                };
                if let Some(note) = taken {
                    return Ok(text(format!(
                        "这个项目现在开不了步 1:**写锁在别的会话手里**({note})。\n\
                         那边正跑着步 1 的话,过一会儿用 `preprocess_progress` 看它;\
                         是在写别的(总结 / 报告 / 校准)就等它收工。\n\
                         只读的活不受影响(get_shot / list_shots / get_transcript 照用)。"
                    )));
                }

                let wanted = new_run_id();
                let job = blocking({
                    let dir = dir.clone();
                    let options = options.clone();
                    let wanted = wanted.clone();
                    move || spawn_run(&dir, &options, &wanted)
                })
                .await?;
                (Some(job), wanted)
            }
        };

        let budget = Duration::from_secs(p.wait_seconds.unwrap_or(DEFAULT_WAIT_SECONDS) as u64);
        let waited = self
            .watch_run(layout.clone(), job, Some(mine), budget, progress_sink(&ctx))
            .await?;

        // ── 把结果说清楚 ────────────────────────────────────────────
        let Some(record) = waited.record else {
            return Err(waited
                .trouble
                .unwrap_or_else(|| failed("起了步 1 作业,但一直没等到它的运行记录。")));
        };
        if !waited.ours {
            // 我们要等的那一趟没露过面。分两种:另一个会话正跑着同一个项目(项目确实在
            // 进步,不是错误),还是盘上那份只是**上一趟**留下的残影(那我们这趟没开工)。
            if record.outcome() == RunOutcome::Running {
                return Ok(text(format!(
                    "这个项目上**另一个会话**正在跑步 1(我们要起的那趟没动起来 —— 写锁在它手里)。\n\n\
                     {}\n\n就等它跑完:`preprocess_progress` 看进度,`preprocess_result` 取结果。",
                    record.summarize()
                )));
            }
            return Err(failed(format!(
                "起了步 1 作业,但一直没看到它的运行记录 —— 盘上那份是**上一趟**留下的({})。\n\
                 多半是它没能开工:写锁在别的会话手里。过一会儿再调一次 `preprocess`;\
                 手打一遍 `vidbrief preprocess <项目>` 能看到当时的原因。",
                record.summarize()
            )));
        }

        match record.outcome() {
            RunOutcome::Done => {
                let result = report_result(&record)?;
                Ok(attach_warning(result, record.warning.as_deref()))
            }
            // 失败/断了:如实报,并给一条去路
            RunOutcome::Failed | RunOutcome::Interrupted => {
                let mut result = failed(format!(
                    "步 1 没跑成:{}\n{}",
                    record.summarize(),
                    if record.outcome() == RunOutcome::Failed {
                        "修掉上面那个原因,再调一次 `preprocess` 重跑(材料是一遍遍覆盖写的)。"
                    } else {
                        "那一趟的进程已经不在了(记录停在半路、很久没有动静)。再调一次 \
                         `preprocess` 重跑 —— 超过 2 分钟没心跳的记录不会拦着新的一趟。"
                    }
                ));
                result
                    .content
                    .push(ContentBlock::text(pretty(&state_json(&record))));
                result.is_error = Some(true);
                Ok(attach_warning(result, record.warning.as_deref()))
            }
            RunOutcome::Running => Ok(text(format!(
                "{}\n\n**它还在跑,这次调用先不等了。**别再起新的(起了也只会接着等这一趟);\
                 隔一会儿再调一次 `preprocess`(接着等)、或者用 `preprocess_progress` 看一眼;\
                 跑完了用 `preprocess_result` 取结果。\n\n{}",
                one_liner(&record),
                pretty(&state_json(&record))
            ))),
        }
    }

    #[tool(
        description = "看一眼步 1 跑到哪儿了 —— **只读,不等**。\
                        返回这一趟的状态(running / interrupted / done / failed)、走到哪一步、\
                        抽帧张数、已经跑了多久。\
                        还没跑过步 1 时如实回答「没有运行记录」(那不是错误)。",
        annotations(title = "看前处理进度", read_only_hint = true, idempotent_hint = true)
    )]
    async fn preprocess_progress(
        &self,
        Parameters(p): Parameters<WatchParams>,
    ) -> Result<CallToolResult, CallToolResult> {
        let opened = self.open_read(&p.project_id).await?;
        let layout = opened.layout().clone();
        drop(opened);

        let record = blocking(move || read_run(&layout)).await?;
        let Some(record) = record else {
            return Ok(text(format!(
                "`{}` 上还没有步 1 的运行记录 —— 要么还没跑过步 1,要么记录被清掉了。\n\
                 跑一趟:`preprocess(project_id=\"{}\")`。",
                p.project_id, p.project_id
            )));
        };
        Ok(text(format!(
            "{}\n\n{}",
            one_liner(&record),
            pretty(&state_json(&record))
        )))
    }

    #[tool(
        description = "取步 1 的结果:跑完了就是那份完整交代(与从前 `preprocess` 直接返回的\
                        是同一份);**还在跑**就如实说「还没好」(不是错误,等一会儿再来);\
                        失败了或者半路断了会说清原因与重跑的办法。",
        annotations(title = "取前处理结果", read_only_hint = true, idempotent_hint = true)
    )]
    async fn preprocess_result(
        &self,
        Parameters(p): Parameters<WatchParams>,
    ) -> Result<CallToolResult, CallToolResult> {
        let opened = self.open_read(&p.project_id).await?;
        let layout = opened.layout().clone();
        drop(opened);

        let record = blocking(move || read_run(&layout)).await?;
        let Some(record) = record else {
            return Err(failed(format!(
                "`{}` 上还没有步 1 的运行记录 —— 结果还不存在。\n\
                 先跑:`preprocess(project_id=\"{}\")`(它会起作业并等;长视频等不够就再调一次)。",
                p.project_id, p.project_id
            )));
        };

        match record.outcome() {
            RunOutcome::Done => {
                let result = report_result(&record)?;
                Ok(attach_warning(result, record.warning.as_deref()))
            }
            RunOutcome::Running => Ok(text(format!(
                "{}\n\n**还没跑完**,所以还没有结果。隔一会儿再调一次 `preprocess_result`;\
                 或者调 `preprocess`(接着等它跑完)。",
                one_liner(&record)
            ))),
            RunOutcome::Failed => Err(failed(format!(
                "步 1 失败了,没有结果可取:{}\n\
                 修掉原因再调一次 `preprocess` 重跑 —— 材料是一遍遍覆盖写的,不会越跑越坏。",
                record.summarize()
            ))),
            RunOutcome::Interrupted => Err(failed(format!(
                "步 1 跑到一半断了,没有结果可取:{}。\n\
                 再调一次 `preprocess` 重跑 —— 那份停在半路的记录不会拦着新的一趟。",
                record.summarize()
            ))),
        }
    }
}

impl VidbriefServer {
    /// 盯着一趟作业:每变一次就往外送一条,直到跑完或者等够时间。
    ///
    /// 进度通知也在这一层发(见 [`progress_sink`])—— 作业那边只管写记录,
    /// 不关心有没有人在看(与库层「不打印任何东西」同一条约定)。
    async fn watch_run(
        &self,
        layout: vidbrief_project::Layout,
        job: Option<Job>,
        mine: Option<String>,
        budget: Duration,
        sink: Option<Sink>,
    ) -> Result<Waited, CallToolResult> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RunRecord>();
        let watcher = tokio::task::spawn_blocking(move || watch(layout, job, mine, budget, tx));

        // 收进度并转发。通道关掉(看完了)这个循环就结束。
        let mut last_sent: Option<(f64, Instant)> = None;
        while let Some(record) = rx.recv().await {
            let Some(sink) = &sink else {
                continue;
            };
            let progress = progress_of(&record);
            let due = match last_sent {
                // 数字涨了就发;没涨但久没发过,补一条 —— 客户端设了进度超时的话,
                // 转录那几分钟的沉默会被当成「服务死了」。
                Some((last, at)) => progress > last || at.elapsed() >= KEEPALIVE,
                None => true,
            };
            if !due {
                continue;
            }
            last_sent = Some((progress, Instant::now()));
            let param = ProgressNotificationParam::new(sink.token.clone(), progress)
                .with_total(TOTAL_STEPS)
                .with_message(one_liner(&record));
            // 通知发不出去不算失败(客户端可能已经走了),忽略即可
            let _ = sink.peer.notify_progress(param).await;
        }

        watcher
            .await
            .map_err(|e| failed(format!("盯着作业的那个线程崩了:{e}")))?
    }
}

/// 一趟作业的收尾状态。
struct Waited {
    record: Option<RunRecord>,
    /// 手头这份记录**是不是我们要等的那一趟**。不是的话(另一个会话正跑着同一个项目),
    /// 调用方得把这件事说出来,别拿别人的一趟冒充自己的。
    ours: bool,
    /// 记录压根没出现时的说明(作业没能开工)。
    trouble: Option<CallToolResult>,
}

/// 起一趟作业的那个子进程。
struct Job {
    child: Child,
}

impl Job {
    /// 它退了没。`Some(None)` = 被信号打断,没有退出码。
    fn exited(&mut self) -> Option<Option<i32>> {
        self.child.try_wait().ok().flatten().map(|status| status.code())
    }
}

/// 起一趟 `vidbrief preprocess`,**不等它**。
///
/// 子进程的 stdout / stderr **都不给管道**,这是踩出来的:
/// 会话可能在作业跑完之前就没了,那时管道的读端跟着消失 —— 子进程往一个没人读的
/// 管道里写,写失败会**当场 panic**,一项跑了五分钟的作业就这么没了
/// (实测:会话一退,作业在「抽关键截图」那一步当场崩掉)。
/// 它的输出去处是**运行记录**(`01_preprocess/run.json`),终端那几行是给人打的,
/// 机器跑的这一趟不需要。
fn spawn_run(dir: &Path, options: &PreprocessOptions, run_id: &str) -> anyhow::Result<Job> {
    let exe = std::env::current_exe().map_err(|e| anyhow::anyhow!("找不到自己的可执行文件:{e}"))?;
    let mut command = Command::new(&exe);
    command
        .arg("preprocess")
        .arg(dir)
        // 这一趟的名字由**起它的人**定:等它的人要能在记录里认出「就是这一趟」
        .args(["--run-id", run_id])
        .args(["-f", &options.fps.to_string()])
        .args(["-w", &options.width.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if !options.transcribe {
        command.arg("--no-transcribe");
    }
    if !options.shots {
        command.arg("--no-shots");
    }

    let child = command
        .spawn()
        .map_err(|e| anyhow::anyhow!("起不了步 1 作业进程({}):{e}", exe.display()))?;
    Ok(Job { child })
}

/// 盯着记录的那只眼睛(跑在阻塞线程上)。
///
/// 四条规则:
/// - **我们要等的那趟**(`mine`)记录跑完/失败了 → 收工,交回去;
/// - 记录还在跑、但不是我们要等的那趟 → 我们起的那趟没能开工(写锁在人家手里),
///   把 `ours = false` 标出来,继续把别人那一趟的进度转出去;
/// - 子进程没了、我们要等的那趟记录又一直没出现 → 报「没能开工」,附上它的 stderr;
/// - 等够时间 → 把手头这份交回去(调用方按「还在跑」说)。
fn watch(
    layout: vidbrief_project::Layout,
    mut job: Option<Job>,
    mine: Option<String>,
    budget: Duration,
    tx: tokio::sync::mpsc::UnboundedSender<RunRecord>,
) -> Result<Waited, CallToolResult> {
    let started = Instant::now();
    let mut last_signature: Option<String> = None;
    // 我们那趟露过面没 / 露过面的是不是别人
    let mut ours_ever_seen = false;
    let mut other_live = false;

    loop {
        let record = read_run(&layout).map_err(|e| failed(format!("{e:#}")))?;

        if let Some(record) = &record {
            let is_ours = mine.as_deref() == Some(record.run_id.as_str());
            ours_ever_seen |= is_ours;
            other_live = !is_ours && record.outcome() == RunOutcome::Running;

            // **只有我们要等的那一趟才算数**。盘上那份可能是上一趟留下的「跑完了」——
            // 拿它冒充这一趟的结果,就是这套机制里最坏的一种谎。
            if is_ours && record.outcome().is_settled() {
                let _ = tx.send(record.clone());
                return Ok(Waited {
                    record: Some(record.clone()),
                    ours: true,
                    trouble: None,
                });
            }
            // 同一份不重复送(签名的粒度就是「看得见的变化」)
            let signature = format!(
                "{:?}/{:?}/{:?}",
                record.stage, record.shots_done, record.updated_at
            );
            if last_signature.as_deref() != Some(signature.as_str()) {
                last_signature = Some(signature);
                let _ = tx.send(record.clone());
            }
        }

        // 子进程没了、而我们要等的那趟记录从来没出现过:它没能开工。
        let exited = job.as_mut().and_then(Job::exited);
        if let Some(code) = exited {
            job = None;
            if !ours_ever_seen && !other_live {
                return Ok(Waited {
                    record: None,
                    ours: false,
                    trouble: Some(failed(format!(
                        "步 1 作业没能开工(退出码 {}),它一个字都没写进项目。\n\
                         多半是**写锁在别的会话手里**(开工前那一眼还空着,起它的时候被占了),\n\
                         或者起进程的环境有问题。过一会儿再调一次 `preprocess`;\n\
                         手打一遍 `vidbrief preprocess <项目>` 能看到当时的原因。",
                        code.map(|c| c.to_string())
                            .unwrap_or_else(|| "被信号打断".into()),
                    ))),
                });
            }
        }

        if started.elapsed() >= budget && ours_ever_seen {
            // 等够了,而且看的就是我们要等的那一趟 —— 把它交回去。
            // (**必须**是我们要等的那一趟:盘上那份可能是上一趟留下的"跑完了",
            // 拿它冒充这一次的结果,就是这套机制里最坏的一种谎。)
            return Ok(Waited {
                record,
                ours: true,
                trouble: None,
            });
        }
        // 硬上限:等第一份记录也不能无限等(`wait_seconds: 0` 也要等到它起步)
        if started.elapsed() >= budget.max(FIRST_RECORD_GRACE) {
            let trouble = (!ours_ever_seen).then(|| {
                failed(
                    "起了步 1 作业,但一直没等到它的运行记录 —— 它没能开工。\n\
                     最可能是**写锁在别的会话手里**(开工前那一眼还空着,起它的时候被占了);\
                     也可能另一个会话正跑着这个项目(那边有记录的话,`preprocess_progress` 看得到)。\n\
                     过一会儿再调一次 `preprocess`;手打一遍 `vidbrief preprocess <项目>` 能看到当时的原因。",
                )
            });
            return Ok(Waited {
                record,
                ours: ours_ever_seen,
                trouble,
            });
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// 进度通知的出口。客户端没给 token 就是 `None` —— 那就一条都不发。
struct Sink {
    token: ProgressToken,
    peer: Peer<RoleServer>,
}

fn progress_sink(ctx: &RequestContext<RoleServer>) -> Option<Sink> {
    let token = ctx.meta.get_progress_token()?;
    Some(Sink {
        token,
        peer: ctx.peer.clone(),
    })
}

/// 抽帧那一段占的区间:2.0 → 2.99。
///
/// **不占满**:抽完最后一张和「开始转录」不该是同一个数 —— 进度条上那两件事不一样,
/// 而协议要求进度单调(回头跳会被当成退步)。0.99 就是留给下一步的那一格。
const SHOTS_HEADROOM: f64 = 0.99;

/// 五步制里的进度(单调不减)。抽帧那一段按张数细分。
fn progress_of(record: &RunRecord) -> f64 {
    match record.stage {
        RunStage::Shots if record.shots_total > 0 => {
            let done = record.shots_done.min(record.shots_total) as f64;
            RunStage::Shots.step() as f64 + SHOTS_HEADROOM * done / record.shots_total as f64
        }
        stage => stage.step() as f64,
    }
}

/// 一条记录的一句话 —— 与 `RunRecord::summarize` 是同一句(一处维护)。
fn one_liner(record: &RunRecord) -> String {
    record.summarize()
}

/// 记录里那些「给人看」的字段(报告本体不在里面 —— 取结果才需要它)。
fn state_json(record: &RunRecord) -> Value {
    json!({
        "run_id": record.run_id,
        "state": match record.outcome() {
            RunOutcome::Running => "running",
            RunOutcome::Interrupted => "interrupted",
            RunOutcome::Done => "done",
            RunOutcome::Failed => "failed",
        },
        "stage": record.stage,
        "stage_text": record.stage.describe(),
        "message": record.message,
        "error": record.error,
        "warning": record.warning,
        "elapsed_seconds": record.elapsed_seconds,
        "shots_done": record.shots_done,
        "shots_total": record.shots_total,
        "pid": record.pid,
        "started_at": record.started_at.to_rfc3339(),
        "updated_at": record.updated_at.to_rfc3339(),
        "options": {
            "fps": record.options.fps,
            "width": record.options.width,
            "transcribe": record.options.transcribe,
            "shots": record.options.shots,
        },
    })
}

/// 跑完了:把记录里那份交代原样交出去(**与从前 `preprocess` 直接返回的同一份**)。
fn report_result(record: &RunRecord) -> Result<CallToolResult, CallToolResult> {
    let report = record
        .report
        .as_ref()
        .ok_or_else(|| failed("记录说跑完了,却没有报告 —— 记录可能被人改过,重跑一次即可。"))?;
    let mut result = json_result(report)?;
    // 转录失败时**内容照给**(agent 需要看视觉部分做到了什么),
    // 但 `isError` 置真,免得 agent 以为步 1 完整跑完了
    if report.get("done") == Some(&Value::Bool(false)) {
        result.is_error = Some(true);
    }
    Ok(result)
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

/// 这一趟的名字。起它的人定,等它的人认。
fn new_run_id() -> String {
    format!(
        "mcp-{}-{}",
        std::process::id(),
        vidbrief_project::now().format("%Y%m%d-%H%M%S")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(stage: RunStage, done: usize, total: usize) -> RunRecord {
        RunRecord {
            run_id: "r1".into(),
            project_id: "demo".into(),
            stage,
            message: String::new(),
            error: None,
            warning: None,
            options: vidbrief_ops::RunOptions {
                fps: 1.0,
                width: 480,
                transcribe: true,
                shots: true,
            },
            pid: 1,
            started_at: vidbrief_project::now(),
            updated_at: vidbrief_project::now(),
            elapsed_seconds: 0.0,
            shots_done: done,
            shots_total: total,
            report: None,
        }
    }

    /// 进度要**单调**:客户端拿它画进度条,回头跳会被当成「退步了」。
    #[test]
    fn progress_never_goes_backwards() {
        let mut last = -1.0;
        for r in [
            record(RunStage::Starting, 0, 0),
            record(RunStage::Analyzing, 0, 0),
            record(RunStage::Shots, 0, 10),
            record(RunStage::Shots, 5, 10),
            record(RunStage::Shots, 10, 10),
            record(RunStage::Transcribing, 0, 0),
            record(RunStage::Rendering, 0, 0),
            record(RunStage::Done, 0, 0),
        ] {
            let step = progress_of(&r);
            assert!(step > last, "进度退了:{last} → {step}({:?})", r.stage);
            assert!(step <= TOTAL_STEPS, "越界:{step}");
            last = step;
        }
        // 抽帧那一段真的在细分(不是一跳到位)
        assert!(
            progress_of(&record(RunStage::Shots, 5, 10))
                > progress_of(&record(RunStage::Shots, 0, 10))
        );
    }

    /// 五步制要覆盖每个阶段 —— 少一个,那条通知就永远不发。
    #[test]
    fn every_stage_has_a_step() {
        for stage in [
            RunStage::Starting,
            RunStage::Analyzing,
            RunStage::Shots,
            RunStage::Transcribing,
            RunStage::Rendering,
            RunStage::Done,
        ] {
            let step = stage.step() as f64;
            assert!((0.0..=TOTAL_STEPS).contains(&step), "{stage:?} 的步数越界");
        }
    }
}
