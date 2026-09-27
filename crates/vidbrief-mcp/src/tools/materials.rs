//! C 组:材料服务(`list_shots` / `get_shot` / `get_transcript` / `search_transcript` /
//! `get_change_curve` / `list_regions`)。
//!
//! 设计见 `docs/DESIGN.md` §6.1。**这里是"agent 能看图"落地的地方** ——
//! `get_shot` 返回的是**图像本身**,不是路径、不是描述。

use base64::Engine as _;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use vidbrief_project::ShotId;

use crate::json::{blocking, json, json_compact};
use crate::server::VidbriefServer;

/// 同期转录取多长(秒)。± 这个值,见 `docs/DESIGN.md` §6.2 ①。
const DEFAULT_NEIGHBOR_WINDOW: f64 = 6.0;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectRef {
    #[schemars(description = "项目:项目目录的路径,或 `projects/` 下的项目名")]
    pub project_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShotRef {
    #[schemars(description = "项目:项目目录的路径,或 `projects/` 下的项目名")]
    pub project_id: String,
    /// 截图 id,形如 `s007`(也接受纯数字 `7`)。
    pub shot_id: String,
    /// 同期转录取前后多少秒。默认 6.0。
    #[serde(default)]
    pub window: Option<f64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TranscriptQuery {
    #[schemars(description = "项目:项目目录的路径,或 `projects/` 下的项目名")]
    pub project_id: String,
    /// 起点(秒)。与 `end` 一起给;两个都不给 = 全量。
    #[serde(default)]
    pub start: Option<f64>,
    /// 终点(秒)。和 `start` 必须成对 —— 只给一头会被打回来问清楚。
    #[serde(default)]
    pub end: Option<f64>,
    /// 按**组**给(默认)还是逐段给。
    ///
    /// 组 = 一口气说完的一小段(whisper 按停顿切段,一段常常只有三五个字)。
    /// 通读、找哪儿不对、报「这些我看过了」都按组;**已经定位到某一句、
    /// 要给 `corrections` 报段号**时才逐段 —— 那时配上 `start`/`end` 只看那一小段。
    #[serde(default)]
    pub grouped: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchQuery {
    #[schemars(description = "项目:项目目录的路径,或 `projects/` 下的项目名")]
    pub project_id: String,
    /// 要找的子串(大小写不敏感)。中文没有词边界,这里就是朴素的包含匹配。
    pub query: String,
    /// 最多返回多少段。默认 40。
    #[serde(default)]
    pub limit: Option<usize>,
}

/// 把 `s007` / `7` 都认成 shot id。
///
/// 契约层只认 `s007`(那是文件里、文件名里的写法)。这里放宽一档:
/// agent 手上常常只有一个从 `list_regions` 或转录里看到的数字,
/// 为了一个前缀把它打回去重来,纯属浪费一轮往返。
pub(crate) fn parse_shot_id(raw: &str) -> Result<ShotId, CallToolResult> {
    let text = raw.trim();
    let normalized = if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) {
        format!("s{text}")
    } else {
        text.to_string()
    };
    ShotId::parse(&normalized).map_err(|e| {
        crate::json::failed(format!(
            "{e:#}\n截图 id 形如 `s007`;list_shots 的返回值里每张图都带着自己的 id。"
        ))
    })
}

#[tool_router(router = materials_tools, vis = "pub")]
impl VidbriefServer {
    #[tool(
        description = "列出项目的全部关键截图(时刻、来源、是否已被校准、是否待裁决)。\
                       **看不到画面**,看图要用 get_shot。",
        annotations(title = "列截图", read_only_hint = true)
    )]
    async fn list_shots(
        &self,
        Parameters(p): Parameters<ProjectRef>,
    ) -> Result<CallToolResult, CallToolResult> {
        let opened = self.open_read(&p.project_id).await?;
        let (shots, counts) = blocking(move || {
            let shots = vidbrief_ops::shots(&opened)?;
            let counts = vidbrief_ops::shot_counts(&shots);
            // 只交数组,不交外层信封:`generated_at` 对判断没有用,
            // 而 `{"shots": {"shots": [...]}}` 这种套娃**真会让人取错**
            // (M4 联调时,写这段客户端的第一版就栽在 KeyError 上)。
            Ok((shots.shots, counts))
        })
        .await?;
        json(&serde_json::json!({
            "counts": counts,
            "shots": shots,
        }))
    }

    #[tool(
        description = "取一张截图:**返回图像本身**,外加它的元数据与该时刻前后 ±6 秒的转录。\
                       这是「看图」的唯一途径 —— 校准图文匹配、判断画面与语音是否对得上,都要用它。\
                       不要凭转录猜画面上有什么。",
        annotations(title = "看一张截图", read_only_hint = true)
    )]
    async fn get_shot(
        &self,
        Parameters(p): Parameters<ShotRef>,
    ) -> Result<CallToolResult, CallToolResult> {
        let id = parse_shot_id(&p.shot_id)?;
        let window = p.window.unwrap_or(DEFAULT_NEIGHBOR_WINDOW);
        let opened = self.open_read(&p.project_id).await?;

        let detail = blocking(move || vidbrief_ops::shot_detail(&opened, id, window)).await?;

        // 元数据在前、图像在后:先告诉模型"这是什么、什么时候、同期说了什么",
        // 再让它看图 —— 顺序反了的话模型容易只描述画面而忽略对照。
        //
        // 这里**不给绝对路径**:get_shot 的路径是 `E:\temp\x\01_preprocess/shots/s001.jpg`
        // 那种混合分隔符(Windows 上 `PathBuf` 拼出来的),既难看又是 agent 用不上的东西
        // (它读不了文件,要图就用这个工具、要引用就用 `shot.id`)。
        // 项目内一律 `RelPath`,这条规矩在 `AGENTS.md` 里。
        let meta = serde_json::json!({
            "shot": detail.shot,
            "window_start": detail.window_start,
            "window_end": detail.window_end,
            "transcript": detail.transcript,
            "mime": detail.mime,
            "bytes": detail.image.len(),
        });
        let encoded = base64::engine::general_purpose::STANDARD.encode(&detail.image);

        Ok(CallToolResult::success(vec![
            ContentBlock::text(
                serde_json::to_string_pretty(&meta)
                    .unwrap_or_else(|e| format!("元数据序列化失败:{e}")),
            ),
            ContentBlock::image(encoded, detail.mime.clone()),
        ]))
    }

    #[tool(
        description = "取转录(或某个时间段)。**默认按「组」返回** —— 组是一口气说完的一小段:\
                       whisper 按停顿切段,一段常常只有三五个字,逐段读一遍两千多段会把上下文顶穿。\
                       有纠错稿就给纠错稿,返回里的 `corrected` 告诉你读的是哪一份。\
                       校准前先用它通读一遍找识别错误;报「这些我看过了」时,把组自己的\
                       `from`/`to` 当区间报给 apply_corrections。\
                       **要逐段原文**(已经定位到某一句、要给 corrections 报段号)就给 \
                       `grouped: false` 配上 `start`/`end`,只看那一小段。",
        annotations(title = "读转录", read_only_hint = true)
    )]
    async fn get_transcript(
        &self,
        Parameters(p): Parameters<TranscriptQuery>,
    ) -> Result<CallToolResult, CallToolResult> {
        let range = range_of(p.start, p.end)?;
        let opened = self.open_read(&p.project_id).await?;
        // 默认按组:通读与表态都按组,逐段是"手术"时才要的形状
        if p.grouped.unwrap_or(true) {
            let view = blocking(move || vidbrief_ops::transcript_groups(&opened, range)).await?;
            return json(&view);
        }
        let view = blocking(move || vidbrief_ops::transcript(&opened, range)).await?;
        json(&view)
    }

    #[tool(
        description = "在转录里搜子串(大小写不敏感),返回命中的段。\
                       找某句话说在哪、某个术语出现过几次时用它,比通读全文省得多。",
        annotations(title = "搜转录", read_only_hint = true)
    )]
    async fn search_transcript(
        &self,
        Parameters(p): Parameters<SearchQuery>,
    ) -> Result<CallToolResult, CallToolResult> {
        let limit = p.limit.unwrap_or(40);
        let opened = self.open_read(&p.project_id).await?;
        let view = blocking(move || vidbrief_ops::transcript_search(&opened, &p.query, limit)).await?;
        json(&view)
    }

    #[tool(
        description = "取差帧图(每个采样时刻的帧间差异)。横轴是时间,尖峰就是画面变了。\
                       整段无变化的地方会是接近 0 的一串。",
        annotations(title = "读差帧图", read_only_hint = true)
    )]
    async fn get_change_curve(
        &self,
        Parameters(p): Parameters<TranscriptQuery>,
    ) -> Result<CallToolResult, CallToolResult> {
        let range = range_of(p.start, p.end)?;
        let opened = self.open_read(&p.project_id).await?;
        let curve = blocking(move || vidbrief_ops::change_curve(&opened, range)).await?;
        // 点数上千、结构扁平:不缩进,省 token
        json_compact(&curve)
    }

    #[tool(
        description = "取变化密集区清单(每段的起止、峰值时刻、为什么被选中)。\
                       这是算法对视频结构的粗判,**不是内容判断** —— 讲的是什么还得看图和转录。",
        annotations(title = "列密集区", read_only_hint = true)
    )]
    async fn list_regions(
        &self,
        Parameters(p): Parameters<ProjectRef>,
    ) -> Result<CallToolResult, CallToolResult> {
        let opened = self.open_read(&p.project_id).await?;
        let regions = blocking(move || vidbrief_ops::list_regions(&opened)).await?;
        json(&regions)
    }
}

/// `start` / `end` 要么都给要么都不给 —— 只给一头多半是笔误,不如直接问清楚。
fn range_of(start: Option<f64>, end: Option<f64>) -> Result<Option<(f64, f64)>, CallToolResult> {
    match (start, end) {
        (None, None) => Ok(None),
        (Some(a), Some(b)) => Ok(Some((a, b))),
        (Some(a), None) => Err(crate::json::failed(format!(
            "只给了 start={a},没给 end —— 时间段要两端都给,或两端都不给(取全量)"
        ))),
        (None, Some(b)) => Err(crate::json::failed(format!(
            "只给了 end={b},没给 start —— 时间段要两端都给,或两端都不给(取全量)"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shot_ids_accept_both_spellings() {
        assert_eq!(parse_shot_id("s007").unwrap(), ShotId::new(7).unwrap());
        assert_eq!(parse_shot_id("7").unwrap(), ShotId::new(7).unwrap());
        assert!(parse_shot_id("第七张").is_err());
    }

    #[test]
    fn half_open_ranges_are_refused_with_the_value_that_was_given() {
        let err = range_of(Some(3.0), None).unwrap_err();
        assert_eq!(err.is_error, Some(true));
        assert!(range_of(None, Some(9.0)).is_err());
        assert_eq!(range_of(None, None).unwrap(), None);
        assert_eq!(range_of(Some(1.0), Some(2.0)).unwrap(), Some((1.0, 2.0)));
    }
}
