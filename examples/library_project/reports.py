"""Formatting, kept away from the domain and the collection."""

from models import Book


def book_line(book: Book) -> str:
    return book.label() + " (" + str(book.year) + ")"


def availability_line(book: Book) -> str:
    return book.label() + ": " + str(book.available()) + " of " + str(book.copies)
