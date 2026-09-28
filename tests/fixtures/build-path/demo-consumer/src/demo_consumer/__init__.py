"""Fixture consumer: imports its internal dependency demo-alpha."""

from demo_alpha import alpha_value


def consumer_value() -> int:
    return alpha_value() + 1
