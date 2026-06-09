// TODO: replace with bip329 crate types once upstream adds `tags` field support
use std::collections::BTreeMap;
use anyhow::Result;
use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AnnotationType {
    Tx,
    Addr,
    Output,
    Input,
}

#[derive(Debug, Serialize)]
pub struct Annotation {
    #[serde(rename = "type")]
    pub type_: AnnotationType,
    #[serde(rename = "ref")]
    pub ref_: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub tags: BTreeMap<String, String>,
}

impl Annotation {
    pub fn to_jsonl(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }
}
