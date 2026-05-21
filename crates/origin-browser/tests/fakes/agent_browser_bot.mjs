#!/usr/bin/env node
// Always responds with a Cloudflare challenge snapshot.
import readline from "node:readline";
const rl = readline.createInterface({ input: process.stdin, terminal: false });
rl.on("line", () => {
  console.log(JSON.stringify({
    ok: true,
    ref: "r0",
    status: 403,
    title: "Just a moment...",
    html: "<script>/* cf-chl- */</script>",
    snapshot: "challenge",
  }));
});
