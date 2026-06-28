//! Deterministic page observation + state signature.
//!
//! `observe` returns *facts only* — no interpretation, no `page_state` guess.
//! Interpreting the page is the parent LLM's job. The `state_signature` is a
//! content-invariant hash of the page's structural skeleton: it stays stable
//! when only content changes (different books, prices, dates) and changes when
//! the page's *kind* changes (catalogue vs. detail vs. error).

use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Caps on how many facts of each kind an `Observation` carries. The parent
/// already gets a screenshot; these arrays are a deterministic supplement, not a
/// full DOM dump. Without caps, a busy page (e.g. a hotel search with hundreds
/// of controls) serializes thousands of mostly-empty entries straight into the
/// model context. Keep the most useful, named entries and summarize the rest.
const MAX_INPUTS: usize = 40;
const MAX_LANDMARKS: usize = 30;
const MAX_TEXT_BLOCKS: usize = 40;

/// Opaque reference the parent can use to fetch a screenshot's bytes.
pub type ResourceId = String;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputFact {
    pub kind: String,            // "text" | "password" | "select" | "button" | ...
    pub accessible_name: String, // aria-label / placeholder / name — best effort
    pub selector_hint: String,   // a best-effort selector to reach it
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Landmark {
    pub role: String, // "nav" | "main" | "form" | "header" | "footer" | explicit role
    pub name: String, // accessible name if any
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub url: String,
    pub title: String,
    pub inputs: Vec<InputFact>,
    pub landmarks: Vec<Landmark>,
    pub text_blocks: Vec<String>,
    pub has_error_region: bool,
    pub state_signature: String,
    pub screenshot_ref: ResourceId,
}

/// Strip ASCII digits so per-run identifiers (counts, ids) don't perturb the
/// structural signature.
fn strip_digits(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_ascii_digit())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Content-invariant structural signature of the page.
///
/// Tokens are de-duplicated into an ordered set, so the signature is independent
/// of *how many* of a thing appears (e.g. number of result rows) and of element
/// order — it captures *what kinds* of structure exist, not the content.
pub fn state_signature(html: &str) -> String {
    let doc = Html::parse_document(html);
    let sel = Selector::parse(
        "input,select,textarea,button,a,form,nav,main,header,footer,h1,h2,h3,[role]",
    )
    .unwrap();

    let mut tokens: BTreeSet<String> = BTreeSet::new();
    for el in doc.select(&sel) {
        let v = el.value();
        let tag = v.name();
        let typ = v.attr("type").unwrap_or("");
        let role = v.attr("role").unwrap_or("");
        let name = strip_digits(v.attr("name").unwrap_or(""));
        tokens.insert(format!("{tag}|{typ}|{role}|{name}"));
    }

    let skeleton = tokens.into_iter().collect::<Vec<_>>().join("\n");
    blake3::hash(skeleton.as_bytes()).to_hex().to_string()
}

/// Append a synthetic "+N more (omitted)" entry when a capped list dropped
/// entries, so the parent knows the view is partial. `total` is the count of
/// kept-eligible items seen before capping; `list` holds the items actually kept.
fn push_overflow_marker<T>(list: &mut Vec<T>, total: usize, make: impl FnOnce(usize) -> T) {
    if total > list.len() {
        let dropped = total - list.len();
        list.push(make(dropped));
    }
}

/// Extract the deterministic, content-bearing facts an LLM would want to reason
/// over. Pure: takes HTML, returns facts. The caller supplies url/screenshot.
pub fn analyze(html: &str) -> AnalyzedFacts {
    let doc = Html::parse_document(html);

    let title = doc
        .select(&Selector::parse("title").unwrap())
        .next()
        .map(|e| e.text().collect::<String>().trim().to_string())
        .unwrap_or_default();

    // Inputs / controls — no values, just shape. An addressable control (one with
    // a real selector or an accessible name) is worth far more to the parent than
    // an anonymous `<button>` with no name, of which busy pages have hundreds. Keep
    // named/addressable ones first and cap the list; record how many were dropped.
    let mut inputs = Vec::new();
    let mut inputs_total = 0usize;
    let ctrl = Selector::parse("input,select,textarea,button").unwrap();
    for el in doc.select(&ctrl) {
        let v = el.value();
        let kind = match v.name() {
            "input" => v.attr("type").unwrap_or("text").to_string(),
            other => other.to_string(),
        };
        let accessible_name = v
            .attr("aria-label")
            .or_else(|| v.attr("placeholder"))
            .or_else(|| v.attr("name"))
            .unwrap_or("")
            .to_string();
        let has_stable_selector = v.attr("id").is_some() || v.attr("name").is_some();
        // Drop pure noise: an unnamed control with no stable selector is not
        // actionable and only inflates the payload.
        if accessible_name.is_empty() && !has_stable_selector {
            continue;
        }
        inputs_total += 1;
        if inputs.len() >= MAX_INPUTS {
            continue;
        }
        let selector_hint = if let Some(id) = v.attr("id") {
            format!("#{id}")
        } else if let Some(name) = v.attr("name") {
            format!("{}[name=\"{name}\"]", v.name())
        } else {
            v.name().to_string()
        };
        inputs.push(InputFact {
            kind,
            accessible_name,
            selector_hint,
        });
    }
    push_overflow_marker(&mut inputs, inputs_total, |dropped| InputFact {
        kind: "_more".to_string(),
        accessible_name: format!("+{dropped} more controls (omitted)"),
        selector_hint: String::new(),
    });

    // Landmarks. De-duplicate (role,name) pairs — repeated nameless dialogs/lists
    // add nothing — and cap. Nameless generic roles are dropped as noise.
    let mut landmarks = Vec::new();
    let mut landmarks_total = 0usize;
    let mut seen_landmarks: BTreeSet<(String, String)> = BTreeSet::new();
    let land = Selector::parse("nav,main,header,footer,form,[role]").unwrap();
    for el in doc.select(&land) {
        let v = el.value();
        let role = v.attr("role").unwrap_or(v.name()).to_string();
        let name = v.attr("aria-label").unwrap_or("").to_string();
        // Skip generic, nameless presentational roles that carry no signal.
        if name.is_empty()
            && matches!(
                role.as_str(),
                "presentation" | "none" | "document" | "tooltip"
            )
        {
            continue;
        }
        if !seen_landmarks.insert((role.clone(), name.clone())) {
            continue;
        }
        landmarks_total += 1;
        if landmarks.len() >= MAX_LANDMARKS {
            continue;
        }
        landmarks.push(Landmark { role, name });
    }
    push_overflow_marker(&mut landmarks, landmarks_total, |dropped| Landmark {
        role: "_more".to_string(),
        name: format!("+{dropped} more landmarks (omitted)"),
    });

    // Salient text: headings and alert regions. De-duplicate and cap.
    let mut text_blocks = Vec::new();
    let mut text_total = 0usize;
    let mut seen_text: BTreeSet<String> = BTreeSet::new();
    let heads = Selector::parse("h1,h2,h3,[role=alert]").unwrap();
    for el in doc.select(&heads) {
        let t = el.text().collect::<String>().trim().to_string();
        if t.is_empty() || !seen_text.insert(t.clone()) {
            continue;
        }
        text_total += 1;
        if text_blocks.len() >= MAX_TEXT_BLOCKS {
            continue;
        }
        text_blocks.push(t);
    }
    if text_total > text_blocks.len() {
        text_blocks.push(format!(
            "+{} more (omitted)",
            text_total - text_blocks.len()
        ));
    }

    // Error region heuristic.
    let err = Selector::parse("[role=alert],.error,.alert-danger,.is-invalid").unwrap();
    let has_error_region = doc.select(&err).next().is_some();

    AnalyzedFacts {
        title,
        inputs,
        landmarks,
        text_blocks,
        has_error_region,
        state_signature: state_signature(html),
    }
}

/// The HTML-derived portion of an `Observation` (everything except url +
/// screenshot_ref, which the driver supplies).
pub struct AnalyzedFacts {
    pub title: String,
    pub inputs: Vec<InputFact>,
    pub landmarks: Vec<Landmark>,
    pub text_blocks: Vec<String>,
    pub has_error_region: bool,
    pub state_signature: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    const CATALOGUE: &str = r#"<!doctype html><html><head><title>Cat</title></head>
        <body><header><h1>Shop</h1><nav><a href="a">A</a></nav></header>
        <main><article class="product_pod"><h3><a href="1.html">Book 1</a></h3>
        <p class="price_color">£10.00</p></article>
        <article class="product_pod"><h3><a href="2.html">Book 2</a></h3>
        <p class="price_color">£20.00</p></article></main></body></html>"#;

    // Same structure, different content (titles + prices changed, one more row).
    const CATALOGUE_OTHER_CONTENT: &str = r#"<!doctype html><html><head><title>Cat</title></head>
        <body><header><h1>Shop</h1><nav><a href="a">A</a></nav></header>
        <main><article class="product_pod"><h3><a href="9.html">Totally Different</a></h3>
        <p class="price_color">£99.99</p></article>
        <article class="product_pod"><h3><a href="8.html">Another One</a></h3>
        <p class="price_color">£7.50</p></article>
        <article class="product_pod"><h3><a href="7.html">Third</a></h3>
        <p class="price_color">£1.00</p></article></main></body></html>"#;

    const DETAIL: &str = r#"<!doctype html><html><head><title>Book</title></head>
        <body><nav><a href="../index.html">Catalogue</a></nav>
        <article class="product_page"><h1>Book 1</h1>
        <p class="price_color">£10.00</p>
        <table><tr><th>UPC</th><td>abc</td></tr></table></article></body></html>"#;

    #[test]
    fn signature_is_stable_across_reloads() {
        assert_eq!(state_signature(CATALOGUE), state_signature(CATALOGUE));
    }

    #[test]
    fn signature_is_content_invariant() {
        // Different titles, prices, and row count — same structural kind.
        assert_eq!(
            state_signature(CATALOGUE),
            state_signature(CATALOGUE_OTHER_CONTENT),
            "signature must ignore content + row count"
        );
    }

    #[test]
    fn signature_distinguishes_page_kind() {
        assert_ne!(
            state_signature(CATALOGUE),
            state_signature(DETAIL),
            "catalogue and detail are different states"
        );
    }

    #[test]
    fn analyze_extracts_headings_and_landmarks() {
        let f = analyze(DETAIL);
        assert!(f.text_blocks.iter().any(|t| t == "Book 1"));
        assert!(f.landmarks.iter().any(|l| l.role == "nav"));
        assert!(!f.has_error_region);
    }

    #[test]
    fn unnamed_anonymous_controls_are_dropped() {
        // Buttons with neither a name nor a stable selector are pure noise.
        let html = r#"<!doctype html><html><body>
            <button></button><button></button>
            <button id="go">Go</button>
            <input aria-label="Search">
        </body></html>"#;
        let f = analyze(html);
        // Only the two addressable controls survive; the anonymous buttons drop.
        assert_eq!(f.inputs.len(), 2);
        assert!(f.inputs.iter().any(|i| i.selector_hint == "#go"));
        assert!(f.inputs.iter().any(|i| i.accessible_name == "Search"));
    }

    #[test]
    fn inputs_are_capped_with_overflow_marker() {
        // Generate many addressable buttons to exceed MAX_INPUTS.
        let mut html = String::from("<!doctype html><html><body>");
        for i in 0..(MAX_INPUTS + 25) {
            html.push_str(&format!("<button id=\"b{i}\">B{i}</button>"));
        }
        html.push_str("</body></html>");
        let f = analyze(&html);
        // Capped to MAX_INPUTS kept entries plus one overflow marker.
        assert_eq!(f.inputs.len(), MAX_INPUTS + 1);
        let marker = f.inputs.last().unwrap();
        assert_eq!(marker.kind, "_more");
        assert!(marker.accessible_name.contains("more controls"));
    }

    #[test]
    fn duplicate_text_blocks_are_deduped() {
        let html = r#"<!doctype html><html><body>
            <h1>Same</h1><h2>Same</h2><h3>Different</h3>
        </body></html>"#;
        let f = analyze(html);
        assert_eq!(f.text_blocks.iter().filter(|t| *t == "Same").count(), 1);
        assert!(f.text_blocks.iter().any(|t| t == "Different"));
    }
}
