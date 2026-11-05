# SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Drive OpenShell's MCP tools from the host through `openshell mcp serve`.

Run with: uv run --with 'mcp>=2' examples/child-sandboxes/host_client.py <parent>
"""

import asyncio
import os
import sys

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client


async def call(session: ClientSession, tool: str, **arguments) -> dict:
    result = await session.call_tool(tool, arguments)
    reply = result.structured_content or {}
    if result.is_error:
        raise RuntimeError(f"{tool}: {reply.get('error')}: {reply.get('message')}")
    return reply


async def main(parent: str) -> None:
    # The SDK starts servers with a minimal environment; pass ours so
    # OPENSHELL_GATEWAY selects the same gateway as the CLI.
    server = StdioServerParameters(
        command="openshell", args=["mcp", "serve"], env=dict(os.environ)
    )
    async with stdio_client(server) as (read, write):
        async with ClientSession(read, write) as session:
            await session.initialize()
            tools = await session.list_tools()
            print("tools:", ", ".join(tool.name for tool in tools.tools))

            child = await call(session, "create_sandbox", name="py1", parent=parent)
            print("created:", child["name"], "under", child["parent"], child["state"])

            job = await call(
                session,
                "exec",
                name="py1",
                command=["sh", "-c", "sleep 3; echo finished in $(hostname)"],
                background=True,
            )
            while True:
                status = await call(
                    session, "wait", name="py1", job=job["job"], timeout_s=40
                )
                if status["state"] != "running":
                    break
            print("job:", status["state"], status["stdout"].strip())

            await call(session, "delete", name="py1")
            print("deleted: py1")


if __name__ == "__main__":
    asyncio.run(main(sys.argv[1] if len(sys.argv) > 1 else "lead"))
