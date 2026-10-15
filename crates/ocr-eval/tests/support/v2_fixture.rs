use ocr_eval::{Split, digest, v2};
use serde_json::{Value, json};
use std::path::Path;

fn anchor(page: u32, rect: [f64; 4], card: bool) -> Value {
    json!({"page_number":page,"source_kind":if card{"identity_card"}else{"contract"},"side":if card{"front"}else{"not_applicable"},"granularity":"line","polygon":[[rect[0],rect[1]],[rect[2],rect[1]],[rect[2],rect[3]],[rect[0],rect[3]]]})
}
fn entity(
    id: &str,
    kind: &str,
    doc: Option<&str>,
    party: Option<&str>,
    role: Option<&str>,
    anchor: Value,
) -> Value {
    json!({"id":id,"kind":kind,"document_id":doc,"party_id":party,"role":role,"anchor":anchor})
}
fn observation(
    id: &str,
    party: &str,
    field: &str,
    text: &str,
    value: Value,
    anchor: Value,
    card: Option<&str>,
) -> Value {
    json!({"id":id,"document_id":"gold-doc","party_id":party,"card_id":card,"field":field,"state":if value.is_null(){"source_blank"}else{"present"},"origin":"human_gold","visible_text":text,"value":value,"anchor":anchor,"evidence":null})
}
fn field(party: &str, key: &str, state: &str, ids: &[&str]) -> Value {
    json!({"target":{"document_id":"gold-doc","party_id":party,"field":key,"state":state,"observation_ids":ids,"ambiguity_reason":null},"critical":true,"annotation":{"annotator_ids":["labeler-1","labeler-2"],"prediction_hidden":true,"pixels_checked":true,"adjudicated":true,"adjudicator_id":"adjudicator-3"}})
}
pub fn fixture() -> (Value, Value) {
    let pages = json!([
        {"number":1,"width":1000,"height":1000,"rotation_degrees":0,"crop_box_pdf":[0.0,0.0,500.0,500.0]},
        {"number":2,"width":1000,"height":1000,"rotation_degrees":0,"crop_box_pdf":[0.0,0.0,500.0,500.0]}
    ]);
    let entities = json!([
        entity(
            "gold-doc",
            "document",
            None,
            None,
            None,
            anchor(1, [0.0, 0.0, 1000.0, 600.0], false)
        ),
        entity(
            "gold-a",
            "party",
            Some("gold-doc"),
            None,
            Some("landlord"),
            anchor(1, [20.0, 50.0, 200.0, 80.0], false)
        ),
        entity(
            "gold-b",
            "party",
            Some("gold-doc"),
            None,
            Some("tenant"),
            anchor(1, [20.0, 150.0, 200.0, 180.0], false)
        ),
        entity(
            "gold-card-a",
            "card",
            Some("gold-doc"),
            Some("gold-a"),
            None,
            anchor(2, [10.0, 10.0, 490.0, 490.0], true)
        ),
        entity(
            "gold-card-b",
            "card",
            Some("gold-doc"),
            Some("gold-b"),
            None,
            anchor(2, [510.0, 10.0, 990.0, 490.0], true)
        )
    ]);
    let observations = json!([
        observation(
            "obs-a-name",
            "gold-a",
            "name",
            "Nguyễn A",
            json!({"type":"text","value":"Nguyễn A"}),
            anchor(1, [20.0, 50.0, 200.0, 80.0], false),
            None
        ),
        observation(
            "obs-b-name",
            "gold-b",
            "name",
            "Trần B",
            json!({"type":"text","value":"Trần B"}),
            anchor(1, [20.0, 150.0, 200.0, 180.0], false),
            None
        ),
        observation(
            "obs-b-contract",
            "gold-b",
            "birth_date",
            "15/02/1990",
            json!({"type":"date","value":"1990-02-15"}),
            anchor(1, [20.0, 200.0, 350.0, 220.0], false),
            None
        ),
        observation(
            "obs-b-card",
            "gold-b",
            "birth_date",
            "20/08/1992",
            json!({"type":"date","value":"1992-08-20"}),
            anchor(2, [550.0, 100.0, 800.0, 120.0], true),
            Some("gold-card-b")
        ),
        observation(
            "obs-a-blank",
            "gold-a",
            "birth_date",
            "",
            Value::Null,
            anchor(1, [20.0, 90.0, 350.0, 110.0], false),
            None
        )
    ]);
    let fields = json!([
        field("gold-a", "name", "found", &["obs-a-name"]),
        field("gold-b", "name", "found", &["obs-b-name"]),
        field(
            "gold-b",
            "birth_date",
            "conflict",
            &["obs-b-contract", "obs-b-card"]
        ),
        field("gold-a", "birth_date", "source_blank", &["obs-a-blank"])
    ]);
    let source_sha = digest(b"synthetic source, not a real document");
    let case = json!({"id":"case-1","group_id":"group-1","template_family":"family-1","split":"test","cohort":"representative","quality":"scan","source_sha256":source_sha,"source_path":"source.txt","prediction_path":"prediction.json","prediction_sha256":"0".repeat(64),"pages":pages,"entities":entities,"observations":observations,"fields":fields});
    let manifest = json!({"schema_version":v2::SCHEMA,"normalization_policy":v2::NORMALIZATION,"dataset_name":"synthetic-v2","synthetic":true,"cases":[case]});
    let mut prediction = json!({"schema_version":"ocr-eval-prediction/2.0.0","normalization_policy":v2::NORMALIZATION,"source_sha256":source_sha,"machine_only":true,"pages":pages,"entities":entities,"transcripts":[],"observations":observations,"fields":fields.as_array().unwrap().iter().map(|f|f["target"].clone()).collect::<Vec<_>>()});
    // Runtime IDs intentionally differ from independently annotated gold IDs.
    let rename = |s: &str| format!("pred-{s}");
    for e in prediction["entities"].as_array_mut().unwrap() {
        e["id"] = json!(rename(e["id"].as_str().unwrap()));
        for key in ["document_id", "party_id"] {
            if let Some(id) = e[key].as_str() {
                e[key] = json!(rename(id));
            }
        }
    }
    for o in prediction["observations"].as_array_mut().unwrap() {
        o["id"] = json!(rename(o["id"].as_str().unwrap()));
        for key in ["document_id", "party_id", "card_id"] {
            if let Some(id) = o[key].as_str() {
                o[key] = json!(rename(id));
            }
        }
        o["origin"] = json!("first_pass");
    }
    for f in prediction["fields"].as_array_mut().unwrap() {
        for key in ["document_id", "party_id"] {
            if let Some(id) = f[key].as_str() {
                f[key] = json!(rename(id));
            }
        }
        for id in f["observation_ids"].as_array_mut().unwrap() {
            *id = json!(rename(id.as_str().unwrap()));
        }
    }
    refresh_evidence(&mut prediction);
    (manifest, prediction)
}
pub fn refresh_evidence(prediction: &mut Value) {
    let mut transcripts = Vec::new();
    for (i, o) in prediction["observations"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        let text = o["visible_text"].as_str().unwrap();
        let id = format!("transcript-{i}");
        let bytes = text.len();
        let hash = digest(text.as_bytes());
        transcripts.push(json!({"id":id,"text":text,"anchor":o["anchor"]}));
        o["evidence"] =
            json!({"transcript_id":id,"byte_start":0,"byte_end":bytes,"quote_sha256":hash});
    }
    prediction["transcripts"] = json!(transcripts);
}
pub fn save(base: &Path, manifest: &mut Value, prediction: &Value) -> std::path::PathBuf {
    std::fs::write(
        base.join("source.txt"),
        b"synthetic source, not a real document",
    )
    .unwrap();
    let bytes = serde_json::to_vec(prediction).unwrap();
    std::fs::write(base.join("prediction.json"), &bytes).unwrap();
    manifest["cases"][0]["prediction_sha256"] = json!(digest(&bytes));
    let path = base.join("manifest.json");
    std::fs::write(&path, serde_json::to_vec(manifest).unwrap()).unwrap();
    path
}
pub fn report(manifest: &mut Value, prediction: &Value) -> anyhow::Result<v2::Report> {
    let dir = tempfile::tempdir()?;
    let path = save(dir.path(), manifest, prediction);
    v2::score(&v2::Dataset::load(&path)?, Split::Test)
}
