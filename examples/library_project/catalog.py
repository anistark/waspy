"""A collection of books, and the operations over it."""

from models import Book


class Catalog:
    def __init__(self):
        self.books = []

    def add(self, title: str, author: str, year: int, copies: int) -> Book:
        book = Book(title, author, year, copies)
        self.books.append(book)
        return book

    def size(self) -> int:
        return len(self.books)

    def total_copies(self) -> int:
        total = 0
        for book in self.books:
            total = total + book.copies
        return total

    def available_copies(self) -> int:
        total = 0
        for book in self.books:
            total = total + book.available()
        return total

    def by_author(self, author: str) -> int:
        found = 0
        for book in self.books:
            if book.author == author:
                found = found + 1
        return found

    def oldest_year(self) -> int:
        best = 0
        for book in self.books:
            if best == 0 or book.year < best:
                best = book.year
        return best

    def borrow_title(self, title: str) -> bool:
        for book in self.books:
            if book.title == title:
                return book.borrow()
        return False
