use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResearchClaim {
    pub claim: String,
    pub source_url: Option<String>,
    pub quote: Option<String>,
    pub verdict: Verdict,
    pub relevance: Relevance,
    pub note: String,
    pub published: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Supported,
    Partial,
    Unsupported,
    NoSource,
    /// The claim did not answer or bear on the original question — caught by
    /// the relevance gate before the (expensive) claim-vs-source verifier
    /// ever ran. Kept in the envelope for visibility, never counted toward
    /// `outcome` or citability.
    OffTopic,
}

impl Verdict {
    pub fn is_supported_or_partial(self) -> bool {
        matches!(self, Verdict::Supported | Verdict::Partial)
    }
}

/// Per-claim relevance-gate outcome, carried alongside `verdict` so a
/// consumer can tell "supported against its source" apart from "actually
/// answers the question that was asked." `direct` is the relevance gate's
/// "yes", `related` is its "partially" (useful context, incomplete), and
/// `off_topic` mirrors `Verdict::OffTopic` — a claim with that verdict always
/// carries this relevance too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relevance {
    Direct,
    Related,
    OffTopic,
}
