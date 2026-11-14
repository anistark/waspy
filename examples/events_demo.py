"""Events from Python, through Pyodide's `create_proxy`: Python functions as
listeners on page elements, the event objects they are handed, bubbling,
stopping it, a listener called once, preventing a default, and removing a
listener and destroying its proxy."""

from js import Event, document
from pyodide.ffi import JsProxy, create_once_callable, create_proxy

log: list[str] = []


def on_click(event):
    kind: str = event.type
    target: str = event.target.id
    log.append(kind + " on " + target)


def on_bubble(event: JsProxy) -> None:
    here: str = event.currentTarget.id
    log.append("bubbled to " + here)


def stop_here(event):
    event.stopPropagation()
    log.append("stopped")


def main() -> None:
    app = document.createElement("div")
    app.id = "events-demo"
    document.body.appendChild(app)
    button = document.createElement("button")
    button.id = "go"
    app.appendChild(button)

    clicked = create_proxy(on_click)
    bubbled = create_proxy(on_bubble)
    button.addEventListener("click", clicked)
    app.addEventListener("click", bubbled)
    button.click()
    button.click()
    print(log)
    log.clear()

    button.removeEventListener("click", clicked)
    clicked.destroy()
    stopper = create_proxy(stop_here)
    button.addEventListener("click", stopper)
    button.click()
    print(log)
    log.clear()

    once = create_once_callable(lambda e: log.append("once " + str(e.type)))
    button.addEventListener("ping", once, once=True)
    button.dispatchEvent(Event.new("ping"))
    button.dispatchEvent(Event.new("ping"))
    print(log)

    preventer = create_proxy(lambda e: e.preventDefault())
    button.addEventListener("check", preventer)
    print(button.dispatchEvent(Event.new("check", cancelable=True)))
    print(button.dispatchEvent(Event.new("other", cancelable=True)))

    app.remove()
    for proxy in [bubbled, stopper, preventer]:
        proxy.destroy()
    print("done", len(log))


if __name__ == "__main__":
    main()
