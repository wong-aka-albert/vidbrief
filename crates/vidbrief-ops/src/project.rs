//! 项目的**建**与**开**。
//!
//! 对应 MCP 工具组的 A 组(`create_project` / `list_projects` / `get_project`),
//! 以及 CLI 的 `init` / `status`。设计见 `docs/DESIGN.md` §4.8(源视频怎么进来)、§10.5。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use vidbrief_project::{
    Health, Layout, Project, ProjectId, SourceLink, VideoMeta, check_source_link, materialize, plan,
    simplify_path, video_rel_for,
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
        "找不到项目:`{arg}` —— 它既不是含 {} 的目录,也不是 projects/ 下的项目名",
        vidbrief_project::layout::PROJECT_FILE
    )
}

/// 一个已经打开的项目:`project.json` + `source/video.meta.json` 都读进来了。
///
/// **不含写锁**。锁回答的是"谁有资格改",那是入口层的事(CLI 一次命令一把,
/// MCP 是会话级长持);混进来会让"只想看一眼"的材料查询也被锁挡住。
#[derive(Debug, Clone)]
pub struct OpenProject {
    layout: Layout,
    project: Project,
    meta: VideoMeta,
}

impl OpenProject {
    /// 按 CLI 的写法解析并打开。
    pub fn open(arg: &str) -> Result<Self> {
        Self::from_layout(resolve(arg)?)
    }

    /// 已经拿到 [`Layout`] 时直接打开(不重复解析路径)。
    pub fn from_layout(layout: Layout) -> Result<Self> {
        let project = Project::load(&layout.project_json()).with_context(|| {
            format!("读取项目文件失败:{}", layout.project_json().display())
        })?;
        let meta = VideoMeta::load(&layout.video_meta()).with_context(|| {
            format!(
                "读取视频元数据失败:{} —— 项目可能没建完(重跑 `vidbrief init` 或换个项目)",
                layout.video_meta().display()
            )
        })?;
        Ok(Self {
            layout,
            project,
            meta,
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

    /// 存回 `project.json`(会把 `rev` +1)。
    pub fn save(&mut self) -> Result<()> {
        self.project
            .save(&self.layout.project_json())
            .with_context(|| format!("写入 {} 失败", self.layout.project_json().display()))
    }

    /// 可变地拿到 `project.json` 的内容(改进度、改术语表)。
    pub fn project_mut(&mut self) -> &mut Project {
        &mut self.project
    }

    /// 源视频的链接健康检查(不算哈希,很快)。
    pub fn link_health(&self) -> Health {
        check_source_link(&self.project, &self.layout)
    }
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
    let mut project = Project::new(
        id.clone(),
        title,
        rel,
        original.display().to_string(),
        placed.link,
    );
    project.save(&layout.project_json())?;

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
    pub rev: u64,
    /// 四步进度。
    pub progress: vidbrief_project::Progress,
    /// 源视频还在不在(轻量检查,不重算哈希)。
    pub health: crate::inspect::HealthView,
    /// 材料做到哪一步了(只看文件在不在,不解析内容)。
    pub materials: crate::inspect::Materials,
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
fn summarize(layout: &Layout) -> Result<ProjectSummary> {
    let materials = crate::inspect::scan_materials(layout);
    let dir = layout.root().to_path_buf();

    let project = match Project::load(&layout.project_json()) {
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
                rev: 0,
                progress: vidbrief_project::Progress::default(),
                health: crate::inspect::HealthView::problem(format!("项目文件读不出来:{e:#}")),
                materials,
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
        rev: project.rev,
        progress: project.progress.clone(),
        health,
        materials,
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
}
