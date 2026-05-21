#!/usr/bin/env node
// Stdio-JSON sidecar for CloakBrowser.
// Reads one JSON verb per line from stdin, writes one JSON response per line
// to stdout. Wire-compatible with the agent-browser snapshot/ref protocol so
// `origin-browser`'s router can swap us in mid-session.

import readline from "node:readline";
import { CloakBrowser } from "cloak-browser";

const sessions = new Map();

async function getCtx(sessionId) {
  let c = sessions.get(sessionId);
  if (!c) {
    const browser = await CloakBrowser.launch({ headless: true });
    const page = await browser.newPage();
    c = { browser, page, refCounter: 0, refs: new Map() };
    sessions.set(sessionId, c);
  }
  return c;
}

function newRef(c, locator) {
  const id = `r${c.refCounter++}`;
  c.refs.set(id, locator);
  return id;
}

async function snapshot(c) {
  const status = c.page.lastResponseStatus ?? 200;
  const title = await c.page.title().catch(() => "");
  const html = await c.page.content().catch(() => "");
  return { status, title, html, snapshot: html.slice(0, 4096) };
}

async function handle(msg) {
  const c = await getCtx(msg.session);
  try {
    switch (msg.v) {
      case "open": {
        const resp = await c.page.goto(msg.url, { waitUntil: "domcontentloaded" });
        c.page.lastResponseStatus = resp?.status() ?? 200;
        const snap = await snapshot(c);
        const ref = newRef(c, "body");
        return { ok: true, ref, ...snap };
      }
      case "click": {
        const loc = c.refs.get(msg.ref);
        if (!loc) return { ok: false, error: `unknown ref ${msg.ref}` };
        await c.page.click(loc);
        const snap = await snapshot(c);
        return { ok: true, ref: newRef(c, loc), ...snap };
      }
      case "fill": {
        const loc = c.refs.get(msg.ref);
        if (!loc) return { ok: false, error: `unknown ref ${msg.ref}` };
        await c.page.fill(loc, msg.value);
        const snap = await snapshot(c);
        return { ok: true, ref: msg.ref, ...snap };
      }
      case "extract": {
        const loc = c.refs.get(msg.ref);
        if (!loc) return { ok: false, error: `unknown ref ${msg.ref}` };
        const text = await c.page.locator(loc).innerText();
        return { ok: true, ref: msg.ref, snapshot: text };
      }
      case "snapshot":
        return { ok: true, ...(await snapshot(c)) };
      case "screenshot":
        await c.page.screenshot({ path: msg.path, fullPage: false });
        return { ok: true };
      case "close": {
        await c.browser.close();
        sessions.delete(msg.session);
        return { ok: true };
      }
      default:
        return { ok: false, error: `unknown verb ${msg.v}` };
    }
  } catch (e) {
    return { ok: false, error: String(e?.message ?? e) };
  }
}

const rl = readline.createInterface({ input: process.stdin, terminal: false });
rl.on("line", async (line) => {
  if (!line.trim()) return;
  let msg;
  try { msg = JSON.parse(line); }
  catch (e) { console.log(JSON.stringify({ ok: false, error: `bad json: ${e.message}` })); return; }
  const resp = await handle(msg);
  console.log(JSON.stringify(resp));
});
