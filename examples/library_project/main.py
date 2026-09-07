"""Entry point: build a catalog, exercise it, report on it."""

from catalog import Catalog
from models import Book
from reports import availability_line, book_line


def build() -> Catalog:
    catalog = Catalog()
    catalog.add("Dune", "Herbert", 1965, 3)
    catalog.add("Neuromancer", "Gibson", 1984, 2)
    catalog.add("Count Zero", "Gibson", 1986, 1)
    return catalog


def catalog_size() -> int:
    return build().size()


def total_copies() -> int:
    return build().total_copies()


def gibson_titles() -> int:
    return build().by_author("Gibson")


def oldest() -> int:
    return build().oldest_year()


def borrow_flow() -> int:
    catalog = build()
    catalog.borrow_title("Dune")
    catalog.borrow_title("Count Zero")
    catalog.borrow_title("Count Zero")
    return catalog.available_copies()


def first_line() -> str:
    return book_line(build().books[0])


def shared_class_roundtrip() -> str:
    book = Book("Solaris", "Lem", 1961, 4)
    book.borrow()
    return availability_line(book)
