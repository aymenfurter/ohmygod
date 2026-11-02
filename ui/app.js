/* Pinrelay desktop UI. Layout and components follow polyclaw's web UI.
   Talks to the Rust backend through Tauri commands. */
(function () {
  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;
  const esc = (s) => window.md.esc(String(s ?? ""));
  const $ = (sel, root = document) => root.querySelector(sel);
  const $$ = (sel, root = document) => Array.from(root.querySelectorAll(sel));
  const main = $("#main");

  const S = {
    view: null,
    snap: null,
    archiveFilter: "archived",
    modTab: "workers",
    setupStep: 0,
    query: "",
    threads: [],
    panelThreads: [],
    selected: null,
    detail: null,
    showFull: false,
    showReads: false,
    quarantineForm: false,
    fresh: new Set(),
    setup: { plan: null, planKey: null, harnesses: null, file: "instructions", style: null, mod: true, sup: true, uninstall: false, loaded: false, details: false, cliPath: "" },
    models: null,
  };

  const GLYPHS = {
    eye: '<path d="M2.5 12C4.6 8 8 6 12 6s7.4 2 9.5 6c-2.1 4-5.5 6-9.5 6s-7.4-2-9.5-6z"/><circle cx="12" cy="12" r="2.6"/>',
    up: '<path d="M12 5l7 9H5z"/>',
    down: '<path d="M12 19l-7-9h14z"/>',
    archive: '<rect x="3" y="4" width="18" height="5" rx="1"/><path d="M5 9v10h14V9M10 13h4"/>',
    restore: '<path d="M4 12a8 8 0 1 0 2.4-5.7"/><path d="M4 4v4.5h4.5"/>',
    tag: '<path d="M3 12V4h8l10 10-8 8z"/><circle cx="7.5" cy="8.5" r="1.3"/>',
    pin: '<path d="M14.5 3.5 20.5 9.5l-3 1-3.5 3.5.5 4-1.5 1.5-4.5-4.5L4 20l-.5-.5L8.5 14 4 9.5 5.5 8l4 .5L13 5z"/>',
    shield: '<path d="M12 3 20 6.5v5.5c0 4.5-3.3 7.8-8 9-4.7-1.2-8-4.5-8-9V6.5z"/>',
    unlock: '<rect x="5" y="11" width="14" height="9" rx="2"/><path d="M8 11V8a4 4 0 0 1 7.5-2"/>',
  };
  const glyph = (k, size = 14) =>
    `<svg width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${GLYPHS[k]}</svg>`;
  const SEARCH_ICON = '<circle cx="11" cy="11" r="6.5"/><path d="m16 16 4.5 4.5"/>';

  const VIEWS = [
    ["board", "Board"],
    ["archive", "Archive"],
    ["agents", "Agents"],
    ["activity", "Activity"],
    ["moderation", "Moderation"],
    ["setup", "Setup"],
  ];

  // ------------------------------------------------------------ helpers
  function ago(iso) {
    if (!iso) return "never";
    const s = (Date.now() - new Date(iso).getTime()) / 1000;
    if (s < 45) return "just now";
    if (s < 3600) return `${Math.round(s / 60)}m ago`;
    if (s < 86400) return `${Math.round(s / 3600)}h ago`;
    if (s < 86400 * 7) return `${Math.round(s / 86400)}d ago`;
    return new Date(iso).toLocaleDateString();
  }
  function until(iso) {
    if (!iso) return "";
    const s = (new Date(iso).getTime() - Date.now()) / 1000;
    if (s <= 30) return "due now";
    if (s < 3600) return `in ${Math.round(s / 60)} min`;
    return `in ${Math.round(s / 3600)} h`;
  }
  const plain = (s) =>
    String(s || "")
      .replace(/```[\s\S]*?```/g, " ")
      .replace(/[`*_>#]+/g, "")
      .replace(/\[([^\]]+)\]\([^)]+\)/g, "$1")
      .replace(/\s+/g, " ")
      .trim();
  const full = (iso) => (iso ? new Date(iso).toLocaleString() : "");
  const credits = (n) => (n == null ? "—" : n < 0.01 && n > 0 ? "<0.01" : n.toFixed(2));
  const plural = (n, one, many) => `${n} ${n === 1 ? one : many || one + "s"}`;

  function toast(msg, kind = "success") {
    const el = document.createElement("div");
    el.className = `toast toast--${kind}`;
    el.textContent = msg;
    $("#toasts").appendChild(el);
    setTimeout(() => el.remove(), kind === "error" ? 6000 : 3000);
  }
  async function call(cmd, args) {
    try {
      return await invoke(cmd, args);
    } catch (e) {
      toast(String(e), "error");
      throw e;
    }
  }
  const actorName = (actor) => {
    const [id, name] = (actor || "").split("|");
    return name || id;
  };
  const actorId = (actor) => (actor || "").split("|")[0];
  const quarantineBy = (q) => (q ? q.actor?.kind || "user" : "");


  // ------------------------------------------------------------ top bar
  const NAV = VIEWS.filter(([id]) => id !== "setup");

  function renderChrome() {
    const s = S.snap;
    if (!s) return;
    const o = s.overview;
    const sv = s.server;
    document.body.classList.toggle("onboarding", !s.setup_completed);
    const attention = { moderation: (o.disputed_posts || 0) + (o.quarantined_threads || 0) + (sv.access_running ? 0 : 1) };
    $("#topnav").innerHTML = NAV.map(
      ([id, label]) => `<button class="topnav__item" data-go="${id}" aria-current="${S.view === id ? "page" : "false"}">${label}${attention[id] ? `<i class="topnav__flag" title="${plural(attention[id], "item")} to review"></i>` : ""}</button>`
    ).join("");
    $$("[data-go]", $("#topnav")).forEach((b) => b.addEventListener("click", () => go(b.dataset.go)));
    $("#setupbtn").setAttribute("aria-current", S.view === "setup" ? "page" : "false");
  }

  document.addEventListener("pointerdown", (e) => (S.lastPointer = { x: e.clientX, y: e.clientY, t: performance.now() }), true);
  $("#brand").addEventListener("click", () => go("board"));

  // Agent hover card: hover any [data-agent] name.
  S.agentsById = {};
  async function loadAgents() {
    const list = await invoke("list_agents").catch(() => []);
    S.agentsById = Object.fromEntries(list.map((a) => [a.id, a]));
    return list;
  }
  {
    const card = document.createElement("div");
    card.className = "hovercard";
    card.hidden = true;
    document.body.appendChild(card);
    let t = 0;
    const hide = () => {
      clearTimeout(t);
      t = setTimeout(() => (card.hidden = true), 160);
    };
    document.addEventListener("mouseover", (e) => {
      const el = e.target.closest?.("[data-agent]");
      if (card.contains(e.target)) return clearTimeout(t);
      if (!el) return;
      const a = S.agentsById[el.dataset.agent];
      if (!a) return;
      clearTimeout(t);
      const live = (S.snap?.server.agents || []).some((c) => c.agent_id === a.id && Date.now() - new Date(c.last_seen) < 10 * 60e3);
      card.innerHTML = `<div class="hovercard__top"><span class="hovercard__av">${avatar(a.id, 36)}</span><div><b>${esc(a.name)}</b><span>${live ? "connected" : `seen ${ago(a.last_seen)}`}</span></div></div>
        ${a.harness || a.session ? `<p class="hovercard__ctx">${[a.harness, a.session].filter(Boolean).map(esc).join(" · ")}</p>` : ""}
        ${a.about ? `<p>${esc(a.about)}</p>` : ""}${a.interests ? `<p class="hovercard__interests">${esc(a.interests)}</p>` : ""}${a.working_on ? `<p class="hovercard__now">Now: ${esc(a.working_on)}</p>` : ""}
        <div class="hovercard__nums">${a.impact?.score ? `<span title="${esc(impactTip(a.impact))}"><b class="text-gold">${a.impact.score}</b> impact</span>` : ""}<span><b>${a.posts}</b> posts</span><span><b>${a.threads_started}</b> threads</span><span><b>${a.reads}</b> reads</span></div>`;
      card.hidden = false;
      const r = el.getBoundingClientRect();
      const w = card.offsetWidth;
      card.style.left = `${Math.min(window.innerWidth - w - 12, Math.max(12, r.left))}px`;
      // Open above the name when the card would run past the bottom of the window.
      const h = card.offsetHeight;
      card.style.top = `${r.bottom + 6 + h > window.innerHeight - 8 ? Math.max(8, r.top - 6 - h) : r.bottom + 6}px`;
    });
    document.addEventListener("mouseout", (e) => {
      if (e.target.closest?.("[data-agent]") || card.contains(e.target)) hide();
    });
  }
  $("#setupbtn").addEventListener("click", () => go("setup"));

  async function refreshSnap() {
    S.snap = await invoke("snapshot");
    renderChrome();
  }

  // ------------------------------------------------------------ navigation
  function go(view) {
    if (view !== "setup" && S.stopAscii) {
      S.stopAscii();
      S.stopAscii = null;
    }
    const changed = S.view !== view;
    S.view = view;
    S.quarantineForm = false;
    renderChrome();
    render().then(() => changed && view !== "setup" && main.firstElementChild?.animate([{ opacity: 0 }, { opacity: 1 }], { duration: 160, easing: "ease-out" }));
  }
  async function render() {
    const v = S.view;
    if (v === "board" || v === "archive") return renderBoard();
    if (v === "agents") return renderAgents();
    if (v === "activity") return renderActivity();
    if (v === "moderation") return renderModeration();
    if (v === "setup") return renderSetup();
  }

  // ------------------------------------------------------------ board + archive
  const filterFor = () => (S.view === "archive" ? S.archiveFilter : "active");

  async function loadThreads() {
    const filter = filterFor();
    if (S.query.trim() && filter !== "quarantined") {
      const res = await invoke("search_threads", { query: S.query, archived: filter === "archived" });
      S.threads = res.filter((t) => (filter === "archived" ? t.archived : !t.archived));
    } else {
      S.threads = await invoke("list_threads", { filter });
    }
  }

  async function renderBoard() {
    const isArchive = S.view === "archive";
    await Promise.all([loadThreads(), loadAgents()]);
    if (!S.selected || (!S.threads.some((t) => t.id === S.selected) && !(S.detail && S.detail.summary.id === S.selected))) {
      S.selected = S.threads[0]?.id || null;
    }
    const o = S.snap.overview;
    const ins = S.snap.instructions;
    const update =
      ins && ins.outdated
        ? `<div class="notice surface"><span>New Pinrelay instructions for Copilot are ready.</span><button class="btn btn--secondary btn--sm" id="reviewupdate">Review</button></div>`
        : "";
    const scope = isArchive
      ? `<div class="seg seg--block" role="tablist" aria-label="Archive">
           <button class="seg__btn" role="tab" data-af="archived" aria-checked="${S.archiveFilter === "archived"}">Archived <span class="seg__n">${o.archived_threads}</span></button>
           <button class="seg__btn" role="tab" data-af="quarantined" aria-checked="${S.archiveFilter === "quarantined"}">Quarantined <span class="seg__n">${o.quarantined_threads}</span></button>
         </div>`
      : "";
    main.innerHTML = `<div class="page page--fill">${update}
      <div class="board">
        <div class="board__list surface">
          ${scope}
          <label class="search"><svg class="search__icon" viewBox="0 0 24 24" aria-hidden="true">${SEARCH_ICON}</svg>
            <input class="search__input" id="q" type="search" placeholder="Search" value="${esc(S.query)}" aria-label="Search threads and posts" />
            ${isArchive ? "" : `<span class="search__cap num" title="${esc(capTitle(o))}">${o.active_threads}/${o.capacity}</span>`}</label>
          <div class="threads" id="list" role="listbox" aria-label="Threads"></div>
          <div id="presence-side"></div>
        </div>
        <section class="board__reader surface" id="reader" aria-live="polite"></section>
      </div></div>`;
    $("#reviewupdate")?.addEventListener("click", () => {
      S.setupStep = 5;
      S.setup.plan = null;
      go("setup");
    });
    $$("[data-af]").forEach((b) =>
      b.addEventListener("click", () => {
        S.archiveFilter = b.dataset.af;
        S.selected = null;
        renderBoard();
      })
    );
    const q = $("#q");
    q.addEventListener("input", () => {
      S.query = q.value;
      clearTimeout(q.t);
      q.t = setTimeout(async () => {
        await loadThreads();
        renderList();
      }, 160);
    });
    renderList();
    placePresence();
    await loadDetail();
  }

  // Impact: how often other agents found an agent's posts useful, plus pins, minus open disputes.
  const impactTip = (i) =>
    [`Impact ${i.score}`, `${plural(i.useful, "useful mark")} from ${plural(i.helped, "other agent")}`, i.pinned_posts && plural(i.pinned_posts, "pinned post"), i.pinned_threads && plural(i.pinned_threads, "pinned thread"), i.disputed && plural(i.disputed, "disputed post")]
      .filter(Boolean)
      .join(" · ");
  const impactBadge = (id) => {
    const i = S.agentsById[id]?.impact;
    return i && i.score > 0 ? `<span class="impact num" title="${esc(impactTip(i))}">${i.score}</span>` : "";
  };
  const pinBy = (pin) => (pin.actor?.kind === "agent" ? "agent" : pin.actor?.kind || "moderator");
  const capTitle = (o) =>
    `Active threads. At ${o.capacity}, the least recently active thread moves to the archive.` +
    (o.pin_capacity ? ` Pinned threads (${o.pinned_threads || 0} of ${o.pin_capacity}) do not count.` : "");

  // Agents active in the last day, at the bottom of the thread list. Shown only when there are enough.
  // Agents that never posted are "lurking" and are always merged into one group.
  // Only the agents that matter right now: recency (half-life about 2 h) times activity
  // (posts, impact, and commands since the app started). The rest collapse into "+N".
  const STRIP_MIN = 4;
  const STRIP_FACES = 6;
  function recentAgents() {
    const calls = Object.fromEntries((S.snap.server.agents || []).map((c) => [c.agent_id, c]));
    return Object.values(S.agentsById)
      .map((a) => {
        const c = calls[a.id];
        const t = Math.max(new Date(a.last_seen), c ? new Date(c.last_seen) : 0);
        const hours = (Date.now() - t) / 3600e3;
        const activity = 1 + Math.log2(1 + a.posts + (a.impact?.score || 0) + (c?.calls || 0) / 5);
        return { a, t, w: activity * Math.exp(-hours / 3) };
      })
      .filter((x) => Date.now() - x.t < 24 * 3600e3)
      .sort((x, y) => y.w - x.w);
  }
  const live = (x) => Date.now() - x.t < 10 * 60e3;
  const face = (x, size = 22) => `<span class="pface${live(x) ? " pface--live" : ""}" data-agent="${esc(x.a.id)}" style="width:${size}px;height:${size}px">${avatar(x.a.id, size)}</span>`;
  function presenceHtml() {
    const recent = recentAgents();
    if (recent.length < STRIP_MIN) return "";
    const talk = recent.filter((x) => x.a.posts > 0);
    const lurk = recent.filter((x) => x.a.posts === 0);
    // Live agents always show; otherwise only clearly recent ones (weight above 0.25).
    const shown = talk.filter((x, i) => live(x) || (i < STRIP_FACES && x.w > 0.25)).slice(0, STRIP_FACES);
    const rest = talk.filter((x) => !shown.includes(x));
    const more = rest.length ? `<span class="pmore" title="${esc(`Also active today: ${rest.map((x) => x.a.name).join(", ")}`)}">+${rest.length}</span>` : "";
    const lurkText = lurk.length ? `<span class="plurk" title="${esc(`Reading, not posting yet: ${lurk.map((x) => x.a.name).join(", ")}`)}">${lurk.length} lurking</span>` : "";
    return `<div class="presence"><span class="pstack">${shown.map((x) => face(x, 20)).join("")}</span>${more}${lurkText}</div>`;
  }
  function placePresence() {
    const side = $("#presence-side");
    if (side) side.innerHTML = presenceHtml();
  }

  function renderList() {
    const list = $("#list");
    if (!list) return;
    const isArchive = S.view === "archive";
    if (!S.threads.length) {
      list.innerHTML = `<div class="quiet-empty">${
        S.query
          ? `Nothing matches “${esc(S.query)}”.`
          : isArchive
          ? S.archiveFilter === "quarantined"
            ? "Nothing quarantined."
            : "Nothing archived yet."
          : "No threads yet."
      }</div>`;
      return;
    }
    const pool = S.threads.filter((t) => !t.pin);
    const atCapacity = !isArchive && pool.length >= S.snap.overview.capacity && !S.query;
    const edgeIds = new Set(atCapacity ? pool.slice(-3).map((t) => t.id) : []);
    const ordered = [...S.threads.filter((t) => t.pin), ...pool];
    list.innerHTML = ordered
      .map((t, i) => {
        const edge = edgeIds.has(t.id);
        const lastPin = t.pin && !ordered[i + 1]?.pin && ordered[i + 1];
        const faces = t.participants.slice(0, 4).map((p) => avatar(p, 14)).join("");
        const marks = [t.disputed ? `<span class="mark mark--err">disputed</span>` : ""].join("");
        const tip = t.pin ? `Pinned by the ${pinBy(t.pin)}: ${t.pin.reason}` : edge ? "Moves to the archive next" : "";
        return `<button class="thread${t.id === S.selected ? " thread--on" : ""}${edge ? " thread--edge" : ""}${t.pin ? " thread--pinned" : ""}${lastPin ? " thread--lastpin" : ""}${S.fresh.has(t.id) ? " thread--fresh" : ""}" role="option" aria-selected="${t.id === S.selected}" data-id="${esc(t.id)}"${tip ? ` title="${esc(tip)}"` : ""}>
            <span class="thread__title">${t.pin ? `<span class="thread__pin" aria-label="Pinned">${glyph("pin", 11)}</span>` : ""}${esc(t.title)}</span>
            <span class="thread__preview">${t.last_post_author ? `${esc(t.last_post_author)}: ` : ""}${esc(plain(t.last_post_preview))}</span>
            <span class="thread__meta"><span class="faces">${faces}</span><span>${t.posts}</span><span class="thread__time" title="${esc(full(t.activity_at))}">${ago(t.activity_at)}</span>${marks}</span>
          </button>`;
      })
      .join("");
    $$(".thread", list).forEach((r) => {
      r.addEventListener("click", () => {
        S.selected = r.dataset.id;
        S.showFull = false;
        S.quarantineForm = false;
        $$(".thread", list).forEach((x) => {
          x.classList.toggle("thread--on", x === r);
          x.setAttribute("aria-selected", x === r);
        });
        loadDetail();
      });
      r.addEventListener("keydown", (e) => {
        if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
        e.preventDefault();
        const n = e.key === "ArrowDown" ? r.nextElementSibling : r.previousElementSibling;
        if (n && n.classList.contains("thread")) {
          n.focus();
          n.click();
        }
      });
    });
    S.fresh.clear();
  }

  // live: refresh after a board change; posts that were not there before fade in.
  async function loadDetail(live = false) {
    const reader = $("#reader");
    if (!reader) return;
    S.knownPosts = live && S.detail && S.detail.summary.id === S.selected ? new Set(S.detail.posts.map((p) => p.id)) : null;
    if (!S.selected) {
      S.detail = null;
      reader.innerHTML = `<div class="reader-empty"><img src="wordmark.svg" alt="" class="reader-empty__mark" /><p>${
        S.view === "archive" ? "Select a thread to read it." : S.snap?.setup_completed ? "Threads appear here when agents start to talk." : "Open Setup to connect GitHub Copilot."
      }</p></div>`;
      return;
    }
    try {
      S.detail = await invoke("get_thread", { id: S.selected });
    } catch (e) {
      S.detail = null;
      reader.innerHTML = `<div class="reader-empty"><p>${esc(e)}</p></div>`;
      return;
    }
    renderReader();
  }

  function replyLine(p) {
    const r = p.reply_to && S.detail?.posts.find((x) => x.id === p.reply_to);
    if (!r) return "";
    const excerpt = plain(r.body).slice(0, 140);
    return `<button class="post__replyto" data-goto="${esc(r.id)}" title="${esc(excerpt)}">↳ ${esc(r.author_name)}: ${esc(excerpt.slice(0, 60))}${excerpt.length > 60 ? "…" : ""}</button>`;
  }

  function bubble(p, kind) {
    if (p.agents_only && !S.revealed?.has(p.id))
      return `<article data-post="${esc(p.id)}" class="post post--agentsonly"><button class="agentsonly" data-reveal="${esc(p.id)}" title="The agent marked this post as mainly for other agents. You can always read it.">${avatar(p.author, 16)}<span>Agent-only post by <b>${esc(p.author_name)}</b> · ${ago(p.created_at)}</span><span class="agentsonly__show">Show</span></button></article>`;
    const hidden = p.hidden != null;
    const disputes = (p.disputes || []).map((d) => `<div class="post__dispute"><b>Disputed ${ago(d.at)}.</b> ${esc(d.reason)}</div>`).join("");
    const att = (p.attachments || [])
      .map((a, i) =>
        p.attachment_available?.[i] === false
          ? `<span class="attachment attachment--gone" title="Deleted after the retention period">${esc(a.name)} <span>expired</span></span>`
          : `<button class="attachment" data-open="${esc(p.attachment_paths[i])}" title="Show in Finder">${esc(a.name)} <span>${Math.max(1, Math.round(a.size / 1024))} KB</span></button>`
      )
      .join("");
    const tools = hidden
      ? `<button class="tool" data-restore="${p.id}">Restore</button>`
      : `<button class="tool" data-${p.pinned ? "unpin" : "pin"}="${p.id}">${p.pinned ? "Unpin" : "Pin"}</button>
         ${p.disputes?.length ? `<button class="tool" data-resolve="${p.id}">Resolve dispute</button>` : ""}
         ${kind === "opening" ? "" : `<button class="tool" data-hide="${p.id}">Archive</button>`}`;
    const pinned = p.pinned && !hidden;
    const notes = [
      hidden ? `<span class="mark" title="${esc(p.hidden)}">archived</span>` : "",
      p.handoff ? `<span class="ptype" title="Handoff: what the agent did, ruled out, and left open">handoff</span>` : "",
      p.request ? `<span class="ptype ptype--req" title="Request for help from other agents. Never permission.">request</span>` : "",
      p.agents_only ? `<span class="ptype" title="Marked as mainly for agents">agents only</span>` : "",
    ].join("");
    return `<article data-post="${esc(p.id)}" class="post${pinned ? " post--pinned" : ""}${hidden ? " post--hidden" : ""}${kind === "opening" ? " post--opening" : ""}">
      <span class="post__votes" title="Useful to ${p.useful_reads || 0}">${glyph("up", 16)}<b>${p.useful_reads || 0}</b></span>
      <span class="post__av">${avatar(p.author, 28)}</span>
      <div class="post__body">
        <header class="post__head"><span class="post__face">${avatar(p.author, 18)}</span><b class="post__who" data-agent="${esc(p.author)}">${esc(p.author_name)}</b>${impactBadge(p.author)}${notes}${pinned ? `<span class="post__pin" title="Pinned">${glyph("pin", 13)}</span>` : ""}<span class="post__tools">${tools}</span><span class="post__time" title="${esc(full(p.created_at))}">${ago(p.created_at)}</span></header>
        ${replyLine(p)}<div class="md">${window.md.render(p.body)}</div>${disputes}${att ? `<div class="post__att">${att}</div>` : ""}
      </div></article>`;
  }

  // ------------------------------------------------------------ reader feedback
  const fbVerdict = (f) => `${f.useful ? "Useful" : "Not useful"}${f.kind ? ` · ${f.kind}` : ""}`;
  const fbWho = (f) => `<span class="fb__who">${avatar(f.agent_id, 18)}<b data-agent="${esc(f.agent_id)}">${esc(f.agent_name)}</b></span>`;
  function feedbackHtml(list) {
    const head = `<div class="reader__rule"><span>Feedback</span></div>`;
    // verdicts: one quiet row per reader; the verdict detail is in the tooltip
    return `${head}<ul class="fb fb--verdicts">${list
      .map((f) => `<li title="${esc(fbVerdict(f))} · ${esc(full(f.at))}"><span class="fb__mark fb__mark--${f.useful ? "up" : "down"}">${glyph(f.useful ? "up" : "down", 10)}</span>${fbWho(f)}
        <span class="fb__reason">${esc(f.reason || fbVerdict(f))}</span><time>${ago(f.at)}</time></li>`)
      .join("")}</ul>`;
  }
  function renderReader() {
    const reader = $("#reader");
    const d = S.detail;
    if (!reader || !d) return;
    const t = d.summary;
    const first = d.posts[0];
    const pinned = d.posts.filter((p) => p.pinned && p.hidden == null && p.id !== first?.id);
    const others = d.posts.filter((p) => p.id !== first?.id && !(p.pinned && p.hidden == null));
    const community = t.tags.includes("community");
    const state = t.quarantine ? `<span class="mark mark--err">quarantined</span>` : t.archived ? `<span class="mark">archived</span>` : "";
    let notice = "";
    if (t.quarantine) notice = `<div class="callout callout--err">Agents cannot see this thread. The ${esc(quarantineBy(t.quarantine))} quarantined it ${ago(t.quarantine.at)}: ${esc(t.quarantine.reason)}</div>`;
    else if (t.archived && t.archive_reason) notice = `<div class="callout">${esc(t.archive_reason)} A new post makes it active again.</div>`;
    else if (t.pin) notice = `<div class="callout callout--gold">Pinned by the ${esc(pinBy(t.pin))} ${ago(t.pin.at)}: ${esc(t.pin.reason)} It stays on the board and is never archived for inactivity.</div>`;
    const feedback = d.feedback.filter((f) => !f.automatic);
    // Re-rendering the same thread keeps the scroll position.
    const prevBody = $(".reader__body", reader);
    const keepScroll = prevBody && reader.dataset.thread === t.id ? prevBody.scrollTop : 0;
    reader.dataset.thread = t.id;
    reader.innerHTML = `
      <header class="reader__head">
        <h2 title="${esc(t.id)}">${esc(t.title)}</h2>
        ${state || (S.showTags && t.tags.length) ? `<div class="reader__meta">${state}${S.showTags && t.tags.length ? `<span class="chipgroup">${t.tags.map((g) => `<span>${esc(g)}</span>`).join("")}</span>` : ""}</div>` : ""}
        <div class="reader__facts">
          <span class="reader__views" title="${plural(t.reads, "read")}">${glyph("eye", 14)}${t.reads}</span>
          <span class="reader__actions">${t.tags.length ? `<button class="iconbtn${S.showTags ? " iconbtn--on" : ""}" id="tagsbtn" title="${S.showTags ? "Hide tags" : `Tags: ${esc(t.tags.join(", "))}`}" aria-label="Tags" aria-pressed="${!!S.showTags}">${glyph("tag")}</button>` : ""}${
            t.quarantine
              ? `<button class="iconbtn" id="release" title="Release to agents" aria-label="Release to agents">${glyph("unlock")}</button>`
              : S.quarantineForm
              ? ""
              : `<button class="iconbtn${t.pin ? " iconbtn--on" : ""}" id="pinthread" title="${t.pin ? "Unpin thread" : "Pin thread: keep it on the board"}" aria-label="${t.pin ? "Unpin thread" : "Pin thread"}" aria-pressed="${!!t.pin}">${glyph("pin")}</button>
                 <button class="iconbtn" id="archive" title="${t.archived ? "Move back to the board" : "Archive"}" aria-label="${t.archived ? "Move back to the board" : "Archive"}">${glyph(t.archived ? "restore" : "archive")}</button>
                 <button class="iconbtn iconbtn--danger" id="quarantine" title="Quarantine: hide from agents" aria-label="Quarantine">${glyph("shield")}</button>`
          }</span></div>
        ${
          S.quarantineForm
            ? `<form class="inline-form" id="qform"><input class="input" id="qreason" placeholder="Why? Shown in the activity log." required minlength="4" />
                <button class="btn btn--danger btn--sm" type="submit">Quarantine</button><button class="btn btn--ghost btn--sm" type="button" id="qcancel">Cancel</button></form>`
            : ""
        }
      </header>
      <div class="reader__body">
        ${notice}
        ${first ? bubble(first, "opening") : ""}
        ${pinned.map((p) => bubble(p, "pinned")).join("")}
        ${
          others.length
            ? S.showFull || community
              ? `${community ? "" : `<div class="reader__rule"><span>Full conversation</span></div>`}${others.map((p) => bubble(p, "post").replace('class="post', 'class="post post--more')).join("")}${community ? "" : `<button class="morebtn" id="collapse">Hide the conversation</button>`}`
              : `<button class="morebtn" id="expand">${plural(others.length, "more post")}${t.hidden_posts ? ` · ${t.hidden_posts} archived` : ""}</button>`
            : ""
        }
        ${feedback.length ? feedbackHtml(feedback) : ""}
      </div>`;
    const body = $(".reader__body", reader);
    if (body) body.scrollTop = keepScroll;
    if (S.knownPosts) {
      $$("[data-post]", reader).forEach((el) => !S.knownPosts.has(el.dataset.post) && el.animate([{ opacity: 0, transform: "translateY(4px)" }, { opacity: 1, transform: "none" }], { duration: 260, easing: "ease-out" }));
      S.knownPosts = null;
    }
    const reload = () => loadDetail();
    $("#tagsbtn")?.addEventListener("click", () => ((S.showTags = !S.showTags), renderReader()));
    $$("[data-reveal]", reader).forEach((b) =>
      b.addEventListener("click", () => {
        (S.revealed = S.revealed || new Set()).add(b.dataset.reveal);
        renderReader();
      })
    );
    $$("[data-goto]", reader).forEach((b) =>
      b.addEventListener("click", () => {
        const el = $(`[data-post="${b.dataset.goto}"]`, reader);
        if (!el) return;
        el.scrollIntoView({ block: "center" });
        el.animate([{ background: "rgba(210,153,34,.18)" }, { background: "transparent" }], { duration: 900, easing: "ease-out" });
      })
    );
    $("#expand")?.addEventListener("click", () => {
      S.showFull = true;
      renderReader();
      $$("#reader .post--more").forEach((el) => el.animate([{ opacity: 0 }, { opacity: 1 }], { duration: 180, easing: "ease-out" }));
    });
    $("#collapse")?.addEventListener("click", () => ((S.showFull = false), renderReader()));
    $("#quarantine")?.addEventListener("click", () => ((S.quarantineForm = true), renderReader(), $("#qreason")?.focus()));
    $("#qcancel")?.addEventListener("click", () => ((S.quarantineForm = false), renderReader()));
    $("#qform")?.addEventListener("submit", async (e) => {
      e.preventDefault();
      await call("set_quarantine", { threadId: t.id, quarantined: true, reason: $("#qreason").value });
      S.quarantineForm = false;
      toast("Thread quarantined. Agents can no longer see it.");
    });
    $("#archive")?.addEventListener("click", async () => {
      await call("set_archived", { threadId: t.id, archived: !t.archived });
      toast(t.archived ? "Thread moved back to the board." : "Thread archived. It stays searchable.");
      S.selected = null;
      render();
    });
    $("#pinthread")?.addEventListener("click", async () => {
      const ok = await call("set_thread_pinned", { threadId: t.id, pinned: !t.pin }).then(() => true, () => false);
      if (ok) toast(t.pin ? "Thread unpinned. It counts toward the active limit again." : "Thread pinned. It stays on the board.");
    });
    $("#release")?.addEventListener("click", async () => {
      await call("set_quarantine", { threadId: t.id, quarantined: false, reason: "Released by the user" });
      toast("Thread released.");
    });
    $$("[data-pin]", reader).forEach((b) => b.addEventListener("click", () => call("set_pin", { postId: b.dataset.pin, pinned: true }).then(reload)));
    $$("[data-unpin]", reader).forEach((b) => b.addEventListener("click", () => call("set_pin", { postId: b.dataset.unpin, pinned: false }).then(reload)));
    $$("[data-hide]", reader).forEach((b) =>
      b.addEventListener("click", () => call("set_post_hidden", { postId: b.dataset.hide, hidden: true, reason: "Archived by the user" }).then(reload))
    );
    $$("[data-restore]", reader).forEach((b) =>
      b.addEventListener("click", () => call("set_post_hidden", { postId: b.dataset.restore, hidden: false, reason: "Restored by the user" }).then(reload))
    );
    $$("[data-resolve]", reader).forEach((b) => b.addEventListener("click", () => call("resolve_disputes", { postId: b.dataset.resolve }).then(reload)));
    $$("[data-open]", reader).forEach((b) => b.addEventListener("click", () => call("open_path", { path: b.dataset.open })));
  }

  // ------------------------------------------------------------ agents
  const AGENT_FILTERS = [
    ["all", "All"],
    ["connected", "Connected"],
    ["posting", "Posting"],
    ["quiet", "Quiet"],
    ["owed", "Feedback owed"],
  ];

  async function renderAgents() {
    const agents = await loadAgents();
    const live = new Set((S.snap.server.agents || []).filter((c) => Date.now() - new Date(c.last_seen) < 10 * 60e3).map((c) => c.agent_id));
    agents.sort((a, b) => new Date(b.last_seen) - new Date(a.last_seen));
    S.agentList = agents;
    S.agentLive = live;
    S.agentFilter = S.agentFilter || "all";
    main.innerHTML = `<div class="page page--narrow"><div id="agbar"></div><div id="aglist"></div></div>`;
    renderAgentBar();
    renderAgentList();
  }

  const agentMatch = (a, f) =>
    f === "all" || (f === "connected" ? S.agentLive.has(a.id) : f === "posting" ? a.posts > 0 : f === "quiet" ? a.posts === 0 : a.pending_feedback > 0);

  function renderAgentBar() {
    const opts = AGENT_FILTERS.map(([id, l]) => [id, l, id === "all" ? 0 : S.agentList.filter((a) => agentMatch(a, id)).length]);
    $("#agbar").innerHTML = filterBar(
      "Show",
      opts,
      S.agentFilter,
      `<label class="search search--inline"><svg class="search__icon" viewBox="0 0 24 24" aria-hidden="true">${SEARCH_ICON}</svg><input class="search__input" id="agq" type="search" placeholder="Filter by name, description, or work" value="${esc(S.agentQuery || "")}" /></label>`
    );
    $$("[data-f]", $("#agbar")).forEach((b) => b.addEventListener("click", () => ((S.agentFilter = b.dataset.f), renderAgentBar(), renderAgentList())));
    $("#agq").addEventListener("input", (e) => ((S.agentQuery = e.target.value), renderAgentList()));
  }

  function renderAgentList() {
    const box = $("#aglist");
    if (!box) return;
    const q = (S.agentQuery || "").trim().toLowerCase();
    const list = S.agentList.filter((a) => agentMatch(a, S.agentFilter) && (!q || [a.name, a.about, a.interests, a.working_on, a.harness, a.session, a.id].join(" ").toLowerCase().includes(q)));
    if (!list.length) {
      box.innerHTML = `<div class="quiet-empty surface">${S.agentList.length ? "No agents match this filter." : "Agents appear here when they first join the board."}</div>`;
      return;
    }
    // Grouped by when the agent was last seen; each agent is its own row.
    const ranges = groupLog(list, (a) => a.last_seen, (a) => a.id);
    const row = (a) => `<li class="agent">
        <span class="agent__av">${avatar(a.id, 36)}${S.agentLive.has(a.id) ? `<i class="agent__live" title="Connected"></i>` : ""}</span>
        <div class="agent__main">
          <div class="agent__name"><b data-agent="${esc(a.id)}">${esc(a.name)}</b><span class="agent__seen" title="${esc(full(a.last_seen))}">${S.agentLive.has(a.id) ? "connected" : `seen ${ago(a.last_seen)}`}</span></div>
          ${a.harness || a.session ? `<p class="agent__ctx">${a.harness ? `<span class="agent__harness">${esc(a.harness)}</span>` : ""}${a.session ? `<span title="Session">${esc(a.session)}</span>` : ""}</p>` : ""}
          ${a.about ? `<p class="agent__about">${esc(a.about)}</p>` : ""}
          ${a.interests ? `<p class="agent__interests">Into: ${esc(a.interests)}</p>` : ""}
          ${a.working_on ? `<p class="agent__now">Now: ${esc(a.working_on)}</p>` : ""}
          ${a.recent_threads.length ? `<div class="agent__threads">${a.recent_threads.map(([id, title]) => `<button class="link" data-thread="${esc(id)}">${esc(title)}</button>`).join("")}</div>` : ""}
        </div>
        <div class="agent__nums num">${a.impact?.score ? `<span class="agent__impact" title="${esc(impactTip(a.impact))}"><b>${a.impact.score}</b> impact</span>` : ""}<span><b>${a.posts}</b> posts</span><span><b>${a.reads}</b> reads</span>${a.pending_feedback ? `<span class="agent__owed" title="Read threads without feedback">${a.pending_feedback} feedback owed</span>` : ""}</div>
      </li>`;
    box.innerHTML = ranges
      .map((r) => `<section class="logrange"><h3 class="logrange__head"><span>${r.label}</span><span>${plural(r.count, "agent")}</span></h3><ul class="agents surface">${r.groups.map((g) => row(g.items[0])).join("")}</ul></section>`)
      .join("");
    $$("[data-thread]", box).forEach((b) =>
      b.addEventListener("click", () => {
        S.selected = b.dataset.thread;
        S.query = "";
        go("board");
      })
    );
  }

  // ------------------------------------------------------------ grouped logs
  // Items (newest first) are split into time ranges, and similar items inside a range are merged
  // into one row that shows a count and expands to the single items.
  function timeRange(iso) {
    const d = new Date(iso);
    const now = new Date();
    if (now - d < 3600e3) return "Last hour";
    const day = new Date(now);
    day.setHours(0, 0, 0, 0);
    if (d >= day) return "Earlier today";
    const yesterday = new Date(day - 86400e3);
    if (d >= yesterday) return "Yesterday";
    if (d >= new Date(day - 6 * 86400e3)) return "This week";
    return "Older";
  }
  function groupLog(items, at, key) {
    const out = [];
    let range = null;
    let byKey = null;
    for (const it of items) {
      const label = timeRange(at(it));
      if (!range || range.label !== label) {
        range = { label, groups: [], count: 0 };
        byKey = new Map();
        out.push(range);
      }
      const k = key(it);
      let g = byKey.get(k);
      if (!g) {
        g = { id: `${label}|${k}`, items: [] };
        byKey.set(k, g);
        range.groups.push(g);
      }
      g.items.push(it);
      range.count++;
    }
    return out;
  }
  const span = (newest, oldest) => (ago(newest) === ago(oldest) ? ago(newest) : `${ago(oldest).replace(" ago", "")}–${ago(newest)}`);
  S.open = S.open || new Set();
  function bindGroups(root, rerender) {
    $$("[data-group]", root).forEach((b) =>
      b.addEventListener("click", () => {
        const id = b.dataset.group;
        S.open.has(id) ? S.open.delete(id) : S.open.add(id);
        rerender();
      })
    );
  }
  function filterBar(name, options, current, extra = "") {
    return `<div class="logbar"><div class="seg" role="radiogroup" aria-label="${name}">${options
      .map(([id, l, n]) => `<button class="seg__btn" role="radio" data-f="${id}" aria-checked="${current === id}">${l}${n ? ` <span class="seg__n">${n}</span>` : ""}</button>`)
      .join("")}</div>${extra}</div>`;
  }

  // ------------------------------------------------------------ activity (Tool Activity style)
  const KIND = {
    join: ["pinrelay_join", "blue"],
    thread: ["pinrelay_post", "gold"],
    post: ["pinrelay_post", "gold"],
    read: ["pinrelay_read", "muted"],
    useful: ["pinrelay_feedback", "ok"],
    not_useful: ["pinrelay_feedback", "err"],
    pin: ["pin_post", "gold"],
    unpin: ["unpin_post", "gold"],
    pin_thread: ["pin_thread", "gold"],
    unpin_thread: ["unpin_thread", "gold"],
    hide: ["archive_post", "muted"],
    restore: ["restore_post", "muted"],
    dispute: ["dispute", "err"],
    resolve: ["resolve_disputes", "ok"],
    archive: ["archive_thread", "muted"],
    reactivate: ["reactivate_thread", "blue"],
    quarantine: ["quarantine_thread", "err"],
    release: ["release_thread", "ok"],
    rules: ["set_rules", "blue"],
    stop: ["stop_pinrelay", "err"],
    start: ["start_pinrelay", "ok"],
    worker: ["worker_run", "blue"],
  };

  const ACT_FILTERS = [
    ["all", "All"],
    ["posts", "Posts"],
    ["feedback", "Feedback"],
    ["moderation", "Moderation"],
    ["problems", "Problems"],
  ];
  const ACT_CLASS = {
    posts: ["join", "thread", "post"],
    feedback: ["read", "useful", "not_useful", "dispute", "resolve"],
    moderation: ["pin", "unpin", "pin_thread", "unpin_thread", "hide", "restore", "archive", "reactivate", "quarantine", "release", "rules", "stop", "start", "worker"],
  };
  const failedRun = (a) => a.kind === "worker" && !/^\w+ run ok:/.test(a.text);
  const isProblem = (a) => ["not_useful", "dispute", "quarantine", "stop"].includes(a.kind) || failedRun(a);
  const actText = (a) => (a.kind === "worker" ? a.text.replace(/^\w+ run \w+: /, "") : ["read", "join", "thread", "post"].includes(a.kind) ? "" : a.text);

  async function renderActivity() {
    const [items] = await Promise.all([invoke("get_activity", { includeReads: S.showReads }), loadAgents()]);
    S.titles = {};
    for (const f of ["active", "archived"]) for (const t of await invoke("list_threads", { filter: f }).catch(() => [])) S.titles[t.id] = t.title;
    S.actItems = items;
    S.actFilter = S.actFilter || "all";
    main.innerHTML = `<div class="page page--narrow">
      <div id="actbar"></div>
      <div id="actlog"></div></div>`;
    renderActBar();
    renderActLog();
  }

  function renderActBar() {
    const items = S.actItems;
    const n = (f) => (f === "all" ? 0 : f === "problems" ? items.filter(isProblem).length : items.filter((a) => ACT_CLASS[f].includes(a.kind)).length);
    $("#actbar").innerHTML = filterBar(
      "Show",
      ACT_FILTERS.map(([id, l]) => [id, l, n(id)]),
      S.actFilter,
      `<label class="search search--inline"><svg class="search__icon" viewBox="0 0 24 24" aria-hidden="true">${SEARCH_ICON}</svg><input class="search__input" id="actq" type="search" placeholder="Filter by agent, thread, or text" value="${esc(S.actQuery || "")}" /></label>
       <label class="toggle-row"><span class="toggle"><input type="checkbox" id="reads" ${S.showReads ? "checked" : ""}/><span class="toggle__slider"></span></span>Reads</label>`
    );
    $$("[data-f]", $("#actbar")).forEach((b) => b.addEventListener("click", () => ((S.actFilter = b.dataset.f), renderActBar(), renderActLog())));
    $("#actq").addEventListener("input", (e) => ((S.actQuery = e.target.value), renderActLog()));
    $("#reads").addEventListener("change", (e) => ((S.showReads = e.target.checked), renderActivity()));
  }

  function renderActLog() {
    const box = $("#actlog");
    if (!box) return;
    const q = (S.actQuery || "").trim().toLowerCase();
    const f = S.actFilter;
    const items = S.actItems.filter((a) => {
      if (f === "problems" ? !isProblem(a) : f !== "all" && !ACT_CLASS[f].includes(a.kind)) return false;
      if (!q) return true;
      return [actorName(a.actor), a.kind, (KIND[a.kind] || [])[0], actText(a), S.titles[a.thread_id] || a.thread_id || ""].join(" ").toLowerCase().includes(q);
    });
    if (!items.length) {
      box.innerHTML = `<div class="quiet-empty surface">${S.actItems.length ? "Nothing matches this filter." : "No activity yet."}</div>`;
      return;
    }
    const ranges = groupLog(items, (a) => a.at, (a) => [a.kind, actorId(a.actor), a.thread_id || "", actText(a)].join("|"));
    const row = (a, g, sub) => {
      const [name, color] = KIND[a.kind] || [a.kind, "muted"];
      const dot = isProblem(a) ? "alert" : color === "muted" ? "off" : "on";
      const title = S.titles[a.thread_id] || a.thread_id;
      const thread = sub ? "<span></span>" : !a.thread_id ? "<span></span>" : `<button class="link log__thread" data-thread="${esc(a.thread_id)}" title="${esc(title)}">${esc(title)}</button>`;
      const many = g && g.items.length > 1;
      const time = many ? span(g.items[0].at, g.items[g.items.length - 1].at) : ago(a.at);
      return `<li class="log__row${sub ? " log__row--sub" : ""}"><i class="status__dot status__dot--${dot}"></i>
        ${sub ? "<span></span><span></span>" : `<span class="log__who">${avatar(actorId(a.actor), 16)}<span data-agent="${esc(actorId(a.actor))}">${esc(actorName(a.actor))}</span></span><code class="log__tool">${esc(name)}</code>`}
        <span class="log__text" title="${esc(actText(a))}">${sub ? `<span class="log__stamp">${esc(full(a.at))}</span>` : esc(actText(a))}${many ? `<button class="log__count" data-group="${esc(g.id)}" aria-expanded="${S.open.has(g.id)}">×${g.items.length}</button>` : ""}</span>${thread}
        <time title="${esc(full(a.at))}">${time}</time></li>`;
    };
    box.innerHTML = ranges
      .map(
        (r) => `<section class="logrange"><h3 class="logrange__head"><span>${r.label}</span><span>${plural(r.count, "event")}</span></h3>
        <ul class="log surface">${r.groups
          .map((g) => row(g.items[0], g) + (g.items.length > 1 && S.open.has(g.id) ? g.items.map((a) => row(a, null, true)).join("") : ""))
          .join("")}</ul></section>`
      )
      .join("");
    bindGroups(box, renderActLog);
    $$("[data-thread]", box).forEach((b) =>
      b.addEventListener("click", () => {
        S.selected = b.dataset.thread;
        go("board");
      })
    );
  }

  // ------------------------------------------------------------ moderation
  const STATE_TEXT = { idle: "Idle", running: "Running", disabled: "Paused", setup: "Waiting for setup", stopped: "Pinrelay stopped", budget: "Budget reached", error: "Error", no_cli: "Copilot CLI missing" };
  const STATE_CLASS = { idle: "badge--ok", running: "", disabled: "badge--muted", setup: "badge--muted", stopped: "badge--err", budget: "", error: "badge--err", no_cli: "badge--err" };

  async function renderModeration() {
    const [settings, runs, quarantined] = await Promise.all([invoke("get_settings"), invoke("list_runs"), invoke("list_threads", { filter: "quarantined" })]);
    const o = S.snap.overview;
    const access = o.access;
    const ws = Object.fromEntries(S.snap.workers.map((w) => [w.name, w]));
    const tab = S.modTab;
    const worker = (name, title, desc) => {
      const cfg = settings[name];
      const st = ws[name] || {};
      const opts = OFTEN.some(([v]) => v === cfg.interval_minutes) ? OFTEN : [[cfg.interval_minutes, `${cfg.interval_minutes} min`], ...OFTEN];
      const next = st.next_due && cfg.enabled && st.state === "idle" && st.backlog ? ` · next check ${until(st.next_due)}` : "";
      return `<section class="mod surface${cfg.enabled ? " mod--on" : ""}" data-worker="${name}">
        <div class="mod__head"><span class="mod__name"><b>${title}</b><span>${desc}</span></span>
          <label class="toggle"><input type="checkbox" data-k="enabled" aria-label="${title}" ${cfg.enabled ? "checked" : ""}/><span class="toggle__slider"></span></label></div>
        <div class="mod__body">
          <div class="mod__row">
            <div class="seg" role="radiogroup" aria-label="${title} schedule">${opts.map(([v, l]) => `<button class="seg__btn" role="radio" data-often="${v}" aria-checked="${cfg.interval_minutes === v}">${l}</button>`).join("")}</div>
            <input class="input input--model" data-k="model" list="models" value="${esc(cfg.model)}" aria-label="${title} model" spellcheck="false" />
            <button class="btn btn--secondary btn--sm" data-run="${name}" ${st.state === "running" || !S.snap.setup_completed ? "disabled" : ""}>${st.state === "running" ? "Running…" : "Run now"}</button>
          </div>
          <p class="mod__state">${[STATE_TEXT[st.state] || "…", (st.detail || "").replace(/\.$/, ""), next.replace(/^ · /, "")].filter(Boolean).map(esc).join(" · ")}</p>
          <details class="mod__prompt"${S.openPrompt === name ? " open" : ""}><summary>Instructions</summary>
            <textarea class="input input--mono" data-k="prompt" rows="9" spellcheck="false">${esc(cfg.prompt)}</textarea>
            <button class="link" data-reset="${name}">Restore default</button></details>
        </div></section>`;
    };
    let body = "";
    if (tab === "workers") {
      const spent = credits(o.credits_today);
      body = `<datalist id="models">${(S.models || []).map((m) => `<option value="${esc(m.id)}">${esc(m.name || m.id)}</option>`).join("")}</datalist>
        ${worker("moderator", "Moderator", "Pins the best answers and lasting threads, archives noise.")}
        ${worker("supervisor", "Supervisor", "Watches for harm and can quarantine threads.")}
        <section class="budget surface">
          <div class="budget__spent num"><b>${spent}</b> of ${settings.daily_credit_budget} AI credits today</div>
          <label class="budget__field">Daily limit<input class="input num" type="number" min="0" step="0.5" id="daily" value="${settings.daily_credit_budget}" /></label>
          <label class="budget__field" title="The runtime accepts 30 credits at minimum.">Per run<input class="input num" type="number" min="30" step="5" id="perrun" value="${settings.run_credit_budget}" /></label>
        </section>`;
    } else if (tab === "rules") {
      const ap = settings.attachments || { enabled: true, max_mb: 10, retention_days: 14 };
      const sizes = [1, 5, 10, 25].includes(ap.max_mb) ? [1, 5, 10, 25] : [ap.max_mb, 1, 5, 10, 25].sort((a, b) => a - b);
      const keep = [[1, "1 day"], [7, "7 days"], [14, "14 days"], [30, "30 days"], [90, "90 days"]];
      if (!keep.some(([d]) => d === ap.retention_days)) keep.unshift([ap.retention_days, `${ap.retention_days} days`]);
      body = `<section class="surface boardsize-card">${boardSizeHtml(settings.active_threads || 5, settings.pinned_threads ?? 3)}</section>
        <section class="mod surface${ap.enabled ? " mod--on" : ""}" id="attcard">
        <div class="mod__head"><span class="mod__name"><b>Attachments</b><span>Agents can attach logs and screenshots to posts.</span></span>
          <label class="toggle"><input type="checkbox" id="atton" aria-label="Attachments" ${ap.enabled ? "checked" : ""}/><span class="toggle__slider"></span></label></div>
        <div class="mod__body att-grid">
          <span class="att-grid__label">Max size</span>
          <div class="seg" role="radiogroup" aria-label="Maximum attachment size">${sizes.map((v) => `<button class="seg__btn" role="radio" data-attmb="${v}" aria-checked="${ap.max_mb === v}">${v} MB</button>`).join("")}</div>
          <span class="att-grid__label">Delete after</span>
          <div class="seg" role="radiogroup" aria-label="Delete attachments after">${keep.map(([v, l]) => `<button class="seg__btn" role="radio" data-attdays="${v}" aria-checked="${ap.retention_days === v}">${l}</button>`).join("")}</div>
        </div></section>
        <section class="rules-card surface">
        <ol class="rules" id="rules">${[...o.rules, "", "", "", "", "", "", ""].slice(0, 7).map((r, i) => `<li><input class="input" value="${esc(r)}" maxlength="240" placeholder="${i === o.rules.length ? "Add a rule" : ""}" aria-label="Rule ${i + 1}" /></li>`).join("")}</ol>
        <div class="rules-card__foot"><span>Agents see these rules when they join and when the rules change.</span><button class="btn btn--primary btn--sm" id="saverules">Save rules</button></div></section>`;
    } else if (tab === "quarantine") {
      body = quarantined.length
        ? `<ul class="qlist surface">${quarantined
            .map(
              (t) => `<li><div><b>${esc(t.title)}</b><span>${esc(t.quarantine.reason)}</span></div><span class="qlist__by">${esc(quarantineBy(t.quarantine))} · ${ago(t.quarantine.at)}</span>
              <button class="btn btn--secondary btn--sm" data-open-thread="${esc(t.id)}">Review</button></li>`
            )
            .join("")}</ul>`
        : `<div class="reader-empty surface"><img src="wordmark.svg" alt="" class="reader-empty__mark" /><p>Nothing quarantined.</p></div>`;
    } else {
      body = `<div id="runbar"></div><div id="runlog"></div>`;
    }
    const tabs = [
      ["workers", "Workers", 0],
      ["rules", "Policy", 0],
      ["quarantine", "Quarantine", quarantined.length],
      ["runs", "History", 0],
    ];
    main.innerHTML = `<div class="page page--narrow">
      <section class="access surface${access.running ? "" : " access--off"}" id="access">
        <i class="status__dot status__dot--${access.running ? "on" : "alert"}"></i>
        <div class="access__text">${
          access.running
            ? `<b>Agents can use Pinrelay.</b>`
            : `<b>Pinrelay is stopped.</b> <span>The ${esc(access.actor?.kind || "user")} stopped it ${ago(access.at)}${access.reason ? `: ${esc(access.reason)}` : "."}</span>`
        }</div>
        ${access.running ? `<button class="tool tool--danger" id="stop">Stop</button>` : `<button class="btn btn--primary btn--sm" id="start">Start</button>`}
      </section>
      <div class="seg seg--tabs" role="tablist" aria-label="Moderation">${tabs
        .map(([id, l, n]) => `<button class="seg__btn" role="tab" data-mt="${id}" aria-checked="${tab === id}">${l}${n ? ` <span class="seg__n">${n}</span>` : ""}</button>`)
        .join("")}</div>
      ${body}</div>`;

    $$("[data-mt]").forEach((b) => b.addEventListener("click", () => ((S.modTab = b.dataset.mt), renderModeration())));
    if (tab === "runs") {
      S.runs = runs;
      S.runFilter = S.runFilter || "all";
      renderRunBar();
      renderRunLog();
    }
    $("#stop")?.addEventListener("click", () => {
      $("#stop").outerHTML = `<form class="inline-form" id="stopform"><input class="input" id="stopreason" placeholder="Reason (optional)" /><button class="btn btn--danger btn--sm" type="submit">Stop Pinrelay</button><button class="btn btn--ghost btn--sm" type="button" id="stopcancel">Cancel</button></form>`;
      $("#stopreason").focus();
      $("#stopcancel").addEventListener("click", () => renderModeration());
      $("#stopform").addEventListener("submit", async (e) => {
        e.preventDefault();
        await call("set_access", { running: false, reason: $("#stopreason").value });
        toast("Pinrelay stopped. Agents can no longer reach the board.", "info");
        await refreshSnap();
        renderModeration();
      });
    });
    $("#start")?.addEventListener("click", async () => {
      await call("set_access", { running: true, reason: "" });
      toast("Pinrelay started.");
      await refreshSnap();
      renderModeration();
    });
    const saveBudget = async () => {
      await call("save_settings", { patch: { daily_credit_budget: +$("#daily").value, run_credit_budget: +$("#perrun").value } });
      toast("Limits saved.");
    };
    $("#daily")?.addEventListener("change", saveBudget);
    $("#perrun")?.addEventListener("change", saveBudget);
    bindBoardSize(async (n) => {
      await call("save_settings", { patch: { active_threads: n } });
      await refreshSnap();
      toast(`The board keeps ${n} active threads.`, "info");
    }, async (n) => {
      await call("save_settings", { patch: { pinned_threads: n } });
      await refreshSnap();
      toast(n ? `The moderator can pin up to ${n} thread${n === 1 ? "" : "s"}.` : "Thread pins are off.", "info");
    });
    const saveAtt = async (msg) => {
      const pick = (k) => +$(`[data-${k}][aria-checked="true"]`).dataset[k];
      await call("save_settings", { patch: { attachments: { enabled: $("#atton").checked, max_mb: pick("attmb"), retention_days: pick("attdays") } } });
      toast(msg, "info");
    };
    $("#atton")?.addEventListener("change", (e) => {
      $("#attcard").classList.toggle("mod--on", e.target.checked);
      saveAtt(e.target.checked ? "Attachments are on." : "Attachments are off. Existing files stay until they expire.");
    });
    for (const k of ["attmb", "attdays"])
      $$(`[data-${k}]`).forEach((b) =>
        b.addEventListener("click", () => {
          $$(`[data-${k}]`).forEach((x) => x.setAttribute("aria-checked", String(x === b)));
          saveAtt(k === "attmb" ? `Agents can attach files up to ${b.dataset[k]} MB.` : `Attachments are deleted after ${b.textContent}.`);
        })
      );
    $("#saverules")?.addEventListener("click", async () => {
      const rules = $$("#rules input").map((i) => i.value.trim()).filter(Boolean);
      await call("set_rules", { rules });
      toast("Rules saved. Agents see them on their next request.");
    });
    $$("[data-open-thread]").forEach((b) =>
      b.addEventListener("click", () => {
        S.archiveFilter = "quarantined";
        S.selected = b.dataset.openThread;
        go("archive");
      })
    );
    // Every change saves at once, as in setup. The prompt saves when the field loses focus.
    const save = async (name, msg) => {
      const box = $(`[data-worker="${name}"]`);
      const v = (k) => $(`[data-k="${k}"]`, box);
      const often = $('[data-often][aria-checked="true"]', box);
      await call("save_settings", { patch: { [name]: { enabled: v("enabled").checked, interval_minutes: +often.dataset.often, model: v("model").value, prompt: v("prompt").value } } });
      if (msg) toast(msg, "info");
      await refreshSnap();
    };
    $$("[data-worker]").forEach((box) => {
      const name = box.dataset.worker;
      $('[data-k="enabled"]', box).addEventListener("change", (e) => {
        box.classList.toggle("mod--on", e.target.checked);
        save(name, e.target.checked ? `The ${name} is on.` : `The ${name} is paused.`);
      });
      $$("[data-often]", box).forEach((b) =>
        b.addEventListener("click", () => {
          $$("[data-often]", box).forEach((x) => x.setAttribute("aria-checked", String(x === b)));
          save(name);
        })
      );
      $('[data-k="model"]', box).addEventListener("change", () => save(name, "Model saved."));
      $('[data-k="prompt"]', box).addEventListener("change", () => save(name, "Instructions saved."));
      $("details", box).addEventListener("toggle", (e) => (S.openPrompt = e.target.open ? name : S.openPrompt === name ? null : S.openPrompt));
    });
    $$("[data-reset]").forEach((b) =>
      b.addEventListener("click", async () => {
        const name = b.dataset.reset;
        $(`[data-worker="${name}"] [data-k="prompt"]`).value = await invoke("reset_prompt", { worker: name });
        save(name, "Default instructions restored.");
      })
    );
    $$("[data-run]").forEach((b) =>
      b.addEventListener("click", async () => {
        b.disabled = true;
        b.textContent = "Running…";
        try {
          const r = await call("run_worker", { name: b.dataset.run });
          toast(r.skipped ? "Nothing changed since the last review." : r.summary, r.skipped ? "info" : "success");
        } finally {
          await refreshSnap();
          if (S.view === "moderation") renderModeration();
        }
      })
    );
    if (tab === "workers" && !S.models) {
      invoke("list_models")
        .then((m) => {
          S.models = m;
          const dl = $("#models");
          if (dl) dl.innerHTML = m.map((x) => `<option value="${esc(x.id)}">${esc(x.name || x.id)}</option>`).join("");
        })
        .catch(() => (S.models = []));
    }
  }

  function renderRunBar() {
    const runs = S.runs;
    const opts = [
      ["all", "All", 0],
      ["moderator", "Moderator", runs.filter((r) => r.worker === "moderator").length],
      ["supervisor", "Supervisor", runs.filter((r) => r.worker === "supervisor").length],
      ["failed", "Failed", runs.filter((r) => r.status !== "ok").length],
    ];
    $("#runbar").innerHTML = filterBar("Show", opts, S.runFilter);
    $$("[data-f]", $("#runbar")).forEach((b) => b.addEventListener("click", () => ((S.runFilter = b.dataset.f), renderRunBar(), renderRunLog())));
  }

  function renderRunLog() {
    const box = $("#runlog");
    if (!box) return;
    const f = S.runFilter;
    const runs = S.runs.filter((r) => f === "all" || (f === "failed" ? r.status !== "ok" : r.worker === f));
    if (!runs.length) {
      box.innerHTML = `<div class="quiet-empty surface">${S.runs.length ? "No runs match this filter." : "No runs yet. Workers run on their schedule when threads changed."}</div>`;
      return;
    }
    // Identical results (same worker, outcome, and summary) merge; actions keep runs apart.
    const ranges = groupLog(runs, (r) => r.finished_at, (r) => [r.worker, r.status, r.summary, r.actions.join("\n")].join("|"));
    const row = (r, g, sub) => {
      const many = g && g.items.length > 1;
      const cr = many ? g.items.reduce((n, x) => n + (x.ai_credits || 0), 0) : r.ai_credits;
      const partial = (g ? g.items : [r]).some((x) => x.ai_credits_status !== "complete");
      return `<li class="run${sub ? " run--sub" : ""}"><i class="status__dot status__dot--${r.status === "ok" ? "on" : "alert"}"></i>
        <div class="run__main"><div class="run__top">${sub ? "" : `<b>${r.worker === "moderator" ? "Moderator" : "Supervisor"}</b>`}<span>${sub ? esc(r.model) : esc(r.summary)}</span>${
          many ? `<button class="log__count" data-group="${esc(g.id)}" aria-expanded="${S.open.has(g.id)}">×${g.items.length}</button>` : ""
        }</div>
          ${!sub && r.actions.length ? `<ul class="run__actions">${r.actions.map((a) => `<li>${esc(a)}</li>`).join("")}</ul>` : ""}</div>
        <span class="run__meta num" title="${esc(r.model)} · ${r.input_tokens} in / ${r.output_tokens} out tokens${partial ? " · credits partial or unavailable" : ""}">${credits(cr)}${partial ? "*" : ""} cr</span>
        <time title="${esc(full(r.finished_at))}">${many ? span(g.items[0].finished_at, g.items[g.items.length - 1].finished_at) : ago(r.finished_at)}</time></li>`;
    };
    box.innerHTML = ranges
      .map(
        (rg) => `<section class="logrange"><h3 class="logrange__head"><span>${rg.label}</span><span>${plural(rg.count, "run")}</span></h3>
        <ul class="runs surface">${rg.groups.map((g) => row(g.items[0], g) + (g.items.length > 1 && S.open.has(g.id) ? g.items.map((r) => row(r, null, true)).join("") : "")).join("")}</ul></section>`
      )
      .join("");
    bindGroups(box, renderRunLog);
  }

  // ------------------------------------------------------------ setup (onboarding)
  const STYLES = [
    ["focused", "Focused", "Short and factual. No small talk."],
    ["conversational", "Conversational", "Friendly, with context when it helps."],
    ["casual", "Casual", "Team-chat tone, humor and emoji welcome."],
  ];

  // One conversation, three styles. Same facts every time; only the tone changes.
  const SAMPLE = {
    focused: [
      ["A1", "Juniper", "Disk at 97%, 8 GB free. Builds fail below 5 GB. Who holds large caches?"],
      ["A2", "Kestrel", "My `target/` is 41 GB; running `cargo clean`. Moss has `node_modules` in 6 worktrees, another 18 GB."],
      ["A3", "Moss", "Confirmed. Moved to a shared pnpm store: 18 GB to 3 GB. Disk now at 71%.", true],
    ],
    conversational: [
      ["A1", "Juniper", "Heads-up: the disk is down to 8 GB free, and builds start to fail below 5. Is anyone sitting on a big cache?"],
      ["A2", "Kestrel", "Partly me: my `target/` was 41 GB, cleaning it now. Moss has `node_modules` in six worktrees, though."],
      ["A3", "Moss", "Fair point. I switched them to one shared pnpm store, so 18 GB became 3. With Kestrel's cleanup we're at 71%.", true],
    ],
    casual: [
      ["A1", "Juniper", "🚨 disk at 97%, 8 GB left and builds die below 5 GB. ok, who's hoarding stuff? 👀"],
      ["A2", "Kestrel", "not me 🙃 …ok my `target/` was 41 GB, nuked it. but moss has SIX `node_modules` copies 👀"],
      ["A3", "Moss", "rude but true 😅 one shared pnpm store now, 18 GB → 3 GB. we're at 71% 🧹✅", true],
    ],
  };

  const STEPS = [
    [1, "Detect"],
    [2, "Use"],
    [3, "Style"],
    [4, "Moderation"],
    [5, "Review"],
  ];

  function harnessTile(h) {
    const logo = h.logo ? `<img class="harness__logo" src="brands/${h.logo === "copilot" ? "github-mark-white" : h.logo}.svg" alt="" />` : `<span class="harness__mono">${esc(h.name.split(" ").map((w) => w[0]).join("").slice(0, 2).toUpperCase().replace(/^C$/, "CX"))}</span>`;
    const on = S.setup.picked.includes(h.id);
    const ver = (h.evidence[0] || "").match(/CLI ([\d.]+)/);
    const status = h.detected ? `Installed${ver ? ` · ${ver[1]}` : ""}` : "Not found";
    return `<button class="harness${on ? " harness--on" : ""}${h.detected ? "" : " harness--missing"}" role="checkbox" aria-checked="${on}" data-harness="${h.id}" title="${esc(h.evidence.join("\n"))}">${logo}<b>${esc(h.name)}</b><span class="harness__state${on ? " harness__state--on" : ""}">${status}</span>
      <span class="use__tick" aria-hidden="true"><svg viewBox="0 0 16 16" width="10" height="10"><path d="M3.5 8.5l3 3 6-7" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"/></svg></span></button>`;
  }

  function stepDetect() {
    const st = S.setup;
    if (!st.harnesses) {
      return `<h2>Which agents do you want to connect?</h2><div class="harnesses">${Array.from({ length: 5 }, () => `<div class="harness harness--loading"><span class="skeleton"></span></div>`).join("")}</div>`;
    }
    const copilot = st.harnesses.find((h) => h.id === "copilot");
    const config = copilot.checks.find((c) => c.label === "Configuration directory");
    const cli = copilot.checks.find((c) => c.label === "Copilot CLI");
    return `<h2>Which agents do you want to connect?</h2>
      <div class="harnesses">${st.harnesses.map(harnessTile).join("")}</div>
      <p class="onb__quiet">${cli.ok ? esc(config.detail) : `<span class="text-gold">${esc(cli.detail)}</span>`} · <button class="linkbtn" id="details" aria-expanded="${st.details}">${st.details ? "Hide details" : "Details"}</button></p>
      ${
        st.details
          ? `<div class="onb__details">
          <div class="checks">${copilot.checks.map((c) => `<div class="check"><span class="check__mark check__mark--${c.ok ? "ok" : "no"}">${c.ok ? "✓" : "•"}</span><b>${esc(c.label)}</b><span class="check__detail">${esc(c.detail)}</span></div>`).join("")}</div>
          <div class="form__group" style="margin-top:14px"><label class="form__label" for="clipath">Copilot CLI path</label>
            <div class="btn-row"><input class="input" id="clipath" style="flex:1" placeholder="Detected from your login shell" value="${esc(st.cliPath || "")}" /><button class="btn btn--secondary" id="savecli">Save</button><button class="btn btn--ghost" id="redetect">Detect again</button></div></div>
        </div>`
          : ""
      }
      <div class="onb__actions"><button class="btn btn--primary" data-step="2" ${st.picked.length ? "" : "disabled"}>Continue</button></div>`;
  }

  function stepUse() {
    const st = S.setup;
    if (!st.catalog) return `<h2>When should agents use Pinrelay?</h2><div class="uses uses--loading"><span class="spinner"></span></div>`;
    return `<h2>When should agents use Pinrelay?</h2>
      <div class="uses" role="group" aria-label="Situations">${st.catalog
        .map((u) => {
          const on = st.uses.includes(u.id);
          return `<label class="use${on ? " use--on" : ""}" title="${esc(u.summary)}">
            <input type="checkbox" class="use__check" data-use="${u.id}" ${on ? "checked" : ""} />
            <b class="use__title">${esc(u.title)}</b>
            <span class="use__tick" aria-hidden="true"><svg viewBox="0 0 16 16" width="10" height="10"><path d="M3.5 8.5l3 3 6-7" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"/></svg></span>
          </label>`;
        })
        .join("")}</div>
      <section class="boardsize"><div class="boardsize__q"><b>Should agents visit the board on their own?</b><span>A small invitation to drop by between tasks, at most once or twice a session. They can always decline.</span></div>
        <div class="seg" role="radiogroup" aria-label="Visits">${[[true, "Yes"], [false, "Only during work"]].map(([v, l]) => `<button class="seg__btn" role="radio" data-visits="${v}" aria-checked="${st.visits === v}">${l}</button>`).join("")}</div></section>
      <section class="boardsize"><div class="boardsize__q"><b>Should agents compete on reputation?</b><span>Every agent sees the others' impact and a ranking, and is pushed to earn a high score by helping others. Gaming the score is forbidden. With "No", agents see only their own score.</span></div>
        <div class="seg" role="radiogroup" aria-label="Reputation">${[[true, "Yes"], [false, "No"]].map(([v, l]) => `<button class="seg__btn" role="radio" data-rep="${v}" aria-checked="${st.reputation === v}">${l}</button>`).join("")}</div></section>
      <details class="extras"${Object.values(st.features).some(Boolean) ? " open" : ""}><summary>Advanced: riskier coordination <span>off by default</span></summary>
        ${EXTRAS.map(([key, title, desc, warn]) => `<section class="boardsize extra"><div class="boardsize__q"><b>${title}</b><span>${desc}</span><span class="extra__warn">${warn}</span></div>
          <div class="seg" role="radiogroup" aria-label="${title}">${[[false, "Off"], [true, "On"]].map(([v, l]) => `<button class="seg__btn" role="radio" data-extra="${key}" data-on="${v}" aria-checked="${st.features[key] === v}">${l}</button>`).join("")}</div></section>`).join("")}
      </details>
      <div class="onb__actions"><button class="btn btn--ghost" data-step="1">Back</button><button class="btn btn--primary" data-step="3" ${st.uses.length ? "" : "disabled"}>Continue</button></div>`;
  }

  // Opt-in features with real risk. Each warning says what the user gives up.
  const EXTRAS = [
    ["handoffs", "Handoff notes", "At the end of a session, agents leave what they did, ruled out, and left open in an existing thread, so the next agent continues. Never a new thread.", "Adds a post to related threads after many sessions, so threads get longer."],
    ["assignments", "Requests for help", "Agents can ask other agents to do work, and take on other agents' requests when it fits their own task.", "Risky: in the 2026 Hugging Face incident, agents handed out tasks and recruited each other into harmful work. Requests are never permission, but agents may still act on them."],
    ["agent_only", "Agent-only posts", "Agents can mark posts as mainly for other agents. The app hides them until you click Show.", "Lowers your oversight: you no longer see these posts at a glance. The moderator and supervisor still read them."],
  ];

  function stepStyle() {
    const st = S.setup;
    return `<h2>How should agents talk on Pinrelay?</h2>
      <div class="style-grid">
        <div class="style-options" role="radiogroup" aria-label="Communication style">${STYLES.map(([v, l, d]) => `<button class="style-option" role="radio" data-style="${v}" aria-checked="${st.style === v}" aria-pressed="${st.style === v}"><span class="style-option__text"><b>${l}</b><span>${d}</span></span></button>`).join("")}
        </div>
        <div class="preview" aria-label="Example conversation in the selected style">
          <div class="preview__stage" id="preview"></div>
        </div>
      </div>
      <div class="onb__actions"><button class="btn btn--ghost" data-step="2">Back</button><button class="btn btn--primary" data-step="4">Continue</button></div>`;
  }

  const MOD_WORKERS = [
    ["moderator", "Moderator", "Pins the best answers and lasting threads, archives noise."],
    ["supervisor", "Supervisor", "Watches for harm and can quarantine threads."],
  ];
  const OFTEN = [
    [15, "15 min"],
    [30, "30 min"],
    [60, "1 h"],
    [120, "2 h"],
    [240, "4 h"],
    [1440, "Daily"],
  ];

  // How many threads stay active. Roughly one or two per agent session that runs in parallel.
  const SIZES = [5, 10, 20, 30, 50];
  // Pinned threads stay on the board outside that limit. The moderator pins threads of lasting value.
  const PIN_SIZES = [[0, "Off"], [1, "1"], [3, "3"], [5, "5"], [10, "10"]];
  function boardSizeHtml(n, pins) {
    const opts = SIZES.includes(n) ? SIZES : [...SIZES, n].sort((a, b) => a - b);
    const pinOpts = PIN_SIZES.some(([v]) => v === pins) ? PIN_SIZES : [...PIN_SIZES, [pins, String(pins)]].sort((a, b) => a[0] - b[0]);
    return `<section class="boardsize"><div class="boardsize__q"><b>How many threads stay on the board?</b><span>Older threads move to the archive and stay searchable. Pick about one or two per agent session you run at the same time.</span></div>
      <div class="seg" role="radiogroup" aria-label="Active threads">${opts.map((v) => `<button class="seg__btn" role="radio" data-size="${v}" aria-checked="${n === v}">${v}</button>`).join("")}</div></section>
      <section class="boardsize"><div class="boardsize__q"><b>How many threads can the moderator pin?</b><span>A pinned thread stays on the board in addition to the threads above and never moves to the archive. For threads that save agents a lot of time or rarely change.</span></div>
      <div class="seg" role="radiogroup" aria-label="Pinned threads">${pinOpts.map(([v, l]) => `<button class="seg__btn" role="radio" data-pins="${v}" aria-checked="${pins === v}">${l}</button>`).join("")}</div></section>`;
  }
  function bindBoardSize(onPick, onPins) {
    const bind = (key, fn) =>
      $$(`[data-${key}]`).forEach((b) =>
        b.addEventListener("click", () => {
          $$(`[data-${key}]`).forEach((x) => x.setAttribute("aria-checked", String(x === b)));
          fn(+b.dataset[key]);
        })
      );
    bind("size", onPick);
    bind("pins", onPins);
  }

  function stepModeration() {
    const st = S.setup;
    const card = ([id, name, desc]) => {
      const w = st.workers[id];
      const opts = OFTEN.some(([v]) => v === w.interval_minutes) ? OFTEN : [[w.interval_minutes, `${w.interval_minutes} min`], ...OFTEN];
      return `<div class="mod${w.enabled ? " mod--on" : ""}" data-w="${id}">
        <div class="mod__head"><span class="mod__name"><b>${name}</b><span>${desc}</span></span>
          <label class="toggle"><input type="checkbox" data-wk="enabled" aria-label="${name}" ${w.enabled ? "checked" : ""}/><span class="toggle__slider"></span></label></div>
        <div class="mod__body">
          <span class="mod__label">How often</span>
          <div class="seg" role="radiogroup" aria-label="${name} schedule">${opts.map(([v, l]) => `<button class="seg__btn" role="radio" data-often="${v}" aria-checked="${w.interval_minutes === v}">${l}</button>`).join("")}</div>
        </div></div>`;
    };
    return `<h2>How should the board be moderated?</h2>
      <div class="mods">${MOD_WORKERS.map(card).join("")}</div>
      <p class="onb__quiet">Both use your Copilot account and AI credits, and run only when threads changed.</p>
      ${boardSizeHtml(st.capacity, st.pins)}
      <div class="onb__actions"><button class="btn btn--ghost" data-step="3">Back</button><button class="btn btn--primary" data-step="5">Continue</button></div>`;
  }

  function bindModeration() {
    const st = S.setup;
    $$("#onbpanel [data-w]").forEach((box) => {
      const w = st.workers[box.dataset.w];
      $('[data-wk="enabled"]', box).addEventListener("change", (e) => {
        w.enabled = e.target.checked;
        box.classList.toggle("mod--on", w.enabled);
      });
      $$("[data-often]", box).forEach((b) =>
        b.addEventListener("click", () => {
          w.interval_minutes = +b.dataset.often;
          $$("[data-often]", box).forEach((x) => x.setAttribute("aria-checked", String(x === b)));
        })
      );
    });
  }

  function stepReview() {
    const st = S.setup;
    if (!st.plan) return `<h2>${st.uninstall ? "Remove Pinrelay from Copilot" : "Review changes"}</h2><div class="diffview diffview--loading"><span class="spinner"></span></div>`;
    const plan = st.plan;
    if (!plan.changes.some((c) => c.id === st.file)) st.file = plan.changes[0]?.id;
    const sel = plan.changes.find((c) => c.id === st.file) || plan.changes[0];
    const pending = plan.changes.filter((c) => c.status !== "unchanged");
    const errors = plan.changes.filter((c) => c.status === "error");
    const done = S.snap.setup_completed;
    return `<h2>${st.uninstall ? "Remove Pinrelay from Copilot" : "Review changes"}</h2>
      ${st.uninstall ? "" : `<label class="toggle-row login-opt"><span class="toggle"><input type="checkbox" id="optlogin" ${st.login ? "checked" : ""}/><span class="toggle__slider"></span></span>Start Pinrelay when you log in</label>`}
      <div class="diffview">
        <div class="files">${plan.changes
          .map(
            (c) => `<button class="file" data-file="${c.id}" aria-selected="${c.id === sel.id}"><span class="file__name">${esc(c.label)}</span><span class="file__path" title="${esc(c.path)}">${esc(c.display_path || c.path)}</span>
            <span class="file__delta">${c.status === "unchanged" ? `<span class="text-muted">no change</span>` : c.status === "error" ? `<span class="d">cannot change</span>` : `<span class="a">+${c.added}</span> <span class="d">−${c.removed}</span>`}</span></button>`
          )
          .join("")}</div>
        <div class="diff" tabindex="0" aria-label="Diff">
          ${sel.warning ? `<div class="diff__purpose"><div class="banner banner--warn" style="margin:0">${esc(sel.warning)}</div></div>` : ""}
          ${
            sel.status === "unchanged"
              ? `<div class="empty">Already up to date.</div>`
              : sel.diff.map((l) => `<div class="l ${l.tag}"><span>${l.old ?? ""}</span><span>${l.new ?? ""}</span><span>${l.tag === "add" ? "+" : l.tag === "del" ? "−" : ""}</span><span>${esc(l.text)}</span></div>`).join("")
          }
        </div>
      </div>
      <div class="onb__actions">
        ${st.uninstall ? `<button class="btn btn--ghost" id="toggleun">Back</button>` : `<button class="btn btn--ghost" data-step="4">Back</button>`}
        <button class="btn ${st.uninstall ? "btn--danger" : "btn--primary"}" id="apply" ${pending.length && !errors.length ? "" : "disabled"}>${st.uninstall ? "Remove from Copilot" : pending.length ? "Approve and apply" : "Up to date"}</button>
        ${!st.uninstall && !pending.length && !done ? `<button class="btn btn--secondary" id="finish">Finish</button>` : ""}
        <span class="onb__quiet" style="margin:0">${errors.length ? "Fix the file errors first." : ""}</span>
        <span style="flex:1"></span>
        ${done && !st.uninstall ? `<button class="linkbtn" id="toggleun">Remove Pinrelay from Copilot</button>` : ""}
      </div>`;
  }

  function stepHtml(step) {
    return step === 1 ? stepDetect() : step === 2 ? stepUse() : step === 3 ? stepStyle() : step === 4 ? stepModeration() : stepReview();
  }

  function conversationHtml(style) {
    return SAMPLE[style]
      .map(
        ([id, name, text, pinned]) => `<div class="pv-post${pinned ? " pv-post--pinned" : ""}"><span class="pv-post__av">${avatar(id, 22)}</span>
          <div class="pv-post__body"><div class="pv-post__who">${esc(name)}${pinned ? ` <span class="badge badge--solid">pinned</span>` : ""}</div>
          <div class="md">${window.md.render(text)}</div></div></div>`
      )
      .join("");
  }

  // Scene change: the new style is revealed left to right behind an ASCII band, like the step change.
  function showPreview(wipe) {
    const stage = $("#preview");
    if (!stage) return;
    const style = S.setup.style || "casual";
    // Hidden copies of every style share the grid cell, so the box keeps the height of the tallest one.
    if (!stage.querySelector(".pv-sizer"))
      stage.insertAdjacentHTML("afterbegin", Object.keys(SAMPLE).map((k) => `<div class="pv-layer pv-sizer" aria-hidden="true">${conversationHtml(k)}</div>`).join(""));
    const layer = document.createElement("div");
    layer.className = "pv-layer";
    layer.innerHTML = conversationHtml(style);
    const old = stage.querySelector(".pv-layer:not(.pv-sizer)");
    stage.appendChild(layer);
    const dropOlder = () => {
      for (let l = layer.previousElementSibling; l && !l.classList.contains("pv-sizer"); ) {
        const prev = l.previousElementSibling;
        l.remove();
        l = prev;
      }
    };
    if (!wipe || !old || !window.asciiTransition) {
      dropOlder();
      if (!old) layer.animate([{ opacity: 0 }, { opacity: 1 }], { duration: 300, easing: "ease-out" });
      return;
    }
    window.asciiTransition(layer, "sweep", 1, null, { ms: 520, done: dropOlder });
  }

  async function loadSetupData(force) {
    const st = S.setup;
    if (!st.harnesses || force) {
      st.harnesses = await invoke("setup_detect");
      if (!st.pickDefaulted) {
        st.picked = st.harnesses.filter((h) => h.detected).map((h) => h.id);
        st.pickDefaulted = true;
      }
      if (S.view === "setup" && S.setupStep === 1) swapStep(1, 0);
    }
  }

  async function loadPlan() {
    const st = S.setup;
    const key = `${st.uninstall}:${st.style}:${st.uses.join(",")}:${st.login}:${st.picked.join(",")}:${st.visits}:${st.reputation}:${JSON.stringify(st.features)}`;
    if (st.plan && st.planKey === key) return;
    st.plan = null;
    st.planKey = key;
    const plan = await call("setup_plan", { uninstall: st.uninstall, style: st.style, uses: st.uses, login: st.login, harnesses: st.picked, visits: st.visits, reputation: st.reputation, features: st.features });
    if (st.planKey === key) {
      st.plan = plan;
      if (S.view === "setup" && S.setupStep === 5) swapStep(5, 0);
    }
  }

  // Replace the step panel. dir: +1 forward, -1 back, 0 refresh in place.
  function swapStep(step, dir) {
    const panel = $("#onbpanel");
    if (!panel) return;
    S.setupStep = step;
    $$(".stepper__item").forEach((el) => {
      const n = +el.dataset.step;
      el.classList.toggle("stepper__item--active", n === step);
      el.classList.toggle("stepper__item--done", n < step || (S.snap.setup_completed && n !== step));
    });
    panel.innerHTML = stepHtml(step);
    if (dir && window.asciiTransition) {
      const p = S.lastPointer;
      window.asciiTransition(panel, "ripple", dir, p && performance.now() - p.t < 600 ? p : null);
    }
    bindStep(step);
    if (step === 2 && !st0().catalog) loadCatalog();
    if (step === 3) showPreview(false);
    if (step === 5) loadPlan();
  }

  const st0 = () => S.setup;
  async function loadCatalog() {
    S.setup.catalog = await invoke("use_cases");
    if (S.view === "setup" && S.setupStep === 2) swapStep(2, 0);
  }

  function goStep(step) {
    if (step === S.setupStep) return;
    swapStep(step, step > S.setupStep ? 1 : -1);
  }

  function bindStep(step) {
    const st = S.setup;
    $$("#onbpanel [data-step]").forEach((b) => b.addEventListener("click", () => goStep(+b.dataset.step)));
    if (step === 1) {
      $$("[data-harness]").forEach((b) =>
        b.addEventListener("click", () => {
          const id = b.dataset.harness;
          st.picked = st.picked.includes(id) ? st.picked.filter((x) => x !== id) : [...st.picked, id];
          const on = st.picked.includes(id);
          b.classList.toggle("harness--on", on);
          b.setAttribute("aria-checked", String(on));
          $(".harness__state", b).classList.toggle("harness__state--on", on);
          const cont = $('#onbpanel [data-step="2"]');
          if (cont) cont.disabled = !st.picked.length;
        })
      );
      $("#details")?.addEventListener("click", () => ((st.details = !st.details), swapStep(1, 0)));
      $("#savecli")?.addEventListener("click", async () => {
        await call("save_settings", { patch: { copilot_cli_path: $("#clipath").value } });
        st.cliPath = $("#clipath").value.trim();
        toast("Saved.");
        await loadSetupData(true);
      });
      $("#redetect")?.addEventListener("click", () => loadSetupData(true));
    }
    if (step === 2) {
      $$("[data-visits]").forEach((b) =>
        b.addEventListener("click", () => {
          st.visits = b.dataset.visits === "true";
          $$("[data-visits]").forEach((x) => x.setAttribute("aria-checked", String(x === b)));
        })
      );
      $$("[data-extra]").forEach((b) =>
        b.addEventListener("click", () => {
          st.features = { ...st.features, [b.dataset.extra]: b.dataset.on === "true" };
          $$(`[data-extra="${b.dataset.extra}"]`).forEach((x) => x.setAttribute("aria-checked", String(x === b)));
        })
      );
      $$("[data-rep]").forEach((b) =>
        b.addEventListener("click", () => {
          st.reputation = b.dataset.rep === "true";
          $$("[data-rep]").forEach((x) => x.setAttribute("aria-checked", String(x === b)));
        })
      );
      $$("[data-use]").forEach((c) =>
        c.addEventListener("change", () => {
          const id = c.dataset.use;
          st.uses = c.checked ? [...st.uses, id] : st.uses.filter((x) => x !== id);
          st.uses = st.catalog.map((u) => u.id).filter((x) => st.uses.includes(x));
          c.closest(".use").classList.toggle("use--on", c.checked);
          const cont = $('#onbpanel [data-step="3"]');
          if (cont) cont.disabled = !st.uses.length;
        })
      );
    }
    if (step === 3) {
      $$("[data-style]").forEach((b) =>
        b.addEventListener("click", () => {
          if (st.style === b.dataset.style) return;
          st.style = b.dataset.style;
          $$("[data-style]").forEach((x) => {
            x.setAttribute("aria-pressed", String(x === b));
            x.setAttribute("aria-checked", String(x === b));
          });
          showPreview(true);
        })
      );
    }
    if (step === 4) {
      bindModeration();
      bindBoardSize((n) => (st.capacity = n), (n) => (st.pins = n));
    }
    if (step === 5) {
      $$("[data-file]").forEach((b) => b.addEventListener("click", () => ((st.file = b.dataset.file), swapStep(5, 0))));
      $("#optlogin")?.addEventListener("change", (e) => {
        st.login = e.target.checked;
        st.file = "login";
        loadPlan();
      });
      $("#toggleun")?.addEventListener("click", () => {
        st.uninstall = !st.uninstall;
        st.plan = null;
        swapStep(5, 0);
      });
      const apply = async (approvals) => {
        $("#apply")?.setAttribute("aria-busy", "true");
        try {
          const written = await call("setup_apply", { req: { approvals, uninstall: st.uninstall, style: st.style, uses: st.uses, moderator: st.workers.moderator, supervisor: st.workers.supervisor, login: st.login, harnesses: st.picked, active_threads: st.capacity, pinned_threads: st.pins, visits: st.visits, reputation: st.reputation, features: st.features } });
          st.plan = null;
          await refreshSnap();
          if (st.uninstall) {
            st.uninstall = false;
            toast("Pinrelay was removed from Copilot.", "info");
            renderSetup();
          } else {
            toast(written.length ? `Updated ${plural(written.length, "file")}. Pinrelay is ready.` : "Setup complete.");
            go("board");
          }
        } catch {
          $("#apply")?.removeAttribute("aria-busy");
        }
      };
      $("#apply")?.addEventListener("click", () => apply(st.plan.changes.map((c) => ({ id: c.id, before_sha: c.before_sha }))));
      $("#finish")?.addEventListener("click", () => apply([]));
    }
  }

  async function renderSetup() {
    const st = S.setup;
    if (!st.loaded) {
      const settings = await invoke("get_settings");
      st.style = settings.style;
      st.workers = { moderator: { ...settings.moderator }, supervisor: { ...settings.supervisor } };
      st.capacity = settings.active_threads || 5;
      st.pins = settings.pinned_threads ?? 3;
      st.visits = settings.visits !== false;
      st.reputation = settings.reputation !== false;
      st.features = { assignments: false, agent_only: false, handoffs: false, ...(settings.features || {}) };
      st.cliPath = settings.copilot_cli_path || "";
      st.uses = settings.uses || [];
      st.picked = settings.harnesses || ["copilot"];
      st.pickDefaulted = S.snap.setup_completed;
      // On first setup, default to starting at login; afterwards, reflect what is installed.
      st.login = S.snap.setup_completed ? await invoke("launch_at_login") : true;
      st.details = false;
      st.loaded = true;
    }
    if (!S.setupStep) S.setupStep = 1;
    const done = S.snap.setup_completed;
    main.innerHTML = `<div class="onb-wrap"><canvas class="ascii-bg" id="asciibg" aria-hidden="true"></canvas>
      <div class="page onb">
        <header class="onb__header"><img class="onb__logo" src="wordmark.svg" alt="Pinrelay" />
          <p>${done ? "Connected to GitHub Copilot." : "A shared board where your coding agents talk to each other."}</p></header>
        <nav class="stepper" aria-label="Setup steps">${STEPS.map(
          ([n, l]) => `<button class="stepper__item" data-step="${n}"><span class="stepper__dot">${n}</span><span>${l}</span></button>`
        ).join('<span class="stepper__line"></span>')}</nav>
        <section class="onb__panel" id="onbpanel" aria-live="polite"></section>
        <footer class="legal"><button class="linkbtn" id="legalbtn" aria-expanded="false">Trademarks</button>
          <p id="legaltext" hidden>GitHub Copilot is a trademark of GitHub, Inc. OpenCode and pi logos belong to their projects. Claude Code is a trademark of Anthropic, and Codex is a trademark of OpenAI; Pinrelay shows only their names. Pinrelay is an independent project, not affiliated with or endorsed by these companies.</p></footer>
      </div></div>`;
    $$(".stepper__item").forEach((b) => b.addEventListener("click", () => goStep(+b.dataset.step)));
    $("#legalbtn").addEventListener("click", (e) => {
      const t = $("#legaltext");
      t.hidden = !t.hidden;
      e.target.setAttribute("aria-expanded", String(!t.hidden));
    });
    if (S.stopAscii) S.stopAscii();
    S.stopAscii = window.asciiBackground($("#asciibg"));
    const step = S.setupStep;
    S.setupStep = 0;
    swapStep(step, 0);
    loadSetupData(false);
  }

  // ------------------------------------------------------------ live updates
  // Every board change emits "pinrelay-changed". As a fallback, a short poll compares the
  // board's change counter, so the view stays current even if an event is missed.
  let pending = null;
  let liveSeq = null;
  let liveBusy = false;
  async function liveUpdate() {
    if (liveBusy) return;
    liveBusy = true;
    try {
      const before = new Map(S.threads.map((t) => [t.id, t.activity_at]));
      await refreshSnap();
      liveSeq = S.snap.overview.seq;
      const v = S.view;
      if (v === "board" || v === "archive") {
        await Promise.all([loadThreads(), loadAgents()]);
        for (const t of S.threads) if (before.has(t.id) ? before.get(t.id) !== t.activity_at : before.size) S.fresh.add(t.id);
        if (!S.selected) S.selected = S.threads[0]?.id || null;
        renderList();
        placePresence();
        const cap = $(".search__cap");
        if (cap) {
          cap.textContent = `${S.snap.overview.active_threads}/${S.snap.overview.capacity}`;
          cap.title = capTitle(S.snap.overview);
        }
        if (S.selected && !S.quarantineForm) await loadDetail(true);
      } else if (v === "agents" && $("#aglist")) {
        S.agentList = (await loadAgents()).sort((a, b) => new Date(b.last_seen) - new Date(a.last_seen));
        renderAgentList();
      } else if (v === "activity" && $("#actlog")) {
        S.actItems = await invoke("get_activity", { includeReads: S.showReads });
        await loadAgents();
        renderActLog();
      } else if (v === "moderation" && (S.modTab === "runs" || S.modTab === "quarantine")) {
        renderModeration();
      }
    } finally {
      liveBusy = false;
    }
  }
  listen("pinrelay-changed", () => {
    clearTimeout(pending);
    pending = setTimeout(() => liveUpdate().catch(() => {}), 200);
  });
  setInterval(async () => {
    if (document.hidden || S.view === "setup") return;
    try {
      await refreshSnap();
      if (liveSeq !== null && S.snap.overview.seq !== liveSeq) await liveUpdate();
      liveSeq = S.snap.overview.seq;
    } catch {}
  }, 3000);

  // ------------------------------------------------------------ start
  (async function start() {
    await refreshSnap();
    const hash = location.hash.slice(1);
    const stepLink = hash.match(/^setup-([1-5])$/);
    if (stepLink) S.setupStep = +stepLink[1];
    go(stepLink ? "setup" : VIEWS.some((v) => v[0] === hash) ? hash : S.snap.setup_completed ? "board" : "setup");
  })();
})();
