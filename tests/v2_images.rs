use base64::{Engine, engine::general_purpose::STANDARD};
use codex_hoshikage_proxy::v2::{images, now, service::Service};
use serde_json::{Value, json};
const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jGZkAAAAASUVORK5CYII=";
fn setup() -> (Service, String) {
    let root = std::env::temp_dir().join(format!("v2-images-{}", uuid::Uuid::new_v4()));
    let s = Service::open(&root.join("state"), &root.join("work")).unwrap();
    let c = s
        .conversation(
            "c",
            &json!({"workspace":{"mode":"automatic"},"model":"chatgpt/test"}),
        )
        .unwrap();
    let (_, rid) = s
        .accept(
            c["resource"]["id"].as_str().unwrap(),
            "run",
            &json!({"input":"draw"}),
        )
        .unwrap();
    let rid = rid.unwrap();
    s.store
        .update("response", &rid, |r| {
            r["thread_id"] = json!("thread");
            r["turn_id"] = json!("turn");
            r["phase"] = json!("finished");
            r["output"] = json!({"state":"failed"});
            Ok(())
        })
        .unwrap();
    (s, rid)
}
fn image(id: &str) -> Value {
    json!({"type":"imageGeneration","id":id,"status":"completed","result":PNG,"savedPath":"/untrusted/do-not-read.png"})
}
fn turn(items: Value) -> Value {
    json!({"id":"turn","status":"completed","itemsView":"full","items":items})
}
#[test]
fn captures_exact_bytes_without_source_file_and_reopens_without_duplicates() {
    let (s, rid) = setup();
    let t = turn(json!([image("one"), image("two")]));
    images::ingest(&s, &rid, &t).unwrap();
    let m = images::read(&s, &rid).unwrap();
    assert_eq!(m["state"], "complete");
    assert_eq!(m["items"].as_array().unwrap().len(), 2);
    for i in m["items"].as_array().unwrap() {
        let aid = i["artifact_id"].as_str().unwrap();
        let a = s.store.get("artifact", aid).unwrap();
        assert_eq!(a["media_type"], "image/png");
        assert_eq!(a["response_id"], rid);
        assert_eq!(
            std::fs::read(s.store.root.join("blobs").join(aid)).unwrap(),
            STANDARD.decode(PNG).unwrap()
        );
    }
    let root = s.store.root.clone();
    let work = s.work_root.clone();
    drop(s);
    let s = Service::open(&root, &work).unwrap();
    images::ingest(&s, &rid, &t).unwrap();
    assert_eq!(s.store.list("artifact").unwrap().len(), 2);
    assert_eq!(images::read(&s, &rid).unwrap(), m);
    assert_eq!(
        s.store.get("response", &rid).unwrap()["output"]["state"],
        "failed"
    );
}
#[test]
fn partial_failure_and_interruption_preserve_successful_images() {
    let (s, rid) = setup();
    let mut bad = image("bad");
    bad["result"] = json!("not base64");
    let mut t = turn(json!([image("good"), bad]));
    t["status"] = json!("interrupted");
    images::ingest(&s, &rid, &t).unwrap();
    let m = images::read(&s, &rid).unwrap();
    assert_eq!(m["state"], "complete");
    assert_eq!(m["items"][0]["state"], "ready");
    assert_eq!(m["items"][1]["state"], "failed");
    assert_eq!(s.store.list("artifact").unwrap().len(), 1);
}
#[test]
fn wrong_turn_and_missing_inventory_never_mean_no_images() {
    let (s, rid) = setup();
    let mut t = turn(json!([image("wrong")]));
    t["id"] = json!("other");
    assert_eq!(
        images::ingest(&s, &rid, &t).unwrap_err().code,
        "image_turn_mismatch"
    );
    assert!(images::ingest(&s, &rid, &json!({"id":"turn","status":"completed"})).is_err());
    assert_eq!(images::read(&s, &rid).unwrap()["state"], "pending");
    assert!(s.store.list("artifact").unwrap().is_empty());
    images::ingest(
        &s,
        &rid,
        &turn(json!([{"type":"agentMessage","text":"![picture](/etc/secret.png)"}])),
    )
    .unwrap();
    assert_eq!(images::read(&s, &rid).unwrap()["state"], "complete");
    assert_eq!(images::read(&s, &rid).unwrap()["items"], json!([]));
}
#[test]
fn bounds_and_duplicate_source_ids_fail_closed() {
    let (s, rid) = setup();
    images::ingest(&s, &rid, &turn(json!([image("same"), image("same")]))).unwrap();
    assert_eq!(images::read(&s, &rid).unwrap()["state"], "unknown");
    let items: Vec<_> = (0..17).map(|n| image(&n.to_string())).collect();
    images::ingest(&s, &rid, &turn(json!(items))).unwrap();
    assert_eq!(
        images::read(&s, &rid).unwrap()["error"]["code"],
        "generated_images_limit_exceeded"
    );
    assert!(s.store.list("artifact").unwrap().is_empty());
}
#[test]
fn registration_commit_response_loss_recovers_same_artifact() {
    let (s, rid) = setup();
    let t = turn(json!([image("one")]));
    images::ingest(&s, &rid, &t).unwrap();
    let before = images::read(&s, &rid).unwrap();
    // Artifact is durable; simulate a crash before the inventory receives its receipt.
    s.store
        .update("response", &rid, |r| {
            r["generated_images"]["state"] = json!("pending");
            let i = &mut r["generated_images"]["items"][0];
            i["state"] = json!("creating");
            i["artifact_id"] = Value::Null;
            Ok(())
        })
        .unwrap();
    let root = s.store.root.clone();
    let work = s.work_root.clone();
    drop(s);
    let s = Service::open(&root, &work).unwrap();
    images::ingest(&s, &rid, &t).unwrap();
    assert_eq!(images::read(&s, &rid).unwrap()["items"], before["items"]);
    assert_eq!(s.store.list("artifact").unwrap().len(), 1);
}
#[test]
fn expiry_and_revocation_apply_to_inventory() {
    let (s, rid) = setup();
    images::ingest(&s, &rid, &turn(json!([]))).unwrap();
    s.store
        .update("response", &rid, |r| {
            r["generated_images"]["expires_at_ms"] = json!(now() - 1);
            Ok(())
        })
        .unwrap();
    assert_eq!(
        images::read(&s, &rid).unwrap_err().code,
        "generated_images_expired"
    );
    let r = s.store.get("response", &rid).unwrap();
    let wid = r["workspace_id"].as_str().unwrap();
    s.store
        .update("workspace", wid, |w| {
            w["state"] = json!("revoked");
            Ok(())
        })
        .unwrap();
    assert_eq!(
        images::read(&s, &rid).unwrap_err().code,
        "workspace_access_revoked"
    );
}

#[test]
#[ignore = "requires a private image-only snapshot from a real App Server Turn"]
fn real_app_server_image_snapshot_is_published_unchanged() {
    let path =
        std::env::var("HOSHIKAGE_IMAGE_TURN_FIXTURE").expect("set HOSHIKAGE_IMAGE_TURN_FIXTURE");
    let t: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let (s, rid) = setup();
    s.store
        .update("response", &rid, |r| {
            r["turn_id"] = t["id"].clone();
            Ok(())
        })
        .unwrap();
    images::ingest(&s, &rid, &t).unwrap();
    let m = images::read(&s, &rid).unwrap();
    assert_eq!(m["state"], "complete");
    assert!(!m["items"].as_array().unwrap().is_empty());
    for (item, source) in m["items"]
        .as_array()
        .unwrap()
        .iter()
        .zip(t["items"].as_array().unwrap())
    {
        assert_eq!(item["state"], "ready");
        let aid = item["artifact_id"].as_str().unwrap();
        let blob = std::fs::read(s.store.root.join("blobs").join(aid)).unwrap();
        assert_eq!(
            blob,
            STANDARD.decode(source["result"].as_str().unwrap()).unwrap()
        );
        println!("verified real generated PNG: {} bytes", blob.len());
    }
}

#[test]
fn reserved_unknown_image_is_never_copied_again() {
    let (s, rid) = setup();
    let iid = format!(
        "img_{}",
        codex_hoshikage_proxy::control::fingerprint(&json!([rid, "one"]))
    );
    let r = s.store.get("response", &rid).unwrap();
    let (op,_)=s.reserve_capture(r["conversation_id"].as_str().unwrap(),&format!("generated-{iid}"),
        &json!({"path":format!("generated/{iid}.png"),"display_name":"generated-image-1.png","response_id":rid})).unwrap();
    let aid = op["resource"]["id"].as_str().unwrap().to_owned();
    let root = s.store.root.clone();
    let work = s.work_root.clone();
    drop(s);
    let s = Service::open(&root, &work).unwrap();
    images::ingest(&s, &rid, &turn(json!([image("one")]))).unwrap();
    let m = images::read(&s, &rid).unwrap();
    assert_eq!(m["state"], "complete");
    assert_eq!(m["items"][0]["state"], "unknown");
    assert_eq!(m["items"][0]["artifact_id"], aid);
    assert!(!s.store.root.join("blobs").join(aid).exists());
    assert_eq!(s.store.list("artifact").unwrap().len(), 1);
}

#[test]
fn summary_view_and_oversize_image_are_not_silently_accepted() {
    let (s, rid) = setup();
    let mut t = turn(json!([]));
    t["itemsView"] = json!("summary");
    assert_eq!(
        images::ingest(&s, &rid, &t).unwrap_err().code,
        "image_inventory_incomplete"
    );
    s.store
        .update("response", &rid, |r| {
            r["generated_images"]["policy"]["max_bytes"] = json!(1);
            Ok(())
        })
        .unwrap();
    images::ingest(&s, &rid, &turn(json!([image("one")]))).unwrap();
    let m = images::read(&s, &rid).unwrap();
    assert_eq!(m["items"][0]["state"], "failed");
    assert_eq!(m["items"][0]["error"]["code"], "generated_image_too_large");
    assert!(s.store.list("artifact").unwrap().is_empty());
}
