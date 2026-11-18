"""Small stdio server that deliberately completes requests out of order."""
import json
import sys
import threading
import time

lock = threading.Lock()
notifications = []


def respond(message):
    method = message.get("method")
    if method == "initialize":
        time.sleep(float(sys.argv[1]))
        result = {"capabilities": {"definitionProvider": True}}
    elif method == "test/notifications":
        result = list(notifications)
    else:
        character = message.get("params", {}).get("position", {}).get("character", 1)
        if character == 0:
            time.sleep(0.2)
        result = {"uri": "file:///tmp/result.rs", "range": {
            "start": {"line": character, "character": 0}}}
    body = json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": result})
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
    if "id" in message:
        threading.Thread(target=respond, args=(message,)).start()
    else:
        notifications.append(message)
