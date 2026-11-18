"""Small stdio server that deliberately completes requests out of order."""
import json
import os
import sys
import threading
import time

if len(sys.argv) > 2:
    with open(sys.argv[2], "w") as pid_file:
        pid_file.write(str(os.getpid()))

lock = threading.Lock()
notifications = []


def respond(message):
    method = message.get("method")
    if method == "initialize":
        time.sleep(float(sys.argv[1]))
        result = {"capabilities": {"definitionProvider": True, "hoverProvider": True}}
    elif method == "textDocument/hover":
        character = message["params"]["position"]["character"]
        cases = [
            {"contents": "valid plaintext hover"},
            {"contents": {"language": "rust", "value": "fn helper() {}"}},
            {"contents": {"kind": "plaintext", "value": "**literal** <b> &"}},
            {"contents": {"kind": "markdown", "value": "**bold**"}, "range": {
                "start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 4}}},
            {"contents": ["docs", {"language": "rust", "value": "fn f() {}"}]},
            None,
        ]
        result = cases[character]
    elif method == "test/notifications":
        result = list(notifications)
    else:
        character = message.get("params", {}).get("position", {}).get("character", 1)
        if character == 0:
            time.sleep(0.2)
        result = {"uri": "file:///tmp/result.rs", "range": {
            "start": {"line": character, "character": 0}}}
    response = {"jsonrpc": "2.0", "id": message["id"], "result": result}
    if method == "initialize" and len(sys.argv) > 3 and sys.argv[3] == "initialize-error":
        response = {"jsonrpc": "2.0", "id": message["id"], "error": {"code": -32002, "message": "init failed"}}
    body = json.dumps(response)
    with lock:
        sys.stdout.write(f"Content-Length: {len(body)}\r\n\r\n{body}")
        sys.stdout.flush()


while True:
    length = 0
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            sys.exit(0)
        if line == b"\r\n":
            break
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":", 1)[1])
    message = json.loads(sys.stdin.buffer.read(length))
    if message.get("method") == "test/exit":
        os._exit(0)
    if message.get("method") == "test/close-input":
        os.close(0)
        respond(message)
        time.sleep(60)
        sys.exit(0)
    if message.get("method") == "test/no-response":
        continue
    if "id" in message:
        threading.Thread(target=respond, args=(message,)).start()
    else:
        notifications.append(message)
