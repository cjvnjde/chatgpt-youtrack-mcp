use schemars::JsonSchema;
use serde::{Deserialize, Deserializer};
use serde_json::Value;
fn deserialize_present_nullable_string<'de, D>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WriteOp {
    Create,
    Update,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Entity {
    Issue,
    Article,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IssueOp {
    Create,
    Update,
    Delete,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CustomFieldWrite {
    /// Custom-field name.
    pub name: String,
    /// New value. Strings and string arrays are shorthand for named single-
    /// and multi-value fields; API-native JSON objects are passed through.
    /// null clears a single value; [] clears a multi-value field.
    pub value: Value,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IssueWrite {
    pub op: IssueOp,
    /// Issue id (required for update; bare number expanded via default project).
    #[serde(default)]
    pub id: Option<String>,
    /// Project shortName or id (required for create).
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub markdown: Option<bool>,
    /// Parent issue id for native subtask hierarchy. Empty string clears it.
    #[serde(default, rename = "parentId")]
    pub parent_id: Option<String>,
    /// Assignee login. Omit to leave unchanged; null clears the assignee.
    #[serde(default, deserialize_with = "deserialize_present_nullable_string")]
    pub assignee: Option<Option<String>>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub state: Option<String>,
    /// Arbitrary issue custom fields. YouTrack field types are discovered from
    /// the issue, so callers provide only each field name and new value.
    #[serde(default, rename = "customFields")]
    pub custom_fields: Option<Vec<CustomFieldWrite>>,
    /// Agile board name or id (any language/casing).
    #[serde(default)]
    pub board: Option<String>,
    /// Sprint name or id within `board`. The valid set is board-specific;
    /// an unknown sprint errors with that board's available sprints.
    #[serde(default)]
    pub sprint: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CommentWrite {
    pub entity: Entity,
    pub op: WriteOp,
    /// Issue or article id.
    #[serde(rename = "parentId")]
    pub parent_id: String,
    #[serde(default, rename = "commentId")]
    pub comment_id: Option<String>,
    pub text: String,
    #[serde(default)]
    pub markdown: Option<bool>,
    #[serde(default)]
    pub mute: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ArticleWrite {
    pub op: WriteOp,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default, rename = "parentArticleId")]
    pub parent_article_id: Option<String>,
    #[serde(default)]
    pub markdown: Option<bool>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LinkOp {
    Add,
    Remove,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LinkRole {
    Outward,
    Inward,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LinkWrite {
    pub op: LinkOp,
    #[serde(rename = "sourceId")]
    pub source_id: String,
    #[serde(rename = "targetId")]
    pub target_id: String,
    /// Link type name, e.g. "Relates", "Depend". Not for parent/child — use issue_write.parentId.
    #[serde(rename = "linkType")]
    pub link_type: String,
    /// Semantic direction for directed types. Default outward.
    #[serde(default)]
    pub role: Option<LinkRole>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkOp {
    Create,
    Update,
    Delete,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WorkitemWrite {
    pub op: WorkOp,
    #[serde(rename = "issueId")]
    pub issue_id: String,
    #[serde(default, rename = "workItemId")]
    pub work_item_id: Option<String>,
    /// ISO date YYYY-MM-DD.
    #[serde(default)]
    pub date: Option<String>,
    pub minutes: Option<i64>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// Work item type name or id (e.g. "Разработка").
    #[serde(default, rename = "type")]
    pub work_type: Option<String>,
    #[serde(default)]
    pub markdown: Option<bool>,
    /// On create: skip if same issue+date+description already logged.
    #[serde(default)]
    pub idempotent: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IdArg {
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApiSchemaArg {
    /// Generated API tool name, for example api_post_issues.
    pub name: String,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SearchFields {
    Short,
    Full,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IssueSearch {
    pub query: String,
    #[serde(default)]
    pub fields: Option<SearchFields>,
    #[serde(default)]
    pub top: Option<i64>,
    #[serde(default)]
    pub skip: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CommentsList {
    pub entity: Entity,
    #[serde(rename = "parentId")]
    pub parent_id: String,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GetOp {
    Get,
    List,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ArticleGet {
    pub op: GetOp,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub query: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WorkitemsList {
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default, rename = "startDate")]
    pub start_date: Option<String>,
    #[serde(default, rename = "endDate")]
    pub end_date: Option<String>,
    #[serde(default, rename = "issueId")]
    pub issue_id: Option<String>,
    #[serde(default)]
    pub top: Option<i64>,
    #[serde(default)]
    pub skip: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WorkitemsReport {
    #[serde(default)]
    pub author: Option<String>,
    #[serde(rename = "startDate")]
    pub start_date: String,
    #[serde(rename = "endDate")]
    pub end_date: String,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UsersOp {
    List,
    Me,
    Get,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UsersArg {
    pub op: UsersOp,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub query: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MetaKind {
    Projects,
    LinkTypes,
    WorkItemTypes,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MetaArg {
    pub kind: MetaKind,
    #[serde(default)]
    pub project: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActivityScope {
    /// Change-history of one issue (GET /api/issues/{id}/activities).
    Issue,
    /// Cross-issue feed for one author (GET /api/activities).
    User,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ActivityArg {
    pub scope: ActivityScope,
    /// Required for scope=issue.
    #[serde(default, rename = "issueId")]
    pub issue_id: Option<String>,
    /// Author login. Required for scope=user; optional filter for scope=issue.
    #[serde(default)]
    pub author: Option<String>,
    /// ISO YYYY-MM-DD or unix ms. scope=user defaults to 30 days back.
    #[serde(default, rename = "startDate")]
    pub start_date: Option<String>,
    /// ISO YYYY-MM-DD or unix ms. Defaults to now.
    #[serde(default, rename = "endDate")]
    pub end_date: Option<String>,
    /// Activity categories. Default CustomFieldCategory,CommentsCategory.
    /// Others: AttachmentsCategory, LinksCategory, WorkItemsActivityCategory,
    /// VcsChangeActivityCategory, TagsCategory, SprintCategory.
    #[serde(default)]
    pub categories: Option<Vec<String>>,
    /// scope=user only: oldest-first when true.
    #[serde(default)]
    pub reverse: Option<bool>,
    #[serde(default)]
    pub top: Option<i64>,
    #[serde(default)]
    pub skip: Option<i64>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttachOp {
    List,
    Get,
    Download,
    Delete,
}

impl AttachOp {
    /// The wire spelling, for error messages that name the failing op.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Get => "get",
            Self::Download => "download",
            Self::Delete => "delete",
        }
    }
}

/// A user-provided file authorized by ChatGPT for this tool call.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub struct OpenAiFile {
    pub download_url: String,
    pub file_id: String,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub file_name: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AttachmentUploadArg {
    /// Which entity owns the attachment. Defaults to `issue`.
    #[serde(default)]
    pub entity: Option<Entity>,
    /// Issue or article id (bare number expanded via default project).
    #[serde(rename = "parentId", alias = "issueId")]
    pub parent_id: String,
    /// User-provided file authorized by ChatGPT for this tool call.
    pub file: OpenAiFile,
    /// Override the uploaded file name when `file_name` is unavailable or unsuitable.
    #[serde(default)]
    pub name: Option<String>,
    /// Include the signed YouTrack `url` in the response. Off by default.
    #[serde(default)]
    pub verbose: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AttachmentArg {
    pub op: AttachOp,
    /// Which entity owns the attachment. Defaults to `issue`.
    #[serde(default)]
    pub entity: Option<Entity>,
    /// Issue or article id (bare number expanded via default project).
    #[serde(rename = "parentId", alias = "issueId")]
    pub parent_id: String,
    /// Exact attachment id. For get/download/delete either this or `name`.
    #[serde(default, rename = "attachmentId")]
    pub attachment_id: Option<String>,
    /// File name. Upload: the name to store under. get/download/delete:
    /// resolved to an id — an ambiguous name errors with the candidate ids.
    #[serde(default)]
    pub name: Option<String>,
    /// Local target for download.
    #[serde(default)]
    pub path: Option<String>,
    /// op=list page size. Default 500.
    #[serde(default)]
    pub top: Option<i64>,
    /// Include the signed `url` field in list/get output. Off by default: it is
    /// a bearer-equivalent link and the bulk of the payload.
    #[serde(default)]
    pub verbose: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_assignee_distinguishes_omitted_null_and_login() {
        let omitted: IssueWrite =
            serde_json::from_value(serde_json::json!({"op":"update","id":"ABC-1"})).unwrap();
        let cleared: IssueWrite =
            serde_json::from_value(serde_json::json!({"op":"update","id":"ABC-1","assignee":null}))
                .unwrap();
        let assigned: IssueWrite = serde_json::from_value(
            serde_json::json!({"op":"update","id":"ABC-1","assignee":"alice"}),
        )
        .unwrap();

        assert_eq!(omitted.assignee, None);
        assert_eq!(cleared.assignee, Some(None));
        assert_eq!(assigned.assignee, Some(Some("alice".into())));
    }

    #[test]
    fn issue_accepts_arbitrary_custom_fields() {
        let issue: IssueWrite = serde_json::from_value(serde_json::json!({
            "op":"update",
            "id":"ABC-1",
            "customFields":[
                {"name":"Priority","value":"Critical"},
                {"name":"Story points","value":8}
            ]
        }))
        .unwrap();

        let fields = issue.custom_fields.unwrap();
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].name, "Priority");
        assert_eq!(fields[0].value, serde_json::json!("Critical"));
        assert_eq!(fields[1].value, serde_json::json!(8));
    }
}
