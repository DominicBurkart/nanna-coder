const net = require("net");
const fs = require("fs");
const SOCK = process.env.NANNA_BROKER_SOCKET;
const LOG = process.env.BROKER_LOG || "/spike/logs/broker.jsonl";
try { fs.unlinkSync(SOCK); } catch (e) {}
net.createServer((c) => {
  let buf = "";
  c.setEncoding("utf8");
  c.on("data", (d) => {
    buf += d;
    const i = buf.indexOf("\n");
    if (i >= 0) {
      const line = buf.slice(0, i);
      fs.appendFileSync(LOG, line + "\n");
      let req = {};
      try { req = JSON.parse(line); } catch (e) {}
      const reply = req.tool === "read_file" ? { ok: true, output: "# README\nhello from the broker" } : { ok: false, output: "not granted", kind: "not_granted", halt: false };
      c.end(JSON.stringify(reply) + "\n");
    }
  });
}).listen(SOCK, () => console.log("broker up"));
