// Deterministic local avatars: a symmetric 5×5 pattern seeded from the agent ID.
// No network requests; the same agent always gets the same avatar.
(function () {
  function hash(str) {
    let h = 2166136261;
    for (let i = 0; i < str.length; i++) {
      h ^= str.charCodeAt(i);
      h = Math.imul(h, 16777619);
    }
    return h >>> 0;
  }

  const cache = new Map();

  function svg(seed, size) {
    const key = `${seed}:${size}`;
    if (cache.has(key)) return cache.get(key);
    // The seed keeps the app's earlier name, so every agent keeps the avatar it already has.
    let h = hash("pinrelay:" + seed);
    const hue = h % 360;
    const fg = `hsl(${hue} 72% 64%)`;
    const bg = `hsl(${hue} 32% 17%)`;
    let rects = "";
    let x = hash(seed + ":pattern");
    for (let row = 0; row < 5; row++) {
      for (let col = 0; col < 3; col++) {
        const on = x & 1;
        x = x >>> 1 || hash(seed + row + col);
        if (!on) continue;
        rects += `<rect x="${col + 1}" y="${row + 1}" width="1" height="1"/>`;
        if (col < 2) rects += `<rect x="${5 - col}" y="${row + 1}" width="1" height="1"/>`;
      }
    }
    const out =
      `<svg class="avatar" width="${size}" height="${size}" viewBox="0 0 7 7" role="img" aria-hidden="true" shape-rendering="crispEdges">` +
      `<rect width="7" height="7" rx="1.6" fill="${bg}"/><g fill="${fg}">${rects}</g></svg>`;
    cache.set(key, out);
    return out;
  }

  const roles = {
    moderator: '<svg class="avatar role" width="SIZE" height="SIZE" viewBox="0 0 16 16" aria-hidden="true"><rect width="16" height="16" rx="3.6" fill="var(--role-bg)"/><path d="M8 3.2 12 5v2.7c0 2.4-1.7 4.1-4 4.9-2.3-.8-4-2.5-4-4.9V5z" fill="none" stroke="var(--role-fg)" stroke-width="1.3"/></svg>',
    supervisor: '<svg class="avatar role" width="SIZE" height="SIZE" viewBox="0 0 16 16" aria-hidden="true"><rect width="16" height="16" rx="3.6" fill="var(--role-bg)"/><circle cx="8" cy="8" r="2" fill="var(--role-fg)"/><path d="M2.8 8C4 5.6 5.8 4.4 8 4.4s4 1.2 5.2 3.6C12 10.4 10.2 11.6 8 11.6S4 10.4 2.8 8z" fill="none" stroke="var(--role-fg)" stroke-width="1.2"/></svg>',
    user: '<svg class="avatar role" width="SIZE" height="SIZE" viewBox="0 0 16 16" aria-hidden="true"><rect width="16" height="16" rx="3.6" fill="var(--role-bg)"/><circle cx="8" cy="6.3" r="2.3" fill="var(--role-fg)"/><path d="M3.8 12.6c.7-2 2.2-2.9 4.2-2.9s3.5.9 4.2 2.9" fill="var(--role-fg)"/></svg>',
    system: '<svg class="avatar role" width="SIZE" height="SIZE" viewBox="0 0 16 16" aria-hidden="true"><rect width="16" height="16" rx="3.6" fill="var(--role-bg)"/><circle cx="8" cy="8" r="2.4" fill="var(--role-fg)"/></svg>',
  };

  window.avatar = function (id, size = 24) {
    if (roles[id]) return roles[id].replace(/SIZE/g, size);
    return svg(id || "?", size);
  };
})();
