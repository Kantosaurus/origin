#!/usr/bin/env node
// Tiny deterministic agent-browser stand-in: echo verbs as snapshots.
import readline from "node:readline";
const rl = readline.createInterface({ input: process.stdin, terminal: false });
rl.on("line", (line) => {
  const m = JSON.parse(line);
  const resp = { ok: true, ref: "r0", status: 200, title: "fake", html: `<html>${m.v}</html>`, snapshot: m.v };
  console.log(JSON.stringify(resp));
});
