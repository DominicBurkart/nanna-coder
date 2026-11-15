const http = require("http");
const fs = require("fs");
const LOG = process.env.GATEWAY_LOG || "/spike/logs/gateway.jsonl";
const MODE = process.env.GATEWAY_MODE || "tool_then_text";
let calls = 0;
function sse(res, chunks) {
  res.writeHead(200, { "content-type": "text/event-stream" });
  for (const c of chunks) res.write("data: " + JSON.stringify(c) + "\n\n");
  res.write("data: [DONE]\n\n");
  res.end();
}
const base = (delta, finish) => ({ id: "chatcmpl-1", object: "chat.completion.chunk", created: 1, model: "gemma4:e4b", choices: [{ index: 0, delta, finish_reason: finish }] });
http.createServer((req, res) => {
  let body = "";
  req.on("data", (d) => (body += d));
  req.on("end", () => {
    if (req.url.endsWith("/models")) { res.writeHead(200, {"content-type":"application/json"}); res.end(JSON.stringify({data:[{id:"gemma4:e4b",object:"model"}]})); return; }
    let parsed = null;
    try { parsed = JSON.parse(body); } catch (e) {}
    calls += 1;
    fs.appendFileSync(LOG, JSON.stringify({ n: calls, url: req.url, tools: parsed && parsed.tools ? parsed.tools.map((t) => (t.function ? t.function.name : t.name)) : null, system: parsed && parsed.messages ? (parsed.messages.find((m) => m.role === "system") || {}).content : null, messages: parsed && parsed.messages ? parsed.messages.map((m) => ({ role: m.role, content: typeof m.content === "string" ? m.content.slice(0, 200) : m.content })) : null, stream: parsed && parsed.stream }) + "\n");
    if (MODE === "error") { res.writeHead(500, {"content-type":"application/json"}); res.end(JSON.stringify({error:{message:"boom"}})); return; }
    const hasToolResult = parsed && parsed.messages && parsed.messages.some((m) => m.role === "tool");
    if (!hasToolResult && MODE === "tool_then_text") {
      sse(res, [
        base({ role: "assistant", content: null, tool_calls: [{ index: 0, id: "call_1", type: "function", function: { name: "read_file", arguments: "" } }] }, null),
        base({ tool_calls: [{ index: 0, function: { arguments: JSON.stringify({ path: "README.md" }) } }] }, null),
        base({}, "tool_calls"),
      ]);
    } else {
      sse(res, [base({ role: "assistant", content: "done" }, null), base({}, "stop")]);
    }
  });
}).listen(18080, "127.0.0.1", () => console.log("gateway up"));
