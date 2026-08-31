# youtrack-mcp

Lean [Model Context Protocol](https://modelcontextprotocol.io) server for [YouTrack](https://www.jetbrains.com/youtrack/), written in Rust. Single static binary, no runtime.

## Why

A complete, typed YouTrack API surface with focused high-level workflows:

- **Full API parity** — every operation in the connected YouTrack instance's
  official OpenAPI schema becomes a typed MCP tool, including administration,
  agile, knowledge-base, saved-query and hub endpoints.
- **Focused workflows included** — 17 curated tools handle common multi-request
  tasks such as parent assignment, board/sprint resolution, reporting and safe
  attachment transfer.
- **Correct subtask hierarchy** — parent set reliably via the command API; the
  real parent issue is surfaced (not an opaque link id).
- **Complete issue write surface** — assignee, arbitrary issue custom fields,
  tags, agile board/sprint, work-item type for time tracking, attachments,
  article + comment create/edit, issue delete.
- **Safe curated defaults** — curated tools never create tags, and secrets never
  leak into error messages. Generated `api_*` tools intentionally expose the
  underlying API without those workflow guardrails.

## Tools

The server keeps 17 curated workflow tools and adds one `api_*` tool for every
operation advertised by the connected YouTrack instance. The exact total
therefore follows the server version and installed features.
Generated input schemas are included directly in MCP discovery. Use
`api_schema` with a generated tool name when its full OpenAPI output schema is
needed.

### Curated workflow tools

| Tool | What it does |
|---|---|
| `issue_write` | Create, update or delete an issue: summary, description, `parentId` (native subtask), assignee (`null` clears), arbitrary `customFields`, tags (must exist), state, board/sprint. `op=delete` is irreversible. |
| `issue_get` | Get a single issue by id with full fields. |
| `issue_search` | Search issues by YouTrack query. `fields` short \| full. |
| `issue_links` | List links of an issue (direction, type, related issues). |
| `link_write` | Add or remove an issue link. `role` outward \| inward for directed types. For parent/child use `issue_write.parentId`. |
| `comment_write` | Create or update a comment on an issue or article (entity). `op=update` needs `commentId`. |
| `comments_list` | List comments of an issue or article (entity). |
| `article_write` | Create or update a knowledge-base article. |
| `article_get` | Get an article by id (`op get`) or list articles (`op list`, optional query). |
| `workitem_write` | Create / update / delete a time-tracking work item. `type` = work item type name/id. `idempotent` skips duplicates. |
| `workitems_list` | List / aggregate work items by author and date range. |
| `workitems_report` | Per-day expected-vs-actual worktime report (480m/day, skips weekends/holidays). |
| `activity` | Activity feed. `scope=issue` (needs `issueId`, optional author) \| `user` (needs author, defaults last 30d). Categories default to CustomFieldCategory, CommentsCategory. Dates ISO or unix ms. |
| `users` | Users: `op` list (optional query) \| me \| get (by id). |
| `meta` | Discovery: `kind` projects \| link_types \| work_item_types (optional project). |
| `attachment_upload` | Upload a user-provided ChatGPT file unchanged to an issue or article through a required `file` parameter. ChatGPT supplies an authorized temporary URL; the server forwards the downloaded bytes without image processing. |
| `attachment` | List, inspect, download, or delete issue/article attachments. Target by `attachmentId` or `name`. All uploads use `attachment_upload`. |

### Full typed API mirror

At startup, the server loads `YOUTRACK_URL/api/openapi.json`, YouTrack's
authoritative OpenAPI schema. Every HTTP operation is registered as an MCP tool:

- Names follow `api_<method>_<path>`, for example `api_post_issues` and
  `api_get_admin_projects`. Path placeholders become name segments.
- Path, query, header and cookie parameters are top-level tool arguments.
  Request payloads use the `body` argument.
- Input JSON Schemas preserve OpenAPI objects, required fields, arrays, enums,
  nullable fields, descriptions and referenced models.
  Output schemas preserve the same information and are returned per operation
  by `api_schema`; they are not repeated in `tools/list`. This keeps the tool
  catalog below infrastructure limits such as Cosmos DB's 2 MiB item limit
  without removing any API operation.
- JSON, form, multipart, text and base64-encoded binary request bodies are
  supported. JSON, text and binary responses return structured status,
  content-type and body data.
- All API families and HTTP methods are available, including irreversible and
  administrative operations. YouTrack permissions attached to
  `YOUTRACK_TOKEN` remain the authorization boundary.

Set `YOUTRACK_OPENAPI_PATH` to a local OpenAPI JSON file only when startup must
be offline or schema versions must be pinned. Requests still go to
`YOUTRACK_URL`; the pinned schema should match that server.

---

## Install

No Rust, no build step, no binary to manage. The prebuilt server is fetched and run by [mcp-bin](https://github.com/sensiarion/mcp-bin) via `npx`. Only requirement: **Node ≥ 22**. Works on macOS, Linux and Windows. ~2 minutes.

### 1. Get a YouTrack token

In YouTrack: avatar → **Profile** → **Account Security** → **New token…** → scope **YouTrack** → copy the `perm-…` string. Treat it like a password.

### 2. Register the MCP

Two env vars are required: `YOUTRACK_URL` (e.g. `https://youtrack.example.com`) and `YOUTRACK_TOKEN` (the `perm-…` token).

**Claude Code** — one command:

```sh
claude mcp add youtrack \
  --env YOUTRACK_URL=https://youtrack.example.com \
  --env YOUTRACK_TOKEN=perm-xxxx \
  -- npx -y mcp-bin sensiarion/youtrack-mcp
```

**Any client via JSON** — Claude Code `~/.claude.json` (global) or project `.mcp.json`; Cursor `~/.cursor/mcp.json` (global) or `.cursor/mcp.json` (project):

```json
{
  "mcpServers": {
    "youtrack": {
      "command": "npx",
      "args": ["-y", "mcp-bin", "sensiarion/youtrack-mcp"],
      "env": {
        "YOUTRACK_URL": "https://youtrack.example.com",
        "YOUTRACK_TOKEN": "perm-xxxx"
      }
    }
  }
}
```

Pin a version by replacing `sensiarion/youtrack-mcp` with `sensiarion/youtrack-mcp@v0.1.2`. Restart the client; the `youtrack` tools appear.

### Update / uninstall

mcp-bin caches a release and reuses it forever by default. To move to a newer release run `npx mcp-bin expire sensiarion/youtrack-mcp` (or pin a tag, or add `--ttl 7d` before the repo in `args`). To uninstall, remove the `youtrack` entry from your client config.

### Serve one instance to remote agents

The same binary can serve MCP Streamable HTTP instead of stdio:

```env
MCP_TRANSPORT=http
MCP_HTTP_ADDR=0.0.0.0:8080
MCP_INTERNAL_ADDR=0.0.0.0:8081
MCP_AUTH_TOKEN=<random 32+ byte secret>
YOUTRACK_URL=https://youtrack.example.com
YOUTRACK_TOKEN=perm-xxxx
```

Generate the bearer secret with `openssl rand -hex 32`. Every request to the
public `MCP_HTTP_ADDR` listener must send
`Authorization: Bearer <MCP_AUTH_TOKEN>`; missing or incorrect credentials
return HTTP `401`. Put that listener behind an HTTPS reverse proxy and expose
only `/mcp`. `/healthz` and `/readyz` are available for private container
health checks.

`MCP_INTERNAL_ADDR` is optional. When set, it starts a second `/mcp` listener
without authentication for a tunnel or sidecar on a private container network.
Never publish or reverse-proxy this internal listener.

Remote MCP client configuration:

```json
{
  "mcpServers": {
    "youtrack": {
      "type": "http",
      "url": "https://youtrack-mcp.example.com/mcp",
      "headers": {
        "Authorization": "Bearer YOUR_MCP_AUTH_TOKEN"
      }
    }
  }
}
```

Stdio remains the default when `MCP_TRANSPORT` is unset.

---

## Environment variables

| Var | Required | Description |
|---|---|---|
| `YOUTRACK_URL` | yes | Base URL, e.g. `https://youtrack.example.com` |
| `YOUTRACK_TOKEN` | yes | Permanent token (Bearer) |
| `MCP_TRANSPORT` | no | `stdio` (default) or authenticated `http` |
| `MCP_HTTP_ADDR` | no | HTTP listen address (default `0.0.0.0:8080`) |
| `MCP_INTERNAL_ADDR` | no | Optional unauthenticated tunnel listener; must remain private |
| `MCP_AUTH_TOKEN` | HTTP only | Bearer secret; at least 32 bytes and no whitespace |
| `YOUTRACK_DEFAULT_PROJECT` | no | Bare numeric ids expand to `PROJ-<n>` |
| `YOUTRACK_TIMEZONE` | no | IANA tz for report bucketing (default `Europe/Moscow`) |
| `YOUTRACK_HOLIDAYS` | no | CSV ISO dates, skipped in `workitems_report` |
| `YOUTRACK_PRE_HOLIDAYS` | no | CSV ISO dates, counted at 7/8 norm |
| `YOUTRACK_USER_ALIASES` | no | CSV `alias:login` pairs |
| `YOUTRACK_DOWNLOAD_DIR` | no | Where non-inlined attachment downloads land (default: system temp dir) |
| `YOUTRACK_OPENAPI_PATH` | no | Local OpenAPI JSON override; otherwise loaded from `<YOUTRACK_URL>/api/openapi.json` |

## Conventions

- Issue ids are readable (`ABC-123`); bare numbers expand via `YOUTRACK_DEFAULT_PROJECT`.
- Parent/child uses `issue_write.parentId` (native subtask) — **not** `link_write`.
- Omitting `issue_write.assignee` leaves it unchanged; `assignee: null` clears it.
- `issue_write.customFields` accepts `[{"name":"Priority","value":"Critical"}]` for any issue custom field. The server discovers each field's YouTrack type. Strings and string arrays are shorthand for named single/multi values; use `null`/`[]` to clear or API-native JSON for other field types.
- Dates are ISO `YYYY-MM-DD`.
- Tags must already exist; an unknown tag is an error (this server never creates tags).
- `issue_write op=delete` permanently deletes the issue.
- Errors are one-line JSON-RPC errors naming the bad/missing field; `YouTrack <status>: <msg>` means the API rejected the call.
- Generated tools are named `api_<method>_<path>`; request bodies belong under
  `body`, while path/query/header/cookie parameters stay top-level.

---

## Build from source (developers)

Needs the Rust toolchain. The pinned version in `rust-toolchain.toml` installs automatically via `rustup`.

Install straight from git:

```sh
cargo install --git https://github.com/sensiarion/youtrack-mcp --locked
```

(Binary lands in `~/.cargo/bin/youtrack-mcp`; point your MCP client's `command` at that path instead of `npx mcp-bin`.)

Or clone and build:

```sh
git clone https://github.com/sensiarion/youtrack-mcp
cd youtrack-mcp
cargo build --release
# binary at target/release/youtrack-mcp
```

Prebuilt release binaries (macOS Apple Silicon, Linux arm64/x64, Windows x64) — the assets `mcp-bin` downloads — are produced by [cargo-dist](https://opensource.axo.dev/cargo-dist/) on every `v*` tag. Intel Macs are not prebuilt; build from source as above.

## License

MIT
