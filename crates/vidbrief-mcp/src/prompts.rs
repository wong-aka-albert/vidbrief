//! §10.4 的提示模板。
//!
//! # 只放"引用的工具都已就绪"的模板
//!
//! 模板是**接口契约**:里面写「调用 record_shot_check」,那个工具就必须存在,
//! 否则 agent 会照着做、拿到 "method not found",然后开始怀疑整个服务。
//! 所以这里按里程碑逐个放开,不做"先把文本写上、工具以后补"的事:
//!
//! | 模板 | 依赖 | 状态 |
//! |------|------|------|
//! | P1 `calibrate_shots` | list_shots / get_shot / record_shot_check / render_timeline | ✅ 已就绪 |
//! | P2 `fix_transcript` | apply_corrections / render_calibration_table | ✅ 已就绪 |
//! | P3 `find_missing_shots` | find_supplement_candidates / extract_shot | ✅ 已就绪 |
//! | P4 `summarize_video` | get_shot / search_transcript / request_shot / write_summary / write_note | ✅ 已就绪 |
//! | P5 `build_report` | **build_report** | ⏳ 等 M6 的报告渲染器 |
//!
//! 没放出来的能力**照样能用** —— agent 直接调那些工具就行,只是暂时没有现成的话术。

use rmcp::ErrorData;
use rmcp::handler::server::router::prompt::PromptRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{PromptMessage, Role};
use rmcp::{prompt, prompt_router};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::server::VidbriefServer;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProjectArg {
    /// 项目:项目目录的路径,或 `projects/` 下的项目名。
    pub project_id: String,
}

fn user_says(text: String) -> Result<Vec<PromptMessage>, ErrorData> {
    Ok(vec![PromptMessage::new_text(Role::User, text)])
}

#[prompt_router]
impl VidbriefServer {
    /// 逐张检查关键截图:图文对不对得上、音画同不同步
    #[prompt(name = "calibrate_shots", description = "逐张检查关键截图:图文匹配 + 音画同步,结论写回 shots.json")]
    async fn calibrate_shots(
        &self,
        Parameters(args): Parameters<ProjectArg>,
    ) -> Result<Vec<PromptMessage>, ErrorData> {
        let project_id = args.project_id;
        user_says(format!(
            "你是视频图文校准助手。项目:`{project_id}`。\n\
             \n\
             请对项目中的每张关键截图完成图文匹配与音画同步检查:\n\
             1. 调用 `list_shots` 拿到截图清单;对每张调用 `get_shot` 拿到**图像本身**与同期转录。\n\
             2. 亲自查看图像,并严格对照同期转录:转录明确说的信息(对象/规格/方向)必须服从;\n\
                画面独有的信息(形态/标注线/数值)如实读出,并注明这是从画面推断的。\n\
             3. 若转录与画面矛盾,写进 `issues`。\n\
             4. 有些图**压根不值得校准**:画面全黑或纯色(讲完话切黑、片尾淡出常出这种)、\n\
                与另一张是同一张幻灯片、或者糊到什么都看不出来。这类用 `update_shot` 的\n\
                `drop` 弃掉(它只标记不删文件),在汇报里说明为什么 —— **不要硬写结论**。\n\
                实测:16 分钟的视频里,末尾那次切黑就让算法白截了一张。\n\
             5. **发现转录写错了就改掉**(`apply_corrections`),不要只写进 `issues`。\n\
                校准是把材料改对,不是列一张待办清单 —— 用户要看的是改完的结果。\n\
                **审过、没问题的段一并报上来**(`reviewed`):不报等于没审,\n\
                工具分不出'看过没问题'和'压根没看'。\n\
             6. 用 `record_shot_check` 逐张存回结论。**不要臆测** —— 不确定就 `confidence: low`,或\n\
                在 `type_read` 里如实写「画面无法确认」。读不出来的地方**不要拿转录去补**。\n\
             7. 全部存完后调用 `render_timeline`,把返回的 url 给用户,让他自己看一遍。\n\
             \n\
             最后向用户汇报:多少张匹配、多少张存疑、哪几张需要他裁决。**不要替他下结论。**\n\
             有要他裁决的项时,进下一步会被工具挡住 —— 那时去问他;他说'就这样过',\n\
             把那句话放进 `approve` 的 `note`。他没说过的,不要自己替他点头。"
        ))
    }

    /// 逐段核对转录,把听错的改对
    #[prompt(name = "fix_transcript", description = "逐段核对转录:同音错字/术语纠错,对照表给用户过目")]
    async fn fix_transcript(
        &self,
        Parameters(args): Parameters<ProjectArg>,
    ) -> Result<Vec<PromptMessage>, ErrorData> {
        let project_id = args.project_id;
        user_says(format!(
            "你是转录校对助手。项目:`{project_id}`。这份视频的转录可能有语音识别错误\n\
             (同音错字、专有名词听错 —— 实测「Python」→「派森」、「达摩院」→「打墨院」),\n\
             请你逐段核对,**把错的改对**。\n\
             \n\
             1. 调用 `get_transcript` 读全量转录;用 `list_shots` / `get_shot` 看截图 ——\n\
                **画面上的文字是纠错的第一依据**(截图里常直接写着正确写法,如「Python 列表」),\n\
                上下文语义次之;用户若给了术语表,再拿它对照。\n\
             2. 用 `apply_corrections` 逐条写回,每条附 `reason`(写清依据:画面文字 / 上下文 / 术语表)。\n\
                - 只改明确的识别错误:**不润色、不改语序**,数字/单位/时间保留原样。\n\
                - 没把握的标 `uncertain`,**不要臆造**一个看起来像的写法 —— 那些留给用户裁决。\n\
                - **审过、没问题的段一并放进 `reviewed`**:不报等于没审,\n\
                  工具分不出「看过没问题」和「压根没看」;有没审的段,写总结时会被门挡住。\n\
             3. 写完调用 `render_calibration_table`,把返回的 url 给用户 —— 那一页能看到\n\
                每段的原文/改后/依据,以及还没核验的部分。\n\
             4. 把这一轮写进校准日志(`write_note` 到 `02_calibrate/calibration_log.md`,\n\
                mode `append`,带 `count_round`):第一行写 `## 第 N 轮(日期 时间段)`,\n\
                下面列这轮改了哪几段、依据是什么。**每轮都要写** —— 对照表把这份日志原样\n\
                嵌在末尾,用户就靠它分清每条结论是哪一轮写的;同一处被后轮改写时,\n\
                旧结论只在这份日志里。\n\
             5. **停下来**等用户看过对照表;他说哪里不对,那就是下一轮 —— 不要自己接着写总结。"
        ))
    }

    /// 找算法漏掉的画面,需要就补图
    #[prompt(name = "find_missing_shots", description = "列出可能缺图的位置,逐条判断后补截")]
    async fn find_missing_shots(
        &self,
        Parameters(args): Parameters<ProjectArg>,
    ) -> Result<Vec<PromptMessage>, ErrorData> {
        let project_id = args.project_id;
        user_says(format!(
            "项目:`{project_id}`。\n\
             \n\
             调用 `find_supplement_candidates` 取回候选补图请求 —— 算法挑的是\n\
             「语音里提到了画面上的东西、但那个时刻附近没有截图」的位置。\n\
             \n\
             **候选不等于结论**:用户说「看这里」不代表真有个图值得看。\n\
             **每一条都要有结论**,用 `disposition_supplements` 一次报一批:\n\
             要补的报 `need_shot`(工具去抽帧;那一刻附近已经有图就沿用,不重复抽),\n\
             不用补的报 `no_shot` —— 两种都要写一句理由。\n\
             `need_shot` 的图补完用 `get_shot` 看一眼,再核结论(`record_shot_check`)。\n\
             \n\
             剩下没结论的候选会被审核门挡住(它数的就是这个数),所以别留下没表态的。\n\
             \n\
             补完后调用 `render_timeline` 刷新,并把新增的图逐张给用户过一眼。"
        ))
    }

    /// 自主探索后产出图文总结
    #[prompt(name = "summarize_video", description = "读材料 + 自主看图探索,产出带溯源的图文总结")]
    async fn summarize_video(
        &self,
        Parameters(args): Parameters<ProjectArg>,
    ) -> Result<Vec<PromptMessage>, ErrorData> {
        let project_id = args.project_id;
        user_says(format!(
            "项目:`{project_id}`。你要为这个视频产出一份图文总结。\n\
             \n\
             1. 先读转录(`get_transcript`)与差帧图/密集区(`get_change_curve` / `list_regions`),\n\
                建立整体结构:分几段、每段在讲什么。\n\
             2. 对每个关键段,用 `get_shot` 看图 + 读同期转录,以**图文互证**得出结论。\n\
                只靠转录写出来的总结会把「样本 15 分钟」写成别的对象 —— 实测过。\n\
             3. 某处缺画面(语音提到某个形态/图表却没有图)时,先用 `search_transcript` 定位,\n\
                再用 `request_shot(t, reason)` 补看。\n\
             4. **每条结论都要附来源**:截图 id + 转录时间戳。没有来源的话就不要写。\n\
             5. 用 `write_summary` 落盘;中间笔记写 `write_note`(`03_summary/notes/…`)。\n\
             6. 写完**停下来**把总结呈现给用户,等他说改哪儿 —— 不要自己接着写报告。"
        ))
    }
}

/// 本服务注册的提示模板。与工具一样,是"服务会什么"的一部分。
///
/// 包一层是因为 `#[prompt_router]` 生成的那个关联函数默认是**模块私有**的
/// (它的 `vis` 属性缺省为空),别的模块调不到。
pub fn router() -> PromptRouter<VidbriefServer> {
    VidbriefServer::prompt_router()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 放出来的每个模板,引用的工具都必须在真工具表里存在。
    ///
    /// 这条测试是"不给 agent 递一张指向不存在工具的说明"这个决定的**执行者** ——
    /// 光靠人记,迟早会有人写个 P5 进来看不出问题。
    #[test]
    fn every_prompt_only_names_tools_that_exist() {
        let names = VidbriefServer::new().tool_names_for_test();
        let cases: [(&str, &[&str]); 4] = [
            (
                "calibrate_shots",
                &[
                    "list_shots",
                    "get_shot",
                    "record_shot_check",
                    "update_shot",
                    "apply_corrections",
                    "render_timeline",
                ],
            ),
            (
                "fix_transcript",
                &[
                    "get_transcript",
                    "list_shots",
                    "get_shot",
                    "apply_corrections",
                    "render_calibration_table",
                    "write_note",
                ],
            ),
            (
                "find_missing_shots",
                &["find_supplement_candidates", "extract_shot", "render_timeline"],
            ),
            (
                "summarize_video",
                &[
                    "get_transcript",
                    "get_change_curve",
                    "list_regions",
                    "get_shot",
                    "search_transcript",
                    "request_shot",
                    "write_summary",
                    "write_note",
                ],
            ),
        ];

        for (prompt, tools) in cases {
            for tool in tools {
                assert!(
                    names.iter().any(|n| n == tool),
                    "提示模板 {prompt} 提到了不存在的工具 {tool}"
                );
            }
        }
    }

    /// 没就绪的模板必须**真的**没注册 —— 免得有人顺手补上却忘了底下的工具还没有。
    #[test]
    fn prompts_waiting_on_later_milestones_are_not_registered() {
        let registered = router();
        for name in ["build_report"] {
            assert!(
                !registered.map.contains_key(name),
                "{name} 依赖的工具还没做出来(M6),不该现在就放出来"
            );
        }
        for name in [
            "calibrate_shots",
            "fix_transcript",
            "find_missing_shots",
            "summarize_video",
        ] {
            assert!(registered.map.contains_key(name), "缺提示模板 {name}");
        }
    }
}
