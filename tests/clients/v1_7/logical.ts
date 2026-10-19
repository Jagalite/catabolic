// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import createClient from "openapi-fetch";
import type { paths } from "./generated/rest.js";
import type { LogicalPolicyQuery, LogicalPolicyQueryVariables } from "./generated/graphql.js";
const fixture = JSON.parse(readFileSync(0, "utf8"));
const client = createClient<paths>({baseUrl:fixture.base,headers:{Authorization:`Bearer ${fixture.token}`}});
const resolved = await client.POST("/v1/items/{identifier}/resolve", {
  params:{path:{identifier:fixture.item}},body:{fallback_policy_id:fixture.policy},
});
assert.equal(resolved.response.status,200,JSON.stringify(resolved.error));
assert.ok(resolved.data && "selected_tier" in resolved.data);
assert.equal(resolved.data.file_id,fixture.file);
assert.equal(resolved.data.selected_tier,1);
assert.equal(resolved.data.revision,fixture.revision);
const policy = await client.GET("/v1/fallback-policies/{identifier}", {params:{path:{identifier:fixture.policy}}});
assert.equal(policy.data?.id,fixture.policy);
const variables: LogicalPolicyQueryVariables = {id:fixture.policy};
const result = await client.POST("/v1/query/graphql", {body:{query:readFileSync(new URL("../logical.graphql",import.meta.url),"utf8"),variables}});
assert.equal(result.data?.errors,undefined);
const document = result.data?.data as LogicalPolicyQuery | undefined;
assert.equal(document?.fallbackPolicy?.id,fixture.policy);
const bytes = await client.GET("/v1/files/{identifier}/content", {params:{path:{identifier:fixture.file},query:{revision:fixture.revision}},parseAs:"arrayBuffer"});
assert.equal(bytes.response.status,200);
assert.equal(Buffer.from(bytes.data!).toString("hex"),fixture.payload);
const mixed = await client.POST("/v1/items/{identifier}/resolve", {params:{path:{identifier:fixture.item}},body:{fallback_policy_id:fixture.policy,file_id:fixture.file}});
assert.equal(mixed.response.status,422);
console.log("Generated native logical REST/GraphQL resolution and authorized exact bytes passed.");
