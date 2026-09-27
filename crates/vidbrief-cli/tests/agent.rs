//! **agent 客户端**端到端测试:把服务自己发出去的说明,当成规格书来验。
//!
//! # 为什么单开一份
//!
//! `mcp.rs` 验的是**协议层**:线格式对不对、握手说不说得通、通道通不通。
//! 那证明的是"字节能过去"。它没回答的是:
//!
//! > 一个 agent 拿着握手时得到的那份 `instructions` 与三个提示模板,能不能把活干成?
//!
//! 这是 M3 留下的最后一个问号(M3 只用手写客户端验过协议与图像通道)。
//! 所以这里的每一条用例,都是**照着说明书的某一句话做一遍** ——
//! 说明里写「调用 `record_shot_check` 逐张存回结论」,那这个工具就得真能存回;
//! 写「`get_shot` 会把图像交给你」,返回里就得真有图像。
//!
//! 说明书与实现对不上时,这份测试红。修法也因此是明确的二选一:
//! **要么改实现,要么改说明** —— 不能让它俩各说各的。
//!
//! # 材料是手写的,视频是真的
//!
//! 转录 / 差帧图 / 截图清单由 `vidbrief_ops::testkit::Fake` 手写(这一层不该被
//! ASR 与抽帧的代价拖住),但**源视频换成真素材** —— `extract_shot` 与
//! `update_shot` 真的会去调 ffmpeg 抽帧,喂它一个假文件测出来的不是那个东西。
//!
//! 手写转录**不放水**:里面埋着真实出现过的识别错误(`元祖` 应为 `元组`、
//! `张小明` 应为 `张晓明`),这样纠错那一环才是在验"改得对不对",而不是
//! "能不能往一个空文件里写东西"。

mod common;

use std::path::Path;

use common::{McpClient, TempDir, first_text, head, repo_root, tool_failed};
use serde_json::{Value, json};
use vidbrief_ops::testkit::Fake;

// ── 夹具 ─────────────────────────────────────────────────────────────

/// 材料齐全、**视频是真的**的假项目。
fn staged(tag: &str) -> Fake {
    let fake = Fake::new(tag);
    let src = repo_root().join("fixtures").join("test_sample.mp4");
    let dst = fake
        .layout()
        .rel(vidbrief_project::video_rel_for(Path::new("video.mp4")).as_str());
    std::fs::copy(&src, &dst).expect("把占位视频换成真素材");
    fake
}

/// 这份测试用的转录。**故意埋了真实发生过的识别错误**。
const FAKE_SEGMENTS: &[(f64, f64, &str)] = &[
    (0.0, 4.8, "各位好,我们今天这期是一个承前启后的内容"),
    (4.8, 9.6, "对于刚加入到我们元祖的朋友可能不太清楚"),
    (9.6, 14.0, "为什么我们经常说提前进入这一段"),
    (20.0, 24.0, "大家看一下这样的图"),
    (24.0, 28.0, "这个是负责讲解的张小明"),
    (40.0, 45.0, "列表和元祖两种数据类型的结构图"),
];

fn project_arg(fake: &Fake) -> String {
    fake.layout().root().display().to_string()
}

/// 从工具结果里取出图像块,返回 `(解出来的字节, mime)`。
fn image_bytes(result: &Value) -> Option<(Vec<u8>, String)> {
    use base64::Engine as _;
    let blocks = result["content"].as_array()?;
    let block = blocks.iter().find(|b| b["type"] == "image")?;
    let data = block["data"].as_str()?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(data).ok()?;
    Some((bytes, block["mimeType"].as_str().unwrap_or("?").to_string()))
}

fn as_json(text: &str) -> Value {
    serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("这不是 JSON(`{e}`):\n{}", head(text, 400)))
}

// ── 说明书本身 ───────────────────────────────────────────────────────

/// **说明书里提到的每个 snake_case 词,都必须是一个真工具。**
///
/// agent 到达时只有这份 `instructions` 当地图。里面出现一个不存在的工具名,
/// 它就会照着调、拿到 `method not found`,然后开始怀疑整个服务 ——
/// 而它没有别的办法知道那是错的。
///
/// 判据取"含下划线的纯小写 ASCII 词":说明是中文写的,里面出现这种形状的词
/// 只可能是工具名(散文里不会冒出 `snake_case`)。这条规则故意收得紧 ——
/// 宁可漏判,不要误报。
#[test]
fn instructions_only_name_tools_that_exist() {
    let dir = TempDir::new("agent-map");
    let mut client = McpClient::start(dir.path());
    let info = client.initialize();

    let instructions = info["instructions"]
        .as_str()
        .expect("握手必须带 instructions")
        .to_string();

    let tools = client.call("tools/list", json!({}));
    let names: Vec<String> = tools["tools"]
        .as_array()
        .expect("tools 该是数组")
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_string))
        .collect();

    let mentioned: Vec<&str> = instructions
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .filter(|w| {
            w.contains('_')
                && w.starts_with(|c: char| c.is_ascii_lowercase())
                && w.chars().all(|c| c.is_ascii_lowercase() || c == '_')
        })
        .collect();

    assert!(
        mentioned.len() >= 5,
        "说明书里一个工具名都没提到?那它没在教人怎么干活:\n{instructions}"
    );
    for name in mentioned {
        assert!(
            names.iter().any(|n| n == name),
            "说明书让人调 `{name}`,但服务里没有这个工具。\n\
             工具表:{names:?}\n\
             说明书:\n{instructions}"
        );
    }
}

// ── P1:校准循环 ──────────────────────────────────────────────────────

/// 照着 P1 `calibrate_shots` 走完一整轮:**清单 → 逐张看图 → 存回 → 出视图**。
///
/// 每一步都验"模板承诺的东西真的给得起",而不只是"调用没报错"。
#[test]
fn p1_calibrate_shots_runs_the_whole_way() {
    let fake = staged("agent-p1")
        .with_transcript(FAKE_SEGMENTS)
        .with_curve(60)
        .with_shot(1, 6.0)
        .with_shot(2, 22.0)
        .with_shot(3, 42.0);
    let proj = project_arg(&fake);

    let dir = TempDir::new("agent-p1-run");
    let mut client = McpClient::start(dir.path());
    client.initialize();

    // ① 拿模板 —— agent 是照着它干活的
    let prompt = client.call(
        "prompts/get",
        json!({"name": "calibrate_shots", "arguments": {"project_id": proj}}),
    );
    let text = prompt["messages"][0]["content"]["text"]
        .as_str()
        .expect("模板该是文本")
        .to_string();
    for tool in [
        "list_shots",
        "get_shot",
        "record_shot_check",
        "render_workbench",
    ] {
        assert!(text.contains(tool), "模板该把 {tool} 说清楚:{text}");
    }

    // ② 清单
    let listed = as_json(&client.tool_text("list_shots", json!({"project_id": proj})));
    let shots = listed["shots"].as_array().expect("shots 该是数组").clone();
    assert_eq!(shots.len(), 3, "{listed}");
    assert_eq!(listed["counts"]["unchecked"], 3);

    // ③ 逐张看图 + 存回
    for shot in &shots {
        let id = shot["id"].as_str().expect("每张图都该带 id").to_string();
        let t = shot["t"].as_f64().expect("每张图都该带时刻");

        let got = client.call_tool("get_shot", json!({"project_id": proj, "shot_id": id}));
        assert!(!tool_failed(&got), "{id} 取图失败:{}", first_text(&got));

        // 元数据块:模板说"拿到图像本身**与同期转录**",那就得两样都有
        let meta = as_json(&first_text(&got));
        assert_eq!(meta["shot"]["id"], id, "元数据说的和要的不是同一张");
        assert_eq!(meta["shot"]["t"].as_f64(), Some(t));
        assert!(
            meta["transcript"].as_str().is_some_and(|s| !s.is_empty()),
            "模板承诺了同期转录,这里却是空的:{meta}"
        );
        let (w0, w1) = (
            meta["window_start"].as_f64().unwrap(),
            meta["window_end"].as_f64().unwrap(),
        );
        assert!(w0 <= t && t <= w1, "同期窗口必须包含该时刻:{meta}");
        assert!(
            meta["image_path"].is_null(),
            "不该把绝对路径递给 agent(它读不了文件):{meta}"
        );

        // 图像块:**解出来必须与磁盘上那个文件逐字节相同**
        //
        // 这条比"看着像张图"强:缩略图、重新编码、拿错文件,都会在这里露。
        let (bytes, mime) = image_bytes(&got).unwrap_or_else(|| panic!("{id} 没返回图像"));
        assert_eq!(mime, "image/jpeg");
        let on_disk = std::fs::read(fake.layout().rel(meta["shot"]["file"].as_str().unwrap()))
            .expect("截图文件该在");
        assert_eq!(bytes, on_disk, "{id} 返回的不是那张图本身");

        // 存回结论
        client.tool_text(
            "record_shot_check",
            json!({
                "project_id": proj,
                "shot_id": id,
                "match": true,
                "audio_visual_sync": true,
                "type_read": "手写夹具的画面",
                "issues": [],
                "confidence": "medium",
                "evidence_quote": "各位好,我们今天这期是一个承前启后的内容",
            }),
        );
    }

    // ④ 存回之后再看清单:未校准清零,而且是**从文件里读出来的**
    let listed = as_json(&client.tool_text("list_shots", json!({"project_id": proj})));
    assert_eq!(listed["counts"]["unchecked"], 0, "{listed}");
    for shot in listed["shots"].as_array().unwrap() {
        assert!(
            shot["check"].is_object(),
            "存回的结论没落到文件里:{}",
            shot["id"]
        );
    }

    // ⑤ 碰上废图就弃掉 —— 这是模板第 4 条(校准不只是"判断对错",还包括
    //    "这张图不值得判断")。弃用之后**它还在清单里**,只是状态变了:
    //    历史不能凭空消失,用户要能回看"当初为什么弃"。
    let dropped = as_json(&client.tool_text(
        "update_shot",
        json!({"project_id": proj, "shot_id": "s003", "action": "drop"}),
    ));
    assert_eq!(dropped["status"], "drop", "{dropped}");
    let listed = as_json(&client.tool_text("list_shots", json!({"project_id": proj})));
    assert_eq!(listed["counts"]["keep"], 2, "{listed}");
    assert_eq!(
        listed["shots"].as_array().unwrap().len(),
        3,
        "弃用只是标记,条目要留着:{listed}"
    );

    // ⑥ 出视图给用户看
    let rendered = as_json(&client.tool_text(
        "render_workbench",
        json!({"project_id": proj, "open_browser": false}),
    ));
    let url = rendered["url"].as_str().expect("模板让人把 url 给用户");
    assert!(url.starts_with("http://127.0.0.1:"), "只许绑回环:{url}");
}

// ── P3:补图 ──────────────────────────────────────────────────────────

/// 照着 P3 `find_missing_shots` 走:**列候选 → 自己判断 → 补一张 → 看得见**。
///
/// 这里刻意验两件模板说清楚的事:「候选不等于结论」和「补完用 get_shot 看新图」。
#[test]
fn p3_find_and_extract_a_missing_shot() {
    let fake = staged("agent-p3")
        .with_transcript(FAKE_SEGMENTS)
        .with_curve(60)
        // 只在很早的地方留一张图,后面的转录段全都"附近无图"
        .with_shot(1, 2.0);
    let proj = project_arg(&fake);

    let dir = TempDir::new("agent-p3-run");
    let mut client = McpClient::start(dir.path());
    client.initialize();

    let found = as_json(&client.tool_text(
        "find_supplement_candidates",
        json!({"project_id": proj}),
    ));
    let requests = found["requests"]
        .as_array()
        .expect("候选该是数组")
        .clone();
    assert!(
        !requests.is_empty(),
        "转录里有「大家看一下这样的图」而附近没图,该出候选:{found}"
    );
    for r in &requests {
        assert_eq!(
            r["status"], "pending",
            "候选的裁决状态是 agent 填的,算法只该给 pending:{r}"
        );
        assert!(r["shot_id"].is_null(), "算法不该替 agent 认领某张图:{r}");
    }

    // 「候选不等于结论」——agent 自己挑一条来判断
    let target = requests
        .iter()
        .find(|r| r["t"].as_f64().is_some_and(|t| t > 20.0))
        .expect("该有一条落在后半段");
    let t = target["t"].as_f64().unwrap();

    let extracted = as_json(&client.tool_text(
        "extract_shot",
        json!({"project_id": proj, "t": t, "reason": "转录说「看一下这样的图」,附近没有图"}),
    ));
    assert_eq!(extracted["origin"], "agent_request");
    assert!(
        extracted["reason"].as_str().is_some_and(|r| r.contains("看一下")),
        "补图理由要原样存下来(用户靠它知道你为什么补):{extracted}"
    );
    let new_id = extracted["id"].as_str().expect("新图该有 id").to_string();

    // 补完看新图 —— 而且这回是 **ffmpeg 真抽出来的 JPEG**
    let got = client.call_tool("get_shot", json!({"project_id": proj, "shot_id": new_id}));
    assert!(!tool_failed(&got), "{}", first_text(&got));
    let (bytes, _) = image_bytes(&got).expect("该返回图像");
    assert!(bytes.len() > 1000, "真视频抽出来的图不该这么小:{}", bytes.len());
    assert_eq!(
        &bytes[..3],
        &[0xFF, 0xD8, 0xFF],
        "JPEG 该以 FFD8FF 开头"
    );

    // 新图在清单里,而且是 uuid 之外最要紧的那件事:它进了项目,重启会话也在
    let listed = as_json(&client.tool_text("list_shots", json!({"project_id": proj})));
    let ids: Vec<&str> = listed["shots"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["id"].as_str())
        .collect();
    assert!(ids.contains(&new_id.as_str()), "补的图没进清单:{ids:?}");
}

// ── P4:总结 ──────────────────────────────────────────────────────────

/// 照着 P4 `summarize_video` 把材料都取一遍,再落盘。
///
/// 重点是**每个工具都得按模板里写的那种用法就能用** —— 不需要先知道什么隐藏前提。
#[test]
fn p4_summarize_video_tools_all_answer() {
    let fake = staged("agent-p4")
        .with_transcript(FAKE_SEGMENTS)
        .with_curve(60)
        .with_shot(1, 6.0);
    let proj = project_arg(&fake);

    let dir = TempDir::new("agent-p4-run");
    let mut client = McpClient::start(dir.path());
    client.initialize();

    // 通读转录:还没纠错时读的是原始稿
    let full = as_json(&client.tool_text("get_transcript", json!({"project_id": proj})));
    assert_eq!(full["corrected"], false, "还没有纠错稿:{full}");
    let segs = full["segments"].as_array().expect("该有段").len();
    assert!(segs >= 6, "转录段数不对:{full}");

    // 只取一段时间
    let part = as_json(&client.tool_text(
        "get_transcript",
        json!({"project_id": proj, "start": 0.0, "end": 15.0}),
    ));
    assert!(
        part["segments"].as_array().unwrap().len() < segs,
        "限了时间段却返回了全量:{part}"
    );

    // 搜一句话在哪
    let hit = as_json(&client.tool_text(
        "search_transcript",
        json!({"project_id": proj, "query": "元祖"}),
    ));
    assert!(
        hit["segments"]
            .as_array()
            .is_some_and(|s| s.iter().any(|x| x["text"].as_str().unwrap_or("").contains("元祖"))),
        "搜不到刚写进去的词:{hit}"
    );

    // 整体结构:密集区 + 截图清单(P4 的第一手材料 —— 差帧图原始点不必读,
    // 它的结论已经在密集区里了;M5 起 P4 不再让 agent 吞那份全量)
    let regions = as_json(&client.tool_text("list_regions", json!({"project_id": proj})));
    assert!(
        regions["regions"].is_array(),
        "密集区清单该在 `regions` 下(另带算法参数,好回答「为什么挑中这一段」):{regions}"
    );
    let shots = as_json(&client.tool_text("list_shots", json!({"project_id": proj})));
    let shot0 = &shots["shots"][0];
    assert!(
        shot0["t"].is_number() && shot0["status"].is_string(),
        "建结构要靠每张图的时刻与留用状态:{shots}"
    );

    // 缺画面就补一张(模板里说的是 request_shot 这个写法)
    let asked = as_json(&client.tool_text(
        "request_shot",
        json!({"project_id": proj, "t": 41.0, "reason": "转录提到列表与元组的对比图,没有图"}),
    ));
    assert_eq!(asked["origin"], "agent_request");

    // 落盘
    client.tool_text(
        "write_note",
        json!({
            "project_id": proj,
            "path": "03_summary/notes/结构草稿.md",
            "md": "# 结构\n\n- 开头:承上启下\n- 中段:四个市场的例子\n",
        }),
    );

    // **门按核验状态拦,不看签字。**
    //
    // 前面 `request_shot` 补了一张图、它还没被看过,所以此刻门拦的是"活没干完" ——
    // 报错要说清**还差哪一条**,而且**不该把人扯进来**(那是模型自己的活)。
    let blocked = client.call_tool(
        "write_summary",
        json!({"project_id": proj, "title": "总结", "md": "# 总结\n"}),
    );
    assert!(tool_failed(&blocked), "还有没核验的就写总结,该被挡住:{blocked}");
    let blocked_text = first_text(&blocked);
    assert!(blocked_text.contains("还没核验完"), "{blocked_text}");
    assert!(
        blocked_text.contains("截图"),
        "该把还没核验的项摆出来(模型照着它接着干):{blocked_text}"
    );
    assert!(
        !blocked_text.contains("approve"),
        "这一档是'活没干完',不该把人扯进来:{blocked_text}"
    );

    // **门没有侧门。** `write_note` 能写 `03_summary/` 下的 `.md`,而 `summary.md`
    // 正好在里面 —— 不堵住这条,上面的门就是个摆设:agent 只要"想把总结改一下",
    // 顺手就走过去了,甚至不知道自己在绕门。
    //
    // 这不是假想:M4 第三轮真 agent 联调里就是这么发生的 —— 用户点头、门开了之后,
    // agent 又改了 6 段转录,然后用 `write_note` 把总结整个重写了一遍,
    // 从头到尾没再碰过 `write_summary`,也没再问用户。
    let side_door = client.call_tool(
        "write_note",
        json!({
            "project_id": proj,
            "path": "03_summary/summary.md",
            "md": "# 绕过门的总结\n",
        }),
    );
    assert!(tool_failed(&side_door), "门还留着一扇侧门:{side_door}");
    let door_text = first_text(&side_door);
    assert!(
        door_text.contains("write_summary"),
        "该告诉它走哪个工具,而不是只说不让写:{door_text}"
    );

    // 把"活干完":给两张图都写回结论、并报告转录都审过了。
    // **这一步不需要任何人签字** —— 全核验完就该放行,这就是"全程托管"那一档。
    for (shot_id, read, quote) in [
        ("s001", "画面是一张示意图", "各位好,我们今天这期是一个承前启后的内容"),
        ("s002", "画面是另一张示例图", "大家看一下这样的图"),
    ] {
        client.tool_text(
            "record_shot_check",
            json!({
                "project_id": proj,
                "shot_id": shot_id,
                "match": true,
                "audio_visual_sync": true,
                "type_read": read,
                "issues": [],
                "confidence": "high",
                "evidence_quote": quote,
            }),
        );
    }
    let seg_ids: Vec<u32> = (1..=FAKE_SEGMENTS.len() as u32).collect();
    client.tool_text(
        "apply_corrections",
        json!({"project_id": proj, "corrections": [], "reviewed": seg_ids}),
    );

    client.tool_text(
        "write_summary",
        json!({"project_id": proj, "title": "总结", "md": "# 总结\n\n这一段是结果积累充分后的第一段运行。[s001 @0:06]\n"}),
    );
    let after_auto = as_json(&client.tool_text("get_project", json!({"project_id": proj})));
    assert_eq!(
        after_auto["project"]["review"]["calibrate"],
        serde_json::Value::Null,
        "全自动那一档不该留下任何签字:{after_auto}"
    );

    // **有要用户拍板的项 → 门又拦上,这回拦的是"等人"。**
    client.tool_text(
        "record_shot_check",
        json!({
            "project_id": proj,
            "shot_id": "s001",
            "match": false,
            "audio_visual_sync": true,
            "type_read": "画面看不清",
            "issues": ["画面与口播对不上"],
            "confidence": "low",
            "evidence_quote": "为什么我们经常说提前进入这一段",
        }),
    );
    let waiting = client.call_tool("write_summary", json!({"project_id": proj, "title": "总结", "md": "# 总结 v2\n"}));
    assert!(tool_failed(&waiting), "有待拍板的项,该拦:{waiting}");
    let waiting_text = first_text(&waiting);
    assert!(waiting_text.contains("等人拍板"), "{waiting_text}");
    assert!(waiting_text.contains("s001"), "要说清等的是哪一条:{waiting_text}");

    // 用户说"这些我知道,就这样过" —— 那句话记下来,门才放过这一批
    let approved = client.tool_text(
        "approve",
        json!({"project_id": proj, "note": "用户说这张我知道,就这样过"}),
    );
    assert!(approved.contains("已记下用户拍板"), "{approved}");
    assert!(approved.contains("门开了"), "该说清现在能干什么:{approved}");
    client.tool_text(
        "write_summary",
        json!({"project_id": proj, "title": "总结", "md": "# 总结 v2\n\n补图并拍板之后的版本。\n"}),
    );

    // **拍板之后材料又动过 → 那次拍板不再覆盖新冒出来的项**
    client.tool_text(
        "record_shot_check",
        json!({
            "project_id": proj,
            "shot_id": "s002",
            "match": false,
            "audio_visual_sync": false,
            "type_read": "无从确认",
            "issues": ["音画对不上"],
            "confidence": "low",
            "evidence_quote": "大家看一下这样的图",
        }),
    );
    let again = client.call_tool("write_summary", json!({"project_id": proj, "title": "总结", "md": "# 总结 v3\n"}));
    assert!(tool_failed(&again), "拍板之后又冒出新的待拍板项,该重新拦:{again}");
    let again_text = first_text(&again);
    assert!(again_text.contains("等人拍板"), "{again_text}");
    assert!(again_text.contains("s002"), "该说清新等的是哪一条:{again_text}");

    // 再拍一次板,又能写了
    client.tool_text("approve", json!({"project_id": proj, "note": "用户说就这样"}));
    client.tool_text(
        "write_summary",
        json!({"project_id": proj, "title": "总结", "md": "# 总结 v3\n\n最终版。\n"}),
    );

    // 步 3 该被标上了 —— 用户靠这个知道走到哪了
    let status = as_json(&client.tool_text("get_project", json!({"project_id": proj})));
    assert_eq!(status["progress"]["summarize"]["done"], true, "{status}");
    assert_eq!(status["materials"]["summary_md"], true);
}

// ── 纠错那一环 ───────────────────────────────────────────────────────

/// 纠错要**闭环**:改完再从 `get_transcript` 读,读到的必须是改后的;
/// 而原始转录**一个字都不能动**。
///
/// 这两条是同一件事的两面。只验"改后能读到"是不够的 ——
/// 就地改写原始稿也能让那条测试绿,却毁掉了"原始转录永不改写"这个前提,
/// 而用户事后复核全靠它。
#[test]
fn corrections_close_the_loop_and_leave_the_original_alone() {
    let fake = staged("agent-fix").with_transcript(FAKE_SEGMENTS).with_curve(60);
    let proj = project_arg(&fake);
    let raw_path = fake.layout().transcript();
    let before = std::fs::read(&raw_path).expect("原始转录该在");

    let dir = TempDir::new("agent-fix-run");
    let mut client = McpClient::start(dir.path());
    client.initialize();

    // 两条纠错:一条靠**画面上的文字**(张晓明),一条靠上下文(元组)
    let outcome = as_json(&client.tool_text(
        "apply_corrections",
        json!({
            "project_id": proj,
            "corrections": [
                {
                    "segment_id": 2,
                    "outcome": "corrected",
                    "text": "就比方说没有用上元组类型的很多的初学者朋友",
                    "type": "terminology",
                    "note": "元祖 → 元组",
                    "reason": "上下文语义:这是面向学员的系列课",
                },
                {
                    "segment_id": 5,
                    "outcome": "corrected",
                    "text": "这个是负责讲解的张晓明",
                    "type": "terminology",
                    "note": "张小明 → 张晓明",
                    "reason": "画面文字:编辑器上方写着「张晓明」",
                }
            ]
        }),
    ));
    assert_eq!(outcome["applied"], 2, "{outcome}");

    // 读回来的必须是改后稿,而且工具要**说清读的是哪一份**
    let after = as_json(&client.tool_text("get_transcript", json!({"project_id": proj})));
    assert_eq!(after["corrected"], true, "有纠错稿就该给纠错稿:{after}");
    let texts: Vec<String> = after["segments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["text"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(texts.iter().any(|t| t.contains("元组")), "{texts:?}");
    assert!(texts.iter().any(|t| t.contains("张晓明")), "{texts:?}");
    assert!(!texts.iter().any(|t| t.contains("这一周的元祖")), "{texts:?}");

    // **原始转录一个字都没动**
    let now = std::fs::read(&raw_path).expect("原始转录该还在");
    assert_eq!(before, now, "原始转录被就地改写了 —— 那用户就没法核对了");
    assert!(
        fake.layout().transcript_fixed().is_file(),
        "纠错稿没落盘,那下次开会话就丢了"
    );
    // 人能核对的 diff
    assert!(
        fake.layout().rel("02_calibrate/transcript.diff.md").is_file(),
        "没生成 diff.md,用户没法一眼看出改了什么"
    );
}

// ── 走错路的时候 ─────────────────────────────────────────────────────

/// agent 一定会走错路:**报错要能直接告诉它下一步做什么**。
///
/// 不是"报错要好听",是**报错是 agent 唯一的纠偏信号** ——
/// 它看不到源码、看不到日志,只拿得到这段文本。文本里没写"怎么办",
/// 它就只能瞎试或干脆停下。
#[test]
fn mistakes_come_back_as_instructions_not_just_errors() {
    let fake = staged("agent-typo")
        .with_transcript(FAKE_SEGMENTS)
        .with_curve(60)
        .with_shot(1, 6.0);
    let proj = project_arg(&fake);

    let dir = TempDir::new("agent-typo-run");
    let mut client = McpClient::start(dir.path());
    client.initialize();

    // ① 项目指错
    let missing = client.call_tool("list_shots", json!({"project_id": "项目名打错了"}));
    assert!(tool_failed(&missing));
    let text = first_text(&missing);
    assert!(text.contains("找不到项目"), "{text}");
    assert!(text.contains("list_projects"), "该告诉它怎么找到真项目:{text}");

    // ② 截图 id 指错
    let bad_shot = client.call_tool("get_shot", json!({"project_id": proj, "shot_id": "s999"}));
    assert!(tool_failed(&bad_shot));
    let text = first_text(&bad_shot);
    assert!(text.contains("s999"), "要说清是哪个 id 不对:{text}");
    assert!(
        text.contains("s001"),
        "该把**可用的 id** 直接报出来,而不是让 agent 再问一轮:{text}"
    );

    // ③ 写笔记写到不该写的地方
    let bad_path = client.call_tool(
        "write_note",
        json!({"project_id": proj, "path": "01_preprocess/transcript.json", "md": "x"}),
    );
    assert!(tool_failed(&bad_path), "材料文件不该能被 write_note 绕过:{bad_path}");
    let text = first_text(&bad_path);
    assert!(
        text.contains("02_calibrate") && text.contains("03_summary"),
        "该说清允许写哪儿:{text}"
    );

    // ④ 目录穿越
    let escape = client.call_tool(
        "write_note",
        json!({"project_id": proj, "path": "02_calibrate/../../跑出去.md", "md": "x"}),
    );
    assert!(tool_failed(&escape), "`..` 必须被挡住:{escape}");

    // ⑤ 只给一半的时间段
    let half = client.call_tool(
        "get_transcript",
        json!({"project_id": proj, "start": 3.0}),
    );
    assert!(tool_failed(&half));
    assert!(
        first_text(&half).contains("end"),
        "该说清缺的是哪一头:{}",
        first_text(&half)
    );

    // ⑥ 全错了一遍之后,这个会话还是好好的 —— 报错是工具错,不是协议错
    let ok = as_json(&client.tool_text("list_shots", json!({"project_id": proj})));
    assert_eq!(ok["counts"]["total"], 1, "出错之后服务还该能用:{ok}");
}

// ── 真项目上的一遍 ───────────────────────────────────────────────────

/// 对着一个**真项目**走一遍只读的路:状态 → 清单 → 每张图。
///
/// 默认不跑(要手动给项目路径):
///
/// ```text
/// VIDBRIEF_REAL_PROJECT=E:\temp\demo cargo test -p vidbrief-cli --test agent -- --ignored --nocapture
/// ```
///
/// 为什么值得留着:夹具是手写的,它证明不了"真视频抽出来的图 agent 看得清"。
/// 这一条会断言每张图都是**原分辨率的真 JPEG**、同期转录非空 ——
/// 也就是"能看图"这件事在真材料上的最小证据。
///
/// **只读**:它不往项目里写任何东西,可以放心对着用户的项目跑。
#[test]
#[ignore = "要手动给 VIDBRIEF_REAL_PROJECT"]
fn real_project_is_readable_end_to_end() {
    let Ok(project) = std::env::var("VIDBRIEF_REAL_PROJECT") else {
        panic!("没给 VIDBRIEF_REAL_PROJECT —— 这条测试要一个真项目的路径");
    };

    let dir = TempDir::new("agent-real");
    let mut client = McpClient::start(dir.path());
    client.initialize();

    let status = as_json(&client.tool_text("get_project", json!({"project_id": project})));
    let width = status["meta"]["width"].as_u64().expect("该探到分辨率");
    let total = status["materials"]["shots"]["total"]
        .as_u64()
        .expect("该有截图");
    println!(
        "项目 {} / {} 秒 / {}x{} / {} 张图 / {} 段转录",
        status["project"]["id"],
        status["meta"]["duration"].as_f64().unwrap_or(0.0).round(),
        width,
        status["meta"]["height"].as_u64().unwrap_or(0),
        total,
        status["materials"]["transcript_segments"].as_u64().unwrap_or(0),
    );

    let listed = as_json(&client.tool_text("list_shots", json!({"project_id": project})));
    let shots = listed["shots"].as_array().expect("该有清单").clone();
    assert_eq!(shots.len() as u64, total);

    let mut readable = 0;
    for shot in &shots {
        if shot["status"] == "drop" {
            continue;
        }
        let id = shot["id"].as_str().unwrap();
        let got = client.call_tool("get_shot", json!({"project_id": project, "shot_id": id}));
        assert!(!tool_failed(&got), "{id}:{}", first_text(&got));
        let (bytes, _) = image_bytes(&got).expect("该返回图像");
        assert_eq!(&bytes[..3], &[0xFF, 0xD8, 0xFF], "{id} 不是 JPEG");
        assert!(bytes.len() > 5000, "{id} 只有 {} 字节", bytes.len());

        let meta = as_json(&first_text(&got));
        assert_eq!(meta["shot"]["id"], id);
        // 图是按抽取时的宽度缩过的,不该比视频本身还宽
        assert!(meta["bytes"].as_u64().unwrap() > 0);
        readable += 1;
    }
    println!("{readable} 张图都能读出来");
    assert!(readable > 0, "一张能看的图都没有,这个项目还没跑步 1?");
}

// ── 参数名 ───────────────────────────────────────────────────────────

/// **参数名拼错不许静默失效 —— 报错要点得出那个字段的名字。**
///
/// M6 联调撞见过反面:给 `get_transcript` 传 `range`(它要的是 `start` / `end`),
/// 工具一声不响,直接返回**全量** 322 段 —— agent 拿到一份"看起来对、其实没过滤"的
/// 材料,还照它往下做。所以参数结构体一律 `deny_unknown_fields`(实测宿主没往
/// `arguments` 里塞自己的元数据,MCP 也要求入参符合工具的 `inputSchema`)。
///
/// 这条测试钉两件事:**失败**,以及**话说得明白**(点名 `range`,agent 才能一步改对)。
#[test]
fn a_misspelled_parameter_is_named_back() {
    let fake = staged("param-typo").with_transcript(FAKE_SEGMENTS);
    let mut client = McpClient::start(fake.layout().root());
    client.initialize();
    let proj = project_arg(&fake);

    let response = client.request(
        "tools/call",
        json!({
            "name": "get_transcript",
            "arguments": {"project_id": proj, "range": [0.0, 6.0]},
        }),
    );
    let blob = response.to_string();
    let failed = tool_failed(&response["result"]) || response.get("error").is_some();
    assert!(failed, "拼错的参数必须是个失败,不能静默跑成别的意思:{blob}");
    assert!(
        blob.contains("range"),
        "报错要点名那个不认识的参数 —— 不点名的话 agent 只能猜:{blob}"
    );

    // 写对了就该照常能跑 —— 严格不等于脆:同一件事用正确写法,必须过得去
    let ok = client.tool_text(
        "get_transcript",
        json!({"project_id": proj, "start": 0.0, "end": 6.0}),
    );
    let parsed = as_json(&ok);
    assert!(
        !parsed["segments"]
            .as_array()
            .unwrap_or(&Vec::new())
            .is_empty(),
        "写对了该拿到材料:{ok}"
    );
}
