"""
Basic arithmetic operations example for ChakraPy.
This demonstrates the core functionality of the compiler with simple functions.
"""

def add(a: int, b: int) -> int:
    """Add two integers."""
    return a + b

def subtract(a: int, b: int) -> int:
    """Subtract one integer from another."""
    return a - b

def multiply(a: int, b: int) -> int:
    """Multiply two integers."""
    return a * b

def divide(a: int, b: int) -> int:
    """Divide one integer by another (integer division)."""
    if b == 0:
        return 0  # Simple error handling
    return a // b

def modulo(a: int, b: int) -> int:
    """Get the remainder of division."""
    if b == 0:
        return 0
    return a % b

def combined_operation(a: int, b: int) -> int:
    """Demonstrate a combination of operations."""
    return add(multiply(a, b), subtract(a, b))
