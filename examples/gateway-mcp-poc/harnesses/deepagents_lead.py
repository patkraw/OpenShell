"""Deep Agent whose built-in `task` subagent tool is replaced by remote MCP tools.

Pinned: deepagents==0.7.22, langchain-openai==1.6.7, langchain-mcp-adapters==0.3.2
"""

import asyncio
import os
import sys

from deepagents import (
    GeneralPurposeSubagentProfile,
    HarnessProfile,
    create_deep_agent,
    register_harness_profile,
)
from langchain_mcp_adapters.client import MultiServerMCPClient
from langchain_openai import ChatOpenAI

MCP_URL = os.environ.get("OPENSHELL_MCP_URL", "http://openshell.local/mcp")
MODE = os.environ.get("SUBAGENT_MODE", "replace")  # "replace" | "supplement"

# ChatOpenAI reports provider "openai", so a provider-level profile matches it.
# Disabling the auto-added general-purpose subagent (and passing no sync
# subagents) means SubAgentMiddleware is never added -> no `task` tool.
if MODE == "replace":
    register_harness_profile(
        "openai",
        HarnessProfile(general_purpose_subagent=GeneralPurposeSubagentProfile(enabled=False)),
    )


async def main(prompt: str) -> None:
    model = ChatOpenAI(
        model="aws/anthropic/bedrock-claude-sonnet-5-5",
        base_url=os.environ.get("MODEL_BASE_URL", "https://inference-api.nvidia.com/v1"),
        api_key=os.environ["OPENAI_API_KEY"],
        use_responses_api=False,  # force /v1/chat/completions
        max_retries=2,
        timeout=300,
    )

    client = MultiServerMCPClient(
        {
            "openshell": {
                "transport": "streamable_http",  # aliases: "streamable-http", "http"
                "url": MCP_URL,
            }
        }
    )
    mcp_tools = await client.get_tools()  # new MCP session per tool call
    print("MCP tools:", [t.name for t in mcp_tools], file=sys.stderr)

    agent = create_deep_agent(
        model=model,
        tools=mcp_tools,
        # Neutral by default, so the matrix measures which spawn path the model picks.
        system_prompt=os.environ.get("EXTRA_SYSTEM_PROMPT") or None,
        # no interrupt_on, no checkpointer -> no human-in-the-loop interrupts
    )

    result = await agent.ainvoke({"messages": [{"role": "user", "content": prompt}]})
    final = result["messages"][-1]
    print(final.text)


if __name__ == "__main__":
    asyncio.run(main(sys.argv[1]))
