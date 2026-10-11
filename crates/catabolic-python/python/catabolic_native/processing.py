"""Persistent bounded analysis, using the shared native processing owner."""


class Processing:
    def __init__(self, app):
        self.app, self.store, self.profile = app, app.store, app.profile

    def _call(self, action, **request):
        return self.store._call("processing", action, request, self.profile)

    def enqueue(
        self,
        operation,
        *,
        file_ids=None,
        location=None,
        limit=100,
        after="",
        refresh=False,
        options=None,
        recipe_id=None,
    ):
        return self._call(
            "enqueue",
            operation=operation,
            file_ids=file_ids
            if file_ids is None or isinstance(file_ids, list)
            else "invalid",
            location=location,
            limit=limit,
            after=after or "",
            refresh=bool(refresh),
            options=options or {},
            recipe_id=recipe_id,
        )

    def list(self, *, state=None, limit=100, after="", file_id=None):
        return self._call(
            "list", state=state, limit=limit, after=after or "", file_id=file_id
        )

    def get(self, identifier):
        return self._call("get", id=identifier)

    def attempts(self, identifier, *, limit=100, after=0):
        return self._call("attempts", id=identifier, limit=limit, after=after)

    def cancel(self, identifier):
        return self._call("cancel", id=identifier)

    def retry(self, identifier):
        return self._call("retry", id=identifier)

    def run(
        self,
        *,
        workers=2,
        per_device=1,
        limit=100,
        storage_groups=None,
        retry_transient=0,
        retry_delay=30,
        job_ids=None,
        _refresh=True,
    ):
        return self._call(
            "run",
            workers=workers,
            per_device=per_device,
            limit=limit,
            storage_groups=storage_groups or {},
            retry_transient=retry_transient,
            retry_delay=retry_delay,
            job_ids=job_ids
            if job_ids is None or isinstance(job_ids, list)
            else "invalid",
            _refresh=bool(_refresh),
        )

    def facts(self, file_id):
        return self._call("facts", file_id=file_id)

    def duplicates(self, *, limit=100, after=""):
        return self._call("duplicates", limit=limit, after=after or "")

    def search(self, text, *, limit=100, after=0):
        return self._call("search", text=text, limit=limit, after=after)
