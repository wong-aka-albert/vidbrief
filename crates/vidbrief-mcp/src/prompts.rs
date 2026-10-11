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
//! | P1 `calibrate_shots` | list_shots / get_shot / record_shot_check / render_workbench | ✅ 已就绪 |
//! | P2 `fix_transcript` | apply_corrections / render_workbench | ✅ 已就绪 |
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
            画面独有的信息(结构/标注线/数值)如实读出,并注明这是从画面推断的。\n\
         3. 若转录与画面矛盾,写进 `issues`。\n\
         4. 有些图**压根不值得校准**:画面全黑或纯色(讲完话切黑、片尾淡出常出这种)、\n\
            与另一张是同一张幻灯片、或者糊到什么都看不出来。这类用 `update_shot` 的\n\
            `drop` 弃掉(它只标记不删文件),在汇报里说明为什么 —— **不要硬写结论**。\n\
            实测:16 分钟的视频里,末尾那次切黑就让算法白截了一张。\n\
         5. **发现转录写错了就改掉**(`apply_corrections`),不要只写进 `issues`。\n\
            校准是把材料改对,不是列一张待办清单 —— 用户要看的是改完的结果。\n\
            **审过、没问题的段一并报上来**(`reviewed` 逐段点名;一整片没问题就用\n\
            `reviewed_ranges` 报闭区间):不报等于没审,工具分不出'看过没问题'和'压根没看'。\n\
         6. 用 `record_shot_check` 逐张存回结论。**不要臆测** —— 不确定就 `confidence: low`,或\n\
            在 `type_read` 里如实写「画面无法确认」。读不出来的地方**不要拿转录去补**。\n\
            `match` 判的是「**画面讲的与同期转录讲的是不是同一件事**」(主题 / 动作 / 对象) ——\n\
            **用字层面的差异不算不符**(转录把一个字听错了,那是转录纠错的活,走第 5 条):\n\
            两种读法都说得通的字段迟早被填成两个样,所以这里写死。\n\
         7. 全部存完后调用 `render_workbench`,把返回的 url 给用户,让他自己看一遍 ——
            一个页面看全部:轴上点一下就展开那一条(图 + 你的结论),右边抽屉切三块。\n\
         \n\
         最后向用户汇报:多少张匹配、多少张存疑、哪几张需要他裁决。**不要替他下结论。**\n\
         有要他裁决的项时,进下一步会被工具挡住 —— 那时去问他;他说'就这样过',\n\
         把那句话放进 `approve` 的 `note`。他没说过的,不要自己替他点头。"
    )
}

/// P2 `fix_transcript` 的说明。
pub(crate) fn fix_transcript_text(project_id: &str) -> String {
    format!(
        "你是转录校对助手。项目:`{project_id}`。这份视频的转录有两件事要做:\n\
         **把听错的字改对**,和**把断句加上**(ASR 的输出几乎不带标点 —— 五份材料实测\n\
         4761 段里只有 36 段带句号,读起来是一串没有逗号句号的字)。\n\
         转录是**按组**给的 —— 一组是一口气说完的一小段\n\
         (whisper 按停顿把一句话切得很碎,`get_transcript` 默认给的就是这个形状);\n\
         已经定位到某一句、要把它改掉时,用 `grouped: false` 只看那一小段。\n\
         \n\
         1. 调用 `get_transcript` 读转录;用 `list_shots` / `get_shot` 看截图 ——\n\
            **画面上的文字是纠错的第一依据**(截图里常直接写着正确写法,如「Python 列表」),\n\
            上下文语义次之;用户若给了术语表,再拿它对照。\n\
            **要通读整份**(加断句就得通读),直接读项目里那份 `01_preprocess/transcript.read.md`:\n\
            一行一段(段号 · 时刻 · 现读法),组头带核验状态 —— 它是给人读的投影,每次写结论时重生成。\n\
            **不要去读 / grep `01_preprocess/transcript.json`**:那是原始材料,JSON 结构壳比正文\n\
            贵四五倍(实测一份 5.9k 字的转录,分页读 JSON 花了 153k 字符),而且读到的不是现读法。\n\
         2. 用 `apply_corrections` 逐条写回。**四种结论各自要带什么,一次说清**:\n\
            - `corrected`(改了字)—— 必给 `text`(**改后的整句**,不是只写改动的那个词,\n\
              那是 `note` / `reason` 里的东西);`type` 选填;\n\
            - `punctuated`(**只加标点 / 断句,字一个不动**)—— 必给 `text`\n\
              (**加了标点之后的整段**)。断句是**读懂了才断得对**的活:一句说完给「。」,\n\
              话没说完给「，」,别每段都点上句号、也别一段里塞一串逗号。\n\
              **去掉标点与空白之后必须跟「现读法」一字不差**(有纠错就照纠错后那一版 ——\n\
              照原文加会把你刚改好的字退回去);顺手改了个字会被拒(那是 `corrected`);\n\
              一字没加的会被折成「看过、没问题」(不报错 —— 但那一段不如直接报进 `reviewed`);\n\
              这一档**不用给** `note` / `reason`;\n\
            - `kept`(**这一段按原文算**:原文没问题不用改,**也包括「撤回前面落的改动、退回原文」**)\
              —— 必给 `note`(为什么);撤完还想补标点,再单独报一条 `punctuated` —— 那时现读法已经回到原文了;\n\
            - `uncertain`(定不了、留给用户裁决)—— 必给 `note`(疑似是什么写法),\n\
              **不要臆造**一个看起来像的写法;\n\
            - 除 `punctuated` 外都要给 `reason`(依据:画面文字 / 上下文 / 术语表);\n\
            - **同一段只能有一条结论** —— 一段里既改了字又加了标点,报一条 `corrected` 就够,\n\
              把标点一起写进那个 `text`;\n\
              `text` 跟原文一字不差会被拒(那不叫改,想保留原句、**或把这一段退回原文**,都换 `kept`),\n\
              跟原文只差标点也会被拒(那一档叫 `punctuated`);\n\
            - 改字只改明确的识别错误:**不润色、不改语序**,数字/单位/时间保留原样;\n\
            - **审过、没问题的段一并放进 `reviewed`**(一整片没问题就用 `reviewed_ranges`\n\
              报闭区间,如一条 from 1 to 300):不报等于没审,\n\
              工具分不出「看过没问题」和「压根没看」;有没审的段,写总结时会被门挡住。\n\
            - 一批里所有毛病工具会**一次全报**给你(不是一条一条蹦);**没毛病的条目照落**,\n\
              只有出问题的那几条会被退回 —— 改完**只把退回的那几条**再交一次,别整批重发。\n\
         3. 写完调用 `render_workbench`,把返回的 url 给用户 —— 抽屉里那块「转录」能看到\n\
            每段的原文/现读法/依据,以及还没核验的部分。\n\
         4. 把这一轮写进校准日志(`write_note` 到 `02_calibrate/calibration_log.md`,\n\
            mode `append`,带 `count_round`):第一行写 `## 第 N 轮(日期 时间段)`,\n\
            下面列这轮改了哪几段、依据是什么(断句整批加过一遍的话,写一句「全稿断句」就够,\n\
            不必逐段列)。**每轮都要写** —— 这份日志给下一轮和审计看\n\
            (页面上不显示它:让读者拿一段手写的轮次时段去对每一行的时刻,是对不上账的;\n\
            每条结论自己带着写下的时刻)。\n\
         5. **停下来**等用户看过;他说哪里不对,那就是下一轮 —— 不要自己接着写总结。"
    )
}

/// P3 `find_missing_shots` 的说明。
pub(crate) fn find_missing_shots_text(project_id: &str) -> String {
    format!(
        "项目:`{project_id}`。\n\
         \n\
         调用 `find_supplement_candidates` 取回候选补图请求 —— 算法挑的是\n\
         「语音里提到了画面上的东西」(一整组话里含指代)的位置。\n\
         返回里**只有还没表态的那些**;已经处置过的留在项目文件里,不用你再过一遍。\n\
         \n\
         **候选不等于结论**:用户说「看这里」不代表真有个图值得看。\n\
         **每一条都要有结论**,用 `disposition_supplements` 一次报一批:\n\
         要补的报 `need_shot`(工具去抽帧;那一刻附近已经有图就沿用,不重复抽),\n\
         不用补的报 `no_shot` —— 两种都要写一句理由。\n\
         `need_shot` 的图补完用 `get_shot` 看一眼,再核结论(`record_shot_check`)。\n\
         \n\
         剩下没结论的候选会被审核门挡住(它数的就是这个数),所以别留下没表态的。\n\
         \n\
         补完后调用 `render_workbench` 刷新,并把新增的图逐张给用户过一眼。"
    )
}

/// P4 `summarize_video` 的说明。
pub(crate) fn summarize_video_text(project_id: &str) -> String {
    format!(
        "项目:`{project_id}`。你要为这个视频产出图文总结 —— **一段一段写,写完一段落一段**。
         
         1. 先建整体结构,三样东西各有分工:
            - `list_regions` 看画面在哪几段变化密集(那是内容段落的候选边界);
            - `list_shots` 看有哪些截图、各落在什么时刻(有些可能已被校准阶段判为弃用);
            - 转录(`get_transcript`)通读一遍;太长就按 `start`/`end` 分段读。
            差帧图原始点(`get_change_curve`)不必读 —— 它的结论已经在密集区里了。
            **通读时把讲者明确给出的步骤、框架、清单记下来**(「先做 A 再做 B」的流程、
            「两种方式/四个方面」这类分点、列出来的清单)—— 那是他特意讲的,总结里要写全;
            只有闲聊和过渡句可以略。实测:漏掉的多半是这些**框架**,而它们是信息密度最高的部分。
         2. 对每个关键段,用 `get_shot` 把图拿到眼前看,和同期转录**互相对照**再下结论。
            只靠转录写出来的总结会把听到的术语写成别的东西 —— 实测过。
            注意:`get_shot` 的返回里带着校准阶段存下的旧结论(`type_read` 等)——
            那是别人写下的,可以参考,但你要自己看过图再引用,不要照抄。
         3. 某处缺画面(语音提到某个结构/图表却没有图)时,先用 `search_transcript` 定位,
            再用 `request_shot(t, reason)` 补看。搜索匹配的是当前文本:搜不到时换个词形
            再试(英文原名 / 中文说法各试一次 —— 语音里说「列表」,转录里不会有「list」)。
            **补来的图也要记一条结论**(`record_shot_check`):它对新图来说等于「还没人看过」。
         4. **一段一段交**(`write_summary`,一次一段;别攒着一次交整篇):
            - 每一段 = 视频里**值得单独讲的一处**:一个概念的界定、一次图表的切换、
              一个案例怎么展开。**视频里逐个讲的,就逐个成段** —— 六个知识点 = 六段,
              挤成一段的话,读的人在时间轴上点不到某一个,在面板里也翻不到「这个案例讲了什么」
              —— 那是把内容埋了,不是省事。
            - 每段给三样:`t`(这一处讲的是视频里哪一刻,秒)、`title`(一句话标题)、
              `md`(正文,markdown)。`t` 决定报告页上那个圆点的位置。
            - 正文写给「一边看视频一边读」的人(说法按用户要的口味):把这一处的上下文逻辑
              理顺、关键概念讲清。不要写「见上文」「如前所述」这类回指 —— 读的人是跳着看的。
            - **听到指代词必须去图里解引用**。讲者说「这一段」「那一段」「这根线」时,
              另一半信息在画面上 —— 只读转录会悬空。取那一刻的图,把指代写成具体范围
              (「他说的『这一段』= 图上 7:45–9:30 的那段代码」);图上找不到对应物就写清
              「指代不明」,并把它挂进这一段的 `verify`。**别把「这一段」原样抄给读者**。
            - **画面上的文字与数字照抄**(函数名、版本号、比例、标注):看不清就不写那个数,
              别给一个「差不多」的。实测:不要求照抄时,模型会把 3.12.1 写成 3.21.1 ——
              **结构对、数字错**,而读的人没法分辨哪个数是抄来的、哪个是记得的。
              **但结论靠的那个数不许静默略过**:看不清就**连不确定一起写出来**
              (「约 3.12(这个版本号画面看不清,待核验)」),并把它挂进这一段的 `verify`
              (写清怎么核:去哪个时刻看什么)。
            - **最后单独交一条总述**:不带 `t` 的那一段(整支视频讲了什么、结构如何)——
              它进页面右侧面板的开头,不上时间轴。
            - 改哪段就再交一次**同名**的那段(同名 = 改写,顺序不动)。
         5. 中间笔记写 `write_note`(`03_summary/notes/…`)。项目里已有旧稿时**先读旧稿**
            (`03_summary/summary.md`)—— 用户的修改意见是在它之上提的。
         6. 写完**停下来**把总结呈现给用户,等他说改哪儿 —— 不要自己接着报告。"
    )
}

/// P5 `build_report` 的说明。
pub(crate) fn build_report_text(project_id: &str) -> String {
    format!(
        "项目:`{project_id}`。出报告:把步 3 写下的那些段摆成**能对着视频看**的一页。
         
         **这个工具不用你交内容** —— 报告页上每一条(标题、正文、圆点位置)就是步 3 交过的段;
         那一刻讲者说了什么、画面上是什么,工具自己就近挂上。你要交的只有**口味**。
         
         1. 先看手上有什么:
            - `list_shots` / `get_project` 扫一眼材料齐不齐;
            - 已经有 `04_report/report.json` 时,那是上一轮的报告 —— **读它**:
              用户的修改意见是在它之上提的,那份数据里的 `style` 是**这一轮的口味默认值**。
            - 段不够、或某一处讲得不对?**回步 3 改**(`write_summary` 同名再交一次那段),
              别在这里重写内容 —— 这里没有那个入口。
         2. **把风格定下来**(`build_report` 的 `style`,必填)—— 它按**用户的话**填,
            别自己默认一个。**先取一次风格说明:`get_guide(step=\"styles\")`** ——
            五种预置风格(快扫版 / 教学版 / 故事版 / 复盘版 / 汇报版)各自怎么填。
            - `preset` 是轮廓的名字;`granularity` 是这一版的粒度说明
              (`brief` / `standard` / `detailed` —— **条数已经由步 3 的段定下了**,这里只是交代);
              `tone` 是语气(自由文本:大白话 / 诙谐 / 讲故事 / 行业腔……);
              `notes` 放用户还有的交代(给谁看、要不要举例子、要不要结论先行)。
            - 用户**没交代**风格时:照风格说明里那张表挑(拿不准用**教学版**),
              并且**交付时说清这一版是按什么做的** —— 他才知道该不该让你换。
         3. 调 `build_report(project_id, style)` —— 数据与页面一次出。返回里的 `url` 给用户:
            左边他看视频、时间轴上的圆点是段、点开是那一段的解析,右边是可滑出的图文总结面板。
            重做就是再调一次 —— **整份替换**。
         4. **写完停下来**把页面给用户,问他要改哪儿 —— 顺带说一句**这一版是按什么做的**
            (返回里的 `style`),以及**有几处待核验、是哪几处**(页面标题栏标着)。
            他要改内容就回步 3 改段、再调一次;要换说法就带着新口味再调一次。
            没有总结(`write_summary`)、或者总结被绕过工具改过,这道门会挡住你 ——
            那时先把段落齐,别绕。"
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
         - `build_report` —— 报告(步 4 的产出):**不用交内容** —— 内容就是步 3 交过的段\n\
           (**视频里逐个讲的内容各自成条**:一个个案例/步骤/样本各占一条),\n\
           工具把它们组装成**每条挂在时刻上的知识点**,那一刻的转录与画面就近挂上;\n\
           **先交代风格**(`style`:轮廓 preset + 粒度 + 语气 + 补充要求),\n\
           一次 `build_report` 落数据并出页面(`04_report/report.html`,纯图文、能单独分享;\n\
           工作台那一页的抽屉里也能读到同一份正文)。\n\
         - `styles` —— **报告的风格轮廓**:五种预置风格(快扫 / 教学 / 故事 / 复盘 / 汇报)\n\
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
        description = "出报告:把步 3 那些段摆成能对着视频看的一页(只交代口味),落数据并渲染页面"
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
                    "render_workbench",
                ],
            ),
            (
                "fix_transcript",
                &[
                    "get_transcript",
                    "list_shots",
                    "get_shot",
                    "apply_corrections",
                    "render_workbench",
                    "write_note",
                ],
            ),
            (
                "find_missing_shots",
                &["find_supplement_candidates", "extract_shot", "render_workbench"],
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

    /// 一个个案例挤成一段 = 内容被埋掉(实测提的):这条口径必须落在模板正文里,
    /// 光写在文档里没用 —— agent 读的是模板。
    ///
    /// 2026-09-27 起这条口径在 **P4(写总结)** 里:段是那时候切的,报告只是把段摆出来。
    #[test]
    fn the_summary_guide_asks_for_one_section_per_case() {
        let text = summarize_video_text("示例项目");
        assert!(text.contains("逐个成段"), "P4 该交代逐个成段:{text}");
        assert!(text.contains("一段一段交"), "P4 该交代分段提交:{text}");
        assert!(text.contains("不带 `t`"), "P4 该交代总述那一段:{text}");

        // 报告那一段不再切内容:它的活是把段摆到页面上
        let report = build_report_text("示例项目");
        assert!(report.contains("不用你交内容"), "P5 该说清渲染不要内容:{report}");
        assert!(
            !report.contains("topics"),
            "P5 不该再提那包结构化数据(它已经不存在了):{report}"
        );
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
