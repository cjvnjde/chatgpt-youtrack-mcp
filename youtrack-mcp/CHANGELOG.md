# Changelog

## Unreleased

- Fixed HTTP `403 Forbidden` responses for Docker service names and
  reverse-proxy domains; RMCP's loopback-only default Host allowlist is
  disabled because bearer authentication and private-network isolation are the
  listener boundaries.
- Added MCP Streamable HTTP transport for shared deployments. Public
  `MCP_HTTP_ADDR` requests require a 32+ byte `MCP_AUTH_TOKEN`; optional
  `MCP_INTERNAL_ADDR` provides a separate unauthenticated listener for a
  private tunnel network. Stdio remains the default.
- Added private `/healthz` and `/readyz` endpoints to the public listener for
  container orchestration.
- Added a complete typed MCP mirror of the connected YouTrack API. At startup,
  every operation in `/api/openapi.json` is registered as an `api_*` tool with
  typed parameters, request body and response schema; JSON, form, multipart,
  text and binary bodies are supported.
- Added `YOUTRACK_OPENAPI_PATH` for offline or pinned OpenAPI schemas.
- `issue_write.assignee` now distinguishes omission from `null`; explicit `null`
  clears the current assignee.
- Added `issue_write.customFields` for changing arbitrary issue custom fields,
  including Type. Field types are discovered automatically; common named values
  have string shorthands, while API-native JSON supports every other type.

## 0.2.0

- Added `attachment_upload`, a dedicated ChatGPT file-input tool that downloads
  the authorized temporary URL and forwards the exact bytes to YouTrack.
- Removed `attachment op=upload` so ChatGPT cannot select the server-local
  `path` flow for a composer upload.

## 0.1.4

Attachments are usable from an agent now. Plus a paging bug that ran through the
whole client.

- **BREAKING** `attachment op=list` returns `{count, truncated, attachments}`,
  was a bare array.
- **BREAKING** the signed `url` is gone from `attachment` output. `verbose: true`
  brings it back.
  It is file access on its own — it does not belong in a transcript.
- `attachment op=get|download|delete` take `name`, not just `attachmentId`.
  Names are not unique. An ambiguous one errors with the candidate ids instead
  of guessing.
- `attachment op=download` hands back an image as an image. Non-image, over 4 MB,
  or an explicit `path` → written to disk (`path` / `YOUTRACK_DOWNLOAD_DIR` /
  temp), path returned.
  It used to emit base64 in a text field: unreadable to the model, ~170k tokens
  for one screenshot.
- `attachment` covers articles: `entity` = `issue` (default) | `article`.
  `issueId` → `parentId`, old name still accepted.
- Nothing stops at 42 any more — attachments, comments, links, boards, link
  types, work-item types.
  YouTrack pages at 42 by default and truncates silently. Boards and link types
  feed name resolution, so a big instance got "not found" for things that exist.
- User lookup by login pages properly.
  It falls back to the first row when no exact login matches, so a cut page
  resolved to a *different user* — a wrong assignee, not a missing one.
- Attachment downloads follow redirects (YouTrack Cloud), dropping the auth token
  when the host changes.
- A collection that comes back exactly full logs `WARN` to stderr. Only
  `attachment op=list` reports truncation in its response.

## 0.1.3

- Attaching an issue to a board/sprint now works on every YouTrack instance.
  Previously it used a localized apply-command and failed on non-English
  instances (e.g. RU: "Неизвестная команда: Board").
- `board` and `sprint` accept either a name or an id, in any language and any
  casing (matched trimmed + Unicode case-insensitive). E.g. `црппо` resolves
  the `ЦРППО` board.
- A wrong/unknown `board` now lists the available boards; a wrong/ambiguous
  `sprint` lists the valid sprints for that board (a board exposes only its
  own sprints, so callers can't know them up front).
- Board/sprint resolution errors are reported as `invalid_params` instead of
  a generic internal error.
