"""os's path constants and its name. The functions that ask the host
(getcwd, getenv, environ, getpid, urandom) and os.path are tested in
tests/unit/stdlib.rs."""

import os


def separators() -> str:
    return os.sep + os.pathsep + os.extsep


def os_name() -> str:
    return os.name


def parent_dir() -> str:
    return os.pardir + os.sep + os.curdir


def path_sep_matches() -> bool:
    return os.path.sep == os.sep and os.path.curdir == os.curdir


def devnull_length() -> int:
    return len(os.devnull)
