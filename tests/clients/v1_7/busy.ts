// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

export type BusyAttempt = { method: string; path: string; status: 503; attempt: number; retried: boolean };
export function replaySafe(request: Request): boolean {
  const { pathname, searchParams } = new URL(request.url);
  // Content GETs can debit tickets. Never infer mutation safety from status503.
  if (pathname.includes("%") || pathname.includes("/content") || pathname.includes("/segments/") || searchParams.has("ticket")) return false;
  if (["GET", "HEAD"].includes(request.method)) return true;
  if (request.method !== "POST") return false;
  if (["/v1/query/graphql", "/v1/query/sql"].includes(pathname) || /^\/v1\/queries\/[^/]+\/runs$/.test(pathname)) return true;
  return pathname === "/v1/rendition-requests" && Boolean(request.headers.get("Idempotency-Key")?.trim());
}
export function busyFetch(
  record: (event: BusyAttempt) => void,
  fetcher: typeof fetch = fetch,
  pause = (ms: number) => new Promise<void>(resolve => setTimeout(resolve, ms)),
): typeof fetch {
  return async (input, init) => {
    const original = new Request(input, init);
    const deadline = Date.now() + 10_000;
    let attempt = 0;
    for (;;) {
      attempt += 1;
      const response = await fetcher(original.clone());
      if (response.status !== 503) return response;
      const problem = await response.clone().json().catch(() => undefined);
      if (problem?.code !== "catalog_busy" || problem?.retryable !== true) return response;
      const retried = replaySafe(original) && Date.now() < deadline;
      record({ method: original.method, path: new URL(original.url).pathname, status: 503, attempt, retried });
      if (!retried) return response;
      await response.body?.cancel();
      const seconds = Number(response.headers.get("Retry-After") ?? 1);
      await pause(Number.isFinite(seconds) ? Math.min(1000, Math.max(10, seconds * 1000)) : 1000);
    }
  };
}
