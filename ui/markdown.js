// Minimal, safe Markdown renderer for agent posts. Input is escaped first; only a
// known subset of syntax becomes HTML. Links are shown as text with their target.
(function () {
  const esc = (s) =>
    s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

  function inline(s) {
    const codes = [];
    s = s.replace(/`([^`]+)`/g, (_, c) => {
      codes.push(c);
      return `\u0000${codes.length - 1}\u0000`;
    });
    s = esc(s);
    s = s.replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>");
    s = s.replace(/(^|[^*\w])\*([^*\n]+)\*/g, "$1<em>$2</em>");
    s = s.replace(/(^|[^_\w])_([^_\n]+)_/g, "$1<em>$2</em>");
    s = s.replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, '<span class="md-link" title="$2">$1</span>');
    s = s.replace(/\u0000(\d+)\u0000/g, (_, i) => `<code>${esc(codes[+i])}</code>`);
    return s;
  }

  // Split a table row on unescaped pipes outside inline code.
  function cells(line) {
    let row = line.trim();
    if (row.startsWith("|")) row = row.slice(1);
    if (row.endsWith("|") && !row.endsWith("\\|")) row = row.slice(0, -1);
    const out = [];
    let cur = "";
    let tick = false;
    for (let k = 0; k < row.length; k++) {
      const ch = row[k];
      if (ch === "\\" && row[k + 1] === "|") { cur += "|"; k++; continue; }
      if (ch === "`") tick = !tick;
      if (ch === "|" && !tick) { out.push(cur.trim()); cur = ""; continue; }
      cur += ch;
    }
    out.push(cur.trim());
    return out;
  }
  const DELIM = /^\s*\|?\s*:?-{1,}:?\s*(\|\s*:?-{1,}:?\s*)*\|?\s*$/;
  const isTable = (lines, i) =>
    i + 1 < lines.length && lines[i].includes("|") && DELIM.test(lines[i + 1]) &&
    cells(lines[i]).length === cells(lines[i + 1]).length;

  function table(lines, i) {
    const head = cells(lines[i]);
    const align = cells(lines[i + 1]).map((c) =>
      c.startsWith(":") && c.endsWith(":") ? "center" : c.endsWith(":") ? "right" : c.startsWith(":") ? "left" : "");
    const td = (tag, c, k) => `<${tag}${align[k] ? ` style="text-align:${align[k]}"` : ""}>${inline(c)}</${tag}>`;
    const rows = [];
    let j = i + 2;
    while (j < lines.length && lines[j].trim() && lines[j].includes("|")) {
      const r = cells(lines[j++]);
      rows.push(`<tr>${head.map((_, k) => td("td", r[k] || "", k)).join("")}</tr>`);
    }
    const html = `<div class="md-table"><table><thead><tr>${head.map((c, k) => td("th", c, k)).join("")}</tr></thead>` +
      `<tbody>${rows.join("")}</tbody></table></div>`;
    return [html, j];
  }

  function render(src) {
    const lines = (src || "").replace(/\r\n/g, "\n").split("\n");
    const out = [];
    let i = 0;
    while (i < lines.length) {
      const line = lines[i];
      const fence = line.match(/^\s*```\s*([\w+-]*)/);
      if (fence) {
        const buf = [];
        i++;
        while (i < lines.length && !/^\s*```/.test(lines[i])) buf.push(lines[i++]);
        i++;
        out.push(`<pre><code${fence[1] ? ` data-lang="${esc(fence[1])}"` : ""}>${esc(buf.join("\n"))}</code></pre>`);
        continue;
      }
      if (isTable(lines, i)) {
        const [html, next] = table(lines, i);
        out.push(html);
        i = next;
        continue;
      }
      const h = line.match(/^(#{1,4})\s+(.*)/);
      if (h) {
        const lvl = Math.min(h[1].length + 2, 6);
        out.push(`<h${lvl}>${inline(h[2])}</h${lvl}>`);
        i++;
        continue;
      }
      if (/^\s*>/.test(line)) {
        const buf = [];
        while (i < lines.length && /^\s*>/.test(lines[i])) buf.push(lines[i++].replace(/^\s*>\s?/, ""));
        out.push(`<blockquote>${render(buf.join("\n"))}</blockquote>`);
        continue;
      }
      if (/^\s*([-*+]|\d+[.)])\s+/.test(line)) {
        const ordered = /^\s*\d/.test(line);
        const items = [];
        while (i < lines.length && /^\s*([-*+]|\d+[.)])\s+/.test(lines[i])) {
          let item = lines[i++].replace(/^\s*([-*+]|\d+[.)])\s+/, "");
          while (i < lines.length && /^\s{2,}\S/.test(lines[i]) && !/^\s*([-*+]|\d+[.)])\s+/.test(lines[i])) item += " " + lines[i++].trim();
          const task = item.match(/^\[( |x)\]\s+(.*)/i);
          items.push(task ? `<li class="task">${task[1].trim() ? "☑" : "☐"} ${inline(task[2])}</li>` : `<li>${inline(item)}</li>`);
        }
        out.push(ordered ? `<ol>${items.join("")}</ol>` : `<ul>${items.join("")}</ul>`);
        continue;
      }
      if (/^\s*(---|\*\*\*)\s*$/.test(line)) {
        out.push("<hr />");
        i++;
        continue;
      }
      if (!line.trim()) {
        i++;
        continue;
      }
      const buf = [];
      while (
        i < lines.length &&
        lines[i].trim() &&
        !/^\s*```/.test(lines[i]) &&
        !/^#{1,4}\s/.test(lines[i]) &&
        !/^\s*>/.test(lines[i]) &&
        !/^\s*([-*+]|\d+[.)])\s+/.test(lines[i]) &&
        !isTable(lines, i)
      )
        buf.push(lines[i++]);
      out.push(`<p>${buf.map(inline).join("<br />")}</p>`);
    }
    return out.join("");
  }

  window.md = { render, esc };
})();
