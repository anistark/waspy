"""logging's level constants. Loggers, handlers, and formatters are tested
in tests/unit/stdlib.rs."""

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
