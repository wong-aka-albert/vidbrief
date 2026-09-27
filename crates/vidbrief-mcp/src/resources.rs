//! 只读项目文件(MCP Resources)。
//!
//! 设计见 `docs/DESIGN.md` §10.2 —— 除了工具,还把项目文件本身挂成资源,
//! 让 agent 能像读文件一样读材料,不必为每个小文件发明一个工具。
//!
//! # URI 长什么样
//!
//! ```text
//! vidbrief://project/<项目 id>/<项目根相对路径>
//! vidbrief://project/demo/01_preprocess/change_curve.json
//! vidbrief://project/demo/01_preprocess/shots/s007_0479.0.jpg
//! ```
//!
//! 用**项目 id** 而不是目录:项目可以建在任何路径(`init -p`),拿路径当 URI
//! 会又长又带盘符和反斜杠,而且换个机器就全失效。
//!
//! # 什么时候该用工具、什么时候该用资源
//!
//! | 想要什么 | 用哪个 | 为什么 |
//! |---------|--------|--------|
//! | 一张截图 + 它的同期转录 | `get_shot` **工具** | 资源只能给文件本身,给不了"这张图在讲什么" |
//! | 原始 JSON / 笔记 / 报告 | 资源 | 一比一对应,不必造工具 |
//! | 视频本身 | 都不行 | 几十 MB,应该开时间线视图看 |

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use rmcp::model::{
    ListResourceTemplatesResult, ListResourcesResult, ReadResourceResult, Resource,
    ResourceContents, ResourceTemplate,
};

use crate::state::State;

/// URI 前缀。改它等于改对外契约。
pub const URI_PREFIX: &str = "vidbrief://project/";

/// 单个资源的大小上限。
///
/// 8 MB 是"一张 jpg、一份 JSON、一篇报告"绰绰有余、而"一个视频"远远不够的量级。
/// 撞上它说明要读的东西不该走资源通道。
const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// 可以被当资源读的项目文件(项目根相对路径)。
///
/// **只列已知的材料** —— 不递归整个目录:那样会把 `source/video.mp4`、
/// `shots/*.jpg` 之外的一堆东西也扫进来,而列举本身是要花 agent 的 token 的。
const LISTED_FILES: &[&str] = &[
    vidbrief_project::layout::PROJECT_FILE,
    vidbrief_project::layout::VIDEO_META_FILE,
    vidbrief_project::layout::CHANGE_CURVE_FILE,
    vidbrief_project::layout::DENSE_REGIONS_FILE,
    vidbrief_project::layout::TRANSCRIPT_FILE,
    vidbrief_project::layout::SHOTS_FILE,
    vidbrief_project::layout::TRANSCRIPT_FIXED_FILE,
    vidbrief_project::layout::TRANSCRIPT_DIFF_FILE,
    vidbrief_project::layout::SUPPLEMENT_REQUESTS_FILE,
    vidbrief_project::layout::CROSS_VALIDATE_FILE,
    vidbrief_project::layout::CALIBRATION_LOG_FILE,
    vidbrief_project::layout::AGENT_LOG_FILE,
    vidbrief_project::layout::SUMMARY_FILE,
    vidbrief_project::layout::REPORT_MD_FILE,
];

/// 解析 `vidbrief://project/<id>/<rel>`。
///
/// 返回 `(项目 id, 项目根相对路径)`。路径部分可能为空(客户端手滑),照样返回,
/// 由调用方决定怎么报错。
pub fn parse_uri(uri: &str) -> Result<(String, String)> {
    let rest = uri
        .strip_prefix(URI_PREFIX)
        .with_context(|| format!("不认识的资源 URI:`{uri}`(应以 `{URI_PREFIX}` 开头)"))?;
    match rest.split_once('/') {
        Some((id, path)) => Ok((id.to_string(), path.to_string())),
        None => Ok((rest.to_string(), String::new())),
    }
}

/// 拼一个资源 URI。
pub fn make_uri(project_id: &str, rel: &str) -> String {
    format!("{URI_PREFIX}{project_id}/{rel}")
}

/// 从项目 id 找到项目目录:先查本会话碰过的,再退回 `cwd/projects/<id>`。
fn project_dir(state: &State, id: &str) -> Result<PathBuf> {
    if let Some(dir) = state.dir_of(id) {
        return Ok(dir);
    }
    let guess = state.cwd().join(vidbrief_project::layout::PROJECTS_DIR).join(id);
    if guess.join(vidbrief_project::layout::PROJECT_FILE).is_file() {
        return Ok(guess);
    }
    bail!(
        "找不到项目 `{id}` —— 它既不是本会话碰过的项目,也不在 {} 下。\
         先调 list_projects 看有哪些。",
        guess.display()
    )
}

/// 按扩展名判 MIME。
///
/// 不引 mime 猜库:本服务只可能吐出这几种,一张表比一个依赖便宜得多。
fn mime_of(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("json") => "application/json",
        Some("md") => "text/markdown",
        Some("txt") => "text/plain",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("vtt") => "text/vtt",
        Some("srt") => "application/x-subrip",
        _ => "application/octet-stream",
    }
}

/// 是文本类吗?决定走 `text` 还是 `blob`。
fn is_text(mime: &str) -> bool {
    mime.starts_with("text/") || mime == "application/json" || mime == "application/x-subrip"
}

/// 这个人读得懂的标题(给资源列表用)。
fn title_of(rel: &str) -> &str {
    match rel {
        vidbrief_project::layout::PROJECT_FILE => "项目元数据与进度",
        vidbrief_project::layout::VIDEO_META_FILE => "源视频探测结果",
        vidbrief_project::layout::CHANGE_CURVE_FILE => "差帧图",
        vidbrief_project::layout::DENSE_REGIONS_FILE => "变化密集区",
        vidbrief_project::layout::TRANSCRIPT_FILE => "转录(原始,永不改写)",
        vidbrief_project::layout::TRANSCRIPT_FIXED_FILE => "转录(纠错稿)",
        vidbrief_project::layout::TRANSCRIPT_DIFF_FILE => "转录纠错对照表",
        vidbrief_project::layout::SHOTS_FILE => "截图清单与校准结论",
        vidbrief_project::layout::SUPPLEMENT_REQUESTS_FILE => "补图候选",
        vidbrief_project::layout::CROSS_VALIDATE_FILE => "图文交叉印证",
        vidbrief_project::layout::CALIBRATION_LOG_FILE => "校准日志",
        vidbrief_project::layout::AGENT_LOG_FILE => "探索日志",
        vidbrief_project::layout::SUMMARY_FILE => "图文总结",
        vidbrief_project::layout::REPORT_MD_FILE => "报告(Markdown)",
        _ => rel,
    }
}

/// 列资源:本会话碰过的项目 + `projects/` 下的项目,各自现存的材料文件与截图。
pub fn list_resources(state: &State) -> ListResourcesResult {
    let mut projects = state.known_projects();

    // `projects/` 下还有本会话没碰过的项目 —— 一并列出来,免得 agent 以为只有自己做过的那些
    let root = state.cwd().join(vidbrief_project::layout::PROJECTS_DIR);
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            let dir = entry.path();
            if dir.is_dir() && dir.join(vidbrief_project::layout::PROJECT_FILE).is_file() {
                let id = read_project_id(&dir).unwrap_or_else(|| {
                    dir.file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("?")
                        .to_string()
                });
                if !projects.iter().any(|(known, _)| known == &dir) {
                    projects.push((dir, id));
                }
            }
        }
    }

    let mut resources = Vec::new();
    for (dir, id) in projects {
        for rel in LISTED_FILES {
            let path = dir.join(rel);
            if !path.is_file() {
                continue;
            }
            resources.push(describe(&id, rel, &path));
        }
        // 截图:一张一个资源
        let shots_dir = dir.join(vidbrief_project::layout::SHOTS_REL_DIR);
        if let Ok(entries) = std::fs::read_dir(&shots_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.ends_with(".jpg") {
                    continue;
                }
                let rel = format!("{}/{name}", vidbrief_project::layout::SHOTS_REL_DIR);
                resources.push(describe(&id, &rel, &entry.path()));
            }
        }
    }

    ListResourcesResult::with_all_items(resources)
}

fn describe(project_id: &str, rel: &str, path: &Path) -> Resource {
    let uri = make_uri(project_id, rel);
    let mut resource = Resource::new(uri, format!("{project_id}:{rel}"))
        .with_title(title_of(rel))
        .with_mime_type(mime_of(path));
    if let Ok(meta) = path.metadata() {
        resource = resource.with_size(meta.len());
    }
    resource
}

/// 从 `project.json` 里读 id(读不出来就返回 `None`,交给调用方兜底)。
fn read_project_id(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(vidbrief_project::layout::PROJECT_FILE)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some(value.get("id")?.as_str()?.to_string())
}

/// 资源模板:让客户端知道"任何项目、任何路径"都可以试 —— 列表只是提示,不是权威。
pub fn list_templates() -> ListResourceTemplatesResult {
    let template = ResourceTemplate::new(format!("{URI_PREFIX}{{project_id}}/{{path}}"), "项目文件")
        .with_title("项目材料文件")
        .with_description(
            "项目根相对路径下的任意文件:材料 JSON、笔记与报告 Markdown、截图 jpg。\
             不知道有哪些就先调 tools/list 里的 list_shots / list_regions,或读 resources/list。",
        );
    ListResourceTemplatesResult::with_all_items(vec![template])
}

/// 读一个资源。
pub fn read(state: &State, uri: &str) -> Result<ReadResourceResult> {
    let (project_id, rel) = parse_uri(uri)?;
    if rel.is_empty() {
        bail!("URI 里没给文件路径:`{uri}`(应形如 {URI_PREFIX}{project_id}/01_preprocess/shots.json)");
    }

    // 两道校验:RelPath 挡住目录穿越(构造即校验),canonicalize 再确认落点真在项目里。
    // 只做前一道也能挡住 `../`,但软链接可以把人带出项目 —— 两道都要。
    let rel_path = vidbrief_project::RelPath::new(&rel)
        .with_context(|| format!("路径不合法:`{rel}`"))?;
    let root = project_dir(state, &project_id)?;
    let full = root.join(rel_path.as_str());

    let canonical = full
        .canonicalize()
        .with_context(|| format!("读不到这个资源:{}", full.display()))?;
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.clone());
    if !canonical.starts_with(&canonical_root) {
        bail!("路径越出了项目目录:`{rel}`");
    }

    let meta = canonical
        .metadata()
        .with_context(|| format!("读不到文件属性:{}", canonical.display()))?;
    if !meta.is_file() {
        bail!("不是一个文件:{}", canonical.display());
    }
    if meta.len() > MAX_BYTES {
        bail!(
            "这个文件 {} 太大了(上限 {}),不适合走资源通道 —— \
             视频请用 render_workbench 打开工作台看,图片用 get_shot",
            vidbrief_project::human_size(meta.len()),
            vidbrief_project::human_size(MAX_BYTES)
        );
    }

    let mime = mime_of(&canonical);
    let contents = if is_text(mime) {
        let text = std::fs::read_to_string(&canonical)
            .with_context(|| format!("按文本读失败:{}", canonical.display()))?;
        ResourceContents::text(text, uri).with_mime_type(mime)
    } else {
        let bytes =
            std::fs::read(&canonical).with_context(|| format!("读失败:{}", canonical.display()))?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        ResourceContents::blob(encoded, uri).with_mime_type(mime)
    };

    Ok(ReadResourceResult::new(vec![contents]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_round_trips() {
        let uri = make_uri("demo", "01_preprocess/shots.json");
        assert_eq!(uri, "vidbrief://project/demo/01_preprocess/shots.json");
        let (id, rel) = parse_uri(&uri).unwrap();
        assert_eq!(id, "demo");
        assert_eq!(rel, "01_preprocess/shots.json");
    }

    #[test]
    fn bare_project_uri_parses_with_an_empty_path() {
        let (id, rel) = parse_uri("vidbrief://project/demo").unwrap();
        assert_eq!(id, "demo");
        assert!(rel.is_empty());
        // 空路径在读的时候会被明确拒绝,而不是悄悄变成"读项目根目录"
        assert!(read(&State::new(), "vidbrief://project/demo").is_err());
    }

    #[test]
    fn foreign_uris_are_refused_with_the_expected_prefix() {
        let err = parse_uri("file:///etc/passwd").unwrap_err().to_string();
        assert!(err.contains(URI_PREFIX), "{err}");
    }

    #[test]
    fn mime_table_covers_what_we_actually_serve() {
        assert_eq!(mime_of(Path::new("a/b.json")), "application/json");
        assert_eq!(mime_of(Path::new("a/b.MD")), "text/markdown");
        assert_eq!(mime_of(Path::new("a/s001_0009.0.jpg")), "image/jpeg");
        assert_eq!(mime_of(Path::new("a/video.mp4")), "application/octet-stream");
        assert!(is_text("application/json"));
        assert!(!is_text("image/jpeg"));
    }

    #[test]
    fn every_listed_file_has_a_human_title() {
        for rel in LISTED_FILES {
            assert_ne!(title_of(rel), *rel, "`{rel}` 没有给人看的标题");
        }
    }
}
