"""
Classic small algorithms for Waspy.

Demonstrates richer arithmetic and control flow than basic_operations.py:
loops with multiple exit conditions, integer math (gcd, primality, digit
manipulation), and mixed int/float computation.
"""


def gcd(a: int, b: int) -> int:
    """Greatest common divisor by Euclid's algorithm."""
    while b != 0:
        temp = b
        b = a % b
        a = temp
    return a


def is_prime(n: int) -> bool:
    """Trial-division primality test."""
    if n < 2:
        return False
    if n < 4:
        return True
    if n % 2 == 0:
        return False
    i = 3
    while i * i <= n:
        if n % i == 0:
            return False
        i = i + 2
    return True


def digit_sum(n: int) -> int:
    """Sum of the decimal digits of a non-negative integer."""
    total = 0
    while n > 0:
        total = total + n % 10
        n = n // 10
    return total


def collatz_steps(n: int) -> int:
    """Number of Collatz steps to reach 1."""
    steps = 0
    while n > 1:
        if n % 2 == 0:
            n = n // 2
        else:
            n = 3 * n + 1
        steps = steps + 1
    return steps


def reverse_number(n: int) -> int:
    """Reverse the decimal digits of a non-negative integer."""
    result = 0
    while n > 0:
        result = result * 10 + n % 10
        n = n // 10
    return result


def approximate_sqrt(x: float) -> float:
    """Square root by ten iterations of Newton's method."""
    if x <= 0.0:
        return 0.0
    guess = x
    i = 0
    while i < 10:
        guess = (guess + x / guess) / 2.0
        i = i + 1
    return guess
