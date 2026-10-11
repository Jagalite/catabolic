"""Persistent generated artifacts, owned by the native Store and runtime."""


class Artifacts:
    def __init__(self, app):
        self.app, self.store, self.profile = app, app.store, app.profile
        self._verification_claim = None

    def _call(self, operation, **request):
        return self.store._call("artifacts", operation, request, self.profile)

    def bind(self, location, root):
        self._call("bind", location=location, root=str(root))
        return self.app.binding("source", location)

    def recipe(self, recipe_name, preset, options=None, output_definition_id=None):
        return self._call(
            "recipe",
            recipe_name=recipe_name,
            preset=preset,
            options=options or {},
            output_definition_id=output_definition_id,
        )

    def default_output(self, preset):
        return self._call("default_output", preset=preset)

    def get_recipe(self, identifier):
        return self._call("get_recipe", id=identifier)

    def recipes(self, limit=100, after=""):
        return self._call("recipes", limit=limit, after=after)

    def list(self, *, limit=100, after="", state=None, file_id=None):
        return self._call(
            "list", limit=limit, after=after, state=state, file_id=file_id
        )

    def get(self, identifier):
        return self._call("get", id=identifier)

    def enqueue(
        self,
        file_id,
        recipe_id,
        location,
        item_id,
        *,
        _capabilities=None,
        _full_cache_check=True,
        _source_lineage_mode=None,
        _component_package=None,
    ):
        return self._call(
            "enqueue",
            file_id=file_id,
            recipe_id=recipe_id,
            location=location,
            item_id=item_id,
            _capabilities=_capabilities,
            _full_cache_check=bool(_full_cache_check),
            _source_lineage_mode=_source_lineage_mode,
            _component_package=_component_package,
        )

    def recover(self, *, limit=100):
        return self._call("recover", limit=limit)

    def run(self, *, limit=100, job_ids=None, _refresh=True):
        return self._call(
            "run",
            limit=limit,
            job_ids=job_ids
            if job_ids is None or isinstance(job_ids, list)
            else "invalid",
            _refresh=bool(_refresh),
        )
