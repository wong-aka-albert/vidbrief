/* ══════════ 共用片段:工具 + 报告正文的渲染 ══════════
 *
 * 工作台(`workbench.html`)与纯图文报告页(`report.html`)**共用这一份**。
 *
 * 为什么抽出来:两边显示的是同一批东西(知识点条:标题 / 正文 / 来源 / 待核验 /
 * 关联转录段),抄两份必然漂 —— 一边改了措辞另一边不知道,用户看到两个说法。
 * 这个文件**不是页面模板**,是被模板包含的片段:Rust 侧 `include_str!` 之后,
 * 替换进模板里的 `/*__SHARED_JS__*​/` 占位符。
 *
 * **调用方要先提供这两张表**(片段只声明依赖,不自己造数据):
 *   - `segById` —— 段 id → 段(芯片悬停要显示的那一句原文)
 *   - `shotById` —— 图 id → 图(条目下面挂的图)
 * 其余全在本片段里,**调用方不要再定义同名函数**(`const` 重定义会直接报错)。
 */

/* ── 工具 ── */
const $ = (s) => document.querySelector(s);
const el = (tag, cls, text) => {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text !== undefined) n.textContent = text;
  return n;
};
function fmtT(s) {
  s = Math.max(0, Math.round(s));
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), sec = s % 60;
  const mm = h ? String(m).padStart(2, "0") : String(m);
  return (h ? h + ":" : "") + mm + ":" + String(sec).padStart(2, "0");
}
// 时刻串到页面上只留到分。**两种形态都要认** ——
// Rust 侧有的是 `Display` 出来的「年-月-日 时:分:秒 +08:00」(checked_at),
// 有的是 serde 出来的 RFC3339「…T…+08:00」(报告数据整块序列化时就是这种):
// 只认前一种的话,页眉会原样吐出 `2026-09-27T10:42:56+08:00`。
const shortAt = (s) => (s || "").replace(/^\d{4}-(\d{2}-\d{2})[T ](\d{2}:\d{2}).*$/, "$1 $2");

function esc(s) {
  return String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}
function md(src) {
  const lines = esc(src == null ? "" : src).split(/\r?\n/);
  let out = "", inList = false, para = [];
  const flush = () => { if (para.length) { out += "<p>" + para.join(" ") + "</p>"; para = []; } };
  const endList = () => { if (inList) { out += "</ul>"; inList = false; } };
  const cells = row => row.replace(/^\||\|$/g, "").split("|").map(c => c.trim());
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i].trim();
    if (!line) { flush(); endList(); continue; }
    let m;
    if (/^\|.*\|$/.test(line) && i + 1 < lines.length &&
        /^\|[\s:|-]+\|$/.test(lines[i + 1].trim())) {
      flush(); endList();
      out += "<table><thead><tr>" + cells(line).map(c => `<th>${c}</th>`).join("") + "</tr></thead><tbody>";
      i += 2;
      while (i < lines.length && /^\|.*\|$/.test(lines[i].trim())) {
        out += "<tr>" + cells(lines[i].trim()).map(c => `<td>${c}</td>`).join("") + "</tr>";
        i++;
      }
      i--;
      out += "</tbody></table>";
    } else if ((m = line.match(/^(#{1,4})\s+(.*)$/))) {
      flush(); endList();
      const h = Math.min(m[1].length + 1, 5);
      out += `<h${h}>${m[2]}</h${h}>`;
    } else if ((m = line.match(/^&gt;\s?(.*)$/))) {
      flush(); endList();
      out += `<blockquote>${m[1]}</blockquote>`;
    } else if ((m = line.match(/^[-*]\s+(.*)$/))) {
      flush();
      if (!inList) { out += "<ul>"; inList = true; }
      out += `<li>${m[1]}</li>`;
    } else {
      if (inList) endList();
      para.push(line);
    }
  }
  flush(); endList();
  return out.replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
            .replace(/`([^`]+)`/g, "<code>$1</code>");
}

/* ── 报告正文:一条知识点的各个零件 ── */

// 一个转录段的芯片:`段 14 [0:54]`。悬停给出这一段的原文(截断)——
// 「段 14 是哪一句」从此答得出来。**点它是 effects 的事**(事件委托在页面里)。
function segChipHtml(id, withLabel) {
  const seg = segById.get(id);
  if (!seg) {
    return `<span class="segchip missing" title="这一段的原文不在材料里(纠正稿可能被换过)">` +
      `段 ${id}(原文不在材料里)</span>`;
  }
  const chars = Array.from(seg.text || "");
  const short = chars.slice(0, 70).join("") + (chars.length > 70 ? "…" : "");
  const label = withLabel ? "段 " + id : "▶ " + fmtT(seg.start);
  const tail = withLabel ? `<span class="t">${fmtT(seg.start)}</span>` : "";
  return `<a class="segchip" href="#" data-start="${seg.start}" ` +
    `title="${esc("段 " + id + " @ " + fmtT(seg.start) + "「" + short + "」—— 点一下视频跳到这一刻")}">` +
    `${label}${tail}</a>`;
}
function segsHtml(t) {
  if (!(t.segments || []).length) return "";
  return `<div class="segs">关联转录段:${t.segments.map((id) => segChipHtml(id, true)).join("")}</div>`;
}
function sourceHtml(t) {
  if (!(t.sources || []).length) return "";
  let html = `<div class="sources">`;
  for (const src of t.sources) {
    if (src.kind === "speaker") {
      html += `<div><span class="k">讲者</span> ` + segChipHtml(src.segment, false) +
        `「${esc(src.quote)}」</div>`;
    } else if (src.kind === "external") {
      html += `<div><span class="k">外部</span> ` +
        `<a href="${esc(src.url)}" target="_blank" rel="noopener">${esc(src.title)}</a></div>`;
    }
  }
  return html + `</div>`;
}
function verifyHtml(t) {
  if (!(t.verify || []).length) return "";
  const items = t.verify.map((v) => `<li>${esc(v)}</li>`).join("");
  return `<div class="verify"><span class="k">待核验</span> —— 这几处请对着视频核一下:<ul>${items}</ul></div>`;
}
function bodyHtml(t) { return md(t.body) + sourceHtml(t) + verifyHtml(t) + segsHtml(t); }

// 一条知识点。`jumpable` = h4 里的时刻能不能点(工作台里跳视频,纯图文页里只是标注)。
function topicNode(t, i, opts) {
  const o = opts || {};
  const node = el("article", "topic-item");
  node.id = "ti-" + i;
  const h4 = el("h4");
  const stamp = el("span", "t" + (o.jumpable ? " jumpable" : ""), fmtT(t.t));
  if (o.jumpable) stamp.dataset.at = t.t;
  else stamp.title = "时刻";
  h4.appendChild(stamp);
  h4.appendChild(document.createTextNode(t.title));
  node.appendChild(h4);
  const body = el("div");
  body.innerHTML = bodyHtml(t);
  node.appendChild(body);
  for (const sid of (t.shots || [])) {          // 图跟着条目走:它就是「图文总结」
    const s = shotById.get(sid);
    if (s) {
      const img = el("img");
      img.loading = "lazy";
      img.src = s.url;
      img.alt = sid;
      node.appendChild(img);
    }
  }
  return node;
}
function topicNodes(items, opts) {
  const frag = document.createDocumentFragment();
  items.forEach((t, i) => frag.appendChild(topicNode(t, i, opts)));
  return frag;
}
