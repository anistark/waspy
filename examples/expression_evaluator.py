"""A small arithmetic evaluator: tokenize, parse, evaluate.

The fourth of the end-to-end programs, and the first one written after 0.17.0.
It was written as ordinary Python, without reference to what waspy supports,
and compiled unchanged. On first contact it found five defects, three of them
silent: a character taken out of a string with `s[i]` was a pointer into the
middle of that string, so passing one to a function, storing it in a field or a
list, or returning it read the preceding characters as its length; `int("12")`
answered 2; `for ch in s` walked memory that was not the string; and an item
assignment whose key or value did any work (`xs[1] = ys[2]`) lost its container
pointer. All five are fixed, and this program is asserted against CPython.

The shape is a tokenizer, a recursive-descent parser, and an AST class
hierarchy whose `evaluate()` and `show()` are overridden per node type, so it
leans on virtual dispatch the way ordinary code does.
"""

from typing import Dict, List


class CalcError(Exception):
    pass


class Token:
    def __init__(self, kind: str, text: str):
        self.kind = kind
        self.text = text


def tokenize(source: str) -> List[Token]:
    tokens: List[Token] = []
    i = 0
    while i < len(source):
        ch = source[i]
        if ch == " ":
            i += 1
        elif ch.isdigit():
            start = i
            while i < len(source) and source[i].isdigit():
                i += 1
            tokens.append(Token("num", source[start:i]))
        elif ch.isalpha():
            start = i
            while i < len(source) and source[i].isalpha():
                i += 1
            tokens.append(Token("name", source[start:i]))
        elif ch in "+-*/()=":
            tokens.append(Token("op", ch))
            i += 1
        else:
            raise CalcError("bad character")
    return tokens


class Node:
    def evaluate(self, env: Dict[str, int]) -> int:
        return 0

    def show(self) -> str:
        return "?"


class Num(Node):
    def __init__(self, value: int):
        self.value = value

    def evaluate(self, env: Dict[str, int]) -> int:
        return self.value

    def show(self) -> str:
        return str(self.value)


class Var(Node):
    def __init__(self, name: str):
        self.name = name

    def evaluate(self, env: Dict[str, int]) -> int:
        if self.name not in env:
            raise CalcError("unknown name")
        return env[self.name]

    def show(self) -> str:
        return self.name


class BinOp(Node):
    def __init__(self, op: str, left: Node, right: Node):
        self.op = op
        self.left = left
        self.right = right

    def evaluate(self, env: Dict[str, int]) -> int:
        a = self.left.evaluate(env)
        b = self.right.evaluate(env)
        if self.op == "+":
            return a + b
        if self.op == "-":
            return a - b
        if self.op == "*":
            return a * b
        if b == 0:
            raise CalcError("division by zero")
        return a // b

    def show(self) -> str:
        return "(" + self.left.show() + " " + self.op + " " + self.right.show() + ")"


class Parser:
    def __init__(self, tokens: List[Token]):
        self.tokens = tokens
        self.pos = 0

    def peek(self) -> str:
        if self.pos < len(self.tokens):
            return self.tokens[self.pos].text
        return ""

    def take(self) -> Token:
        tok = self.tokens[self.pos]
        self.pos += 1
        return tok

    def expression(self) -> Node:
        node = self.term()
        while self.peek() == "+" or self.peek() == "-":
            op = self.take().text
            node = BinOp(op, node, self.term())
        return node

    def term(self) -> Node:
        node = self.factor()
        while self.peek() == "*" or self.peek() == "/":
            op = self.take().text
            node = BinOp(op, node, self.factor())
        return node

    def factor(self) -> Node:
        if self.pos >= len(self.tokens):
            raise CalcError("unexpected end")
        tok = self.take()
        if tok.kind == "num":
            return Num(int(tok.text))
        if tok.kind == "name":
            return Var(tok.text)
        if tok.text == "(":
            node = self.expression()
            if self.peek() != ")":
                raise CalcError("missing )")
            self.take()
            return node
        raise CalcError("unexpected token")


def run(lines: List[str]) -> int:
    env: Dict[str, int] = {}
    last = 0
    for line in lines:
        tokens = tokenize(line)
        if len(tokens) >= 2 and tokens[1].text == "=":
            name = tokens[0].text
            value = Parser(tokens[2:]).expression().evaluate(env)
            env[name] = value
            last = value
        else:
            last = Parser(tokens).expression().evaluate(env)
    return last


def program() -> int:
    return run(["x = 6", "y = x * 7", "(y - 2) / 5 + x"])


def shown() -> str:
    return Parser(tokenize("1 + 2 * (x - 3)")).expression().show()


def precedence() -> int:
    return run(["2 + 3 * 4 - 10 / 2"])


def errors() -> int:
    caught = 0
    for bad in ["1 / 0", "q + 1", "(1 + 2", "3 $ 4", "4 +"]:
        try:
            run([bad])
        except CalcError:
            caught += 1
    return caught


def token_count() -> int:
    return len(tokenize("alpha = 12 * (beta + 345)"))


if __name__ == "__main__":
    print(program(), shown(), precedence(), errors(), token_count())
