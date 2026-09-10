// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
import createClient from "openapi-fetch";
import type { paths, components, operations } from "./generated/rest.js";
import type { CatalogQueryVariables } from "./generated/graphql.js";
const client = createClient<paths>();
// These must remain compiler errors; unused directives fail compilation.
// @ts-expect-error revision is required for exact-byte access
void client.GET("/v1/files/{identifier}/content", { params: { path: { identifier: "file" } } });
// @ts-expect-error page limits are numbers
void client.GET("/v1/items", { params: { query: { limit: "100" } } });
// @ts-expect-error processing cannot accept arbitrary encoder arguments
const demand: components["schemas"]["Demand"] = { item_id: "i", source_file_id: "f", source_revision: "r", operation_id: "o", ffmpeg_args: "anything" };
// @ts-expect-error unknown operation IDs cannot enter a client
 type Removed = operations["items_v1_items_get"];
// @ts-expect-error GraphQL first must be numeric
const variables: CatalogQueryVariables = { first: "1" };
void demand; void variables;
