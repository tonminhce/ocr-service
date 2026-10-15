//! Evidence-preserving, deliberately scoped Vietnamese rental-contract extraction.
//!
//! Rules propose observations from OCR; they do not establish source accuracy,
//! legal validity, identity, or authenticity. Every result requires human review.
mod normalize;
mod rules;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const SCHEMA_VERSION: &str = "rental-extraction/1.0.0";
pub const PROFILE_VERSION: &str = "vi_rental_contract_v1";
pub const NORMALIZER_VERSION: &str = "vi_conservative/1.0.0";
pub const POLICY_VERSION: &str = "review_all_uncalibrated/1.0.0";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceBlock {
    pub id: String,
    pub page_number: u32,
    pub region_index: Option<u32>,
    pub source: String,
    pub text: String,
    pub bbox_2d: Option<[i32; 4]>,
    pub detector_score: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionResult {
    pub schema_version: String,
    pub profile_version: String,
    pub normalizer_version: String,
    pub policy_version: String,
    pub supported_template: bool,
    pub decision: Decision,
    pub source_blocks: Vec<SourceReference>,
    pub parties: Vec<Party>,
    pub fields: BTreeMap<String, FieldResult>,
    pub identity_documents: Vec<IdentityDocument>,
    pub issues: Vec<Issue>,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    NeedsReview,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceReference {
    pub block_id: String,
    pub page_number: u32,
    pub region_index: Option<u32>,
    pub source: String,
    pub text_sha256: String,
    pub text_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Party {
    pub id: String,
    pub designator: String,
    pub role: PartyRole,
    pub role_evidence: Vec<Evidence>,
    /// Contract observations stay separate from identity-card observations.
    pub contract_fields: BTreeMap<String, FieldResult>,
    pub identity_fields: BTreeMap<String, FieldResult>,
    /// A conflict has no selected value, including cross-source disagreements.
    pub fields: BTreeMap<String, FieldResult>,
    pub linked_identity_documents: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PartyRole {
    Landlord,
    Tenant,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityDocument {
    pub id: String,
    pub source_block_ids: Vec<String>,
    pub side: String,
    pub fields: BTreeMap<String, FieldResult>,
    pub linkage: IdentityLinkage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityLinkage {
    pub state: String,
    pub party_ids: Vec<String>,
    pub method: String,
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FieldState {
    Found,
    Missing,
    Unreadable,
    Ambiguous,
    Conflict,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldResult {
    pub state: FieldState,
    pub selected: Option<FieldValue>,
    pub confidence: Option<f64>,
    pub candidates: Vec<Candidate>,
    /// Empty/invalid anchored values remain evidence even with no candidate.
    pub observations: Vec<Evidence>,
}

impl Default for FieldResult {
    fn default() -> Self {
        Self {
            state: FieldState::Missing,
            selected: None,
            confidence: None,
            candidates: Vec::new(),
            observations: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FieldValue {
    Text {
        value: String,
    },
    Identifier {
        value: String,
    },
    Date {
        value: String,
    },
    Money {
        amount: String,
        currency: String,
        period: Option<String>,
    },
    Area {
        amount: String,
        unit: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub value: FieldValue,
    pub raw_value: String,
    pub origin: String,
    pub method: String,
    pub confidence: Option<f64>,
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub block_id: String,
    /// Pages are 1-based; byte offsets are 0-based UTF-8, end exclusive.
    pub page_number: u32,
    pub region_index: Option<u32>,
    pub source: String,
    pub byte_start: usize,
    pub byte_end: usize,
    pub quote: String,
    /// Geometry belongs to the entire source block, never to the extracted word.
    pub bbox_2d: Option<[i32; 4]>,
    pub geometry_scope: String,
    pub detector_score: Option<f64>,
    pub detector_score_kind: String,
    pub confidence: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub code: String,
    pub severity: String,
    pub field_path: Option<String>,
    pub message: String,
    pub suggested_correction: Option<String>,
    pub evidence: Vec<Evidence>,
}

pub fn extract(blocks: &[SourceBlock]) -> ExtractionResult {
    rules::extract(blocks)
}

/// A bounded recovery trigger, not proof that recognition is incorrect.
/// Literal placeholders and low layout scores alone do not trigger model calls.
pub fn recognition_recovery_reason(text: &str) -> Option<&'static str> {
    rules::recognition_recovery_reason(text)
}
