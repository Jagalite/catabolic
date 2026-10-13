"""Source event hints invalidate evidence through the existing native owner."""


def invalidate(app, source, *, reason="event"):
    result = app.store._call("watchers", "invalidate", {"source": source}, app.profile)
    if "ignored" not in result:
        result["reason"] = reason
    return result
