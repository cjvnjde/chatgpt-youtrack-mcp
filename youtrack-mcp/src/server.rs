use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use rmcp::handler::server::tool::{ToolRoute, ToolRouter};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{tool, tool_handler, tool_router, ErrorData, ServerHandler};
use serde_json::Value;

use crate::model::*;
use crate::openapi::{self, ApiOperation};
use crate::report;
use crate::youtrack::{strip_url, Resolved, YouTrack, LIST_TOP};

#[derive(Clone)]
pub struct Server {
    yt: Arc<YouTrack>,
    router: ToolRouter<Self>,
    api_output_schemas: Arc<HashMap<String, Value>>,
}

/// Drop `$type` discriminator keys YouTrack stamps on every object — pure
/// noise for an LLM consumer and ~10-15% of response tokens.
fn strip_noise(v: &mut Value) {
    match v {
        Value::Object(map) => {
            map.remove("$type");
            for child in map.values_mut() {
                strip_noise(child);
            }
        }
        Value::Array(arr) => arr.iter_mut().for_each(strip_noise),
        _ => {}
    }
}

fn ok(mut v: Value) -> Result<String, ErrorData> {
    strip_noise(&mut v);
    Ok(serde_json::to_string(&v).unwrap_or_else(|_| "null".into()))
}

/// Same payload as [`ok`], as a full result — for tools that may also return
/// non-text content.
fn ok_result(v: Value) -> Result<CallToolResult, ErrorData> {
    Ok(CallToolResult::success(vec![ContentBlock::text(ok(v)?)]))
}

fn req<'a>(v: &'a Option<String>, msg: &str) -> Result<&'a str, ErrorData> {
    v.as_deref()
        .ok_or_else(|| ErrorData::invalid_params(msg.to_string(), None))
}

fn file_upload_tool_meta() -> rmcp::model::MetaObject {
    let mut meta = rmcp::model::MetaObject::new();
    meta.insert("openai/fileParams".into(), serde_json::json!(["file"]));
    meta
}

async fn fetch_openai_file(file: &OpenAiFile) -> Result<Vec<u8>, ErrorData> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|e| ErrorData::internal_error(format!("create file client: {e}"), None))?;
    let response = client
        .get(&file.download_url)
        .send()
        .await
        // reqwest errors can include the signed temporary URL. Do not put it
        // into an MCP error that may be retained in the conversation.
        .map_err(|_| ErrorData::internal_error("download ChatGPT file failed", None))?;
    let status = response.status();
    if !status.is_success() {
        return Err(ErrorData::invalid_params(
            format!("download ChatGPT file failed with HTTP {status}"),
            None,
        ));
    }
    response
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|e| ErrorData::internal_error(format!("read ChatGPT file: {e}"), None))
}

#[tool_router]
impl Server {
    pub async fn new(yt: Arc<YouTrack>) -> anyhow::Result<Self> {
        let spec = if let Ok(path) = std::env::var("YOUTRACK_OPENAPI_PATH") {
            let raw = tokio::fs::read_to_string(&path)
                .await
                .map_err(|error| anyhow::anyhow!("read YOUTRACK_OPENAPI_PATH {path:?}: {error}"))?;
            serde_json::from_str(&raw)
                .map_err(|error| anyhow::anyhow!("parse YOUTRACK_OPENAPI_PATH {path:?}: {error}"))?
        } else {
            yt.openapi_spec()
                .await
                .map_err(|error| anyhow::anyhow!("load YouTrack OpenAPI schema: {error}"))?
        };
        Self::from_openapi(yt, &spec)
    }

    fn from_openapi(yt: Arc<YouTrack>, spec: &Value) -> anyhow::Result<Self> {
        let operations = openapi::generate(spec)?;
        let generated_count = operations.len();
        let mut output_schemas = HashMap::with_capacity(generated_count);
        let mut router = Self::tool_router();
        for mut operation in operations {
            let name = operation.tool.name.to_string();
            let output_schema = operation
                .tool
                .output_schema
                .take()
                .map(|schema| Value::Object((*schema).clone()))
                .unwrap_or(Value::Null);
            output_schemas.insert(name, output_schema);
            let operation = Arc::new(operation);
            let tool = operation.tool.clone();
            router.add_route(Self::api_route(tool, operation));
        }
        tracing::info!(
            generated_api_tools = generated_count,
            total_tools = router.list_all().len(),
            "loaded typed YouTrack API mirror"
        );
        Ok(Self {
            yt,
            router,
            api_output_schemas: Arc::new(output_schemas),
        })
    }

    fn api_route(tool: rmcp::model::Tool, operation: Arc<ApiOperation>) -> ToolRoute<Self> {
        ToolRoute::new_dyn(
            tool,
            move |context: rmcp::handler::server::tool::ToolCallContext<'_, Self>| {
                let operation = operation.clone();
                Box::pin(async move {
                    let args = context.arguments.unwrap_or_default();
                    let result = match context
                        .service
                        .yt
                        .execute_api_operation(&operation, args)
                        .await
                    {
                        Ok(response) => response.into_tool_result(),
                        Err(error) => {
                            CallToolResult::error(vec![ContentBlock::text(error.to_string())])
                        }
                    };
                    Ok(result.into())
                })
            },
        )
    }

    #[tool(
        description = "Return the full OpenAPI output JSON Schema for a generated api_* operation. Generated output schemas are available on demand instead of being duplicated in tools/list, which keeps MCP discovery below infrastructure payload limits. Input schemas remain attached directly to every generated tool."
    )]
    async fn api_schema(
        &self,
        Parameters(a): Parameters<ApiSchemaArg>,
    ) -> Result<String, ErrorData> {
        let schema = self.api_output_schemas.get(&a.name).ok_or_else(|| {
            ErrorData::invalid_params(
                format!("api_schema unknown generated tool {:?}", a.name),
                None,
            )
        })?;
        serde_json::to_string(&serde_json::json!({
            "tool": a.name,
            "outputSchema": schema,
        }))
        .map_err(|error| {
            ErrorData::internal_error(format!("serialize API output schema: {error}"), None)
        })
    }

    #[tool(
        description = "Create, update or delete an issue: summary, description, parentId (native subtask), assignee (login; null clears), tags (must exist), state, customFields, board/sprint. customFields=[{name,value}] updates any issue custom field: strings/string arrays are shorthand for named single/multi values; null/[] clear; API-native JSON values pass through. Field types are discovered automatically. board+sprint resolve by name or id, any language/casing. op=delete is irreversible."
    )]
    async fn issue_write(
        &self,
        Parameters(a): Parameters<IssueWrite>,
    ) -> Result<String, ErrorData> {
        let v = match a.op {
            IssueOp::Create => self.yt.issue_create(&a).await?,
            IssueOp::Update => self.yt.issue_update(&a).await?,
            IssueOp::Delete => {
                self.yt
                    .issue_delete(req(&a.id, "issue_write op=delete requires 'id'")?)
                    .await?
            }
        };
        ok(v)
    }

    #[tool(description = "Get a single issue by id with full fields.")]
    async fn issue_get(&self, Parameters(a): Parameters<IdArg>) -> Result<String, ErrorData> {
        ok(self.yt.issue_get(&a.id).await?)
    }

    #[tool(description = "Search issues by YouTrack query. fields short|full.")]
    async fn issue_search(
        &self,
        Parameters(a): Parameters<IssueSearch>,
    ) -> Result<String, ErrorData> {
        let full = matches!(a.fields, Some(SearchFields::Full));
        ok(self
            .yt
            .issue_search(&a.query, full, a.top.unwrap_or(50), a.skip.unwrap_or(0))
            .await?)
    }

    #[tool(description = "List links of an issue (direction, type, related issues).")]
    async fn issue_links(&self, Parameters(a): Parameters<IdArg>) -> Result<String, ErrorData> {
        ok(self.yt.issue_links(&a.id).await?)
    }

    #[tool(
        description = "Add or remove an issue link. role outward|inward for directed types. For parent/child use issue_write.parentId."
    )]
    async fn link_write(&self, Parameters(a): Parameters<LinkWrite>) -> Result<String, ErrorData> {
        let inward = matches!(a.role, Some(LinkRole::Inward));
        let v = match a.op {
            LinkOp::Add => {
                self.yt
                    .link_add(&a.source_id, &a.target_id, &a.link_type, inward)
                    .await?
            }
            LinkOp::Remove => {
                self.yt
                    .link_remove(&a.source_id, &a.target_id, &a.link_type, inward)
                    .await?
            }
        };
        ok(v)
    }

    #[tool(
        description = "Create or update a comment on an issue or article (entity). op=update needs commentId."
    )]
    async fn comment_write(
        &self,
        Parameters(a): Parameters<CommentWrite>,
    ) -> Result<String, ErrorData> {
        let comment_id = match a.op {
            WriteOp::Create => None,
            WriteOp::Update => Some(req(
                &a.comment_id,
                "comment_write op=update requires 'commentId'",
            )?),
        };
        ok(self
            .yt
            .comment_write(
                a.entity,
                &a.parent_id,
                comment_id,
                &a.text,
                a.markdown,
                a.mute.unwrap_or(false),
            )
            .await?)
    }

    #[tool(description = "List comments of an issue or article (entity).")]
    async fn comments_list(
        &self,
        Parameters(a): Parameters<CommentsList>,
    ) -> Result<String, ErrorData> {
        ok(self.yt.comments_list(a.entity, &a.parent_id).await?)
    }

    #[tool(description = "Create or update a knowledge-base article.")]
    async fn article_write(
        &self,
        Parameters(a): Parameters<ArticleWrite>,
    ) -> Result<String, ErrorData> {
        let v = match a.op {
            WriteOp::Create => self.yt.article_create(&a).await?,
            WriteOp::Update => self.yt.article_update(&a).await?,
        };
        ok(v)
    }

    #[tool(
        description = "Get an article by id (op get) or list articles (op list, optional query)."
    )]
    async fn article_get(
        &self,
        Parameters(a): Parameters<ArticleGet>,
    ) -> Result<String, ErrorData> {
        let v = match a.op {
            GetOp::Get => {
                self.yt
                    .article_get(req(&a.id, "article_get op=get requires 'id'")?)
                    .await?
            }
            GetOp::List => self.yt.article_list(a.query.as_deref()).await?,
        };
        ok(v)
    }

    #[tool(
        description = "Create/update/delete a time-tracking work item. type = work item type name/id. idempotent skips duplicates."
    )]
    async fn workitem_write(
        &self,
        Parameters(a): Parameters<WorkitemWrite>,
    ) -> Result<String, ErrorData> {
        let v = match a.op {
            WorkOp::Create => self.yt.workitem_create(&a).await?,
            WorkOp::Update => self.yt.workitem_update(&a).await?,
            WorkOp::Delete => {
                let wid = req(
                    &a.work_item_id,
                    "workitem_write op=delete requires 'workItemId'",
                )?;
                self.yt.workitem_delete(&a.issue_id, wid).await?
            }
        };
        ok(v)
    }

    #[tool(description = "List/aggregate work items by author and date range.")]
    async fn workitems_list(
        &self,
        Parameters(a): Parameters<WorkitemsList>,
    ) -> Result<String, ErrorData> {
        ok(self
            .yt
            .workitems_list(
                a.author.as_deref(),
                a.start_date.as_deref(),
                a.end_date.as_deref(),
                a.issue_id.as_deref(),
                a.top.unwrap_or(200),
                a.skip.unwrap_or(0),
            )
            .await?)
    }

    #[tool(
        description = "Per-day expected-vs-actual worktime report (480m/day, skips weekends/holidays)."
    )]
    async fn workitems_report(
        &self,
        Parameters(a): Parameters<WorkitemsReport>,
    ) -> Result<String, ErrorData> {
        ok(
            report::workitems_report(&self.yt, a.author.as_deref(), &a.start_date, &a.end_date)
                .await?,
        )
    }

    #[tool(description = "Users: op list (optional query) | me | get (by id).")]
    async fn users(&self, Parameters(a): Parameters<UsersArg>) -> Result<String, ErrorData> {
        let v = match a.op {
            UsersOp::List => self.yt.users_list(a.query.as_deref()).await?,
            UsersOp::Me => self.yt.user_current().await?,
            UsersOp::Get => {
                self.yt
                    .user_get(req(&a.id, "users op=get requires 'id'")?)
                    .await?
            }
        };
        ok(v)
    }

    #[tool(
        description = "Discovery: kind projects | link_types | work_item_types (optional project)."
    )]
    async fn meta(&self, Parameters(a): Parameters<MetaArg>) -> Result<String, ErrorData> {
        let v = match a.kind {
            MetaKind::Projects => self.yt.meta_projects().await?,
            MetaKind::LinkTypes => self.yt.meta_link_types().await?,
            MetaKind::WorkItemTypes => self.yt.meta_work_types(a.project.as_deref()).await?,
        };
        ok(v)
    }

    #[tool(
        description = "Activity feed. scope=issue (needs issueId, optional author) | user (needs author, defaults last 30d). categories default CustomFieldCategory,CommentsCategory. Dates ISO or unix ms."
    )]
    async fn activity(&self, Parameters(a): Parameters<ActivityArg>) -> Result<String, ErrorData> {
        let cats = a.categories.as_deref();
        let top = a.top.unwrap_or(100);
        let skip = a.skip.unwrap_or(0);
        let v = match a.scope {
            ActivityScope::Issue => {
                let issue = req(&a.issue_id, "activity scope=issue requires 'issueId'")?;
                self.yt
                    .issue_activities(
                        issue,
                        a.author.as_deref(),
                        a.start_date.as_deref(),
                        a.end_date.as_deref(),
                        cats,
                        top,
                        skip,
                    )
                    .await?
            }
            ActivityScope::User => {
                let author = req(&a.author, "activity scope=user requires 'author'")?;
                self.yt
                    .users_activity(
                        author,
                        a.start_date.as_deref(),
                        a.end_date.as_deref(),
                        cats,
                        a.reverse,
                        top,
                        skip,
                    )
                    .await?
            }
        };
        ok(v)
    }

    #[tool(
        description = "Upload a user-provided ChatGPT file unchanged to an issue (default) or article. Pass the attached file in 'file', never as a /mnt/data path or base64. ChatGPT replaces its internal file reference with an authorized temporary download URL before calling this tool.",
        meta = file_upload_tool_meta()
    )]
    async fn attachment_upload(
        &self,
        Parameters(a): Parameters<AttachmentUploadArg>,
    ) -> Result<CallToolResult, ErrorData> {
        let entity = a.entity.unwrap_or(Entity::Issue);
        let name = a
            .name
            .as_deref()
            .or(a.file.file_name.as_deref())
            .ok_or_else(|| {
                ErrorData::invalid_params(
                    "attachment_upload requires 'name' when the file has no file_name",
                    None,
                )
            })?;
        let bytes = fetch_openai_file(&a.file).await?;
        let mut v = self
            .yt
            .attachment_upload(entity, &a.parent_id, name, bytes)
            .await?;
        if !a.verbose.unwrap_or(false) {
            strip_url(&mut v);
        }
        ok_result(v)
    }

    #[tool(
        description = "List, inspect, download, or delete attachments on an issue (default) or article: op list|get|download|delete. This tool cannot upload; use attachment_upload for every upload. Target get/download/delete by 'attachmentId' or 'name'."
    )]
    async fn attachment(
        &self,
        Parameters(a): Parameters<AttachmentArg>,
    ) -> Result<CallToolResult, ErrorData> {
        let entity = a.entity.unwrap_or(Entity::Issue);
        let verbose = a.verbose.unwrap_or(false);
        let parent = a.parent_id.as_str();
        let top = a.top.unwrap_or(LIST_TOP).max(1);
        let v = match a.op {
            AttachOp::List => {
                self.yt
                    .attachments_list(entity, parent, top, verbose)
                    .await?
            }
            AttachOp::Get => {
                let r = self.resolve(entity, parent, &a, top).await?;
                let mut v = self.yt.attachment_meta(entity, parent, r).await?;
                if !verbose {
                    strip_url(&mut v);
                }
                v
            }
            AttachOp::Download => {
                let r = self.resolve(entity, parent, &a, top).await?;
                let meta = self.yt.attachment_meta(entity, parent, r).await?;
                let (name, mime, bytes) = self.yt.attachment_download(&meta).await?;
                return self
                    .deliver_download(a.path.as_deref(), name, mime, bytes)
                    .await;
            }
            AttachOp::Delete => {
                let r = self.resolve(entity, parent, &a, top).await?;
                self.yt.attachment_delete(entity, parent, &r.id).await?
            }
        };
        ok_result(v)
    }
}

/// Cap on inlining a downloaded image as a content block. Beyond this the file
/// goes to disk instead. Base64 on the wire plus the serialized copy peaks at
/// several times the file size, so this bounds that peak; for scale, the
/// largest attachment on the busiest issue tested was 393 KB.
const INLINE_IMAGE_MAX: usize = 4 * 1024 * 1024;

impl Server {
    /// Resolve `attachmentId`/`name` to an attachment, reporting a missing
    /// reference the way every other tool does — naming the op that needs it.
    async fn resolve(
        &self,
        entity: Entity,
        parent: &str,
        a: &AttachmentArg,
        top: i64,
    ) -> Result<Resolved, ErrorData> {
        if a.attachment_id.is_none() && a.name.is_none() {
            return Err(ErrorData::invalid_params(
                format!(
                    "attachment op={} requires 'attachmentId' or 'name'",
                    a.op.as_str()
                ),
                None,
            ));
        }
        Ok(self
            .yt
            .attachment_resolve(
                entity,
                parent,
                a.attachment_id.as_deref(),
                a.name.as_deref(),
                top,
            )
            .await?)
    }

    /// Hand a downloaded file to the caller in the form it can actually use: an
    /// image as a real image block it can look at, anything else as a file on
    /// disk. Base64 in a text result was neither — it blew up the context and
    /// stayed unreadable.
    async fn deliver_download(
        &self,
        requested_path: Option<&str>,
        name: String,
        mime: String,
        bytes: Vec<u8>,
    ) -> Result<CallToolResult, ErrorData> {
        if should_inline_image(requested_path.is_some(), &mime, bytes.len()) {
            let summary = serde_json::json!({"name": name, "bytes": bytes.len(), "mimeType": mime});
            return Ok(CallToolResult::success(vec![
                ContentBlock::text(ok(summary)?),
                ContentBlock::image(YouTrack::b64_encode(&bytes), mime),
            ]));
        }

        let path = match requested_path {
            // Caller-chosen target: written as given, but its directory is NOT
            // created. The name that lands here can be steered by whatever the
            // model just read from an issue, and creating missing parents turns
            // "write fails" into "write anywhere" (~/Library/LaunchAgents/…).
            // An operator pointing at a real directory is unaffected.
            Some(p) => PathBuf::from(p),
            None => {
                let dir = self
                    .yt
                    .cfg
                    .download_dir
                    .as_ref()
                    .map_or_else(std::env::temp_dir, PathBuf::from);
                // This root comes from env, not from the model, so creating it
                // is safe; the file name within it is separator-stripped.
                tokio::fs::create_dir_all(&dir).await.map_err(|e| {
                    ErrorData::internal_error(format!("create {}: {e}", dir.display()), None)
                })?;
                dir.join(sanitize_file_name(&name))
            }
        };
        tokio::fs::write(&path, &bytes).await.map_err(|e| {
            ErrorData::internal_error(format!("write {}: {e}", path.display()), None)
        })?;
        ok_result(serde_json::json!({
            "saved": path.to_string_lossy(),
            "bytes": bytes.len(),
            "mimeType": mime,
        }))
    }
}

/// Inline only what the caller can actually look at, and only when they did not
/// ask for a file: an explicit `path` is a request to put the bytes there.
fn should_inline_image(explicit_path: bool, mime: &str, len: usize) -> bool {
    !explicit_path && mime.starts_with("image/") && len <= INLINE_IMAGE_MAX
}

/// Keep a YouTrack-supplied file name from escaping the download directory —
/// it is attacker-controlled in the sense that anyone who can attach a file
/// picks it. Separators and parent refs are the only things that matter here.
fn sanitize_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if std::path::is_separator(c) || c == '\0' {
                '_'
            } else {
                c
            }
        })
        .collect();
    if cleaned.trim().trim_matches('.').is_empty() {
        return "attachment".to_string();
    }
    cleaned
}

#[tool_handler(
    router = self.router,
    name = "youtrack-mcp",
    // `version` deliberately omitted: the macro then reports CARGO_PKG_VERSION.
    // It used to be hardcoded and drifted to 0.1.0 while the crate was at 0.1.3,
    // so the handshake could not tell a client which build it had connected to.
    instructions = "Complete typed YouTrack MCP. Every api_* tool mirrors one operation from the connected instance's /api/openapi.json; path/query/header/cookie parameters are top-level and request payloads use body. Input schemas are attached to each generated tool; call api_schema with its name when the full output schema is needed. Generated tools include administrative and irreversible API operations, subject to YOUTRACK_TOKEN permissions. Curated-tool conventions: issue ids are readable like ABC-123 (bare numbers expand via YOUTRACK_DEFAULT_PROJECT). For parent/child use issue_write.parentId (native subtask) — NOT link_write. issue_write assignee=null clears the assignee. issue_write customFields=[{name,value}] updates arbitrary custom fields; use strings/string arrays for named single/multi values, null/[] to clear, or API-native JSON values. link_write needs sourceId+targetId+linkType (name e.g. 'Relates','Depend') and role outward|inward for directed types. Dates are ISO YYYY-MM-DD. tags must already exist (curated tools never create tags; unknown tag = error). issue_write op=delete permanently deletes the issue. workitem_write needs date+minutes (+type name like 'Разработка'); idempotent=true skips a same issue+date+description entry. workitems_list/report default to the current user. Errors state the missing/invalid field or return 'YouTrack <status>: <msg>' when the API rejected the call."
)]
impl ServerHandler for Server {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_upload_declares_required_chatgpt_file_parameter() {
        let tools = Server::tool_router().list_all();
        assert!(tools.iter().any(|tool| tool.name == "attachment_upload"));

        let tool = serde_json::to_value(Server::attachment_upload_tool_attr()).unwrap();
        assert_eq!(
            tool["_meta"]["openai/fileParams"],
            serde_json::json!(["file"])
        );

        let schema = &tool["inputSchema"];
        assert!(schema["required"]
            .as_array()
            .is_some_and(|required| required.iter().any(|field| field == "file")));
        assert!(schema["properties"]["path"].is_null());
        let file_schema = &schema["$defs"]["OpenAiFile"];
        assert_eq!(
            file_schema["required"],
            serde_json::json!(["download_url", "file_id"])
        );
        assert_eq!(file_schema["additionalProperties"], false);
        for field in ["download_url", "file_id", "mime_type", "file_name"] {
            assert!(file_schema["properties"][field].is_object());
        }
    }

    #[test]
    fn attachment_tool_rejects_upload() {
        let tool = serde_json::to_value(Server::attachment_tool_attr()).unwrap();
        assert_eq!(
            tool["inputSchema"]["$defs"]["AttachOp"]["enum"],
            serde_json::json!(["list", "get", "download", "delete"])
        );

        let args = serde_json::json!({
            "op": "upload",
            "parentId": "MI-1427",
            "path": "/mnt/data/original.png",
            "name": "original.png"
        });

        assert!(serde_json::from_value::<AttachmentArg>(args).is_err());
    }

    fn schema_permits_null(schema: &Value) -> bool {
        match schema {
            Value::String(value) => value == "null",
            Value::Array(values) => values.iter().any(schema_permits_null),
            Value::Object(values) => values.values().any(schema_permits_null),
            _ => false,
        }
    }

    #[test]
    fn issue_write_schema_exposes_arbitrary_custom_fields_and_nullable_assignee() {
        let tool = serde_json::to_value(Server::issue_write_tool_attr()).unwrap();
        let schema = &tool["inputSchema"];
        let properties = &schema["properties"];

        assert!(properties["customFields"].is_object());
        assert!(properties["type"].is_null());
        assert!(schema_permits_null(&properties["assignee"]));
        assert_eq!(
            schema["$defs"]["CustomFieldWrite"]["required"],
            serde_json::json!(["name", "value"])
        );
    }

    #[tokio::test]
    async fn registers_generated_operations_with_output_schemas_available_on_demand() {
        let yt = YouTrack::new(crate::config::Config {
            base_url: "http://yt.invalid".into(),
            token: "t".into(),
            timezone: chrono_tz::Europe::Moscow,
            default_project: None,
            holidays: Default::default(),
            pre_holidays: Default::default(),
            user_aliases: Default::default(),
            download_dir: None,
        })
        .unwrap();
        let spec = serde_json::json!({
            "openapi":"3.0.1",
            "paths":{
                "/widgets":{
                    "get":{"responses":{"200":{"content":{"application/json":{
                        "schema":{"type":"array","items":{"type":"object"}}
                    }}}}},
                    "post":{"requestBody":{"content":{"application/json":{
                        "schema":{"type":"object","properties":{"name":{"type":"string"}}}
                    }}},"responses":{"200":{"description":"updated"}}}
                }
            }
        });

        let server = Server::from_openapi(yt, &spec).unwrap();
        let tools = server.router.list_all();
        assert!(tools.iter().any(|tool| tool.name == "issue_write"));
        assert!(tools.iter().any(|tool| tool.name == "api_schema"));
        for name in ["api_get_widgets", "api_post_widgets"] {
            let tool = tools.iter().find(|tool| tool.name == name).unwrap();
            assert!(
                tool.output_schema.is_none(),
                "generated output schemas must not inflate tools/list"
            );
        }

        let schema = server
            .api_schema(Parameters(ApiSchemaArg {
                name: "api_get_widgets".into(),
            }))
            .await
            .unwrap();
        let schema: Value = serde_json::from_str(&schema).unwrap();
        assert_eq!(schema["tool"], "api_get_widgets");
        assert_eq!(schema["outputSchema"]["type"], "array");
        assert_eq!(
            schema["outputSchema"]["items"]["type"],
            serde_json::json!("object")
        );
    }

    #[test]
    fn generated_output_schema_cannot_push_discovery_over_cosmos_item_limit() {
        const COSMOS_DB_ITEM_LIMIT_BYTES: usize = 2 * 1024 * 1024;

        let yt = YouTrack::new(crate::config::Config {
            base_url: "http://yt.invalid".into(),
            token: "t".into(),
            timezone: chrono_tz::Europe::Moscow,
            default_project: None,
            holidays: Default::default(),
            pre_holidays: Default::default(),
            user_aliases: Default::default(),
            download_dir: None,
        })
        .unwrap();
        let spec = serde_json::json!({
            "openapi":"3.0.1",
            "paths":{
                "/oversized":{
                    "get":{"responses":{"200":{"content":{"application/json":{
                        "schema":{
                            "type":"object",
                            "description":"x".repeat(COSMOS_DB_ITEM_LIMIT_BYTES)
                        }
                    }}}}}
                }
            }
        });

        let server = Server::from_openapi(yt, &spec).unwrap();
        assert!(
            server.api_output_schemas["api_get_oversized"]
                .to_string()
                .len()
                > COSMOS_DB_ITEM_LIMIT_BYTES
        );
        let discovery = serde_json::to_vec(&server.router.list_all()).unwrap();
        assert!(
            discovery.len() < COSMOS_DB_ITEM_LIMIT_BYTES,
            "tools/list payload was {} bytes",
            discovery.len()
        );
    }

    #[tokio::test]
    async fn chatgpt_file_download_preserves_exact_bytes() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let expected = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0xff];
        let response_body = expected.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response_body.len()
            )
            .unwrap();
            stream.write_all(&response_body).unwrap();
        });
        let file = OpenAiFile {
            download_url: format!("http://{address}/original.png"),
            file_id: "file_test".into(),
            mime_type: Some("image/png".into()),
            file_name: Some("original.png".into()),
        };

        let actual = fetch_openai_file(&file).await.unwrap();
        server.join().unwrap();

        assert_eq!(actual, expected);
    }

    #[test]
    fn sanitize_file_name_neutralises_traversal() {
        assert_eq!(sanitize_file_name("../../etc/passwd"), ".._.._etc_passwd");
        assert_eq!(sanitize_file_name(".."), "attachment");
        assert_eq!(sanitize_file_name("   "), "attachment");
    }

    #[test]
    fn sanitize_file_name_keeps_ordinary_names() {
        assert_eq!(sanitize_file_name("image51.png"), "image51.png");
        assert_eq!(sanitize_file_name("Иерархия 2.0.txt"), "Иерархия 2.0.txt");
    }

    #[test]
    fn inlines_a_small_image_without_an_explicit_path() {
        assert!(should_inline_image(false, "image/png", 1024));
    }

    #[test]
    fn does_not_inline_when_a_path_was_requested() {
        assert!(!should_inline_image(true, "image/png", 1024));
    }

    #[test]
    fn does_not_inline_a_non_image() {
        assert!(!should_inline_image(false, "application/pdf", 1024));
    }

    #[test]
    fn does_not_inline_an_oversized_image() {
        assert!(!should_inline_image(
            false,
            "image/png",
            INLINE_IMAGE_MAX + 1
        ));
    }

    #[test]
    fn strip_noise_removes_type_keys_recursively() {
        let mut v = serde_json::json!({"$type": "Issue", "a": {"$type": "X", "b": 1}});
        strip_noise(&mut v);
        assert_eq!(v, serde_json::json!({"a": {"b": 1}}));
    }
}
