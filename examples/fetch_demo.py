"""HTTP requests from Python, through Pyodide's `pyfetch`: a GET whose JSON
is read into a typed dict, a POST with headers and a body, a status error
raised by `raise_for_status()`, and a body read twice. The browser gate
serves the files and echoes the POST on loopback."""

import asyncio
import json

from pyodide.http import HttpStatusError, pyfetch


async def main() -> None:
    resp = await pyfetch("/tests/browser/fixtures/scores.json")
    print(resp.status, resp.ok, resp.status_text, resp.headers["content-type"])
    scores: dict[str, int] = await resp.json()
    best = ""
    for name, score in scores.items():
        if best == "" or score > scores[best]:
            best = name
    print(sorted(scores.items()), "best:", best)

    sent = await pyfetch(
        "/__echo",
        method="POST",
        headers={"Content-Type": "application/json"},
        body=json.dumps({"name": "ada", "team": "otters"}),
    )
    echoed: dict[str, str] = await sent.json()
    print(echoed["method"], echoed["type"], echoed["body"])

    for method in ["PUT", "DELETE"]:
        r = await pyfetch("/__echo", method=method, body="x")
        seen: dict[str, str] = await r.json()
        print(method, "->", seen["method"])

    missing = await pyfetch("/tests/browser/fixtures/missing.json")
    print(missing.status, missing.ok)
    try:
        missing.raise_for_status()
    except HttpStatusError as e:
        print("status error", e.status, str(e).split(" for url: ")[0])
    print(repr(await missing.text()), missing.body_used)
    try:
        await missing.text()
    except OSError as e:
        print(type(e).__name__, e)


if __name__ == "__main__":
    asyncio.run(main())
