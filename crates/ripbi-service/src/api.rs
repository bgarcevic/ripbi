//! The response shapes this client reads. Every field defaults and unknown
//! fields are ignored: the relations API is beta and Power BI schemas drift
//! (coding standard 3). Type strings stay strings — the tenant answers
//! `Model` where the docs say `SemanticModel` — and are classified by the
//! `is_*` helpers instead of a closed enum.

use base64::Engine as _;
use serde::Deserialize;

use crate::error::{Error, Result};

/// A workspace, from `GET /v1/workspaces` or a relations response.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Workspace {
    /// The workspace GUID.
    pub id: String,
    /// The workspace name.
    pub display_name: String,
    /// `Workspace`, `Personal`, `AdminWorkspace`, …
    #[serde(rename = "type")]
    pub kind: String,
}

/// A Fabric item: a report, a semantic model, or anything else.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Item {
    /// The item GUID.
    pub id: String,
    /// The item name.
    pub display_name: String,
    /// The item type as the API spells it.
    #[serde(rename = "type")]
    pub kind: String,
    /// The owning workspace's GUID.
    pub workspace_id: String,
}

impl Item {
    /// Whether this is a (PBIR) report. Paginated reports are a different type.
    #[must_use]
    pub fn is_report(&self) -> bool {
        self.kind.eq_ignore_ascii_case("Report")
    }

    /// Whether this is a semantic model, under any of its spellings.
    #[must_use]
    pub fn is_semantic_model(&self) -> bool {
        ["SemanticModel", "Model", "Dataset"]
            .iter()
            .any(|kind| self.kind.eq_ignore_ascii_case(kind))
    }
}

/// One edge in a relations response: `item_id` depends on `dependent_on_item_id`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Relation {
    /// The dependent item.
    pub item_id: String,
    /// The item it depends on.
    pub dependent_on_item_id: String,
    /// `Association`, `Shortcut`, … (undocumented values included).
    pub relation_type: String,
}

/// A relations response: the items, the edges between them, and the
/// workspaces they live in. The root item is one of the items.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Relations {
    /// Every item in the slice, the root included.
    pub items: Vec<Item>,
    /// The edges.
    pub relations: Vec<Relation>,
    /// Names for every `workspace_id` the items carry.
    pub workspaces: Vec<Workspace>,
    /// The next page, when the API paged the answer.
    pub continuation_uri: Option<String>,
}

/// One page of a Fabric (`value` + `continuationUri`) or Power BI (`value`)
/// list.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Page<T> {
    #[serde(default = "Vec::new")]
    pub value: Vec<T>,
    #[serde(default)]
    pub continuation_uri: Option<String>,
}

/// A report in the Power BI REST shape (`/groups/{id}/reports`,
/// `/apps/{id}/reports`), which names what the Fabric shape omits.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct PbiReport {
    /// The report GUID — for an app report, the app copy's own id.
    pub id: String,
    /// The report name.
    pub name: String,
    /// The bound semantic model.
    pub dataset_id: Option<String>,
    /// For an app report: the workspace report it was published from.
    pub original_report_object_id: Option<String>,
}

/// An org app the caller can see.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct App {
    /// The app GUID.
    pub id: String,
    /// The app name.
    pub name: String,
}

/// An item definition, as `getDefinition` returns it.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Definition {
    /// `PBIR`, `PBIR-Legacy`, `TMDL`, …
    pub format: Option<String>,
    /// The files.
    pub parts: Vec<Part>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct DefinitionEnvelope {
    pub definition: Definition,
}

/// One file of a definition.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Part {
    /// The path inside the item folder, `/`-separated. Untrusted: check it
    /// before writing (TMDL escapes `/` in object names as `%2F`).
    pub path: String,
    /// The encoded contents.
    pub payload: String,
    /// `InlineBase64` — the only type the API documents.
    pub payload_type: String,
}

impl Part {
    /// The decoded file bytes.
    ///
    /// # Errors
    ///
    /// [`Error::Response`] for an unknown payload type or invalid base64.
    pub fn decode(&self) -> Result<Vec<u8>> {
        if !self.payload_type.is_empty() && !self.payload_type.eq_ignore_ascii_case("InlineBase64")
        {
            return Err(Error::Response {
                endpoint: self.path.clone(),
                detail: format!("unsupported payload type '{}'", self.payload_type),
            });
        }
        base64::engine::general_purpose::STANDARD
            .decode(self.payload.as_bytes())
            .map_err(|error| Error::Response {
                endpoint: self.path.clone(),
                detail: format!("invalid base64 payload: {error}"),
            })
    }
}

/// The state of a long-running operation.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct OperationState {
    pub status: String,
    pub error: Option<OperationError>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct OperationError {
    pub error_code: Option<String>,
    pub message: Option<String>,
}

/// Fabric (`errorCode`) and Power BI (`error.code`) error bodies.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct ErrorBody {
    pub error_code: Option<String>,
    pub error: Option<ErrorDetail>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct ErrorDetail {
    pub code: Option<String>,
}

impl ErrorBody {
    pub(crate) fn code(body: &[u8]) -> Option<String> {
        let parsed: Self = serde_json::from_slice(body).ok()?;
        parsed
            .error_code
            .or_else(|| parsed.error.and_then(|detail| detail.code))
    }
}

/// Whether `text` is shaped like a GUID (`8-4-4-4-12` hex digits).
#[must_use]
pub fn is_guid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(group, len)| group.len() == len && group.bytes().all(|b| b.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relations_parse_loosely() {
        let json = r#"{
            "items": [
                {"id": "r1", "type": "Report", "displayName": "Sales", "workspaceId": "w1", "extra": 1},
                {"id": "m1", "type": "Model", "displayName": "Sales", "workspaceId": "w0"},
                {"id": "x1", "type": "SomethingNew"}
            ],
            "relations": [{"itemId": "r1", "dependentOnItemId": "m1", "relationType": "Association"}],
            "workspaces": [{"id": "w1", "displayName": "Marketing"}]
        }"#;
        let relations: Relations = serde_json::from_str(json).expect("parses");
        assert!(relations.items[0].is_report());
        assert!(relations.items[1].is_semantic_model());
        assert!(!relations.items[2].is_report() && !relations.items[2].is_semantic_model());
        assert_eq!(relations.relations[0].relation_type, "Association");
        assert_eq!(relations.continuation_uri, None);
    }

    #[test]
    fn parts_decode_inline_base64_as_bytes() {
        let part = Part {
            path: "definition/tables/Yes%2FNo.tmdl".to_string(),
            payload: "w6bDuMOl".to_string(),
            payload_type: "InlineBase64".to_string(),
        };
        assert_eq!(part.decode().expect("decodes"), "æøå".as_bytes());
        let odd = Part {
            payload_type: "Reference".to_string(),
            ..part
        };
        assert!(odd.decode().is_err());
    }

    #[test]
    fn error_codes_come_from_either_shape() {
        assert_eq!(
            ErrorBody::code(br#"{"requestId":"x","errorCode":"ItemNotFound","message":"m"}"#)
                .as_deref(),
            Some("ItemNotFound")
        );
        assert_eq!(
            ErrorBody::code(br#"{"error":{"code":"PowerBINotAuthorizedException"}}"#).as_deref(),
            Some("PowerBINotAuthorizedException")
        );
        assert_eq!(ErrorBody::code(b"<html>"), None);
    }

    #[test]
    fn guids_are_recognised() {
        assert!(is_guid("0a1b2c3d-0000-41a4-9808-ABCDEF012345"));
        assert!(!is_guid("Sales"));
        assert!(!is_guid("0a1b2c3d-0000-41a4-9808-ABCDEF01234"));
    }
}
