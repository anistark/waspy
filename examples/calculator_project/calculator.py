def percent_of(value: int, percent: int) -> int:
    return multiply(value, percent) // 100


def average(a: int, b: int, c: int) -> int:
    return divide(add(add(a, b), c), 3)


def net_price(price: int, tax_percent: int) -> int:
    return add(price, percent_of(price, tax_percent))
