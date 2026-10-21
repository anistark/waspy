---
title: Limitations
eyebrow: Language
description: "What the compiler refuses, or traps on, rather than answering differently from CPython."
---

Each of these is refused at compile time, or traps, rather than answering differently from CPython.

## Types

- A place with no type holds a single word read back as an int: an unannotated parameter or return value, and the elements of a bare `list`, `set`, or `dict` parameter or return. Only ints, bools, None, and functions pass through one; a string, float, collection, or instance is refused with a hint to annotate, since it would lose its length, width, or type on the way. A bare annotation on a variable or field is fine: its elements take the type of the value assigned. `len()` of an untyped value is refused for the same reason
- None is the word 0, which an untyped value can also be, so `x is None` on an untyped value is refused. An `Optional[int]`, `Optional[bool]`, or `Optional[float]` is boxed, so None is told apart from 0: `is None`, `==`, truthiness, `str()`, and `dict.get(key)` on number values give CPython's answer, and arithmetic or an ordering on None raises its `TypeError`. An optional number passed or returned where a plain one is declared (past an `is None` test, typically) is its number, and None there traps, since CPython would pass the None on and the place cannot hold it. Arithmetic between two optional numbers is refused
- `dict.get(key)` without a default is refused where the values are ints, floats, or strings, since a missing key answers None, which those values have no distinct word for; pass a default
- Reading an attribute through a value whose type is not known, most often an unannotated parameter, is a compile error with a hint to annotate it. The rich comparison methods are the exception: an unannotated `other` in `__eq__`/`__lt__`/... is typed as the class. A field the class does not have is refused too
- A variable, field, or collection that holds instances of more than one class is typed as their nearest common base for the whole function, so a method call on it dispatches on each instance's own class. Types are not tracked per program point, so reading a field that only one of the subclasses has through it is refused, and so is a method call on instances with no common base. Storing an instance where an annotation names an unrelated class (a `Rect` passed to a `Circle` parameter, or returned from `-> Circle`) is refused rather than dispatched as the annotated class
- A lambda's parameters, and a nested function's unannotated ones, are typed from the calls in their scope when those agree, and a `sorted()` key's from the elements it is given. A parameter nothing types (a lambda only passed on to an unannotated parameter, or called with an int in one place and a str in another) holds a word like any untyped place, so indexing it, measuring it, or calling a method on it is refused; annotate a named `def` instead
- A `@singledispatch` call needs a first argument whose type the compiler knows (a literal, an annotated parameter, an instance); dispatching on an untyped value is refused rather than sent to the base implementation

## Collections

- A collection holds one element type, so a literal mixing types no one type holds (`[1, "a"]`, `{"n": "bob", "age": 30}`, `[1, True]`) is refused; use a class or a tuple for a record. A tuple is typed per position, so `(1, 2.5)` and `(1, "a")` are fine, but walking one whose members differ (a loop, `sorted`, `sum`) is refused
- A collection reads every slot at one width, so a value written into one is converted to its element type: an int into a `List[float]` widens. Where it cannot be converted the write is refused rather than stored wrong: a float into a collection of ints (it would truncate), a float into a collection with no element type, a value whose type is unknown into a collection of floats, and `list.extend()` between lists of different widths. `list.extend()` takes a list, or a tuple whose members share a type; any other iterable (a range, a set, a string) is refused
- Comparing two collections needs both to have the same, known element types: `(1, 2) == (1.0, 2)` (True in CPython) and a comparison with an untyped list are refused rather than read at the wrong width. Dict equality, comparing collections whose elements are dicts or sets, and a list, dict, or set as a set member or dict key (unhashable in CPython) are refused; two sets compare directly (`==`, and `<=`/`<` as subset tests). A class that defines `__eq__` cannot be a set member or dict key, since calling a user `__hash__` is not supported
- A method called with the wrong number of arguments is a compile error: `xs.append(3, 4)` and `xs.clear(9)` are refused. The optional forms Python accepts compile, so `pop()`, `pop(i)`, `sort()`, and `sort(reverse=True)` all work
- `for k, v in obj.items()` (and `.keys()` / `.values()`) over a user class defining that method is refused, since it is read as the dict form

## Functions and classes

- Closures capture the variable, not a snapshot of it: a captured variable reassigned after the closure is made changes what the closure sees, and closures created in a loop share the loop variable. A captured variable keeps one type (a float or a string included), so rebinding it to a value of another type is refused
- A nested `def` has to sit at the top level of the enclosing function's body (not inside an `if`, loop, `try`, or `with`), outside a generator, with constant defaults, and it cannot be called before its `def` runs. A function value that passes through an untyped place (an unannotated parameter) is called with int arguments for an int result, so one taking or returning a float, string, collection, or instance is refused there; annotate the parameter with `Callable[[...], R]`
- Every implementation of an overridden method is reached through one indirect call, which names a single WebAssembly signature, so an override that changes the parameter or return types is a compile error naming both methods
- Decorators: a wrapper that uses `*args` other than to pass it on, a user decorator on a method or a class, `@cached_property`, `@singledispatchmethod`, and any decorator name the program does not define and the compiler does not implement are refused. So are calls to `functools.partial`, `reduce`, and `cmp_to_key`, and to any other name the program does not define and the compiler does not implement (`divmod()` and `hash()`, for example)
- A `with` statement needs its context manager's class to be known at compile time (an instantiation, a call with an annotated class return type, or a variable of known class type). `__exit__`'s `exc_type` receives the exception object as well as `exc`, and `tb` None; either may only be compared with None or tested for truth, and any other use is refused
- The default `repr` of an instance whose class defines neither `__repr__` nor `__str__` is refused, since CPython's includes an address

## Exceptions and formatting

- Types are static, so the name a handler binds has the class the handler names (the common base of several): a field only a subclass has is refused through it. `e.args` reads back when every argument is a str, and traps otherwise
- A traceback is not kept: a `__main__` block's uncaught exception writes only the last line of CPython's report (`ValueError: boom`), and `UnicodeDecodeError`'s position counts from the start of the line where CPython counts from its read buffer
- A format spec CPython rejects (`format(1.5, "d")`, `"{:,s}".format("a")`) is a compile error carrying CPython's `ValueError` message when the spec is a literal, and traps when it is built at run time. `str.format` and `%` need a literal format string; a `*` width or precision in `%`, and a precision on `%d`/`%x`/`%o`, are refused

## Generators

- A generator is not finalized when it is no longer reachable (there is no collector), so a generator abandoned mid-iteration does not run its `finally` blocks until `close()` is called
- A generator method that another class of its hierarchy also defines is refused, since which generator a call makes would depend on the instance, and so is iterating a generator whose class is not known where it is iterated (one returned through an unannotated function, say)
- A comprehension can iterate a generator or iterator only in its first `for` clause: an inner clause is evaluated twice (once to size the result, once to fill it), so an iterator there is refused rather than drained twice. `len()` of a generator is refused, as CPython raises `TypeError`

## Memory, modules, and files

- Memory is never reclaimed: the bump allocator has no `free`, there is no garbage collection or reference counting, every object lives until the module is torn down, and `__del__` is not invoked
- Imported user modules share one flat namespace in the output module, so two modules defining the same function name is a compile error naming both files
- `f.read()` without a size reads up to 64 KiB per call; `open()` modes must be string literals
