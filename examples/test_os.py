import os

def test_os():
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
