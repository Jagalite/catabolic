// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import createClient from "openapi-fetch";
import type { paths } from "./generated/rest.js";
import type { FallbackQuery, FallbackQueryVariables } from "./generated/graphql.js";
const fixture = JSON.parse(readFileSync(0, "utf8"));
const client = createClient<paths>({baseUrl: fixture.base, headers: {Authorization: `Bearer ${fixture.token}`}});
const resolved = await client.POST("/v1/items/{identifier}/resolve", {
  params: {path: {identifier: fixture.item}}, body: {fallback_policy_id: fixture.policy},
});
assert.ok(resolved.data && "selected_tier" in resolved.data, JSON.stringify(resolved.error));
assert.equal(resolved.data.file_id, fixture.files.A);
assert.equal(resolved.data.selected_tier, 0);
const page = await client.GET("/v1/projections/{identifier}/resolutions", {params: {path: {identifier: "global"}}});
assert.equal(page.data?.data[0]?.file_id, fixture.files.A);
const variables: FallbackQueryVariables = {policy: fixture.policy, catalog: "global"};
const result = await client.POST("/v1/query/graphql", {body: {query: readFileSync(new URL("../fallback.graphql", import.meta.url), "utf8"), variables}});
assert.equal(result.data?.errors, undefined);
const document = result.data?.data as FallbackQuery;
assert.equal(document.fallbackPolicy?.id, fixture.policy);
assert.equal(document.fallbackResolutions?.nodes[0]?.fileId, fixture.files.A);
console.log("Generated fallback REST and GraphQL client acceptance passed.");
