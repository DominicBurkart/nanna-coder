import { connect } from "node:net";
import { readFileSync } from "node:fs";

const SOCKET = process.env.NANNA_BROKER_SOCKET ?? "/nanna/broker.sock";
const CAPABILITIES = JSON.parse(
  readFileSync(process.env.NANNA_CAPABILITIES ?? "/nanna/pi/capabilities.json", "utf8"),
);

function call(tool: string, args: unknown): Promise<{ ok: boolean; output: unknown }> {
  return new Promise((resolve, reject) => {
    const socket = connect(SOCKET);
    let buffer = "";
    socket.on("connect", () => socket.write(JSON.stringify({ tool, args }) + "\n"));
    socket.on("data", (chunk) => {
      buffer += chunk.toString("utf8");
      const end = buffer.indexOf("\n");
      if (end >= 0) {
        socket.end();
        try {
          resolve(JSON.parse(buffer.slice(0, end)));
        } catch (error) {
          reject(error);
        }
      }
    });
    socket.on("error", reject);
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
        return { content: [{ type: "text", text }], details: { ok: reply.ok } };
      },
    });
  }
}
