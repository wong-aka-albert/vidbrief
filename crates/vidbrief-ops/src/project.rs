//! 项目的**建**与**开**。
//!
//! 对应 MCP 工具组的 A 组(`create_project` / `list_projects` / `get_project`),
//! 以及 CLI 的 `init` / `status`。设计见 `docs/DESIGN.md` §4.8(源视频怎么进来)、§10.5。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use vidbrief_project::{
    Health, Layout, Project, ProjectId, ProjectState, SourceLink, Transcript, VideoMeta,
    check_source_link, materialize, plan, simplify_path, video_rel_for,
};

/// 打开(或新建)项目时的统一入口参数。
///
/// `arg` 的两种写法都认(与 CLI 一致):
/// 直接给项目目录,或只给 `projects/` 下的项目名。
pub fn resolve(arg: &str) -> Result<Layout> {
    let direct = Path::new(arg);
    if direct.join(vidbrief_project::layout::PROJECT_FILE).is_file() {
        return Ok(Layout::new(direct));
    }
    let under = Path::new(vidbrief_project::layout::PROJECTS_DIR).join(arg);
    if under.join(vidbrief_project::layout::PROJECT_FILE).is_file() {
        return Ok(Layout::new(under));
    }
    bail!(
        "找不到项目:`{arg}` —— 它既不是含 {} 的目录,也不是 projects/ 下的项目名。\n\
         先 `list_projects` 看看有哪些项目,或者让用户给一下项目目录的路径。",
        vidbrief_project::layout::PROJECT_FILE
    )
}

/// 一个已经打开的项目:身份 + 视频元数据 + **状态**。
///
/// # 状态为什么是必要条件
///
/// 状态(`state.bin`)装着"谁审过、哪张图看过、哪一步拍过板" —— 那是这道门的判据,
/// 也是所有工具写回去的地方。它读不动时**必须报错**,不许拿一份空状态顶上:
/// 空状态的意思是"什么都还没核验"(拦),而"读不动"的意思是"**不知道**审过什么"。
/// 两者都拦,但说出来的话完全不同,人要做的事也不同(前者接着干,后者去修文件)。
/// M4 第五轮实测:`load().ok()` 把一份读不动的材料当成了"没有",门数出 0 条、一路放行。
///
/// # 不含写锁
///
/// 锁回答的是"谁有资格改",那是入口层的事(CLI 一次命令一把,MCP 是会话级长持);
/// 混进来会让"只想看一眼"的材料查询也被锁挡住。
#[derive(Debug, Clone)]
pub struct OpenProject {
    layout: Layout,
    project: Project,
    meta: VideoMeta,
    state: ProjectState,
    /// 状态文件**不在**时的一句话(读得动就是 `None`)。
    ///
    /// 为什么"不在"不报错:"不在"推不出"被人拿掉了" —— 新项目、手搓的项目目录都长这样,
    /// 而且按空状态算是**最保守**的那头(什么都没核验 → 门全拦),
    /// 拿不到任何便宜。所以工具照常能用,只把这件事报到明面上,由 `status` / `get_project` 说给人听。
    state_note: Option<String>,
    /// **打开时**读到的 `rev`(状态文件不在/读不动时为 `None`)。
    ///
    /// 写回之前拿它当基准比对 —— 见 [`OpenProject::save`] 的乐观并发。
    /// 没有它的话,`preprocess` 那种"开头读、跑几分钟、最后整份写回"的活
    /// 会把别人这期间的改动静静抹掉。
    opened_rev: Option<u64>,
}

impl OpenProject {
    /// 按 CLI 的写法解析并打开。
    pub fn open(arg: &str) -> Result<Self> {
        Self::from_layout(resolve(arg)?)
    }

    /// 已经拿到 [`Layout`] 时直接打开(不重复解析路径)。
    pub fn from_layout(layout: Layout) -> Result<Self> {
        let project = load_identity(&layout)?;
        let meta = VideoMeta::load(&layout.video_meta()).with_context(|| {
            format!(
                "读取视频元数据失败:{} —— 项目可能没建完(重跑 `vidbrief init` 或换个项目)",
                layout.video_meta().display()
            )
        })?;
        let (state, state_note) = load_state(&layout)?;
        // 打开时的 `rev` —— 写回之前拿它当基准比对(乐观并发,见 `save`)
        let opened_rev = if state_note.is_some() {
            None
        } else {
            Some(state.rev)
        };
        Ok(Self {
            layout,
            project,
            meta,
            state,
            state_note,
            opened_rev,
        })
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }
    pub fn project(&self) -> &Project {
        &self.project
    }
    pub fn meta(&self) -> &VideoMeta {
        &self.meta
    }
    /// 项目状态 —— **门和所有写操作读的都是它**。
    pub fn state(&self) -> &ProjectState {
        &self.state
    }
    pub fn state_mut(&mut self) -> &mut ProjectState {
        &mut self.state
    }
    /// 状态文件不在时的那句话(`None` = 它在,而且读得动)。
    pub fn state_note(&self) -> Option<&str> {
        self.state_note.as_deref()
    }
    /// 项目 id。
    pub fn id(&self) -> &str {
        self.project.id.as_ref()
    }
    pub fn dir(&self) -> &Path {
        self.layout.root()
    }

    /// 项目里那一份源视频的绝对路径。
    ///
    /// 注意 `hardlink` / `symlink` 的项目里,这个路径与原始视频是**同一份数据**,
    /// 不是副本 —— 别往它写东西。
    pub fn video(&self) -> PathBuf {
        self.layout.rel(self.project.source.path.as_str())
    }

    /// 源视频必须还在。不在就带着"怎么排查"一起报错。
    pub fn require_video(&self) -> Result<PathBuf> {
        let video = self.video();
        if !video.is_file() {
            bail!(
                "项目里的源视频不见了:{}\n\
                 (source.link = {:?};软链接的项目一旦原文件被移走就废了)\n\
                 先查一下:`vidbrief status {}`",
                video.display(),
                self.project.source.link,
                self.layout.root().display()
            );
        }
        Ok(video)
    }

    /// 存回**状态**(`state.bin`),并把 `rev` +1。
    ///
    /// 身份(`project.json`)不在这里写:它只在建项目和升级时变一次
    /// (见 [`Self::save_identity`])。
    pub fn save(&mut self) -> Result<()> {
        // **乐观并发**:写回之前确认"我读到的那份"还是当前那份。
        //
        // 为什么非做不可:`preprocess` 那种活会**开头读状态、跑几分钟、最后整份写回**,
        // 这期间别的会话改的东西会被静静抹掉 —— 从前是靠会话级写锁粗盖着,
        // 而锁现在改成按次取放(空转的会话不该把项目占死),那就必须由这里兜住。
        //
        // 基准是"打开时读到的 rev";状态文件不在/读不动时没有基准(`None`),不拦 ——
        // 那种情况下门已经按"什么都还没核验"算了。
        if let Some(seen) = self.opened_rev {
            let now = ProjectState::read_rev(&self.layout.state_file()).with_context(|| {
                format!(
                    "写回前读一次 {} 失败 —— 状态读不动就没法确认这次写安不安全",
                    self.layout.state_file().display()
                )
            })?;
            if now != seen {
                bail!(
                    "项目状态在你读它之后被**别的会话改过**了(你读到 rev={seen},现在是 rev={now})。
                     
                     这次写**没有落盘** —— 落下去会把对方那一段改动抹掉,而它可能是几分钟的活。
                     重来一遍就行:每个写工具都会重新打开项目,读到的就是最新的那份。"
                );
            }
        }
        self.state
            .save(&self.layout.state_file())
            .with_context(|| format!("写入 {} 失败", self.layout.state_file().display()))?;
        // 刚写出去的这份就是最新的 —— 同一次调用里再存一次不该自己撞自己
        self.opened_rev = Some(self.state.rev);
        Ok(())
    }

    /// 存回**身份**(`project.json`)。
    pub fn save_identity(&self) -> Result<()> {
        self.project
            .save(&self.layout.project_json())
            .with_context(|| format!("写入 {} 失败", self.layout.project_json().display()))
    }

    /// 源视频的链接健康检查(不算哈希,很快)。
    pub fn link_health(&self) -> Health {
        check_source_link(&self.project, &self.layout)
    }
}

/// 读 `project.json` —— 版本对不上时把**去路**说清楚。
///
/// 老项目(v1)与新项目要给的提示完全不一样:"跑一次 `vidbrief upgrade`"和
/// "你这一版工具打不开这个项目"是两条不同的路,含糊成"解析失败"就等于什么都没说。
pub fn load_identity(layout: &Layout) -> Result<Project> {
    let path = layout.project_json();
    match Project::load(&path) {
        Ok(p) => Ok(p),
        Err(e) => {
            let version = Project::peek_version(&path).ok();
            match version {
                Some(v) if v < vidbrief_project::SCHEMA_VERSION => bail!(
                    "这个项目是旧格式(v{v}):{}\n\
                     \n\
                     材料都在,只是 `project.json` 的版本比本工具旧。跑一次\n\
                     `vidbrief upgrade {}`\n\
                     就会把它对齐到 v{}:进度、核验戳、看图结论、拍板记录照旧收进状态文件,\
                     材料一个字节都不动。",
                    path.display(),
                    layout.root().display(),
                    vidbrief_project::SCHEMA_VERSION
                ),
                Some(v) => bail!(
                    "这个项目的版本(v{v})比本工具认的(v{})新:{} —— \
                     用**写它的那一版**工具打开它,别在这一版里覆盖。\n\
                     原始报错:{e:#}",
                    vidbrief_project::SCHEMA_VERSION,
                    path.display()
                ),
                None => Err(e).with_context(|| {
                    format!("读取项目文件失败:{} —— 这份文件读不动了", path.display())
                }),
            }
        }
    }
}

/// 旧格式(v1)项目的**身份 + 状态那几栏**。不是 v1 就返回 `None`。
///
/// 给体检(`vidbrief status`)用:它要能在"项目还没迁"的时候照样把项目里有什么
/// 说清楚 —— 连进度和修订号也要,不然旧项目在它嘴里会变成"什么都没做"。
/// 真正的升级走 [`upgrade`]。
pub fn load_legacy(layout: &Layout) -> Option<(Project, vidbrief_project::LegacyStateV1)> {
    let path = layout.project_json();
    let version = Project::peek_version(&path).ok()?;
    if version >= vidbrief_project::SCHEMA_VERSION {
        return None;
    }
    Project::load_v1(&path).ok()
}

/// 读 `state.bin`。三态见 [`OpenProject`] 的说明。
pub fn load_state(layout: &Layout) -> Result<(ProjectState, Option<String>)> {
    let path = layout.state_file();
    match ProjectState::read_file(&path)? {
        Some(state) => Ok((state, None)),
        None => Ok((ProjectState::new(), Some(missing_state_note(layout)))),
    }
}

/// 状态文件**不在**时的那句话。**不许出现"删"**(`AGENTS.md` §2)。
fn missing_state_note(layout: &Layout) -> String {
    format!(
        "这个项目没有状态文件({})。按「什么都还没核验」算 —— \
         材料(转录、截图、候选)都还在,但谁审过、哪张图看过、哪一步拍过板都推不出来了,得重做。\n\
         让人跑一次 `vidbrief upgrade {}` 可以照材料重新起一份状态。",
        layout.state_file().display(),
        layout.root().display()
    )
}

// ── 升级 / 重新起一份状态 ────────────────────────────────────────────

/// `upgrade` 的选项。
#[derive(Debug, Clone, Copy, Default)]
pub struct UpgradeOptions {
    /// 什么都不收,起一份空状态(材料原样保留)。
    pub fresh: bool,
    /// **把现在这份材料当成新底账** —— 连"跟底账对不上"的那些也重记。
    ///
    /// 只有一种时候该用它:人**知道**材料为什么变了、并且认了它。
    /// 不显式说就把对不上的底账悄悄换掉的话,底账就成了摆设 ——
    /// 而它挡的正是"把没审的行从文件里拿掉"这件事。
    pub accept_material: bool,
}

/// `upgrade` 干了什么 —— **要说清楚"收回了什么"**,不然人不知道自己那几轮活还在不在。
#[derive(Debug, Clone, serde::Serialize)]
pub struct UpgradeOutcome {
    pub project_id: String,
    /// 动手之前 `project.json` 是第几版。
    pub from_version: u32,
    /// 升级之后的修订号。
    pub rev: u64,
    /// 收回来的:有看图结论的截图张数。
    pub shot_checks: usize,
    /// 有结论或核验戳的转录段数。
    pub segments: usize,
    /// 有处置结论的补图候选条数。
    pub supplements: usize,
    /// 有没有收回用户拍板的记录。
    pub approval: bool,
    /// 旧格式里那几栏进度是不是搬过来了。
    pub progress_moved: bool,
    /// **内容底账**每份材料一行:记上了 / 重记了 / 没动。
    pub content: Vec<String>,
    /// 什么都没做时的说明(幂等:已经是最新的就什么都不用做)。
    pub note: Option<String>,
}

/// 把一个项目对齐到当前版本:**状态照材料重新起一份**。
///
/// # 什么时候用
///
/// 两种情况,去路是同一条:
///
/// 1. **旧格式(v1)**:进度、核验戳、看图结论、拍板记录当年都写在明文里
///    (`project.json` / `shots.json` / `transcript.fixed.json` / `supplement_requests.json`)。
///    这里把它们收进 `state.bin`,并把 `project.json` 改写成 v2 的身份部分。
///    **材料一个字节都不动。**
/// 2. **状态文件不见了或者读不动**:照明文材料里还读得到的那部分重新起一份。
///
/// # 为什么它可以被随手跑,而且不可怕
///
/// 它的产物永远是**最保守**的那一份:读得到什么就收什么,读不到的就是"还没核验"。
/// 拿它去绕门是绕不过去的 —— "还没核验"只会让门拦得更死。
/// (所以就算这是条命令行、模型自己也跑得动,它也不构成一条捷径。)
///
/// `fresh` = 什么都不收,起一份空的:所有核验记录清零,材料原样保留。
/// 那是留给"状态坏了、也不想照明文将就"的人的。
///
/// **`fresh` 清的只是核验记录**:步 1 的完成标记(`progress.preprocess`)保留 ——
/// 它答的是「这趟算法活儿跑完了没有」,而材料一个字节都没动,答案没变。
/// 清掉它,`status` 就会报「① 前处理 — 未做」而材料齐全 —— 自相矛盾,
/// 而且显示上分不出"没跑过"和"跑过、状态被重起过"([260927-16] 记着那次排查)。
/// 把**三份明文投影**按当前状态重刷一遍(`transcript.fixed.json` / `transcript.read.md` /
/// `transcript.diff.md`),返回刷了多少段(0 = 没有转录材料)。
///
/// 为什么要单独一个函数、还被两条路都调:它们是投影,不进判据,但**口径**这种东西
/// 恰好只落在它们身上(2026-09-28:材料里的时刻从 `m:ss` 改成秒)。那天实测发现
/// "已经是最新的"那条早退路径只看版本号,于是回一句"什么都没动" —— 那句话对状态成立,
/// 对这三份明文不成立:它们还挂着旧写法,而 `upgrade` 的职责就是"把项目对齐到当前版本"。
fn refresh_projections(
    layout: &vidbrief_project::layout::Layout,
    state: &ProjectState,
) -> Result<usize> {
    let path = layout.transcript();
    if !path.is_file() {
        return Ok(0);
    }
    let Ok(raw) = Transcript::load(&path) else {
        return Ok(0);
    };
    let mut merged = raw.clone();
    state.apply_to_transcript(&mut merged);
    crate::materials::save_transcript_projection(layout, &merged)?;
    crate::conclusions::save_transcript_diff(layout, &raw, &merged)?;
    Ok(merged.segments.len())
}

pub fn upgrade(arg: &str, opts: &UpgradeOptions) -> Result<UpgradeOutcome> {
    let layout = resolve(arg)?;
    // 自己取锁:这条路要能在"项目打不开"的时候跑,所以不走 `with_write`
    // (那个先 `OpenProject::open`,而状态读不动时它直接报错)。
    let _guard = vidbrief_project::acquire(&layout)?;

    let from_version = Project::peek_version(&layout.project_json()).unwrap_or(0);

    // 身份:v1 与 v2 两种读法
    let (project, legacy) = if from_version < vidbrief_project::SCHEMA_VERSION {
        let (project, legacy) = Project::load_v1(&layout.project_json())?;
        (project, Some(legacy))
    } else {
        (load_identity(&layout)?, None)
    };

    // 已经是最新的、而且状态读得动 → **只对底账**,不动别的
    //
    // (状态在、材料也在,那就没有"收回来"这回事;剩下唯一可能要做的,
    //  是这份材料还没有底账 —— 那种情况记上就行。)
    if legacy.is_none() && !opts.fresh {
        if let Ok(Some(mut state)) = ProjectState::read_file(&layout.state_file()) {
            let content = record_ledgers(&layout, &mut state, opts.accept_material)?;
            let changed = content.iter().any(|line| !line.contains("没动"));
            if changed {
                state.save(&layout.state_file())?;
            }
            // **投影也要跟着当前版本走**:状态是最新的,不代表印出来的那些明文是最新的
            // (2026-09-28 实测:材料里的时刻从 `m:ss` 改成秒之后,老项目那三份还挂着旧写法,
            //  而这条路只看版本号,会回一句"什么都没动" —— 那句话对状态成立,对投影不成立)。
            let refreshed = refresh_projections(&layout, &state)?;
            let mut note = if changed {
                "已经是最新的:只把内容底账对齐了一遍,核验记录一条都没动。".to_string()
            } else {
                "已经是最新的:状态文件在,而且读得动,核验记录一条都没动。".to_string()
            };
            if refreshed > 0 {
                note.push_str(&format!(
                    "三份明文投影按当前口径重刷了一遍({refreshed} 段;\
                     它们是投影,手改没用,内容只由状态决定)。"
                ));
            } else {
                note.push_str("(没有转录材料,投影没得刷)");
            }
            return Ok(UpgradeOutcome {
                project_id: project.id.to_string(),
                from_version,
                rev: state.rev,
                shot_checks: state.shots.values().filter(|s| s.check.is_some()).count(),
                segments: state.segments.len(),
                supplements: state.supplements.len(),
                approval: state.review.calibrate.is_some(),
                progress_moved: false,
                content,
                note: Some(note),
            });
        }
    }

    // `--fresh` 走这条路重新起一份状态。**先把旧那份读下来**(读得动的话)——
    // 它里面有两样东西不该跟着清(见下面那段)。
    let previous = if opts.fresh {
        ProjectState::read_file(&layout.state_file()).ok().flatten()
    } else {
        None
    };

    let mut state = ProjectState::new();
    if let Some(legacy) = &legacy {
        state.rev = legacy.rev;
        state.progress = legacy.progress.clone();
        state.review = legacy.review.clone();
    }

    let mut note = None;
    if opts.fresh {
        if let Some(old) = &previous {
            // **步 1 的完成标记不是核验记录**,它记的是「这趟算法活儿跑完了没有」。
            // 而 `--fresh` 明确保住材料(还要照它们重记底账)—— 那条事实一个字没变。
            // 清掉它只会让 `status` 报「① 前处理 — 未做」而材料齐全,**自相矛盾**;
            // 2026-09-27 实测就是被它带进沟里的:查了半天"标记怎么丢了",
            // 结论是它按上面这条设计被清了,而显示上分不出"没跑过"和"跑过、状态被重起过"。
            state.progress.preprocess = old.progress.preprocess.clone();
            // `rev` 一路往上走,**不倒退** —— 倒退的那个号会让人以为文件是旧的
            state.rev = old.rev;
        }
        note = Some(
            "`--fresh`:什么都没收 —— 已有的核验记录清零,材料原样保留。\
             (步 1 的完成标记保留:材料没动,那条事实没变)"
                .to_string(),
        );
    } else {
        // 明文里那几份材料:读得动就收,读不动就**报错**(不许当没有 ——
        // 一次静默的"没有结论"会让升级这件事本身变成一把抹掉记录的刀)
        if let Some(shots) = load_legacy_shots(&layout)? {
            state.absorb_shots(&shots);
        }
        if let Some(fixed) = load_legacy_transcript(&layout)? {
            state.absorb_transcript(&fixed);
        }
        if let Some(list) = load_legacy_supplements(&layout)? {
            state.absorb_supplements(&list);
        }
    }
    state.prune();

    // 内容底账:重起一份状态时**总是**记上(它记的是"工具写过的材料长什么样",
    // 而这条路本身就是"照材料起一份";`--fresh` 也不例外 —— 不然新状态一出生就带着
    // 一堆"没有底账"的假警报)。
    let content = record_ledgers(&layout, &mut state, true)?;

    // 落盘:状态 + 身份(v2 的身份里没有状态那几栏,所以这一步同时也是 v1 → v2)
    state.save(&layout.state_file())?;
    project.save(&layout.project_json())?;

    // **投影跟着状态重写** —— `transcript.fixed.json` 与 `transcript.read.md` 都是
    // "状态长什么样"的投影,状态都重起了,它们还挂着上一版就是**两处各说各话**:
    // `--fresh` 清完结论之后,agent 打开那份人读投影会看到"改了 19 段、存疑 4 段",
    // 而那些结论一条都不在了(2026-09-27 实测:清完场差点照着旧投影往下做)。
    // 材料没动 —— 动的只是"结论那几栏",而它们本来就是投影。
    refresh_projections(&layout, &state)?;
    Ok(UpgradeOutcome {
        project_id: project.id.to_string(),
        from_version,
        rev: state.rev,
        shot_checks: state.shots.values().filter(|s| s.check.is_some()).count(),
        segments: state.segments.len(),
        supplements: state.supplements.len(),
        approval: state.review.calibrate.is_some(),
        progress_moved: legacy.is_some(),
        content,
        note,
    })
}

/// 照**当前**这几份材料把内容底账记一遍。
///
/// 规则只有两条,都是为了"底账不能顺手就被换掉":
///
/// - **缺的记上**:没底账的材料,记上它 —— 这是"把工具写出去的这份认下来",不构成绕过;
/// - **对不上的不换**:除非 `overwrite`(`--accept-material`)。
///   `overwrite` 还负责把**不在了**的那几份的底账抹掉(显式认了"它没就没了吧")。
fn record_ledgers(
    layout: &Layout,
    state: &mut ProjectState,
    overwrite: bool,
) -> Result<Vec<String>> {
    use vidbrief_project::layout as paths;
    let mut lines = Vec::new();

    let shots = load_legacy_shots(layout)?;
    let transcript = load_raw_transcript(layout)?;
    let supplements = load_legacy_supplements(layout)?;

    let rows: [(&str, Option<vidbrief_project::ContentPrint>); 3] = [
        (
            paths::TRANSCRIPT_FILE,
            transcript.as_ref().map(vidbrief_project::transcript_print),
        ),
        (
            paths::SHOTS_FILE,
            shots.as_ref().map(vidbrief_project::shots_print),
        ),
        (
            paths::SUPPLEMENT_REQUESTS_FILE,
            supplements
                .as_ref()
                .map(vidbrief_project::supplements_print),
        ),
    ];

    for (rel, now) in rows {
        match (&now, state.content.get(rel).cloned()) {
            // 材料不在:有底账的话,只有显式认了才抹掉
            (None, Some(then)) => {
                if overwrite {
                    state.content.remove(rel);
                    lines.push(format!(
                        "{rel}:材料不在,底账也跟着抹了(原来记着 {} 条)",
                        then.items
                    ));
                } else {
                    lines.push(format!("{rel}:底账没动(原来记着 {} 条,材料现在不在)", then.items));
                }
            }
            (None, None) => {}
            (Some(print), None) => {
                state.note_content(rel, print.clone());
                lines.push(format!("{rel}:记上底账({} 条)", print.items));
            }
            (Some(print), Some(then)) => {
                if print.digest == then.digest {
                    lines.push(format!("{rel}:底账没动({} 条,对得上)", then.items));
                } else if overwrite {
                    lines.push(format!(
                        "{rel}:**重记了底账**(原来 {} 条,现在 {} 条)",
                        then.items, print.items
                    ));
                    state.note_content(rel, print.clone());
                } else {
                    lines.push(format!(
                        "{rel}:底账没动 —— 它跟现在这份对不上(底账 {} 条,现在 {} 条);要认现在这份,加 `--accept-material`",
                        then.items, print.items
                    ));
                }
            }
        }
    }
    Ok(lines)
}

/// 读**原始**转录(不是纠错稿)。不在 → `None`,读不动 → 报错。
///
/// 底账记的是**原文**:纠错稿是投影(结论在状态里),原文才是"材料"。
fn load_raw_transcript(layout: &Layout) -> Result<Option<vidbrief_project::Transcript>> {
    let path = layout.transcript();
    if !path.is_file() {
        return Ok(None);
    }
    let t = vidbrief_project::Transcript::load(&path)
        .with_context(|| format!("读取 {} 失败 —— 转录取不出来(被改过或坏了)", path.display()))?;
    Ok(Some(t))
}

/// 明文里能读到的截图清单。不在 → `None`,读不动 → 报错。
fn load_legacy_shots(layout: &Layout) -> Result<Option<vidbrief_project::Shots>> {
    let path = layout.shots_json();
    if !path.is_file() {
        return Ok(None);
    }
    let shots = vidbrief_project::Shots::load(&path).with_context(|| {
        format!(
            "读取 {} 失败 —— 这份截图清单读不动,没法从它里面收结论。\
             先修好它(或者跑一次 `preprocess` 重算),再来升级。",
            path.display()
        )
    })?;
    Ok(Some(shots))
}

/// 明文里能读到的转录结论(`transcript.fixed.json`)。
fn load_legacy_transcript(layout: &Layout) -> Result<Option<vidbrief_project::Transcript>> {
    let path = layout.transcript_fixed();
    if !path.is_file() {
        return Ok(None);
    }
    let fixed = vidbrief_project::Transcript::load(&path)
        .with_context(|| format!("读取 {} 失败 —— 这份纠错稿读不动,没法从它里面收结论。", path.display()))?;
    Ok(Some(fixed))
}

/// 明文里能读到的补图候选处置。
fn load_legacy_supplements(layout: &Layout) -> Result<Option<vidbrief_project::SupplementRequests>> {
    let path = layout.supplement_requests();
    if !path.is_file() {
        return Ok(None);
    }
    let list = vidbrief_project::SupplementRequests::load(&path).with_context(|| {
        format!(
            "读取 {} 失败 —— 这份补图候选清单读不动,没法从它里面收处置。",
            path.display()
        )
    })?;
    Ok(Some(list))
}

// ── 建项目 ───────────────────────────────────────────────────────────

/// `create` 的入参。对应 CLI `init` 的三个选项。
#[derive(Debug, Clone, Default)]
pub struct CreateOptions {
    /// 源视频路径(命令行里那一份,还没 canonicalize)。
    pub video: PathBuf,
    /// 项目 id。`None` = 从 `-p` 目录名或视频文件名净化。
    pub name: Option<String>,
    /// 人类可读名。`None` = 取视频文件名。
    pub title: Option<String>,
    /// 项目目录本身。`None` = 默认位置 `projects/<id>`(冲突时自动加后缀)。
    pub project_dir: Option<PathBuf>,
}

/// 动手**之前**要说的话。
///
/// 存在的理由很实在:跨卷时那几百 MB 的复制要好几秒,不该让人干等到结束才知道在等什么
/// (`docs/DESIGN.md` §4.8)。但**说什么由调用方决定** —— 所以这里只给事实,不给文案。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// 素材与项目不在同一个卷:硬链接用不了,即将复制。
    CrossVolumeCopy {
        size_bytes: u64,
        /// "把项目建到素材同卷"的具体命令;推不出来时为 `None`。
        hint: Option<String>,
    },
}

/// 建好的项目。
#[derive(Debug, Clone)]
pub struct Created {
    pub id: ProjectId,
    pub layout: Layout,
    pub project: Project,
    pub meta: VideoMeta,
    /// 源视频的原始路径(已剥掉 Windows 的 `\\?\` 前缀,可以直接给人看)。
    pub original: PathBuf,
    /// 退让记录:硬链接为什么没成、软链接为什么没成。空 = 一次到位。
    ///
    /// **必须能报出来**:用户看到"复制了一份"却不知道为什么,只会以为工具坏了。
    pub fallbacks: Vec<String>,
}

/// 建项目:校验 → 定 id 与目录 → 搬源视频 → 探测 → 写 `project.json`。
///
/// 只建目录与元数据,**不跑任何算法**(步 1 是 [`crate::preprocess`])。
pub fn create(opts: &CreateOptions, on_notice: &mut dyn FnMut(Notice)) -> Result<Created> {
    let video = opts
        .video
        .canonicalize()
        .with_context(|| format!("找不到视频文件:{}", opts.video.display()))?;
    if !video.is_file() {
        bail!("不是文件:{}", video.display());
    }

    let stem = video
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();

    // **占位就在 `choose_placement` 里做掉了** —— 它返回时目录已经归我们。
    // 分成两步("先挑一个空位" + "再占住它")必然有窗口,而那个窗口正是要消灭的东西。
    let (id, layout) = choose_placement(opts, &stem)?;

    // 目录已经归我们了,再补齐子目录(幂等)
    layout.create_dirs()?;

    // 从这里往后都是"往这个目录里填东西"。**中途失败要把占位还回去** ——
    // 否则用户重试 `init` 时会撞上自己上一次留下的半成品("目录已存在,拒绝覆盖"),
    // 而且看不出那是个残骸。目录是我们原子占下来的,删它不会误伤别人。
    let outcome = fill_project(&layout, &video, &stem, opts, id, on_notice);
    if outcome.is_err() {
        let _ = std::fs::remove_dir_all(layout.root());
    }
    outcome
}

/// 往已经占下来的目录里填:搬源视频 → 探测 → 写 `project.json`。
///
/// 单独一个函数,是为了让 [`create`] 能在它失败时把目录整个退掉 ——
/// Rust 没有 try/finally,把"要善后的那段"圈成一个函数是最直白的写法。
fn fill_project(
    layout: &Layout,
    video: &std::path::Path,
    stem: &str,
    opts: &CreateOptions,
    id: ProjectId,
    on_notice: &mut dyn FnMut(Notice),
) -> Result<Created> {
    let rel = video_rel_for(video);
    let dst = layout.rel(rel.as_str());

    // 动手**之前**先预判(见 `Notice` 的说明)
    if let Ok(placement) = plan(video, &dst) {
        if placement.will_copy_across_volumes() {
            on_notice(Notice::CrossVolumeCopy {
                size_bytes: placement.size_bytes,
                hint: same_volume_hint(video, id.as_ref()),
            });
        }
    }

    let placed = materialize(video, &dst)?;

    // 探测的是**项目里**那一份 —— 软链接的情况下它会跟随到原文件,这正是我们想测的
    let meta = vidbrief_core::probe(&dst, rel.clone())?;
    meta.save(&layout.video_meta())?;

    let title = opts.title.clone().unwrap_or_else(|| stem.to_string());
    // `canonicalize` 在 Windows 上带 `\\?\` 前缀;写进 project.json 之前去掉,
    // 否则这个字段对人就没法看了
    let original = simplify_path(video);
    let project = Project::new(
        id.clone(),
        title,
        rel,
        original.display().to_string(),
        placed.link,
    );
    project.save(&layout.project_json())?;
    // 状态文件在**建项目时**就立起来(空的那一份)。
    //
    // 为什么不让它"以后写第一次的时候再建":那样"状态文件不在"就同时表示
    // "这个项目还没跑过"和"那份记录被人拿掉了",两者要人做的事完全相反。
    // 立起来之后,"不在"只剩后者一种意思,`status` 才敢把它当异常报。
    let mut state = ProjectState::new();
    state.save(&layout.state_file())?;

    Ok(Created {
        id,
        layout: layout.clone(),
        project,
        meta,
        original,
        fallbacks: placed.fallbacks,
    })
}

/// 定项目 id 与目录。**优先级是契约的一部分**(`docs/DESIGN.md` §10.5):
/// `--name` → `-p` 的最后一段 → 视频文件名净化。
fn choose_placement(opts: &CreateOptions, stem: &str) -> Result<(ProjectId, Layout)> {
    let base = match &opts.name {
        Some(raw) => ProjectId::from_video_stem(raw)
            .with_context(|| format!("--name `{raw}` 净化后为空,请用一个 ASCII 名字"))?,
        None => {
            let from_dir = opts.project_dir.as_deref().and_then(id_from_dir);
            from_dir
                .or_else(|| ProjectId::from_video_stem(stem))
                .with_context(|| {
                    let source = match &opts.project_dir {
                        Some(dir) => format!("文件名 `{stem}` 或目录名 `{}`", dir.display()),
                        None => format!("文件名 `{stem}`"),
                    };
                    format!("无法从{source}推导项目 id(可能是中文名),请用 --name 指定一个 ASCII 名字")
                })?
        }
    };

    let id = ProjectId::new(base)?;
    // 显式给了 `-p` 就用它本身,不再套一层;没给就落在默认位置 `projects/<id>`。
    // **两种情况的冲突处理完全一样:报错,把选择权交回去。**
    let layout = match &opts.project_dir {
        Some(dir) => Layout::new(dir),
        None => Layout::for_project(Path::new(vidbrief_project::layout::PROJECTS_DIR), id.as_ref()),
    };
    claim_dir(&layout)?;
    Ok((id, layout))
}

/// **原子地占住这个目录**,占不上就报错 —— 名字撞了绝不自己换一个。
///
/// 这里是全仓唯一一个"没有锁可用、也不需要锁"的互斥点:项目目录还不存在,
/// 没有 `project.lock` 可以锁。而 `create_dir` 本身就带互斥语义 ——
/// POSIX 与 Windows 都保证"已存在就失败"。有原子原语,就别自己做检查。
///
/// 父目录(默认位置下的 `projects/`)先用 `create_dir_all` 铺好:那是多个项目共用的容器,
/// 谁铺都一样,**不需要互斥**;要独占的只有 `projects/<id>` 这一层。
///
/// # 为什么不自动加后缀
///
/// 这里曾经会在默认位置冲突时自己挑一个 `x-2` / `x-3`。去掉了,理由有三条:
///
/// 1. **那是命名,命名是人的决定。** `x-2` 是用户会看到、会用它调工具、会写进报告的标识 ——
///    工具悄悄替他定了一个,而这个决定本该由他(或 agent)来做。
/// 2. **它对"用户显式起了名"的情况尤其错。** `-n demo` 明确说了叫 demo,结果旁边冒出个
///    `demo-2` —— 而显式给 `-p` 撞了车我们却是报错的。同一条规则不能一半这样一半那样。
/// 3. **撞名往往不是在说"换一个",而是在说"别建"。** 多半是:这个视频已经建过了、
///    上次建到一半失败了、或者根本是点错了。这几种的正确回应都是**看一眼再决定**,
///    不是再造一个出来。
///
/// 所以报错要把**去路**说清楚,让 agent/用户一次就能选对(见下面的分支)。
fn claim_dir(layout: &Layout) -> Result<()> {
    if let Some(parent) = layout.root().parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建父目录失败:{}", parent.display()))?;
        }
    }

    match std::fs::create_dir(layout.root()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let root = layout.root();
            // 已存在的那个位置**是不是一个能用的项目**?这决定了两条完全不同的去路 ——
            // 「换个名字」和「先去看看那是什么」,含糊成一句话会让人选错。
            if layout.exists() {
                // 用**项目自己的 id**(不是目录名)告诉人怎么打开它 ——
                // 两者可以不一样:`-p /某个怪目录 -n demo` 的 id 是 demo。
                // 拿目录名当 id 在那种情况下会指错。
                let how = vidbrief_project::Project::load(&layout.project_json())
                    .map(|p| p.id.to_string())
                    .unwrap_or_else(|_| {
                        root.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default()
                    });
                bail!(
                    "这个位置已经有一个项目了:{}\n\
                     用 `{how}` 就能打开它,**不需要重新建**。\n\
                     确实想另外建一个,就换个 id(`-n` / `name`)或换个位置(`-p` / `project_dir`)。",
                    root.display()
                )
            } else {
                bail!(
                    "这个位置被一个**还没建完的目录**占着:{}\n\
                     里面没有 {} —— 有两种可能,从外面看不出是哪一种:\n\
                     · 另一个会话**正在**建这个项目 → 等一会儿再试;\n\
                     · 上一次建到一半**失败**留下的 → 换个 id(`-n` / `name`)\
                     或换个位置(`-p` / `project_dir`)另起一个。\n\
                     那个目录**不要动** —— 它可能是别人的,也可能还有救。",
                    root.display(),
                    vidbrief_project::layout::PROJECT_FILE
                )
            }
        }
        Err(e) => Err(e).with_context(|| format!("创建项目目录失败:{}", layout.root().display())),
    }
}

/// 从目录路径的最后一段推导 id(`/tmp/vb/s4` → `s4`)。
///
/// 路径以 `.` / `..` 结尾,或末段净化不出 ASCII 时返回 `None`(交给下一优先级)。
fn id_from_dir(dir: &Path) -> Option<String> {
    ProjectId::from_video_stem(dir.file_name()?.to_str()?)
}

/// 给出"把项目建到素材同卷"的**具体命令**,而不是只说一句"建议同卷"。
fn same_volume_hint(video: &Path, id: &str) -> Option<String> {
    let dir = simplify_path(video).parent()?.join("vidbrief").join(id);
    Some(format!("vidbrief init <同一个视频> -p {}", dir.display()))
}

// ── 列项目 ───────────────────────────────────────────────────────────

/// `list_projects` 的一行。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProjectSummary {
    pub id: String,
    pub name: String,
    pub dir: PathBuf,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    /// 源视频的来处(纯信息性,可能已经失效)。
    pub source_original: String,
    pub link: SourceLink,
    /// 修订号与四步进度 —— **都来自状态文件**。
    pub rev: u64,
    pub progress: vidbrief_project::Progress,
    /// 状态文件读不动 / 不在时的说明(正常是 `null`)。
    pub state_note: Option<String>,
    /// 源视频还在不在(轻量检查,不重算哈希)。
    pub health: crate::inspect::HealthView,
    /// 材料做到哪一步了(只看文件在不在,不解析内容)。
    pub materials: crate::inspect::Materials,
    /// **步 1 那趟作业现在什么状态**(`RunRecord` 的摘要)。
    ///
    /// 为什么要它在列表里:项目一多,"现在谁在跑、谁断了、谁上次是失败"得**一屏看清** ——
    /// 从前只有**单项目**的入口(`preprocess_progress` 一个项目问一次),
    /// agent 手里排五个项目就要挨个问五遍,还得自己记。这里是**只读信息**,
    /// 排哪个先跑仍然是它自己的判断(见 [260927-30])。
    pub run: Option<RunBrief>,
}

/// 那趟作业的一句话状态(见 [`ProjectSummary::run`])。
#[derive(Debug, Clone, serde::Serialize)]
pub struct RunBrief {
    pub run_id: String,
    /// `running` / `done` / `failed` / `interrupted`。
    pub state: &'static str,
    /// 记录里那句人话(`跑完了,用时 1333.5 秒` / `转录失败:…`)。
    pub message: String,
    /// 这一趟从起步到现在(秒)。
    pub elapsed_seconds: f64,
    /// 记录多久没动了(秒)—— 判"断了"看它(`run::STALE_AFTER_SECONDS`)。
    pub quiet_seconds: i64,
}

/// 扫一遍 `projects/` 下的项目。
///
/// **坏项目不跳过**:读不出来的目录会作为一条 `health = problem` 的记录返回 ——
/// "以为自己有 5 个项目,其实第 3 个早就打不开了"比"列表里少一行"危险得多。
pub fn list_projects(root: &Path) -> Result<Vec<ProjectSummary>> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        // 一个项目都没有是**正常**状态,不是错误
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => {
            return Err(e).with_context(|| format!("列目录失败:{}", root.display()));
        }
    };

    for entry in entries {
        let entry = entry.with_context(|| format!("列目录失败:{}", root.display()))?;
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let layout = Layout::new(&dir);
        if !layout.exists() {
            continue; // 不是项目目录,当没看见
        }
        out.push(summarize(&layout)?);
    }

    // 按建立时间排:最近的在前(用户多半想找刚建的那个)
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(out)
}

/// 单个项目的摘要。读不出来时也返回一条记录,只是 `health` 是 problem。
///
/// 身份读不出来(版本对不上、文件坏了)时**照样返回一条**,而且把原因原样带上 ——
/// 列表里少一行比多一行坏消息危险得多。
fn summarize(layout: &Layout) -> Result<ProjectSummary> {
    let materials = crate::inspect::scan_materials(layout);
    let dir = layout.root().to_path_buf();
    // 状态读不动时,进度与修订号照 `Default` 走,并把原因放进 `state_note` 说给人听
    let (state, state_note) = match load_state(layout) {
        Ok((state, note)) => (state, note),
        Err(e) => (ProjectState::new(), Some(format!("{e:#}"))),
    };

    let project = match load_identity(layout) {
        Ok(p) => p,
        Err(e) => {
            return Ok(ProjectSummary {
                id: dir
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("?")
                    .to_string(),
                name: String::new(),
                dir,
                created_at: vidbrief_project::now(),
                source_original: String::new(),
                link: SourceLink::Copy,
                rev: state.rev,
                progress: state.progress.clone(),
                state_note,
                health: crate::inspect::HealthView::problem(format!("{e:#}")),
                materials,
                run: run_brief(layout),
            });
        }
    };

    let health = crate::inspect::HealthView::from(check_source_link(&project, layout));

    Ok(ProjectSummary {
        id: project.id.as_ref().to_string(),
        name: project.name.clone(),
        dir,
        created_at: project.created_at,
        source_original: project.source.original.clone(),
        link: project.source.link,
        rev: state.rev,
        progress: state.progress.clone(),
        state_note,
        health,
        materials,
        run: run_brief(layout),
    })
}

/// 那趟作业的一句话状态;没有记录(从没跑过 / 记录被清掉)就是 `None`。
///
/// **读不动不当错误**:列表是给人扫一眼的,一行读不出来不该让整份清单失败 ——
/// 那种情况返回 `None`,与"没跑过"同形(要看细的用 `preprocess_progress`,它会说出来)。
fn run_brief(layout: &Layout) -> Option<RunBrief> {
    let record = crate::run::read(layout).ok().flatten()?;
    Some(RunBrief {
        run_id: record.run_id.clone(),
        state: match record.outcome() {
            crate::run::RunOutcome::Running => "running",
            crate::run::RunOutcome::Done => "done",
            crate::run::RunOutcome::Failed => "failed",
            crate::run::RunOutcome::Interrupted => "interrupted",
        },
        message: record.summarize(),
        elapsed_seconds: record.elapsed_seconds,
        quiet_seconds: record.quiet_seconds(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_rejects_a_directory_without_project_json() {
        let dir = std::env::temp_dir().join(format!("vidbrief-ops-resolve-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let err = resolve(&dir.display().to_string()).unwrap_err().to_string();
        assert!(err.contains("找不到项目"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn id_from_dir_handles_dots_and_chinese() {
        assert_eq!(id_from_dir(Path::new("/tmp/vb/s4")).as_deref(), Some("s4"));
        // Rust 的 Path 会把末尾的 `.` 规范化掉,所以这里拿到的是最后一段真名字
        assert_eq!(id_from_dir(Path::new("/tmp/vb/.")).as_deref(), Some("vb"));
        assert_eq!(id_from_dir(Path::new("/tmp/vb/..")), None);
        assert_eq!(id_from_dir(Path::new("/tmp/vb/第一讲")), None);
    }

    #[test]
    fn list_projects_on_a_missing_root_is_empty_not_an_error() {
        let root = std::env::temp_dir().join(format!("vidbrief-ops-nope-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        assert!(list_projects(&root).unwrap().is_empty());
    }

    // ── 升级:旧格式 → 状态文件 ──────────────────────────────────────

    /// 把假项目改造成**旧格式(v1)**:`project.json` 带上那三栏、`state.bin` 拿掉。
    fn make_v1(fake: &crate::testkit::Fake, rev: u64) {
        let text = format!(
            r#"{{
  "schema_version": 1,
  "rev": {rev},
  "id": "demo",
  "name": "测试项目",
  "created_at": "2026-09-24T21:00:00+08:00",
  "source": {{"path": "source/video.mp4", "original": "E:/fake/video.mp4", "link": "copy"}},
  "glossary": null,
  "progress": {{
    "preprocess": {{"done": true, "at": "2026-09-24T21:03:11+08:00"}},
    "calibrate": {{"rounds": 2, "last_at": "2026-09-24T21:20:00+08:00"}},
    "summarize": {{"done": false, "at": null}},
    "report": {{"done": false, "at": null}}
  }},
  "review": {{"calibrate": null}}
}}
"#
        );
        std::fs::write(fake.layout().project_json(), text).unwrap();
        std::fs::remove_file(fake.layout().state_file()).unwrap();
    }

    fn a_check() -> vidbrief_project::ShotCheck {
        vidbrief_project::ShotCheck {
            r#match: true,
            audio_visual_sync: true,
            type_read: "Python 列表 结构图".into(),
            neighbor_text: None,
            issues: vec![],
            confidence: vidbrief_project::Confidence::High,
            evidence: Some(vidbrief_project::Evidence {
                segment: 1,
                quote: "各位好".into(),
            }),
            checked_at: vidbrief_project::now(),
        }
    }

    /// **旧项目迁一次:状态从明文收进 state.bin —— 材料一个字节都不动。**
    ///
    /// 这条和 `vidbrief-project` 里那条"老格式读得进来"是同一件事的两半:
    /// 那边保证**读得到**,这边保证**收得回、而且不改材料**。
    #[test]
    fn upgrade_collects_the_state_that_lived_in_plaintext() {
        let fake = crate::testkit::Fake::new("upgrade-v1")
            .with_transcript(&[(0.0, 2.0, "派森列表"), (2.0, 4.0, "各位好")])
            .with_shot(1, 2.0);
        make_v1(&fake, 7);

        // 明文里放一份"当年写的"看图结论与转录结论
        let mut shots = vidbrief_project::Shots::load(&fake.layout().shots_json()).unwrap();
        shots.shots[0].check = Some(a_check());
        shots.save(&fake.layout().shots_json()).unwrap();
        let mut fixed = vidbrief_project::Transcript::load(&fake.layout().transcript()).unwrap();
        fixed.segments[0].conclusion = Some(vidbrief_project::Conclusion::Corrected {
            text: "Python 列表".into(),
            kind: vidbrief_project::CorrectionType::Terminology,
            note: "派森 → Python".into(),
            reason: "画面标题".into(),
        });
        fixed.segments[0].checked_at = Some(vidbrief_project::now());
        fixed.save(&fake.layout().transcript_fixed()).unwrap();

        let shots_before = std::fs::read(fake.layout().shots_json()).unwrap();
        let fixed_before = std::fs::read(fake.layout().transcript_fixed()).unwrap();
        let transcript_before = std::fs::read(fake.layout().transcript()).unwrap();

        let out = upgrade(
            &fake.layout().root().display().to_string(),
            &UpgradeOptions::default(),
        )
        .unwrap();
        assert_eq!(out.from_version, 1);
        assert_eq!(out.shot_checks, 1, "那张图的结论该收回来");
        assert_eq!(out.segments, 1, "那一句的结论该收回来");
        assert!(out.progress_moved);
        assert_eq!(out.rev, 8, "旧修订号 7 之上再加一次写");
        assert!(out.note.is_none());

        // 材料**一个字节都没动**
        assert_eq!(std::fs::read(fake.layout().shots_json()).unwrap(), shots_before);
        assert_eq!(
            std::fs::read(fake.layout().transcript_fixed()).unwrap(),
            fixed_before
        );
        assert_eq!(
            std::fs::read(fake.layout().transcript()).unwrap(),
            transcript_before
        );

        // 身份回来了,状态搬进去了
        let project = Project::load(&fake.layout().project_json()).unwrap();
        assert_eq!(project.id.as_ref(), "demo");
        let state = vidbrief_project::ProjectState::read_file(&fake.layout().state_file())
            .unwrap()
            .expect("升级之后状态文件该在");
        assert_eq!(state.rev, 8);
        assert!(state.progress.preprocess.done, "进度也搬过来了");
        assert_eq!(state.progress.calibrate.rounds, 2);
        assert!(
            state
                .shot(vidbrief_project::ShotId::FIRST)
                .unwrap()
                .check
                .is_some()
        );
        assert_eq!(state.segments.len(), 1);
        match state.segment(1).unwrap().conclusion() {
            Some(vidbrief_project::Conclusion::Corrected { text, .. }) => {
                assert_eq!(text, "Python 列表")
            }
            other => panic!("该收回「改了」这条结论:{other:?}"),
        }

        // 升级之后项目能正常打开,而且门读的是刚收回来的状态
        let opened = OpenProject::from_layout(fake.layout().clone()).unwrap();
        assert!(opened.state_note().is_none());
        let s = crate::review::state(&opened).unwrap();
        assert!(s.unchecked.shots.is_empty(), "{s:?}");
        assert_eq!(s.unchecked.segments.len(), 1, "第 2 段还没核验:{s:?}");

        // 幂等:状态与核验记录一条不动(修订号也不再推进)——
        // 但**明文投影照刷**:它们是"状态长什么样"的投影,口径变了就得跟上。
        let again = upgrade(
            &fake.layout().root().display().to_string(),
            &UpgradeOptions::default(),
        )
        .unwrap();
        assert!(again.note.is_some(), "第二次该什么都不做:{again:?}");
        assert_eq!(again.rev, 8, "什么都不做就不该再推进修订号");
        assert!(
            again.note.as_deref().unwrap_or("").contains("投影"),
            "状态没动,但投影要刷:{:?}",
            again.note
        );
    }

    /// **底账默认不换**:材料被人手改过时,`upgrade` 只把话说清楚,不替人认下来。
    ///
    /// 这一条是"底账"这套东西的命门:要是随手跑一次 `upgrade` 就能把对不上的底账换掉,
    /// 那它挡不住任何人 —— 而它挡的正是"把没审的行从文件里拿掉"这件事。
    /// 真要认,得显式说 `--accept-material`。
    #[test]
    fn upgrade_keeps_a_mismatched_ledger_unless_asked_to_accept_it() {
        let fake = crate::testkit::Fake::new("upgrade-ledger")
            .with_transcript(&[(0.0, 2.0, "各位好"), (2.0, 4.0, "接着讲")])
            .with_shot(1, 2.0);

        // 手改正文:底账立刻对不上
        let path = fake.layout().transcript();
        let mut t = vidbrief_project::Transcript::load(&path).unwrap();
        t.segments[0].text = "手改过的正文".into();
        t.save(&path).unwrap();

        let opened = OpenProject::from_layout(fake.layout().clone()).unwrap();
        assert_eq!(crate::materials::content_mismatches(&opened).unwrap().len(), 1);

        // ① 不加 flag:底账**不换**,而且报出"要认就加 --accept-material"
        let out = upgrade(
            &fake.layout().root().display().to_string(),
            &UpgradeOptions::default(),
        )
        .unwrap();
        assert!(
            out.content.iter().any(|l| l.contains("--accept-material")),
            "该说清怎么才能认下它:{:?}",
            out.content
        );
        let opened = OpenProject::from_layout(fake.layout().clone()).unwrap();
        assert_eq!(
            crate::materials::content_mismatches(&opened).unwrap().len(),
            1,
            "没让改的底账不许被换掉"
        );

        // ② 显式认了:重记底账,并对不上变对得上
        let out = upgrade(
            &fake.layout().root().display().to_string(),
            &UpgradeOptions {
                fresh: false,
                accept_material: true,
            },
        )
        .unwrap();
        assert!(
            out.content.iter().any(|l| l.contains("重记了底账")),
            "重记了要说出来:{:?}",
            out.content
        );
        let opened = OpenProject::from_layout(fake.layout().clone()).unwrap();
        assert!(crate::materials::content_mismatches(&opened).unwrap().is_empty());
        // 核验记录**一条都没动**:这条路只管底账
        let s = crate::review::state(&opened).unwrap();
        assert_eq!(s.unchecked.shots.len(), 1, "那张图还是没看过:{s:?}");
    }

    /// 旧格式迁过来时,底账**一次记齐** —— 不然新状态一出生就带着一堆假警报。
    #[test]
    fn upgrade_records_ledgers_for_a_v1_project() {
        let fake = crate::testkit::Fake::new("upgrade-ledger-v1")
            .with_transcript(&[(0.0, 2.0, "各位好")])
            .with_shot(1, 2.0);
        make_v1(&fake, 3);
        upgrade(
            &fake.layout().root().display().to_string(),
            &UpgradeOptions::default(),
        )
        .unwrap();

        let opened = OpenProject::from_layout(fake.layout().clone()).unwrap();
        assert!(
            crate::materials::content_mismatches(&opened).unwrap().is_empty(),
            "迁移之后底账该是齐的"
        );
        assert_eq!(opened.state().content.len(), 2, "转录 + 截图,两份都在底账里");
    }

    /// **列表里带出步 1 那趟作业的状态** —— 一屏看清"谁在跑、谁断了",不用逐个去问。
    ///
    /// 排哪个先跑仍是 **agent 的判断**,这条只是把话说清(见 [260927-30])。
    #[test]
    fn the_list_carries_the_step_one_run_state() {
        use crate::run::{RunRecorder, RunStage};

        let fake = crate::testkit::Fake::new("list-run-state").with_curve(3);
        let layout = fake.layout().clone();
        assert!(run_brief(&layout).is_none(), "还没跑过 → 没有那条");

        let rec = RunRecorder::start(&layout, &crate::PreprocessOptions::default())
            .expect("起一份运行记录");
        rec.stage(RunStage::Transcribing, "正在跑 whisper", None);

        let brief = run_brief(&layout).expect("记录在,就该有摘要:");
        assert_eq!(brief.state, "running");
        assert!(brief.message.contains("whisper"), "{}", brief.message);
        assert!(!brief.run_id.is_empty());
    }

    /// `--fresh`:什么都不收,状态清零 —— 留给"状态坏了、也不想照明文将就"的人。
    ///
    /// 而且它**拿不到任何便宜**:清出来的状态是"什么都还没核验",门只会拦得更死。
    #[test]
    fn upgrade_fresh_starts_over_without_touching_the_materials() {
        let fake = crate::testkit::Fake::new("upgrade-fresh")
            .with_transcript(&[(0.0, 2.0, "各位好")])
            .with_shot(1, 2.0);
        fake.set_shot_check(1, Some(a_check()));
        let shots_before = std::fs::read(fake.layout().shots_json()).unwrap();

        // 状态读不动了 —— 正是这条路要救的场面
        std::fs::write(fake.layout().state_file(), b"broken").unwrap();
        let out = upgrade(
            &fake.layout().root().display().to_string(),
            &UpgradeOptions {
                fresh: true,
                accept_material: false,
            },
        )
        .unwrap();
        assert!(out.note.as_deref().unwrap_or("").contains("--fresh"));
        assert_eq!(out.shot_checks, 0);

        let state = vidbrief_project::ProjectState::read_file(&fake.layout().state_file())
            .unwrap()
            .expect("该写出一份能读的状态");
        assert!(state.shots.is_empty(), "清零就该真的空");

        // 材料原样:投影不写、内容不动
        assert_eq!(std::fs::read(fake.layout().shots_json()).unwrap(), shots_before);
    }

    /// **`--fresh` 清的是核验记录,不是「步 1 做完了」那件事。**
    ///
    /// 2026-09-27 实测踩过:五份项目跑过一次 `--fresh` 重做模型那部分,材料一个字节
    /// 没动,而 `status` 从此报「① 前处理 — 未做」—— 显示上分不出"没跑过"和
    /// "跑过、状态被重起过",查了半天"标记怎么丢了"。
    /// 步 1 的标记记的是**这趟算法活儿跑完了没有**,材料还在,它就还成立。
    #[test]
    fn upgrade_fresh_keeps_the_step_one_mark_but_clears_the_checks() {
        let fake = crate::testkit::Fake::new("upgrade-fresh-keeps-mark")
            .with_transcript(&[(0.0, 2.0, "各位好")])
            .with_shot(1, 2.0);
        fake.set_shot_check(1, Some(a_check()));
        {
            let mut opened = fake.open();
            opened.state_mut().progress.preprocess.mark_done();
            opened.state_mut().progress.summarize.mark_done();
            opened.save().unwrap();
        }
        let before = vidbrief_project::ProjectState::read_file(&fake.layout().state_file())
            .unwrap()
            .unwrap();
        assert!(before.progress.preprocess.done && before.progress.summarize.done);

        let out = upgrade(
            &fake.layout().root().display().to_string(),
            &UpgradeOptions {
                fresh: true,
                accept_material: false,
            },
        )
        .unwrap();
        assert_eq!(out.shot_checks, 0, "核验记录该清零");
        assert!(!out.progress_moved, "没有旧格式的进度要搬");

        let after = vidbrief_project::ProjectState::read_file(&fake.layout().state_file())
            .unwrap()
            .unwrap();
        assert!(after.progress.preprocess.done, "步 1 的完成标记不该被清");
        assert!(!after.progress.summarize.done, "总结是模型产物,该清");
        assert!(after.shots.is_empty(), "核验记录该清");
        assert!(
            after.rev > before.rev,
            "rev 该往上走、不倒退({} → {})",
            before.rev,
            after.rev
        );
    }

    /// **清完结论,投影跟着重写。**
    ///
    /// 投影(`transcript.fixed.json` 与那份人读的 `transcript.read.md`)说的是"状态长什么样"。
    /// 状态重起之后它们还挂着上一版,就是两处各说各话 —— 实测:清完场之后打开那份人读投影,
    /// 上面还写着"改了 19 段、存疑 4 段",而那些结论一条都不在了。
    #[test]
    fn fresh_upgrade_rewrites_the_transcript_projections() {
        let fake = crate::testkit::Fake::new("upgrade-fresh-projections")
            .with_transcript(&[(0.0, 2.0, "派森列表"), (2.0, 4.0, "各位好")]);
        {
            let mut opened = fake.open();
            crate::apply_corrections(
                &mut opened,
                &[crate::CorrectionInput {
                    segment_id: 1,
                    conclusion: vidbrief_project::Conclusion::Corrected {
                        text: "Python 列表".into(),
                        kind: vidbrief_project::CorrectionType::Terminology,
                        note: String::new(),
                        reason: "画面文字".into(),
                    },
                }],
                &[2],
                &[],
            )
            .unwrap();
        }
        let read_before = std::fs::read_to_string(fake.layout().transcript_read()).unwrap();
        assert!(
            read_before.contains("Python 列表"),
            "先确认投影里真写着结论:\n{read_before}"
        );
        assert!(read_before.contains("改了 1"), "{read_before}");

        upgrade(
            &fake.layout().root().display().to_string(),
            &UpgradeOptions {
                fresh: true,
                accept_material: false,
            },
        )
        .unwrap();

        let read_after = std::fs::read_to_string(fake.layout().transcript_read()).unwrap();
        assert!(read_after.contains("派森列表"), "该回到原文:\n{read_after}");
        assert!(!read_after.contains("Python 列表"), "{read_after}");
        assert!(
            read_after.contains("改了 0") && read_after.contains("未核验 1 组"),
            "{read_after}"
        );

        let fixed: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(fake.layout().transcript_fixed()).unwrap(),
        )
        .unwrap();
        assert!(
            fixed["segments"]
                .as_array()
                .unwrap()
                .iter()
                .all(|s| s["conclusion"].is_null()),
            "纠错稿投影也该跟着清:{fixed}"
        );
    }

    /// **不改任何状态的一次 `upgrade`,照样把三份投影刷成当前口径。**
    ///
    /// 这条是 2026-09-28 那趟(材料里的时刻从 `m:ss` 换成秒)逼出来的:早退路径只看版本号,
    /// 于是回一句"什么都没动" —— 那句话对状态成立,对**明文投影**不成立,它们还挂着旧写法。
    /// 判据就盯这个:把投影手改成过时的样子,跑一次普通 `upgrade`,三份都该按状态重写回来。
    #[test]
    fn a_plain_upgrade_refreshes_all_three_projections() {
        let fake = crate::testkit::Fake::new("upgrade-refresh-projections")
            .with_transcript(&[(899.6, 906.0, "派森列表")]);
        {
            let mut opened = fake.open();
            crate::apply_corrections(
                &mut opened,
                &[crate::CorrectionInput {
                    segment_id: 1,
                    conclusion: vidbrief_project::Conclusion::Corrected {
                        text: "Python 列表".into(),
                        kind: vidbrief_project::CorrectionType::Terminology,
                        note: String::new(),
                        reason: "画面文字".into(),
                    },
                }],
                &[],
                &[],
            )
            .unwrap();
        }

        // 把三份投影糟蹋成"过时的样子"(旧口径的 `m:ss` 时刻)
        for path in [
            fake.layout().transcript_read(),
            fake.layout().transcript_diff(),
            fake.layout().transcript_fixed(),
        ] {
            std::fs::write(&path, "14:60 旧写法\n").unwrap();
        }

        let outcome = upgrade(
            &fake.layout().root().display().to_string(),
            &UpgradeOptions::default(),
        )
        .unwrap();
        assert!(
            outcome.note.as_deref().unwrap_or("").contains("投影"),
            "得说清刷了投影:{:?}",
            outcome.note
        );

        let read = std::fs::read_to_string(fake.layout().transcript_read()).unwrap();
        assert!(!read.contains("14:60"), "投影该按当前口径重写:\n{read}");
        assert!(read.contains("899.6"), "时刻是秒:\n{read}");
        assert!(read.contains("Python 列表"), "结论还在:\n{read}");

        let diff = std::fs::read_to_string(fake.layout().transcript_diff()).unwrap();
        assert!(!diff.contains("14:60") && diff.contains("| 899.6 |"), "{diff}");

        let fixed = std::fs::read_to_string(fake.layout().transcript_fixed()).unwrap();
        assert!(fixed.contains("Python 列表"), "纠错稿投影也重写了:{fixed}");
    }
}
