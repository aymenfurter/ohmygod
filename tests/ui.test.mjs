import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { createServer } from "node:http";
import { readFile, mkdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";

const ui = new URL("../ui/", import.meta.url);
let browser, server, origin;
before(async () => {
  server = createServer(async (req, res) => {
    const name = new URL(req.url, "http://localhost").pathname.slice(1) || "index.html";
    if (!/^[\w.-]+$/.test(name)) return res.writeHead(404).end();
    try {
      const body = await readFile(new URL(name, ui));
      res.setHeader("Content-Type", name.endsWith(".js") ? "text/javascript" : name.endsWith(".css") ? "text/css" : name.endsWith(".svg") ? "image/svg+xml" : "text/html");
      res.end(body);
    } catch {
      res.writeHead(404).end();
    }
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  origin = `http://127.0.0.1:${server.address().port}`;
  browser = await chromium.launch();
});
after(async () => {
  await browser?.close();
  await new Promise((resolve) => server.close(resolve));
});

async function pageFor(platform, width = 1280) {
  const page = await browser.newPage({ viewport: { width, height: 820 } });
  await page.addInitScript((platform) => {
    window.__calls = [];
    window.__listeners = {};
    window.__TAURI__ = {
      core: {
        invoke: async (name) => {
          window.__calls.push(name);
          if (name === "about_info") {
            if (window.__failAbout) throw "Build information is unavailable. Restart the app.";
            return { name: "OH MY GOD!", version: "0.2.0", revision: "123456abcdef", platform, architecture: platform === "macos" ? "aarch64" : "x86_64", license: "MIT" };
          }
          if (name === "snapshot") return {
            setup_completed: true, style: "casual", instructions: { outdated: false },
            overview: { seq: 0, active_threads: 0, capacity: 5, pinned_threads: 0, pin_capacity: 3 },
            server: { listening: true, access_running: true, agents: [], agents_active: 0 },
            sync: { state: "local" }, workers: [],
          };
          if (["list_threads", "list_agents", "agent_ladder"].includes(name)) return [];
          if (name === "open_releases") return;
          throw `Unexpected test command: ${name}`;
        },
      },
      event: { listen: async (name, callback) => { window.__listeners[name] = callback; } },
    };
  }, platform);
  await page.goto(origin);
  await page.waitForSelector(".board");
  return page;
}

for (const platform of ["windows", "macos"]) {
  test(`About displays ${platform} details and restores keyboard focus`, async () => {
    const page = await pageFor(platform);
    try {
      await page.getByRole("button", { name: "About OH MY GOD!", exact: true }).click();
      const dialog = page.getByRole("dialog", { name: "About OH MY GOD!" });
      assert.equal(await dialog.isVisible(), true);
      assert.equal(await page.locator("#about-version").textContent(), "0.2.0");
      assert.match(await page.locator("#about-platform").textContent(), platform === "macos" ? /macOS \(aarch64\)/ : /Windows \(x86_64\)/);
      await page.getByRole("button", { name: "Downloads", exact: true }).click();
      assert.equal(await page.evaluate(() => window.__calls.includes("open_releases")), true);
      await mkdir(fileURLToPath(new URL("../test-results/", import.meta.url)), { recursive: true });
      await page.screenshot({ path: fileURLToPath(new URL(`../test-results/about-${platform}.png`, import.meta.url)) });
      await page.keyboard.press("Escape");
      assert.equal(await dialog.isVisible(), false);
      assert.equal(await page.locator("#aboutbtn").evaluate((el) => document.activeElement === el), true);
      const padding = await page.locator(".topbar").evaluate((el) => getComputedStyle(el).paddingLeft);
      assert.equal(padding, platform === "macos" ? "92px" : "16px");
      await page.evaluate(() => window.__listeners["ohmygod-about"]());
      await dialog.waitFor();
      await page.getByRole("button", { name: "Close", exact: true }).click();
      assert.equal(await dialog.isVisible(), false);
    } finally {
      await page.close();
    }
  });
}

test("About is available during setup, fits small windows, and closes on an outside press", async () => {
  const page = await pageFor("windows", 360);
  try {
    await page.evaluate(() => document.body.classList.add("onboarding"));
    await page.locator("#aboutbtn").click();
    const bounds = await page.locator("#about").boundingBox();
    assert.ok(bounds.x >= 0 && bounds.x + bounds.width <= 360);
    await page.mouse.click(2, 2);
    assert.equal(await page.locator("#about").isVisible(), false);
  } finally {
    await page.close();
  }
});

test("An About error is shown instead of an empty dialog", async () => {
  const page = await pageFor("windows");
  try {
    await page.evaluate(() => { window.__failAbout = true; });
    await page.locator("#aboutbtn").click();
    await page.locator(".toast--error").waitFor();
    assert.match(await page.locator(".toast--error").textContent(), /Restart the app/);
    assert.equal(await page.locator("#about").isVisible(), false);
  } finally {
    await page.close();
  }
});
