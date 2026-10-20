# Waspy

A Python to WebAssembly compiler written in Rust.

[![Crates.io Version](https://img.shields.io/crates/v/waspy)
](https://crates.io/crates/waspy) [![Crates.io Downloads](https://img.shields.io/crates/d/waspy)](https://crates.io/crates/waspy) [![Crates.io Downloads (latest version)](https://img.shields.io/crates/dv/waspy)](https://crates.io/crates/waspy) [![Open Source](https://img.shields.io/badge/open-source-brightgreen)](https://github.com/anistark/waspy) [![Contributors](https://img.shields.io/github/contributors/anistark/waspy)](https://github.com/anistark/waspy/graphs/contributors) ![maintenance-status](https://img.shields.io/badge/maintenance-actively--developed-brightgreen.svg)

![waspy](./assets/logo.png)

## Overview

Waspy compiles a typed subset of Python ahead of time into a standalone WebAssembly module, with no interpreter or VM in the output. This README's [Supported Python subset](#supported-python-subset) and [Limitations](#limitations) sections are the authoritative statement of what compiles and runs today.

### Compilation Pipeline

```sh
[Python Source Code]
       ↓ 
Parse & Analyze
       ↓
[Intermediate Representation]
       ↓
Generate & Optimize
       ↓
[WebAssembly Binary]
```

## Supported Python subset

- Compiles Python functions to WebAssembly
- Supports multiple functions in a single WebAssembly module
- Compiles multiple files into a single module
- Handles control flow with if/else, while and for loops, including `break` and `continue`
- Conditional expressions (`a if cond else b`), evaluating only the arm the condition picks. Both arms must read as one type: two classes share their nearest common base, a class and `None` make an `Optional`, an empty collection takes the other arm's element type, and an int widens only where a float is expected; arms that would read differently (`1 if c else 2.5`, `True if c else 0`, `"a" if c else None`) are refused
- Processes variable declarations and assignments
- Supports type annotations for function parameters and return values. For a collection passed into or out of a function, the parameterised form (`List[str]`, `Dict[str, int]`, `Tuple[int, str]`) is what gives the compiler its element type; a bare `list` or `dict` there carries only ints, bools, and None (see [Limitations](#limitations))
- Calls pass arguments by position or by keyword (`f(1, c=7)`, `Point(x=1)`), with defaults filled in for the parameters skipped. Functions and `__init__` take keyword-only parameters (`def f(x, *, factor=2)`), `*args: T` (a tuple of `T`), and `**kwargs: T` (a `dict[str, T]`), and `f(*xs)` passes a list or tuple on as `*args`. A call to one of the module's own functions is checked against its signature, so a surplus or missing argument is a compile-time `TypeError`
- `assert test, msg` raises `AssertionError` when the test is false, evaluating the message only then
- `global name` in a function or method rebinds the module definition for every reader; a rebinding keeps the definition's type
- `del xs[i]`, `del xs[a:b]`, and `del d[k]`, raising `IndexError` / `KeyError` for what is not there
- Enables function calls between compiled functions
- Includes an expanded type system: integers (32-bit, see [Numbers](#numbers)), floats, booleans, strings
- String operations: slicing (with any step, `s[::-1]` included), concatenation, repetition (`s * n`), 20+ methods, and formatting: f-strings, `.format()`, `format()`, and `%`, all through CPython's format specification mini-language (fill, alignment, sign, `z`, `#`, `0`, width, `,`/`_` grouping, precision, and every type letter), digit for digit with CPython. `.format()` takes automatic, positional, and named fields, attribute and index lookups (`{0.x}`, `{1[2]}`, `{d[key]}`), conversions, and specs with nested fields; `%` takes every conversion with its flags, a tuple operand, and `%(key)s` over a dict
- A `str` is a sequence of code points, as in CPython: `len`, indexing, slicing, `find`/`index`, padding, iteration, and comprehensions count characters, not UTF-8 bytes. Case conversion (`upper`, `lower`, `capitalize`, `title`, with CPython's final-sigma and titlecase rules and `ß` to `SS`) and the `is*` predicates (`isalpha`, `isalnum`, `isdigit`, `isdecimal`, `isnumeric`, `isspace`, `isupper`, `islower`) cover the whole of Unicode, from tables generated from CPython 3.13 (Unicode 15.1), and so do `strip()` and `split()`'s idea of whitespace
- F-strings interpolate every placeholder: `f"{count} items"` renders each value with `str()` and concatenates the pieces, so an f-string is the `+` chain you would write by hand. `{x!r}`, `{x!a}`, and `{x:spec}` (with nested fields, `{x:{w}.{p}f}`) format as CPython does. Constant placeholders fold into the literal at compile time
- `str()` and `repr()` of every value: a float by its shortest round-trip digits (CPython's repr, exponent notation from 1e16), bools, None, quoted and escaped strings and bytes, ranges, nested lists, tuples, dicts, and sets, and instances through their `__str__` and `__repr__`. `float()` parses a string correctly rounded, as CPython does, and raises a catchable `ValueError` on bad input
- Supports arithmetic operations (`+`, `-`, `*`, `/`, `%`, `//`, `**`)
- Processes comparison operators (`==`, `!=`, `<`, `<=`, `>`, `>=`)
- Handles boolean operators (`and`, `or`) and bitwise operators (`&`, `|`, `^`, `<<`, `>>`)
- Built-in functions: `int()` (including parsing a string, with a catchable `ValueError` on bad input), `float()` (of a number, or a string, correctly rounded), `str()`, `repr()`, `ascii()`, `format()`, `bool()`, `len()` (of strings, collections, and ranges), `abs()`, `round(x[, ndigits])` (rounding the exact binary value half-to-even, as CPython does), `min()`/`max()` (several arguments, all ints, all floats, or all strings, or one list, tuple, or range, with `key=` and `default=`), `sum()` over lists/tuples (with optional start value), and `isinstance()` against a class, a builtin type, or a tuple of them. `print()` writes to standard output (with `sep`, `end`, `file=sys.stderr`, and `flush`), and `input()` reads a line of standard input, both through WASI (see [The host interface](#the-host-interface))
- A walk over a collection visits what CPython's does: a str its characters, a dict its keys, a set its members in CPython's order, in a comprehension, `sum`, `sorted`, `min`, and `max` alike
- Rejects unsupported Python syntax up front with located errors and hints, instead of failing deep in code generation
- Performs automatic WebAssembly optimization using Binaryen
- Detects and handles project structure and dependencies
- Supports module-level variables and class definitions with heap-allocated instances, multiple live instances per class, usable as function arguments and return values. A module-level definition that is not a plain constant (a collection, an instance, a call) is evaluated once at instantiation, in source order, and shared by every function, so mutating it is seen everywhere; class-level variables work the same way, are read through an instance or the class (a subclass's own when it defines one), and are rebound through the class that defines them (`Counter.count += 1`)
- Classes are values: `type(x)` and `x.__class__` give an instance's class, a class name is its class object, compared by identity (`type(s) is Square`), named by `__name__`, shown as `<class '__main__.Square'>`, read for its class variables, and annotated `type[A]`. Calling one held in a variable, and `type()` of a builtin value, are refused
- Metaclasses: `class M(type)` with `metaclass=M`, inherited by subclasses. M's `__new__` and `__init__` run once as each class is created, specialized for it at compile time, so a registry or a counter works and the attributes they give the class (`cls.plugin_id = ...`) are its class variables; `M.__call__` replaces instantiation (a singleton works); M's other methods and class variables are reached through the class (`Plugin.describe()`). The class namespace argument, `__prepare__`, and creating a class by calling the metaclass are refused
- Values compare by what they hold: `==`, `!=`, and the orderings on tuples, lists, and strings follow CPython, and so do `in`, `list.index`/`count`, set membership, and dict keys, so a tuple can be a set member or a dict key. Sets compare as sets (`==`, and `<=`/`<` as subset tests). `is` compares identity: two instances, an instance and `None`, or a `str` against `None`
- Tuple unpacking types each target from the value, and a value of the wrong length raises `ValueError`
- Augmented assignment through a name, an attribute, or a subscript (`counts[w] += 1`, `self.items[k].qty += n`), each target evaluated once, and annotated fields (`self.items: Dict[str, Item] = {}`)
- Virtual dispatch: a method call reaches the override the receiver's runtime class defines, so a base method calling `self.method()` gets the subclass's version (the template-method pattern), including through a variable, a parameter, or a list element declared as the base. `@property` getters and `__eq__` dispatch the same way, and `super().method()` stays non-virtual as in Python
- Object-oriented Python: inheritance from one base or several, with CPython's C3 method resolution order and cooperative `super()`, `isinstance`/`issubclass` over the class hierarchy, `@staticmethod`/`@classmethod`/`@property` (with setters), `@dataclass` (generated `__init__`/`__eq__`/`__repr__`), abstract base classes via `abc.ABC`, and rich comparisons: `==` calls `__eq__`, and `<`/`<=`/`>`/`>=` between instances call `__lt__`/`__le__`/`__gt__`/`__ge__` (the left operand's own, then the right operand's reflection, as CPython does)
- `functools` decorators that change what a program computes are implemented: `@singledispatch` with `@f.register` (dispatch is static, on the first argument's type) and `@total_ordering` (the missing ordering methods are derived from the one defined). `@lru_cache`/`@cache` are accepted and change nothing observable. A decorator the compiler does not implement, on a function, method, or class, is a compile error rather than a name dropped on the floor
- Collections: lists, dicts, sets, tuples, and ranges: literals, indexing, slicing, methods, and membership (`in`/`not in`), with full-precision f64 elements and hash-table sets; lists and dicts reallocate as they grow past their literal's size. `list.sort`/`list.reverse`/`list.pop(i)`/`list.index`, `tuple.index`/`count`, `dict.get(key, default)` (and `dict.get(key)` where the values are instances, since a missing key answers None, which an int, float, or str value has no distinct word for)/`.keys`/`.values`/`.items`, and `sorted(iterable[, key][, reverse])` are all real operations, and an empty collection is falsy as in Python. Lists and tuples concatenate (`xs + ys`) and repeat (`[0] * n`, `(a, b) * 3`); sets take `|`, `&`, `^`, and `-` (and their in-place forms) and the full method set (`union`, `intersection`, `difference`, `symmetric_difference`, the `_update` forms, `issubset`, `issuperset`, `isdisjoint`, taking a set, list, or tuple; `copy`, `pop`, `clear`; `set(iterable)`); and dicts merge with `d1 | d2` and `|=` in CPython's order and have `update`, `pop`, `popitem`, `setdefault`, `copy`, `clear`, and `dict.fromkeys`
- String methods run on a string built at runtime, not only on a literal: case and trim transforms, `find`/`count`/`startswith`/`endswith`, `split`/`join`/`replace`, the layout and predicate methods. A method folded on a literal answers what it answers at run time. Strings compare by content everywhere, so a key built at runtime matches an equal one already in a dict
- Exceptions: `raise` transfers control, `try`/`except`/`finally` catch and clean up, and an exception propagates out of a call into the caller's handler. `except (A, B):` catches either type, `except Exception:` catches any, a bare `raise` (or `raise e`) in a handler re-raises the exception it caught, and an exception nothing catches unwinds out of the program and traps
- Comprehensions: list, set, and dict comprehensions with filters, multiple generators, nesting, and `{k: v for k, v in pairs}` unpacking
- Generators with real state preservation: `yield` suspends and resumes (inside `try`/`except`/`finally` and `with` too), `yield from` delegates, and `next()`/`send()`/`close()` work, `close()` raising `GeneratorExit` at the suspended `yield` so `finally` blocks and handlers run; generator methods; user classes implementing `__iter__`/`__next__` iterate in `for` loops with `StopIteration` ending the loop, and `zip()` pulls a generator one value per round in any argument position
- Tuple targets in `for` loops (`for a, b in pairs`, star targets included) and the iterator-shaped builtins: `enumerate(xs[, start])` (`start=` too), `zip(...)`, and `dict.items()`/`.keys()`/`.values()`
- `else:` clauses on `for`, `while`, and `try`, run exactly when CPython runs them
- Closures with full variable capture: lambdas compile to real functions dispatched through a `call_indirect` table, capture enclosing variables by reference (see [Limitations](#limitations)), and work as first-class values: returned, passed as arguments, and stored in collections
- Nested functions: a `def` inside a function or method keeps its annotations, closes over the enclosing variables by reference (floats and strings included), recurses, calls its siblings, and takes keywords and defaults. Called by name it is a direct, fully typed call; returned or passed on it is a closure value called at its signature where that is known (`Callable[[float], float]` annotations are understood). `nonlocal name` rebinds the enclosing variable for every reader
- Extended unpacking: `a, *b, c = xs` binds the starred target to the middle slice as a real list
- User-written module imports: `import mod`, `import mod as m`, and `from mod import f [as g]` resolve sibling `.py` files (and `pkg/mod.py` packages) and statically link them into the single output WASM module, with each module compiled exactly once however many import paths reach it
- File I/O through WASI: `open()`, `read([n])`, `write(s)`, `close()`, and `with open(...) as f:` go through `path_open` on the host's preopened directories (`wasmtime run --dir .`), and a file that cannot be opened raises CPython's `FileNotFoundError` (or `PermissionError`, `IsADirectoryError`, ...); modules that never call `open()` import nothing. The older `waspy_host` interface is kept behind a deprecated option: see [File I/O](#file-io)
- Context managers: `with obj as name:` over a user class implementing `__enter__`/`__exit__`, including nested blocks, inherited protocols, and `__exit__` running before an early `return`
- Standard library, a small part so far: the module constants (`math.pi`/`e`/`tau`/`inf`/`nan`, `os.name` (`"posix"`, as CPython reports on WebAssembly hosts), `os.sep` and friends, the `re` flags, `sys.maxsize`, the `logging` levels, `datetime.MINYEAR`/`MAXYEAR`), read directly or through `from math import pi`; `re.sub` and `re.escape` over constant arguments, folded at compile time to the string CPython produces (a pattern the two regular-expression engines could read differently is refused); and the `functools` decorators above. Every other function in `sys`, `os`, `os.path`, `math`, `random`, `json`, `re`, `datetime`, `logging`, `collections`, `itertools`, and `functools`, and the host values (`sys.argv`, `os.environ`, ...), is refused at compile time, naming the release that implements it (0.20.0). The [development board](https://anistark.github.io/waspy/modules/) tracks each module

## Numbers

`int` is a 32-bit two's-complement integer, and `float` is an IEEE-754 double.

This is a deliberate narrowing of Python's semantics, and part of the definition
of the subset waspy accepts rather than an accident of the backend. CPython's
`int` is arbitrary precision: it grows to hold whatever it is given. Here a
value that leaves the range -2147483648 to 2147483647 wraps around, so

```python
1000000 * 1000000   # CPython: 1000000000000    waspy: -727379968
2 ** 40             # CPython: 1099511627776    waspy: 0
2147483647 + 1      # CPython: 2147483648       waspy: -2147483648
```

A program whose integer values stay inside that range gets Python's answers. One
that leaves it gets two's-complement wraparound, quietly, the way it would in C
or Rust's release profile.

The alternative was to check every add, subtract, multiply, and power for
overflow and trap. That makes the divergence loud, but it costs instructions on
the hottest path in any program and rejects code that deliberately relies on
wrapping (hashes, checksums, PRNGs). A 64-bit integer would only move the
boundary rather than remove it: arbitrary precision needs heap-allocated digits
and an allocator call on every operation, which is a different project.

Everywhere else, waspy holds itself to the opposite standard: a construct either
produces Python's answer or fails loudly, never a quietly different one. Integer
width is one of two documented exceptions, so if your program's arithmetic can
exceed 32 bits, it is not in the supported subset.

The other is a NaN inside a collection. Floats are unboxed values here, not
objects, so there is no identity for CPython's "same object, therefore equal"
shortcut to find: with `x = float("nan")`, `(x,) == (x,)` and `[x] == [x]` are
False, where CPython answers True. `x == x` is False in both.

Float division by zero and integer division or modulo by zero raise
`ZeroDivisionError` rather than producing `inf` or trapping.

## Limitations

- Object instances are never reclaimed: the bump allocator has no `free`, so every instance lives until the module is torn down and `__del__` is not invoked
- Lists, dicts, and sets grow at runtime (`append`/`extend`/`insert`, `dict[key] = value` for a new key, and `set.add` reallocate when full). A collection's elements live in a block its header points at, so growing one never moves the collection: a list grown inside a function it was passed to, or reached by indexing another collection, is grown for every other name for it too
- Generators cover the common shapes. A generator is not finalized when it is garbage-collected (there is no collector), so a generator abandoned mid-iteration does not run its `finally` blocks until `close()` is called. A generator method that another class of its hierarchy also defines is refused, since which generator a call makes would depend on the instance, and so is iterating a generator whose class is not known where it is iterated (one returned through an unannotated function, say). A comprehension can iterate a generator or iterator only in its first `for` clause: an inner clause is evaluated twice (once to size the result, once to fill it), so an iterator there is refused rather than drained twice
- Closures capture the variable, not a snapshot of it: a captured variable reassigned after the closure is made changes what the closure sees, and closures created in a loop share the loop variable. A captured variable keeps one type (a float or a string included), so rebinding it to a value of another type is refused
- A nested `def` has to sit at the top level of the enclosing function's body (not inside an `if`, loop, `try`, or `with`), outside a generator, with constant defaults, and it cannot be called before its `def` runs: each would need a binding that does not exist yet. A function value that passes through an untyped place (an unannotated parameter) is called with int arguments for an int result, so one taking or returning a float, string, collection, or instance is refused there; annotate the parameter with `Callable[[...], R]`
- Every implementation of an overridden method is reached through one indirect call, which names a single WebAssembly signature, so an override that changes the parameter or return types is a compile error naming both methods
- Imported user modules share one flat namespace in the output module, so two modules defining the same function name is a compile error naming both files. It used to keep the first definition and print a warning, which meant `utils.format` next to `report.format` compiled successfully and answered with the wrong one. Qualifying names by module is the real fix and is not done yet
- `f.read()` without a size reads up to 64 KiB per call; `open()` modes must be string literals
- A `with` statement needs its context manager's class to be resolvable at compile time (an instantiation, a call with an annotated class return type, or a variable of known class type). `__exit__` runs on every way out, receives an exception leaving the block, and suppresses it by answering true, as in CPython. Classes are not values here, so `__exit__`'s `exc_type` receives the exception object as well (and `tb` None); either may only be compared with None or tested for truth, and any other use is refused
- Division by zero raises `ZeroDivisionError` (integer and float, `/`, `//`, and `%`), catchable like any other exception
- `**` computes by repeated multiplication: a fractional float exponent (`2.0 ** 0.5`) traps, since it needs exp/log this runtime does not carry, and a negative integer exponent traps because Python's answer is a float an int result cannot hold
- Sequence indexing is checked: a negative index counts from the end, an index outside the sequence raises `IndexError`, a missing dict key raises `KeyError`, and item assignment into a tuple or a string is a compile error. Slicing clamps instead of raising, as Python's does
- Where Python raises over a value a collection does not hold, so does the compiled module, catchably: `list.index(v)` and `list.remove(v)` of a missing value raise `ValueError`, `set.remove(v)` of a missing member raises `KeyError`, and `list.pop()` from an empty list or `list.pop(i)` with a position the list does not have raises `IndexError`. `list.count(v)` answers 0 and `set.discard(v)` ignores the miss, both as Python's do. `list.extend()` takes a list, or a tuple whose members share a type; any other iterable (a range, a set, a string) is refused
- A method called with the wrong number of arguments is a compile error, not a silently ignored one: `xs.append(3, 4)` and `xs.clear(9)` are refused. The optional forms Python accepts compile, so `pop()`, `pop(i)`, `sort()`, and `sort(reverse=True)` all work
- A collection reads every slot at one width, so a value written into one is converted to its element type: an int into a `List[float]` widens. Where it cannot be converted the write is refused rather than stored wrong: a float into a collection of ints (it would truncate), a float into a collection with no element type, a value whose type is unknown into a collection of floats, and `list.extend()` between lists of different widths
- `int` is 32-bit and wraps on overflow rather than growing like CPython's, which is the one place waspy answers differently without saying so by design. See [Numbers](#numbers), and [Known wrong answers](#known-wrong-answers) for the defects that do the same by accident
- Decorators: the standard ones (`@staticmethod`, `@classmethod`, `@property`/`@x.setter`, `@abstractmethod`, `@dataclass`, `@total_ordering`, `@singledispatch`/`@f.register`, and the caching decorators), and decorators the program writes, on module functions. A user-written decorator is applied at compile time: its wrapper is specialized for each function it decorates, so a wrapper forwarding `*args` and `**kwargs` gets the function's own parameters, state it keeps through `nonlocal` is one per decorated function as in CPython, and a factory (`@repeat(3)`) and a stack of decorators work. A wrapper that uses `*args` other than to pass it on, a user decorator on a method or a class, `@cached_property`, `@singledispatchmethod`, and any other name are refused. `functools.partial`, `reduce`, and `cmp_to_key` are refused when called, like any other name the program does not define and the compiler does not implement (`divmod()` and `hash()`, for example)
- A `@singledispatch` call needs a first argument whose type the compiler knows (a literal, an annotated parameter, an instance); dispatching on an untyped value is refused rather than sent to the base implementation
- Reading an attribute through a value whose type is not known, most often an unannotated parameter, is a compile error with a hint to annotate it. The rich comparison methods are the exception: an unannotated `other` in `__eq__`/`__lt__`/... is typed as the class. A field the class does not have is refused too
- Exceptions are objects, as in CPython: `raise E(args)` raises an instance, `except E as e` binds it, and `str(e)`, `repr(e)`, `e.args`, a user exception's own fields and `__str__`, and `raise X from Y` (`e.__cause__`) work. `except Base` catches its subclasses, along user classes and CPython's builtin hierarchy (`LookupError` over `IndexError` and `KeyError`, `ArithmeticError` over `ZeroDivisionError`, and the rest). The runtime's own faults carry CPython's messages (`list index out of range`, `invalid literal for int() with base 10: 'x'`, `KeyError('k')`). Types are static, so the name a handler binds has the class the handler names (the common base of several): a field only a subclass has is refused through it. `e.args` reads back when every argument is a str, and traps otherwise. A traceback is not kept: a `__main__` block's uncaught exception writes only the last line of CPython's report (`ValueError: boom`), and `UnicodeDecodeError`'s position counts from the start of the line where CPython counts from its read buffer
- A format spec CPython rejects (`format(1.5, "d")`, `"{:,s}".format("a")`) is a compile error carrying CPython's `ValueError` message when the spec is a literal, and traps when it is built at run time. `str.format` and `%` need a literal format string; a `*` width or precision in `%`, and a precision on `%d`/`%x`/`%o`, are refused. The default `repr` of an instance whose class defines neither `__repr__` nor `__str__` is refused, since CPython's includes an address
- A lambda's parameters, and a nested function's unannotated ones, are typed from the calls in their scope when those agree, and a `sorted()` key's from the elements it is given. A parameter nothing types (a lambda only passed on to an unannotated parameter, or called with an int in one place and a str in another) holds a word like any untyped place, so indexing it, measuring it, or calling a method on it is refused; annotate a named `def` instead
- Comparing two collections needs both to have the same, known element types: `(1, 2) == (1.0, 2)` (True in CPython) and a comparison with an untyped list are refused rather than read at the wrong width. Dict and set equality, and a list, dict, or set as a set member or dict key (unhashable in CPython), are refused. A class that defines `__eq__` cannot be a set member or dict key, since calling a user `__hash__` is not supported
- `float()` and `int()` of a string take ASCII digits and whitespace only: CPython also accepts other Unicode decimal digits and spaces, which trap here rather than raise. `round(x, n)` needs `0 <= n <= 22`
- A collection holds one element type, so a literal mixing types no one type holds (`[1, "a"]`, `{"n": "bob", "age": 30}`, `[1, True]`) is refused; use a class or a tuple for a record. A tuple is typed per position, so `(1, 2.5)` and `(1, "a")` are fine, but walking one whose members differ (a loop, `sorted`, `sum`) is refused
- A place with no type holds a single word read back as an int: an unannotated parameter or return value, and the elements of a bare `list`, `set`, or `dict` parameter or return. Only ints, bools, None, and functions pass through one; a string, float, collection, or instance is refused with a hint to annotate, since it would lose its length, width, or type on the way (a string passed to `def f(s): return len(s)` used to answer its first four bytes, and runtime strings returned through `-> list` stopped matching as dict keys, [#116](https://github.com/anistark/waspy/issues/116)). A bare annotation on a variable or field is fine: its elements take the type of the value assigned. `len()` of an untyped value is refused for the same reason
- None is the word 0, which an untyped value can also be, so `x is None` on an untyped value is refused. An `Optional[int]`, `Optional[bool]`, or `Optional[float]` is boxed, so None is told apart from 0: `is None`, `==`, truthiness, `str()`, and `dict.get(key)` on number values give CPython's answer, and arithmetic or an ordering on None raises its `TypeError`. An optional number passed or returned where a plain one is declared (past an `is None` test, typically) is its number, and None there traps, since CPython would pass the None on and the place cannot hold it. Arithmetic between two optional numbers is refused
- A variable, field, or collection that holds instances of more than one class is typed as their nearest common base for the whole function, so a method call on it dispatches on each instance's own class. Types are not tracked per program point, so reading a field that only one of the subclasses has through it is refused, and so is a method call on instances with no common base. Storing an instance where an annotation names an unrelated class (a `Rect` passed to a `Circle` parameter, or returned from `-> Circle`) is refused rather than dispatched as the annotated class
- `len()` of a generator is refused, as CPython raises `TypeError`
- No garbage collection or reference counting: the bump allocator never frees

### Known wrong answers

None are known. Every construct the board audit found compiling and then answering something CPython does not, the stdlib placeholders, `tuple.index`, repetition, the set and dict operators, `except ... as e`, keyword arguments, `is None`, and the rest, now gives CPython's answer or is a compile error. The one deliberate difference is integer width (see [Numbers](#numbers)). If you find a program that compiles and answers differently from CPython, that is a bug: please [report it](https://github.com/anistark/waspy/issues).

### Explicitly unsupported (rejected at compile time)

The compiler validates syntax up front and rejects these with a located error and a hint, rather than miscompiling them:

- `async def` / `await` / `async for` / `async with` (planned for 1.0)
- `match` statements, `type` aliases, `except*`, and `del` of a name, an attribute, or a stepped slice
- `from module import *`
- `*args`, `**kwargs`, and keyword-only parameters on methods other than `__init__`, `**` unpacking in a call, and positional-only parameters (`/`)
- Class keywords other than `metaclass=` (`__init_subclass__` arguments)
- Decorators CPython does not define (`@memoize`, `@timer`, `@debug`, `@pure`, `@type_check`, `@default_value`, `@wasm_export`), which CPython rejects with `NameError`
- Module-level statements other than definitions. A WebAssembly module has no top-level run step, so only definitions run: `X = 1`, `X: int = 1`, `X = helper()`, `X = ClassName()`, `def`, `class`, and imports (a `try`/`except ImportError` guarded import included). Definitions that are not plain constants are evaluated once, in source order, when the module is instantiated; one that reads a definition appearing after it is refused, as it is a `NameError` in CPython. A module-level loop, `if`, `try`, bare call statement, augmented assignment, tuple unpacking, or write through a subscript or attribute is rejected, because it would otherwise be compiled away and later reads would silently see the value from before it. Put the code in a function and call it, or drive it from `if __name__ == "__main__":`, which is recognized as the entry point

A set iterates in CPython's order: its hashes for ints, bools, floats, and tuples of them, and its table algorithm, are CPython's, so a loop over a set, `pop()`, and the order a set prints in agree with CPython 3.13 for those members. A set of strings has no reproducible order in CPython, whose string hash is keyed per process; here the order is fixed but unspecified. `remove` of a value the set does not hold raises `KeyError`; `discard` ignores the miss like Python's.

## Installation

```sh
cargo add waspy
```

Or add it to your `Cargo.toml`:

```toml
[dependencies]
waspy = "0.18.0"
```

The Binaryen optimization pass is the default `optimize` feature. Binaryen is a native C++ library, so turn the feature off to build waspy for a target without a C++ toolchain, such as `wasm32-unknown-unknown`. `CompilerOptions::optimize` then has no effect, and the output is the unoptimized module, which is already valid and correct:

```toml
[dependencies]
waspy = { version = "0.18.0", default-features = false }
```

To try it without installing anything, the [playground](https://anistark.github.io/waspy/playground/) runs waspy itself, built this way, in the browser.

## Quick Start

### Using the Library

```rust
use waspy::compile_python_to_wasm;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let python_code = r#"
    def add(a: int, b: int) -> int:
        return a + b
        
    def fibonacci(n: int) -> int:
        if n <= 1:
            return n
        a = 0
        b = 1
        i = 2
        while i <= n:
            temp = a + b
            a = b
            b = temp
            i = i + 1
        return b
    "#;
    
    let wasm = compile_python_to_wasm(python_code)?;
    // Write to file or use the WebAssembly binary
    std::fs::write("output.wasm", &wasm)?;
    
    Ok(())
}
```

With Compiler Options:

```rust
use waspy::{compile_python_to_wasm_with_options, CompilerOptions, Verbosity};

let options = CompilerOptions {
    optimize: true,                 // Binaryen pass over the output (default: true)
    verbosity: Verbosity::Verbose,  // or Verbosity::Debug
};

let wasm = compile_python_to_wasm_with_options(python_code, &options)?;
```

### Verbosity Levels

Waspy supports different verbosity levels for logging output:

- **`Verbosity::Quiet`** - Minimal output (errors only)
- **`Verbosity::Normal`** - Standard output (default)
- **`Verbosity::Verbose`** - Detailed output
- **`Verbosity::Debug`** - Detail for debugging

If your project has `--verbose` or `--debug` flags, use the `from_flags` helper:

```rust
use waspy::{CompilerOptions, Verbosity};

// Map CLI flags to verbosity level
let options = CompilerOptions {
    verbosity: Verbosity::from_flags(verbose_flag, debug_flag),
    ..CompilerOptions::default()
};
```

For multiple files compilation:

```rust
use waspy::compile_multiple_python_files;

let sources = vec![
    ("math.py", "def add(a: int, b: int) -> int:\n    return a + b"),
    ("main.py", "def compute(x: int) -> int:\n    return add(x, 10)")
];

let wasm = compile_multiple_python_files(&sources, true)?;
```

For unoptimized WebAssembly (useful for debugging or further processing):

```rust
use waspy::{compile_python_to_wasm_with_options, CompilerOptions};

let options = CompilerOptions {
    optimize: false,
    ..CompilerOptions::default()
};
let wasm = compile_python_to_wasm_with_options(python_code, &options)?;
```

Compiling an entry file with its own module imports resolved from disk (`import mod` finds the sibling `mod.py`, transitively):

```rust
use waspy::compile_python_file;

let wasm = compile_python_file("app/main.py", true)?;
```

Compiling Projects:

```rust
use waspy::compile_python_project;

let wasm = compile_python_project("./my_python_project", true)?;
```

### Example Python Code

```python
def factorial(n: int) -> int:
    result = 1
    i = 1
    while i <= n:
        result = result * i
        i = i + 1
    return result

def max_num(a: float, b: float) -> float:
    if a > b:
        return a
    else:
        return b
```

### Using the Generated WebAssembly

The compiled WebAssembly can be used in various environments:

```js
// Browser or Node.js
WebAssembly.instantiate(wasmBuffer).then(result => {
  const instance = result.instance;
  console.log(instance.exports.factorial(5)); // 120
  console.log(instance.exports.max_num(42, 17)); // 42
});
```

## Implementation Details

### Multiple Functions

Waspy supports multiple function definitions:

- Each function is compiled to a separate WebAssembly function
- All functions are exported with their original names
- Functions can call other functions within the same module
- Functions from multiple files can be compiled into a single module

### Type System

The type system now includes:

- **Type Annotations**: Support for Python's type hints on function params and return values
- **Integers**: Mapped to WebAssembly's `i32` type
- **Floats**: Supported as `f64` with conversion to `i32` when necessary
- **Booleans**: Represented as `i32` (`0` for `false`, `1` for `true`)
- **Strings**: An `(offset, length)` pair into linear memory; methods run on a runtime receiver, and a literal receiver is folded at compile time
- **Type Coercion**: Automatic conversion between compatible types when needed

### Control Flow

The compiler supports basic control flow constructs:

- **If/Else Statements**: Conditional execution using WebAssembly's block and branch instructions
- **While and For Loops**: Implemented using WebAssembly's loop and branch instructions
- **Break and Continue**: Early loop exit and next-iteration skip, including correct
  behavior when nested inside `if`/`try` blocks and in nested loops
- **Comparison Operators**: All standard Python comparison operators
- **Boolean Operators**: Support for `and` and `or` with short-circuit evaluation

### Variable Support

Waspy handles variables through WebAssembly locals:

- Local variables are allocated in the function's local variable space
- Assignment statements modify these locals
- Variables can be statically typed with annotations
- Type inference for variables based on usage

### Error Handling

Enhanced error reporting system:

- Python syntax errors report their line and column
- Known-unsupported constructs are rejected before code generation with the construct named, its location, the enclosing function, and a workaround hint
- Specific error types for different issues (parsing, type errors, unsupported features, name errors)
- A construct the compiler cannot compile correctly is an error, not a warning: two modules defining the same function name, a decorator it does not implement, a call to a name it does not know, and a write it cannot make good on all fail the build rather than reporting success

### The host interface

A compiled program's side effects go through [WASI preview 1](https://github.com/WebAssembly/WASI/blob/main/legacy/preview1/docs.md),
importing from `wasi_snapshot_preview1` exactly the functions it uses, so a
program that prints nothing, reads nothing, and opens nothing imports nothing
and instantiates with an empty import object. The same module runs unchanged
under `wasmtime run`, Node's `node:wasi`, or the small browser shim the
playground ships (`docs/playground/wasi.js`).

| What the program does | Imports |
|------|---------|
| `print()`, `sys.stdout.write()`, `sys.stderr.write()` | `fd_write` |
| `input()`, `sys.stdin.readline()` / `read()` / `readlines()`, `for line in sys.stdin` | `fd_read` (and `fd_write` for a prompt) |
| `sys.argv` | `args_sizes_get`, `args_get` |
| a `__main__` block that can raise an exception it does not catch | `fd_write`, `proc_exit` |
| `open()` and the file methods | `path_open`, `fd_prestat_get`, `fd_prestat_dir_name`, `fd_read`, `fd_write`, `fd_close` |

A program with an `if __name__ == "__main__":` block is a WASI command: it
exports `_start`, which runs the block, so `wasmtime run program.wasm args...`
runs it like `python3 program.py args...`. An exception the block does not
catch makes it exit with status 1. Every other function is exported too and
can be called directly, as before; an exception escaping one traps.

Writes are unbuffered, one `fd_write` per `print` or `write`, so standard output
and standard error come out in the order the program wrote them. Standard input
is read in UTF-8, as CPython's is on POSIX: lines end at `\n` alone (a `\r`
is kept, as CPython keeps it there), and bytes that are not UTF-8 raise
`UnicodeDecodeError`. `input()` at the end of input raises `EOFError`.
`sys.argv[0]` is whatever the host passes first (`wasmtime` passes the
module's path). `sys.stdout` and the other streams are used through their
methods; one passed around as a value is refused.

#### File I/O

A program that calls `open()` also imports `path_open`, `fd_prestat_get`,
`fd_prestat_dir_name`, `fd_read`, `fd_write`, and `fd_close`. A path is
resolved against the host's preopened directories as wasi-libc resolves it:
an absolute path against the longest preopen naming a prefix of it, a relative
one against the first relative preopen (`.`), else `/`. So
`wasmtime run --dir . program.wasm` reads and writes the current directory, and
a program run with no preopen can open nothing. A file `open()` cannot open
raises CPython's exception, with CPython's message on a POSIX host:
`FileNotFoundError: [Errno 2] No such file or directory: 'notes.txt'`, and
`PermissionError`, `IsADirectoryError`, `NotADirectoryError`, and
`FileExistsError` for the matching WASI errors. A size-less `f.read()` asks for
at most one 64 KiB page per call.

**Confinement is the preopens'.** A program reaches only the directories its
host preopened, which is WASI's sandbox, so a host running untrusted Python
chooses them, and its own limits on descriptor count and transfer size. The
playground's shim (`docs/playground/wasi.js`) keeps files in a `Map` and
touches no filesystem.

**The `waspy_host` interface (deprecated).** With
`CompilerOptions::waspy_host_file_io` set, file I/O compiles instead to four
imports from the `waspy_host` namespace, as it did before WASI, for embedders
that implement them. It is kept until 1.0 freezes the API.

| Import | Signature | Result |
|--------|-----------|--------|
| `waspy_host.open` | `(path_ptr: i32, path_len: i32, flags: i32) -> i32` | file descriptor, or `-1` |
| `waspy_host.read` | `(fd: i32, buf_ptr: i32, len: i32) -> i32` | bytes read, `0` at EOF, negative on error |
| `waspy_host.write` | `(fd: i32, buf_ptr: i32, len: i32) -> i32` | bytes written, negative on error |
| `waspy_host.close` | `(fd: i32) -> i32` | `0` on success |

`path_ptr`/`path_len` and `buf_ptr`/`len` address the module's own linear
memory, which the host reads and writes through the exported `memory`. `flags`
is the Python mode string folded at compile time: `1` read, `2` write, `4`
append, `8` binary, `16` update, or-ed together, so `"w+b"` is `26`. Under
this interface the embedder owns confinement (the program chooses the path it
passes), and a failed open answers -1 rather than raising.

### Comment Preservation

Comments from the Python sources are preserved in the generated binary as a `python.comments` custom section: UTF-8 text, one `file:line:text` entry per line. Custom sections carry no code, so this changes nothing about how the module runs, and the section survives optimization. Read it back with `waspy::core::comments::comments_from_wasm`, or with any tool that dumps WebAssembly custom sections.

### Testing

Every bundled example compiles, instantiates, and has its runtime results asserted by the integration suite (`tests/integration/`), alongside operator-level unit tests (`tests/unit/`). Run everything with `just test`, the full CI-equivalent gate with `just ci`, or compile every example through the real drivers with `just verify-examples`.

## Examples

Waspy includes several examples to demonstrate its functionality:

```sh
# Basic compiler example
cargo run --example simple_compiler

# Advanced compiler with options
cargo run --example advanced_compiler examples/typed_demo.py --metadata --html

# Multi-file compilation
cargo run --example multi_file_compiler examples/output/combined.wasm examples/basic_operations.py examples/calculator.py

# Project compilation
cargo run --example project_compiler examples/calculator_project examples/output/project.wasm

# Type system demonstration
cargo run --example typed_demo
```

Or through the justfile:

```sh
just compile examples/typed_demo.py     # compile one file (reports sizes)
just verify-examples                    # compile every bundled example
just benchmark                          # time compilation (release build)
just examples                           # run the full driver suite
```

The docs-site playground (`docs/playground/`) is built from `playground/`, a small wasm-bindgen wrapper crate. It needs the `wasm32-unknown-unknown` target and the wasm-bindgen CLI at the version `playground/Cargo.toml` pins:

```sh
just playground          # build docs/playground/compiler/
just playground-verify   # compile every playground example, check against CPython
just playground-serve    # build, then serve the site at localhost:8000/playground/
```

## Contributing

Contributions are welcome! See [CONTRIBUTING.md](CONTRIBUTING.md) for details on how to get started.

## Roadmap

The path to 1.0 focuses on the remaining correctness and runtime gaps:

- The standard library on runtime values: `math`, `random`, `sys`, `os`, `collections`, `itertools`, `functools`, `json`, `re`, `datetime`, `logging`
- Hashed `dict` lookups (sets already use CPython's hash table)
- Garbage collection / reference counting for the bump-allocated heap

![waspy](./assets/waspy.png)

## License

[MIT](./LICENSE)
