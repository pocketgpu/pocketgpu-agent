use serde_json::json;

use super::*;

const SETTINGS: &str = r#"{
  "model": "opus",
  "permissions": {
    "allow": ["Bash(ls)"]
  },
  "zeta": 1
}
"#;

#[test]
fn insert_keeps_order_and_spacing() {
    let out = insert(SETTINGS, &[], "mcp", &json!({ "a": [1] })).unwrap();
    assert_eq!(
        out,
        "{\n  \"model\": \"opus\",\n  \"permissions\": {\n    \"allow\": [\"Bash(ls)\"]\n  },\n  \
         \"zeta\": 1,\n  \"mcp\": {\n    \"a\": [\n      1\n    ]\n  }\n}\n"
    );
    // Taking it out again gives the same bytes back.
    assert_eq!(remove(&out, &[], "mcp").unwrap(), SETTINGS);
}

#[test]
fn push_makes_the_path_and_removes_cleanly() {
    let hook = json!({ "hooks": [{ "type": "command", "command": "x hook claude" }] });
    let out = push(SETTINGS, &["hooks", "Stop"], &hook).unwrap();
    assert_eq!(read(&out).unwrap()["hooks"]["Stop"][0], hook);
    assert_eq!(read(&out).unwrap()["model"], "opus");
    let out2 = push(&out, &["hooks", "Stop"], &json!({ "other": true })).unwrap();
    assert_eq!(
        read(&out2).unwrap()["hooks"]["Stop"][1],
        json!({ "other": true })
    );
    // Removing ours leaves the user's item.
    let left = remove_items(&out2, &["hooks", "Stop"], |v| v == &hook).unwrap();
    assert_eq!(
        read(&left).unwrap()["hooks"]["Stop"],
        json!([{ "other": true }])
    );
    // And the containers go when emptied.
    let empty = remove_items(&left, &["hooks", "Stop"], |_| true).unwrap();
    let gone = remove(&remove(&empty, &["hooks"], "Stop").unwrap(), &[], "hooks").unwrap();
    assert_eq!(gone, SETTINGS);
}

#[test]
fn comments_trailing_commas_and_tabs_are_kept() {
    let text = "{\n\t// my servers\n\t\"mcpServers\": {\n\t\t\"other\": { \"command\": \"x\" }, /* keep */\n\t},\n}\n";
    let out = insert(
        text,
        &["mcpServers"],
        "pocketgpu",
        &json!({ "command": "p" }),
    )
    .unwrap();
    assert!(out.contains("// my servers") && out.contains("/* keep */"));
    assert!(out.contains("\n\t\t\"pocketgpu\": {\n\t\t\t\"command\": \"p\"\n\t\t}"));
    let v = read(&out).unwrap();
    assert_eq!(v["mcpServers"]["other"]["command"], "x");
    assert_eq!(v["mcpServers"]["pocketgpu"]["command"], "p");
}

#[test]
fn one_line_and_empty_objects() {
    let out = insert("{\"a\": 1}", &[], "b", &json!([2])).unwrap();
    assert_eq!(out, "{\"a\": 1, \"b\": [2]}");
    let out = insert("{}", &[], "b", &json!(2)).unwrap();
    assert_eq!(out, "{\n  \"b\": 2\n}");
    let out = insert("", &["x"], "b", &json!(2)).unwrap();
    assert_eq!(read(&out).unwrap(), json!({ "x": { "b": 2 } }));
    assert_eq!(remove(&out, &["x"], "b").unwrap(), "{\n  \"x\": {}\n}\n");
}

#[test]
fn a_middle_entry_comes_out_whole() {
    let text = "{\n  \"a\": 1,\n  \"b\": 2,\n  \"c\": 3\n}";
    assert_eq!(
        remove(text, &[], "b").unwrap(),
        "{\n  \"a\": 1,\n  \"c\": 3\n}"
    );
    assert_eq!(
        remove(text, &[], "a").unwrap(),
        "{\n  \"b\": 2,\n  \"c\": 3\n}"
    );
    assert_eq!(remove(text, &[], "missing").unwrap(), text);
}

#[test]
fn numbers_pick_array_items() {
    let text = r#"{"hooks": {"Stop": [{"hooks": [{"command": "a"}, {"command": "b"}]}]}}"#;
    let out = remove_items(text, &["hooks", "Stop", "0", "hooks"], |v| {
        v["command"] == "b"
    })
    .unwrap();
    assert_eq!(
        get(&out, &["hooks", "Stop", "0", "hooks"]).unwrap(),
        Some(json!([{ "command": "a" }]))
    );
}

#[test]
fn bad_shapes_and_bad_json_are_refused() {
    assert!(insert("{\"a\": 1}", &["a"], "b", &json!(1)).is_err());
    assert!(insert("{\"a\": 1}", &[], "a", &json!(1)).is_err());
    assert!(push("{\"a\": {}}", &["a"], &json!(1)).is_err());
    assert!(read("{\"a\": }").is_err());
    assert!(read("{\"a\": 1} x").is_err());
    assert!(read("{\"a\": 1 /* open").is_err());
    assert!(read("[1, 2,]").is_ok());
}

#[test]
fn strings_with_brackets_and_escapes() {
    let text = r#"{"a": "}{,\"[", "b": [ "x]" ]}"#;
    let out = push(text, &["b"], &json!("y")).unwrap();
    assert_eq!(
        read(&out).unwrap(),
        json!({ "a": "}{,\"[", "b": ["x]", "y"] })
    );
}

#[test]
fn new_entries_put_their_keys_in_the_usual_order() {
    let hook = json!({ "async": true, "command": "c", "type": "command" });
    let group = json!({ "hooks": [hook], "matcher": "m" });
    let out = push("{}", &["hooks", "Notification"], &group).unwrap();
    // The last "hooks" is the group's own, after the path's.
    let at = |k: &str| out.rfind(&format!("\"{k}\"")).unwrap();
    assert!(
        at("matcher") < at("hooks") && at("type") < at("command") && at("command") < at("async")
    );
    let one_line = insert(
        "{\"a\": 1}",
        &[],
        "s",
        &json!({ "args": ["mcp"], "command": "p" }),
    );
    assert_eq!(
        one_line.unwrap(),
        "{\"a\": 1, \"s\": {\"command\": \"p\", \"args\": [\"mcp\"]}}"
    );
}
