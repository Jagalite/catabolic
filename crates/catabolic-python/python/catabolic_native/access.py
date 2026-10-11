"""Public access error type; native administration adapters remain unavailable."""

from .store import CatabolicError


class AccessError(CatabolicError):
    def __init__(self, code="forbidden", status=403):
        self.code, self.status = code, status
        super().__init__(code)
