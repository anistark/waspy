---
title: Not supported
eyebrow: Language
description: "Syntax the compiler rejects up front, with a located error and a hint."
---

The compiler validates syntax up front and rejects these with a located error and a hint, rather than miscompiling them:

- `async def`, `await`, `async for`, and `async with`
- `match` statements, `type` aliases, `except*`, and `del` of a name, an attribute, or a stepped slice
- `from module import *`
- `*args`, `**kwargs`, and keyword-only parameters on methods other than `__init__`, `**` unpacking in a call, and positional-only parameters (`/`)
- Class keywords other than `metaclass=` (`__init_subclass__` arguments)
- Decorators CPython does not define (`@memoize`, `@timer`, `@debug`, `@pure`, `@type_check`, `@default_value`, `@wasm_export`), which CPython rejects with `NameError`
- Module-level statements other than definitions. A WebAssembly module has no top-level run step, so only definitions run: `X = 1`, `X: int = 1`, `X = helper()`, `X = ClassName()`, `def`, `class`, and imports (a `try`/`except ImportError` guarded import included). Definitions that are not plain constants are evaluated once, in source order, when the module is instantiated; one that reads a definition appearing after it is refused, as it is a `NameError` in CPython. A module-level loop, `if`, `try`, bare call statement, augmented assignment, tuple unpacking, or write through a subscript or attribute is rejected, because it would otherwise be compiled away and later reads would see the value from before it. Put the code in a function and call it, or drive it from `if __name__ == "__main__":`, which is recognized as the entry point
- Third-party packages: the supported subset is your own modules plus the standard library above

If you find a program that compiles and answers differently from CPython, other than through integer width or a NaN in a collection, that is a bug: please [report it](https://github.com/anistark/waspy/issues).
