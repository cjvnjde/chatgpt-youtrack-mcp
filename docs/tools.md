# MCP tool reference

[Documentation home](../README.md#documentation)

The server registers 17 curated tools, `api_schema`, and one generated `api_*`
tool for each operation in the instance's OpenAPI document. The total is therefore
18 plus the number of generated operations. MCP discovery is the authoritative
input schema for your running version.

## Tool summary

| Tool | Purpose |
|---|---|
| `issue_write` | Create, update, or delete an issue. |
| `issue_get` | Read one issue with full fields. |
| `issue_search` | Search using YouTrack query syntax. |
| `issue_links` | Read an issue's relationships. |
| `link_write` | Add or remove a non-parent relationship. |
| `comment_write` | Create or update an issue/article comment. |
| `comments_list` | List issue/article comments. |
| `article_write` | Create or update a knowledge-base article. |
| `article_get` | Read one article or list articles. |
| `workitem_write` | Create, update, or delete a time entry. |
| `workitems_list` | List work entries by author, period, or issue. |
| `workitems_report` | Compare daily expected and recorded time. |
| `activity` | Read issue or user activity. |
| `users` | List users, read one, or identify the token owner. |
| `meta` | Discover projects, link types, and work-item types. |
| `attachment_upload` | Upload an authorized file unchanged. |
| `attachment` | List, inspect, download, or delete attachments. |
| `api_schema` | Retrieve a generated operation's full output schema. |
| `api_*` | Call an operation from the connected YouTrack API. |

## Shared conventions

Examples below are tool argument objects, not complete JSON-RPC requests. Names
and IDs are illustrative; select real projects, fields, users, and types from your
instance. Optional inputs can be omitted unless an operation requires them.

- Use readable issue IDs such as `ABC-123`. Curated issue helpers support expansion
  through `YOUTRACK_DEFAULT_PROJECT`.
- Dates use `YYYY-MM-DD`; `activity` also accepts Unix milliseconds as strings.
- `entity` is `issue` or `article`. It is required for comment tools and defaults
  to `issue` for attachment tools.
- Pagination uses `top` and `skip` where exposed. Do not assume a single page is
  the complete result set.
- Curated tools generally return JSON encoded as MCP text with `$type` metadata
  removed. Attachment downloads can also return image content.
- Writes execute with the token owner's permissions. Delete operations have no
  built-in confirmation step and may be irreversible.

## Issues and links

### `issue_write`

Required: `op` (`create`, `update`, `delete`). Create requires `project`; update
and delete require `id`. For creation, supply a meaningful `summary`.

Optional write fields: `summary`, `description`, `markdown`, `parentId`,
`assignee`, `tags` (string array), `state`, `customFields` (array of `{name,value}`),
`board`, and `sprint`.

```json
{
  "op": "create",
  "project": "ABC",
  "summary": "Document the deployment",
  "description": "Include the tunnel and direct-client setup.",
  "parentId": "ABC-100",
  "customFields": [{"name": "Priority", "value": "Normal"}]
}
```

For custom fields, strings and arrays of strings select named values; `null`
clears a single value and `[]` clears a multi-value field. API-native JSON values
are accepted for other field types. Types are discovered from YouTrack. There is
no top-level `type` input; use `customFields` for an issue's Type field.

Omit `assignee` to preserve it, pass a login to assign it, or `null` to clear it.
Set `parentId` for native subtasks and use `""` to clear the parent. Tags must
already exist; an empty `tags` array is ignored, so it does not clear tags.
`board` and `sprint` resolve by name or ID; sprint selection is board-specific.

```json
{"op":"update","id":"ABC-123","assignee":null,"parentId":""}
```

Create/update return issue data; delete returns a deletion result. Multi-request
writes can partially succeed; read the issue before retrying after a failure.

### `issue_get`, `issue_search`, and `issue_links`

| Tool | Required inputs | Optional inputs and defaults |
|---|---|---|
| `issue_get` | `id` | None. Returns one issue with full fields. |
| `issue_search` | `query` | `fields`: `short` (default) or `full`; `top`: 50; `skip`: 0. Returns matching issues. |
| `issue_links` | `id` | None. Returns links with type, direction, and related issues. |

```json
{"query":"project: ABC #Unresolved","fields":"short","top":20,"skip":0}
```

### `link_write`

Required: `op` (`add` or `remove`), `sourceId`, `targetId`, and `linkType`.
Optional `role` is `outward` (default) or `inward` for directed types. Discover
valid types with `meta` using `kind: "link_types"`.

```json
{"op":"add","sourceId":"ABC-123","targetId":"ABC-124","linkType":"Relates"}
```

Use `issue_write.parentId` for parent/child hierarchy.

## Comments and articles

### `comment_write` and `comments_list`

`comment_write` requires `entity`, `op` (`create` or `update`), `parentId`, and
`text`. Update also requires `commentId`. Optional: `markdown` and `mute`
(default `false`). Returns the created or updated comment.

```json
{"entity":"issue","op":"create","parentId":"ABC-123","text":"Deployment documentation is ready."}
```

`comments_list` requires `entity` and `parentId` and returns comments. It does not
expose pagination arguments; the current implementation requests up to 500.

### `article_write` and `article_get`

`article_write` requires `op` (`create` or `update`). Create requires `project`;
update requires `id`. Optional content fields: `summary`, `content`,
`parentArticleId`, and `markdown`. Returns article data.

```json
{"op":"create","project":"ABC","summary":"Deployment guide","content":"Start with the Compose stack."}
```

`article_get` requires `op`: `get` also requires `id`, while `list` accepts an
optional `query`. Returns one article or the article list. No pagination inputs
are exposed by this curated tool; use generated API tools for additional control.

## Time tracking and activity

### `workitem_write`

Required: `op` (`create`, `update`, `delete`) and `issueId`. Create also requires
`date` and integer `minutes`; update/delete require `workItemId`.
Optional: `text`, `description`, `type` (work-item type name or ID), `markdown`,
and `idempotent` (default `false`, used on create). Update can supply `date` and
`minutes` as needed.

```json
{
  "op": "create",
  "issueId": "ABC-123",
  "date": "2026-09-04",
  "minutes": 60,
  "description": "Document deployment",
  "idempotent": true
}
```

Duplicate detection compares issue, date, and description; it does not include
duration or type and is not an atomic concurrency guarantee. Use
`workitems_list` to obtain IDs before editing or deleting an entry.

### `workitems_list`

All inputs are optional: `author` (defaults to current user), `startDate`,
`endDate`, `issueId`, `top` (200), and `skip` (0). Returns work-item records.

```json
{"startDate":"2026-09-01","endDate":"2026-09-04","top":200,"skip":0}
```

### `workitems_report`

Required: `startDate` and `endDate`. Optional: `author` (current user by default).
The end date must not precede the start date. Returns:

- `summary`: `totalMinutes`, `totalHours`, `expectedMinutes`, `workDays`, and `avgHoursPerDay`;
- `period`: `startDate` and `endDate`;
- `days`: working-day rows with `date`, `expected`, `actual`, `diff`, and `percent`;
- `invalidDays`: rows whose actual time differs from expected time.

Expected time is 480 minutes per weekday or 420 on configured pre-holidays.
Weekends and configured holidays are excluded from daily rows. Summary actual
time includes all fetched entries, including those on excluded days. The report
fetches at most 1,000 entries; use shorter periods for larger histories. Date
bucketing uses [the configured timezone](configuration.md#optional-youtrack-settings).

### `activity`

Required: `scope` (`issue` or `user`). Issue scope requires `issueId` and accepts
an optional `author`; user scope requires `author` and defaults to the last 30
days. Optional inputs: `startDate`, `endDate`, `categories` (string array),
`reverse` (user scope: `true` means oldest first), `top` (100), and `skip` (0).

Default categories are `CustomFieldCategory` and `CommentsCategory`. Other
examples include `AttachmentsCategory`, `LinksCategory`,
`WorkItemsActivityCategory`, `VcsChangeActivityCategory`, `TagsCategory`, and
`SprintCategory`. Returns the activity feed.

```json
{"scope":"issue","issueId":"ABC-123","top":50}
```

## Discovery

### `users`

Required `op`: `me`, `list`, or `get`. `list` accepts optional `query`; `get`
requires `id`. `me` identifies the YouTrack token owner. Returns user data or a
user list.

### `meta`

Required `kind`: `projects`, `link_types`, or `work_item_types`. Optional
`project` scopes work-item type discovery. Returns the corresponding metadata.

```json
{"kind":"work_item_types","project":"ABC"}
```

## Attachments

### `attachment_upload`

Required: `parentId` and `file`. Optional: `entity` (default `issue`), `name`
(filename override), and `verbose` (default `false`). `issueId` is accepted as
an alias for `parentId`.

The required `file` object contains `download_url` and `file_id`; optional fields
are `mime_type` and `file_name`. The tool advertises
`_meta["openai/fileParams"]: ["file"]` so a compatible ChatGPT host can resolve
a user file reference to an authorized temporary download URL. A filename must
come from `name` or `file.file_name`.

Pass the user's file through that mechanism, not as a `/mnt/data` path or base64
string. The server transfers the downloaded bytes unchanged and returns upload
metadata. Direct clients need to supply a compatible file object with a URL
reachable from the MCP server; local filesystem references are insufficient.

### `attachment`

Required: `op` (`list`, `get`, `download`, `delete`) and `parentId` (alias:
`issueId`). Optional: `entity` (default `issue`), `attachmentId`, `name`, `path`,
`top` (500, minimum 1), and `verbose` (default `false`).

`get`, `download`, and `delete` need an attachment ID or name. An ambiguous name
returns candidate IDs; retry with the exact ID. `top` also bounds name resolution.

```json
{"op":"list","parentId":"ABC-123"}
```

List/get return metadata, omitting signed URLs unless `verbose: true`. Download
returns images up to 4 MiB inline as image content when `path` is absent. Other
downloads return `{saved, bytes, mimeType}` after writing to the MCP filesystem.
Explicit target paths require an existing parent directory; otherwise the server
uses `YOUTRACK_DOWNLOAD_DIR` or its temporary directory. Existing target files can
be overwritten. Remote clients do not automatically gain access to saved paths.

## Generated API tools

Names follow `api_<method>_<path>`, such as `api_post_issues` and
`api_get_admin_projects`; path placeholders become name segments. Discover exact
names and required arguments from your running server instead of guessing.

Path, query, header, and cookie parameters are top-level inputs. Request payloads
belong under `body`. Supported body formats include JSON, form, multipart, text,
and base64-encoded binary data. Follow the generated schema for each operation.
Generated tools include administrative and irreversible operations, constrained
by the YouTrack token's permissions.

Successful JSON responses are returned directly as structured content. Empty
responses return `{status}`; text responses return `{status, contentType, body}`;
binary responses return `{status, contentType, base64}`. Non-success HTTP
responses return a structured MCP error with `{status, contentType, body}`.

### `api_schema`

Required: `name`, the exact name of a generated operation.

```json
{"name":"api_post_issues"}
```

Returns `{tool, outputSchema}` as JSON text. This retrieves an output contract;
it does not call the operation. Curated tool names and unknown generated names
are rejected. Input schemas remain available directly in `tools/list`.

## Errors

Curated validation errors identify missing or invalid arguments. A YouTrack `404`
maps to an MCP resource-not-found error; other API failures are reported as
`YouTrack <status>: <message>`. Network/configuration failures become internal
errors. Generated HTTP failures use the structured error format above; generated
request-building or network failures can return an MCP error with text instead.

HTTP `401` from the public MCP listener means MCP bearer authentication failed.
A YouTrack `401` or `403` reported by a tool concerns the upstream token or its
permissions. See [troubleshooting](deployment.md#troubleshooting).
