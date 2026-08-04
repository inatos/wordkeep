use reqwest::{Method, RequestBuilder, Response, StatusCode};
use serde::Serialize;
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Clone)]
pub(crate) struct MeiliClient {
    http: reqwest::Client,
    base_url: String,
    key: Option<String>,
    index_uid: String,
}

impl MeiliClient {
    pub fn new(base_url: String, key: Option<String>, index_uid: String) -> Result<Self, String> {
        if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
            return Err("Meilisearch URL must begin with http:// or https://".to_string());
        }
        Ok(Self {
            http: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            key,
            index_uid,
        })
    }

    pub async fn ensure_index(&self) -> Result<(), String> {
        let response = self
            .request(Method::GET, &format!("/indexes/{}", self.index_uid))
            .send()
            .await
            .map_err(|error| format!("connect to Meilisearch: {error}"))?;

        if response.status() == StatusCode::NOT_FOUND {
            let task = self
                .send_json(
                    self.request(Method::POST, "/indexes")
                        .json(&json!({"uid": self.index_uid, "primaryKey": "id"})),
                )
                .await?;
            self.await_task(task_uid(&task)?).await?;
        } else {
            successful_json(response).await?;
        }

        let settings = json!({
            "searchableAttributes": ["heading", "content", "path", "tags", "anchor"],
            "filterableAttributes": ["kind", "root", "tags", "path"],
            "sortableAttributes": ["updated_at", "mtime_ns", "path"],
            "displayedAttributes": [
                "id", "path", "heading", "heading_path", "heading_level", "anchor",
                "body", "content", "kind", "root", "tags", "start_line", "end_line",
                "mtime_ns", "updated_at"
            ],
            "typoTolerance": { "enabled": true }
        });
        let task = self
            .send_json(
                self.request(
                    Method::PATCH,
                    &format!("/indexes/{}/settings", self.index_uid),
                )
                .json(&settings),
            )
            .await?;
        self.await_task(task_uid(&task)?).await
    }

    pub async fn upsert_documents<T: Serialize>(&self, documents: &[T]) -> Result<(), String> {
        if documents.is_empty() {
            return Ok(());
        }
        let task = self
            .send_json(
                self.request(
                    Method::POST,
                    &format!("/indexes/{}/documents?primaryKey=id", self.index_uid),
                )
                .json(documents),
            )
            .await?;
        self.await_task(task_uid(&task)?).await
    }

    pub async fn delete_documents(&self, document_ids: &[String]) -> Result<(), String> {
        if document_ids.is_empty() {
            return Ok(());
        }
        let task = self
            .send_json(
                self.request(
                    Method::POST,
                    &format!("/indexes/{}/documents/delete-batch", self.index_uid),
                )
                .json(document_ids),
            )
            .await?;
        self.await_task(task_uid(&task)?).await
    }

    pub async fn search(&self, request: Value) -> Result<Value, String> {
        self.send_json(
            self.request(Method::POST, &format!("/indexes/{}/search", self.index_uid))
                .json(&request),
        )
        .await
    }

    pub async fn health(&self) -> Result<Value, String> {
        self.send_json(self.request(Method::GET, "/health")).await
    }

    pub async fn index_stats(&self) -> Result<Value, String> {
        self.send_json(self.request(Method::GET, &format!("/indexes/{}/stats", self.index_uid)))
            .await
    }

    async fn await_task(&self, uid: u64) -> Result<(), String> {
        for _ in 0..600 {
            let task = self
                .send_json(self.request(Method::GET, &format!("/tasks/{uid}")))
                .await?;
            match task.get("status").and_then(Value::as_str) {
                Some("succeeded") => return Ok(()),
                Some("failed") | Some("canceled") => {
                    return Err(format!(
                        "Meilisearch task {uid} {}: {}",
                        task.get("status")
                            .and_then(Value::as_str)
                            .unwrap_or("failed"),
                        task.get("error").cloned().unwrap_or(Value::Null)
                    ));
                }
                _ => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
        Err(format!("timed out waiting for Meilisearch task {uid}"))
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let request = self
            .http
            .request(method, format!("{}{}", self.base_url, path));
        if let Some(key) = &self.key {
            request.bearer_auth(key)
        } else {
            request
        }
    }

    async fn send_json(&self, request: RequestBuilder) -> Result<Value, String> {
        let response = request
            .send()
            .await
            .map_err(|error| format!("Meilisearch request failed: {error}"))?;
        successful_json(response).await
    }
}

fn task_uid(value: &Value) -> Result<u64, String> {
    value
        .get("taskUid")
        .or_else(|| value.get("uid"))
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("Meilisearch response omitted task uid: {value}"))
}

async fn successful_json(response: Response) -> Result<Value, String> {
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| format!("read Meilisearch response: {error}"))?;
    if !status.is_success() {
        return Err(format!(
            "Meilisearch returned {status}: {}",
            text.trim().chars().take(1000).collect::<String>()
        ));
    }
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text)
        .map_err(|error| format!("decode Meilisearch response ({status}): {error}"))
}
