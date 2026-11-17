"""Decoy: same function name as helpers.math_hash, deliberately NOT imported."""


def math_hash(values):
    return sum(hash(str(v)) for v in values)
