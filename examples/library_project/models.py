"""The domain type, shared across the whole project."""


class Book:
    def __init__(self, title: str, author: str, year: int, copies: int):
        self.title = title
        self.author = author
        self.year = year
        self.copies = copies
        self.out = 0

    def available(self) -> int:
        return self.copies - self.out

    def is_available(self) -> bool:
        return self.available() > 0

    def borrow(self) -> bool:
        if self.is_available():
            self.out = self.out + 1
            return True
        return False

    def give_back(self) -> bool:
        if self.out > 0:
            self.out = self.out - 1
            return True
        return False

    def label(self) -> str:
        return self.title + " by " + self.author
