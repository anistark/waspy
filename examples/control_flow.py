"""
Control flow examples for ChakraPy.
This demonstrates if/else statements and while loops.
"""

def factorial(n: int) -> int:
    """Calculate the factorial of n using a while loop."""
    result = 1
    i = 1
    while i <= n:
        result = result * i
        i = i + 1
    return result

def fibonacci(n: int) -> int:
    """Calculate the nth Fibonacci number using if/else and while loop."""
    if n <= 1:
        return n
    else:
        a = 0
        b = 1
        i = 2
        while i <= n:
            temp = a + b
            a = b
            b = temp
            i = i + 1
        return b

def max_num(a: int, b: int) -> int:
    """Return the maximum of two numbers using if/else."""
    if a > b:
        return a
    else:
        return b

def is_even(n: int) -> bool:
    """Check if a number is even using the modulo operator."""
    return (n % 2) == 0

def count_up_to(n: int) -> int:
    """Sum numbers from 1 to n using a while loop."""
    total = 0
    i = 1
    while i <= n:
        total = total + i
        i = i + 1
    return total
