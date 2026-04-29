import json, subprocess, sys, time, os

msgs = [
    {"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}},
    {"jsonrpc":"2.0","method":"notifications/initialized"},
    {"jsonrpc":"2.0","id":2,"method":"tools/list"},
    {"jsonrpc":"2.0","id":3,"method":"resources/list"},
]

env = os.environ.copy()
env.update({"REPO_RECALL_CWD":"/tmp","REPO_RECALL_DB":"/tmp/mcp-smoke.sqlite","REPO_RECALL_REFRESH_INTERVAL_SECS":"0","RUST_LOG":"warn"})

proc = subprocess.Popen(
    ["./target/debug/repo-recall", "mcp"],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    env=env,
)

for m in msgs:
    proc.stdin.write((json.dumps(m) + "\n").encode())
proc.stdin.flush()
time.sleep(2)
proc.kill()
out, err = proc.communicate()

print("=== STDERR (last 20 lines) ===")
print("\n".join(err.decode(errors="replace").splitlines()[-20:]))
print()
print("=== STDOUT (parsed) ===")
for line in out.decode(errors="replace").splitlines():
    if not line.strip(): continue
    try:
        obj = json.loads(line)
        if obj.get("id") == 1:
            r = obj.get("result", {})
            print("initialize ->", r.get("serverInfo"), "caps:", list(r.get("capabilities", {}).keys()))
        elif obj.get("id") == 2:
            tools = obj.get("result", {}).get("tools", [])
            print(f"tools/list -> {len(tools)} tools:")
            for t in tools:
                meta = t.get("_meta", {})
                ui = (meta.get("ui") or {}).get("resourceUri")
                openai_tpl = meta.get("openai/outputTemplate")
                tag = f"  ui={ui}" if ui else ("  ui(openai)=" + openai_tpl if openai_tpl else "")
                print(f"  - {t['name']}{tag}")
        elif obj.get("id") == 3:
            res = obj.get("result", {}).get("resources", [])
            print(f"resources/list -> {len(res)} resources:")
            for r in res:
                print(f"  - {r.get('uri')}  mime={r.get('mimeType')}")
        elif "error" in obj:
            print("ERROR:", obj.get("error"))
    except json.JSONDecodeError:
        print("non-json:", line[:160])
