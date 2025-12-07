"""
Test that all standard library modules can be imported.
This is a basic compilation test to verify module registration.
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

def test_imports():
    """Test that imports work by accessing a simple attribute from sys."""
    maxsize = sys.maxsize
    name = os.name
    return maxsize
