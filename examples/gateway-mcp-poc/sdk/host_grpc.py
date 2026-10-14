"""Use OpenShell's own Python SDK (gRPC) for grants and child sandboxes, as the
user. Run on the host against a dev gateway:

    uv run --frozen python examples/gateway-mcp-poc/sdk/host_grpc.py
"""
import os

from openshell import SandboxClient
from openshell._proto import openshell_pb2

ENDPOINT = os.environ.get("GW_GRPC", "127.0.0.1:18080")
WS = "default"
GRANT = "max_depth=1,max_breadth=2,max_total=3,expires=1h,program=/usr/local/bin/node"

client = SandboxClient(ENDPOINT)

# Create a lead that may create children, with OpenShell's SDK.
spec = openshell_pb2.SandboxSpec(
    template=openshell_pb2.SandboxTemplate(image="pi-agent:local"),
    command=["sleep", "3600"],
)
lead = client.create(workspace=WS, spec=spec, name="grpc-lead", grant=GRANT)
print("created:", lead.name)

grants = [g for g in client.list_grants() if g.holder == "grpc-lead"]
print("grant:", grants[0])

wider = client.update_grant(grants[0].grant_id, max_breadth=3, max_total=4)
print("widened:", wider.max_breadth, wider.max_total)

print("children:", client.list_children("grpc-lead", workspace=WS))
