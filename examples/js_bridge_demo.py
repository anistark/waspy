"""JavaScript values from Python, through Pyodide's `js` and `pyodide.ffi`:
parse JSON with JavaScript's parser, read and write a JS object's
properties, call JS functions, and convert between JS and Python values."""

import js
from js import JSON, Math, Object
from pyodide.ffi import JsException, JsProxy, to_js
from typing import Dict, List


def summarize(scores: JsProxy) -> str:
    best = ""
    top = -1
    for name in Object.keys(scores):
        key: str = name
        value: int = js.Reflect.get(scores, name)
        if value > top:
            best, top = key, value
    return f"{best} leads with {top}"


def main() -> None:
    data = JSON.parse('{"team": "otters", "scores": {"ada": 7, "bo": 12, "cy": 9}, "tags": ["a", "b"]}')
    team: str = data.team
    print("team", team.title(), "with", len(data.tags), "tags")
    print(summarize(data.scores))
    scores: Dict[str, int] = data.scores.to_py()
    print(sorted(scores.items()))
    data.updated = True
    data.ratio = 0.5
    print(JSON.stringify(data))
    biggest: int = Math.max(3, 41, 7)
    pi: float = Math.PI
    print("max", biggest, "pi", round(pi, 4))
    tags: List[str] = data.tags.to_py()
    print("tags", tags, JSON.stringify(to_js([t.upper() for t in tags])))
    try:
        JSON.parse("{not json")
    except JsException:
        print("JavaScript refused the JSON")
    try:
        print(data.missing)
    except AttributeError as e:
        print("no attribute", e)


if __name__ == "__main__":
    main()
