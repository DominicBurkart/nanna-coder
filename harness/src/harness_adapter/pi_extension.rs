pub(super) const SOURCE: &str = r##"import { connect } from "node:net";
import { readFileSync } from "node:fs";

const SOCKET = process.env.NANNA_BROKER_SOCKET ?? "/nanna/broker.sock";
const CAPABILITIES = JSON.parse(
  readFileSync(process.env.NANNA_CAPABILITIES ?? "/nanna/pi/capabilities.json", "utf8"),
);
const TIMEOUT_MS = 600000;
const MAX_REPLY_BYTES = 1048576;

function call(tool: string, args: unknown): Promise<{ ok: boolean; output: unknown; halt?: boolean }> {
  return new Promise((resolve, reject) => {
    const socket = connect(SOCKET);
    let buffer = "";
    let done = false;
    const finish = (action: () => void) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      socket.destroy();
      action();
    };
    const timer = setTimeout(
      () => finish(() => reject(new Error("broker call timed out"))),
      TIMEOUT_MS,
    );
    socket.setEncoding("utf8");
    socket.on("connect", () => socket.write(JSON.stringify({ tool, args }) + "\n"));
    socket.on("data", (chunk: string) => {
      buffer += chunk;
      if (buffer.length > MAX_REPLY_BYTES) {
        finish(() => reject(new Error("broker reply too large")));
        return;
      }
      const end = buffer.indexOf("\n");
      if (end >= 0) {
        finish(() => {
          try {
            resolve(JSON.parse(buffer.slice(0, end)));
          } catch (error) {
            reject(error);
          }
        });
      }
    });
    socket.on("error", (error: Error) => finish(() => reject(error)));
    socket.on("close", () => finish(() => reject(new Error("broker closed the connection"))));
  });
}

export default function (pi: any) {
  for (const capability of CAPABILITIES) {
    pi.registerTool({
      name: capability.name,
      label: capability.name,
      description: capability.description,
      parameters: capability.parameters,
      async execute(_id: string, params: unknown) {
        const reply = await call(capability.name, params);
        const text = typeof reply.output === "string" ? reply.output : JSON.stringify(reply.output);
        if (!reply.ok) {
          throw new Error(text);
        }
        return { content: [{ type: "text", text }], details: { ok: true } };
      },
    });
  }
}
"##;
