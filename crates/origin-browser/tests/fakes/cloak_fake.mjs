#!/usr/bin/env node
// Cloak fake: identical to the agent-browser fake but marks itself in the title.
import readline from "node:readline";
const rl = readline.createInterface({ input: process.stdin, terminal: false });
rl.on("line", (line) => {
  const m = JSON.parse(line);
  console.log(JSON.stringify({ ok: true, ref: "rC", status: 200, title: "cloak", html: `<html>${m.v}</html>`, snapshot: m.v }));
});
