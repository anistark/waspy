---
title: Not supported
eyebrow: Language
description: "Syntax the compiler rejects up front, with a located error and a hint."
---

The compiler validates syntax up front and rejects these with a located error and a hint, rather than miscompiling them:

- Asynchronous generators (an `async def` that yields, and so an `async for` over one), asynchronous comprehensions, and an `async def` inside a function (`async def`, `await`, `async with`, and `async for` over a class with `__aiter__` and `__anext__` are supported: see [JavaScript and async](/docs/javascript/))
- `match` statements, `type` aliases, `except*`, and `del` of an attribute or a stepped slice
- `from module import *`
- A generator expression kept in a variable, returned, stored, or passed to the program's own functions, and `print()`, `len()`, or `bool()` of one: it goes where a builtin, a standard-library call, a `for` statement, or a comprehension walks it once (see [Limitations](/docs/limitations/#generators))
- A class whose base is a builtin value type (`class Stack(list)`), an undefined name, or an expression computed when the class statement runs
- `*args`, `**kwargs`, and keyword-only parameters on methods other than `__init__`, `**` unpacking in a call, and positional-only parameters (`/`)
- Class keywords other than `metaclass=` (`__init_subclass__` arguments)
- Decorators CPython does not define (`@memoize`, `@timer`, `@debug`, `@pure`, `@type_check`, `@default_value`, `@wasm_export`), which CPython rejects with `NameError`
- Third-party packages (`import numpy as np`, `import pandas`) and the standard-library modules the compiler does not support: an import of a module that is neither one of your own nor listed on [the Python subset](/docs/python-subset/#modules-files-and-the-standard-library) is refused at the import, with its line. `typing`, `__future__`, and `collections.abc` are imported for annotations, and an import guarded by `try`/`except ImportError` is left to its fallback

If you find a program that compiles and answers differently from CPython, other than as listed under [Where the answer differs](/docs/limitations/#where-the-answer-differs), that is a bug: please [report it](https://github.com/anistark/waspy/issues).
