"""Native projection identity on the caller's persistent Store."""


def projection_digest(app, catalog):
    return app.store._call(
        "layouts", "projection_digest", dict(catalog=catalog), app.profile
    )
