use super::{Error, Result, service::Service, store};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
pub fn list(s: &Service, path: &[String], query: Option<&str>) -> Result<Value> {
    list_with_policy(s, path, query, None)
}
pub fn list_with_policy(
    s: &Service,
    path: &[String],
    query: Option<&str>,
    policy: Option<&crate::config::CwdPolicy>,
) -> Result<Value> {
    let url = reqwest::Url::parse(&format!("http://local/?{}", query.unwrap_or("")))
        .map_err(|_| Error::code(400, "invalid_argument"))?;
    let mut limit = 50;
    let mut cursor = None;
    for (k, v) in url.query_pairs() {
        match k.as_ref() {
            "limit" => {
                limit = v
                    .parse::<usize>()
                    .map_err(|_| Error::code(400, "invalid_argument"))?;
                if !(1..=100).contains(&limit) {
                    return Err(Error::code(400, "invalid_argument"));
                }
            }
            "cursor" => {
                cursor = Some(v.into_owned());
            }
            "selectable" => {
                if v != "true" {
                    return Err(Error::code(400, "invalid_argument"));
                }
            }
            _ => return Err(Error::code(400, "invalid_argument")),
        }
    }
    let (kind, scope) = match path {
        [a] if a == "workspaces" => ("workspace", None),
        [a, b, c] if (a == "conversations" || a == "workspaces") && c == "artifacts" => (
            "artifact",
            Some((
                if a == "conversations" {
                    "conversation_id"
                } else {
                    "workspace_id"
                },
                b,
            )),
        ),
        _ => return Err(Error::code(404, "resource_not_found")),
    };
    s.store.transaction(|tx|{
  if let Some((scope,id))=scope{
let r=store::get(tx,if scope=="conversation_id"{
"conversation"}
else{
"workspace"}
,id)?;
if scope=="workspace_id"&&r["state"]!="ready"{
return Err(Error::code(403,"workspace_access_revoked"));
}
}
  let upper:i64=tx.query_row("SELECT COALESCE(MAX(sequence),0) FROM record_sequences",[],|r|r.get(0))?;
  let(after,upper)=if let Some(cursor)=cursor{
   let raw=URL_SAFE_NO_PAD.decode(cursor).map_err(|_|Error::code(400,"invalid_cursor"))?;
let c:Value=serde_json::from_slice(&raw).map_err(|_|Error::code(400,"invalid_cursor"))?;
   if c["generation"]!=s.store.generation||c["scope"]!=json!(path){
return Err(Error::code(409,"cursor_scope_mismatch"));
}
   (c["after"].as_i64().ok_or_else(||Error::code(400,"invalid_cursor"))?,c["upper"].as_i64().ok_or_else(||Error::code(400,"invalid_cursor"))?)
  }
else{
(0,upper)}
;
  let mut stmt=tx.prepare("SELECT q.sequence,r.value FROM record_sequences q JOIN records r ON q.kind=r.kind AND q.id=r.id WHERE r.kind=?1 AND q.sequence>?2 AND q.sequence<=?3 ORDER BY q.sequence")?;
  let mut rows=stmt.query(rusqlite::params![kind,after,upper])?;
let mut data=Vec::new();
let mut last=after;
let mut more=false;
  while let Some(row)=rows.next()?{
let seq:i64=row.get(0)?;
let mut r:Value=serde_json::from_str(&row.get::<_,String>(1)?)?;
   if scope.is_some_and(|(key,value)|r[key]!=*value)||kind=="workspace"&&(r["mode"]!="shared"||r["state"]!="ready"){
continue;
}
   if kind=="workspace" && policy.is_some_and(|p|p.validate(r["path"].as_str().unwrap_or("")).is_err()){
continue;
}
   if kind=="artifact"{
let w=store::get(tx,"workspace",r["workspace_id"].as_str().unwrap_or(""))?;
if w["state"]!="ready"||policy.is_some_and(|p|p.validate(w["path"].as_str().unwrap_or("")).is_err()){
continue;
}
 if r["state"]=="ready"&&super::retention::expiry(tx,kind,r["artifact_id"].as_str().unwrap_or(""),r["expires_at_ms"].as_u64().unwrap_or(0))?<=super::now(){
r["state"]=json!("expired");
}
}
   if data.len()==limit{
more=true;
break;
}
   for key in ["path","source_path","device","inode"]{
r.as_object_mut().unwrap().remove(key);
}
data.push(r);
last=seq;
  }
  let next=more.then(||URL_SAFE_NO_PAD.encode(json!({
      "after":last,
      "upper":upper,
      "scope":path,
      "generation":s.store.generation
  }).to_string()));
Ok(json!({ "data":data,"next_cursor":next}))
 }
)
}
