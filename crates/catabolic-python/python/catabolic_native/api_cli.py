"""Local credential output through the caller's native Store owner."""


def save_secret(store, filename, result):
    return store._owner.save_secret(filename, result)
