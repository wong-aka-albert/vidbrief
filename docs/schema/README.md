# vidbrief JSON Schema(项目材料格式规范)

> **状态**:M0 定稿(v1)。本目录是"项目目录里的每个 JSON 文件长什么样"的**唯一权威**。
> 主设计见 [`../DESIGN.md` §4](../DESIGN.md);Rust 侧类型见 `crates/vidbrief-project/`。

本目录同时是:

1. **人读**的格式规范(本文件);
2. **机器校验**的 [JSON Schema](https://json-schema.org/)(`*.schema.json`,Draft 2020-12);
3. **测试夹具**(`examples/*.json`)—— 被 `cargo test` 用来三方锚定"文档 ↔ schema ↔ Rust 类型"。

---

## 1. 全局约定

### 1.1 命名

| 项 | 规则 | 例 |
|----|------|----|
| JSON 字段 | **`snake_case`**,与 Rust 字段同名(不用 `rename`) | `fps_sampled` |
| MCP 工具名 / 入参 | 同上,`snake_case` | `record_shot_check`、`shot_id` |
| 时间点字段 | 视频内时间统一叫 `t` / `start` / `end` | `peak_t` |
| 时刻字段 | 墙上时钟统一以 `_at` 结尾 | `created_at`、`checked_at` |

> 全项目(文件、MCP 工具、CLI 参数)Rust 侧和 JSON 侧**完全同名**,不做大小写风格转换——少一层心智负担,少一类 bug。

### 1.2 时间

| 两类时间 | 表示 | 例 |
|---------|------|----|
| **视频内时间**(位置) | `number`,单位**秒**,`f64`,从 `0.0` 起 | `86.0` |
| **墙上时钟**(何时发生) | `string`,**RFC 3339 带 UTC 偏移** | `"2026-09-24T21:03:11+08:00"` |

禁止 `NaN` / `Infinity`(JSON 无法表示;Rust 侧解析时会报错)。

### 1.3 路径

**项目内所有路径一律是"项目根相对路径",用 `/` 分隔。**

- ✅ `01_preprocess/shots/s007_0479.0.jpg`
- ❌ `shots/s007.jpg`(缺前缀,基准不明)
- ❌ `E:/proj/x.jpg`、`/abs/x.jpg`、`./x.jpg`、`02_calibrate/../01_preprocess/x.jpg`(绝对 / 冗余 / 回退)

**为什么强制**:项目可以整体搬走、压缩、发给别人;任何绝对路径或 `..` 都会在某一步炸掉。Rust 侧由 `vidbrief_project::RelPath` 类型强制(构造即校验),读盘时也会复检。

**唯一例外**:`project.json.source.original` 记录"源视频从哪来"的原始绝对路径,纯信息性、不用于访问文件 —— 字段名里带 `original` 以示区别。

### 1.4 空值:显式 `null`,不省略字段

"未知 / 不适用"一律写 `null`,**不省略该字段**。因此每个 schema 的 `required` 是确定的,不会因数据而变。

> 例:未被校准的截图 `"check": null`,而不是把这个 key 删掉。
> 好处:`jq '.shots[].check'` 永远能跑;Rust 侧 `Option<T>` 默认行为就是写 `null`。

### 1.5 严格性

- **JSON Schema**:`additionalProperties: false` —— 拼错的字段名会被抓出来。
- **Rust**:`#[serde(deny_unknown_fields)]` —— 与 schema 严格对齐。
- 因此**加字段 = 破坏性变更**,必须同步:改 schema → 改 Rust 类型 → 改 examples → **bump `schema_version`**。

### 1.6 版本与迁移

版本号只出现在 **`project.json.schema_version`**(当前 `1`),它代表**该项目下所有 JSON 文件**的格式版本。

规则:

- 任一文件结构发生**不兼容**变更 → `schema_version` +1;
- 工具读项目时**先读 `project.json`**;版本不等于代码期望值 → **直接报错退出**,不做静默兼容、不猜测;
- 需要支持旧项目时,写显式迁移函数(而不是让解析器容忍两种形状)。

### 1.7 数值范围

| 字段 | 范围 | 说明 |
|------|------|------|
| `score` | `[0, 1]` | 相邻帧差异归一化值 |
| `mass` | `≥ 0` | 滑窗累计变化量(不是归一化的) |
| `confidence` | 枚举 | `high` / `medium` / `low` |
| `gap` | `≥ 0` | 时间距离(秒) |

### 1.8 谁写这些文件

**这是本设计最容易搞混的一点**:

| 文件 | 谁写 | 何时 |
|------|------|------|
| `project.json` | 工具 | 建项目 / 每步完成后更新进度 |
| `source/video.meta.json` | 工具(ffprobe) | 建项目时 |
| `change_curve.json` | 工具(算法) | 步 1 |
| `dense_regions.json` | 工具(算法) | 步 1 |
| `transcript.json` | 工具(whisper) | 步 1 |
| `shots.json` | 工具建,**agent 改** | 步 1 建;步 2/3 增删改 |
| `transcript.fixed.json` | **agent** | 步 2 |
| `supplement_requests.json` | 工具建,**agent 改** `status` | 步 2 |
| `*.md`(summary / report / log) | **agent** | 步 2/3/4 |

- `transcript.json`(步 1,**原始**)与 `transcript.fixed.json`(步 2,**经纠错**)是**两个独立文件**,共用 [`transcript.schema.json`](transcript.schema.json)。
- `transcript.json` 里 `corrected_text` / `correction` **恒为 `null`** —— 原始产出不得被就地改写,否则"agent 改了什么"就查不出来了。

---

## 2. 文件清单

| 文件 | 位置 | Schema | 示例 |
|------|------|--------|------|
| 项目元数据与进度 | `project.json` | [project.schema.json](project.schema.json) | [examples/project.json](examples/project.json) |
| 源视频技术元数据 | `source/video.meta.json` | [video_meta.schema.json](video_meta.schema.json) | [examples/video.meta.json](examples/video.meta.json) |
| 差帧图 | `01_preprocess/change_curve.json` | [change_curve.schema.json](change_curve.schema.json) | [examples/change_curve.json](examples/change_curve.json) |
| 变化密集区 | `01_preprocess/dense_regions.json` | [dense_regions.schema.json](dense_regions.schema.json) | [examples/dense_regions.json](examples/dense_regions.json) |
| 转录(原始) | `01_preprocess/transcript.json` | [transcript.schema.json](transcript.schema.json) | [examples/transcript.json](examples/transcript.json) |
| 转录(纠错后) | `02_calibrate/transcript.fixed.json` | [transcript.schema.json](transcript.schema.json) | [examples/transcript.fixed.json](examples/transcript.fixed.json) |
| 截图清单 | `01_preprocess/shots.json` | [shots.schema.json](shots.schema.json) | [examples/shots.json](examples/shots.json) |
| 补图请求 | `02_calibrate/supplement_requests.json` | [supplement_requests.schema.json](supplement_requests.schema.json) | [examples/supplement_requests.json](examples/supplement_requests.json) |

无 schema 的文件(`.md` 与 `.srt/.txt/.vtt` 转录副本、图片、HTML 视图)不在本目录管辖范围。

---

## 3. 标识符规则

| 标识 | 形状 | 生成规则 | 例 |
|------|------|---------|-----|
| `project.id` | `^[a-z0-9][a-z0-9_-]{0,63}$` | 由视频文件名净化而来;冲突时加 `-2` | `demo` |
| `shot.id` | `^s[0-9]{3,}$` | 项目内**单调递增、永不复用**;`s001` 起 | `s007` |
| `shot.file` 文件名 | `{id}_{t:06.1}.jpg` | 由 id 与时间戳合成 | `s007_0479.0.jpg` |
| `region.id` | 正整数 | 从 `1` 起,按时间顺序 | `3` |
| `segment.id` | 正整数 | 从 `1` 起,等于 SRT 里的序号 | `12` |
| `supplement request.id` | 正整数 | 从 `1` 起 | `4` |

**`{:06.1}` 的行为**:整数部分补零到至少 4 位(`86.0` → `0086.0`,`479.0` → `0479.0`);超过 9999 秒时自然变宽(`7200.0`),不截断、不重排。所以 2.7 小时以上的视频也能用,只是文件名长度不再统一。

**shot id 永不复用**:`update_shot(action=drop)` 只改 `status`,不回收 id;补图继续往后发号。这样任何日志、报告、对话里出现过的 `s007` **永远指同一个时刻的同一张图**。

---

## 4. 枚举速查

| 枚举 | 取值 | 含义 |
|------|------|------|
| `region.reason` | `high_density` / `isolated_peak` / `long_static_break` | 该密集区被哪条判据选中(DESIGN §5.2) |
| `shot.origin` | `dense_region` / `isolated_peak` / `agent_request` / `manual` | 这张图从哪来 |
| `shot.status` | `keep` / `drop` | 是否保留(算法默认全 `keep`) |
| `shot.check.confidence` | `high` / `medium` / `low` | agent 看图结论的把握程度 |
| `correction.type` | `homophone` / `terminology` / `number` / `uncertain` / `other` | 纠错类型 |
| `supplement.trigger` | `deictic_phrase` / `no_shot_nearby` / `both` | 为什么被列为补图候选 |
| `supplement.status` | `pending` / `accepted` / `rejected` | agent 的判断结果 |
| `progress.*` | `done` / `at` / `rounds` / `last_at` | 见 §5 |

### `correction.type` 的一个要点

`type: "uncertain"` 表示 **agent 认为这里可能有问题但不敢改** —— 此时 `corrected_text` **保持 `null`**(不得臆造),只在 `correction.note` 里写下疑似正确写法。这区分了两种情况:

- **改了**:`corrected_text` 有值,`type` 是具体错因;
- **存疑但没改**:`corrected_text` 为 `null`,`type: "uncertain"`,留给用户在审查节点裁决。

---

## 5. `progress` 与 `review`:做过什么 vs 用户点过头没有

**这两件事分开记。**

`progress` 只记录"做过什么",供 `vidbrief status` / `list_projects` 显示。
"这一步通过了没有"不在这里 —— 它在 `review` 里,是**审核门**:

```json
"review": {
  "calibrate": {                     // null = 还没被确认过
    "approved_at": "2026-09-24T21:22:30+08:00",
    "counts": { "unchecked": 0, "needing_review": 3,
                "supplement_pending": 0, "uncertain": 2 },
    "note": "用户说:那 3 张我知道,先往下走"
  }
}
```

- **没有 `approved_by`。** 拿不到:MCP 握手只给客户端名,没有模型名;而"让模型自报"
  就是让它编。服务端能观察到的只有**什么时候**、**当时什么状态**。
- **`counts` 全是服务端自己数得出来的四项。** 数不出来的东西不进这道门。
- **一变就作废**:确认之后任何一项变了,这次确认不再算数,得重新看一眼再确认 ——
  否则用户确认的是第 2 轮,agent 拿着第 3 轮去写总结,门就成了摆设。

| 字段 | 形状 | 说明 |
|------|------|------|
| `preprocess` | `{done, at}` | 步 1 是原子的:跑完即 `done` |
| `calibrate` | `{rounds, last_at}` | **没有** `done` —— 校准是迭代,轮数由 agent 每轮 `+1` |
| `summarize` | `{done, at}` | 可重做,重做后仍是 `done`(时间更新) |
| `report` | `{done, at}` | 同上 |

---

## 6. 与并发的关系

本目录定义格式,**不定义加锁**。并发规则见 DESIGN §4.7:

- `project.json.rev` 是**乐观并发**的修订号:任何写操作须带上读到的 `rev`,不匹配则返回 `stale`,由 agent 重读后再改。工具自身每次成功写入 `project.json` 时 `rev += 1`。
- 所有写盘走"**临时文件 + 原子 rename**"(见 `vidbrief_project::json::write_atomic`),避免读到半截 JSON。
- `project.lock` **不归本目录管**:它是个 **0 字节的哨兵**,不是材料文件 ——
  里面没有任何内容,所以既没有 schema 也没有示例。判据是"锁拿不拿得到",
  **不是"文件存不存在"**(空闲的项目里它也在)。
  > M0 曾经给它定过格式(pid / host / session / opened_at),M3 实现时发现那是错的:
  > 把持有者信息写进锁文件,会在 Windows 上被强制锁挡住 —— 恰恰在最需要那句话的时候读不到
  > (详见 `docs/devlog/2026-09.md` 的 260925-05 与 260925-10)。

---

## 7. 改了 schema 之后要做什么

```bash
# 1) 改 docs/schema/*.schema.json
# 2) 同步 crates/vidbrief-project/src/*.rs(保持 deny_unknown_fields)
# 3) 同步 docs/schema/examples/*.json —— 测试会用它做三方锚定
# 4) 若为不兼容变更:project.json 的 schema_version +1
# 5) cargo test -p vidbrief-project   # 三方锚定测试必须绿
# 6) 补 devlog 条目 + 更新 ../DESIGN.md §4,bump 文档版本号
```

`cargo test -p vidbrief-project` 会验证:

- 每个 `examples/*.json` 都能被**对应 schema** 校验通过(用 `jsonschema` crate);
- 每个 `examples/*.json` 都能被**对应 Rust 类型**反序列化(`deny_unknown_fields` 生效);
- 再序列化回去,与示例文件**逐字节相同**(确保字段顺序、`null` 显式写出、缩进风格一致)。

任何一环漂移都会红。所以**示例文件不是文档,是契约**。
