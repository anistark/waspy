

# ---------------------------------------------------------------------------
# Runtime checks, appended to examples/text_report.py by
# examples/verify_runtime.rs
# ---------------------------------------------------------------------------
#
# See shopping_cart_checks.py for why these are exported functions rather than
# host-side assertions. The expected report is what CPython prints for the same
# source.


def runtime_check_report() -> int:
    expected = "23 words, 14 unique\nlongest=quick avg=3.43\nfox: 4\nthe: 4\na: 2"
    if main() == expected:
        return 1
    return 0


def runtime_check_word_count() -> int:
    if word_count(SAMPLE) == 23:
        return 1
    return 0


def runtime_check_unique_count() -> int:
    if unique_count(SAMPLE) == 14:
        return 1
    return 0


def runtime_check_longest_word() -> int:
    if longest_word(tokenize(SAMPLE)) == "quick":
        return 1
    return 0


def runtime_check_average_length() -> int:
    avg = average_length(tokenize(SAMPLE))
    diff = avg - 3.4347826086956523
    if diff < 0.0:
        diff = 0.0 - diff
    if diff < 0.000000001:
        return 1
    return 0


def runtime_check_negative_report() -> int:
    if main() == "23 words, 14 unique":
        return 1
    return 0
