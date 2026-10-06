use automerge::marks::{ExpandMark, Mark};
use automerge::sync::{Capability, Have, Message, State, SyncDoc};
use automerge::transaction::{CommitOptions, Transactable};
use automerge::{
    ActorId, AutoCommit, ChangeHash, ObjId, ObjType, ReadDoc, ScalarValue, ROOT,
};
use base64::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::ffi::{c_char, CStr};
use std::str::FromStr;

extern "C" {
    fn malloc(size: usize) -> *mut u8;
}

fn to_c_string(s: String) -> *mut c_char {
    let bytes = s.into_bytes();
    let len = bytes.len();
    unsafe {
        let ptr = malloc(len + 1);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, len);
        *ptr.add(len) = 0;
        ptr as *mut c_char
    }
}

// --- Standard JSON Response Envelope ---

#[derive(Serialize)]
struct Response<T> {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn ok<T: Serialize>(val: T) -> *mut c_char {
    let resp = Response {
        success: true,
        data: Some(val),
        error: None,
    };
    to_c_string(serde_json::to_string(&resp).unwrap())
}

fn err(msg: impl ToString) -> *mut c_char {
    let resp: Response<()> = Response {
        success: false,
        data: None,
        error: Some(msg.to_string()),
    };
    to_c_string(serde_json::to_string(&resp).unwrap())
}

// --- Path Segment Handling ---

#[derive(Deserialize, Clone, Debug)]
#[serde(untagged)]
enum PathSegment {
    Key(String),
    Index(usize),
}

// --- Action DSL Definitions ---

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
enum Action {
    Put {
        path: Vec<PathSegment>,
        value: serde_json::Value,
    },
    Counter {
        path: Vec<PathSegment>,
        value: i64,
    },
    Insert {
        path: Vec<PathSegment>,
        index: usize,
        value: serde_json::Value,
    },
    Delete {
        path: Vec<PathSegment>,
    },
    Inc {
        path: Vec<PathSegment>,
        value: i64,
    },
    Splicetext {
        path: Vec<PathSegment>,
        index: usize,
        del: isize,
        text: String,
    },
    Splicelist {
        path: Vec<PathSegment>,
        index: usize,
        del: isize,
        values: Vec<serde_json::Value>,
    },
    // Rich Text Marks (Peritext styling)
    Mark {
        path: Vec<PathSegment>,
        start: usize,
        end: usize,
        name: String,
        value: serde_json::Value,
    },
    Unmark {
        path: Vec<PathSegment>,
        start: usize,
        end: usize,
        name: String,
    },
}

#[derive(Deserialize)]
struct InitPayload {
    actor: String,
}

#[derive(Deserialize)]
struct ChangePayload {
    doc: String,
    actor: String,
    actions: Vec<Action>,
    message: Option<String>,
    time: Option<i64>,
}

#[derive(Deserialize)]
struct MergePayload {
    doc1: String,
    doc2: String,
}

#[derive(Deserialize)]
struct DocOnlyPayload {
    doc: String,
}

#[derive(Deserialize)]
struct GetPayload {
    doc: String,
    path: Vec<PathSegment>,
}

#[derive(Deserialize)]
struct DiffPayload {
    doc: String,
    before: Vec<String>,
    after: Vec<String>,
}

#[derive(Deserialize)]
struct SyncGenPayload {
    doc: String,
    sync_state: String, // base64
}

#[derive(Deserialize)]
struct SyncRecvPayload {
    doc: String,
    sync_state: String, // base64
    message: String,    // base64
}

#[derive(Deserialize)]
struct ForkAtPayload {
    doc: String,
    heads: Vec<String>,
}

#[derive(Deserialize)]
struct ConflictsPayload {
    doc: String,
    path: Vec<PathSegment>,
}

#[derive(Serialize)]
struct DocResult {
    doc: String,
}

#[derive(Serialize)]
struct SyncGenResult {
    doc: String,
    sync_state: String,
    message: Option<String>,
}

#[derive(Serialize)]
struct SyncRecvResult {
    doc: String,
    sync_state: String,
}

#[derive(Serialize, Deserialize)]
struct SyncStateData {
    shared_heads: Vec<String>,
    last_sent_heads: Vec<String>,
    their_heads: Option<Vec<String>>,
    their_need: Option<Vec<String>>,
    their_have: Option<Vec<SyncHaveData>>,
    sent_hashes: Vec<String>,
    in_flight: bool,
    have_responded: bool,
    their_capabilities: Option<Vec<String>>,
    read_only: bool,
    peer_read_only: bool,
    needs_reset: bool,
}

#[derive(Serialize, Deserialize)]
struct SyncHaveData {
    last_sync: Vec<String>,
}

fn encode_hash(hash: &ChangeHash) -> String {
    BASE64_STANDARD.encode(hash.0)
}

fn decode_hash(hash: &str) -> Result<ChangeHash, String> {
    let bytes = BASE64_STANDARD
        .decode(hash)
        .map_err(|error| format!("Invalid sync state hash: {}", error))?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "Invalid sync state hash length".to_string())?;
    Ok(ChangeHash(bytes))
}

fn encode_hashes(hashes: &[ChangeHash]) -> Vec<String> {
    hashes.iter().map(encode_hash).collect()
}

fn decode_hashes(hashes: &[String]) -> Result<Vec<ChangeHash>, String> {
    hashes.iter().map(|hash| decode_hash(hash)).collect()
}

fn encode_capability(capability: &Capability) -> String {
    match capability {
        Capability::MessageV1 => "message-v1",
        Capability::MessageV2 => "message-v2",
        Capability::SyncReset => "sync-reset",
    }
    .to_string()
}

fn decode_capability(capability: &str) -> Result<Capability, String> {
    match capability {
        "message-v1" => Ok(Capability::MessageV1),
        "message-v2" => Ok(Capability::MessageV2),
        "sync-reset" => Ok(Capability::SyncReset),
        _ => Err(format!("Unknown sync capability: {}", capability)),
    }
}

fn encode_sync_state(state: &State) -> Result<String, String> {
    let data = SyncStateData {
        shared_heads: encode_hashes(&state.shared_heads),
        last_sent_heads: encode_hashes(&state.last_sent_heads),
        their_heads: state.their_heads.as_ref().map(|heads| encode_hashes(heads)),
        their_need: state.their_need.as_ref().map(|heads| encode_hashes(heads)),
        their_have: state.their_have.as_ref().map(|have| {
            have.iter()
                .map(|item| SyncHaveData {
                    last_sync: encode_hashes(&item.last_sync),
                })
                .collect()
        }),
        sent_hashes: state.sent_hashes.iter().map(encode_hash).collect(),
        in_flight: state.in_flight,
        have_responded: state.have_responded,
        their_capabilities: state
            .their_capabilities
            .as_ref()
            .map(|caps| caps.iter().map(encode_capability).collect()),
        read_only: state.read_only,
        peer_read_only: state.peer_read_only,
        needs_reset: state.needs_reset,
    };
    let json = serde_json::to_vec(&data).map_err(|error| error.to_string())?;
    Ok(BASE64_STANDARD.encode(json))
}

fn decode_sync_state(encoded: &str) -> Result<State, String> {
    let bytes = BASE64_STANDARD
        .decode(encoded)
        .map_err(|error| format!("Invalid sync state: {}", error))?;
    if bytes.first() == Some(&0x43) {
        return State::decode(&bytes).map_err(|error| error.to_string());
    }
    let data: SyncStateData = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Invalid sync state data: {}", error))?;
    let their_have = data.their_have.map(|have| {
        have.into_iter()
            .map(|item| {
                Ok(Have {
                    last_sync: decode_hashes(&item.last_sync)?,
                    bloom: Default::default(),
                })
            })
            .collect::<Result<Vec<_>, String>>()
    }).transpose()?;
    Ok(State {
        shared_heads: decode_hashes(&data.shared_heads)?,
        last_sent_heads: decode_hashes(&data.last_sent_heads)?,
        their_heads: data.their_heads.as_ref().map(|heads| decode_hashes(heads)).transpose()?,
        their_need: data.their_need.as_ref().map(|heads| decode_hashes(heads)).transpose()?,
        their_have,
        sent_hashes: data
            .sent_hashes
            .iter()
            .map(|hash| decode_hash(hash))
            .collect::<Result<BTreeSet<_>, _>>()?,
        in_flight: data.in_flight,
        have_responded: data.have_responded,
        their_capabilities: data
            .their_capabilities
            .as_ref()
            .map(|caps| caps.iter().map(|cap| decode_capability(cap)).collect())
            .transpose()?,
        read_only: data.read_only,
        peer_read_only: data.peer_read_only,
        needs_reset: data.needs_reset,
    })
}

#[derive(Serialize)]
struct ChangeMeta {
    hash: String,
    actor: String,
    seq: u64,
    message: Option<String>,
    time: i64,
    deps: Vec<String>,
}

#[derive(Serialize)]
struct DiffPatch {
    path: Vec<serde_json::Value>,
    action: String,
    details: serde_json::Value,
}

#[derive(Serialize)]
struct MarkMeta {
    name: String,
    value: serde_json::Value,
    start: usize,
    end: usize,
}

fn resolve_parent_obj<T: Transactable>(
    tx: &mut T,
    path: &[PathSegment],
) -> Result<(ObjId, PathSegment), String> {
    if path.is_empty() {
        return Err("Path cannot be empty".to_string());
    }
    let mut current = ROOT;
    for segment in &path[..path.len() - 1] {
        match segment {
            PathSegment::Key(k) => match tx.get(&current, k.as_str()) {
                Ok(Some((_, child))) => current = child,
                Ok(None) => {
                    current = tx
                        .put_object(&current, k.as_str(), ObjType::Map)
                        .map_err(|e| e.to_string())?;
                }
                Err(e) => return Err(e.to_string()),
            },
            PathSegment::Index(i) => match tx.get(&current, *i) {
                Ok(Some((_, child))) => current = child,
                Ok(None) => return Err(format!("List index {} out of bounds", i)),
                Err(e) => return Err(e.to_string()),
            },
        }
    }
    Ok((current, path.last().unwrap().clone()))
}

fn put_json_val<T: Transactable>(
    tx: &mut T,
    obj: &ObjId,
    seg: &PathSegment,
    val: serde_json::Value,
) -> Result<(), String> {
    match seg {
        PathSegment::Key(key) => put_json_val_key(tx, obj, key, val),
        PathSegment::Index(idx) => put_json_val_idx(tx, obj, *idx, val),
    }
}

fn put_json_val_key<T: Transactable>(
    tx: &mut T,
    obj: &ObjId,
    prop: &str,
    val: serde_json::Value,
) -> Result<(), String> {
    match val {
        serde_json::Value::Null => tx.put(obj, prop, ()).map_err(|e| e.to_string()),
        serde_json::Value::Bool(b) => tx.put(obj, prop, b).map_err(|e| e.to_string()),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                tx.put(obj, prop, i).map_err(|e| e.to_string())
            } else if let Some(f) = n.as_f64() {
                tx.put(obj, prop, f).map_err(|e| e.to_string())
            } else {
                Err("Unsupported number".into())
            }
        }
        serde_json::Value::String(s) => tx.put(obj, prop, s).map_err(|e| e.to_string()),
        serde_json::Value::Array(arr) => {
            let list = tx
                .put_object(obj, prop, ObjType::List)
                .map_err(|e| e.to_string())?;
            for (idx, item) in arr.into_iter().enumerate() {
                insert_json_val(tx, &list, idx, item)?;
            }
            Ok(())
        }
        serde_json::Value::Object(map) => {
            let child = tx
                .put_object(obj, prop, ObjType::Map)
                .map_err(|e| e.to_string())?;
            for (k, v) in map {
                put_json_val_key(tx, &child, &k, v)?;
            }
            Ok(())
        }
    }
}

fn put_json_val_idx<T: Transactable>(
    tx: &mut T,
    obj: &ObjId,
    idx: usize,
    val: serde_json::Value,
) -> Result<(), String> {
    match val {
        serde_json::Value::Null => tx.put(obj, idx, ()).map_err(|e| e.to_string()),
        serde_json::Value::Bool(b) => tx.put(obj, idx, b).map_err(|e| e.to_string()),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                tx.put(obj, idx, i).map_err(|e| e.to_string())
            } else if let Some(f) = n.as_f64() {
                tx.put(obj, idx, f).map_err(|e| e.to_string())
            } else {
                Err("Unsupported number".into())
            }
        }
        serde_json::Value::String(s) => tx.put(obj, idx, s).map_err(|e| e.to_string()),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            let child = tx
                .put_object(obj, idx, ObjType::Map)
                .map_err(|e| e.to_string())?;
            if let serde_json::Value::Object(map) = val {
                for (k, v) in map {
                    put_json_val_key(tx, &child, &k, v)?;
                }
            }
            Ok(())
        }
    }
}

fn insert_json_val<T: Transactable>(
    tx: &mut T,
    obj: &ObjId,
    idx: usize,
    val: serde_json::Value,
) -> Result<(), String> {
    match val {
        serde_json::Value::Null => tx.insert(obj, idx, ()).map_err(|e| e.to_string()),
        serde_json::Value::Bool(b) => tx.insert(obj, idx, b).map_err(|e| e.to_string()),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                tx.insert(obj, idx, i).map_err(|e| e.to_string())
            } else if let Some(f) = n.as_f64() {
                tx.insert(obj, idx, f).map_err(|e| e.to_string())
            } else {
                Err("Unsupported number".into())
            }
        }
        serde_json::Value::String(s) => tx.insert(obj, idx, s).map_err(|e| e.to_string()),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            tx.insert(obj, idx, serde_json::to_string(&val).unwrap())
                .map_err(|e| e.to_string())
        }
    }
}

fn json_to_scalar(val: serde_json::Value) -> ScalarValue {
    match val {
        serde_json::Value::Null => ScalarValue::Null,
        serde_json::Value::Bool(b) => ScalarValue::Boolean(b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                ScalarValue::Int(i)
            } else if let Some(f) = n.as_f64() {
                ScalarValue::F64(f)
            } else {
                ScalarValue::Null
            }
        }
        serde_json::Value::String(s) => ScalarValue::Str(s.into()),
        _ => ScalarValue::Str(val.to_string().into()),
    }
}

fn export_value<D: ReadDoc>(doc: &D, obj: &ObjId) -> serde_json::Value {
    match doc.object_type(obj) {
        Ok(ObjType::Map) => {
            let mut map = serde_json::Map::new();
            for key in doc.keys(obj) {
                if let Ok(Some((val, child_id))) = doc.get(obj, &key) {
                    map.insert(key, scalar_to_json(doc, val, child_id));
                }
            }
            serde_json::Value::Object(map)
        }
        Ok(ObjType::List) => {
            let len = doc.length(obj);
            let mut arr = Vec::with_capacity(len);
            for i in 0..len {
                if let Ok(Some((val, child_id))) = doc.get(obj, i) {
                    arr.push(scalar_to_json(doc, val, child_id));
                }
            }
            serde_json::Value::Array(arr)
        }
        Ok(ObjType::Text) => {
            let text = doc.text(obj).unwrap_or_default();
            serde_json::Value::String(text)
        }
        _ => serde_json::Value::Null,
    }
}

fn scalar_to_json<D: ReadDoc>(
    doc: &D,
    val: automerge::Value,
    child_id: ObjId,
) -> serde_json::Value {
    match val {
        automerge::Value::Object(_) => export_value(doc, &child_id),
        automerge::Value::Scalar(s) => scalar_to_json_val(s.as_ref()),
    }
}

fn scalar_to_json_val(s: &ScalarValue) -> serde_json::Value {
    match s {
        ScalarValue::Bytes(b) => serde_json::to_value(BASE64_STANDARD.encode(b)).unwrap(),
        ScalarValue::Str(sm) => serde_json::Value::String(sm.to_string()),
        ScalarValue::Int(i) => serde_json::Value::Number((*i).into()),
        ScalarValue::Uint(u) => serde_json::Value::Number((*u).into()),
        ScalarValue::F64(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        ScalarValue::Counter(c) => serde_json::Value::Number(i64::from(c).into()),
        ScalarValue::Timestamp(t) => serde_json::Value::Number((*t).into()),
        ScalarValue::Boolean(b) => serde_json::Value::Bool(*b),
        ScalarValue::Null => serde_json::Value::Null,
        ScalarValue::Unknown { .. } => serde_json::Value::Null,
    }
}

// --- C-ABI Export Implementations with Safe Error Handling ---

#[no_mangle]
pub unsafe extern "C" fn automerge_init_doc(input: *const c_char) -> *mut c_char {
    let raw = match CStr::from_ptr(input).to_str() {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    let payload: InitPayload = match serde_json::from_str(raw) {
        Ok(p) => p,
        Err(e) => return err(e),
    };

    let actor = ActorId::from(payload.actor.as_bytes());
    let mut doc = AutoCommit::new().with_actor(actor);
    ok(DocResult {
        doc: BASE64_STANDARD.encode(doc.save()),
    })
}

#[no_mangle]
pub unsafe extern "C" fn automerge_change(input: *const c_char) -> *mut c_char {
    let raw = match CStr::from_ptr(input).to_str() {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    let payload: ChangePayload = match serde_json::from_str(raw) {
        Ok(p) => p,
        Err(e) => return err(e),
    };

    let doc_bytes = match BASE64_STANDARD.decode(payload.doc) {
        Ok(b) => b,
        Err(e) => return err(format!("Invalid base64 document: {}", e)),
    };
    let mut doc = match AutoCommit::load(&doc_bytes) {
        Ok(d) => d,
        Err(e) => return err(format!("Automerge load error: {}", e)),
    };
    doc.set_actor(ActorId::from(payload.actor.as_bytes()));

    for action in payload.actions {
        match action {
            Action::Put { path, value } => {
                let (parent, seg) = match resolve_parent_obj(&mut doc, &path) {
                    Ok(res) => res,
                    Err(e) => return err(e),
                };
                if let Err(e) = put_json_val(&mut doc, &parent, &seg, value) {
                    return err(e);
                }
            }
            Action::Counter { path, value } => {
                let (parent, seg) = match resolve_parent_obj(&mut doc, &path) {
                    Ok(res) => res,
                    Err(e) => return err(e),
                };
                match seg {
                    PathSegment::Key(k) => {
                        if let Err(e) = doc.put(&parent, k.as_str(), ScalarValue::Counter(value.into())) {
                            return err(e);
                        }
                    }
                    PathSegment::Index(i) => {
                        if let Err(e) = doc.put(&parent, i, ScalarValue::Counter(value.into())) {
                            return err(e);
                        }
                    }
                }
            }
            Action::Insert { path, index, value } => {
                let (parent, seg) = match resolve_parent_obj(&mut doc, &path) {
                    Ok(res) => res,
                    Err(e) => return err(e),
                };
                let child_id = match seg {
                    PathSegment::Key(k) => match doc.get(&parent, k.as_str()) {
                        Ok(Some((_, id))) => id,
                        _ => match doc.put_object(&parent, k.as_str(), ObjType::List) {
                            Ok(id) => id,
                            Err(e) => return err(e),
                        },
                    },
                    PathSegment::Index(i) => match doc.get(&parent, i) {
                        Ok(Some((_, id))) => id,
                        _ => match doc.put_object(&parent, i, ObjType::List) {
                            Ok(id) => id,
                            Err(e) => return err(e),
                        },
                    },
                };
                if let Err(e) = insert_json_val(&mut doc, &child_id, index, value) {
                    return err(e);
                }
            }
            Action::Delete { path } => {
                let (parent, seg) = match resolve_parent_obj(&mut doc, &path) {
                    Ok(res) => res,
                    Err(e) => return err(e),
                };
                match seg {
                    PathSegment::Key(k) => {
                        let _ = doc.delete(&parent, k.as_str());
                    }
                    PathSegment::Index(i) => {
                        let _ = doc.delete(&parent, i);
                    }
                }
            }
            Action::Inc { path, value } => {
                let (parent, seg) = match resolve_parent_obj(&mut doc, &path) {
                    Ok(res) => res,
                    Err(e) => return err(e),
                };
                let res = match seg {
                    PathSegment::Key(k) => doc.increment(&parent, k.as_str(), value),
                    PathSegment::Index(i) => doc.increment(&parent, i, value),
                };
                if let Err(e) = res {
                    return err(format!("Increment error: {}", e));
                }
            }
            Action::Splicetext { path, index, del, text } => {
                let (parent, seg) = match resolve_parent_obj(&mut doc, &path) {
                    Ok(res) => res,
                    Err(e) => return err(e),
                };
                let text_id = match seg {
                    PathSegment::Key(k) => match doc.get(&parent, k.as_str()) {
                        Ok(Some((automerge::Value::Object(ObjType::Text), id))) => id,
                        _ => match doc.put_object(&parent, k.as_str(), ObjType::Text) {
                            Ok(id) => id,
                            Err(e) => return err(e),
                        },
                    },
                    PathSegment::Index(i) => match doc.get(&parent, i) {
                        Ok(Some((automerge::Value::Object(ObjType::Text), id))) => id,
                        _ => match doc.put_object(&parent, i, ObjType::Text) {
                            Ok(id) => id,
                            Err(e) => return err(e),
                        },
                    },
                };
                if let Err(e) = doc.splice_text(&text_id, index, del, &text) {
                    return err(format!("SpliceText error: {}", e));
                }
            }
            Action::Splicelist { path, index, del, values } => {
                let (parent, seg) = match resolve_parent_obj(&mut doc, &path) {
                    Ok(res) => res,
                    Err(e) => return err(e),
                };
                let list_id = match seg {
                    PathSegment::Key(k) => match doc.get(&parent, k.as_str()) {
                        Ok(Some((automerge::Value::Object(ObjType::List), id))) => id,
                        _ => match doc.put_object(&parent, k.as_str(), ObjType::List) {
                            Ok(id) => id,
                            Err(e) => return err(e),
                        },
                    },
                    PathSegment::Index(i) => match doc.get(&parent, i) {
                        Ok(Some((automerge::Value::Object(ObjType::List), id))) => id,
                        _ => match doc.put_object(&parent, i, ObjType::List) {
                            Ok(id) => id,
                            Err(e) => return err(e),
                        },
                    },
                };
                for _ in 0..del.max(0) as usize {
                    let _ = doc.delete(&list_id, index);
                }
                for (offset, item) in values.into_iter().enumerate() {
                    if let Err(e) = insert_json_val(&mut doc, &list_id, index + offset, item) {
                        return err(e);
                    }
                }
            }
            Action::Mark { path, start, end, name, value } => {
                let (parent, seg) = match resolve_parent_obj(&mut doc, &path) {
                    Ok(res) => res,
                    Err(e) => return err(e),
                };
                let text_id = match seg {
                    PathSegment::Key(k) => match doc.get(&parent, k.as_str()) {
                        Ok(Some((automerge::Value::Object(ObjType::Text), id))) => id,
                        _ => return err("Mark target is not a Text object"),
                    },
                    PathSegment::Index(i) => match doc.get(&parent, i) {
                        Ok(Some((automerge::Value::Object(ObjType::Text), id))) => id,
                        _ => return err("Mark target is not a Text object"),
                    },
                };
                let mark = Mark::new(name, json_to_scalar(value), start, end);
                if let Err(e) = doc.mark(&text_id, mark, ExpandMark::Both) {
                    return err(format!("Mark error: {}", e));
                }
            }
            Action::Unmark { path, start, end, name } => {
                let (parent, seg) = match resolve_parent_obj(&mut doc, &path) {
                    Ok(res) => res,
                    Err(e) => return err(e),
                };
                let text_id = match seg {
                    PathSegment::Key(k) => match doc.get(&parent, k.as_str()) {
                        Ok(Some((automerge::Value::Object(ObjType::Text), id))) => id,
                        _ => return err("Unmark target is not a Text object"),
                    },
                    PathSegment::Index(i) => match doc.get(&parent, i) {
                        Ok(Some((automerge::Value::Object(ObjType::Text), id))) => id,
                        _ => return err("Unmark target is not a Text object"),
                    },
                };
                if let Err(e) = doc.unmark(&text_id, &name, start, end, ExpandMark::Both) {
                    return err(format!("Unmark error: {}", e));
                }
            }
        }
    }

    let mut opts = CommitOptions::default();
    if let Some(m) = payload.message {
        opts = opts.with_message(m);
    }
    if let Some(t) = payload.time {
        opts = opts.with_time(t);
    }
    let _ = doc.commit_with(opts);

    ok(DocResult {
        doc: BASE64_STANDARD.encode(doc.save()),
    })
}

#[no_mangle]
pub unsafe extern "C" fn automerge_merge(input: *const c_char) -> *mut c_char {
    let raw = match CStr::from_ptr(input).to_str() {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    let payload: MergePayload = match serde_json::from_str(raw) {
        Ok(p) => p,
        Err(e) => return err(e),
    };

    let b1 = match BASE64_STANDARD.decode(payload.doc1) {
        Ok(b) => b,
        Err(e) => return err(e),
    };
    let b2 = match BASE64_STANDARD.decode(payload.doc2) {
        Ok(b) => b,
        Err(e) => return err(e),
    };

    let mut doc1 = match AutoCommit::load(&b1) {
        Ok(d) => d,
        Err(e) => return err(e),
    };
    let mut doc2 = match AutoCommit::load(&b2) {
        Ok(d) => d,
        Err(e) => return err(e),
    };

    if let Err(e) = doc1.merge(&mut doc2) {
        return err(format!("Merge error: {}", e));
    }

    ok(DocResult {
        doc: BASE64_STANDARD.encode(doc1.save()),
    })
}

// --- 2-Way Sync Protocol Implementation (Veilid / P2P) ---

#[no_mangle]
pub unsafe extern "C" fn automerge_sync_state_new(_input: *const c_char) -> *mut c_char {
    let state = State::new();
    match encode_sync_state(&state) {
        Ok(encoded) => ok(encoded),
        Err(error) => err(error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn automerge_sync_generate_message(input: *const c_char) -> *mut c_char {
    let raw = match CStr::from_ptr(input).to_str() {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    let payload: SyncGenPayload = match serde_json::from_str(raw) {
        Ok(p) => p,
        Err(e) => return err(e),
    };

    let doc_bytes = match BASE64_STANDARD.decode(payload.doc) {
        Ok(b) => b,
        Err(e) => return err(e),
    };
    let mut doc = match AutoCommit::load(&doc_bytes) {
        Ok(d) => d,
        Err(e) => return err(e),
    };
    let mut state = match decode_sync_state(&payload.sync_state) {
        Ok(s) => s,
        Err(e) => return err(format!("SyncState decode error: {}", e)),
    };

    let maybe_msg = doc.sync().generate_sync_message(&mut state);
    let msg_str = maybe_msg.map(|m| BASE64_STANDARD.encode(m.encode()));

    let sync_state = match encode_sync_state(&state) {
        Ok(encoded) => encoded,
        Err(error) => return err(error),
    };
    ok(SyncGenResult {
        doc: BASE64_STANDARD.encode(doc.save()),
        sync_state,
        message: msg_str,
    })
}

#[no_mangle]
pub unsafe extern "C" fn automerge_sync_receive_message(input: *const c_char) -> *mut c_char {
    let raw = match CStr::from_ptr(input).to_str() {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    let payload: SyncRecvPayload = match serde_json::from_str(raw) {
        Ok(p) => p,
        Err(e) => return err(e),
    };

    let doc_bytes = match BASE64_STANDARD.decode(payload.doc) {
        Ok(b) => b,
        Err(e) => return err(e),
    };
    let msg_bytes = match BASE64_STANDARD.decode(payload.message) {
        Ok(b) => b,
        Err(e) => return err(e),
    };

    let mut doc = match AutoCommit::load(&doc_bytes) {
        Ok(d) => d,
        Err(e) => return err(e),
    };
    let mut state = match decode_sync_state(&payload.sync_state) {
        Ok(s) => s,
        Err(e) => return err(format!("SyncState decode error: {}", e)),
    };
    let msg = match Message::decode(&msg_bytes) {
        Ok(m) => m,
        Err(e) => return err(format!("SyncMessage decode error: {:?}", e)),
    };
    if let Err(e) = doc.sync().receive_sync_message(&mut state, msg) {
        return err(format!("Sync receive error: {}", e));
    }

    let sync_state = match encode_sync_state(&state) {
        Ok(encoded) => encoded,
        Err(error) => return err(error),
    };
    ok(SyncRecvResult {
        doc: BASE64_STANDARD.encode(doc.save()),
        sync_state,
    })
}

// --- Automerge Native Diff Engine ---

#[no_mangle]
#[no_mangle]
pub unsafe extern "C" fn automerge_diff(input: *const c_char) -> *mut c_char {
    let raw = match CStr::from_ptr(input).to_str() {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    let payload: DiffPayload = match serde_json::from_str(raw) {
        Ok(p) => p,
        Err(e) => return err(e),
    };

    let doc_bytes = match BASE64_STANDARD.decode(payload.doc) {
        Ok(b) => b,
        Err(e) => return err(e),
    };
    let mut doc = match AutoCommit::load(&doc_bytes) {
        Ok(d) => d,
        Err(e) => return err(e),
    };

    let before_heads: Result<Vec<ChangeHash>, _> = payload
        .before
        .iter()
        .map(|h| ChangeHash::from_str(h))
        .collect();
    let after_heads: Result<Vec<ChangeHash>, _> = payload
        .after
        .iter()
        .map(|h| ChangeHash::from_str(h))
        .collect();

    let (b_heads, a_heads) = match (before_heads, after_heads) {
        (Ok(b), Ok(a)) => (b, a),
        _ => return err("Invalid change hash in heads"),
    };

    let patches = doc.diff(&b_heads, &a_heads);
    let result: Vec<DiffPatch> = patches
        .into_iter()
        .map(|patch| {
            let path = patch
                .path
                .iter()
                .map(|(_, prop)| match prop {
                    automerge::Prop::Map(k) => serde_json::Value::String(k.clone()),
                    automerge::Prop::Seq(i) => serde_json::Value::Number((*i).into()),
                })
                .collect();

            let (action, details) = match patch.action {
                // Fixed: Added `..` to ignore conflict flags and passed cloned values to `scalar_to_json`
                automerge::PatchAction::PutMap { key, value, .. } => (
                    "putMap".to_string(),
                    serde_json::json!({ "key": key, "value": scalar_to_json(&doc, value.0.clone(), value.1.clone()) }),
                ),
                automerge::PatchAction::PutSeq { index, value, .. } => (
                    "putSeq".to_string(),
                    serde_json::json!({ "index": index, "value": scalar_to_json(&doc, value.0.clone(), value.1.clone()) }),
                ),
                automerge::PatchAction::Insert { index, values, .. } => (
                    "insert".to_string(),
                    serde_json::json!({ "index": index, "count": values.len() }),
                ),
                automerge::PatchAction::SpliceText { index, value, .. } => (
                    "spliceText".to_string(),
                    serde_json::json!({ "index": index, "text": value.make_string() }),
                ),
                automerge::PatchAction::Increment { prop, value, .. } => (
                    "inc".to_string(),
                    serde_json::json!({ "prop": prop.to_string(), "value": value }),
                ),
                automerge::PatchAction::DeleteMap { key, .. } => (
                    "deleteMap".to_string(),
                    serde_json::json!({ "key": key }),
                ),
                automerge::PatchAction::DeleteSeq { index, length, .. } => (
                    "deleteSeq".to_string(),
                    serde_json::json!({ "index": index, "length": length }),
                ),
                automerge::PatchAction::Mark { marks, .. } => (
                    "mark".to_string(),
                    serde_json::json!({
                        "marks": marks.iter().map(|m| serde_json::json!({
                            "name": m.name(),
                            "value": scalar_to_json_val(m.value()),
                            "start": m.start,
                            "end": m.end,
                        })).collect::<Vec<_>>()
                    }),
                ),
                automerge::PatchAction::Conflict { prop, .. } => (
                    "conflict".to_string(),
                    serde_json::json!({ "prop": prop.to_string() }),
                ),
            };

            DiffPatch { path, action, details }
        })
        .collect();

    ok(result)
}

// --- Targeted Read ---

#[no_mangle]
pub unsafe extern "C" fn automerge_get(input: *const c_char) -> *mut c_char {
    let raw = match CStr::from_ptr(input).to_str() {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    let payload: GetPayload = match serde_json::from_str(raw) {
        Ok(p) => p,
        Err(e) => return err(e),
    };

    let doc_bytes = match BASE64_STANDARD.decode(payload.doc) {
        Ok(b) => b,
        Err(e) => return err(e),
    };
    let mut doc = match AutoCommit::load(&doc_bytes) {
        Ok(d) => d,
        Err(e) => return err(e),
    };

    let (parent, seg) = match resolve_parent_obj(&mut doc, &payload.path) {
        Ok(res) => res,
        Err(e) => return err(e),
    };

    let val = match seg {
        PathSegment::Key(k) => doc.get(&parent, k.as_str()),
        PathSegment::Index(i) => doc.get(&parent, i),
    };

    match val {
        Ok(Some((v, id))) => ok(scalar_to_json(&doc, v, id)),
        Ok(None) => ok(serde_json::Value::Null),
        Err(e) => err(format!("Get error: {}", e)),
    }
}

// --- Marks Inspection ---

#[no_mangle]
pub unsafe extern "C" fn automerge_get_marks(input: *const c_char) -> *mut c_char {
    let raw = match CStr::from_ptr(input).to_str() {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    let payload: GetPayload = match serde_json::from_str(raw) {
        Ok(p) => p,
        Err(e) => return err(e),
    };

    let doc_bytes = match BASE64_STANDARD.decode(payload.doc) {
        Ok(b) => b,
        Err(e) => return err(e),
    };
    let mut doc = match AutoCommit::load(&doc_bytes) {
        Ok(d) => d,
        Err(e) => return err(e),
    };

    let (parent, seg) = match resolve_parent_obj(&mut doc, &payload.path) {
        Ok(res) => res,
        Err(e) => return err(e),
    };

    let text_id = match seg {
        PathSegment::Key(k) => match doc.get(&parent, k.as_str()) {
            Ok(Some((automerge::Value::Object(ObjType::Text), id))) => id,
            _ => return err("Target is not a Text object"),
        },
        PathSegment::Index(i) => match doc.get(&parent, i) {
            Ok(Some((automerge::Value::Object(ObjType::Text), id))) => id,
            _ => return err("Target is not a Text object"),
        },
    };

    let marks = match doc.marks(&text_id) {
        Ok(m) => m,
        Err(e) => return err(format!("Marks error: {}", e)),
    };

    let result: Vec<MarkMeta> = marks
        .into_iter()
        .map(|m| MarkMeta {
            name: m.name().to_string(),
            value: scalar_to_json_val(m.value()),
            start: m.start,
            end: m.end,
        })
        .collect();

    ok(result)
}

#[no_mangle]
pub unsafe extern "C" fn automerge_heads(input: *const c_char) -> *mut c_char {
    let raw = match CStr::from_ptr(input).to_str() {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    let payload: DocOnlyPayload = match serde_json::from_str(raw) {
        Ok(p) => p,
        Err(e) => return err(e),
    };

    let doc_bytes = match BASE64_STANDARD.decode(payload.doc) {
        Ok(b) => b,
        Err(e) => return err(e),
    };
    let mut doc = match AutoCommit::load(&doc_bytes) {
        Ok(d) => d,
        Err(e) => return err(e),
    };

    let heads: Vec<String> = doc.get_heads().into_iter().map(|h| h.to_string()).collect();
    ok(heads)
}

#[no_mangle]
pub unsafe extern "C" fn automerge_history(input: *const c_char) -> *mut c_char {
    let raw = match CStr::from_ptr(input).to_str() {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    let payload: DocOnlyPayload = match serde_json::from_str(raw) {
        Ok(p) => p,
        Err(e) => return err(e),
    };

    let doc_bytes = match BASE64_STANDARD.decode(payload.doc) {
        Ok(b) => b,
        Err(e) => return err(e),
    };
    let mut doc = match AutoCommit::load(&doc_bytes) {
        Ok(d) => d,
        Err(e) => return err(e),
    };

    let changes: Vec<ChangeMeta> = doc
        .get_changes(&[])
        .into_iter()
        .map(|c| ChangeMeta {
            hash: c.hash().to_string(),
            actor: c.actor_id().to_string(),
            seq: c.seq(),
            message: c.message().map(|m| m.to_string()),
            time: c.timestamp(),
            deps: c.deps().iter().map(|h| h.to_string()).collect(),
        })
        .collect();

    ok(changes)
}

#[no_mangle]
pub unsafe extern "C" fn automerge_fork_at(input: *const c_char) -> *mut c_char {
    let raw = match CStr::from_ptr(input).to_str() {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    let payload: ForkAtPayload = match serde_json::from_str(raw) {
        Ok(p) => p,
        Err(e) => return err(e),
    };

    let doc_bytes = match BASE64_STANDARD.decode(payload.doc) {
        Ok(b) => b,
        Err(e) => return err(e),
    };
    let mut doc = match AutoCommit::load(&doc_bytes) {
        Ok(d) => d,
        Err(e) => return err(e),
    };

    let heads: Result<Vec<ChangeHash>, _> = payload
        .heads
        .iter()
        .map(|h| ChangeHash::from_str(h))
        .collect();

    let heads_vec = match heads {
        Ok(h) => h,
        Err(e) => return err(format!("Invalid head hash: {}", e)),
    };

    let mut forked = match doc.fork_at(&heads_vec) {
        Ok(f) => f,
        Err(e) => return err(format!("ForkAt error: {}", e)),
    };

    ok(DocResult {
        doc: BASE64_STANDARD.encode(forked.save()),
    })
}

#[no_mangle]
pub unsafe extern "C" fn automerge_conflicts(input: *const c_char) -> *mut c_char {
    let raw = match CStr::from_ptr(input).to_str() {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    let payload: ConflictsPayload = match serde_json::from_str(raw) {
        Ok(p) => p,
        Err(e) => return err(e),
    };

    let doc_bytes = match BASE64_STANDARD.decode(payload.doc) {
        Ok(b) => b,
        Err(e) => return err(e),
    };
    let mut doc = match AutoCommit::load(&doc_bytes) {
        Ok(d) => d,
        Err(e) => return err(e),
    };

    let (parent, seg) = match resolve_parent_obj(&mut doc, &payload.path) {
        Ok(res) => res,
        Err(e) => return err(e),
    };

    let values = match seg {
        PathSegment::Key(k) => doc.get_all(&parent, k.as_str()).unwrap_or_default(),
        PathSegment::Index(i) => doc.get_all(&parent, i).unwrap_or_default(),
    };

    let result: Vec<serde_json::Value> = values
        .into_iter()
        .map(|(val, child_id)| scalar_to_json(&doc, val, child_id))
        .collect();

    ok(result)
}

#[no_mangle]
pub unsafe extern "C" fn automerge_materialize(input: *const c_char) -> *mut c_char {
    let raw = match CStr::from_ptr(input).to_str() {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    let payload: DocOnlyPayload = match serde_json::from_str(raw) {
        Ok(p) => p,
        Err(e) => return err(e),
    };

    let doc_bytes = match BASE64_STANDARD.decode(payload.doc) {
        Ok(b) => b,
        Err(e) => return err(e),
    };
    let doc = match AutoCommit::load(&doc_bytes) {
        Ok(d) => d,
        Err(e) => return err(e),
    };

    let json_val = export_value(&doc, &ROOT);
    ok(json_val)
}