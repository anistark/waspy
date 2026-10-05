"""
Every supported standard library module imports, and its constants compile.
The modules' functions are not implemented yet (planned for 0.20.0), so this
example sticks to constants. print() writes nothing until the host interface
lands in 0.19.0, so main() returns a checksum of what it read.

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

    # A float has no runtime str() yet, so an f-string placeholder cannot
    # render one; truncate to an int to print the value.
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
