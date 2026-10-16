//! Explicit v2 evaluation contract. Gold identities are independent of runtime IDs.
//! Matching uses source geometry once per case, never the field values being scored.
use crate::{
    MAX_MANIFEST_BYTES, MAX_RESULT_BYTES, MAX_SOURCE_BYTES, Split, digest, read_bounded,
    safe_resolve,
};
use anyhow::{Context, Result, ensure};
use ocr_extraction::FieldValue;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
use unicode_normalization::UnicodeNormalization;

pub const SCHEMA: &str = "ocr-eval-manifest/2.0.0";
pub const NORMALIZATION: &str = "nfc-typed/1.0.0";
const MAX_ENTITIES: usize = 32;
const MAX_OBSERVATIONS: usize = 1024;
const MAX_FIELDS: usize = 256;
const MAX_CER_CHARS: usize = 1024;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: String,
    pub normalization_policy: String,
    pub dataset_name: String,
    pub synthetic: bool,
    pub cases: Vec<Case>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cohort {
    Representative,
    Challenge,
    UnseenTemplate,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    Native,
    Scan,
    Camera,
    Mixed,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    pub group_id: String,
    pub template_family: String,
    pub split: Split,
    pub cohort: Cohort,
    pub quality: Quality,
    pub source_sha256: String,
    pub source_path: String,
    pub prediction_path: String,
    pub prediction_sha256: String,
    pub pages: Vec<Page>,
    pub entities: Vec<Entity>,
    pub observations: Vec<Observation>,
    pub fields: Vec<GoldField>,
}
/// Top-left rendered pixel coordinates after rotation and CropBox are applied.
/// PDF CropBox is preserved separately in PDF points; it is never inferred from pixels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    pub number: u32,
    pub width: u32,
    pub height: u32,
    pub rotation_degrees: u16,
    pub crop_box_pdf: [f64; 4],
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Document,
    Party,
    Card,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Landlord,
    Tenant,
    Unknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Contract,
    IdentityCard,
    Attachment,
    Other,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Front,
    Back,
    NotApplicable,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Granularity {
    Word,
    Line,
    Block,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Anchor {
    pub page_number: u32,
    pub source_kind: SourceKind,
    pub side: Side,
    pub granularity: Granularity,
    /// 3..16 vertices, strictly convex, either winding, rendered pixels.
    pub polygon: Vec<[f64; 2]>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entity {
    pub id: String,
    pub kind: EntityKind,
    pub document_id: Option<String>,
    pub party_id: Option<String>,
    pub role: Option<Role>,
    /// A manually chosen stable source anchor; do not derive it from field values.
    pub anchor: Anchor,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationState {
    Present,
    SourceBlank,
    Unreadable,
    Placeholder,
    Unsupported,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    FirstPass,
    Recovery,
    HumanGold,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub id: String,
    pub document_id: String,
    pub party_id: Option<String>,
    pub card_id: Option<String>,
    pub field: String,
    pub state: ObservationState,
    pub origin: Origin,
    pub visible_text: String,
    #[serde(default, deserialize_with = "deserialize_typed_value")]
    pub value: Option<FieldValue>,
    pub anchor: Anchor,
    /// Absent on independent gold; mandatory on machine predictions, including blanks.
    pub evidence: Option<SpanEvidence>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpanEvidence {
    pub transcript_id: String,
    pub byte_start: usize,
    pub byte_end: usize,
    pub quote_sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transcript {
    pub id: String,
    pub text: String,
    pub anchor: Anchor,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Found,
    Conflict,
    Ambiguous,
    Missing,
    SourceBlank,
    Unreadable,
    Placeholder,
    Unsupported,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub document_id: String,
    pub party_id: Option<String>,
    pub field: String,
    pub state: State,
    pub observation_ids: Vec<String>,
    pub ambiguity_reason: Option<AmbiguityReason>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AmbiguityReason {
    RecoveryOnly,
    UncertainSource,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Annotation {
    pub annotator_ids: Vec<String>,
    pub prediction_hidden: bool,
    pub pixels_checked: bool,
    pub adjudicated: bool,
    pub adjudicator_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldField {
    pub target: Field,
    pub critical: bool,
    pub annotation: Annotation,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prediction {
    pub schema_version: String,
    pub normalization_policy: String,
    pub source_sha256: String,
    /// This artifact is machine output before human corrections.
    pub machine_only: bool,
    pub pages: Vec<Page>,
    pub entities: Vec<Entity>,
    pub transcripts: Vec<Transcript>,
    pub observations: Vec<Observation>,
    pub fields: Vec<Field>,
}

pub struct Dataset {
    pub manifest: Manifest,
    base: PathBuf,
}
impl Dataset {
    pub fn load(path: &Path) -> Result<Self> {
        let path = path.canonicalize().context("v2 manifest does not exist")?;
        let base = path.parent().context("manifest has no parent")?.to_owned();
        let manifest: Manifest = serde_json::from_slice(&read_bounded(&path, MAX_MANIFEST_BYTES)?)
            .map_err(|_| anyhow::anyhow!("invalid v2 manifest JSON or field shape"))?;
        ensure!(
            manifest.schema_version == SCHEMA && manifest.normalization_policy == NORMALIZATION,
            "unsupported v2 schema/normalization policy"
        );
        ensure!(id(&manifest.dataset_name), "invalid dataset identifier");
        ensure!(
            (1..=1000).contains(&manifest.cases.len()),
            "v2 dataset must contain 1..1000 cases"
        );
        let mut ids = BTreeSet::new();
        let mut hashes = BTreeSet::new();
        let mut groups = BTreeMap::new();
        for case in &manifest.cases {
            ensure!(
                id(&case.id) && id(&case.group_id) && id(&case.template_family),
                "invalid case/group/template identifier"
            );
            ensure!(
                ids.insert(&case.id) && hashes.insert(&case.source_sha256),
                "duplicate case/source hash"
            );
            if let Some(split) = groups.insert(&case.group_id, case.split) {
                ensure!(split == case.split, "case group crosses splits");
            }
            ensure!(
                sha(&case.source_sha256) && sha(&case.prediction_sha256),
                "invalid source/prediction hash"
            );
            ensure!(
                digest(&read_bounded(
                    &safe_resolve(&base, &case.source_path)?,
                    MAX_SOURCE_BYTES
                )?) == case.source_sha256,
                "v2 source hash mismatch"
            );
            safe_resolve(&base, &case.prediction_path)?;
            validate_pages(&case.pages)?;
            validate_graph(
                &case.pages,
                &case.entities,
                &case.observations,
                &case
                    .fields
                    .iter()
                    .map(|f| f.target.clone())
                    .collect::<Vec<_>>(),
                false,
                &[],
            )?;
            ensure!(
                case.fields.iter().any(|f| f.critical),
                "case must label at least one critical field"
            );
            for field in &case.fields {
                validate_annotation(&field.annotation, field.critical)?;
            }
        }
        for case in manifest
            .cases
            .iter()
            .filter(|c| c.cohort == Cohort::UnseenTemplate)
        {
            ensure!(
                case.split == Split::Test
                    && !manifest.cases.iter().any(|other| other.split != Split::Test
                        && other.template_family == case.template_family),
                "unseen-template family leaked into development/calibration"
            );
        }
        Ok(Self { manifest, base })
    }
    fn prediction(&self, case: &Case) -> Result<Prediction> {
        // Recheck source at scoring time, as v1 does; validation is not a permanent attestation.
        ensure!(
            digest(&read_bounded(
                &safe_resolve(&self.base, &case.source_path)?,
                MAX_SOURCE_BYTES
            )?) == case.source_sha256,
            "v2 source changed after validation"
        );
        let bytes = read_bounded(
            &safe_resolve(&self.base, &case.prediction_path)?,
            MAX_RESULT_BYTES,
        )?;
        ensure!(
            digest(&bytes) == case.prediction_sha256,
            "v2 prediction artifact hash mismatch"
        );
        let prediction: Prediction = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid v2 prediction JSON or field shape"))?;
        ensure!(
            prediction.schema_version == "ocr-eval-prediction/2.0.0"
                && prediction.normalization_policy == NORMALIZATION
                && prediction.machine_only,
            "v2 requires versioned machine-only prediction"
        );
        ensure!(
            prediction.source_sha256 == case.source_sha256,
            "v2 prediction source hash mismatch"
        );
        validate_pages(&prediction.pages)?;
        validate_graph(
            &prediction.pages,
            &prediction.entities,
            &prediction.observations,
            &prediction.fields,
            true,
            &prediction.transcripts,
        )?;
        Ok(prediction)
    }
}
fn deserialize_typed_value<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<FieldValue>, D::Error> {
    use serde::de::Error;
    let raw = Option::<serde_json::Value>::deserialize(deserializer)?;
    raw.map(|raw| {
        let object = raw
            .as_object()
            .ok_or_else(|| D::Error::custom("typed value must be an object"))?;
        let keys: &[&str] = match object.get("type").and_then(serde_json::Value::as_str) {
            Some("text" | "identifier" | "date") => &["type", "value"],
            Some("money") => &["type", "amount", "currency", "period"],
            Some("area") => &["type", "amount", "unit"],
            _ => return Err(D::Error::custom("unsupported typed value")),
        };
        if object.keys().any(|key| !keys.contains(&key.as_str())) {
            return Err(D::Error::custom("unknown typed value field"));
        }
        serde_json::from_value(raw).map_err(|_| D::Error::custom("malformed typed value"))
    })
    .transpose()
}
fn id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}
fn sha(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn field_key(value: &str) -> bool {
    matches!(
        value,
        "name"
            | "id_number"
            | "birth_date"
            | "issue_date"
            | "residence_address"
            | "contact_address"
            | "phone"
            | "bank_account"
            | "monthly_rent"
            | "deposit_amount"
            | "property_address"
            | "property_area"
    )
}
fn validate_annotation(value: &Annotation, critical: bool) -> Result<()> {
    let ids: BTreeSet<_> = value.annotator_ids.iter().collect();
    ensure!(
        !ids.is_empty()
            && ids.len() == value.annotator_ids.len()
            && ids.len() <= 8
            && ids.iter().all(|s| id(s))
            && value.prediction_hidden
            && value.pixels_checked,
        "independent initial annotation required"
    );
    if critical {
        ensure!(
            ids.len() >= 2
                && value.adjudicated
                && value
                    .adjudicator_id
                    .as_ref()
                    .is_some_and(|a| id(a) && !ids.contains(a)),
            "critical labels need two independent annotators and a separate adjudicator"
        );
    }
    ensure!(
        value.adjudicated == value.adjudicator_id.is_some()
            && value.adjudicator_id.as_deref().is_none_or(id),
        "inconsistent adjudication provenance"
    );
    Ok(())
}
fn validate_pages(pages: &[Page]) -> Result<()> {
    ensure!(
        !pages.is_empty() && pages.len() <= 1000,
        "page count out of bounds"
    );
    let mut ids = BTreeSet::new();
    for p in pages {
        ensure!(
            p.number > 0
                && ids.insert(p.number)
                && p.width > 0
                && p.width <= 32768
                && p.height > 0
                && p.height <= 32768
                && matches!(p.rotation_degrees, 0 | 90 | 180 | 270)
                && p.crop_box_pdf.iter().all(|x| x.is_finite())
                && p.crop_box_pdf[2] > p.crop_box_pdf[0]
                && p.crop_box_pdf[3] > p.crop_box_pdf[1],
            "invalid page transform"
        );
    }
    Ok(())
}
fn validate_anchor(anchor: &Anchor, pages: &[Page]) -> Result<()> {
    let page = pages
        .iter()
        .find(|p| p.number == anchor.page_number)
        .context("anchor page missing")?;
    ensure!(
        (3..=16).contains(&anchor.polygon.len()),
        "polygon vertex count out of bounds"
    );
    ensure!(
        anchor
            .polygon
            .iter()
            .all(|p| p.iter().all(|v| v.is_finite())
                && p[0] >= 0.0
                && p[1] >= 0.0
                && p[0] <= page.width as f64
                && p[1] <= page.height as f64),
        "polygon outside page"
    );
    ensure!(area(&anchor.polygon) > 0.01, "polygon area is empty");
    let signs: Vec<_> = (0..anchor.polygon.len())
        .map(|i| {
            cross(
                anchor.polygon[i],
                anchor.polygon[(i + 1) % anchor.polygon.len()],
                anchor.polygon[(i + 2) % anchor.polygon.len()],
            )
        })
        .collect();
    ensure!(
        signs.iter().all(|s| *s > 1e-8) || signs.iter().all(|s| *s < -1e-8),
        "polygon must be strictly convex and ordered"
    );
    for i in 0..anchor.polygon.len() {
        for j in i + 1..anchor.polygon.len() {
            if j == i + 1 || (i == 0 && j + 1 == anchor.polygon.len()) {
                continue;
            }
            ensure!(
                !segments_intersect(
                    anchor.polygon[i],
                    anchor.polygon[(i + 1) % anchor.polygon.len()],
                    anchor.polygon[j],
                    anchor.polygon[(j + 1) % anchor.polygon.len()]
                ),
                "polygon self-intersects"
            );
        }
    }
    ensure!(
        (anchor.source_kind == SourceKind::IdentityCard) == (anchor.side != Side::NotApplicable),
        "card source requires front/back; other sources require not_applicable"
    );
    Ok(())
}
fn validate_graph(
    pages: &[Page],
    entities: &[Entity],
    observations: &[Observation],
    fields: &[Field],
    prediction: bool,
    transcripts: &[Transcript],
) -> Result<()> {
    ensure!(
        !entities.is_empty()
            && entities.len() <= MAX_ENTITIES
            && observations.len() <= MAX_OBSERVATIONS
            && !fields.is_empty()
            && fields.len() <= MAX_FIELDS
            && transcripts.len() <= 2048,
        "v2 graph budget exceeded"
    );
    let mut em = BTreeMap::new();
    for e in entities {
        ensure!(
            id(&e.id) && em.insert(e.id.as_str(), e).is_none(),
            "invalid/duplicate entity ID"
        );
        validate_anchor(&e.anchor, pages)?;
    }
    for e in entities {
        match e.kind {
            EntityKind::Document => ensure!(
                e.document_id.is_none() && e.party_id.is_none() && e.role.is_none(),
                "document has invalid parent/role"
            ),
            EntityKind::Party => ensure!(
                entity_kind(&em, e.document_id.as_deref()) == Some(EntityKind::Document)
                    && e.party_id.is_none()
                    && e.role.is_some(),
                "party has invalid document/role"
            ),
            EntityKind::Card => {
                ensure!(
                    entity_kind(&em, e.document_id.as_deref()) == Some(EntityKind::Document)
                        && entity_kind(&em, e.party_id.as_deref()) == Some(EntityKind::Party)
                        && e.role.is_none(),
                    "card has invalid document/party"
                );
                ensure!(
                    em[e.party_id.as_deref().unwrap()].document_id == e.document_id,
                    "card party belongs to another document"
                );
            }
        }
    }
    let mut tm = BTreeMap::new();
    let mut total_text = 0usize;
    for t in transcripts {
        ensure!(
            id(&t.id) && tm.insert(t.id.as_str(), t).is_none() && t.text.len() <= 65536,
            "invalid/duplicate transcript"
        );
        validate_anchor(&t.anchor, pages)?;
        total_text += t.text.len();
    }
    ensure!(
        total_text <= 4 * 1024 * 1024,
        "transcript byte budget exceeded"
    );
    let mut om = BTreeMap::new();
    for o in observations {
        ensure!(
            id(&o.id)
                && om.insert(o.id.as_str(), o).is_none()
                && field_key(&o.field)
                && o.visible_text.len() <= 4096,
            "invalid/duplicate observation"
        );
        validate_target(&em, &o.document_id, o.party_id.as_deref())?;
        if let Some(card) = o.card_id.as_deref() {
            ensure!(
                em.get(card).is_some_and(|c| c.kind == EntityKind::Card
                    && c.document_id.as_deref() == Some(o.document_id.as_str())
                    && c.party_id == o.party_id)
                    && o.anchor.source_kind == SourceKind::IdentityCard,
                "observation card attribution invalid"
            );
        }
        ensure!(
            (o.anchor.source_kind == SourceKind::IdentityCard) == o.card_id.is_some(),
            "card observations require card identity"
        );
        validate_anchor(&o.anchor, pages)?;
        ensure!(
            (o.state == ObservationState::Present) == o.value.is_some(),
            "observation state/value inconsistent"
        );
        if let Some(v) = &o.value {
            ensure!(
                crate::value_type_matches(&o.field, v),
                "observation field type mismatch"
            );
            normalized_value(v)?;
        }
        ensure!(
            o.state != ObservationState::Present || !o.visible_text.is_empty(),
            "present observation needs visible transcript"
        );
        ensure!(
            o.state != ObservationState::SourceBlank || o.visible_text.is_empty(),
            "source_blank must have empty transcript"
        );
        if prediction {
            ensure!(
                o.origin != Origin::HumanGold,
                "prediction contains gold/human origin"
            );
            let ev = o
                .evidence
                .as_ref()
                .context("machine observation requires structural evidence")?;
            let t = tm
                .get(ev.transcript_id.as_str())
                .context("dangling transcript reference")?;
            ensure!(
                ev.byte_start <= ev.byte_end
                    && t.text.get(ev.byte_start..ev.byte_end) == Some(o.visible_text.as_str())
                    && sha(&ev.quote_sha256)
                    && digest(o.visible_text.as_bytes()) == ev.quote_sha256,
                "evidence hash/UTF-8 byte span mismatch"
            );
            ensure!(
                o.state != ObservationState::SourceBlank
                    || (t.text.is_empty() && ev.byte_start == 0 && ev.byte_end == 0),
                "source_blank requires an empty source transcript region"
            );
            ensure!(
                t.anchor == o.anchor,
                "evidence geometry/granularity must equal its supplied transcript region"
            );
        } else {
            ensure!(
                o.origin == Origin::HumanGold && o.evidence.is_none(),
                "gold observations must be independently transcribed"
            );
        }
    }
    let mut keys = BTreeSet::new();
    let mut used = BTreeSet::new();
    for f in fields {
        validate_target(&em, &f.document_id, f.party_id.as_deref())?;
        ensure!(
            field_key(&f.field)
                && keys.insert((&f.document_id, &f.party_id, &f.field))
                && f.observation_ids.len() <= 32,
            "invalid/duplicate field target"
        );
        let mut unique = BTreeSet::new();
        let mut values = BTreeSet::new();
        for reference in &f.observation_ids {
            ensure!(unique.insert(reference), "duplicate candidate reference");
            let o = om
                .get(reference.as_str())
                .context("dangling candidate reference")?;
            ensure!(
                o.document_id == f.document_id && o.party_id == f.party_id && o.field == f.field,
                "candidate belongs to another target"
            );
            if let Some(v) = &o.value {
                values.insert(normalized_value(v)?);
            }
            used.insert(reference.as_str());
        }
        let obs: Vec<_> = f.observation_ids.iter().map(|s| om[s.as_str()]).collect();
        let expected = match f.state {
            State::SourceBlank => Some(ObservationState::SourceBlank),
            State::Unreadable => Some(ObservationState::Unreadable),
            State::Placeholder => Some(ObservationState::Placeholder),
            State::Unsupported => Some(ObservationState::Unsupported),
            _ => None,
        };
        let shape = match f.state {
            State::Found => {
                values.len() == 1 && obs.iter().all(|o| o.state == ObservationState::Present)
            }
            State::Conflict => {
                values.len() >= 2 && obs.iter().all(|o| o.state == ObservationState::Present)
            }
            State::Ambiguous => {
                !values.is_empty() && obs.iter().all(|o| o.state == ObservationState::Present)
            }
            State::Missing => obs.is_empty(),
            _ => !obs.is_empty() && obs.iter().all(|o| Some(o.state) == expected),
        };
        ensure!(shape, "field state/candidate shape inconsistent");
        if prediction && f.state == State::Found {
            ensure!(
                obs.iter().any(|o| o.origin == Origin::FirstPass),
                "found prediction requires first-pass support"
            );
        }
        ensure!(
            (f.state == State::Ambiguous) == f.ambiguity_reason.is_some(),
            "ambiguity reason/state inconsistent"
        );
        if f.state == State::Ambiguous && values.len() == 1 {
            ensure!(
                f.ambiguity_reason == Some(AmbiguityReason::RecoveryOnly),
                "singleton ambiguity must declare recovery_only"
            );
        }
        if prediction && f.ambiguity_reason == Some(AmbiguityReason::RecoveryOnly) {
            ensure!(
                obs.iter().all(|o| o.origin == Origin::Recovery),
                "recovery-only ambiguity contains first-pass observation"
            );
        }
    }
    ensure!(
        used.len() == observations.len(),
        "unreferenced observation outside exhaustive scoring scope"
    );
    Ok(())
}
fn entity_kind(entities: &BTreeMap<&str, &Entity>, id: Option<&str>) -> Option<EntityKind> {
    id.and_then(|s| entities.get(s)).map(|e| e.kind)
}
fn validate_target(
    entities: &BTreeMap<&str, &Entity>,
    document: &str,
    party: Option<&str>,
) -> Result<()> {
    ensure!(
        entity_kind(entities, Some(document)) == Some(EntityKind::Document),
        "target document missing"
    );
    if let Some(p) = party {
        ensure!(
            entities.get(p).is_some_and(
                |e| e.kind == EntityKind::Party && e.document_id.as_deref() == Some(document)
            ),
            "target party/document mismatch"
        );
    }
    Ok(())
}
/// Typed canonicalization is versioned and deliberately preserves case, accents,
/// identifier zeros, currency, period and unit. Raw transcript is never rewritten.
pub fn normalized_value(value: &FieldValue) -> Result<String> {
    let normalized = match value {
        FieldValue::Text { value } => {
            ensure!(
                !value.is_empty() && value.len() <= 4096,
                "text value out of bounds"
            );
            FieldValue::Text {
                value: value.nfc().collect(),
            }
        }
        FieldValue::Identifier { value } => {
            ensure!(
                !value.is_empty() && value.len() <= 128,
                "identifier out of bounds"
            );
            FieldValue::Identifier {
                value: value.nfc().collect(),
            }
        }
        // Existing typed checks preserve units and validate calendar dates and decimals.
        _ => {
            return crate::normalized_value(value);
        }
    };
    Ok(serde_json::to_string(&normalized)?)
}
fn cross(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}
fn segments_intersect(a: [f64; 2], b: [f64; 2], c: [f64; 2], d: [f64; 2]) -> bool {
    let ab_c = cross(a, b, c);
    let ab_d = cross(a, b, d);
    let cd_a = cross(c, d, a);
    let cd_b = cross(c, d, b);
    if ab_c * ab_d < 0.0 && cd_a * cd_b < 0.0 {
        return true;
    }
    let on = |p: [f64; 2], q: [f64; 2], r: [f64; 2]| {
        r[0] >= p[0].min(q[0]) - 1e-8
            && r[0] <= p[0].max(q[0]) + 1e-8
            && r[1] >= p[1].min(q[1]) - 1e-8
            && r[1] <= p[1].max(q[1]) + 1e-8
    };
    (ab_c.abs() < 1e-8 && on(a, b, c))
        || (ab_d.abs() < 1e-8 && on(a, b, d))
        || (cd_a.abs() < 1e-8 && on(c, d, a))
        || (cd_b.abs() < 1e-8 && on(c, d, b))
}
fn signed_area(p: &[[f64; 2]]) -> f64 {
    (0..p.len())
        .map(|i| p[i][0] * p[(i + 1) % p.len()][1] - p[(i + 1) % p.len()][0] * p[i][1])
        .sum::<f64>()
        / 2.0
}
fn area(p: &[[f64; 2]]) -> f64 {
    signed_area(p).abs()
}
fn intersection(a: &[[f64; 2]], b: &[[f64; 2]]) -> f64 {
    let mut output = a.to_vec();
    let sign = signed_area(b).signum();
    for i in 0..b.len() {
        let edge_a = b[i];
        let edge_b = b[(i + 1) % b.len()];
        let input = std::mem::take(&mut output);
        if input.is_empty() {
            break;
        }
        let mut previous = *input.last().unwrap();
        for current in input {
            let pc = cross(edge_a, edge_b, previous) * sign;
            let cc = cross(edge_a, edge_b, current) * sign;
            if (cc >= -1e-8) != (pc >= -1e-8) {
                let t = pc / (pc - cc);
                output.push([
                    previous[0] + t * (current[0] - previous[0]),
                    previous[1] + t * (current[1] - previous[1]),
                ]);
            }
            if cc >= -1e-8 {
                output.push(current);
            }
            previous = current;
        }
    }
    area(&output)
}
fn geometry(a: &Anchor, b: &Anchor) -> (f64, f64, f64) {
    let aa = area(&a.polygon);
    let ba = area(&b.polygon);
    let i = intersection(&a.polygon, &b.polygon).clamp(0.0, aa.min(ba));
    (i / (aa + ba - i), i / ba, (aa - i) / aa)
}
fn compatible(a: &Anchor, b: &Anchor) -> bool {
    a.page_number == b.page_number && a.source_kind == b.source_kind && a.side == b.side
}
/// Conservative mutual unique best >= 0.5 IoU. Ties abstain; iteration order and
/// values cannot alter attribution. Results are frozen for the complete case.
fn unique_mapping<P, G>(
    predicted: &[P],
    gold: &[G],
    similarity: impl Fn(&P, &G) -> Option<f64>,
) -> BTreeMap<usize, usize> {
    // Streaming mutual-best state uses O(P + G) memory, not a P*G matrix.
    let mut forward = vec![(0.0_f64, None, false); predicted.len()];
    let mut backward = vec![(0.0_f64, None, false); gold.len()];
    fn offer(slot: &mut (f64, Option<usize>, bool), score: f64, index: usize) {
        if score > slot.0 + 1e-8 {
            *slot = (score, Some(index), false);
        } else if (score - slot.0).abs() < 1e-8 {
            slot.2 = true;
        }
    }
    for (pi, p) in predicted.iter().enumerate() {
        for (gi, g) in gold.iter().enumerate() {
            if let Some(score) = similarity(p, g) {
                offer(&mut forward[pi], score, gi);
                offer(&mut backward[gi], score, pi);
            }
        }
    }
    forward
        .into_iter()
        .enumerate()
        .filter_map(|(pi, (score, gi, tie))| {
            gi.filter(|&gi| !tie && score >= 0.5 && !backward[gi].2 && backward[gi].1 == Some(pi))
                .map(|gi| (pi, gi))
        })
        .collect()
}
#[derive(Debug, Clone, Default, Serialize)]
pub struct Metrics {
    pub cases: u64,
    pub all_critical_cases_correct: u64,
    pub all_critical_accuracy: Option<f64>,
    pub entities_gold: u64,
    pub entities_matched: u64,
    pub entities_exact: u64,
    pub entities_missing: u64,
    pub entities_extra: u64,
    pub fields_gold: u64,
    pub field_state_matches: u64,
    pub field_value_state_matches: u64,
    pub field_grounded_exact: u64,
    pub fields_extra: u64,
    pub field_value_state_accuracy: Option<f64>,
    pub field_grounded_accuracy: Option<f64>,
    pub candidates_gold: u64,
    pub candidates_exact: u64,
    pub candidates_missing: u64,
    pub candidates_extra: u64,
    pub candidate_precision: Option<f64>,
    pub candidate_recall: Option<f64>,
    pub expected_conflicts: u64,
    pub conflict_complete: u64,
    pub conflict_complete_recall: Option<f64>,
    pub roles_gold: u64,
    pub roles_exact: u64,
    pub observations_gold: u64,
    pub observations_matched: u64,
    pub observations_exact: u64,
    pub page_transform_mismatches: u64,
    pub page_transform_extra: u64,
    pub cer_reference_characters: u64,
    pub cer_edit_distance: u64,
    pub cer_pairs: u64,
    pub cer_unavailable_pairs: u64,
    pub character_error_rate: Option<f64>,
    pub geometry_pairs: u64,
    pub geometry_mean_coverage: Option<f64>,
    pub geometry_mean_extraneous: Option<f64>,
    pub geometry_by_granularity: BTreeMap<Granularity, GeometryMetrics>,
    #[serde(skip)]
    coverage_sum: f64,
    #[serde(skip)]
    extraneous_sum: f64,
}
#[derive(Debug, Clone, Default, Serialize)]
pub struct GeometryMetrics {
    pub pairs: u64,
    pub mean_coverage: Option<f64>,
    pub mean_extraneous: Option<f64>,
    #[serde(skip)]
    coverage: f64,
    #[serde(skip)]
    extraneous: f64,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub schema_version: String,
    pub synthetic: bool,
    pub split: Split,
    pub normalization_policy: String,
    pub entity_matching_policy: String,
    pub metrics: Metrics,
    pub cohorts: BTreeMap<Cohort, Metrics>,
    pub qualities: BTreeMap<Quality, Metrics>,
    pub machine_auto_acceptance_enabled: bool,
    pub structural_integrity: String,
    pub pixel_correctness: String,
    pub limitations: Vec<String>,
}
impl Metrics {
    fn merge(&mut self, other: &Self) {
        self.cases += other.cases;
        self.all_critical_cases_correct += other.all_critical_cases_correct;
        self.entities_gold += other.entities_gold;
        self.entities_matched += other.entities_matched;
        self.entities_exact += other.entities_exact;
        self.entities_missing += other.entities_missing;
        self.entities_extra += other.entities_extra;
        self.fields_gold += other.fields_gold;
        self.field_state_matches += other.field_state_matches;
        self.field_value_state_matches += other.field_value_state_matches;
        self.field_grounded_exact += other.field_grounded_exact;
        self.fields_extra += other.fields_extra;
        self.candidates_gold += other.candidates_gold;
        self.candidates_exact += other.candidates_exact;
        self.candidates_missing += other.candidates_missing;
        self.candidates_extra += other.candidates_extra;
        self.expected_conflicts += other.expected_conflicts;
        self.conflict_complete += other.conflict_complete;
        self.roles_gold += other.roles_gold;
        self.roles_exact += other.roles_exact;
        self.observations_gold += other.observations_gold;
        self.observations_matched += other.observations_matched;
        self.observations_exact += other.observations_exact;
        self.page_transform_mismatches += other.page_transform_mismatches;
        self.page_transform_extra += other.page_transform_extra;
        self.cer_reference_characters += other.cer_reference_characters;
        self.cer_edit_distance += other.cer_edit_distance;
        self.cer_pairs += other.cer_pairs;
        self.cer_unavailable_pairs += other.cer_unavailable_pairs;
        self.geometry_pairs += other.geometry_pairs;
        self.coverage_sum += other.coverage_sum;
        self.extraneous_sum += other.extraneous_sum;
        for (key, value) in &other.geometry_by_granularity {
            let entry = self.geometry_by_granularity.entry(*key).or_default();
            entry.pairs += value.pairs;
            entry.coverage += value.coverage;
            entry.extraneous += value.extraneous;
        }
    }
    fn finish(&mut self) {
        self.all_critical_accuracy = ratio(self.all_critical_cases_correct, self.cases);
        self.field_value_state_accuracy = ratio(self.field_value_state_matches, self.fields_gold);
        self.field_grounded_accuracy = ratio(self.field_grounded_exact, self.fields_gold);
        self.candidate_precision = ratio(
            self.candidates_exact,
            self.candidates_exact + self.candidates_extra,
        );
        self.candidate_recall = ratio(self.candidates_exact, self.candidates_gold);
        self.conflict_complete_recall = ratio(self.conflict_complete, self.expected_conflicts);
        self.character_error_rate = ratio(self.cer_edit_distance, self.cer_reference_characters);
        self.geometry_mean_coverage =
            (self.geometry_pairs > 0).then(|| self.coverage_sum / self.geometry_pairs as f64);
        self.geometry_mean_extraneous =
            (self.geometry_pairs > 0).then(|| self.extraneous_sum / self.geometry_pairs as f64);
        for g in self.geometry_by_granularity.values_mut() {
            g.mean_coverage = Some(g.coverage / g.pairs as f64);
            g.mean_extraneous = Some(g.extraneous / g.pairs as f64);
        }
    }
}
fn ratio(n: u64, d: u64) -> Option<f64> {
    (d > 0).then(|| n as f64 / d as f64)
}
pub fn score(dataset: &Dataset, split: Split) -> Result<Report> {
    let cases: Vec<_> = dataset
        .manifest
        .cases
        .iter()
        .filter(|c| c.split == split)
        .collect();
    ensure!(!cases.is_empty(), "requested v2 split empty");
    let mut report=Report{schema_version:"ocr-eval-report/2.0.0".into(),synthetic:dataset.manifest.synthetic,split,normalization_policy:NORMALIZATION.into(),entity_matching_policy:"mutual_unique_geometry_iou_ge_0.5_frozen_per_case/1.0.0".into(),metrics:Metrics::default(),cohorts:BTreeMap::new(),qualities:BTreeMap::new(),machine_auto_acceptance_enabled:false,structural_integrity:"source_and_prediction_hashes; UTF-8 spans; quote hashes; graph references; transcript geometry".into(),pixel_correctness:"independent gold annotation is required; no automatic pixel verification is claimed".into(),limitations:vec!["No calibrated confidence, acceptance policy, statistical risk bound or human-correction scoring.".into(),"Convex polygons only; evidence geometry cannot be finer than its transcript region. CER uses NFC Unicode scalar values, preserves accents/case/whitespace, and excludes unmatched or >1024-character pairs with explicit counts.".into(),"Entity anchors must be independently annotated; ambiguous geometry ties abstain instead of rematching field values.".into(),"Synthetic cases validate mechanics only. Representative random-stream sampling and held-out challenge labels remain external dataset requirements.".into(),"V1 business artifacts lack card/document/region identities. A reviewed machine-only v2 adapter is required before scoring live jobs; no v1 geometry is invented.".into()]};
    for case in cases {
        let prediction = dataset.prediction(case)?;
        let mut case_metrics = Metrics::default();
        score_case(case, &prediction, &mut case_metrics)?;
        report.metrics.merge(&case_metrics);
        report
            .cohorts
            .entry(case.cohort)
            .or_default()
            .merge(&case_metrics);
        report
            .qualities
            .entry(case.quality)
            .or_default()
            .merge(&case_metrics);
    }
    report.metrics.finish();
    for m in report
        .cohorts
        .values_mut()
        .chain(report.qualities.values_mut())
    {
        m.finish();
    }
    Ok(report)
}
fn mapped_ref<'a>(
    id: Option<&str>,
    pred: &'a [Entity],
    gold: &'a [Entity],
    mapping: &BTreeMap<usize, usize>,
) -> Option<&'a str> {
    id.and_then(|s| pred.iter().position(|e| e.id == s))
        .and_then(|p| mapping.get(&p))
        .map(|g| gold[*g].id.as_str())
}
fn target_matches(
    doc: &str,
    party: Option<&str>,
    gd: &str,
    gp: Option<&str>,
    p: &Prediction,
    c: &Case,
    m: &BTreeMap<usize, usize>,
) -> bool {
    mapped_ref(Some(doc), &p.entities, &c.entities, m) == Some(gd)
        && (party.is_none() && gp.is_none()
            || party.is_some()
                && gp.is_some()
                && mapped_ref(party, &p.entities, &c.entities, m) == gp)
}
fn score_case(c: &Case, p: &Prediction, m: &mut Metrics) -> Result<()> {
    m.cases += 1;
    let transforms_ok = |page: u32| {
        c.pages.iter().find(|x| x.number == page) == p.pages.iter().find(|x| x.number == page)
    };
    let mismatches = c.pages.iter().filter(|x| !transforms_ok(x.number)).count();
    let extra_pages = p
        .pages
        .iter()
        .filter(|x| !c.pages.iter().any(|g| g.number == x.number))
        .count();
    m.page_transform_mismatches += mismatches as u64;
    m.page_transform_extra += extra_pages as u64;
    let em = unique_mapping(&p.entities, &c.entities, |a, b| {
        (a.kind == b.kind
            && compatible(&a.anchor, &b.anchor)
            && transforms_ok(a.anchor.page_number))
        .then(|| geometry(&a.anchor, &b.anchor).0)
    });
    m.entities_gold += c.entities.len() as u64;
    m.entities_matched += em.len() as u64;
    m.entities_missing += (c.entities.len() - em.len()) as u64;
    m.entities_extra += (p.entities.len() - em.len()) as u64;
    let mut entities_exact = BTreeSet::new();
    for (&pi, &gi) in &em {
        let a = &p.entities[pi];
        let b = &c.entities[gi];
        let exact = a.role == b.role
            && mapped_ref(a.document_id.as_deref(), &p.entities, &c.entities, &em)
                == b.document_id.as_deref()
            && mapped_ref(a.party_id.as_deref(), &p.entities, &c.entities, &em)
                == b.party_id.as_deref();
        if exact {
            entities_exact.insert(gi);
            m.entities_exact += 1;
        }
    }
    for (gi, g) in c
        .entities
        .iter()
        .enumerate()
        .filter(|(_, g)| g.kind == EntityKind::Party)
    {
        m.roles_gold += 1;
        m.roles_exact += u64::from(
            em.iter()
                .any(|(pi, mapped)| *mapped == gi && p.entities[*pi].role == g.role),
        );
    }
    let om = unique_mapping(&p.observations, &c.observations, |a, b| {
        let card_ok = a.card_id.is_none() && b.card_id.is_none()
            || a.card_id.is_some()
                && b.card_id.is_some()
                && mapped_ref(a.card_id.as_deref(), &p.entities, &c.entities, &em)
                    == b.card_id.as_deref();
        (a.field == b.field
            && target_matches(
                &a.document_id,
                a.party_id.as_deref(),
                &b.document_id,
                b.party_id.as_deref(),
                p,
                c,
                &em,
            )
            && card_ok
            && compatible(&a.anchor, &b.anchor)
            && transforms_ok(a.anchor.page_number))
        .then(|| geometry(&a.anchor, &b.anchor).0)
    });
    let mut obs_exact = BTreeSet::new();
    m.observations_gold += c.observations.len() as u64;
    m.observations_matched += om.len() as u64;
    m.cer_unavailable_pairs += (c.observations.len() - om.len()) as u64;
    for (&pi, &gi) in &om {
        let a = &p.observations[pi];
        let b = &c.observations[gi];
        let (_, coverage, extraneous) = geometry(&a.anchor, &b.anchor);
        m.geometry_pairs += 1;
        m.coverage_sum += coverage;
        m.extraneous_sum += extraneous;
        let gm = m
            .geometry_by_granularity
            .entry(a.anchor.granularity)
            .or_default();
        gm.pairs += 1;
        gm.coverage += coverage;
        gm.extraneous += extraneous;
        let av = a.visible_text.nfc().collect::<String>();
        let bv = b.visible_text.nfc().collect::<String>();
        let ac: Vec<_> = av.chars().collect();
        let bc: Vec<_> = bv.chars().collect();
        if ac.len() <= MAX_CER_CHARS && bc.len() <= MAX_CER_CHARS {
            m.cer_pairs += 1;
            m.cer_reference_characters += bc.len() as u64;
            m.cer_edit_distance += levenshtein(&ac, &bc) as u64;
        } else {
            m.cer_unavailable_pairs += 1;
        }
        let entity_ok = [
            Some(b.document_id.as_str()),
            b.party_id.as_deref(),
            b.card_id.as_deref(),
        ]
        .into_iter()
        .flatten()
        .all(|s| {
            c.entities
                .iter()
                .position(|e| e.id == s)
                .is_some_and(|i| entities_exact.contains(&i))
        });
        if entity_ok
            && a.state == b.state
            && a.value.as_ref().map(normalized_value).transpose()?
                == b.value.as_ref().map(normalized_value).transpose()?
            && av == bv
            && a.anchor.granularity == b.anchor.granularity
            && coverage >= 0.9
            && extraneous <= 0.1
        {
            obs_exact.insert(gi);
            m.observations_exact += 1;
        }
    }
    let mut used_fields = BTreeSet::new();
    let mut critical_ok = true;
    for g in &c.fields {
        m.fields_gold += 1;
        let actual = p.fields.iter().enumerate().find(|(_, a)| {
            a.field == g.target.field
                && target_matches(
                    &a.document_id,
                    a.party_id.as_deref(),
                    &g.target.document_id,
                    g.target.party_id.as_deref(),
                    p,
                    c,
                    &em,
                )
        });
        let expected: BTreeSet<_> = g
            .target
            .observation_ids
            .iter()
            .map(|s| c.observations.iter().position(|o| &o.id == s).unwrap())
            .collect();
        m.candidates_gold += expected.len() as u64;
        let mut exact = false;
        if let Some((index, a)) = actual {
            used_fields.insert(index);
            let actual_obs: Vec<_> = a
                .observation_ids
                .iter()
                .map(|s| p.observations.iter().position(|o| &o.id == s).unwrap())
                .collect();
            let mapped: BTreeSet<_> = actual_obs
                .iter()
                .filter_map(|i| om.get(i).copied())
                .collect();
            let correct = mapped
                .intersection(&expected)
                .filter(|i| obs_exact.contains(i))
                .count();
            m.candidates_exact += correct as u64;
            m.candidates_missing += (expected.len() - correct) as u64;
            m.candidates_extra += (actual_obs.len() - correct) as u64;
            let state_ok =
                a.state == g.target.state && a.ambiguity_reason == g.target.ambiguity_reason;
            m.field_state_matches += u64::from(state_ok);
            let expected_values = values_for(&g.target, &c.observations)?;
            let actual_values = values_for(a, &p.observations)?;
            m.field_value_state_matches += u64::from(state_ok && expected_values == actual_values);
            exact = state_ok
                && correct == expected.len()
                && correct == actual_obs.len()
                && [
                    Some(g.target.document_id.as_str()),
                    g.target.party_id.as_deref(),
                ]
                .into_iter()
                .flatten()
                .all(|s| {
                    c.entities
                        .iter()
                        .position(|e| e.id == s)
                        .is_some_and(|i| entities_exact.contains(&i))
                });
            m.field_grounded_exact += u64::from(exact);
        } else {
            m.candidates_missing += expected.len() as u64;
        }
        if g.target.state == State::Conflict {
            m.expected_conflicts += 1;
            m.conflict_complete += u64::from(exact);
        }
        if g.critical && !exact {
            critical_ok = false;
        }
    }
    let extra_fields = p.fields.len() - used_fields.len();
    m.fields_extra += extra_fields as u64;
    // Candidates attached to extra/unmatched fields remain false positives.
    m.candidates_extra += p
        .fields
        .iter()
        .enumerate()
        .filter(|(i, _)| !used_fields.contains(i))
        .map(|(_, f)| f.observation_ids.len() as u64)
        .sum::<u64>();
    m.all_critical_cases_correct += u64::from(
        critical_ok
            && em.len() == p.entities.len()
            && em.len() == c.entities.len()
            && entities_exact.len() == c.entities.len()
            && extra_fields == 0
            && om.len() == p.observations.len()
            && mismatches == 0
            && extra_pages == 0,
    );
    Ok(())
}
fn values_for(field: &Field, obs: &[Observation]) -> Result<BTreeSet<String>> {
    field
        .observation_ids
        .iter()
        .filter_map(|s| {
            obs.iter()
                .find(|o| &o.id == s)
                .and_then(|o| o.value.as_ref())
        })
        .map(normalized_value)
        .collect()
}
fn levenshtein(a: &[char], b: &[char]) -> usize {
    let mut row: Vec<_> = (0..=b.len()).collect();
    for (i, ac) in a.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, bc) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (row[j] + 1)
                .min(above + 1)
                .min(diagonal + usize::from(ac != bc));
            diagonal = above;
        }
    }
    row[b.len()]
}
