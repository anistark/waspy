"""
Test all standard library modules to verify they compile properly.
This file tests imports and basic attribute access for all stdlib modules.

The imports stay at module level (that is part of what this example
exercises); the attribute accesses live in main() so the file compiles
through every entry point, including the per-file driver.
"""

import sys
import os
import math
import random
import json
import re
import datetime
import collections
import itertools
import functools


def main() -> int:
    # Test sys module
    print("Testing sys module...")
    platform = sys.platform
    version = sys.version
    maxsize = sys.maxsize
    print(f"Platform: {platform}")
    print(f"Version: {version}")
    print(f"Max size: {maxsize}")

    # Test os module
    print("\nTesting os module...")
    os_name = os.name
    separator = os.sep
    path_sep = os.pathsep
    line_sep = os.linesep
    print(f"OS name: {os_name}")
    print(f"Separator: {separator}")
    print(f"Path separator: {path_sep}")

    # Test math module
    print("\nTesting math module...")
    pi_value = math.pi
    e_value = math.e
    tau_value = math.tau
    print(f"Pi: {pi_value}")
    print(f"E: {e_value}")
    print(f"Tau: {tau_value}")

    # Test math functions (function references, not calls yet)
    # These test that the functions exist and can be referenced
    sqrt_func = math.sqrt
    sin_func = math.sin
    cos_func = math.cos
    floor_func = math.floor
    ceil_func = math.ceil
    print("Math functions loaded successfully")

    # Test random module
    print("\nTesting random module...")
    # Test function references
    random_func = random.random
    randint_func = random.randint
    choice_func = random.choice
    print("Random functions loaded successfully")

    # Test json module
    print("\nTesting json module...")
    loads_func = json.loads
    dumps_func = json.dumps
    load_func = json.load
    dump_func = json.dump
    print("JSON functions loaded successfully")

    # Test re module
    print("\nTesting re module...")
    # Test regex flags
    ignorecase_flag = re.IGNORECASE
    multiline_flag = re.MULTILINE
    dotall_flag = re.DOTALL
    print(f"IGNORECASE flag: {ignorecase_flag}")
    print(f"MULTILINE flag: {multiline_flag}")
    # Test function references
    compile_func = re.compile
    search_func = re.search
    match_func = re.match
    findall_func = re.findall
    print("Regex functions loaded successfully")

    # Test datetime module
    print("\nTesting datetime module...")
    minyear = datetime.MINYEAR
    maxyear = datetime.MAXYEAR
    print(f"MINYEAR: {minyear}")
    print(f"MAXYEAR: {maxyear}")
    # Test type references
    datetime_type = datetime.datetime
    date_type = datetime.date
    time_type = datetime.time
    print("Datetime types loaded successfully")

    # Test collections module
    print("\nTesting collections module...")
    # Test function references
    namedtuple_func = collections.namedtuple
    deque_func = collections.deque
    counter_func = collections.Counter
    print("Collections functions loaded successfully")

    # Test itertools module
    print("\nTesting itertools module...")
    # Test function references
    count_func = itertools.count
    cycle_func = itertools.cycle
    repeat_func = itertools.repeat
    chain_func = itertools.chain
    product_func = itertools.product
    permutations_func = itertools.permutations
    print("Itertools functions loaded successfully")

    # Test functools module
    print("\nTesting functools module...")
    # Test function references
    reduce_func = functools.reduce
    partial_func = functools.partial
    wraps_func = functools.wraps
    lru_cache_func = functools.lru_cache
    cache_func = functools.cache
    print("Functools functions loaded successfully")

    print("\n=== All standard library modules tested successfully! ===")
    return 0


if __name__ == "__main__":
    main()
