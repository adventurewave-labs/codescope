"""Python side: real beta + render. Same names appear in the TS fixtures."""


def beta(x):
    return gamma(x) * 2 # @eval gamma=gamma@beta.py


def gamma(x):
    return x + 1


def run_py(seed):
    return beta(seed) # @eval beta=beta@beta.py
