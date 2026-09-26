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
//! | P5 `build_report` | build_report(M6 报告工具) | ✅ 已就绪 |
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

// ── 说明文本:两个出口,一份维护 ─────────────────────────────────────
//
// MCP 的 prompts 是**可选能力**:宿主可以不实现 `prompts/list`。M5 真 agent 联调实测
// 手上的宿主就没实现 —— 四个提示模板一个都到不了 agent(`tools/guide.rs` 开头有全记录)。
// 所以同一份说明再开一个**工具**出口(`get_guide`);文本留在这一处,
// 两个出口都从函数拿。

/// P1 `calibrate_shots` 的说明。
pub(crate) fn calibrate_shots_text(project_id: &str) -> String {
    format!(
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
            `match` 判的是「**画面讲的与同期转录讲的是不是同一件事**」(主题 / 动作 / 对象) ——\n\
            **用字层面的差异不算不符**(转录把一个字听错了,那是转录纠错的活,走第 5 条):\n\
            两种读法都说得通的字段迟早被填成两个样,所以这里写死。\n\
         7. 全部存完后调用 `render_timeline`,把返回的 url 给用户,让他自己看一遍。\n\
         \n\
         最后向用户汇报:多少张匹配、多少张存疑、哪几张需要他裁决。**不要替他下结论。**\n\
         有要他裁决的项时,进下一步会被工具挡住 —— 那时去问他;他说'就这样过',\n\
         把那句话放进 `approve` 的 `note`。他没说过的,不要自己替他点头。"
    )
}

/// P2 `fix_transcript` 的说明。
pub(crate) fn fix_transcript_text(project_id: &str) -> String {
    format!(
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
    )
}

/// P3 `find_missing_shots` 的说明。
pub(crate) fn find_missing_shots_text(project_id: &str) -> String {
    format!(
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
    )
}

/// P4 `summarize_video` 的说明。
pub(crate) fn summarize_video_text(project_id: &str) -> String {
    format!(
        "项目:`{project_id}`。你要为这个视频产出一份图文总结。\n\
         \n\
         1. 先建整体结构,三样东西各有分工:\n\
            - `list_regions` 看画面在哪几段变化密集(那是内容段落的候选边界);\n\
            - `list_shots` 看有哪些截图、各落在什么时刻(有些可能已被校准阶段判为弃用);\n\
            - 转录(`get_transcript`)通读一遍;太长就按 `start`/`end` 分段读。\n\
            差帧图原始点(`get_change_curve`)不必读 —— 它的结论已经在密集区里了。\n\
            **通读时把讲者明确给出的步骤、框架、清单记下来**(「先做 A 再做 B」的流程、\n\
            「两种方式/四个方面」这类分点、列出来的清单)—— 那是他特意讲的,\n\
            总结里要写全;只有闲聊和过渡句可以略。实测:漏掉的多半是这些**框架**,\n\
            而它们是视频里信息密度最高的部分。\n\
         2. 对每个关键段,用 `get_shot` 把图拿到眼前看,和同期转录**互相对照**再下结论。\n\
            只靠转录写出来的总结会把「样本 15 分钟」写成别的对象 —— 实测过。\n\
            注意:`get_shot` 的返回里带着校准阶段存下的旧结论(`type_read` 等)——\n\
            那是**上一轮**写下的,可以参考,但你要自己看过图再引用,不要照抄。\n\
         3. 某处缺画面(语音提到某个形态/图表却没有图)时,先用 `search_transcript` 定位,\n\
            再用 `request_shot(t, reason)` 补看。搜索匹配的是当前文本:搜不到时换个词形\n\
            再试(英文原名 / 中文说法各试一次 —— 语音里说「列表」,转录里不会有「list」)。\n\
            **补来的图也要记一条结论**(`record_shot_check`):它对新图来说等于「还没人看过」,\
            不写的话写总结时门会拦。\n\
         4. **每条结论都要附来源**:截图 id + 转录时间戳。没有来源的话就不要写。\n\
             **画面上的文字与数字照抄下来**(函数名、版本号、比例、标注)——看不清就不写那个数,\n\
             别给一个「差不多」的。实测:不要求照抄时,模型会把画面上的 3.12.1 写成 3.21.1,\n\
             把画面里没有的 3.12 写成一个像是从图上读来的数 —— **结构对、数字错**,\n\
             而读总结的人没法分辨哪个数是抄来的、哪个是记得的。\n\
         5. 用 `write_summary` 落盘;中间笔记写 `write_note`(`03_summary/notes/…`)。\n\
            项目里已有 `03_summary/summary.md` 时,那是上一轮的总结 —— 先读旧稿,\n\
            用户的修改意见是在它之上提的。\n\
         6. 写完**停下来**把总结呈现给用户,等他说改哪儿 —— 不要自己接着报告。"
    )
}

/// P5 `build_report` 的说明。
pub(crate) fn build_report_text(project_id: &str) -> String {
    format!(
        "项目:`{project_id}`。你要为这个视频出一份**报告** —— 它和总结不是一回事:\n\
         总结是一篇连着读的文章,报告是**挂在时间轴上、边看视频边对照**的一页。\n\
         \n\
         1. 先看手上有什么。已经有 `04_report/report.json`(或 `report.md`)时,\n\
            那是上一轮的报告 —— **先读它**:用户的修改意见是在它之上提的,\n\
            那份数据里的 `style` 也是**这一轮的口味默认值**(用户没改口就照旧)。\n\
            全局总结不必重写:`build_report` 不给 `summary` 就用 `03_summary/summary.md`\n\
            那一份(步 3 的产物);只在报告要的总述与它不一样时才自己写。\n\
         2. **先把风格定下来**(`build_report` 的 `style`,必填)—— 它按**用户的话**填,\n\
            别自己默认一个。用户的提示词里带着他要什么,这一栏就是那个落点:\n\
            - **先取一次风格说明:`get_guide(step=\"styles\")`** —— 五种预置风格\n\
              (快扫版 / 教学版 / 故事版 / 元组版 / 汇报版)各自**怎么填**:条数怎么按长度换算、\n\
              每条按什么骨架写、给谁看、要不要挂图。挑一种,或按用户的要求拼一个 `custom`。\n\
            - `preset` 是**轮廓的名字**(上一步挑的那个);`granularity` 是这一版的粒度\n\
              (`brief` 概括 / `standard` 标准 / `detailed` 详细 —— **它同时决定条数与篇幅**);\n\
              `tone` 是语气(**自由文本**:大白话 / 诙谐 / 讲故事 / 行业腔……);\n\
              `notes` 放用户还有的交代(给谁看、要不要举例子、要不要结论先行)。\n\
            - 用户**没交代**风格时:照风格说明里那张表挑(拿不准用**教学版**),\n\
              并且**交付时说清这一版是按什么做的** —— 他才知道该不该让你换。\n\
         3. **定锚点。** 通读转录(`get_transcript`)与截图清单(`list_shots`),\n\
            挑出值得单独讲的那几处:一个概念的界定、一次图表的切换、一个案例怎么展开。\n\
            **视频里逐个讲的,报告里逐个成条**:一个个知识点、一个个步骤、一个个知识点,\n\
            每一个各占一条 —— 它被讲到的那一刻就是它的锚点,它自己的画面就是它的证据。\n\
            把六个案例合成一张表塞进一条,读的人在时间轴上点不到某一个案例,\n\
            在面板里也翻不到「这个案例讲了什么」—— 那是把内容埋了,不是省事。\n\
            每条给一个时刻(秒,落在视频长度之内)—— 时间轴上那个圆点就是它。\n\
            条数与每条的长度**按上面定的档位**(`brief` 就真的只留主脉,\n\
            `detailed` 也别堆空标题 —— 两头都不许:不堆空标题,也别把几条并成一条)。\n\
         4. 每条知识点写给「一边看视频一边读」的人(**说法按 `tone`**):\n\
            - `title` 一句话标题;`body` 是**完整解析**(markdown)—— 把这一处的上下文逻辑\n\
              理顺、关键概念讲清。**浮层里显示的就是 `body`**:只给标题等于没写。\n\
            - 不要写「见上文」「如前所述」这类回指 —— 读的人是跳着看的,每条要能自己站住。\n\
            - **听到指代词必须去图里解引用**。讲者说「这一段」「那一段」「这根线」时,\n\
            另一半信息在画面上 —— 只读转录会悬空。取那一刻的图,把指代写成具体范围\n\
            (「他说的『这一段』= 图上 7:45–9:30 的那段代码」);图上找不到对应物就\n\
            写清「指代不明」,并挂一条 `verify`。**别把「这一段」原样抄给读者** ——\n\
            那等于把疑问递下去。哪类话术算指代,见技能里的 `workflow.md` §0.1。\n\
            - `shots` 放**真正说明了这条**的截图(先看清单里的 id);右侧图文面板会把它们\n\
              显示出来。视频画面上已经有的东西不必再摆一遍。\n\
            - `segments` 放这条关联的转录段号(读的人可以点回原文)。\n\
              `verify` 放**待核验**的点(画面看不清但结论靠它的数字、一闪而过的标注……),\n\
              一条一句、写清**怎么核**(去哪个时刻看什么);没有就空着。\n\
            - **画面上的文字与数字照抄**(版本号、规格、比例):看不清就不写那个数,\n\
              别给一个「差不多」的 —— 与步 3 同一条,实测最常错的就是数字。\n\
              **但结论靠的那个数不许静默略过**:看不清就**连不确定一起写出来**\n\
              (「约 3.12(这个版本号画面看不清,待核验)」),并挂一条 `verify`\n\
              (「回看 8:05 确认版本号」)。略过它等于悄悄削弱结论,读的人看不出少了什么。\n\
         5. **每条都要有来源**,至少一条:\n\
            - 讲者原话:`speaker` + **照抄**的一句转录原文(`quote`)。工具拿它去转录里\n\
              找段号:**找不到就拒收**(自己报段号没用,工具不认)。别跨段拼。\n\
            - 外部补充:`external` + 一句话大意 + 能点开的链接。讲者提到的术语、对象、\n\
              机构、事件里**材料没说清**的,联网查一下补上,读的人能点回出处。\n\
              **查不到就别编** —— 链接点不开会被工具拒收。\n\
         6. 一次 `build_report` 把数据与页面一起出了(`topics` 一次给全,工具按时刻排)。\n\
            返回里的 `url` 给用户:左边他看视频、时间轴上的圆点是知识点,右边是可滑出的\n\
            图文总结面板。重做就是再调一次 —— **整份替换**。\n\
         7. **写完停下来**把页面给用户,问他要改哪儿 —— 顺带说一句**这一版是按什么做的**\n\
            (返回里的 `style`),以及**有几处待核验、是哪几处**(返回里的 `verify`,\n\
            页面标题栏也标着);他提了意见就带着旧数据再调一次。\n\
            没有总结(`write_summary`)、或者总结被绕过工具改过,这道门会挡住你 ——\n\
            那时先把总结落一遍,别绕。"
    )
}

/// 报告的**风格轮廓** —— 直接把 `skill/styles.md` 原样发出去。
///
/// 为什么用 `include_str!` 而不是另写一份:同一份手艺有两个出口 ——
/// 装了技能目录的宿主读 `skill/styles.md`,只认协议的宿主走 `get_guide(step="styles")`。
/// 抄两份迟早各说各话(而「怎么写」恰恰是最常改的那一样)。
pub(crate) fn styles_text() -> &'static str {
    include_str!("../../../skill/styles.md")
}

/// `get_guide` 的总目录:有哪几步、各能拿到什么。
pub(crate) fn guide_index(project_id: &str) -> String {
    format!(
        "vidbrief 的四步工作流,每一步开工前用 `get_guide(step=…)` 取详细说明。\n\
         项目:`{project_id}`。\n\
         \n\
         - `preprocess` —— 前处理:`preprocess` 一次调用做完(纯算法);细节见服务的\n\
           instructions。\n\
         - `calibrate_shots` —— 逐张看截图:list_shots → get_shot 看图 → record_shot_check。\n\
         - `fix_transcript` —— 逐段核对转录(`get_transcript`):画面文字是第一依据,改完用\n\
           `apply_corrections` 报回(审过没问题的段也要报)。\n\
         - `find_missing_shots` —— 补图候选逐条表态:`find_supplement_candidates` →\n\
           `disposition_supplements`(补的报 need_shot、不补的报 no_shot)。\n\
         - `summarize_video` —— 图文总结:`list_regions` + `list_shots` + 转录通读建结构,\n\
           逐张看图互证,溯源(截图 id + 时间戳),`write_summary` 落盘。\n\
         - `build_report` —— 报告(步 4 的产出):把总结与材料整理成**每条挂在时刻上的\n\
           知识点**(完整解析 + 关联截图/转录段 + 至少一条来源),**视频里逐个讲的\n\
           内容逐个成条**(一个个案例/步骤/样本各占一条,别合成一张表);\n\
           **先交代风格**(`style`:轮廓 preset + 粒度 + 语气 + 补充要求),\n\
           一次 `build_report` 落数据并出页面(时间轴锚点 + 浮动解析 + 图文总结面板,\n\
           可回跳视频)。\n\
         - `styles` —— **报告的风格轮廓**:五种预置风格(快扫 / 教学 / 故事 / 元组 / 汇报)\n\
           各自怎么填 —— 条数怎么按视频长度换算、每条按什么骨架写、给谁看、要不要挂图,\n\
           以及怎么按用户的话挑一种。报告**动笔前先取这一份**。\n\
         \n\
         说明是给「手上没有提示模板能力」的宿主用的;能取到 prompts 的话,那边是同一份文本。"
    )
}

#[prompt_router]
impl VidbriefServer {
    /// 逐张检查关键截图:图文对不对得上、音画同不同步
    #[prompt(name = "calibrate_shots", description = "逐张检查关键截图:图文匹配 + 音画同步,结论写回 shots.json")]
    async fn calibrate_shots(
        &self,
        Parameters(args): Parameters<ProjectArg>,
    ) -> Result<Vec<PromptMessage>, ErrorData> {
        user_says(calibrate_shots_text(&args.project_id))
    }

    /// 逐段核对转录,把听错的改对
    #[prompt(name = "fix_transcript", description = "逐段核对转录:同音错字/术语纠错,对照表给用户过目")]
    async fn fix_transcript(
        &self,
        Parameters(args): Parameters<ProjectArg>,
    ) -> Result<Vec<PromptMessage>, ErrorData> {
        user_says(fix_transcript_text(&args.project_id))
    }

    /// 找算法漏掉的画面,需要就补图
    #[prompt(name = "find_missing_shots", description = "列出可能缺图的位置,逐条判断后补截")]
    async fn find_missing_shots(
        &self,
        Parameters(args): Parameters<ProjectArg>,
    ) -> Result<Vec<PromptMessage>, ErrorData> {
        user_says(find_missing_shots_text(&args.project_id))
    }

    /// 自主探索后产出图文总结
    #[prompt(name = "summarize_video", description = "读材料 + 自主看图探索,产出带溯源的图文总结")]
    async fn summarize_video(
        &self,
        Parameters(args): Parameters<ProjectArg>,
    ) -> Result<Vec<PromptMessage>, ErrorData> {
        user_says(summarize_video_text(&args.project_id))
    }

    /// 把总结与材料整理成挂在时间轴上的知识点,出报告页
    ///
    /// (函数名带 `_guide` 是为了避开同名工具 `build_report` 生成的方法 ——
    /// 对协议暴露的名字由 `name` 定,仍然是 `build_report`。)
    #[prompt(
        name = "build_report",
        description = "出报告:每条知识点挂在一个时刻上(完整解析 + 来源),落数据并渲染页面"
    )]
    async fn build_report_guide(
        &self,
        Parameters(args): Parameters<ProjectArg>,
    ) -> Result<Vec<PromptMessage>, ErrorData> {
        user_says(build_report_text(&args.project_id))
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
    /// 光靠人记,迟早会有人写一个新模板进来看不出问题(P5 就是这么等到 M6 的)。
    #[test]
    fn every_prompt_only_names_tools_that_exist() {
        let names = VidbriefServer::new().tool_names_for_test();
        let cases: [(&str, &[&str]); 5] = [
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
                    "list_regions",
                    "list_shots",
                    "get_shot",
                    "search_transcript",
                    "request_shot",
                    "record_shot_check",
                    "write_summary",
                    "write_note",
                ],
            ),
            (
                "build_report",
                &[
                    "build_report",
                    "get_transcript",
                    "list_shots",
                    "write_summary",
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

    /// 四步 + 报告的模板**全都在**,一步不落。
    ///
    /// (以前这里有一条"没就绪的不许注册"的检查 —— P5 到 M6 才放出来,
    /// 它盯着那件事。现在五步齐了,同一处改成正面断言:缺了谁都不行。)
    #[test]
    fn every_prompt_is_registered() {
        let registered = router();
        for name in [
            "calibrate_shots",
            "fix_transcript",
            "find_missing_shots",
            "summarize_video",
            "build_report",
        ] {
            assert!(registered.map.contains_key(name), "缺提示模板 {name}");
        }
    }

    /// `get_guide` 的目录是 agent 拿到说明的第一入口 —— 它点名的工具都得在,
    /// 而且四步一步不落(少了哪步,没 prompts 能力的宿主就永远不知道有那么一步)。
    #[test]
    fn guide_index_covers_every_step_and_names_real_tools() {
        let names = VidbriefServer::new().tool_names_for_test();
        let text = guide_index("示例项目");
        for (step, tools) in [
            ("preprocess", &["preprocess"][..]),
            (
                "calibrate_shots",
                &["list_shots", "get_shot", "record_shot_check"][..],
            ),
            ("fix_transcript", &["get_transcript", "apply_corrections"][..]),
            (
                "find_missing_shots",
                &["find_supplement_candidates", "disposition_supplements"][..],
            ),
            (
                "summarize_video",
                &["list_regions", "list_shots", "get_transcript", "write_summary"][..],
            ),
            (
                "build_report",
                &["get_transcript", "list_shots", "build_report", "write_summary"][..],
            ),
        ] {
            assert!(text.contains(step), "目录缺这一步 {step}:{text}");
            for tool in tools {
                assert!(text.contains(tool), "{step} 那段该提到 {tool}:{text}");
                assert!(
                    names.iter().any(|n| n == tool),
                    "目录提到了不存在的工具 {tool}"
                );
            }
        }
        assert!(text.contains("示例项目"), "项目名该填进说明里:{text}");
    }

    /// 一个个案例挤成一张表 = 内容被埋掉(实测提的):这条口径必须落在模板正文里,
    /// 光写在文档里没用 —— agent 读的是模板。
    #[test]
    fn build_report_guide_asks_for_one_topic_per_case() {
        let text = build_report_text("示例项目");
        assert!(text.contains("逐个成条"), "P5 该交代逐个成条:{text}");
        assert!(text.contains("各占一条"), "P5 该交代各占一条:{text}");
        assert!(!text.contains("宁可少而实"), "旧的「少而实」措辞会盖掉新口径:{text}");
    }

    /// **风格文档与代码不许各说各话**:`skill/styles.md` 是轮廓的定义处,
    /// 代码里那组预设名是用户能点的把手 —— 文档里少一种、或者名字拼错,
    /// 用户说「换成快扫版」时模型就找不到对应物。
    #[test]
    fn every_preset_in_code_is_spelled_out_in_the_styles_skill() {
        let text = styles_text();
        for preset in vidbrief_project::ReportPreset::ALL {
            assert!(
                text.contains(preset.as_str()),
                "skill/styles.md 里没写 `{}`(`{}`)—— 用户点这个名字时,模型得找得到定义",
                preset.as_str(),
                preset.label()
            );
            assert!(
                text.contains(&format!("### {} ", preset.label())),
                "skill/styles.md 缺 `{}` 这一节(只出现名字不算定义)",
                preset.label()
            );
        }
    }

    /// 风格说明不是空壳:它得真讲「怎么填」(条数换算 + 每条骨架),否则等于没写。
    #[test]
    fn the_styles_skill_says_how_to_fill_not_only_what_to_call_it() {
        let text = styles_text();
        for must in ["每 10 分钟", "结论先行", "骨架", "granularity", "preset"] {
            assert!(text.contains(must), "skill/styles.md 该讲到 `{must}`");
        }
    }

    /// 报告模板要把风格说明指出来 —— 光有 `get_guide(step="styles")` 这一个入口不够,
    /// 动笔前那一步得有人提。
    #[test]
    fn the_report_guide_points_at_the_styles_guide() {
        let text = build_report_text("示例项目");
        assert!(text.contains("get_guide(step=\"styles\")"), "{text}");
        assert!(guide_index("示例项目").contains("`styles`"), "{text}");
    }
}
