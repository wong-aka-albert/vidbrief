//! A 组:项目(`create_project` / `list_projects` / `get_project`)。
//!
//! 设计见 `docs/DESIGN.md` §11 A。这三个是"入口三件套":
//! agent 靠它们知道**有哪些项目、这个项目做到哪了**。

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use vidbrief_ops::{CreateOptions, Notice};
use vidbrief_project::SourceLink;

use crate::json::{blocking, json};
use crate::server::VidbriefServer;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateProjectParams {
    /// 源视频路径(任何 ffmpeg 认得的格式:mp4/mkv/mov…)。
    pub video_path: String,
    /// 项目 id:ASCII 小写字母/数字/`-`/`_`。默认由视频文件名净化;
    /// 纯中文名净化不出来时会报错,那时必须显式给一个。
    #[serde(default)]
    pub name: Option<String>,
    /// 人类可读名(可含中文)。默认取视频文件名。
    #[serde(default)]
    pub title: Option<String>,
    /// 项目目录本身。**默认建在 `projects/<id>`**。
    ///
    /// 位置已经存在时会报错 —— 工具**不覆盖、也不会自动换个名字**。
    /// 报错里会说清那边是什么:一个已经有项目的目录,还是一个没建完的目录。
    ///
    /// **撞上了怎么办由你判断**:换个 `name` / `project_dir` 另起一个,
    /// 或者先问用户 —— 他可能本来就想要那一个。
    /// **不要为了腾地方去删目录**(那里面可能是别人的活,或你自己的半成品)。
    #[serde(default)]
    pub project_dir: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectRef {
    #[schemars(description = "项目:项目目录的路径,或 `projects/` 下的项目名")]
    pub project_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListProjectsParams {
    /// 项目们所在的父目录。默认是服务启动时工作目录下的 `projects/`。
    #[serde(default)]
    pub projects_dir: Option<String>,
}

/// `create_project` 的回执。
///
/// 只放"agent 下一步要用到的东西":项目怎么称呼它(`project_id` 与 `dir` 两种写法)、
/// 源视频是什么、以及**它是不是自包含的**(决定报告里能不能引用视频)。
#[derive(Debug, Serialize)]
struct CreatedView<'a> {
    project_id: &'a str,
    dir: String,
    name: &'a str,
    source_path: String,
    source_original: String,
    /// `copy` 才是自包含的;`hardlink` / `symlink` 依赖原文件还在。
    link: SourceLink,
    self_contained: bool,
    duration: f64,
    size_bytes: u64,
    resolution: Option<String>,
    fps: Option<f64>,
    sha256: &'a str,
    created_at: chrono::DateTime<chrono::FixedOffset>,
    /// 退让记录:硬链接为什么没成。空数组 = 一次到位。
    fallbacks: &'a [String],
    /// 建项目期说过的话(跨卷复制等)。**给人看的**,agent 应转述给用户。
    notes: Vec<String>,
    next_step: String,
}

#[tool_router(router = project_tools, vis = "pub")]
impl VidbriefServer {
    #[tool(
        description = "新建一个项目:建目录骨架、把源视频搬进来(硬链接→软链接→复制)、\
                       用 ffprobe 探测元数据。**只建壳子,不跑任何算法** —— \
                       跑前处理是紧接着的 preprocess。返回 project_id 与目录。",
        annotations(title = "建项目", read_only_hint = false, destructive_hint = false)
    )]
    async fn create_project(
        &self,
        Parameters(p): Parameters<CreateProjectParams>,
    ) -> Result<CallToolResult, CallToolResult> {
        let state = self.state.clone();
        let notes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = notes.clone();

        let created = blocking(move || {
            let mut on_notice = |notice: Notice| {
                let text = match notice {
                    Notice::CrossVolumeCopy { size_bytes, hint } => {
                        let mut line = format!(
                            "素材与项目不在同一个卷,硬链接用不了,已复制 {}",
                            vidbrief_project::human_size(size_bytes)
                        );
                        if let Some(hint) = hint {
                            line.push_str(&format!(
                                ";想省下这份空间,下次把项目建到素材同卷:{hint}"
                            ));
                        }
                        line
                    }
                };
                sink.lock().unwrap_or_else(|e| e.into_inner()).push(text);
            };
            vidbrief_ops::create(
                &CreateOptions {
                    video: p.video_path.clone().into(),
                    name: p.name.clone(),
                    title: p.title.clone(),
                    project_dir: p.project_dir.clone().map(Into::into),
                },
                &mut on_notice,
            )
        })
        .await?;

        state.note(created.layout.root(), created.id.as_ref());

        let meta = &created.meta;
        let view = CreatedView {
            project_id: created.id.as_ref(),
            dir: created.layout.root().display().to_string(),
            name: &created.project.name,
            source_path: created.project.source.path.as_str().to_string(),
            source_original: created.project.source.original.clone(),
            link: created.project.source.link,
            self_contained: created.project.source.link.is_self_contained(),
            duration: meta.duration,
            size_bytes: meta.size_bytes,
            resolution: meta.resolution().map(|(w, h)| format!("{w}x{h}")),
            fps: meta.fps,
            sha256: &meta.sha256,
            created_at: created.project.created_at,
            fallbacks: &created.fallbacks,
            notes: notes.lock().map(|n| n.clone()).unwrap_or_default(),
            next_step: "跑 preprocess 做步 1(纯算法,一次做完);做完停下来给用户看时间线。".into(),
        };
        json(&view)
    }

    #[tool(
        description = "列出 `projects/` 下的所有项目,含各自做到第几步、材料齐不齐、\
                       源视频还在不在。**坏项目也会列出来**(标 problem),不会悄悄跳过。",
        annotations(title = "列项目", read_only_hint = true)
    )]
    async fn list_projects(
        &self,
        Parameters(p): Parameters<ListProjectsParams>,
    ) -> Result<CallToolResult, CallToolResult> {
        let state = self.state.clone();
        let root = match &p.projects_dir {
            Some(dir) => std::path::PathBuf::from(dir),
            None => state.cwd().join(vidbrief_project::layout::PROJECTS_DIR),
        };

        let listed = blocking({
            let root = root.clone();
            move || vidbrief_ops::list_projects(&root)
        })
        .await?;

        // 列出来的项目也记进"见过",这样 resources/list 能一并给出
        for item in &listed {
            state.note(&item.dir, &item.id);
        }

        let view = serde_json::json!({
            "projects_dir": root.display().to_string(),
            "count": listed.len(),
            "projects": listed,
        });
        json(&view)
    }

    #[tool(
        description = "看一个项目的全貌:四步进度、材料清单(差帧图/密集区/截图/转录各做了多少)、\
                       源视频健康检查。**开工前先调它** —— 你会知道哪些材料已经有了、\
                       哪些还没做,不用靠猜。项目状态文件读不动、或者项目还是旧格式时,\
                       它也照样答,并在 materials.state_note 里说清是怎么回事、下一步该做什么。",
        annotations(title = "看项目状态", read_only_hint = true)
    )]
    async fn get_project(
        &self,
        Parameters(p): Parameters<ProjectRef>,
    ) -> Result<CallToolResult, CallToolResult> {
        let state = self.state.clone();
        let id = p.project_id.clone();
        // 走**能容忍坏状态**的那条路(与 `vidbrief status` 同一条):这是"看一眼"的工具,
        // 项目有毛病的时候正是它最该说话的时候。要动手的工具走 `OpenProject`
        // (状态读不动就直接拒绝开工)—— 两者的分工见 `inspect` 的模块说明。
        let report =
            blocking(move || vidbrief_ops::inspect(&id, &vidbrief_ops::InspectOptions::default()))
                .await?;
        state.note(&report.dir, report.project.id.as_ref());
        json(&report)
    }
}
