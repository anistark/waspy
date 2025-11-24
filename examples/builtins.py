def test_len():
    s = "hello"
    s_len = len(s)
    return s_len

def test_len_list():
    lst = [1, 2, 3, 4, 5]
    lst_len = len(lst)
    return lst_len

def test_min_max():
    a = min(5, 3, 8, 1, 9)
    b = max(5, 3, 8, 1, 9)
    return a

def test_print():
    print("Hello")
    print(42)
    print("World")

def main():
    test_len()
    test_len_list()
    test_min_max()
    test_print()
