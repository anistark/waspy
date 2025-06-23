"""
Type system demonstration for Waspy.

This file demonstrates various type annotations and type conversions
in Waspy's Python to WebAssembly compiler.
"""

# Module-level constants with type annotations
PI: float = 3.14159
MAX_VALUE: int = 100
DEBUG: bool = False
MESSAGE: str = "Hello from typed demo"

def add_integers(a: int, b: int) -> int:
    """Add two integers and return an integer."""
    return a + b

def add_floats(a: float, b: float) -> float:
    """Add two floats and return a float."""
    return a + b

def mixed_types(a: int, b: float) -> float:
    """Demonstrate automatic type conversion.
    
    The integer 'a' is automatically converted to float
    when added to the float 'b'.
    """
    return a + b

def bool_operations(a: bool, b: bool) -> bool:
    """Demonstrate boolean operations.
    
    Returns (a AND b) OR (NOT a)
    """
    return (a and b) or (not a)

def comparisons(a: int, b: int) -> bool:
    """Demonstrate comparison operators.
    
    Returns true if a > b and a != b
    """
    return (a > b) and (a != b)

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

def calculate_circle_area(radius: float) -> float:
    """Calculate the area of a circle using PI."""
    return PI * radius * radius

def calculate_circle_circumference(radius: float) -> float:
    """Calculate the circumference of a circle using PI."""
    return 2 * PI * radius

def is_debug_mode() -> bool:
    """Check if debug mode is enabled."""
    return DEBUG

def get_message() -> str:
    """Return the module-level message."""
    return MESSAGE
