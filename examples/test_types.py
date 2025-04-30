def is_even(n):
    return (n % 2) == 0

def truncate_float(x):
    # Converts float to int by truncation
    return int(x)

def average(a, b):
    # Returns the average as an integer
    return (a + b) // 2

def absolute(x):
    if x < 0:
        return -x
    else:
        return x

def logical_operations(a, b):
    # Demonstrates boolean operations
    result = 0
    
    if a > 0 and b > 0:
        result = 1
    
    if a > 0 or b > 0:
        result = result + 2
        
    if not (a == b):
        result = result + 4
        
    return result
