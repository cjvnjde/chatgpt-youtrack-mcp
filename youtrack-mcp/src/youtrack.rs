use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use base64::Engine;
use reqwest::{Client, Method, StatusCode};
use serde_json::{json, Value};
use tokio::sync::RwLock;

use crate::config::Config;
use crate::error::{AppError, Result};
use crate::model::{CustomFieldWrite, Entity};
use crate::openapi::{ApiOperation, ApiResponse, ApiResponseBody, PreparedBody};

const F_ISSUE: &str = "id,idReadable,summary,description,usesMarkdown,created,updated,resolved,project(id,shortName,name),parent(issues(idReadable,summary)),assignee(id,login,name),reporter(id,login,name),tags(id,name),customFields(id,name,value(id,login,name,presentation),$type)";
const F_ISSUE_SHORT: &str = "id,idReadable,summary,project(shortName),customFields(name,value(name,login,presentation),$type)";
const F_ISSUE_FULL: &str = "id,idReadable,summary,description,usesMarkdown,project(id,shortName,name),parent(issues(idReadable,summary)),assignee(login,name),tags(name),customFields(name,value(name,login,presentation),$type)";
const F_LINKS: &str = "id,direction,linkType(id,name,directed,sourceToTarget,targetToSource),issues(idReadable,summary,project(shortName),assignee(login,name))";
const F_LINK_TYPES: &str = "id,name,directed,sourceToTarget,targetToSource,aggregation";
const F_COMMENT: &str = "id,text,usesMarkdown,author(id,login,name),created,updated";
const F_ARTICLE: &str = "id,idReadable,summary,content,usesMarkdown,parentArticle(id,idReadable),project(id,shortName,name)";
const F_ARTICLE_LIST: &str =
    "id,idReadable,summary,parentArticle(id,idReadable),project(shortName)";
const F_WORKITEM: &str = "id,date,updated,duration(minutes,presentation),text,description,usesMarkdown,type(id,name),issue(id,idReadable),author(id,login,name)";
const F_USERS: &str = "id,login,name,fullName,email";
const F_PROJECTS: &str = "id,shortName,name";
/// `url` is a pre-signed, bearer-equivalent link and is the bulk of the payload
/// (~60% of a list response), so it is fetched but stripped unless `verbose`.
const F_ATTACH: &str = "id,name,author(login),created,size,mimeType,url,extension";
/// Page size for collection endpoints. YouTrack's own default is 42 and it
/// truncates to that silently — no marker, no count — so every collection
/// states its own ceiling instead of inheriting one. Nothing realistic
/// exceeds this.
pub(crate) const LIST_TOP: i64 = 500;
const F_ACTIVITY: &str = "id,timestamp,author(login,name),category(id),target(text,issue(idReadable,summary)),added(name,login),removed(name,login)";
const ACTIVITY_DEFAULT_CATEGORIES: &str = "CustomFieldCategory,CommentsCategory";

/// Transport settings shared by every client this server builds.
fn client_builder() -> reqwest::ClientBuilder {
    Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .connect_timeout(std::time::Duration::from_secs(15))
}

pub struct YouTrack {
    pub cfg: Config,
    http: Client,
    /// Follows redirects; see `YouTrack::new`.
    http_files: Client,
    link_types: RwLock<Vec<Value>>,
    projects: RwLock<HashMap<String, String>>,
    work_types: RwLock<HashMap<String, String>>,
    tags: RwLock<HashMap<String, String>>,
    current_login: RwLock<Option<String>>,
}

fn is_id(s: &str) -> bool {
    let mut parts = s.split('-');
    matches!((parts.next(), parts.next(), parts.next()),
        (Some(a), Some(b), None) if !a.is_empty() && !b.is_empty()
            && a.bytes().all(|c| c.is_ascii_digit())
            && b.bytes().all(|c| c.is_ascii_digit()))
}

fn require<'a>(opt: Option<&'a String>, msg: &str) -> Result<&'a str> {
    opt.map(|s| s.as_str())
        .ok_or_else(|| AppError::Bad(msg.to_string()))
}

fn api_scalar(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Null => String::new(),
        _ => value.to_string(),
    }
}

/// True if `node.name` or `node.id` matches `needle`, comparing trimmed and
/// case-folded. `to_lowercase` is full-Unicode (folds Cyrillic/Greek/etc.),
/// so names in any language/script and any casing resolve — not just exact
/// bytes. Used to resolve agile boards/sprints supplied by name.
fn name_or_id_eq(node: &Value, needle: &str) -> bool {
    let want = needle.trim().to_lowercase();
    ["name", "id"].iter().any(|k| {
        node.get(k)
            .and_then(Value::as_str)
            .is_some_and(|s| s.trim().to_lowercase() == want)
    })
}

/// Comma-joined quoted `name`s, for the "valid values" hint in a resolution
/// error — a caller can't know the accepted set up front (a board exposes only
/// its own sprints; attachment names are whatever was uploaded), so it is
/// surfaced on failure. `cap` bounds long lists, and states how many it cut:
/// a silently short list is what sent callers guessing ids in the first place.
fn quoted_names(nodes: &[Value], cap: Option<usize>) -> String {
    let names: Vec<&str> = nodes
        .iter()
        .filter_map(|n| n.get("name").and_then(Value::as_str))
        .collect();
    if names.is_empty() {
        return "none".to_string();
    }
    let cap = cap.unwrap_or(names.len());
    let shown = names
        .iter()
        .take(cap)
        .map(|n| format!("\"{n}\""))
        .collect::<Vec<_>>()
        .join(", ");
    match names.len().saturating_sub(cap) {
        0 => shown,
        rest => format!("{shown} … and {rest} more"),
    }
}

/// Drop the pre-signed `url`: it grants access to the file on its own, so it
/// does not belong in output an LLM will echo into logs and transcripts.
/// Recurses, because an upload answers with an *array* of created attachments —
/// stripping only a top-level object would silently miss it.
pub(crate) fn strip_url(v: &mut Value) {
    match v {
        Value::Object(map) => {
            map.remove("url");
            map.values_mut().for_each(strip_url);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_url),
        _ => {}
    }
}

/// Stringify an attachment field for error messages; numbers included, since
/// `size` arrives as a JSON number.
fn attach_str(node: &Value, key: &str) -> String {
    match node.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => "?".to_string(),
    }
}

/// Attachments whose `name` matches `needle`. Exact match wins; only if nothing
/// matches exactly is the trimmed/case-folded comparison tried, so `a.PNG` can
/// never shadow a literal `a.png` that exists alongside it.
fn match_by_name<'a>(items: &'a [Value], needle: &str) -> Vec<&'a Value> {
    let exact: Vec<&Value> = items
        .iter()
        .filter(|a| a.get("name").and_then(Value::as_str) == Some(needle))
        .collect();
    if !exact.is_empty() {
        return exact;
    }
    let want = needle.trim().to_lowercase();
    items
        .iter()
        .filter(|a| {
            a.get("name")
                .and_then(Value::as_str)
                .is_some_and(|n| n.trim().to_lowercase() == want)
        })
        .collect()
}

/// How many attachment names a "no such name" error lists before cutting.
const NAMES_IN_ERROR: usize = 40;

/// An attachment reference resolved to an id.
pub struct Resolved {
    pub id: String,
    /// The listing row, present only when the id came from a name lookup.
    /// Carries the same fields a per-id GET returns, so callers reuse it
    /// instead of asking YouTrack for what they already have.
    pub meta: Option<Value>,
}

impl YouTrack {
    pub fn new(cfg: Config) -> Result<Arc<Self>> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", cfg.token)
                .parse()
                .map_err(|_| AppError::Config("invalid token".into()))?,
        );
        headers.insert(reqwest::header::ACCEPT, "application/json".parse().unwrap());
        let http = client_builder()
            .default_headers(headers.clone())
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        // File downloads are the one place redirects are expected: YouTrack
        // Cloud answers /api/files/… with a 302 to object storage. reqwest
        // follows them and strips Authorization the moment host-or-port
        // changes, which is stricter than a host-only check would be.
        let http_files = client_builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()?;
        Ok(Arc::new(Self {
            cfg,
            http,
            http_files,
            link_types: RwLock::new(vec![]),
            projects: RwLock::new(HashMap::new()),
            work_types: RwLock::new(HashMap::new()),
            tags: RwLock::new(HashMap::new()),
            current_login: RwLock::new(None),
        }))
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.cfg.base_url, path)
    }

    async fn check(&self, resp: reqwest::Response) -> Result<Value> {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if status.is_success() {
            if body.trim().is_empty() {
                return Ok(Value::Null);
            }
            serde_json::from_str(&body).map_err(|e| AppError::Network(format!("bad JSON: {e}")))
        } else {
            let msg = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| {
                    for k in ["error_description", "message", "error", "code"] {
                        if let Some(s) = v.get(k).and_then(|x| x.as_str()) {
                            return Some(s.to_string());
                        }
                    }
                    None
                })
                .unwrap_or_else(|| body.chars().take(200).collect());
            Err(AppError::Api {
                status: status.as_u16(),
                message: msg,
            })
        }
    }

    async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        let has_page = query.iter().any(|(k, _)| *k == "$top" || *k == "$skip");
        // GETs are idempotent: retry once on transient connect/timeout failures.
        let resp = match self.http.get(self.url(path)).query(query).send().await {
            Ok(r) => r,
            Err(e) if e.is_timeout() || e.is_connect() => {
                self.http.get(self.url(path)).query(query).send().await?
            }
            Err(e) => return Err(e.into()),
        };
        if has_page && resp.status() == StatusCode::BAD_REQUEST {
            let plain: Vec<(String, String)> = query
                .iter()
                .map(|(k, v)| (k.trim_start_matches('$').to_string(), v.clone()))
                .collect();
            let resp2 = self.http.get(self.url(path)).query(&plain).send().await?;
            return self.check(resp2).await;
        }
        self.check(resp).await
    }

    pub async fn openapi_spec(&self) -> Result<Value> {
        self.get("/api/openapi.json", &[]).await
    }

    pub async fn execute_api_operation(
        &self,
        operation: &ApiOperation,
        args: serde_json::Map<String, Value>,
    ) -> Result<ApiResponse> {
        let prepared = operation.prepare(args).map_err(AppError::Bad)?;
        let client = if operation.method == Method::GET || operation.method == Method::HEAD {
            &self.http_files
        } else {
            &self.http
        };
        let mut request = client
            .request(operation.method.clone(), self.url(&prepared.path))
            .query(&prepared.query);
        for (name, value) in prepared.headers {
            request = request.header(name, value);
        }
        request = match prepared.body {
            Some(PreparedBody::Json(body)) => {
                let request = request.json(&body);
                if prepared.content_type.as_deref() == Some("application/json") {
                    request
                } else {
                    request.header(
                        reqwest::header::CONTENT_TYPE,
                        prepared
                            .content_type
                            .as_deref()
                            .unwrap_or("application/json"),
                    )
                }
            }
            Some(PreparedBody::Form(fields)) => request.form(&fields),
            Some(PreparedBody::Multipart {
                fields,
                binary_fields,
            }) => {
                let mut form = reqwest::multipart::Form::new();
                for (name, value) in fields {
                    if binary_fields.contains(&name) {
                        let encoded_values: Vec<&str> = match &value {
                            Value::String(value) => vec![value],
                            Value::Array(values) => values
                                .iter()
                                .map(|value| {
                                    value.as_str().ok_or_else(|| {
                                        AppError::Bad(format!(
                                            "multipart binary field {name:?} must contain base64 strings"
                                        ))
                                    })
                                })
                                .collect::<Result<Vec<_>>>()?,
                            _ => {
                                return Err(AppError::Bad(format!(
                                    "multipart binary field {name:?} must be base64 or an array of base64 strings"
                                )));
                            }
                        };
                        for encoded in encoded_values {
                            let bytes = Self::b64_decode(encoded)?;
                            let part =
                                reqwest::multipart::Part::bytes(bytes).file_name(name.clone());
                            form = form.part(name.clone(), part);
                        }
                    } else {
                        match value {
                            Value::Array(values) => {
                                for value in values {
                                    form = form.text(name.clone(), api_scalar(&value));
                                }
                            }
                            value => form = form.text(name, api_scalar(&value)),
                        }
                    }
                }
                request.multipart(form)
            }
            Some(PreparedBody::Binary(body)) => request
                .header(
                    reqwest::header::CONTENT_TYPE,
                    prepared
                        .content_type
                        .as_deref()
                        .unwrap_or("application/octet-stream"),
                )
                .body(body),
            Some(PreparedBody::Text(body)) => request
                .header(
                    reqwest::header::CONTENT_TYPE,
                    prepared.content_type.as_deref().unwrap_or("text/plain"),
                )
                .body(body),
            None => request,
        };
        let response = request.send().await?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.bytes().await?;
        let body = if bytes.is_empty() {
            ApiResponseBody::Empty
        } else if let Ok(json) = serde_json::from_slice(&bytes) {
            ApiResponseBody::Json(json)
        } else if content_type.as_deref().is_some_and(|content_type| {
            content_type.starts_with("text/")
                || content_type.contains("xml")
                || content_type.contains("javascript")
                || content_type.contains("x-www-form-urlencoded")
        }) {
            ApiResponseBody::Text(String::from_utf8_lossy(&bytes).into_owned())
        } else {
            ApiResponseBody::Binary(Self::b64_encode(&bytes))
        };
        Ok(ApiResponse {
            status,
            content_type,
            body,
        })
    }

    async fn send_json(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: &Value,
    ) -> Result<Value> {
        let resp = self
            .http
            .request(method, self.url(path))
            .query(query)
            .json(body)
            .send()
            .await?;
        self.check(resp).await
    }

    async fn post(&self, path: &str, query: &[(&str, String)], body: &Value) -> Result<Value> {
        self.send_json(Method::POST, path, query, body).await
    }

    async fn delete(&self, path: &str) -> Result<Value> {
        let resp = self.http.delete(self.url(path)).send().await?;
        self.check(resp).await
    }

    async fn command(&self, query: &str, id_readable: &str) -> Result<()> {
        let body = json!({"query": query, "issues": [{"idReadable": id_readable}], "silent": true});
        self.post("/api/commands", &[], &body).await?;
        Ok(())
    }

    fn fq(&self, fields: &str) -> [(&'static str, String); 1] {
        [("fields", fields.to_string())]
    }

    /// GET a collection with an explicit page size.
    ///
    /// Use this — never bare `get` — for any endpoint returning an array.
    /// `get` is for single entities, where `$top` is meaningless and would also
    /// arm its `has_page` 400-retry path for no reason. A saturated page is
    /// warned about rather than passed off as a complete list.
    async fn list(&self, path: &str, fields: &str, top: i64) -> Result<Value> {
        let v = self
            .get(
                path,
                &[("fields", fields.to_string()), ("$top", top.to_string())],
            )
            .await?;
        if v.as_array().is_some_and(|a| a.len() as i64 >= top) {
            tracing::warn!(
                path,
                top,
                "collection filled the page; results may be truncated"
            );
        }
        Ok(v)
    }

    // ---- resolvers / caches ----

    /// name→id resolver shared by projects/tags/work-item-types: bare ids pass
    /// through, otherwise fetch the list once and cache `key_field`→`id`.
    async fn cached_id(
        &self,
        cache: &RwLock<HashMap<String, String>>,
        endpoint: &str,
        fields: &str,
        key_field: &str,
        name: &str,
        noun: &str,
    ) -> Result<String> {
        if is_id(name) {
            return Ok(name.to_string());
        }
        if let Some(id) = cache.read().await.get(name) {
            return Ok(id.clone());
        }
        let list = self
            .get(
                endpoint,
                &[("fields", fields.to_string()), ("$top", "1000".to_string())],
            )
            .await?;
        let mut map = cache.write().await;
        if let Some(arr) = list.as_array() {
            for e in arr {
                if let (Some(k), Some(id)) = (
                    e.get(key_field).and_then(|x| x.as_str()),
                    e.get("id").and_then(|x| x.as_str()),
                ) {
                    map.insert(k.to_string(), id.to_string());
                }
            }
        }
        map.get(name)
            .cloned()
            .ok_or_else(|| AppError::Bad(format!("unknown {noun} '{name}'")))
    }

    pub async fn project_id(&self, project: &str) -> Result<String> {
        self.cached_id(
            &self.projects,
            "/api/admin/projects",
            F_PROJECTS,
            "shortName",
            project,
            "project",
        )
        .await
    }

    async fn user_value(&self, login: &str) -> Result<Value> {
        let login = self.cfg.resolve_alias(login).to_string();
        // Paged explicitly: on a truncated page the exact-login match can fall
        // off the end, and the `arr.first()` fallback below would then resolve
        // to a different user entirely — a silently wrong assignee, not a
        // missing result.
        let q = [
            ("fields", "id,login,name".to_string()),
            ("query", login.clone()),
            ("$top", LIST_TOP.to_string()),
        ];
        let res = self.get("/api/users", &q).await?;
        if let Some(arr) = res.as_array() {
            if let Some(u) = arr
                .iter()
                .find(|u| u.get("login").and_then(|x| x.as_str()) == Some(&login))
            {
                return Ok(json!({"id": u.get("id"), "login": login}));
            }
            if let Some(u) = arr.first() {
                return Ok(json!({"id": u.get("id"), "login": u.get("login")}));
            }
        }
        Err(AppError::Bad(format!("unknown user '{login}'")))
    }

    pub async fn work_type_id(&self, name_or_id: &str) -> Result<String> {
        self.cached_id(
            &self.work_types,
            "/api/admin/timeTrackingSettings/workItemTypes",
            "id,name",
            "name",
            name_or_id,
            "work item type",
        )
        .await
    }

    async fn link_type(&self, name: &str) -> Result<Value> {
        {
            let c = self.link_types.read().await;
            if let Some(t) = c.iter().find(|t| {
                t.get("name").and_then(|x| x.as_str()) == Some(name)
                    || t.get("id").and_then(|x| x.as_str()) == Some(name)
            }) {
                return Ok(t.clone());
            }
        }
        let list = self
            .list("/api/issueLinkTypes", F_LINK_TYPES, LIST_TOP)
            .await?;
        let arr = list.as_array().cloned().unwrap_or_default();
        *self.link_types.write().await = arr.clone();
        arr.into_iter()
            .find(|t| {
                t.get("name").and_then(|x| x.as_str()) == Some(name)
                    || t.get("id").and_then(|x| x.as_str()) == Some(name)
            })
            .ok_or_else(|| AppError::Bad(format!("unknown link type '{name}'")))
    }

    // ---- issues ----

    pub async fn issue_get(&self, id: &str) -> Result<Value> {
        let id = self.cfg.expand_issue_id(id);
        self.get(&format!("/api/issues/{id}"), &self.fq(F_ISSUE))
            .await
    }

    pub async fn issue_search(
        &self,
        query: &str,
        full: bool,
        top: i64,
        skip: i64,
    ) -> Result<Value> {
        let fields = if full { F_ISSUE_FULL } else { F_ISSUE_SHORT };
        let q = [
            ("fields", fields.to_string()),
            ("query", query.to_string()),
            ("$top", top.to_string()),
            ("$skip", skip.to_string()),
        ];
        self.get("/api/issues", &q).await
    }

    pub async fn issue_links(&self, id: &str) -> Result<Value> {
        let id = self.cfg.expand_issue_id(id);
        self.list(&format!("/api/issues/{id}/links"), F_LINKS, LIST_TOP)
            .await
    }

    /// Resolve an existing tag name to its id. This server never creates tags;
    /// an unknown name is an error.
    async fn tag_id(&self, name: &str) -> Result<String> {
        self.cached_id(
            &self.tags,
            "/api/tags",
            "id,name",
            "name",
            name,
            "tag (must already exist)",
        )
        .await
    }

    /// Attach an issue to an agile board/sprint.
    ///
    /// The `Board` apply-command keyword is localized per YouTrack instance,
    /// so command-based attach is not portable across instances/languages.
    /// The agiles REST collection is locale-independent: resolve the board and
    /// sprint by name (or id) and POST into the sprint's issues collection.
    async fn set_board(&self, id_readable: &str, board: &str, sprint: Option<&str>) -> Result<()> {
        let resp = self
            .list("/api/agiles", "id,name,sprints(id,name)", LIST_TOP)
            .await?;
        let boards = resp.as_array().map(Vec::as_slice).unwrap_or(&[]);
        let agile = boards
            .iter()
            .find(|b| name_or_id_eq(b, board))
            .ok_or_else(|| {
                AppError::Bad(format!(
                    "agile board not found: {board}. Available boards: {}",
                    quoted_names(boards, None)
                ))
            })?;
        let aid = agile
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::Api {
                status: 500,
                message: "agile board missing id".into(),
            })?;
        let sprints = agile
            .get("sprints")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let sid = match sprint {
            Some(s) => sprints
                .iter()
                .find(|sp| name_or_id_eq(sp, s))
                .and_then(|sp| sp.get("id").and_then(Value::as_str))
                .ok_or_else(|| {
                    AppError::Bad(format!(
                        "sprint \"{s}\" not found on board {board}. Valid sprints: {}",
                        quoted_names(sprints, None)
                    ))
                })?
                .to_string(),
            // Board membership on YouTrack is sprint-scoped. With no sprint
            // requested: a single-sprint board has an unambiguous target; an
            // empty board is a no-op; a multi-sprint board needs a choice.
            None => match sprints {
                [only] => only
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| AppError::Api {
                        status: 500,
                        message: "sprint missing id".into(),
                    })?
                    .to_string(),
                [] => return Ok(()),
                _ => {
                    return Err(AppError::Bad(format!(
                        "board {board} has multiple sprints; specify `sprint`. Valid sprints: {}",
                        quoted_names(sprints, None)
                    )));
                }
            },
        };
        let body = json!({"$type": "Issue", "idReadable": id_readable});
        self.post(
            &format!("/api/agiles/{aid}/sprints/{sid}/issues"),
            &[],
            &body,
        )
        .await?;
        Ok(())
    }

    pub async fn issue_delete(&self, id: &str) -> Result<Value> {
        let id = self.cfg.expand_issue_id(id);
        self.delete(&format!("/api/issues/{id}")).await?;
        Ok(json!({"deleted": true, "id": id}))
    }

    /// Parent/child on this on-prem is only honored via the command API
    /// (`subtask of <parent>` on the child); the issue `parent` body field is
    /// silently ignored. `parent=None` detaches from the current parent.
    async fn set_parent(&self, child_readable: &str, parent: Option<&str>) -> Result<()> {
        match parent {
            Some(p) => {
                let p = self.cfg.expand_issue_id(p);
                self.command(&format!("subtask of {p}"), child_readable)
                    .await
            }
            None => {
                let cur = self
                    .get(
                        &format!("/api/issues/{child_readable}"),
                        &self.fq("parent(issues(idReadable))"),
                    )
                    .await?;
                if let Some(p) = cur
                    .pointer("/parent/issues/0/idReadable")
                    .and_then(|x| x.as_str())
                {
                    self.command(&format!("remove subtask of {p}"), child_readable)
                        .await?;
                }
                Ok(())
            }
        }
    }

    pub async fn issue_create(&self, a: &crate::model::IssueWrite) -> Result<Value> {
        let project = require(
            a.project.as_ref(),
            "issue_write op=create requires 'project' (project shortName like ABC or its id)",
        )?;
        let mut body = json!({
            "summary": a.summary.clone().unwrap_or_default(),
            "project": {"id": self.project_id(project).await?},
        });
        if let Some(d) = &a.description {
            body["description"] = json!(d);
        }
        if let Some(m) = a.markdown {
            body["usesMarkdown"] = json!(m);
        }
        let created = self.post("/api/issues", &self.fq(F_ISSUE), &body).await?;
        let internal = created
            .get("id")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string();
        let readable = created
            .get("idReadable")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string();
        if self.apply_side_effects(&internal, &readable, a).await? {
            self.get(&format!("/api/issues/{internal}"), &self.fq(F_ISSUE))
                .await
        } else {
            Ok(created)
        }
    }

    pub async fn issue_update(&self, a: &crate::model::IssueWrite) -> Result<Value> {
        let raw = require(
            a.id.as_ref(),
            "issue_write op=update requires 'id' (issue id like ABC-123)",
        )?;
        let id = self.cfg.expand_issue_id(raw);
        let mut body = json!({});
        if let Some(s) = &a.summary {
            body["summary"] = json!(s);
        }
        if let Some(d) = &a.description {
            body["description"] = json!(d);
        }
        if let Some(m) = a.markdown {
            body["usesMarkdown"] = json!(m);
        }
        let mut current = if body.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
            Some(
                self.post(&format!("/api/issues/{id}"), &self.fq(F_ISSUE), &body)
                    .await?,
            )
        } else {
            None
        };
        if self.apply_side_effects(&id, &id, a).await? || current.is_none() {
            current = Some(
                self.get(&format!("/api/issues/{id}"), &self.fq(F_ISSUE))
                    .await?,
            );
        }
        Ok(current.unwrap())
    }

    fn normalize_custom_field_value(field_type: &str, value: &Value) -> Value {
        let reference_key = if field_type.contains("UserIssueCustomField") {
            Some("login")
        } else if field_type == "StateIssueCustomField"
            || field_type.starts_with("Single")
            || field_type.starts_with("Multi")
        {
            Some("name")
        } else {
            None
        };
        let Some(reference_key) = reference_key else {
            return value.clone();
        };

        let reference = |name: &str| match reference_key {
            "login" => json!({"login": name}),
            _ => json!({"name": name}),
        };
        match value {
            Value::String(name) if !field_type.starts_with("Multi") => reference(name),
            Value::Array(values)
                if field_type.starts_with("Multi") && values.iter().all(Value::is_string) =>
            {
                Value::Array(
                    values
                        .iter()
                        .map(|value| reference(value.as_str().unwrap()))
                        .collect(),
                )
            }
            _ => value.clone(),
        }
    }

    async fn resolve_custom_field_updates(
        &self,
        issue_id: &str,
        requested: &[CustomFieldWrite],
    ) -> Result<Vec<Value>> {
        let issue = self
            .get(
                &format!("/api/issues/{issue_id}"),
                &self.fq("customFields(id,name,$type)"),
            )
            .await?;
        let available = issue
            .get("customFields")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                AppError::Bad("YouTrack returned no customFields for the issue".into())
            })?;
        let mut resolved = Vec::with_capacity(requested.len());
        let mut seen = HashSet::with_capacity(requested.len());

        for update in requested {
            let matches: Vec<&Value> = available
                .iter()
                .filter(|field| name_or_id_eq(field, &update.name))
                .collect();
            let field = match matches.as_slice() {
                [field] => *field,
                [] => {
                    return Err(AppError::Bad(format!(
                        "unknown custom field {:?}; valid fields: {}",
                        update.name,
                        quoted_names(available, Some(40))
                    )));
                }
                _ => {
                    return Err(AppError::Bad(format!(
                        "custom field name {:?} is ambiguous",
                        update.name
                    )));
                }
            };
            let field_id = field.get("id").and_then(Value::as_str).ok_or_else(|| {
                AppError::Bad(format!("custom field {:?} has no id", update.name))
            })?;
            if !seen.insert(field_id) {
                return Err(AppError::Bad(format!(
                    "custom field {:?} was provided more than once",
                    update.name
                )));
            }
            let field_type = field.get("$type").and_then(Value::as_str).ok_or_else(|| {
                AppError::Bad(format!("custom field {:?} has no $type", update.name))
            })?;
            resolved.push(json!({
                "id": field_id,
                "$type": field_type,
                "value": Self::normalize_custom_field_value(field_type, &update.value),
            }));
        }
        Ok(resolved)
    }

    /// Apply assignee/custom fields/state/tags in a single issue POST and board via command.
    /// Returns true if anything was changed (caller then re-reads the issue).
    async fn apply_side_effects(
        &self,
        id: &str,
        readable: &str,
        a: &crate::model::IssueWrite,
    ) -> Result<bool> {
        let mut fields = Vec::new();
        if let Some(assignee) = &a.assignee {
            let value = match assignee {
                Some(login) => {
                    let mut user = self.user_value(login).await?;
                    user["$type"] = json!("User");
                    user
                }
                None => Value::Null,
            };
            fields.push(
                json!({"name":"Assignee","$type":"SingleUserIssueCustomField","value":value}),
            );
        }
        if let Some(state) = &a.state {
            fields.push(
                json!({"name":"State","$type":"StateIssueCustomField","value":{"name":state}}),
            );
        }
        if let Some(custom_fields) = a.custom_fields.as_ref().filter(|fields| !fields.is_empty()) {
            if a.assignee.is_some()
                && custom_fields
                    .iter()
                    .any(|field| field.name.trim().eq_ignore_ascii_case("Assignee"))
            {
                return Err(AppError::Bad(
                    "provide Assignee through either assignee or customFields, not both".into(),
                ));
            }
            if a.state.is_some()
                && custom_fields
                    .iter()
                    .any(|field| field.name.trim().eq_ignore_ascii_case("State"))
            {
                return Err(AppError::Bad(
                    "provide State through either state or customFields, not both".into(),
                ));
            }
            fields.extend(self.resolve_custom_field_updates(id, custom_fields).await?);
        }
        let mut body = json!({});
        if !fields.is_empty() {
            body["customFields"] = json!(fields);
        }
        if let Some(tags) = a.tags.as_ref().filter(|t| !t.is_empty()) {
            let mut refs = Vec::with_capacity(tags.len());
            for t in tags {
                refs.push(json!({"id": self.tag_id(t).await?}));
            }
            body["tags"] = json!(refs);
        }
        let mut changed = false;
        if body.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
            self.post(&format!("/api/issues/{id}"), &[], &body).await?;
            changed = true;
        }
        if let Some(p) = &a.parent_id {
            if p.is_empty() {
                self.set_parent(readable, None).await?;
            } else {
                self.set_parent(readable, Some(p)).await?;
            }
            changed = true;
        }
        if let Some(board) = &a.board {
            self.set_board(readable, board, a.sprint.as_deref()).await?;
            changed = true;
        }
        Ok(changed)
    }

    // ---- links ----

    fn link_keyword(lt: &Value, inward: bool) -> Result<String> {
        let directed = lt
            .get("directed")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        let s = lt
            .get("sourceToTarget")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let t = lt
            .get("targetToSource")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let k = if directed && inward { t } else { s };
        if k.is_empty() {
            Err(AppError::Bad("link type has no usable keyword".into()))
        } else {
            Ok(k.to_string())
        }
    }

    pub async fn link_add(
        &self,
        source: &str,
        target: &str,
        link_type: &str,
        inward: bool,
    ) -> Result<Value> {
        let source = self.cfg.expand_issue_id(source);
        let target = self.cfg.expand_issue_id(target);
        let lt = self.link_type(link_type).await?;
        let body =
            json!({"linkType": {"name": lt.get("name")}, "issues": [{"idReadable": target}]});
        match self
            .post(
                &format!("/api/issues/{source}/links"),
                &self.fq(F_LINKS),
                &body,
            )
            .await
        {
            Ok(v) => Ok(v),
            Err(AppError::Api { status, .. }) if status == 404 || status == 405 => {
                let keyword = Self::link_keyword(&lt, inward)?;
                self.command(&format!("{keyword}: {target}"), &source)
                    .await?;
                Ok(json!({"linked": true, "source": source, "target": target, "via": "command"}))
            }
            Err(e) => Err(e),
        }
    }

    pub async fn link_remove(
        &self,
        source: &str,
        target: &str,
        link_type: &str,
        inward: bool,
    ) -> Result<Value> {
        let source = self.cfg.expand_issue_id(source);
        let target = self.cfg.expand_issue_id(target);
        let lt = self.link_type(link_type).await?;
        let keyword = Self::link_keyword(&lt, inward)?;
        self.command(&format!("remove {keyword}: {target}"), &source)
            .await?;
        Ok(json!({"unlinked": true, "source": source, "target": target}))
    }

    // ---- comments ----

    /// Canonical id of an entity as YouTrack addresses it. Bare issue numbers
    /// expand via the default project; article ids are already qualified.
    fn entity_id(&self, entity: Entity, parent: &str) -> String {
        match entity {
            Entity::Issue => self.cfg.expand_issue_id(parent),
            Entity::Article => parent.to_string(),
        }
    }

    /// Path to a sub-collection (`comments`, `attachments`, …) of an entity.
    fn entity_root(&self, entity: Entity, parent: &str, sub: &str) -> String {
        let base = match entity {
            Entity::Issue => "issues",
            Entity::Article => "articles",
        };
        format!("/api/{base}/{}/{sub}", self.entity_id(entity, parent))
    }

    fn comment_root(&self, entity: Entity, parent: &str) -> String {
        self.entity_root(entity, parent, "comments")
    }

    pub async fn comments_list(&self, entity: Entity, parent: &str) -> Result<Value> {
        self.list(&self.comment_root(entity, parent), F_COMMENT, LIST_TOP)
            .await
    }

    /// Create (comment_id None) or update (Some) a comment on an issue/article.
    pub async fn comment_write(
        &self,
        entity: Entity,
        parent: &str,
        comment_id: Option<&str>,
        text: &str,
        markdown: Option<bool>,
        mute: bool,
    ) -> Result<Value> {
        let mut body = json!({ "text": text });
        if let Some(m) = markdown {
            body["usesMarkdown"] = json!(m);
        }
        let root = self.comment_root(entity, parent);
        let path = match comment_id {
            Some(c) => format!("{root}/{c}"),
            None => root,
        };
        let mut q = vec![("fields", F_COMMENT.to_string())];
        if mute && comment_id.is_some() {
            q.push(("muteUpdateNotifications", "true".to_string()));
        }
        self.post(&path, &q, &body).await
    }

    // ---- articles ----

    pub async fn article_get(&self, id: &str) -> Result<Value> {
        self.get(&format!("/api/articles/{id}"), &self.fq(F_ARTICLE))
            .await
    }

    pub async fn article_list(&self, query: Option<&str>) -> Result<Value> {
        let mut q = vec![
            ("fields", F_ARTICLE_LIST.to_string()),
            ("$top", "100".to_string()),
        ];
        if let Some(query) = query {
            q.push(("query", query.to_string()));
        }
        self.get("/api/articles", &q).await
    }

    pub async fn article_create(&self, a: &crate::model::ArticleWrite) -> Result<Value> {
        let project = a.project.as_deref().ok_or_else(|| {
            AppError::Bad(
                "article_write op=create requires 'project' (project shortName or id)".into(),
            )
        })?;
        let pid = self.project_id(project).await?;
        let mut body = json!({
            "summary": a.summary.clone().unwrap_or_default(),
            "content": a.content.clone().unwrap_or_default(),
            "project": {"id": pid},
        });
        if let Some(m) = a.markdown {
            body["usesMarkdown"] = json!(m);
        }
        if let Some(p) = &a.parent_article_id {
            body["parentArticle"] = json!({"id": p});
        }
        self.post("/api/articles", &self.fq(F_ARTICLE), &body).await
    }

    pub async fn article_update(&self, a: &crate::model::ArticleWrite) -> Result<Value> {
        let id = a.id.as_deref().ok_or_else(|| {
            AppError::Bad("article_write op=update requires 'id' (article id)".into())
        })?;
        let mut body = json!({});
        if let Some(s) = &a.summary {
            body["summary"] = json!(s);
        }
        if let Some(c) = &a.content {
            body["content"] = json!(c);
        }
        if let Some(m) = a.markdown {
            body["usesMarkdown"] = json!(m);
        }
        if let Some(p) = &a.parent_article_id {
            body["parentArticle"] = json!({"id": p});
        }
        self.post(&format!("/api/articles/{id}"), &self.fq(F_ARTICLE), &body)
            .await
    }

    // ---- work items ----

    pub async fn workitems_list(
        &self,
        author: Option<&str>,
        start: Option<&str>,
        end: Option<&str>,
        issue: Option<&str>,
        top: i64,
        skip: i64,
    ) -> Result<Value> {
        let mut q = vec![
            ("fields", F_WORKITEM.to_string()),
            ("$top", top.to_string()),
            ("$skip", skip.to_string()),
        ];
        let author_login = match author {
            Some(a) => self.cfg.resolve_alias(a).to_string(),
            None => self.current_login().await?,
        };
        q.push(("author", author_login));
        if let Some(s) = start {
            q.push(("startDate", s.to_string()));
        }
        if let Some(e) = end {
            q.push(("endDate", e.to_string()));
        }
        if let Some(i) = issue {
            q.push(("issueId", self.cfg.expand_issue_id(i)));
        }
        self.get("/api/workItems", &q).await
    }

    fn date_to_epoch_ms(&self, iso: &str) -> Result<i64> {
        use chrono::TimeZone;
        let d = chrono::NaiveDate::parse_from_str(iso, "%Y-%m-%d")
            .map_err(|_| AppError::Bad(format!("bad date '{iso}', expected YYYY-MM-DD")))?;
        let dt = d.and_hms_opt(12, 0, 0).unwrap();
        let local = self
            .cfg
            .timezone
            .from_local_datetime(&dt)
            .single()
            .ok_or_else(|| AppError::Bad("ambiguous date".into()))?;
        Ok(local.timestamp_millis())
    }

    pub async fn workitem_create(&self, a: &crate::model::WorkitemWrite) -> Result<Value> {
        let issue = self.cfg.expand_issue_id(&a.issue_id);
        let date = a.date.as_deref().ok_or_else(|| {
            AppError::Bad("workitem_write op=create requires 'date' (ISO YYYY-MM-DD)".into())
        })?;
        let minutes = a.minutes.ok_or_else(|| {
            AppError::Bad("workitem_write op=create requires 'minutes' (integer duration)".into())
        })?;
        let desc = a
            .description
            .clone()
            .or_else(|| a.text.clone())
            .unwrap_or_default();

        if a.idempotent.unwrap_or(false) {
            let existing = self
                .workitems_list(None, Some(date), Some(date), Some(&issue), 200, 0)
                .await?;
            if let Some(arr) = existing.as_array() {
                if arr.iter().any(|w| {
                    w.get("description").and_then(|x| x.as_str()) == Some(&desc)
                        || w.get("text").and_then(|x| x.as_str()) == Some(&desc)
                }) {
                    return Ok(json!({"skipped": true, "reason": "already logged"}));
                }
            }
        }

        let mut body = json!({
            "date": self.date_to_epoch_ms(date)?,
            "duration": {"minutes": minutes},
            "text": a.text.clone().unwrap_or_else(|| desc.clone()),
            "description": desc,
        });
        if let Some(m) = a.markdown {
            body["usesMarkdown"] = json!(m);
        }
        if let Some(t) = &a.work_type {
            body["type"] = json!({"id": self.work_type_id(t).await?});
        }
        self.post(
            &format!("/api/issues/{issue}/timeTracking/workItems"),
            &self.fq(F_WORKITEM),
            &body,
        )
        .await
    }

    pub async fn workitem_update(&self, a: &crate::model::WorkitemWrite) -> Result<Value> {
        let issue = self.cfg.expand_issue_id(&a.issue_id);
        let wid = a.work_item_id.as_deref().ok_or_else(|| {
            AppError::Bad(
                "workitem_write op=update requires 'workItemId' (get it from workitems_list)"
                    .into(),
            )
        })?;
        let mut body = json!({});
        if let Some(d) = &a.date {
            body["date"] = json!(self.date_to_epoch_ms(d)?);
        }
        if let Some(m) = a.minutes {
            body["duration"] = json!({"minutes": m});
        }
        if let Some(t) = &a.text {
            body["text"] = json!(t);
        }
        if let Some(d) = &a.description {
            body["description"] = json!(d);
        }
        if let Some(t) = &a.work_type {
            body["type"] = json!({"id": self.work_type_id(t).await?});
        }
        let path = format!("/api/issues/{issue}/timeTracking/workItems/{wid}");
        match self.post(&path, &self.fq(F_WORKITEM), &body).await {
            Ok(v) => Ok(v),
            Err(AppError::Api { status, .. }) if status == 404 || status == 405 => {
                let existing = self
                    .get(&format!("/api/workItems/{wid}"), &self.fq(F_WORKITEM))
                    .await?;
                let merged = crate::model::WorkitemWrite {
                    op: crate::model::WorkOp::Create,
                    issue_id: issue.clone(),
                    work_item_id: None,
                    date: a.date.clone().or_else(|| {
                        existing
                            .get("date")
                            .and_then(|x| x.as_i64())
                            .map(|ms| self.epoch_ms_to_iso(ms))
                    }),
                    minutes: a.minutes.or_else(|| {
                        existing
                            .get("duration")
                            .and_then(|d| d.get("minutes"))
                            .and_then(|x| x.as_i64())
                    }),
                    text: a.text.clone(),
                    description: a.description.clone().or_else(|| {
                        existing
                            .get("description")
                            .and_then(|x| x.as_str())
                            .map(String::from)
                    }),
                    work_type: a.work_type.clone(),
                    markdown: a.markdown,
                    idempotent: None,
                };
                let created = self.workitem_create(&merged).await?;
                self.delete(&format!("/api/issues/{issue}/timeTracking/workItems/{wid}"))
                    .await?;
                Ok(created)
            }
            Err(e) => Err(e),
        }
    }

    pub fn epoch_ms_to_iso(&self, ms: i64) -> String {
        use chrono::TimeZone;
        self.cfg
            .timezone
            .timestamp_millis_opt(ms)
            .single()
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default()
    }

    pub async fn workitem_delete(&self, issue: &str, wid: &str) -> Result<Value> {
        let issue = self.cfg.expand_issue_id(issue);
        self.delete(&format!("/api/issues/{issue}/timeTracking/workItems/{wid}"))
            .await?;
        Ok(json!({"deleted": true, "issueId": issue, "workItemId": wid}))
    }

    // ---- activities ----

    /// Activity time bounds accept ISO YYYY-MM-DD or a raw unix-ms integer.
    fn activity_ts(&self, s: &str) -> Result<i64> {
        match s.parse::<i64>() {
            Ok(n) => Ok(n),
            Err(_) => self.date_to_epoch_ms(s),
        }
    }

    fn activity_categories(cats: Option<&[String]>) -> String {
        match cats {
            Some(c) if !c.is_empty() => c.join(","),
            _ => ACTIVITY_DEFAULT_CATEGORIES.to_string(),
        }
    }

    pub async fn issue_activities(
        &self,
        issue: &str,
        author: Option<&str>,
        start: Option<&str>,
        end: Option<&str>,
        categories: Option<&[String]>,
        top: i64,
        skip: i64,
    ) -> Result<Value> {
        let issue = self.cfg.expand_issue_id(issue);
        let mut q = vec![
            ("fields", F_ACTIVITY.to_string()),
            ("categories", Self::activity_categories(categories)),
            ("$top", top.to_string()),
            ("$skip", skip.to_string()),
        ];
        if let Some(a) = author {
            q.push(("author", self.cfg.resolve_alias(a).to_string()));
        }
        if let Some(s) = start {
            q.push(("start", self.activity_ts(s)?.to_string()));
        }
        if let Some(e) = end {
            q.push(("end", self.activity_ts(e)?.to_string()));
        }
        self.get(&format!("/api/issues/{issue}/activities"), &q)
            .await
    }

    pub async fn users_activity(
        &self,
        author: &str,
        start: Option<&str>,
        end: Option<&str>,
        categories: Option<&[String]>,
        reverse: Option<bool>,
        top: i64,
        skip: i64,
    ) -> Result<Value> {
        let end_ms = match end {
            Some(e) => self.activity_ts(e)?,
            None => chrono::Utc::now().timestamp_millis(),
        };
        let start_ms = match start {
            Some(s) => self.activity_ts(s)?,
            None => end_ms - 30 * 24 * 60 * 60 * 1000,
        };
        if start_ms > end_ms {
            return Err(AppError::Bad("startDate after endDate".into()));
        }
        let mut q = vec![
            ("fields", F_ACTIVITY.to_string()),
            ("author", self.cfg.resolve_alias(author).to_string()),
            ("categories", Self::activity_categories(categories)),
            ("start", start_ms.to_string()),
            ("end", end_ms.to_string()),
            ("$top", top.to_string()),
            ("$skip", skip.to_string()),
        ];
        if let Some(r) = reverse {
            q.push(("reverse", r.to_string()));
        }
        self.get("/api/activities", &q).await
    }

    // ---- users / meta ----

    pub async fn users_list(&self, query: Option<&str>) -> Result<Value> {
        let mut q = vec![("fields", F_USERS.to_string()), ("$top", "100".to_string())];
        if let Some(query) = query {
            q.push(("query", query.to_string()));
        }
        self.get("/api/users", &q).await
    }

    pub async fn user_current(&self) -> Result<Value> {
        self.get("/api/users/me", &self.fq(F_USERS)).await
    }

    async fn current_login(&self) -> Result<String> {
        if let Some(l) = self.current_login.read().await.clone() {
            return Ok(l);
        }
        let login = self
            .user_current()
            .await?
            .get("login")
            .and_then(|x| x.as_str())
            .map(String::from)
            .ok_or_else(|| AppError::Bad("cannot resolve current user".into()))?;
        *self.current_login.write().await = Some(login.clone());
        Ok(login)
    }

    pub async fn user_get(&self, id: &str) -> Result<Value> {
        self.get(&format!("/api/users/{id}"), &self.fq(F_USERS))
            .await
    }

    pub async fn meta_projects(&self) -> Result<Value> {
        self.list("/api/admin/projects", F_PROJECTS, LIST_TOP).await
    }

    pub async fn meta_link_types(&self) -> Result<Value> {
        self.list("/api/issueLinkTypes", F_LINK_TYPES, LIST_TOP)
            .await
    }

    pub async fn meta_work_types(&self, project: Option<&str>) -> Result<Value> {
        match project {
            Some(p) => {
                let pid = self.project_id(p).await?;
                self.list(
                    &format!("/api/admin/projects/{pid}/timeTrackingSettings/workItemTypes"),
                    "id,name",
                    LIST_TOP,
                )
                .await
            }
            None => {
                self.list(
                    "/api/admin/timeTrackingSettings/workItemTypes",
                    "id,name",
                    LIST_TOP,
                )
                .await
            }
        }
    }

    // ---- attachments ----

    fn attach_root(&self, entity: Entity, parent: &str) -> String {
        self.entity_root(entity, parent, "attachments")
    }

    /// Raw listing, always paged explicitly — YouTrack's implicit default cuts
    /// off at 42 with nothing in the response saying so.
    async fn attachments_raw(&self, entity: Entity, parent: &str, top: i64) -> Result<Vec<Value>> {
        let v = self
            .list(&self.attach_root(entity, parent), F_ATTACH, top)
            .await?;
        match v {
            Value::Array(a) => Ok(a),
            other => Err(AppError::Bad(format!(
                "expected attachment array, got {other}"
            ))),
        }
    }

    pub async fn attachments_list(
        &self,
        entity: Entity,
        parent: &str,
        top: i64,
        verbose: bool,
    ) -> Result<Value> {
        let mut items = self.attachments_raw(entity, parent, top).await?;
        // A full page means YouTrack may be holding more back; say so rather
        // than letting the caller read a cut-off list as complete.
        let truncated = items.len() as i64 >= top;
        if !verbose {
            items.iter_mut().for_each(strip_url);
        }
        Ok(json!({"count": items.len(), "truncated": truncated, "attachments": items}))
    }

    pub async fn attachment_get(&self, entity: Entity, parent: &str, aid: &str) -> Result<Value> {
        self.get(
            &format!("{}/{aid}", self.attach_root(entity, parent)),
            &self.fq(F_ATTACH),
        )
        .await
    }

    /// Resolve an attachment reference. `aid` wins; otherwise `name` is matched
    /// against the listing. Names are NOT unique — YouTrack auto-names pasted
    /// screenshots per entity, and Culture-435 carries both "image25.png" and
    /// "Иерархия 2.0.txt" twice — so a name resolves only when exactly one
    /// attachment carries it, and an ambiguous one names the candidates.
    pub async fn attachment_resolve(
        &self,
        entity: Entity,
        parent: &str,
        aid: Option<&str>,
        name: Option<&str>,
        top: i64,
    ) -> Result<Resolved> {
        if let Some(id) = aid {
            return Ok(Resolved {
                id: id.to_string(),
                meta: None,
            });
        }
        let Some(name) = name else {
            return Err(AppError::Bad("requires 'attachmentId' or 'name'".into()));
        };
        let items = self.attachments_raw(entity, parent, top).await?;
        let matches = match_by_name(&items, name);
        match matches.as_slice() {
            [one] => Ok(Resolved {
                id: attach_str(one, "id"),
                meta: Some((*one).clone()),
            }),
            [] => Err(AppError::Bad(format!(
                "no attachment named {name:?} on {parent}. Available: {}",
                quoted_names(&items, Some(NAMES_IN_ERROR))
            ))),
            many => Err(AppError::Bad(format!(
                "attachment name {name:?} is ambiguous on {parent} ({} matches: {}). \
                 Pass 'attachmentId'.",
                many.len(),
                many.iter()
                    .map(|a| format!("{} ({} bytes)", attach_str(a, "id"), attach_str(a, "size")))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    /// Metadata for a resolved attachment, reusing the listing row when the id
    /// came from a name lookup — that row already holds every field a per-id
    /// GET would return, so re-fetching it is a wasted round trip.
    pub async fn attachment_meta(
        &self,
        entity: Entity,
        parent: &str,
        r: Resolved,
    ) -> Result<Value> {
        match r.meta {
            Some(v) => Ok(v),
            None => self.attachment_get(entity, parent, &r.id).await,
        }
    }

    pub async fn attachment_upload(
        &self,
        entity: Entity,
        parent: &str,
        name: &str,
        bytes: Vec<u8>,
    ) -> Result<Value> {
        let part = reqwest::multipart::Part::bytes(bytes).file_name(name.to_string());
        let form = reqwest::multipart::Form::new().part(name.to_string(), part);
        let resp = self
            .http
            .post(self.url(&self.attach_root(entity, parent)))
            .query(&[("fields", F_ATTACH)])
            .multipart(form)
            .send()
            .await?;
        self.check(resp).await
    }

    /// Returns `(name, mimeType, bytes)`.
    pub async fn attachment_download(&self, meta: &Value) -> Result<(String, String, Vec<u8>)> {
        let name = meta
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("attachment")
            .to_string();
        let mime = meta
            .get("mimeType")
            .and_then(Value::as_str)
            .unwrap_or("application/octet-stream")
            .to_string();
        let rel = meta
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::Bad("attachment has no url".into()))?;
        let full = if rel.starts_with("http") {
            rel.to_string()
        } else {
            format!("{}{}", self.cfg.base_url, rel)
        };
        Ok((name, mime, self.fetch_file(&full).await?))
    }

    async fn fetch_file(&self, url: &str) -> Result<Vec<u8>> {
        let resp = self.http_files.get(url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(AppError::Api {
                status: status.as_u16(),
                message: "attachment download failed".into(),
            });
        }
        Ok(resp.bytes().await?.to_vec())
    }

    pub async fn attachment_delete(
        &self,
        entity: Entity,
        parent: &str,
        aid: &str,
    ) -> Result<Value> {
        self.delete(&format!("{}/{aid}", self.attach_root(entity, parent)))
            .await?;
        // Echo the id YouTrack actually addressed, not the shorthand we were
        // handed — a caller passing "435" should see "Culture-435" back.
        Ok(json!({
            "deleted": true,
            "parentId": self.entity_id(entity, parent),
            "attachmentId": aid,
        }))
    }

    pub fn b64_decode(s: &str) -> Result<Vec<u8>> {
        base64::engine::general_purpose::STANDARD
            .decode(s)
            .map_err(|e| AppError::Bad(format!("bad base64: {e}")))
    }

    pub fn b64_encode(b: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_detection() {
        assert!(is_id("2-42310"));
        assert!(is_id("0-92"));
        assert!(!is_id("Culture-510"));
        assert!(!is_id("123"));
        assert!(!is_id("a-b"));
    }

    #[test]
    fn name_or_id_eq_folds_case_trim_and_non_latin() {
        let node = json!({"name": "ЦРППО", "id": "98-53"});
        assert!(name_or_id_eq(&node, "црппо"));
        assert!(name_or_id_eq(&node, "  ЦРппО "));
        assert!(name_or_id_eq(&node, "98-53"));
        assert!(!name_or_id_eq(&node, "other"));
    }

    #[test]
    fn quoted_names_quotes_and_joins() {
        let nodes = vec![
            json!({"name": "Первый спринт"}),
            json!({"name": "2 Спринт"}),
        ];
        assert_eq!(
            quoted_names(&nodes, None),
            "\"Первый спринт\", \"2 Спринт\""
        );
    }

    #[test]
    fn quoted_names_empty_is_none() {
        assert_eq!(quoted_names(&[], None), "none");
    }

    #[test]
    fn b64_roundtrip() {
        let data = b"hello attach";
        let enc = YouTrack::b64_encode(data);
        assert_eq!(YouTrack::b64_decode(&enc).unwrap(), data);
    }

    fn attach(id: &str, name: &str) -> Value {
        json!({"id": id, "name": name, "size": 10})
    }

    #[test]
    fn match_by_name_finds_single_exact() {
        let items = vec![attach("7-1", "a.png"), attach("7-2", "b.png")];
        let hits = match_by_name(&items, "b.png");
        assert_eq!(
            hits.iter().map(|h| attach_str(h, "id")).collect::<Vec<_>>(),
            ["7-2"]
        );
    }

    #[test]
    fn match_by_name_falls_back_to_case_and_trim() {
        let items = vec![attach("7-1", "Иерархия.txt")];
        let hits = match_by_name(&items, "  иерархия.TXT ");
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn match_by_name_prefers_exact_over_case_variant() {
        let items = vec![attach("7-1", "a.PNG"), attach("7-2", "a.png")];
        let hits = match_by_name(&items, "a.png");
        assert_eq!(
            hits.iter().map(|h| attach_str(h, "id")).collect::<Vec<_>>(),
            ["7-2"]
        );
    }

    #[test]
    fn match_by_name_reports_every_duplicate() {
        // Real case: Culture-435 carries "Иерархия 2.0.txt" twice.
        let items = vec![attach("7-1", "dup.txt"), attach("7-2", "dup.txt")];
        assert_eq!(match_by_name(&items, "dup.txt").len(), 2);
    }

    #[test]
    fn match_by_name_empty_on_miss() {
        let items = vec![attach("7-1", "a.png")];
        assert!(match_by_name(&items, "zzz.png").is_empty());
    }

    #[test]
    fn quoted_names_lists_all_when_under_cap() {
        let items = vec![attach("7-1", "a.png"), attach("7-2", "b.png")];
        assert_eq!(
            quoted_names(&items, Some(NAMES_IN_ERROR)),
            "\"a.png\", \"b.png\""
        );
    }

    #[test]
    fn quoted_names_states_how_many_it_cut() {
        let items: Vec<Value> = (0..45)
            .map(|i| attach(&format!("7-{i}"), &format!("f{i}.png")))
            .collect();
        assert!(quoted_names(&items, Some(NAMES_IN_ERROR)).ends_with("… and 5 more"));
    }

    #[test]
    fn quoted_names_empty_is_none_when_capped() {
        assert_eq!(quoted_names(&[], Some(NAMES_IN_ERROR)), "none");
    }

    #[test]
    fn strip_url_drops_only_the_signed_link() {
        let mut v = json!({"id": "7-1", "url": "/api/files/7-1?sign=secret"});
        strip_url(&mut v);
        assert_eq!(v, json!({"id": "7-1"}));
    }

    #[test]
    fn strip_url_reaches_into_an_array() {
        // An upload answers with an array of created attachments.
        let mut v = json!([{"id": "7-1", "url": "?sign=a"}, {"id": "7-2", "url": "?sign=b"}]);
        strip_url(&mut v);
        assert_eq!(v, json!([{"id": "7-1"}, {"id": "7-2"}]));
    }

    #[test]
    fn attach_str_stringifies_numbers() {
        assert_eq!(attach_str(&json!({"size": 42}), "size"), "42");
        assert_eq!(attach_str(&json!({}), "size"), "?");
    }

    fn yt(default_project: Option<&str>) -> Arc<YouTrack> {
        YouTrack::new(Config {
            base_url: "https://yt.example".into(),
            token: "t".into(),
            timezone: chrono_tz::Europe::Moscow,
            default_project: default_project.map(String::from),
            holidays: Default::default(),
            pre_holidays: Default::default(),
            user_aliases: Default::default(),
            download_dir: None,
        })
        .expect("client builds")
    }

    fn test_client(address: std::net::SocketAddr) -> Arc<YouTrack> {
        YouTrack::new(Config {
            base_url: format!("http://{address}"),
            token: "t".into(),
            timezone: chrono_tz::Europe::Moscow,
            default_project: None,
            holidays: Default::default(),
            pre_holidays: Default::default(),
            user_aliases: Default::default(),
            download_dir: None,
        })
        .expect("client builds")
    }

    fn read_request(stream: &mut std::net::TcpStream) -> (String, Option<Value>) {
        use std::io::Read;

        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        loop {
            let read = stream.read(&mut buffer).unwrap();
            assert_ne!(read, 0, "request ended before its body");
            request.extend_from_slice(&buffer[..read]);

            let Some(header_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
                continue;
            };
            let headers = std::str::from_utf8(&request[..header_end]).unwrap();
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|value| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            let body_start = header_end + 4;
            if request.len() < body_start + content_length {
                continue;
            }

            let body = (content_length > 0).then(|| {
                serde_json::from_slice(&request[body_start..body_start + content_length]).unwrap()
            });
            return (headers.lines().next().unwrap().to_string(), body);
        }
    }

    fn respond_json(stream: &mut std::net::TcpStream, body: &Value) {
        use std::io::Write;

        let body = serde_json::to_vec(body).unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(&body).unwrap();
    }

    fn capture_issue_post() -> (
        Arc<YouTrack>,
        std::sync::mpsc::Receiver<Value>,
        std::thread::JoinHandle<()>,
    ) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (body_tx, body_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let (_, body) = read_request(&mut stream);
            body_tx.send(body.unwrap()).unwrap();
            respond_json(&mut stream, &json!({}));
        });

        (test_client(address), body_rx, server)
    }

    fn capture_custom_fields_post(
        available: Value,
    ) -> (
        Arc<YouTrack>,
        std::sync::mpsc::Receiver<Value>,
        std::thread::JoinHandle<()>,
    ) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (body_tx, body_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let (request_line, body) = read_request(&mut stream);
            assert!(request_line.starts_with("GET /api/issues/ABC-1?"));
            assert!(body.is_none());
            respond_json(&mut stream, &json!({"customFields": available}));

            let (mut stream, _) = listener.accept().unwrap();
            let (request_line, body) = read_request(&mut stream);
            assert!(request_line.starts_with("POST /api/issues/ABC-1 "));
            body_tx.send(body.unwrap()).unwrap();
            respond_json(&mut stream, &json!({}));
        });

        (test_client(address), body_rx, server)
    }

    #[tokio::test]
    async fn generated_api_operation_executes_typed_request() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let (request_line, body) = read_request(&mut stream);
            assert!(request_line.starts_with("POST /api/widgets/ABC%2D1?fields=id%2Cname HTTP/1.1"));
            assert_eq!(body, Some(json!({"name":"updated"})));
            respond_json(&mut stream, &json!({"id":"ABC-1","name":"updated"}));
        });
        let spec = json!({
            "openapi":"3.0.1",
            "paths":{
                "/widgets/{id}":{
                    "post":{
                        "parameters":[
                            {"name":"id","in":"path","required":true,"schema":{"type":"string"}},
                            {"name":"fields","in":"query","schema":{"type":"string"}}
                        ],
                        "requestBody":{"required":true,"content":{"application/json":{
                            "schema":{"type":"object","properties":{"name":{"type":"string"}}}
                        }}},
                        "responses":{"200":{"content":{"application/json":{
                            "schema":{"type":"object"}
                        }}}}
                    }
                }
            }
        });
        let operation = crate::openapi::generate(&spec).unwrap().remove(0);
        let args = serde_json::from_value(json!({
            "id":"ABC-1",
            "fields":"id,name",
            "body":{"name":"updated"}
        }))
        .unwrap();

        let response = test_client(address)
            .execute_api_operation(&operation, args)
            .await
            .unwrap();
        assert_eq!(response.status, 200);
        match response.body {
            ApiResponseBody::Json(body) => {
                assert_eq!(body, json!({"id":"ABC-1","name":"updated"}));
            }
            body => panic!("expected JSON response, got {body:?}"),
        }
        server.join().unwrap();
    }

    #[tokio::test]
    async fn generated_get_follows_binary_download_redirects() {
        use std::io::Write;

        let api_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let api_address = api_listener.local_addr().unwrap();
        let file_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let file_address = file_listener.local_addr().unwrap();
        let api_server = std::thread::spawn(move || {
            let (mut stream, _) = api_listener.accept().unwrap();
            let (request_line, body) = read_request(&mut stream);
            assert_eq!(request_line, "GET /api/download HTTP/1.1");
            assert!(body.is_none());
            write!(
                stream,
                "HTTP/1.1 302 Found\r\nLocation: http://{file_address}/artifact\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        });
        let file_server = std::thread::spawn(move || {
            let (mut stream, _) = file_listener.accept().unwrap();
            let (request_line, body) = read_request(&mut stream);
            assert_eq!(request_line, "GET /artifact HTTP/1.1");
            assert!(body.is_none());
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: 8\r\nConnection: close\r\n\r\nartifact",
                )
                .unwrap();
        });
        let spec = json!({
            "openapi":"3.0.1",
            "paths":{"/download":{"get":{"responses":{"200":{"content":{
                "application/octet-stream":{"schema":{"type":"string","format":"binary"}}
            }}}}}}
        });
        let operation = crate::openapi::generate(&spec).unwrap().remove(0);

        let response = test_client(api_address)
            .execute_api_operation(&operation, Default::default())
            .await
            .unwrap();
        assert_eq!(response.status, 200);
        assert!(matches!(
            response.body,
            ApiResponseBody::Binary(base64)
                if base64 == YouTrack::b64_encode(b"artifact")
        ));
        api_server.join().unwrap();
        file_server.join().unwrap();
    }

    #[tokio::test]
    async fn null_assignee_posts_a_custom_field_clear() {
        let (client, body_rx, server) = capture_issue_post();
        let args: crate::model::IssueWrite =
            serde_json::from_value(json!({"op":"update","id":"ABC-1","assignee":null})).unwrap();

        assert!(client
            .apply_side_effects("ABC-1", "ABC-1", &args)
            .await
            .unwrap());
        assert_eq!(
            body_rx.recv().unwrap(),
            json!({"customFields":[{
                "name":"Assignee",
                "$type":"SingleUserIssueCustomField",
                "value":null
            }]})
        );
        server.join().unwrap();
    }

    #[tokio::test]
    async fn arbitrary_custom_fields_resolve_types_and_post_values() {
        let (client, body_rx, server) = capture_custom_fields_post(json!([
            {"id":"92-1","name":"Type","$type":"SingleEnumIssueCustomField"},
            {"id":"92-2","name":"Story points","$type":"SimpleIssueCustomField"},
            {"id":"92-3","name":"Reviewers","$type":"MultiUserIssueCustomField"},
            {"id":"92-4","name":"Estimate","$type":"PeriodIssueCustomField"}
        ]));
        let args: crate::model::IssueWrite = serde_json::from_value(json!({
            "op":"update",
            "id":"ABC-1",
            "customFields":[
                {"name":"Type","value":"Bug"},
                {"name":"Story points","value":8},
                {"name":"Reviewers","value":["alice","bob"]},
                {"name":"Estimate","value":{"minutes":90}}
            ]
        }))
        .unwrap();

        assert!(client
            .apply_side_effects("ABC-1", "ABC-1", &args)
            .await
            .unwrap());
        assert_eq!(
            body_rx.recv().unwrap(),
            json!({"customFields":[
                {
                    "id":"92-1",
                    "$type":"SingleEnumIssueCustomField",
                    "value":{"name":"Bug"}
                },
                {
                    "id":"92-2",
                    "$type":"SimpleIssueCustomField",
                    "value":8
                },
                {
                    "id":"92-3",
                    "$type":"MultiUserIssueCustomField",
                    "value":[{"login":"alice"},{"login":"bob"}]
                },
                {
                    "id":"92-4",
                    "$type":"PeriodIssueCustomField",
                    "value":{"minutes":90}
                }
            ]})
        );
        server.join().unwrap();
    }

    #[test]
    fn attach_root_routes_issues_and_expands_bare_ids() {
        assert_eq!(
            yt(Some("Culture")).attach_root(Entity::Issue, "435"),
            "/api/issues/Culture-435/attachments"
        );
    }

    #[test]
    fn attach_root_routes_articles_verbatim() {
        assert_eq!(
            yt(None).attach_root(Entity::Article, "Culture-A-81"),
            "/api/articles/Culture-A-81/attachments"
        );
    }

    #[test]
    fn entity_id_is_what_responses_should_echo() {
        // A caller passing "435" gets the resolved id back, not its shorthand.
        assert_eq!(
            yt(Some("Culture")).entity_id(Entity::Issue, "435"),
            "Culture-435"
        );
        assert_eq!(
            yt(None).entity_id(Entity::Article, "Culture-A-81"),
            "Culture-A-81"
        );
    }
}
