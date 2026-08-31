use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use anyhow::{bail, Context};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use reqwest::Method;
use rmcp::model::{CallToolResult, JsonObject, Tool, ToolAnnotations};
use serde_json::{json, Map, Value};

const HTTP_METHODS: [(&str, Method); 8] = [
    ("get", Method::GET),
    ("post", Method::POST),
    ("put", Method::PUT),
    ("patch", Method::PATCH),
    ("delete", Method::DELETE),
    ("head", Method::HEAD),
    ("options", Method::OPTIONS),
    ("trace", Method::TRACE),
];
const MAX_TOOL_NAME: usize = 96;
const MAX_DESCRIPTION_CHARS: usize = 1200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParameterLocation {
    Path,
    Query,
    Header,
    Cookie,
}

#[derive(Debug, Clone)]
pub(crate) struct ApiParameter {
    pub arg_name: String,
    pub wire_name: String,
    pub location: ParameterLocation,
    pub required: bool,
    pub style: String,
    pub explode: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BodyKind {
    Json,
    Form,
    Multipart,
    Binary,
    Text,
}

#[derive(Debug, Clone)]
pub(crate) struct ApiRequestBody {
    pub arg_name: String,
    pub required: bool,
    pub content_type: String,
    pub kind: BodyKind,
    pub binary_fields: HashSet<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ApiOperation {
    pub tool: Tool,
    pub method: Method,
    pub path: String,
    pub parameters: Vec<ApiParameter>,
    pub request_body: Option<ApiRequestBody>,
}

#[derive(Debug)]
pub(crate) enum PreparedBody {
    Json(Value),
    Form(Vec<(String, String)>),
    Multipart {
        fields: Map<String, Value>,
        binary_fields: HashSet<String>,
    },
    Binary(Vec<u8>),
    Text(String),
}

#[derive(Debug)]
pub(crate) struct PreparedRequest {
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub content_type: Option<String>,
    pub body: Option<PreparedBody>,
}

#[derive(Debug)]
pub(crate) enum ApiResponseBody {
    Json(Value),
    Text(String),
    Binary(String),
    Empty,
}

#[derive(Debug)]
pub(crate) struct ApiResponse {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: ApiResponseBody,
}

impl ApiResponse {
    pub fn into_tool_result(self) -> CallToolResult {
        let success = (200..300).contains(&self.status);
        if success {
            match self.body {
                ApiResponseBody::Json(body) => CallToolResult::structured(body),
                ApiResponseBody::Empty => {
                    CallToolResult::structured(json!({"status": self.status}))
                }
                ApiResponseBody::Text(body) => CallToolResult::structured(json!({
                    "status": self.status,
                    "contentType": self.content_type,
                    "body": body,
                })),
                ApiResponseBody::Binary(base64) => CallToolResult::structured(json!({
                    "status": self.status,
                    "contentType": self.content_type,
                    "base64": base64,
                })),
            }
        } else {
            let body = match self.body {
                ApiResponseBody::Json(body) => body,
                ApiResponseBody::Text(body) => Value::String(body),
                ApiResponseBody::Binary(base64) => json!({"base64": base64}),
                ApiResponseBody::Empty => Value::Null,
            };
            CallToolResult::structured_error(json!({
                "status": self.status,
                "contentType": self.content_type,
                "body": body,
            }))
        }
    }
}

impl ApiOperation {
    pub fn prepare(&self, mut args: JsonObject) -> Result<PreparedRequest, String> {
        let mut path = if self.path == "/api" || self.path.starts_with("/api/") {
            self.path.clone()
        } else {
            format!("/api{}", self.path)
        };
        let mut query = Vec::new();
        let mut headers = Vec::new();

        for parameter in &self.parameters {
            let value = args.remove(&parameter.arg_name);
            let Some(value) = value.filter(|value| !value.is_null()) else {
                if parameter.required {
                    return Err(format!(
                        "missing required parameter {:?}",
                        parameter.arg_name
                    ));
                }
                continue;
            };

            match parameter.location {
                ParameterLocation::Path => {
                    let rendered = render_compound(&value, &parameter.style, parameter.explode);
                    let encoded = utf8_percent_encode(&rendered, NON_ALPHANUMERIC).to_string();
                    let token = format!("{{{}}}", parameter.wire_name);
                    if !path.contains(&token) {
                        return Err(format!(
                            "path parameter {:?} has no placeholder in {}",
                            parameter.wire_name, self.path
                        ));
                    }
                    path = path.replace(&token, &encoded);
                }
                ParameterLocation::Query => serialize_query_parameter(
                    &mut query,
                    &parameter.wire_name,
                    &value,
                    &parameter.style,
                    parameter.explode,
                ),
                ParameterLocation::Header => headers.push((
                    parameter.wire_name.clone(),
                    render_compound(&value, &parameter.style, parameter.explode),
                )),
                ParameterLocation::Cookie => {
                    let rendered = render_compound(&value, &parameter.style, parameter.explode);
                    headers.push((
                        "Cookie".into(),
                        format!("{}={rendered}", parameter.wire_name),
                    ));
                }
            }
        }

        let (content_type, body) = if let Some(request_body) = &self.request_body {
            let value = args.remove(&request_body.arg_name);
            let Some(value) = value.filter(|value| !value.is_null()) else {
                if request_body.required {
                    return Err(format!(
                        "missing required request body {:?}",
                        request_body.arg_name
                    ));
                }
                if !args.is_empty() {
                    return Err(unknown_arguments(&args));
                }
                return Ok(PreparedRequest {
                    path,
                    query,
                    headers,
                    content_type: None,
                    body: None,
                });
            };
            let body = match request_body.kind {
                BodyKind::Json => PreparedBody::Json(value),
                BodyKind::Form => PreparedBody::Form(form_fields(&value)?),
                BodyKind::Multipart => PreparedBody::Multipart {
                    fields: value
                        .as_object()
                        .cloned()
                        .ok_or_else(|| "multipart body must be an object".to_string())?,
                    binary_fields: request_body.binary_fields.clone(),
                },
                BodyKind::Binary => {
                    let encoded = value
                        .as_str()
                        .ok_or_else(|| "binary body must be a base64 string".to_string())?;
                    PreparedBody::Binary(
                        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded)
                            .map_err(|error| format!("binary body is not valid base64: {error}"))?,
                    )
                }
                BodyKind::Text => PreparedBody::Text(
                    value
                        .as_str()
                        .ok_or_else(|| "text body must be a string".to_string())?
                        .to_string(),
                ),
            };
            (Some(request_body.content_type.clone()), Some(body))
        } else {
            (None, None)
        };

        if !args.is_empty() {
            return Err(unknown_arguments(&args));
        }
        Ok(PreparedRequest {
            path,
            query,
            headers,
            content_type,
            body,
        })
    }
}

fn unknown_arguments(args: &JsonObject) -> String {
    let mut names: Vec<_> = args.keys().cloned().collect();
    names.sort();
    format!("unknown arguments: {}", names.join(", "))
}

fn scalar(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Null => String::new(),
        Value::Bool(_) | Value::Number(_) => value.to_string(),
        Value::Array(_) | Value::Object(_) => value.to_string(),
    }
}

fn render_compound(value: &Value, style: &str, explode: bool) -> String {
    match value {
        Value::Array(values) => {
            let delimiter = match style {
                "spaceDelimited" => " ",
                "pipeDelimited" => "|",
                _ => ",",
            };
            values
                .iter()
                .map(scalar)
                .collect::<Vec<_>>()
                .join(delimiter)
        }
        Value::Object(values) => {
            let delimiter = if explode { "," } else { "," };
            values
                .iter()
                .flat_map(|(key, value)| {
                    if explode {
                        vec![format!("{key}={}", scalar(value))]
                    } else {
                        vec![key.clone(), scalar(value)]
                    }
                })
                .collect::<Vec<_>>()
                .join(delimiter)
        }
        _ => scalar(value),
    }
}

fn serialize_query_parameter(
    output: &mut Vec<(String, String)>,
    name: &str,
    value: &Value,
    style: &str,
    explode: bool,
) {
    match value {
        Value::Array(values) if explode => {
            output.extend(values.iter().map(|value| (name.to_string(), scalar(value))));
        }
        Value::Object(values) if style == "deepObject" => {
            output.extend(
                values
                    .iter()
                    .map(|(key, value)| (format!("{name}[{key}]"), scalar(value))),
            );
        }
        Value::Object(values) if explode => {
            output.extend(
                values
                    .iter()
                    .map(|(key, value)| (key.clone(), scalar(value))),
            );
        }
        _ => output.push((name.to_string(), render_compound(value, style, explode))),
    }
}

fn form_fields(value: &Value) -> Result<Vec<(String, String)>, String> {
    let fields = value
        .as_object()
        .ok_or_else(|| "form body must be an object".to_string())?;
    let mut output = Vec::new();
    for (name, value) in fields {
        match value {
            Value::Array(values) => {
                output.extend(values.iter().map(|value| (name.clone(), scalar(value))))
            }
            _ => output.push((name.clone(), scalar(value))),
        }
    }
    Ok(output)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SchemaDirection {
    Input,
    Output,
}

struct SchemaCollector<'a> {
    source: Option<&'a Map<String, Value>>,
    defs: Map<String, Value>,
    visiting: HashSet<String>,
    direction: SchemaDirection,
}

impl<'a> SchemaCollector<'a> {
    fn new(spec: &'a Value, direction: SchemaDirection) -> Self {
        Self {
            source: spec
                .pointer("/components/schemas")
                .and_then(Value::as_object),
            defs: Map::new(),
            visiting: HashSet::new(),
            direction,
        }
    }

    fn root(mut self, schema: &Value) -> Value {
        let mut root = self.convert(schema);
        if let Some(object) = root.as_object_mut() {
            object.insert(
                "$schema".into(),
                Value::String("https://json-schema.org/draft/2020-12/schema".into()),
            );
            self.attach_defs(object);
        }
        root
    }

    fn attach_defs(&mut self, root: &mut Map<String, Value>) {
        if !self.defs.is_empty() {
            root.insert(
                "$defs".into(),
                Value::Object(std::mem::take(&mut self.defs)),
            );
        }
    }

    fn convert(&mut self, schema: &Value) -> Value {
        let Some(source) = schema.as_object() else {
            return schema.clone();
        };

        if let Some(reference) = source.get("$ref").and_then(Value::as_str) {
            if let Some(name) = reference.strip_prefix("#/components/schemas/") {
                self.collect_def(name);
                let mut output = Map::new();
                output.insert("$ref".into(), Value::String(format!("#/$defs/{name}")));
                return Value::Object(output);
            }
        }

        let removed_properties: HashSet<&str> = source
            .get("properties")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|properties| properties.iter())
            .filter_map(|(name, property)| {
                let property = property.as_object()?;
                let remove = match self.direction {
                    SchemaDirection::Input => property
                        .get("readOnly")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    SchemaDirection::Output => property
                        .get("writeOnly")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                };
                remove.then_some(name.as_str())
            })
            .collect();

        let mut output = Map::new();
        for (key, value) in source {
            if matches!(
                key.as_str(),
                "nullable"
                    | "discriminator"
                    | "xml"
                    | "externalDocs"
                    | "example"
                    | "deprecated"
                    | "readOnly"
                    | "writeOnly"
            ) {
                continue;
            }
            let converted = match key.as_str() {
                "properties" => Value::Object(
                    value
                        .as_object()
                        .into_iter()
                        .flat_map(|properties| properties.iter())
                        .filter(|(name, _)| !removed_properties.contains(name.as_str()))
                        .map(|(name, schema)| (name.clone(), self.convert(schema)))
                        .collect(),
                ),
                "required" => Value::Array(
                    value
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|name| {
                            name.as_str()
                                .is_none_or(|name| !removed_properties.contains(name))
                        })
                        .cloned()
                        .collect(),
                ),
                "items" | "additionalProperties" | "not" => self.convert(value),
                "allOf" | "anyOf" | "oneOf" | "prefixItems" => Value::Array(
                    value
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|schema| self.convert(schema))
                        .collect(),
                ),
                _ => value.clone(),
            };
            output.insert(key.clone(), converted);
        }

        if source
            .get("nullable")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            if let Some(Value::String(kind)) = output.get("type") {
                output.insert(
                    "type".into(),
                    Value::Array(vec![
                        Value::String(kind.clone()),
                        Value::String("null".into()),
                    ]),
                );
            } else {
                return json!({"anyOf":[Value::Object(output), {"type":"null"}]});
            }
        }
        if matches!(
            output.get("format").and_then(Value::as_str),
            Some("binary" | "byte")
        ) {
            output.insert("contentEncoding".into(), Value::String("base64".into()));
            output.insert(
                "description".into(),
                Value::String(append_description(
                    output.get("description").and_then(Value::as_str),
                    "Supply bytes as base64.",
                )),
            );
        }
        Value::Object(output)
    }

    fn collect_def(&mut self, name: &str) {
        if self.defs.contains_key(name) || !self.visiting.insert(name.to_string()) {
            return;
        }
        let Some(schema) = self.source.and_then(|source| source.get(name)) else {
            self.visiting.remove(name);
            return;
        };
        let converted = self.convert(schema);
        self.defs.insert(name.to_string(), converted);
        self.visiting.remove(name);
    }
}

pub(crate) fn generate(spec: &Value) -> anyhow::Result<Vec<ApiOperation>> {
    let paths = spec
        .get("paths")
        .and_then(Value::as_object)
        .context("OpenAPI document has no paths object")?;
    let mut operations = Vec::new();
    let mut names = HashSet::new();

    for (path, raw_path_item) in paths {
        let resolved_path_item = resolve_reference(spec, raw_path_item, "pathItems")?;
        let path_item = resolved_path_item
            .as_object()
            .with_context(|| format!("OpenAPI path {path:?} is not an object"))?;
        for (method_name, method) in HTTP_METHODS.iter() {
            let Some(operation) = path_item.get(*method_name) else {
                continue;
            };
            let operation = operation
                .as_object()
                .with_context(|| format!("{method_name} {path} is not an object"))?;
            operations.push(build_operation(
                spec,
                path,
                path_item,
                method_name,
                method.clone(),
                operation,
                &mut names,
            )?);
        }
    }
    if operations.is_empty() {
        bail!("OpenAPI document contains no HTTP operations");
    }
    operations.sort_by(|left, right| left.tool.name.cmp(&right.tool.name));
    Ok(operations)
}

fn build_operation(
    spec: &Value,
    path: &str,
    path_item: &Map<String, Value>,
    method_name: &str,
    method: Method,
    operation: &Map<String, Value>,
    names: &mut HashSet<String>,
) -> anyhow::Result<ApiOperation> {
    let mut input_properties = Map::new();
    let mut required = Vec::new();
    let mut schema_collector = SchemaCollector::new(spec, SchemaDirection::Input);
    let mut parameters = Vec::new();
    let mut raw_parameters = Vec::new();
    if let Some(path_parameters) = path_item.get("parameters").and_then(Value::as_array) {
        raw_parameters.extend(path_parameters.iter().cloned());
    }
    if let Some(operation_parameters) = operation.get("parameters").and_then(Value::as_array) {
        raw_parameters.extend(operation_parameters.iter().cloned());
    }

    let mut merged_parameters = Vec::new();
    let mut parameter_positions = HashMap::new();
    for raw_parameter in raw_parameters {
        let parameter = resolve_reference(spec, &raw_parameter, "parameters")?;
        let object = parameter
            .as_object()
            .with_context(|| format!("parameter on {method_name} {path} is not an object"))?;
        let key = (
            object
                .get("in")
                .and_then(Value::as_str)
                .context("OpenAPI parameter has no location")?
                .to_string(),
            object
                .get("name")
                .and_then(Value::as_str)
                .context("OpenAPI parameter has no name")?
                .to_string(),
        );
        if let Some(index) = parameter_positions.get(&key).copied() {
            merged_parameters[index] = parameter;
        } else {
            parameter_positions.insert(key, merged_parameters.len());
            merged_parameters.push(parameter);
        }
    }

    for parameter in merged_parameters {
        let parameter = parameter
            .as_object()
            .with_context(|| format!("parameter on {method_name} {path} is not an object"))?;
        let wire_name = parameter
            .get("name")
            .and_then(Value::as_str)
            .context("OpenAPI parameter has no name")?
            .to_string();
        let location_name = parameter
            .get("in")
            .and_then(Value::as_str)
            .context("OpenAPI parameter has no location")?;
        let location = match location_name {
            "path" => ParameterLocation::Path,
            "query" => ParameterLocation::Query,
            "header" => ParameterLocation::Header,
            "cookie" => ParameterLocation::Cookie,
            other => bail!("unsupported OpenAPI parameter location {other:?}"),
        };
        let mut arg_name = wire_name.clone();
        if input_properties.contains_key(&arg_name) {
            arg_name = format!("{location_name}_{wire_name}");
        }
        let required_parameter = location == ParameterLocation::Path
            || parameter
                .get("required")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        let style = parameter
            .get("style")
            .and_then(Value::as_str)
            .unwrap_or(match location {
                ParameterLocation::Path | ParameterLocation::Header => "simple",
                ParameterLocation::Query | ParameterLocation::Cookie => "form",
            })
            .to_string();
        let explode = parameter
            .get("explode")
            .and_then(Value::as_bool)
            .unwrap_or(style == "form");
        let raw_schema = parameter_schema(parameter).unwrap_or_else(|| json!({"type":"string"}));
        let mut schema = schema_collector.convert(&raw_schema);
        if let Some(schema) = schema.as_object_mut() {
            let description = parameter.get("description").and_then(Value::as_str);
            if let Some(description) = description.filter(|description| !description.is_empty()) {
                schema.insert("description".into(), Value::String(compact(description)));
            }
        }
        input_properties.insert(arg_name.clone(), schema);
        if required_parameter {
            required.push(Value::String(arg_name.clone()));
        }
        parameters.push(ApiParameter {
            arg_name,
            wire_name,
            location,
            required: required_parameter,
            style,
            explode,
        });
    }

    let mut request_body = None;
    if let Some(raw_request_body) = operation.get("requestBody") {
        let resolved = resolve_reference(spec, raw_request_body, "requestBodies")?;
        let body = resolved
            .as_object()
            .context("OpenAPI requestBody is not an object")?;
        let content = body
            .get("content")
            .and_then(Value::as_object)
            .context("OpenAPI requestBody has no content")?;
        let (content_type, media) = select_request_media(content)
            .context("OpenAPI requestBody has no supported media type")?;
        let raw_schema = media.get("schema").cloned().unwrap_or_else(|| json!({}));
        let mut schema = schema_collector.convert(&raw_schema);
        if let Some(schema) = schema.as_object_mut() {
            let description = body.get("description").and_then(Value::as_str);
            if let Some(description) = description.filter(|description| !description.is_empty()) {
                schema.insert("description".into(), Value::String(compact(description)));
            }
        }
        let mut arg_name = "body".to_string();
        if input_properties.contains_key(&arg_name) {
            arg_name = "requestBody".to_string();
        }
        let required_body = body
            .get("required")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if required_body {
            required.push(Value::String(arg_name.clone()));
        }
        input_properties.insert(arg_name.clone(), schema);
        let kind = body_kind(spec, content_type, &raw_schema);
        request_body = Some(ApiRequestBody {
            arg_name,
            required: required_body,
            content_type: content_type.to_string(),
            binary_fields: multipart_binary_fields(spec, &raw_schema),
            kind,
        });
    }

    let mut input_schema = Map::new();
    input_schema.insert(
        "$schema".into(),
        Value::String("https://json-schema.org/draft/2020-12/schema".into()),
    );
    input_schema.insert("type".into(), Value::String("object".into()));
    input_schema.insert("properties".into(), Value::Object(input_properties));
    input_schema.insert("additionalProperties".into(), Value::Bool(false));
    if !required.is_empty() {
        input_schema.insert("required".into(), Value::Array(required));
    }
    schema_collector.attach_defs(&mut input_schema);

    let name = unique_tool_name(method_name, path, names);
    let summary = operation
        .get("summary")
        .and_then(Value::as_str)
        .or_else(|| operation.get("description").and_then(Value::as_str));
    let description = append_description(
        summary.map(compact).as_deref(),
        &format!("YouTrack REST operation: {} {path}", method.as_str()),
    );
    let read_only = matches!(method, Method::GET | Method::HEAD | Method::OPTIONS);
    let annotations = ToolAnnotations::new()
        .read_only(read_only)
        .destructive(!read_only)
        .idempotent(matches!(
            method,
            Method::GET | Method::HEAD | Method::OPTIONS | Method::PUT | Method::DELETE
        ))
        .open_world(false);
    let mut tool = Tool::new(name, description, input_schema).with_annotations(annotations);
    if let Some(title) = operation
        .get("summary")
        .and_then(Value::as_str)
        .filter(|title| !title.is_empty())
    {
        tool = tool.with_title(compact(title));
    }
    if let Some(output_schema) = response_schema(spec, operation) {
        if let Some(output_schema) = output_schema.as_object().cloned() {
            tool = tool.with_raw_output_schema(Arc::new(output_schema));
        }
    }

    Ok(ApiOperation {
        tool,
        method,
        path: path.to_string(),
        parameters,
        request_body,
    })
}

fn parameter_schema(parameter: &Map<String, Value>) -> Option<Value> {
    if let Some(schema) = parameter.get("schema") {
        return Some(schema.clone());
    }
    parameter
        .get("content")
        .and_then(Value::as_object)
        .and_then(|content| {
            content
                .get("application/json")
                .or_else(|| content.values().next())
        })
        .and_then(|media| media.get("schema"))
        .cloned()
}

fn select_request_media(content: &Map<String, Value>) -> Option<(&str, &Value)> {
    for exact in [
        "application/json",
        "multipart/form-data",
        "application/x-www-form-urlencoded",
        "application/octet-stream",
        "text/plain",
    ] {
        if let Some(media) = content.get(exact) {
            return Some((exact, media));
        }
    }
    content
        .iter()
        .find(|(kind, _)| kind.ends_with("+json"))
        .or_else(|| content.iter().next())
        .map(|(kind, media)| (kind.as_str(), media))
}

fn body_kind(spec: &Value, content_type: &str, schema: &Value) -> BodyKind {
    if content_type == "application/json" || content_type.ends_with("+json") {
        BodyKind::Json
    } else if content_type == "application/x-www-form-urlencoded" {
        BodyKind::Form
    } else if content_type == "multipart/form-data" {
        BodyKind::Multipart
    } else if content_type == "application/octet-stream" || schema_is_binary(spec, schema) {
        BodyKind::Binary
    } else {
        BodyKind::Text
    }
}

fn schema_is_binary(spec: &Value, schema: &Value) -> bool {
    let schema = resolve_schema_reference(spec, schema).unwrap_or(schema);
    matches!(
        schema.get("format").and_then(Value::as_str),
        Some("binary" | "byte")
    )
}

fn response_schema(spec: &Value, operation: &Map<String, Value>) -> Option<Value> {
    let responses = operation.get("responses")?.as_object()?;
    let response = ["200", "201", "202", "203", "204", "2XX"]
        .iter()
        .find_map(|status| responses.get(*status))?;
    let response = resolve_reference(spec, response, "responses").ok()?;
    let Some(content) = response.get("content").and_then(Value::as_object) else {
        return Some(json!({
            "$schema":"https://json-schema.org/draft/2020-12/schema",
            "type":"object",
            "properties":{"status":{"type":"integer"}},
            "required":["status"],
            "additionalProperties":false
        }));
    };
    let (content_type, media) = content
        .get_key_value("application/json")
        .or_else(|| content.iter().find(|(kind, _)| kind.ends_with("+json")))
        .or_else(|| content.iter().next())?;
    let schema = media.get("schema").cloned().unwrap_or_else(|| json!({}));
    if content_type == "application/json" || content_type.ends_with("+json") {
        return Some(SchemaCollector::new(spec, SchemaDirection::Output).root(&schema));
    }

    let field = if schema_is_binary(spec, &schema)
        || !(content_type.starts_with("text/")
            || content_type.contains("xml")
            || content_type.contains("javascript")
            || content_type.contains("x-www-form-urlencoded"))
    {
        "base64"
    } else {
        "body"
    };
    let mut collector = SchemaCollector::new(spec, SchemaDirection::Output);
    let body_schema = collector.convert(&schema);
    let mut properties = Map::new();
    properties.insert("status".into(), json!({"type":"integer"}));
    properties.insert("contentType".into(), json!({"type":["string","null"]}));
    properties.insert(field.into(), body_schema);
    let mut root = Map::from_iter([
        (
            "$schema".into(),
            Value::String("https://json-schema.org/draft/2020-12/schema".into()),
        ),
        ("type".into(), Value::String("object".into())),
        ("properties".into(), Value::Object(properties)),
        ("required".into(), json!(["status", "contentType", field])),
        ("additionalProperties".into(), Value::Bool(false)),
    ]);
    collector.attach_defs(&mut root);
    Some(Value::Object(root))
}

fn resolve_reference(spec: &Value, value: &Value, component: &str) -> anyhow::Result<Value> {
    let Some(reference) = value.get("$ref").and_then(Value::as_str) else {
        return Ok(value.clone());
    };
    let prefix = format!("#/components/{component}/");
    let name = reference
        .strip_prefix(&prefix)
        .with_context(|| format!("unsupported OpenAPI reference {reference:?}"))?;
    spec.pointer(&format!("/components/{component}/{}", escape_pointer(name)))
        .cloned()
        .with_context(|| format!("missing OpenAPI component {component}/{name}"))
}

fn escape_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

fn multipart_binary_fields(spec: &Value, schema: &Value) -> HashSet<String> {
    let schema = resolve_schema_reference(spec, schema).unwrap_or(schema);
    schema
        .get("properties")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|properties| properties.iter())
        .filter_map(|(name, property)| {
            let property = resolve_schema_reference(spec, property).unwrap_or(property);
            let binary = matches!(
                property.get("format").and_then(Value::as_str),
                Some("binary" | "byte")
            ) || property
                .get("items")
                .and_then(|items| resolve_schema_reference(spec, items))
                .and_then(|items| items.get("format"))
                .and_then(Value::as_str)
                .is_some_and(|format| matches!(format, "binary" | "byte"));
            binary.then_some(name.clone())
        })
        .collect()
}

fn resolve_schema_reference<'a>(spec: &'a Value, schema: &'a Value) -> Option<&'a Value> {
    let reference = schema.get("$ref")?.as_str()?;
    let name = reference.strip_prefix("#/components/schemas/")?;
    spec.pointer(&format!("/components/schemas/{}", escape_pointer(name)))
}

fn unique_tool_name(method: &str, path: &str, used: &mut HashSet<String>) -> String {
    let mut base = format!("api_{}", sanitize_name(&format!("{method}_{path}")));
    let hash = operation_hash(method, path);
    if base.len() > MAX_TOOL_NAME {
        base.truncate(MAX_TOOL_NAME - 9);
        base.push('_');
        base.push_str(&hash);
    }
    if used.insert(base.clone()) {
        return base;
    }
    let suffix = format!("_{hash}");
    base.truncate(MAX_TOOL_NAME.saturating_sub(suffix.len()));
    base.push_str(&suffix);
    used.insert(base.clone());
    base
}

fn sanitize_name(source: &str) -> String {
    let mut output = String::new();
    let mut previous_lower_or_digit = false;
    for character in source.chars() {
        if character.is_ascii_alphanumeric() {
            if character.is_ascii_uppercase() && previous_lower_or_digit && !output.ends_with('_') {
                output.push('_');
            }
            output.push(character.to_ascii_lowercase());
            previous_lower_or_digit = character.is_ascii_lowercase() || character.is_ascii_digit();
        } else {
            if !output.is_empty() && !output.ends_with('_') {
                output.push('_');
            }
            previous_lower_or_digit = false;
        }
    }
    output.trim_matches('_').to_string()
}

fn operation_hash(method: &str, path: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    method.hash(&mut hasher);
    path.hash(&mut hasher);
    format!("{:08x}", hasher.finish() as u32)
}

fn append_description(existing: Option<&str>, extra: &str) -> String {
    match existing.filter(|description| !description.trim().is_empty()) {
        Some(existing) => format!("{} {}", existing.trim(), extra.trim()),
        None => extra.trim().to_string(),
    }
}

fn compact(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_DESCRIPTION_CHARS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Value {
        json!({
            "openapi":"3.0.1",
            "paths":{
                "/issues/{issueID}":{
                    "parameters":[{
                        "name":"issueID","in":"path","required":true,
                        "schema":{"type":"string"}
                    }],
                    "get":{
                        "operationId":"fetchIssueUsingInternalName",
                        "summary":"Get an issue",
                        "parameters":[{
                            "name":"fields","in":"query","schema":{"type":"string"}
                        }],
                        "responses":{"200":{"content":{"application/json":{"schema":{
                            "$ref":"#/components/schemas/Issue"
                        }}}}}
                    },
                    "post":{
                        "summary":"Update an issue",
                        "requestBody":{"required":true,"content":{"application/json":{"schema":{
                            "$ref":"#/components/schemas/IssueUpdate"
                        }}}},
                        "responses":{"200":{"content":{"application/json":{"schema":{
                            "$ref":"#/components/schemas/Issue"
                        }}}}}
                    },
                    "delete":{"responses":{"204":{"description":"Deleted"}}}
                },
                "/files":{
                    "post":{
                        "requestBody":{"required":true,"content":{"multipart/form-data":{"schema":{
                            "type":"object","properties":{
                                "file":{"type":"string","format":"binary"},
                                "name":{"type":"string"}
                            },"required":["file"]
                        }}}},
                        "responses":{"200":{"content":{"application/json":{"schema":{"type":"object"}}}}}
                    }
                }
            },
            "components":{"schemas":{
                "Issue":{"type":"object","properties":{
                    "id":{"type":"string","readOnly":true},
                    "summary":{"type":"string"},
                    "secret":{"type":"string","writeOnly":true}
                }},
                "IssueUpdate":{"type":"object","properties":{
                    "summary":{"type":"string"},
                    "serverOnly":{"type":"string","readOnly":true}
                },"required":["summary","serverOnly"]}
            }}
        })
    }

    #[test]
    fn generates_every_http_operation() {
        let operations = generate(&fixture()).unwrap();
        assert_eq!(operations.len(), 4);
        assert!(operations
            .iter()
            .any(|operation| operation.method == Method::GET));
        assert!(operations
            .iter()
            .any(|operation| operation.method == Method::DELETE));
        assert!(operations
            .iter()
            .all(|operation| operation.tool.name.starts_with("api_")));
        assert!(operations
            .iter()
            .any(|operation| operation.tool.name == "api_get_issues_issue_id"));
        let delete = operations
            .iter()
            .find(|operation| operation.method == Method::DELETE)
            .unwrap();
        let delete_tool = serde_json::to_value(&delete.tool).unwrap();
        assert_eq!(
            delete_tool.pointer("/outputSchema/properties/status/type"),
            Some(&json!("integer"))
        );
    }

    #[test]
    fn includes_all_openapi_http_methods() {
        let path_item = HTTP_METHODS
            .iter()
            .map(|(method, _)| {
                (
                    (*method).to_string(),
                    json!({"responses":{"204":{"description":"done"}}}),
                )
            })
            .collect::<Map<_, _>>();
        let spec = json!({
            "openapi":"3.0.1",
            "paths":{"/anything":Value::Object(path_item)}
        });

        let operations = generate(&spec).unwrap();
        assert_eq!(operations.len(), HTTP_METHODS.len());
        for (method, _) in HTTP_METHODS {
            assert!(operations
                .iter()
                .any(|operation| { operation.method.as_str().eq_ignore_ascii_case(method) }));
        }
    }

    #[test]
    fn operation_parameters_override_path_parameters_cleanly() {
        let spec = json!({
            "openapi":"3.0.1",
            "paths":{"/items":{
                "parameters":[{
                    "name":"fields","in":"query","schema":{"type":"string"}
                }],
                "get":{
                    "parameters":[{
                        "name":"fields","in":"query","required":true,
                        "schema":{"type":"integer"}
                    }],
                    "responses":{"204":{"description":"done"}}
                }
            }}
        });

        let operation = generate(&spec).unwrap().remove(0);
        let schema = operation.tool.schema_as_json_value();
        assert_eq!(operation.parameters.len(), 1);
        assert_eq!(
            schema.pointer("/properties/fields/type"),
            Some(&json!("integer"))
        );
        assert_eq!(schema.pointer("/required"), Some(&json!(["fields"])));
    }

    #[test]
    fn generates_typed_body_and_rewrites_component_references() {
        let operations = generate(&fixture()).unwrap();
        let update = operations
            .iter()
            .find(|operation| operation.method == Method::POST && operation.path.contains("issues"))
            .unwrap();
        let schema = update.tool.schema_as_json_value();
        assert_eq!(
            schema.pointer("/properties/body/$ref"),
            Some(&json!("#/$defs/IssueUpdate"))
        );
        assert!(schema
            .pointer("/$defs/IssueUpdate/properties/serverOnly")
            .is_none());
        assert_eq!(
            schema.pointer("/$defs/IssueUpdate/required"),
            Some(&json!(["summary"]))
        );
        let tool = serde_json::to_value(&update.tool).unwrap();
        assert_eq!(
            tool.pointer("/outputSchema/$defs/Issue/properties/id/type"),
            Some(&json!("string"))
        );
        assert!(tool
            .pointer("/outputSchema/$defs/Issue/properties/secret")
            .is_none());
    }

    #[test]
    fn prepares_path_query_and_json_body() {
        let operations = generate(&fixture()).unwrap();
        let get = operations
            .iter()
            .find(|operation| operation.method == Method::GET)
            .unwrap();
        let prepared = get
            .prepare(
                serde_json::from_value(json!({"issueID":"ABC-1","fields":"id,summary"})).unwrap(),
            )
            .unwrap();
        assert_eq!(prepared.path, "/api/issues/ABC%2D1");
        assert_eq!(prepared.query, [("fields".into(), "id,summary".into())]);
    }

    #[test]
    fn marks_multipart_binary_fields_as_base64() {
        let operations = generate(&fixture()).unwrap();
        let upload = operations
            .iter()
            .find(|operation| operation.path == "/files")
            .unwrap();
        let body = upload.request_body.as_ref().unwrap();
        assert_eq!(body.kind, BodyKind::Multipart);
        assert!(body.binary_fields.contains("file"));
        assert_eq!(
            upload
                .tool
                .schema_as_json_value()
                .pointer("/properties/body/properties/file/contentEncoding"),
            Some(&json!("base64"))
        );
    }

    #[test]
    fn types_custom_binary_requests_and_text_responses() {
        let spec = json!({
            "openapi":"3.0.1",
            "paths":{"/render":{
                "post":{
                    "requestBody":{"required":true,"content":{"image/png":{"schema":{
                        "type":"string","format":"binary"
                    }}}},
                    "responses":{"200":{"content":{"text/plain":{"schema":{
                        "type":"string"
                    }}}}}
                }
            }}
        });

        let operation = generate(&spec).unwrap().remove(0);
        assert_eq!(
            operation.request_body.as_ref().unwrap().kind,
            BodyKind::Binary
        );
        let prepared = operation
            .prepare(serde_json::from_value(json!({"body":"aGVsbG8="})).unwrap())
            .unwrap();
        assert!(matches!(
            prepared.body,
            Some(PreparedBody::Binary(bytes)) if bytes == b"hello"
        ));
        let tool = serde_json::to_value(&operation.tool).unwrap();
        assert_eq!(
            tool.pointer("/outputSchema/properties/body/type"),
            Some(&json!("string"))
        );
    }
}
