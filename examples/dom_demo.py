"""The page's DOM from Python, through Pyodide's `js`: make elements, set
their text, attributes, classes, and styles, query them, move them around
the tree, and take them out again."""

from js import document
from pyodide.ffi import JsProxy


def texts(parent: JsProxy) -> list[str]:
    out: list[str] = []
    for child in parent.children:
        text: str = child.textContent
        out.append(text)
    return out


def main() -> None:
    app = document.createElement("div")
    app.id = "dom-demo"
    document.body.appendChild(app)

    title = document.createElement("h2")
    title.textContent = "Groceries"
    app.appendChild(title)
    items = document.createElement("ul")
    items.className = "list"
    app.appendChild(items)
    for name in ["apples", "bread", "milk"]:
        li = document.createElement("li")
        li.textContent = name
        li.classList.add("item")
        li.setAttribute("data-name", name)
        items.appendChild(li)

    print(len(items.children), items.children[1].textContent, texts(items))
    print(document.getElementById("dom-demo").tagName, items.innerHTML)

    first = items.firstElementChild
    first.classList.add("done", "fresh")
    done = document.querySelector("#dom-demo li.done")
    print(done.textContent, done.classList.contains("done"), done.getAttribute("data-name"))
    print(done.getAttribute("data-missing"), len(document.querySelectorAll("#dom-demo .item")))
    print(first.classList.toggle("fresh"), first.className, len(first.classList))

    items.appendChild(first)
    print("moved", texts(items), first.previousElementSibling.textContent)
    extra = document.createElement("li")
    extra.textContent = "eggs"
    items.insertBefore(extra, items.firstElementChild)
    print("inserted", texts(items))
    gone = items.removeChild(items.lastElementChild)
    print("removed", gone.textContent, items.childElementCount, items.contains(gone))

    title.textContent += "!"
    title.style.color = "green"
    title.setAttribute("title", "list")
    print(title.textContent, title.style.color, title.hasAttribute("title"))
    title.removeAttribute("title")
    print(title.hasAttribute("title"), app.firstElementChild.tagName)

    app.remove()
    print(document.getElementById("dom-demo"), document.body.contains(app))


if __name__ == "__main__":
    main()
