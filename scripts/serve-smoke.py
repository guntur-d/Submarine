"""Gateway smoke suite: exercises `submarine serve` end to end.

Usage: serve-smoke.py <ws-url-with-token>
Covers: app_info, profiles, locked-vault fail-closed, unknown/malformed
frames, event fan-out, select/unlock flow, read-model fail-closed, live
refused-connect driver (real SSH worker events), terminal + SFTP
fail-closed, HTTP static/401/403 surface, upload validation + temp
hygiene, auth continuations, persist, tunnels, vault CRUD fail-closed,
CLOUD_ONLY / DESKTOP_ONLY / MOVED arms, origin gate, vault-locked gates,
Bearer parity.

Frontend-shaped args throughout (camelCase): the gateway must accept
what the desktop bridge accepts — a snake_case-only test hid a real bug.
"""
import glob
import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

import websocket

url = sys.argv[1]
ws = websocket.create_connection(url, timeout=15)
next_id = 0


def call(cmd, args=None):
    global next_id
    next_id += 1
    ws.send(json.dumps({"id": next_id, "cmd": cmd, "args": args or {}}))
    resp = json.loads(ws.recv())
    assert resp["id"] == next_id, f"id mismatch: {resp}"
    return resp


# 1. app_info — stateless, must succeed with real build metadata.
r = call("app_info")
assert r["ok"], f"app_info failed: {r}"
print("app_info:", r["result"]["version"], r["result"]["github_repo_url"])
assert r["result"]["version"], "empty version"

# 2. list_profiles — reads the real profile dir.
r = call("list_profiles")
assert r["ok"], f"list_profiles failed: {r}"
print("list_profiles:", r["result"])
assert isinstance(r["result"], list)

# 3. get_servers with no unlocked profile — must fail CLOSED, not crash.
r = call("get_servers")
assert not r["ok"], f"get_servers should fail with no open vault: {r}"
assert "DATABASE_NOT_INITIALIZED" in r["error"], f"unexpected error: {r}"
print("get_servers (locked) correctly refused:", r["error"])

# 4. Unknown command — structured error, connection stays alive.
r = call("nope_not_a_command")
assert not r["ok"] and "UNKNOWN_COMMAND" in r["error"], f"bad unknown-cmd: {r}"
print("unknown cmd correctly refused:", r["error"])

# 5. Malformed frame — structured BAD_REQUEST, connection stays alive.
ws.send("this is not json")
r = json.loads(ws.recv())
assert not r["ok"] and "BAD_REQUEST" in r["error"], f"bad frame handling: {r}"
print("malformed frame correctly refused:", r["error"])

# 6. Still alive after all that — one more real call.
r = call("app_info")
assert r["ok"], f"connection died: {r}"
print("connection healthy after errors.")

# 7. Event fan-out — sub, trigger a real emitting command, assert delivery.
sid = "serve-smoke-1"
ws.send(json.dumps({"type": "sub", "events": [f"session-disconnected-{sid}"]}))
ack = json.loads(ws.recv())
assert ack.get("type") == "sub-ack", f"expected sub-ack: {ack}"
assert f"session-disconnected-{sid}" in ack["events"], f"sub-ack missing event: {ack}"
r = call("disconnect_session", {"sessionId": sid})
assert r["ok"], f"disconnect_session failed: {r}"
evt = json.loads(ws.recv())
assert evt.get("event") == f"session-disconnected-{sid}", f"wrong event: {evt}"
assert evt["payload"]["reason"] == "User disconnected", f"wrong payload: {evt}"
assert evt["payload"]["user_initiated"] is True, f"wrong payload: {evt}"
print("event fan-out works:", evt["event"], "->", evt["payload"]["reason"])

# 8. Profile select + unlock flow (wrong password fails closed).
r = call("select_profile", {"name": "gtr"})
assert r["ok"] and r["result"] is True, f"select_profile failed: {r}"
print("select_profile gtr -> exists:", r["result"])
r = call("check_db_exists")
assert r["ok"] and r["result"] is True, f"check_db_exists failed: {r}"
print("check_db_exists -> True (profile selected, vault closed)")
r = call("setup_master_db", {"password": "definitely-wrong-password"})
assert not r["ok"], f"wrong password should fail: {r}"
print("unlock with wrong password correctly refused:", r["error"][:60])

# 9. Read-model commands (locked vault fails closed, same as desktop).
for cmd in ["get_notes", "get_ssh_keys", "get_commands", "get_folders", "get_credentials"]:
    r = call(cmd)
    assert not r["ok"] and "DATABASE_NOT_INITIALIZED" in r["error"], f"{cmd}: {r}"
print("read commands fail closed without open vault. OK")

# 10. Live driver — quick-connect to a refused port must emit progress +
# failure events through the real connection worker. camelCase on purpose:
# this is the exact SessionView shape; a snake_case-only test hid a bug.
sid2 = "serve-smoke-live"
for ev in [f"session-log-{sid2}", f"connection-failed-{sid2}"]:
    ws.send(json.dumps({"type": "sub", "events": [ev]}))
    ack = json.loads(ws.recv())
    assert ack.get("type") == "sub-ack", f"no ack for {ev}: {ack}"
r = call("initiate_connection", {
    "sessionId": sid2, "serverId": 0, "customPassword": None,
    "quickAuth": {"host": "127.0.0.1", "port": 1, "username": "smoke"},
    "sessionRole": "primary", "separateSessions": False,
})
assert r["ok"], f"initiate should accept (failure comes as events): {r}"
logs, failed = [], None
deadline = time.time() + 30
while time.time() < deadline:
    evt = json.loads(ws.recv())
    if evt.get("event") == f"session-log-{sid2}":
        logs.append(evt["payload"])
    elif evt.get("event") == f"connection-failed-{sid2}":
        failed = evt["payload"]
        break
assert logs, "no session-log events from live driver"
assert failed, "no connection-failed event from live driver"
print(f"live driver emitted {len(logs)} session-log events, then failed closed:")
print("  last log:", str(logs[-1].get("msg", logs[-1]))[:100])
print("  failure:", str(failed.get("reason", failed))[:100])

# 11. Terminal commands fail closed without a live session.
r = call("open_terminal", {"sessionId": "nope", "terminalId": "nope-term-1", "cols": 80, "rows": 24})
assert not r["ok"], f"open_terminal should fail: {r}"
print("open_terminal (no session) correctly refused:", r["error"][:80])
r = call("write_terminal_data", {"terminalId": "nope-term-1", "data": [104, 105]})
assert r["ok"], f"write should be fire-and-forget ok (desktop parity): {r}"
print("write_terminal_data to missing terminal is a silent no-op (desktop parity)")
r = call("resize_terminal", {"terminalId": "nope-term-1", "cols": 80, "rows": 24})
assert r["ok"], f"resize should be silent no-op (desktop parity): {r}"
r = call("close_terminal", {"terminalId": "nope-term-1"})
assert r["ok"], f"close_terminal should succeed (idempotent remove): {r}"
print("close_terminal idempotent OK")

# 12. SFTP commands fail closed without a live session (camelCase shapes).
for cmd, args in [
    ("sftp_list_dir", {"sessionId": "nope", "path": "/"}),
    ("sftp_create_dir", {"sessionId": "nope", "path": "/x"}),
    ("sftp_rename", {"sessionId": "nope", "oldpath": "/a", "newpath": "/b"}),
    ("sftp_set_permissions", {"sessionId": "nope", "path": "/a", "permissions": 644}),
]:
    r = call(cmd, args)
    assert not r["ok"], f"{cmd} should fail: {r}"
print("sftp commands fail closed without session. OK (camelCase shapes)")

# 13. HTTP surface: static public, API gated, origin enforced.
base = url.split("/api/ws")[0].replace("ws://", "http://").replace("wss://", "https://")
token = url.split("token=")[1]
r = urllib.request.urlopen(base + "/", timeout=10)
assert r.status == 200, r.status
print("HTTP static bundle public without token OK")
r = urllib.request.urlopen(base + "/index.html", timeout=10)
assert r.status == 200, r.status
print("HTTP index.html public OK")
req = urllib.request.Request(base + "/api/ping?token=" + token)
assert json.loads(urllib.request.urlopen(req, timeout=10).read())["ok"] is True
print("HTTP ping with token OK")
try:
    urllib.request.urlopen(base + "/api/ping", timeout=10)
    raise SystemExit("expected 401 on ping without token")
except urllib.error.HTTPError as e:
    assert e.code == 401, e.code
print("HTTP ping 401 without token OK")
req = urllib.request.Request(base + "/api/ping?token=dead-token")
try:
    urllib.request.urlopen(req, timeout=10)
    raise SystemExit("expected 401 on ping with bad token")
except urllib.error.HTTPError as e:
    assert e.code == 401, e.code
print("HTTP ping 401 with stale token OK (drives the UI's re-connect hint)")
# Origin gate: evil Origin + VALID token must 403; absent/same-origin passes.
req = urllib.request.Request(
    base + "/api/ping?token=" + token, headers={"Origin": "http://evil.example"},
)
try:
    urllib.request.urlopen(req, timeout=10)
    raise SystemExit("expected 403 on cross-origin ping")
except urllib.error.HTTPError as e:
    assert e.code == 403, e.code
print("HTTP cross-origin ping correctly refused (403)")
req = urllib.request.Request(base + "/api/ping?token=" + token)
assert json.loads(urllib.request.urlopen(req, timeout=10).read())["ok"] is True
print("HTTP no-Origin ping OK (curl/scripts unaffected)")
req = urllib.request.Request(
    base + "/api/files/download?token=" + token,
)
try:
    urllib.request.urlopen(req, timeout=10)
    raise SystemExit("expected 400 without params")
except urllib.error.HTTPError as e:
    assert e.code == 400, e.code
print("HTTP download 400 without params OK")

# 14. HTTP upload validation + temp hygiene (no session needed).
tmpdir = os.environ.get("TEMP", os.environ.get("TMP", "/tmp"))
before = set(glob.glob(os.path.join(tmpdir, "submarine-upload-*")))
boundary = "----smoke1234"
body = (
    "--" + boundary + "\r\n"
    'Content-Disposition: form-data; name="session_id"\r\n\r\nnope\r\n'
    "--" + boundary + "\r\n"
    'Content-Disposition: form-data; name="remote_path"\r\n\r\n/x\r\n'
    "--" + boundary + "--\r\n"
).encode()
req = urllib.request.Request(
    base + "/api/files/upload?token=" + token,
    data=body,
    headers={"Content-Type": "multipart/form-data; boundary=" + boundary},
    method="POST",
)
r = json.loads(urllib.request.urlopen(req, timeout=10).read())
assert not r["ok"] and "MISSING_ARG" in r["error"], f"upload validation: {r}"
print("upload without file correctly refused:", r["error"])
body2 = (
    "--" + boundary + "\r\n"
    'Content-Disposition: form-data; name="session_id"\r\n\r\nnope\r\n'
    "--" + boundary + "\r\n"
    'Content-Disposition: form-data; name="remote_path"\r\n\r\n/x/y.txt\r\n'
    "--" + boundary + "\r\n"
    'Content-Disposition: form-data; name="file"; filename="y.txt"\r\n'
    "Content-Type: application/octet-stream\r\n\r\n"
    "hello-gateway\r\n"
    "--" + boundary + "--\r\n"
).encode()
req2 = urllib.request.Request(
    base + "/api/files/upload?token=" + token,
    data=body2,
    headers={"Content-Type": "multipart/form-data; boundary=" + boundary},
    method="POST",
)
r = json.loads(urllib.request.urlopen(req2, timeout=15).read())
assert not r["ok"], f"upload to bogus session should fail: {r}"
print("upload to bogus session correctly refused:", r["error"][:70])
after = set(glob.glob(os.path.join(tmpdir, "submarine-upload-*")))
assert after == before, f"temp files leaked: {after - before}"
print("no temp files leaked. OK")

# 15. Auth continuations + persist + tunnels (no live session needed).
r = call("verify_fingerprint_response", {"nonce": "nope", "accepted": True})
assert r["ok"], f"verify_fingerprint should be no-op ok: {r}"
r = call("submit_kbi_response", {"nonce": "nope", "responses": ["x"]})
assert r["ok"], f"submit_kbi should be no-op ok: {r}"
r = call("submit_kbi_response", {"nonce": "nope", "responses": None})
assert r["ok"], f"submit_kbi cancel should be ok: {r}"
print("auth continuations no-op OK (stale nonce)")
r = call("persist_vault")
assert not r["ok"], f"persist locked should fail: {r}"
print("persist_vault locked correctly refused:", r["error"][:60])
r = call("list_tunnels", {})
assert r["ok"] and r["result"] == [], f"list_tunnels empty: {r}"
r = call("list_tunnels", {"sessionId": "nope"})
assert r["ok"] and r["result"] == [], f"list_tunnels sid: {r}"
print("list_tunnels empty OK")
r = call("stop_tunnel", {"tunnelId": "nope"})
assert not r["ok"], f"stop bogus should fail: {r}"
print("stop_tunnel bogus correctly refused:", r["error"][:60])
r = call("start_tunnel", {"sessionId": "nope", "spec": {"type": "L", "local": "18080", "remote": "127.0.0.1:80"}})
assert not r["ok"] and "Session not connected" in r["error"], f"start no-session should fail: {r}"
print("start_tunnel no-session correctly refused:", r["error"][:70])
r = call("persist_session_tunnels", {"sessionId": "nope", "serverId": 0})
assert r["ok"], f"persist quick-connect no-op: {r}"
r = call("restart_session_tunnels", {"sessionId": "nope", "serverId": 0})
assert r["ok"], f"restart no-specs no-op: {r}"
print("persist/restart tunnels no-op OK")

# 16. Vault CRUD fail closed (locked vault) + explicit error arms.
for cmd, args in [
    ("add_server", {"name": "x", "host": "h", "port": 22, "proxy_type": "none", "proxy_host": "", "proxy_port": 1080, "tunnels": [], "auth_type": "custom_pass"}),
    ("get_servers", {}),
    ("add_folder", {"name": "f"}),
    ("add_credential", {"name": "c", "auth_type": "password", "username": "u"}),
    ("add_ssh_key", {"name": "k", "public_key": "p", "private_key": "k"}),
    ("generate_ssh_key", {"name": "g"}),
    ("add_command", {"title": "t", "content": "c"}),
    ("add_note", {"title": "t", "body": "b"}),
    ("cmd_history_add", {"server_name": "s", "command": "ls"}),
    ("reveal_server_password", {"id": 1}),
    ("create_profile", {"name": "x", "password": "y"}),
    ("monitor_list", {}),
    ("ssh_info_probe_section", {"sessionId": "nope", "section": "overview"}),
    ("parse_ssh_config", {"path": "/x"}),
]:
    r = call(cmd, args)
    assert not r["ok"], f"{cmd} should fail closed or refuse: {r}"
print("vault CRUD fail closed OK")
r = call("delete_profile", {"name": "definitely-not-a-profile-xyz"})
assert r["ok"], f"delete missing profile should be idempotent ok: {r}"
print("delete missing profile idempotent OK")
r = call("sync_now", {})
assert not r["ok"] and "CLOUD_ONLY" in r["error"], f"cloud should be CLOUD_ONLY: {r}"
r = call("export_profile", {"name": "gtr"})
assert not r["ok"] and "DESKTOP_ONLY" in r["error"], f"export should be DESKTOP_ONLY: {r}"
r = call("sftp_upload_dir", {"sessionId": "s", "localPath": "/x", "remotePath": "/y"})
assert not r["ok"] and "MOVED" in r["error"], f"upload_dir should be MOVED: {r}"
print("CLOUD_ONLY / DESKTOP_ONLY / MOVED arms OK")

# 17. Vault gate: mirrors + ssh-config import require an UNLOCKED vault.
# (Smoke never unlocks — active_profile may be set, but master_key is not.)
for cmd, args in [
    ("mirror_dry_run", {"sessionId": "nope", "spec": {"local": "/tmp/x", "remote": "/r", "direction": "up"}}),
    ("start_mirror", {"sessionId": "nope", "spec": {"local": "/tmp/x", "remote": "/r", "direction": "up"}}),
    ("stop_mirror", {"mirrorId": "nope"}),
    ("list_mirrors", {}),
]:
    r = call(cmd, args)
    assert not r["ok"] and "VAULT_LOCKED" in r["error"], f"{cmd} should demand unlock: {r}"
print("mirror commands correctly gated on unlocked vault")

# 18. Bearer auth — same rights as ?token= (regression: handler re-check).
token = url.split("token=")[1]
ws2 = websocket.create_connection(
    url.split("?")[0], timeout=15, header={"Authorization": f"Bearer {token}"}
)
ws2.send(json.dumps({"id": 99, "cmd": "app_info", "args": {}}))
r = json.loads(ws2.recv())
assert r["ok"] and r["id"] == 99, f"bearer auth failed: {r}"
print("bearer auth works. ALL SMOKE TESTS PASSED")
ws.close()
ws2.close()
