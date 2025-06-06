"""
Type system demonstration for Waspy.
This file demonstrates various type annotations and type conversions.
"""

def add_integers(a: int, b: int) -> int:
    """Add two integers and return an integer."""
    return a + b

def add_floats(a: float, b: float) -> float:
    """Add two floats and return a float."""
    return a + b

def mixed_types(a: int, b: float) -> float:
    """Demonstrate automatic type conversion."""
    return a + b

def bool_operations(a: bool, b: bool) -> bool:
    """Demonstrate boolean operations."""
    return a and b

def comparisons(a: int, b: int) -> bool:
    """Demonstrate comparison operators."""
    return (a > b) and (a >= b) or (a < b) or (a <= b) or (a == b) or (a != b)

def int_to_float(a: int) -> float:
    """Convert an integer to a float."""
    return float(a)

def float_to_int(a: float) -> int:
    """Convert a float to an integer."""
    return int(a)

def power_calculation(base: float, exponent: int) -> float:
    """Calculate base raised to exponent power."""
    result = 1.0
    i = 0
    
    if exponent < 0:
        # For simplicity, just return 0 for negative exponents
        return 0.0
    
    while i < exponent:
        result = result * base
        i = i + 1
        
    return result
