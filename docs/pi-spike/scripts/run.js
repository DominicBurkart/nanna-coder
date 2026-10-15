const { spawn } = require("child_process");
const fs = require("fs");
const OUT = process.env.RPC_OUT || "/spike/logs/rpc.jsonl";
const extra = (process.env.PI_EXTRA_ARGS || "").split(" ").filter(Boolean);
const args = ["--mode","rpc","--no-session","--provider","nanna","--model","gemma4:e4b","--no-builtin-tools","--no-extensions","--extension","/nanna/pi/extensions/nanna-capabilities.ts","--no-skills","--no-prompt-templates","--no-context-files","--no-mcp","--no-approve","--offline"];
const final = process.env.PI_ARGS_OVERRIDE ? process.env.PI_ARGS_OVERRIDE.split(" ") : args.concat(extra);
const child = spawn("/inst/node_modules/.bin/pi", final, { cwd: process.env.PI_CWD || "/nanna/work", env: process.env });
let buf = "";
let settled = false;
const out = fs.createWriteStream(OUT);
child.stdout.on("data", (d) => {
  buf += d;
  let i;
  while ((i = buf.indexOf("\n")) >= 0) {
    const line = buf.slice(0, i); buf = buf.slice(i + 1);
    out.write(line + "\n");
    try { const ev = JSON.parse(line); if (ev.type === "agent_settled") { settled = true; child.stdin.end(); } } catch (e) {}
  }
});
child.stderr.on("data", (d) => fs.appendFileSync(OUT + ".stderr", d));
child.on("exit", (code, sig) => { fs.writeFileSync(OUT + ".exit", JSON.stringify({ code, sig, settled })); process.exit(0); });
setTimeout(() => { fs.writeFileSync(OUT + ".timeout", "1"); child.kill("SIGKILL"); }, Number(process.env.RUN_TIMEOUT_MS || 60000));
if (!process.env.NO_PROMPT) child.stdin.write(JSON.stringify({ id: "task", type: "prompt", message: "Read README.md and summarise it." }) + "\n");
