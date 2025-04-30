def factorial(n):
    result = 1
    i = 1
    while i <= n:
        result = result * i
        i = i + 1
    return result

def fibonacci(n):
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

def max_num(a, b):
    if a > b:
        return a
    else:
        return b
