"""
Test all standard library modules - comprehensive compilation test.
Tests that all modules can be imported and their attributes accessed.
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

def test_sys_module():
    """Test sys module attributes."""
    platform = sys.platform
    version = sys.version
    maxsize = sys.maxsize
    return maxsize

def test_os_module():
    """Test os module attributes."""
    name = os.name
    sep = os.sep
    pathsep = os.pathsep
    linesep = os.linesep
    devnull = os.devnull
    curdir = os.curdir
    pardir = os.pardir
    extsep = os.extsep
    return name

def test_math_module():
    """Test math module constants."""
    pi = math.pi
    e = math.e
    tau = math.tau
    inf = math.inf
    nan = math.nan
    return pi

def test_re_module():
    """Test re module flags."""
    i_flag = re.IGNORECASE
    m_flag = re.MULTILINE
    s_flag = re.DOTALL
    x_flag = re.VERBOSE
    a_flag = re.ASCII
    return i_flag

def test_datetime_module():
    """Test datetime module constants."""
    minyear = datetime.MINYEAR
    maxyear = datetime.MAXYEAR
    return maxyear

def test_all_modules():
    """Test that all modules are accessible."""
    sys_result = test_sys_module()
    os_result = test_os_module()
    math_result = test_math_module()
    re_result = test_re_module()
    dt_result = test_datetime_module()

    return sys_result + re_result + dt_result
