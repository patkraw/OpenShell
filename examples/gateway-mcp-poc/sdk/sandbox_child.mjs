// Inside a sandbox started with --grant: create a tool sandbox through the
// supervisor door with the official MCP TypeScript SDK, run a command in it,
// read the result and stop it. No credential: the door adds the sandbox's token.
//
//   openshell -g docker-dev sandbox exec <lead> -- node /usr/local/lib/gateway-mcp/sandbox_child.mjs
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";

const url = new URL(process.env.MCP_URL ?? "http://openshell.local/mcp");
const client = new Client({ name: "sdk-example", version: "0.1.0" });
await client.connect(new StreamableHTTPClientTransport(url));

const call = async (name, args = {}) => {
  const r = await client.callTool({ name, arguments: args });
  console.log(`${name}:`, r.isError ? "error" : "ok", JSON.stringify(r.structuredContent));
  return r.structuredContent;
};

const { tools } = await client.listTools();
console.log("tools:", tools.map((t) => t.name).join(", "));
await call("get_limits");
await call("create_sandbox", { name: "sdk-tool", kind: "tool", request_id: "sdk-tool-1" });
await call("wait", { handle: "sdk-tool", timeout_s: 120 });
await call("exec", { handle: "sdk-tool", command: ["sh", "-c", "echo hello from $(hostname)"] });
await call("stop_sandbox", { handle: "sdk-tool" });
await client.close();
