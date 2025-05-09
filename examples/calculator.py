"""
Calculator implementation for ChakraPy multi-file demonstration.
This file uses functions from basic_operations.py to implement a calculator.
"""

def calculate(operation: str, a: int, b: int) -> int:
    """Perform the specified calculation using functions from basic_operations.py."""
    if operation == "add":
        return add(a, b)
    elif operation == "subtract":
        return subtract(a, b)
    elif operation == "multiply":
        return multiply(a, b)
    elif operation == "divide":
        return divide(a, b)
    elif operation == "modulo":
        return modulo(a, b)
    else:
        return 0  # Unknown operation

def complex_calculation(x: int, y: int) -> int:
    """Perform a more complex calculation using multiple operations."""
    # (x + y) * (x - y)
    sum_result = add(x, y)
    diff_result = subtract(x, y)
    return multiply(sum_result, diff_result)

def apply_operations(a: int, b: int) -> int:
    """Apply multiple operations in sequence."""
    # (a + b) - (a * b) / 2
    sum_result = add(a, b)
    product = multiply(a, b)
    half_product = divide(product, 2)
    return subtract(sum_result, half_product)

def calculate_factorial(n: int) -> int:
    """Calculate factorial recursively."""
    if n <= 1:
        return 1
    return multiply(n, calculate_factorial(subtract(n, 1)))
