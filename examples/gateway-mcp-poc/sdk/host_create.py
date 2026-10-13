"""Create a sandbox with a grant through the gateway MCP, as the user, with the
official MCP Python SDK. Run on the host:

    uv run --with "mcp==2.3.0" python examples/gateway-mcp-poc/sdk/host_create.py
"""
import asyncio
import json
import os

from mcp import ClientSession
from mcp.client.streamable_http import streamable_http_client

URL = os.environ.get("GW_URL", "http://127.0.0.1:18080/mcp")


async def main() -> None:
    async with streamable_http_client(URL) as streams:
        read, write = streams[0], streams[1]
        async with ClientSession(read, write) as session:
            await session.initialize()

            tools = await session.list_tools()
            print("tools:", [t.name for t in tools.tools])

            result = await session.call_tool(
                "create_sandbox",
                {
                    "name": "sdk-lead",
                    "image": "pi-agent:local",
                    "command": ["sleep", "3600"],
                    "grant": "max_depth=1,max_breadth=2,max_total=3,expires=1h,program=/usr/local/bin/node",
                },
            )
            print("create_sandbox:", "error" if result.is_error else "ok", json.dumps(result.structured_content))

            grants = await session.call_tool("grant_list", {})
            print("grant_list:", json.dumps(grants.structured_content))


asyncio.run(main())
