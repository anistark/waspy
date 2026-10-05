"""
Every supported standard library module imports, and the constants it
defines read back as CPython's values (sys.maxsize under waspy's 32-bit int).
The modules' functions are not implemented yet (planned for 0.20.0).
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


def test_sys_module() -> int:
    return sys.maxsize


def test_os_module() -> str:
    return os.name + os.sep + os.pathsep + os.curdir + os.pardir + os.extsep


def test_math_module() -> float:
    return math.pi + math.e + math.tau


def test_re_module() -> int:
    return re.IGNORECASE | re.MULTILINE | re.DOTALL | re.VERBOSE | re.ASCII


def test_datetime_module() -> int:
    return datetime.MAXYEAR - datetime.MINYEAR


def test_all_modules() -> int:
    return test_re_module() + test_datetime_module()
