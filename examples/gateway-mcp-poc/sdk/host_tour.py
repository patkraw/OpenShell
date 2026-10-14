"""A tour of the child-sandbox and grant functions in OpenShell's Python SDK
(gRPC), as the user on the host. Run against a dev gateway:

    uv run --frozen python examples/gateway-mcp-poc/sdk/host_tour.py
"""
import json
import os

from openshell import SandboxClient
from openshell._proto import openshell_pb2

ENDPOINT = os.environ.get("GW_GRPC", "127.0.0.1:18080")
WS = "default"
NODE = "/usr/local/bin/node"
MCP = "/usr/local/lib/gateway-mcp/mcp-client.mjs"

client = SandboxClient(ENDPOINT)
spec = openshell_pb2.SandboxSpec(
    template=openshell_pb2.SandboxTemplate(image="pi-agent:local"),
    command=["sleep", "3600"],
)

# 1. A lead that may create children: one grant, set at create.
client.create(
    workspace=WS,
    spec=spec,
    name="sdk-lead",
    grant=f"max_depth=1,max_breadth=2,max_total=3,expires=1h,program={NODE}",
)
client.wait_ready("sdk-lead", workspace=WS)
grant = next(g for g in client.list_grants() if g.holder == "sdk-lead")
print("grant:", grant.grant_id, grant.max_depth, grant.max_breadth, grant.max_total)

# 2. The lead's agent creates a child through the gateway MCP (here we run
#    the small MCP client inside the lead, as an agent would).
out = client.exec(
    "sdk-lead",
    ["node", MCP, "create_sandbox", json.dumps({"name": "sdk-worker", "command": ["sleep", "600"]})],
    workspace=WS,
)
print("child:", out.stdout.strip())
print("children:", [(c.handle, c.kind) for c in client.list_children("sdk-lead", workspace=WS)])

# 3. Widen the grant; it applies from the next call in the tree.
wider = client.update_grant(grant.grant_id, max_breadth=3, max_total=4)
print("widened:", wider.max_breadth, wider.max_total)

# 4. Attach a grant to a sandbox that is already running.
client.create(workspace=WS, spec=spec, name="sdk-plain")
client.wait_ready("sdk-plain", workspace=WS)
attached = client.create_grant("sdk-plain", workspace=WS, program=NODE, max_total=2)
print("attached:", attached.grant_id, "to", attached.holder)

# 5. Revoke, then clean up (deleting a parent stops its children first).
print("revoked:", client.revoke_grant(attached.grant_id))
for name in ("sdk-lead", "sdk-plain"):
    client.delete(name, workspace=WS, allow_missing=True)
print("deleted")
