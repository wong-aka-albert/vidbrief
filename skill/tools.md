# 工具速查(**自动生成**,别手改)

> **唯一事实源是工具自己**:名字、说明、必填参数都从活的工具表里读出来,
> 这份文件是它的投影。改了工具的说明或参数,
> `cargo test -p vidbrief-mcp skill_tools_index` 会红;
> 按提示重跑一次就把这份同步了。
>
> 参数怎么填、门的规矩、报错怎么解,**以 `get_guide(step=…)` 为准** ——
> 那份会展开讲;这里只有「有哪些工具、干什么、必须给什么」。

一共 26 件工具。

- **`apply_corrections`** — 把转录纠错写回 `02_calibrate/transcript.fixed.json`(原始转录永不就地改写),并刷新人可核对的 `transcript.diff.md`。
  必填:`corrections`、`project_id`
- **`approve`** — 记下「**用户拍板了悬着的那一批**」—— 只对'等人拍板'那一类有效。
  必填:`project_id`
- **`build_report`** — 出**报告**(步 4 的产出):把步 3 写下的那些段(`write_summary`)组装成报告 —— 每段(标题、正文、锚点)就是一条挂在**一个时刻**上的知识点(**视频…
  必填:`project_id`、`style`
- **`create_project`** — 新建一个项目:建目录骨架、把源视频搬进来(硬链接→软链接→复制)、用 ffprobe 探测元数据。
  必填:`video_path`
- **`disposition_supplements`** — 处置补图候选:**逐条给结论** —— 补(`need_shot`)或者不补(`no_shot`)。
  必填:`items`、`project_id`
- **`extract_shot`** — 在指定时刻补截一张图(登记进 shots.json,`origin = agent_request`)。
  必填:`project_id`、`reason`、`t`
- **`find_supplement_candidates`** — 扫转录,列出**可能缺图**的位置(语音提到画面上的东西但附近没有截图),写进 `02_calibrate/supplement_requests.json` 并返回。
  必填:`project_id`
- **`get_change_curve`** — 取差帧图(每个采样时刻的帧间差异)。
  必填:`project_id`
- **`get_guide`** — 每一步该怎么做 —— 与服务的提示模板(prompts)同一份文本。
  必填:`project_id`
- **`get_project`** — 看一个项目的全貌:四步进度、材料清单(差帧图/密集区/截图/转录各做了多少)、源视频健康检查,以及**三个外挂**(ffmpeg / ffprobe / whisper)在不在、什…
  必填:`project_id`
- **`get_shot`** — 取一张截图:**返回图像本身**,外加它的元数据与该时刻前后 ±6 秒的转录。
  必填:`project_id`、`shot_id`
- **`get_transcript`** — 取转录全文(或某个时间段)。
  必填:`project_id`
- **`list_projects`** — 列出 `projects/` 下的所有项目,含各自做到第几步、材料齐不齐、源视频还在不在。
- **`list_regions`** — 取变化密集区清单(每段的起止、峰值时刻、为什么被选中)。
  必填:`project_id`
- **`list_shots`** — 列出项目的全部关键截图(时刻、来源、是否已被校准、是否待裁决)。
  必填:`project_id`
- **`preprocess`** — 步 1 前处理(**起作业并等它**):抽帧算差帧图 → 找变化密集区 → 抽关键截图 → 语音转文字 → 渲染时间线。
  必填:`project_id`
- **`preprocess_progress`** — 看一眼步 1 跑到哪儿了 —— **只读,不等**。
  必填:`project_id`
- **`preprocess_result`** — 取步 1 的结果:跑完了就是那份完整交代(与从前 `preprocess` 直接返回的是同一份);**还在跑**就如实说「还没好」(不是错误,等一会儿再来);失败了或者半路断了会说…
  必填:`project_id`
- **`record_shot_check`** — 把**你对一张截图的判断**写回 shots.json(全量替换,复核后重写一次即以最后一次为准)。
  必填:`audio_visual_sync`、`confidence`、`match`、`project_id`、`shot_id`、`type_read`
- **`render_calibration_table`** — 把校准结果渲染成对照表(`02_calibrate/calibrate.html`),起本地查看服务,并打开用户浏览器。
  必填:`project_id`
- **`render_timeline`** — 把最新材料渲染成四层共轴时间线(`01_preprocess/timeline.html`),起本地查看服务,并打开用户浏览器。
  必填:`project_id`
- **`request_shot`** — 同 extract_shot:在指定时刻补截一张图。
  必填:`project_id`、`reason`、`t`
- **`search_transcript`** — 在转录里搜子串(大小写不敏感),返回命中的段。
  必填:`project_id`、`query`
- **`update_shot`** — 改一张现有截图:`keep` 留用 / `drop` 弃用 / `retime` 换个时刻重截(重抽帧、换文件名、删旧图)。
  必填:`action`、`project_id`、`shot_id`
- **`write_note`** — 往项目里写一份 Markdown:交叉印证结论(`02_calibrate/cross_validate.md`)、校准日志(`02_calibrate/calibration_l…
  必填:`md`、`path`、`project_id`
- **`write_summary`** — 交**一段**总结(一小节)—— 写完一段落一段,别攒着一次交整篇。
  必填:`md`、`project_id`、`title`
