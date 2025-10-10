def divide(a: float, b: float) -> float:
    """
    Divides a by b and raises ValueError if division by zero occurs.
    Returns the result as float.
    """
    try:
        return a / b
    except ZeroDivisionError:
        raise ValueError("Cannot divide by zero")
    except ValueError:
        raise
    finally:
        print("Execution completed.")
