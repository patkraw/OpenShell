// Minimal MCP client for the gateway MCP POC: talks JSON-RPC to the
// supervisor door at http://openshell.local/mcp (no credential needed).
// Usage: node mcp-client.mjs <method-or-tool> [json-args]
//   node mcp-client.mjs tools/list
//   node mcp-client.mjs get_limits
//   node mcp-client.mjs create_sandbox '{"command":["sleep","300"]}'
const url = process.env.MCP_URL ?? "http://openshell.local/mcp";
const [what = "tools/list", rawArgs = "{}"] = process.argv.slice(2);
let id = 0;
async function rpc(method, params) {
  const res = await fetch(url, {
    method: "POST",
    headers: { "content-type": "application/json", accept: "application/json, text/event-stream" },
    body: JSON.stringify({ jsonrpc: "2.0", id: ++id, method, params }),
  });
  const text = await res.text();
  if (!res.ok) return { http_status: res.status, body: text };
  return JSON.parse(text);
}
await rpc("initialize", { protocolVersion: "2025-11-25", capabilities: {}, clientInfo: { name: "poc", version: "0" } });
const out = what.includes("/")
  ? await rpc(what, JSON.parse(rawArgs))
  : await rpc("tools/call", { name: what, arguments: JSON.parse(rawArgs) });
const r = out.result;
if (r?.tools) console.log(JSON.stringify(r.tools.map((t) => t.name)));
else if (r?.structuredContent) console.log(JSON.stringify({ isError: r.isError, ...r.structuredContent }));
else console.log(JSON.stringify(out));
