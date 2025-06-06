"""
Module-level demonstration for Waspy.
This demonstrates support for module-level variables and simple classes.
"""

# Module-level variables
PI = 3.14159
VERSION = "1.0.0"
DEBUG = False
MAX_VALUE = 100

# Variable with type annotation
message: str = "Hello from module level"
count: int = 42

# Class definition
class Rectangle:
    """A simple rectangle class."""
    
    # Class variables
    default_width = 10
    default_height = 5
    
    def __init__(self, width: float, height: float):
        """Initialize the rectangle with width and height."""
        self.width = width
        self.height = height
    
    def area(self) -> float:
        """Calculate the area of the rectangle."""
        return self.width * self.height
    
    def perimeter(self) -> float:
        """Calculate the perimeter of the rectangle."""
        return 2 * (self.width + self.height)
    
    def scale(self, factor: float) -> None:
        """Scale the rectangle by a factor."""
        self.width *= factor
        self.height *= factor

# Functions that use module-level variables
def get_pi() -> float:
    """Return the value of PI."""
    return PI

def is_debug_mode() -> bool:
    """Check if debug mode is enabled."""
    return DEBUG

def get_message() -> str:
    """Return the module-level message."""
    return message

def calculate_circle_area(radius: float) -> float:
    """Calculate the area of a circle using PI."""
    return PI * radius * radius

def calculate_circle_circumference(radius: float) -> float:
    """Calculate the circumference of a circle using PI."""
    return 2 * PI * radius

def create_default_rectangle() -> Rectangle:
    """Create a rectangle with default dimensions."""
    return Rectangle(Rectangle.default_width, Rectangle.default_height)

def get_version() -> str:
    """Return the version string."""
    return VERSION