# Test data

`openapi-3.1-schema-2026-08-03.json` is the JSON Schema of OpenAPI 3.1
documents, iteration 2026-08-03, byte for byte as published at
<https://spec.openapis.org/oas/3.1/schema/2026-08-03> (from
<https://github.com/OAI/spec.openapis.org>, `oas/3.1/schema/2026-08-03`).
It is the OpenAPI Initiative's work, under the Apache License 2.0, the same
license as OpenXet. `tests/openapi.rs` validates `openxet openapi` against
it. To move to a newer iteration, add its file and point the test at it.
