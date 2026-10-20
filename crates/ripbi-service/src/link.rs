//! Power BI / Fabric portal links: what a user copies from the address bar.

use crate::api::is_guid;

/// What a portal link points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// A semantic model (`/datasets/{id}` or `/semanticmodels/{id}`).
    SemanticModel,
    /// A report (`/reports/{id}`); its model is found upstream.
    Report,
}

/// A parsed portal link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// The workspace GUID (`/groups/{id}`).
    pub workspace_id: String,
    /// What the link points at.
    pub kind: LinkKind,
    /// The item GUID.
    pub item_id: String,
}

/// Parses `https://app.powerbi.com/groups/<ws>/datasets/<id>/details` and
/// its siblings (`app.fabric.microsoft.com`, `/semanticmodels/`,
/// `/reports/<id>/<page>`, any query string). `None` for anything else,
/// including `My workspace` links (`/groups/me/`), which name no GUID.
#[must_use]
pub fn parse_link(text: &str) -> Option<Link> {
    let rest = text
        .trim()
        .strip_prefix("https://")
        .or_else(|| text.trim().strip_prefix("http://"))?;
    let path = rest.split(['?', '#']).next()?;
    let segments: Vec<&str> = path.split('/').skip(1).filter(|s| !s.is_empty()).collect();
    let groups = segments
        .iter()
        .position(|segment| segment.eq_ignore_ascii_case("groups"))?;
    let workspace_id = segments.get(groups + 1).filter(|id| is_guid(id))?;
    let kind_segment = segments.get(groups + 2)?.to_ascii_lowercase();
    let kind = match kind_segment.as_str() {
        "datasets" | "semanticmodels" => LinkKind::SemanticModel,
        "reports" => LinkKind::Report,
        _ => return None,
    };
    let item_id = segments.get(groups + 3).filter(|id| is_guid(id))?;
    Some(Link {
        workspace_id: (*workspace_id).to_string(),
        kind,
        item_id: (*item_id).to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const WS: &str = "11111111-1111-1111-1111-111111111111";
    const ID: &str = "22222222-2222-2222-2222-222222222222";

    #[test]
    fn dataset_and_semantic_model_links_parse() {
        for url in [
            format!(
                "https://app.powerbi.com/groups/{WS}/datasets/{ID}/details?experience=power-bi"
            ),
            format!("https://app.fabric.microsoft.com/groups/{WS}/semanticmodels/{ID}"),
            format!("  https://app.powerbi.com/groups/{WS}/datasets/{ID}  "),
        ] {
            let link = parse_link(&url).unwrap_or_else(|| panic!("parses: {url}"));
            assert_eq!(link.kind, LinkKind::SemanticModel);
            assert_eq!(
                (link.workspace_id.as_str(), link.item_id.as_str()),
                (WS, ID)
            );
        }
    }

    #[test]
    fn report_links_parse_with_a_page() {
        let link = parse_link(&format!(
            "https://app.powerbi.com/groups/{WS}/reports/{ID}/ReportSection1?ctid=x"
        ))
        .expect("parses");
        assert_eq!(link.kind, LinkKind::Report);
        assert_eq!(link.item_id, ID);
    }

    #[test]
    fn other_links_do_not_parse() {
        assert_eq!(
            parse_link(&format!("https://app.powerbi.com/groups/me/datasets/{ID}")),
            None
        );
        assert_eq!(
            parse_link(&format!(
                "https://app.powerbi.com/groups/{WS}/dashboards/{ID}"
            )),
            None
        );
        assert_eq!(parse_link("Sales"), None);
    }
}
