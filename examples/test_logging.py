"""logging's level constants. The logging functions and classes are not
implemented yet (planned for 0.20.0), so calling one is a compile error."""

import logging


def warning_level() -> int:
    return logging.WARNING


def level_sum() -> int:
    return (
        logging.NOTSET
        + logging.DEBUG
        + logging.INFO
        + logging.WARNING
        + logging.ERROR
        + logging.CRITICAL
    )


def aliases_match() -> bool:
    return logging.WARN == logging.WARNING and logging.FATAL == logging.CRITICAL
