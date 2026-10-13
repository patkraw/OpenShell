#!/usr/bin/env python3
"""Call the gateway MCP endpoint as the user (dev gateway: unauthenticated local user).
Usage: user-mcp.py <tool> [key=value ...]   (numbers and true/false are converted)"""
import json, os, sys, urllib.request
url = os.environ.get("GW_URL", "http://127.0.0.1:18080/mcp")
tool = sys.argv[1]
def conv(v):
    return int(v) if v.isdigit() else {"true": True, "false": False}.get(v, v)
args = {k: conv(v) for k, v in (a.split("=", 1) for a in sys.argv[2:])}
body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                   "params": {"name": tool, "arguments": args}}).encode()
req = urllib.request.Request(url, body, {"content-type": "application/json"})
res = json.load(urllib.request.urlopen(req))
r = res.get("result", {})
print(json.dumps({"isError": r.get("isError"), **r.get("structuredContent", {})}) if r else json.dumps(res))
