"""
Test all standard library modules to verify they compile properly.
This file tests imports and constant access for all stdlib modules. A
function or class taken as a value (`f = math.sqrt`) is not a supported
expression, so this file sticks to constants.

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
    # A float has no runtime str() yet, so an f-string placeholder cannot
    # render one; truncate to an int to print the value.
    print(f"Pi (truncated): {int(pi_value)}")
    print(f"E (truncated): {int(e_value)}")
    print(f"Tau (truncated): {int(tau_value)}")

    print("Math constants loaded successfully")

    # Test random module
    print("\nTesting random module...")
    print("Random functions loaded successfully")

    # Test json module
    print("\nTesting json module...")
    print("JSON functions loaded successfully")

    # Test re module
    print("\nTesting re module...")
    # Test regex flags
    ignorecase_flag = re.IGNORECASE
    multiline_flag = re.MULTILINE
    dotall_flag = re.DOTALL
    print(f"IGNORECASE flag: {ignorecase_flag}")
    print(f"MULTILINE flag: {multiline_flag}")
    print("Regex functions loaded successfully")

    # Test datetime module
    print("\nTesting datetime module...")
    minyear = datetime.MINYEAR
    maxyear = datetime.MAXYEAR
    print(f"MINYEAR: {minyear}")
    print(f"MAXYEAR: {maxyear}")
    # Test type references
    print("Datetime types loaded successfully")

    # Test collections module
    print("\nTesting collections module...")
    print("Collections functions loaded successfully")

    # Test itertools module
    print("\nTesting itertools module...")
    print("Itertools functions loaded successfully")

    # Test functools module
    print("\nTesting functools module...")
    print("Functools functions loaded successfully")

    print("\n=== All standard library modules tested successfully! ===")
    return 0


if __name__ == "__main__":
    main()
