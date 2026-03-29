# Braze API Reference

Reference for the Braze REST API endpoints used by braze-sync.

Base URL: `https://rest.{instance}.braze.com`
Auth: `Authorization: Bearer {API_KEY}` header on all requests.

## Catalogs API

| Operation | Method | Path | Notes |
|:----------|:-------|:-----|:------|
| List catalogs | `GET` | `/catalogs` | Returns ALL catalogs with `fields[]`. No pagination. |
| Create catalog | `POST` | `/catalogs` | Body: `{ catalogs: [{ name, description, fields: [{ name, type }] }] }`. One per request. |
| Create fields | `POST` | `/catalogs/{catalog_name}/fields` | Body: `{ fields: [{ name, type }] }`. Max 50 fields/req, 500 fields/catalog. Returns 202. |
| Delete field | `DELETE` | `/catalogs/{catalog_name}/fields/{field_name}` | Returns 202. |

Rate limit: **50 req/min** (shared across all catalog sync endpoints)

Important:
- `GET /catalogs` returns fields for ALL catalogs in one response — no need for per-catalog fetch
- Field create/delete are **async** (return 202)
- No "update field" endpoint — to change a field type, must delete and recreate
- Catalog `id` field is auto-created and cannot be modified

## Content Blocks API

| Operation | Method | Path | Notes |
|:----------|:-------|:-----|:------|
| List | `GET` | `/content_blocks/list` | Query: `limit` (max 1000), `offset`, `modified_after`, `modified_before`. Returns `content_blocks[]` with `content_block_id`, `name`. |
| Get info | `GET` | `/content_blocks/info` | Query: `content_block_id` (required). Returns full `content`, `description`, `tags[]`, `state`. |
| Create | `POST` | `/content_blocks/create` | Body: `{ name, content, description, state, tags }`. Returns `content_block_id`. |
| Update | `POST` | `/content_blocks/update` | Body: `{ content_block_id, name, content, description, state, tags }`. |

Rate limit: **250,000 req/hr**

Important:
- **No DELETE endpoint** — content blocks cannot be deleted via API
- List returns minimal info — must call `/info` for each block to get `content`
- `state`: "active" or "draft"
- Tags must already exist in the workspace
- Both create and update use `POST` (not PUT/PATCH)
- Identification: list returns `content_block_id`, which is needed for info/update. Match by `name` for Git-to-Braze mapping.

## Custom Attributes API

| Operation | Method | Path | Notes |
|:----------|:-------|:-----|:------|
| Export | `GET` | `/custom_attributes` | Returns 50 per page. Cursor-based pagination via `Link` header. |

Rate limit: **1,000 req/hr** (shared with `/events`, `/events/list`, `/purchases/product_list`)

Important:
- **Read-only** — no create/update/delete API. Attributes are implicitly created when first set via `/users/track`
- For braze-sync: diff-only (detect drift between definition file and live state). Cannot apply.

## Email Templates API

| Operation | Method | Path | Notes |
|:----------|:-------|:-----|:------|
| List | `GET` | `/templates/email/list` | Query: `limit` (max 1000), `offset`. Returns `templates[]` with `email_template_id`, `template_name`. |
| Get info | `GET` | `/templates/email/info` | Query: `email_template_id`. Returns `body`, `subject`, `preheader`, `tags[]`, etc. |
| Create | `POST` | `/templates/email/create` | Body: `{ template_name, subject, body, plaintext_body, preheader, tags, should_inline_css }`. |
| Update | `POST` | `/templates/email/update` | Body: `{ email_template_id, template_name, subject, body, ... }`. |

Rate limit: **250,000 req/hr**

Important:
- **No DELETE endpoint**
- Drag-and-drop editor templates are NOT accessible via API
- Match by `template_name` for Git-to-Braze mapping (list returns `email_template_id`)
