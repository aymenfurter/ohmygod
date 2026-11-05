// Setup background: agents radiate waves of attention; where waves meet they interfere and brighten.
// The renderer batches characters by color, so the full-screen field stays cheap.
// Everything dims smoothly near the setup content so text stays readable.
(function () {
  const FONT = 10;
  const LINE = 12;
  const AURORA = [[236, 178, 56], [255, 134, 120], [214, 112, 214], [130, 128, 255], [72, 200, 224], [84, 206, 126], [236, 178, 56]];

  const clamp = (v, a, b) => (v < a ? a : v > b ? b : v);
  const rand = (a, b) => a + Math.random() * (b - a);
  const smooth = (e0, e1, x) => {
    const t = clamp((x - e0) / (e1 - e0), 0, 1);
    return t * t * (3 - 2 * t);
  };
  const mix = (a, b, t) => [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t];
  function grad(stops, t) {
    const x = clamp(t, 0, 1) * (stops.length - 1);
    const i = Math.min(stops.length - 2, Math.floor(x));
    return mix(stops[i], stops[i + 1], x - i);
  }
  const loop = (stops, t) => grad(stops, ((t % 1) + 1) % 1);

  // ------------------------------------------------------------------ renderer
  function stage(canvas, opts) {
    const ctx = canvas.getContext("2d");
    const floor = opts.floor ?? 0.04;
    const S = { ctx, w: 0, h: 0, cw: 6, cols: 0, rows: 0, mask: new Float32Array(0), blocked: [], buckets: new Map() };
    S.maskAt = (x, y) => {
      let d = 1e9;
      for (const b of S.blocked) {
        const dx = Math.max(b.x - x, 0, x - (b.x + b.w));
        const dy = Math.max(b.y - y, 0, y - (b.y + b.h));
        d = Math.min(d, Math.hypot(dx, dy));
      }
      return floor + (1 - floor) * smooth(0, 90, d);
    };
    S.measure = () => {
      const root = canvas.parentElement;
      const base = canvas.getBoundingClientRect();
      S.blocked = root
        ? [...root.querySelectorAll(opts.mask || ".onb__header, .stepper, .onb__panel, .legal")]
            .map((el) => {
              const r = el.getBoundingClientRect();
              return { x: r.left - base.left, y: r.top - base.top, w: r.width, h: r.height };
            })
            .filter((r) => r.w > 0 && r.h > 0)
        : [];
      const m = new Float32Array(S.cols * S.rows);
      for (let r = 0; r < S.rows; r++) for (let c = 0; c < S.cols; c++) m[r * S.cols + c] = S.maskAt((c + 0.5) * S.cw, (r + 0.5) * LINE);
      S.mask = m;
    };
    S.resize = () => {
      const dpr = window.devicePixelRatio || 1;
      S.w = canvas.clientWidth;
      S.h = canvas.clientHeight;
      canvas.width = Math.max(1, S.w * dpr);
      canvas.height = Math.max(1, S.h * dpr);
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.font = `${FONT}px "JetBrains Mono", "SF Mono", Menlo, monospace`;
      ctx.textBaseline = "top";
      S.cw = ctx.measureText("M").width || 6;
      S.cols = Math.ceil(S.w / S.cw);
      S.rows = Math.ceil(S.h / LINE);
      S.measure();
    };
    // Characters are grouped by quantized color and alpha, then drawn with one fillStyle per group.
    S.put = (x, y, str, col, a) => {
      if (a < 0.012) return;
      const k = `${col[0] >> 3},${col[1] >> 3},${col[2] >> 3},${Math.round(clamp(a, 0, 1) * 48)}`;
      let b = S.buckets.get(k);
      if (!b) S.buckets.set(k, (b = []));
      b.push(x, y, str);
    };
    S.flush = () => {
      ctx.clearRect(0, 0, S.w, S.h);
      for (const [k, arr] of S.buckets) {
        const [r, g, b, a] = k.split(",").map(Number);
        ctx.fillStyle = `rgba(${r << 3},${g << 3},${b << 3},${a / 48})`;
        for (let i = 0; i < arr.length; i += 3) ctx.fillText(arr[i + 2], arr[i], arr[i + 1]);
      }
      S.buckets.clear();
    };
    return S;
  }

  // ------------------------------------------------------------------ field
  // Each agent radiates a wave of attention. Where waves meet, they interfere and brighten:
  // collaboration as constructive interference, rendered as a colorful ASCII density field.
  // No dot characters: low intensity stays blank, which keeps the field calm.
  const RAMP = " -=+*#%@";
  const RING = 110;
  const outCubic = (t) => 1 - Math.pow(1 - clamp(t, 0, 1), 3);
  const field = {
    fps: 24,
    init(S, st) {
      st.src = Array.from({ length: 5 }, (_, i) => ({ fx: rand(0.6, 1.3), fy: rand(0.6, 1.3), px: rand(0, 6.28), py: rand(0, 6.28), sp: rand(0.8, 1.3) * (i % 2 ? 1 : -1) }));
    },
    step(S, st, now) {
      for (const s of st.src) {
        s.x = S.w * (0.5 + 0.47 * Math.sin(now * 0.000095 * s.fx + s.px));
        s.y = S.h * (0.5 + 0.44 * Math.sin(now * 0.00012 * s.fy + s.py));
      }
    },
    draw(S, st, now) {
      const src = st.src;
      const len = RAMP.length;
      // A pulse is a ring that travels through the field. Inside the ring the field brightens,
      // also over the setup content, so the step change and the background are one effect.
      const P = st.pulse;
      let pR = -1;
      let pAmp = 0;
      if (P) {
        const t = (now - P.t0) / P.ms;
        if (t >= 1) st.pulse = null;
        else {
          pR = P.Rmax * outCubic(t);
          pAmp = Math.pow(1 - t, 1.2);
        }
      }
      for (let r = 0; r < S.rows; r++) {
        const y = (r + 0.5) * LINE;
        for (let c = 0; c < S.cols; c++) {
          const m = S.mask[r * S.cols + c];
          const x = (c + 0.5) * S.cw;
          let ring = 0;
          if (pR >= 0) {
            const dd = pR - Math.hypot(x - P.x, y - P.y);
            // The ring is strongest over the content and dissolves into the plain field outside it.
            if (dd >= 0 && dd < RING) ring = Math.pow(1 - dd / RING, 1.6) * pAmp * (1 - smooth(0.2, 1, m));
          }
          if (m < 0.08 && ring < 0.04) continue;
          let v = 0;
          let near = 0;
          for (let i = 0; i < src.length; i++) {
            const s = src[i];
            const d = Math.hypot(x - s.x, y - s.y);
            const fall = Math.exp(-d / 300);
            v += Math.cos(d * 0.05 - now * 0.0024 * s.sp) * fall;
            if (fall > near) near = fall;
          }
          let I = clamp(0.5 + v * 0.3, 0, 1);
          I = clamp(I * I + ring * 0.9, 0, 1);
          if (I < 0.1) continue;
          const ch = RAMP[Math.min(len - 1, Math.floor(I * len))];
          const col = loop(AURORA, c / S.cols * 0.45 + r / S.rows * 0.3 + I * 0.25 + now * 0.00004);
          S.put(c * S.cw, r * LINE, ch, col, ((0.04 + I * 0.34) * Math.max(m, ring) * (0.55 + near * 0.6) + ring * 0.4) * S.gain);
        }
      }
    },
  };

  let active = null;

  // opts: mask (selector of content to keep clear), floor (field strength under content),
  // gain (overall strength), fps.
  function start(canvas, opts = {}) {
    const eff = { ...field, fps: opts.fps || field.fps };
    const S = stage(canvas, opts);
    S.gain = opts.gain ?? 1;
    const st = {};
    let raf = 0;
    let last = 0;
    let prev = performance.now();
    let lastMeasure = 0;
    let stopped = false;
    S.resize();
    eff.init(S, st, performance.now());
    const me = { canvas, st };
    active = me;
    const ro = new ResizeObserver(() => {
      S.resize();
    });
    ro.observe(canvas);
    const frame = (now) => {
      if (stopped) return;
      raf = requestAnimationFrame(frame);
      if (now - last < 1000 / (st.pulse ? 60 : eff.fps) - 2) return;
      const dt = Math.min(80, now - prev);
      prev = now;
      last = now;
      if (now - lastMeasure > 350) {
        S.measure();
        lastMeasure = now;
      }
      eff.step(S, st, now, dt);
      eff.draw(S, st, now);
      S.flush();
    };
    const onVis = () => {
      cancelAnimationFrame(raf);
      if (!document.hidden) {
        prev = performance.now();
        raf = requestAnimationFrame(frame);
      }
    };
    document.addEventListener("visibilitychange", onVis);
    raf = requestAnimationFrame(frame);
    return () => {
      stopped = true;
      if (active === me) active = null;
      cancelAnimationFrame(raf);
      ro.disconnect();
      document.removeEventListener("visibilitychange", onVis);
      S.ctx.clearRect(0, 0, S.w, S.h);
    };
  }

  // ------------------------------------------------------------------ transitions
  const TRAIL = "@%#*+=-";
  const hash = (c, r) => {
    let h = Math.imul(c, 374761393) + Math.imul(r, 668265263);
    h = Math.imul(h ^ (h >>> 13), 1274126177);
    return ((h ^ (h >>> 16)) >>> 0) / 4294967296;
  };
  const inOut = (t) => (t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2);

  // Step change: a ripple starts at the click, reveals the new step, and continues through the
  // background field until it fades. The ring is drawn by the field itself (see field.draw).
  function ripple(panel, origin, opts) {
    const wrap = panel.closest(".onb-wrap");
    if (!wrap || !active) return opts.done && opts.done();
    const wr = wrap.getBoundingClientRect();
    const pr0 = panel.getBoundingClientRect();
    const o = origin || { x: pr0.left + pr0.width / 2, y: pr0.top + Math.min(pr0.height / 2, 120) };
    const px = o.x - wr.left;
    const py = o.y - wr.top;
    // The ring ends a little past the farthest panel corner, where the field takes over again.
    const ox = o.x - pr0.left;
    const oy = o.y - pr0.top;
    const Rmax = Math.hypot(Math.max(ox, pr0.width - ox), Math.max(oy, pr0.height - oy)) + RING * 1.6;
    const ms = opts.ms || 900;
    const t0 = performance.now();
    active.st.pulse = { x: px, y: py, t0, ms, Rmax };
    const ps = panel.style;
    let raf = 0;
    const stop = () => {
      cancelAnimationFrame(raf);
      ps.clipPath = "";
      panel._fxStop = null;
      if (opts.done) opts.done();
    };
    panel._fxStop = stop;
    const frame = (now) => {
      const pr = panel.getBoundingClientRect();
      const cx = o.x - pr.left;
      const cy = o.y - pr.top;
      const r = Rmax * outCubic((now - t0) / ms) - RING * 0.3;
      if (r > Math.hypot(Math.max(cx, pr.width - cx), Math.max(cy, pr.height - cy))) return stop();
      ps.clipPath = `circle(${Math.max(0, r)}px at ${cx}px ${cy}px)`;
      raf = requestAnimationFrame(frame);
    };
    frame(t0);
  }

  // Style preview: the new layer is revealed left to right behind a band of field characters.
  function sweep(el, dir, opts) {
    const wrap = el.closest(".onb-wrap") || el.parentElement;
    const canvas = document.createElement("canvas");
    canvas.className = "step-fx";
    canvas.setAttribute("aria-hidden", "true");
    wrap.appendChild(canvas);
    const ctx = canvas.getContext("2d");
    const ps = el.style;
    const ms = opts.ms || 520;
    const band = 110;
    let W = 0, H = 0, cw = 6;
    const layout = () => {
      const wr = wrap.getBoundingClientRect();
      const pr = el.getBoundingClientRect();
      canvas.style.left = `${pr.left - wr.left}px`;
      canvas.style.top = `${pr.top - wr.top}px`;
      if (W !== pr.width || H !== pr.height) {
        W = pr.width;
        H = pr.height;
        const dpr = window.devicePixelRatio || 1;
        canvas.style.width = `${W}px`;
        canvas.style.height = `${H}px`;
        canvas.width = W * dpr;
        canvas.height = H * dpr;
        ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
        ctx.font = `${FONT}px "JetBrains Mono", "SF Mono", Menlo, monospace`;
        ctx.textBaseline = "top";
        cw = ctx.measureText("M").width || 6;
      }
    };
    const draw = (t) => {
      const cols = Math.ceil(W / cw);
      const rows = Math.ceil(H / LINE);
      const front = -band + (W + band * 2) * inOut(t);
      const pos = dir < 0 ? W - front : front;
      const shown = clamp(front / W, 0, 1) * 100;
      ps.clipPath = dir > 0 ? `inset(0 ${100 - shown}% 0 0)` : `inset(0 0 0 ${100 - shown}%)`;
      ctx.clearRect(0, 0, W, H);
      for (let r = 0; r < rows; r++)
        for (let c = 0; c < cols; c++) {
          const d = dir < 0 ? (c + 0.5) * cw - pos : pos - (c + 0.5) * cw;
          if (d < 0 || d > band) continue;
          const I = 1 - d / band;
          if (hash(c, r + Math.floor(t * 24)) > I + 0.15) continue;
          const col = loop(AURORA, (c / cols) * 0.5 + (r / rows) * 0.3 + t * 0.25);
          ctx.fillStyle = `rgba(${col[0] | 0},${col[1] | 0},${col[2] | 0},${Math.min(1, 0.15 + I * 0.75)})`;
          ctx.fillText(TRAIL[Math.min(TRAIL.length - 1, Math.floor((1 - I) * TRAIL.length + hash(c + Math.floor(t * 30), r) * 1.5))], c * cw, r * LINE);
        }
    };
    let raf = 0;
    const t0 = performance.now();
    const stop = () => {
      cancelAnimationFrame(raf);
      canvas.remove();
      ps.clipPath = "";
      el._fxStop = null;
      if (opts.done) opts.done();
    };
    el._fxStop = stop;
    const frame = (now) => {
      const t = Math.min(1, (now - t0) / ms);
      layout();
      draw(t);
      if (t < 1) raf = requestAnimationFrame(frame);
      else stop();
    };
    layout();
    draw(0);
    raf = requestAnimationFrame(frame);
  }

  // kind: "ripple" (step change) or "sweep" (style preview).
  // opts.ms overrides the duration; opts.done runs when the effect ends or is replaced.
  function transition(el, kind, dir = 1, origin = null, opts = {}) {
    if (el._fxStop) el._fxStop();
    if (kind === "sweep") sweep(el, dir, opts);
    else ripple(el, origin, opts);
  }

  window.asciiBackground = start;
  window.asciiTransition = transition;
  // Field gradient color for t in [0, 1], for UI accents that match the background.
  window.asciiColor = (t) => {
    const c = loop(AURORA, t);
    return `rgb(${c[0] | 0},${c[1] | 0},${c[2] | 0})`;
  };
})();
