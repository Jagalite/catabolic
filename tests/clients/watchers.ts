// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import createClient from "openapi-fetch";
import type { paths } from "./generated/rest.js";
const fixture = JSON.parse(readFileSync(0, "utf8"));
const client = createClient<paths>({baseUrl: fixture.base, headers: {Authorization: `Bearer ${fixture.token}`}});
const created = await client.PUT("/v1/operator/watchers/{name}", {
  params: {path: {name: "generated-audit"}},
  body: {plan: {kind: "query", query_id: fixture.query}},
});
assert.ok(created.data, JSON.stringify(created.error));
assert.equal(created.data.enabled, false);
const admitted = await client.POST("/v1/operator/watchers/{name}/runs", {params: {path: {name: "generated-audit"}}});
assert.equal(admitted.response.status, 202);
assert.equal(admitted.data?.admitted, true);
const history = await client.GET("/v1/operator/watchers/{name}/history", {params: {path: {name: "generated-audit"}}});
assert.deepEqual(history.data?.runs, []);
console.log("Generated watcher client acceptance passed.");
