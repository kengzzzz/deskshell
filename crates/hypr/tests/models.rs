//! Deserializes Hyprland JSON fixtures.

use deskshell_hypr::{
    Address, Bind, Devices, HardwareDetails, Monitor, Window, Workspace, WorkspaceKind,
    WorkspaceRef,
};

fn fixture<T: serde::de::DeserializeOwned>(name: &str) -> T {
    let path = format!(
        "{}/tests/fixtures/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{path}: {err}"));
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("{path}: {err}"))
}

#[test]
fn clients_fixture() {
    let windows: Vec<Window> = fixture("clients");
    assert_eq!(windows.len(), 6);
    let first = &windows[0];
    assert_eq!(first.address, Address(0x560000000010));
    assert_eq!(first.at, [3431, 69]);
    assert_eq!(first.size, [1678, 1350]);
    assert_eq!(
        first.workspace,
        WorkspaceRef {
            address: "1".into(),
            id: Some(1),
            kind: WorkspaceKind::Normal,
            name: "1".into()
        }
    );
    assert_eq!(first.title, "Docs, page 2 - Browser");
    assert_eq!(first.initial_class, "org.example.Browser");
    assert_eq!(first.initial_title, "New Tab - Browser");
    assert_eq!(first.focus_history_id, 5);
    assert!(first.mapped && !first.hidden && !first.xwayland && !first.pinned);
    assert_eq!(first.fullscreen, 0);
    assert_eq!(first.monitor, 0);
    assert_eq!(
        windows
            .iter()
            .filter(|w| w.floating)
            .count(),
        1
    );
}

#[test]
fn active_window_fixture() {
    let window: Window = fixture("activewindow");
    assert_eq!(window.focus_history_id, 0);
    assert_eq!(window.workspace.id, Some(2));
}

#[test]
fn workspaces_fixture() {
    let workspaces: Vec<Workspace> = fixture("workspaces");
    assert_eq!(workspaces.len(), 2);
    let ws = &workspaces[0];
    assert_eq!(ws.address, "1");
    assert_eq!(ws.id, Some(1));
    assert_eq!(ws.kind, WorkspaceKind::Normal);
    assert_eq!(ws.monitor, "DP-3");
    assert_eq!(ws.monitor_id, Some(0));
    assert_eq!(ws.windows, 5);
    assert!(!ws.has_fullscreen);
    assert!(ws.last_window.is_some());

    let active: Workspace = fixture("activeworkspace");
    assert_eq!(active.id, Some(2));
    assert_eq!(active.last_window_title, "Remote Desktop");
}

#[test]
fn monitors_fixture() {
    let monitors: Vec<Monitor> = fixture("monitors");
    let m = &monitors[0];
    assert_eq!(m.name, "DP-3");
    assert_eq!(m.description, "Example Vendor EX34 0x00000000");
    assert_eq!((m.width, m.height, m.x, m.y), (3440, 1440, 0, 0));
    assert_eq!(m.scale, 1.0);
    assert!(m.focused && m.dpms_status && !m.vrr && !m.disabled);
    assert_eq!(
        m.active_workspace
            .as_ref()
            .and_then(|w| w.id),
        Some(2)
    );
    assert_eq!(m.special_workspace, None);
    assert_eq!(m.reserved, [0, 60, 0, 0]);
    assert_eq!(m.color_management_preset, "srgb");
    assert_eq!(m.transform, 0);
    assert!((m.refresh_rate - 59.97).abs() < 1e-9);
    assert_eq!(
        m.hardware_details,
        Some(HardwareDetails { hdr: Some(true) })
    );
    assert!(m.hdr_capable());
}

#[test]
fn devices_fixture() {
    let devices: Devices = fixture("devices");
    assert_eq!(devices.keyboards.len(), 6);
    let main = devices
        .main_keyboard()
        .expect("main keyboard");
    assert_eq!(main.name, "example-keyboard");
    assert_eq!(main.active_keymap, "English (US)");
}

#[test]
fn binds_fixture() {
    let binds: Vec<Bind> = fixture("binds");
    assert_eq!(binds[0].key, "RETURN");
    assert_eq!(binds[0].modmask, 64);
    assert!(binds[0].has_description);
    assert_eq!(binds[0].dispatcher, "HL.Dispatcher(exec_cmd)");
    assert!(binds.iter().any(|b| b.mouse));
    assert!(
        binds
            .iter()
            .any(|b| b.repeat && b.locked)
    );
    assert!(
        binds
            .iter()
            .any(|b| b.submap == "passthru")
    );
}

#[test]
fn named_and_special_workspaces_have_no_id() {
    let json = r#"[
        {"address": "web", "type": "normal", "name": "Web, mail", "monitor": "?", "monitorID": null,
         "windows": 0, "hasfullscreen": false, "lastwindow": "0x0", "lastwindowtitle": "",
         "ispersistent": true, "tiledLayout": "dwindle"},
        {"address": "special:magic", "type": "special", "name": "special:magic", "monitor": "DP-1",
         "monitorID": 1, "windows": 1, "hasfullscreen": true, "lastwindow": "0x55b0cd60fa30",
         "lastwindowtitle": "x", "ispersistent": false, "tiledLayout": "dwindle"}
    ]"#;
    let workspaces: Vec<Workspace> = serde_json::from_str(json).unwrap();
    assert_eq!(workspaces[0].id, None);
    assert_eq!(workspaces[0].kind, WorkspaceKind::Normal);
    assert_eq!(workspaces[0].monitor, "?");
    assert_eq!(workspaces[0].monitor_id, None);
    assert_eq!(workspaces[0].last_window, None);
    assert_eq!(workspaces[1].id, None);
    assert_eq!(workspaces[1].kind, WorkspaceKind::Special);
    assert_eq!(
        workspaces[1].last_window,
        Some(Address(0x55b0cd60fa30))
    );
}

#[test]
fn monitor_with_special_and_no_active_workspace() {
    let json = r#"{
        "id": 1, "name": "HDMI-A-1", "description": "", "width": 1920, "height": 1080,
        "refreshRate": 60.00000, "x": 3440, "y": 0,
        "activeWorkspace": {"address": "", "type": "", "name": ""},
        "specialWorkspace": {"address": "special:term", "type": "special", "name": "special:term"},
        "reserved": [0, 0, 0, 0], "scale": 1.25, "transform": 1, "focused": false,
        "dpmsStatus": false, "vrr": true, "disabled": true, "colorManagementPreset": "hdr"
    }"#;
    let m: Monitor = serde_json::from_str(json).unwrap();
    assert_eq!(m.hardware_details, None);
    assert!(
        m.hdr_capable(),
        "an older Hyprland without hardwareDetails counts as able"
    );
    assert_eq!(m.active_workspace, None);
    let special = m
        .special_workspace
        .expect("special workspace");
    assert_eq!(special.kind, WorkspaceKind::Special);
    assert_eq!(special.id, None);
    assert_eq!(m.scale, 1.25);
}

#[test]
fn hardware_details_hdr() {
    let details: HardwareDetails = serde_json::from_str(
        r#"{"backend": "drm", "hdr": false, "chroma": false, "bt2020": false, "vrrCapable": false}"#,
    )
    .unwrap();
    let monitor = Monitor { hardware_details: Some(details), ..Monitor::default() };
    assert!(!monitor.hdr_capable());
    let details: HardwareDetails = serde_json::from_str(r#"{"backend": "drm"}"#).unwrap();
    assert_eq!(details.hdr, None);
    let monitor = Monitor { hardware_details: Some(details), ..Monitor::default() };
    assert!(monitor.hdr_capable());
}

#[test]
fn window_on_named_workspace() {
    let mut window: serde_json::Value = fixture("activewindow");
    window["workspace"] = serde_json::json!({"address": "web", "type": "normal", "name": "Web"});
    let window: Window = serde_json::from_value(window).unwrap();
    assert_eq!(window.workspace.id, None);
    assert_eq!(window.workspace.address, "web");
}

#[test]
fn bad_window_address_is_rejected() {
    let mut window: serde_json::Value = fixture("activewindow");
    window["address"] = "0xzz".into();
    assert!(serde_json::from_value::<Window>(window).is_err());
}
