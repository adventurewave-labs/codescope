"""Helpers: the math_hash that pipeline.py actually imports."""


def math_hash(values):
    acc = 17
    for v in values:
        acc = (acc * 31 + hash(str(v))) & 0xFFFFFFFF
    return acc
