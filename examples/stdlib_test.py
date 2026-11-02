"""
Every supported standard library module imports, and its constants compile.
This example sticks to the constants (the modules' functions have their own
examples and tests), printing each and returning a checksum of what it read.

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
    maxsize = sys.maxsize
    print(f"Max size: {maxsize}")

    separator = os.sep
    path_sep = os.pathsep
    print(f"OS name: {os.name}")
    print(f"Separator: {separator}")
    print(f"Path separator: {path_sep}")

    # Truncated to ints, as main()'s checksum adds them.
    print(f"Pi (truncated): {int(math.pi)}")
    print(f"E (truncated): {int(math.e)}")
    print(f"Tau (truncated): {int(math.tau)}")

    ignorecase_flag = re.IGNORECASE
    multiline_flag = re.MULTILINE
    print(f"IGNORECASE flag: {ignorecase_flag}")
    print(f"MULTILINE flag: {multiline_flag}")

    print(f"MINYEAR: {datetime.MINYEAR}")
    print(f"MAXYEAR: {datetime.MAXYEAR}")

    return (
        int(math.pi)
        + int(math.e)
        + int(math.tau)
        + ignorecase_flag
        + multiline_flag
        + datetime.MAXYEAR
        + len(separator + path_sep)
    )
