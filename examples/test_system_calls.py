"""
Comprehensive test for system calls (sys and os modules).
Tests all features requested in issue #27.
"""

import sys
import os

def test_sys_module():
    """Test sys module attributes."""
    print("Testing sys module:")
    print("sys.platform:", sys.platform)
    print("sys.version:", sys.version)
    print("sys.maxsize:", sys.maxsize)
    print("sys.argv:", sys.argv)
    print("sys.path:", sys.path)
    return 0

def test_os_module():
    """Test os module basic attributes and functions."""
    print("\nTesting os module:")
    print("os.name:", os.name)
    print("os.sep:", os.sep)
    print("os.pathsep:", os.pathsep)
    print("os.linesep:", os.linesep)
    print("os.devnull:", os.devnull)
    print("os.curdir:", os.curdir)
    print("os.pardir:", os.pardir)
    print("os.extsep:", os.extsep)

    # Test os functions
    print("\nTesting os functions:")
    cwd = os.getcwd()
    print("os.getcwd():", cwd)

    pid = os.getpid()
    print("os.getpid():", pid)

    env_val = os.getenv("HOME")
    print("os.getenv('HOME'):", env_val)

    # Test environ
    print("os.environ:", os.environ)

    return 0

def test_os_path_module():
    """Test os.path module functions."""
    print("\nTesting os.path module:")

    # Test path attributes
    print("os.path.sep:", os.path.sep)
    print("os.path.pathsep:", os.path.pathsep)

    # Test path functions
    joined = os.path.join("/usr", "bin", "python")
    print("os.path.join('/usr', 'bin', 'python'):", joined)

    exists = os.path.exists("/tmp")
    print("os.path.exists('/tmp'):", exists)

    isfile = os.path.isfile("/etc/hosts")
    print("os.path.isfile('/etc/hosts'):", isfile)

    isdir = os.path.isdir("/tmp")
    print("os.path.isdir('/tmp'):", isdir)

    basename = os.path.basename("/usr/bin/python")
    print("os.path.basename('/usr/bin/python'):", basename)

    dirname = os.path.dirname("/usr/bin/python")
    print("os.path.dirname('/usr/bin/python'):", dirname)

    abspath = os.path.abspath("file.txt")
    print("os.path.abspath('file.txt'):", abspath)

    return 0

def test_all():
    """Run all system call tests."""
    test_sys_module()
    test_os_module()
    test_os_path_module()
    print("\nAll system call tests completed!")
    return 0

# Run the tests
test_all()
