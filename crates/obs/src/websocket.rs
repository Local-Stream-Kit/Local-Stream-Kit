//! obs-websocket 5's messages, as far as the plugin uses them: every
//! message is `{ "op": …, "d": … }`. Parsed against messages captured from
//! OBS (`fixtures/`).

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// The RPC version the plugin speaks.
pub const RPC_VERSION: u32 = 1;

/// The events it asks for: obs-websocket's default (`All`, every event
/// that isn't sent many times a second) and SceneItemTransformChanged
/// (sent while a source is dragged), for Scaled overlays.
pub const EVENTS: u32 = 0x7FF | 1 << 19;

/// A message from OBS.
#[derive(Debug, PartialEq)]
pub enum Incoming {
    /// The first message on a connection.
    Hello(Hello),
    /// The login went through.
    Identified,
    /// Something happened in OBS (`eventType`, `eventData`).
    Event { kind: String, data: Value },
    /// The answer to a request.
    Response(Response),
    /// An op the plugin doesn't use.
    Other(u64),
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
    pub obs_studio_version: String,
    pub obs_web_socket_version: String,
    pub rpc_version: u32,
    /// Present when OBS asks for a password.
    #[serde(default)]
    pub authentication: Option<Challenge>,
}

#[derive(Debug, PartialEq, Deserialize)]
pub struct Challenge {
    pub challenge: String,
    pub salt: String,
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    pub request_type: String,
    pub request_id: String,
    pub request_status: Status,
    #[serde(default)]
    pub response_data: Option<Value>,
}

#[derive(Debug, PartialEq, Deserialize)]
pub struct Status {
    pub result: bool,
    pub code: u32,
    #[serde(default)]
    pub comment: Option<String>,
}

#[derive(Deserialize)]
struct Envelope {
    op: u64,
    #[serde(default)]
    d: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventData {
    event_type: String,
    #[serde(default)]
    event_data: Value,
}

/// Reads a message from OBS.
pub fn parse(text: &str) -> Result<Incoming, String> {
    let envelope: Envelope = serde_json::from_str(text).map_err(|error| error.to_string())?;
    let read = |what: &str, error: serde_json::Error| format!("{what}: {error}");
    Ok(match envelope.op {
        0 => Incoming::Hello(serde_json::from_value(envelope.d).map_err(|error| read("Hello", error))?),
        2 => Incoming::Identified,
        5 => {
            let event: EventData =
                serde_json::from_value(envelope.d).map_err(|error| read("Event", error))?;
            Incoming::Event { kind: event.event_type, data: event.event_data }
        }
        7 => Incoming::Response(
            serde_json::from_value(envelope.d).map_err(|error| read("RequestResponse", error))?,
        ),
        op => Incoming::Other(op),
    })
}

/// The answer to a Hello: Identify, with the password worked into OBS's
/// challenge when it asks for one.
pub fn identify(hello: &Hello, password: &str) -> String {
    let mut d = json!({ "rpcVersion": RPC_VERSION, "eventSubscriptions": EVENTS });
    if let Some(challenge) = &hello.authentication {
        d["authentication"] = Value::String(authentication(password, &challenge.salt, &challenge.challenge));
    }
    json!({ "op": 1, "d": d }).to_string()
}

/// obs-websocket's authentication string: base64(sha256(base64(sha256(
/// password + salt)) + challenge)).
pub fn authentication(password: &str, salt: &str, challenge: &str) -> String {
    let hash = |text: &str| BASE64.encode(Sha256::digest(text.as_bytes()));
    let secret = hash(&format!("{password}{salt}"));
    hash(&format!("{secret}{challenge}"))
}

/// GetSceneList's answer: the scene on air, and every scene in the order
/// OBS's Scenes list shows them (top first; OBS numbers them from the
/// bottom).
#[derive(Debug, PartialEq)]
pub struct SceneList {
    pub on_air: Option<String>,
    pub scenes: Vec<String>,
}

pub fn scene_list(data: &Value) -> Result<SceneList, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Data {
        current_program_scene_name: Option<String>,
        scenes: Vec<Scene>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Scene {
        scene_index: i64,
        scene_name: String,
    }
    let mut data = Data::deserialize(data).map_err(|error| format!("GetSceneList: {error}"))?;
    data.scenes.sort_by_key(|scene| std::cmp::Reverse(scene.scene_index));
    Ok(SceneList {
        on_air: data.current_program_scene_name,
        scenes: data.scenes.into_iter().map(|scene| scene.scene_name).collect(),
    })
}

/// An input (a source that isn't a scene): its name and kind
/// (`text_gdiplus_v2`, `wasapi_input_capture`…).
#[derive(Debug, PartialEq, Deserialize)]
pub struct Input {
    #[serde(rename = "inputName")]
    pub name: String,
    /// Without its version (`text_gdiplus`).
    #[serde(rename = "unversionedInputKind")]
    pub kind: String,
}

/// GetInputList's answer.
pub fn input_list(data: &Value) -> Result<Vec<Input>, String> {
    #[derive(Deserialize)]
    struct Data {
        inputs: Vec<Input>,
    }
    Ok(Data::deserialize(data).map_err(|error| format!("GetInputList: {error}"))?.inputs)
}

/// A source in a scene: its item id (what showing and hiding it takes),
/// its source's name, whether it's shown, and its transform (its position,
/// scale, crop…: `scaling::Transform`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SceneItem {
    #[serde(rename = "sceneItemId")]
    pub id: i64,
    #[serde(rename = "sourceName")]
    pub source: String,
    #[serde(rename = "sceneItemEnabled")]
    pub enabled: bool,
    #[serde(rename = "sceneItemTransform", default)]
    pub transform: Value,
}

/// GetSceneItemList's answer, top of OBS's Sources list first (OBS numbers
/// them from the bottom).
pub fn scene_items(data: &Value) -> Result<Vec<SceneItem>, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Data {
        scene_items: Vec<Item>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Item {
        scene_item_index: i64,
        #[serde(flatten)]
        item: SceneItem,
    }
    let mut items =
        Data::deserialize(data).map_err(|error| format!("GetSceneItemList: {error}"))?.scene_items;
    items.sort_by_key(|item| std::cmp::Reverse(item.scene_item_index));
    Ok(items.into_iter().map(|item| item.item).collect())
}

/// GetVideoSettings's answer: OBS's canvas (its base resolution), width
/// and height.
pub fn canvas(data: &Value) -> Result<(u32, u32), String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Data {
        base_width: u32,
        base_height: u32,
    }
    let data = Data::deserialize(data).map_err(|error| format!("GetVideoSettings: {error}"))?;
    Ok((data.base_width, data.base_height))
}

/// A filter on a source: its name and whether it's on.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Filter {
    #[serde(rename = "filterName")]
    pub name: String,
    #[serde(rename = "filterEnabled")]
    pub enabled: bool,
}

/// GetSourceFilterList's answer, in the order OBS's Filters window shows
/// them (top first).
pub fn filter_list(data: &Value) -> Result<Vec<Filter>, String> {
    #[derive(Deserialize)]
    struct Data {
        filters: Vec<Item>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Item {
        filter_index: i64,
        #[serde(flatten)]
        filter: Filter,
    }
    let mut filters =
        Data::deserialize(data).map_err(|error| format!("GetSourceFilterList: {error}"))?.filters;
    filters.sort_by_key(|item| item.filter_index);
    Ok(filters.into_iter().map(|item| item.filter).collect())
}

/// GetInputSettings' answer for a Browser Source: the page it shows.
pub fn browser_url(data: &Value) -> Option<String> {
    data["inputSettings"]["url"].as_str().map(str::to_string)
}

/// Whether a Browser Source's page is this app's: on this computer
/// (`localhost`, `127.0.0.1` or `[::1]`) at its port.
pub fn is_app_url(url: &str, port: u16) -> bool {
    let Some(rest) = url.strip_prefix("http://") else { return false };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let (host, at) = match authority.rsplit_once(':') {
        Some((host, at)) if !at.contains(']') => (host, at.parse::<u16>().ok()),
        _ => (authority, Some(80)),
    };
    matches!(host.to_ascii_lowercase().as_str(), "localhost" | "127.0.0.1" | "[::1]") && at == Some(port)
}

/// The events the plugin's event nodes come from, and those that keep
/// what it knows of OBS up to date.
#[derive(Debug, PartialEq)]
pub enum ObsEvent {
    /// Another scene is on air (after its transition).
    SceneChanged { scene: String },
    /// The recording (`Record`) or the stream (`Stream`) started or ended;
    /// the steps in between (starting, stopping) are left out. A recording
    /// that ended has its file.
    Output { output: Output, started: bool, file: Option<String> },
    /// A source in a scene was shown or hidden.
    SceneItemShown { scene: String, id: i64, shown: bool },
    /// A filter was switched on or off.
    FilterSwitched { source: String, filter: String, on: bool },
    /// A source in a scene was moved, resized, cropped… (its whole
    /// transform).
    Transformed { scene: String, id: i64, transform: Value },
    /// An input's settings changed: for a Browser Source, its page.
    InputSettings { input: String, url: Option<String> },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Output {
    Record,
    Stream,
}

/// The [`ObsEvent`] an event is, if it's one of them.
pub fn obs_event(kind: &str, data: &Value) -> Option<ObsEvent> {
    match kind {
        "CurrentProgramSceneChanged" => {
            Some(ObsEvent::SceneChanged { scene: data["sceneName"].as_str()?.to_string() })
        }
        "RecordStateChanged" | "StreamStateChanged" => {
            let output = if kind == "RecordStateChanged" { Output::Record } else { Output::Stream };
            let started = match data["outputState"].as_str()? {
                "OBS_WEBSOCKET_OUTPUT_STARTED" => true,
                "OBS_WEBSOCKET_OUTPUT_STOPPED" => false,
                _ => return None,
            };
            let file = data["outputPath"].as_str().filter(|_| !started).map(str::to_string);
            Some(ObsEvent::Output { output, started, file })
        }
        "SceneItemEnableStateChanged" => Some(ObsEvent::SceneItemShown {
            scene: data["sceneName"].as_str()?.to_string(),
            id: data["sceneItemId"].as_i64()?,
            shown: data["sceneItemEnabled"].as_bool()?,
        }),
        "SourceFilterEnableStateChanged" => Some(ObsEvent::FilterSwitched {
            source: data["sourceName"].as_str()?.to_string(),
            filter: data["filterName"].as_str()?.to_string(),
            on: data["filterEnabled"].as_bool()?,
        }),
        "SceneItemTransformChanged" => Some(ObsEvent::Transformed {
            scene: data["sceneName"].as_str()?.to_string(),
            id: data["sceneItemId"].as_i64()?,
            transform: data["sceneItemTransform"].clone(),
        }),
        "InputSettingsChanged" => Some(ObsEvent::InputSettings {
            input: data["inputName"].as_str()?.to_string(),
            url: browser_url(data),
        }),
        _ => None,
    }
}

/// A request, answered with a [`Response`] with the same id.
pub fn request(kind: &str, id: &str, data: Option<Value>) -> String {
    let mut d = json!({ "requestType": kind, "requestId": id });
    if let Some(data) = data {
        d["requestData"] = data;
    }
    json!({ "op": 6, "d": d }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scaling::Transform;

    #[test]
    fn hello_from_obs() {
        let Incoming::Hello(hello) = parse(include_str!("../fixtures/hello.json")).unwrap() else {
            panic!("a Hello")
        };
        assert_eq!(hello.obs_studio_version, "32.2.2");
        assert_eq!(hello.obs_web_socket_version, "5.7.4");
        assert_eq!(hello.rpc_version, 1);
        let challenge = hello.authentication.as_ref().expect("a password is asked for");
        assert_eq!(challenge.salt.len(), 44);

        let identify: Value = serde_json::from_str(&identify(&hello, "pw")).unwrap();
        assert_eq!(identify["op"], 1);
        assert_eq!(identify["d"]["rpcVersion"], 1);
        assert_eq!(identify["d"]["eventSubscriptions"], 526_335);
        assert_eq!(
            identify["d"]["authentication"],
            authentication("pw", &challenge.salt, &challenge.challenge).as_str()
        );
    }

    fn response(fixture: &str) -> Response {
        match parse(fixture).unwrap() {
            Incoming::Response(response) => response,
            other => panic!("a response, not {other:?}"),
        }
    }

    fn event(fixture: &str) -> Option<ObsEvent> {
        match parse(fixture).unwrap() {
            Incoming::Event { kind, data } => obs_event(&kind, &data),
            other => panic!("an event, not {other:?}"),
        }
    }

    #[test]
    fn identified_by_obs() {
        assert_eq!(parse(include_str!("../fixtures/identified.json")).unwrap(), Incoming::Identified);
    }

    #[test]
    fn scenes_from_obs() {
        let answer = response(include_str!("../fixtures/get_scene_list.json"));
        assert_eq!((answer.request_type.as_str(), answer.request_id.as_str()), ("GetSceneList", "scenes"));
        assert!(answer.request_status.result);
        let list = scene_list(answer.response_data.as_ref().unwrap()).unwrap();
        assert_eq!(list.on_air.as_deref(), Some("WoW"));
        assert_eq!(list.scenes.len(), 11);
        assert_eq!(list.scenes.first().map(String::as_str), Some("WoW"), "OBS's list, top first");
        assert_eq!(list.scenes.last().map(String::as_str), Some("Cooking"));
    }

    #[test]
    fn a_switch_answered_by_obs() {
        // Answered before the transition ends; CurrentProgramSceneChanged
        // follows once it has.
        let answer = response(include_str!("../fixtures/set_current_program_scene.json"));
        assert_eq!(answer.request_type, "SetCurrentProgramScene");
        assert!(answer.request_status.result);
        assert_eq!(answer.request_status.code, 100);
        assert_eq!(answer.response_data, None);
    }

    #[test]
    fn inputs_from_obs() {
        let answer = response(include_str!("../fixtures/get_input_list.json"));
        let inputs = input_list(answer.response_data.as_ref().unwrap()).unwrap();
        assert_eq!(inputs.len(), 28);
        assert_eq!(inputs[3], Input { name: "Status".into(), kind: "text_gdiplus".into() });
        assert!(
            inputs.iter().any(|input| input.name == "Microphone" && input.kind == "wasapi_input_capture")
        );
    }

    #[test]
    fn a_scenes_sources_from_obs() {
        let answer = response(include_str!("../fixtures/get_scene_item_list.json"));
        let items = scene_items(answer.response_data.as_ref().unwrap()).unwrap();
        assert_eq!(items.len(), 12);
        let named = |item: &SceneItem| (item.id, item.source.clone(), item.enabled);
        assert_eq!(named(&items[0]), (23, "LSK Default".into(), true));
        assert_eq!(named(&items[11]), (1, "Display".into(), false));
        assert_eq!(items[0].transform["scaleX"], 1.3333333730697632);
    }

    #[test]
    fn events_from_obs() {
        assert_eq!(
            event(include_str!("../fixtures/current_program_scene_changed.json")),
            Some(ObsEvent::SceneChanged { scene: "Be right back".into() })
        );
        assert_eq!(event(include_str!("../fixtures/scene_transition_started.json")), None);
        assert_eq!(event(include_str!("../fixtures/record_starting.json")), None);
        assert_eq!(
            event(include_str!("../fixtures/record_started.json")),
            Some(ObsEvent::Output { output: Output::Record, started: true, file: None })
        );
        assert_eq!(event(include_str!("../fixtures/record_stopping.json")), None);
        assert_eq!(
            event(include_str!("../fixtures/record_stopped.json")),
            Some(ObsEvent::Output {
                output: Output::Record,
                started: false,
                file: Some("C:/Users/streamer/Videos/2026-10-13 10-22-53.mp4".into())
            })
        );
    }

    #[test]
    fn showing_and_hiding_from_obs() {
        let answer = response(include_str!("../fixtures/set_scene_item_enabled.json"));
        assert_eq!(answer.request_type, "SetSceneItemEnabled");
        assert!(answer.request_status.result);
        assert_eq!(answer.response_data, None);
        assert_eq!(
            event(include_str!("../fixtures/scene_item_enable_state_changed.json")),
            Some(ObsEvent::SceneItemShown { scene: "WoW".into(), id: 26, shown: false })
        );
    }

    #[test]
    fn switching_filters_from_obs() {
        let answer = response(include_str!("../fixtures/set_source_filter_enabled.json"));
        assert_eq!(answer.request_type, "SetSourceFilterEnabled");
        assert!(answer.request_status.result);
        assert_eq!(
            event(include_str!("../fixtures/source_filter_enable_state_changed.json")),
            Some(ObsEvent::FilterSwitched {
                source: "Image 2".into(),
                filter: "Color Correction".into(),
                on: false
            })
        );
    }

    #[test]
    fn setting_a_text_from_obs() {
        let answer = response(include_str!("../fixtures/set_input_settings.json"));
        assert_eq!(answer.request_type, "SetInputSettings");
        assert!(answer.request_status.result);
        assert_eq!(answer.response_data, None);
        // OBS tells the text it now has; only a Browser Source's page is
        // kept.
        let Incoming::Event { kind, data } =
            parse(include_str!("../fixtures/input_settings_changed.json")).unwrap()
        else {
            panic!("an event")
        };
        assert_eq!(data["inputSettings"]["text"], "Gone Fishing");
        assert_eq!(
            obs_event(&kind, &data),
            Some(ObsEvent::InputSettings { input: "Text".into(), url: None })
        );
    }

    #[test]
    fn muting_from_obs() {
        let answer = response(include_str!("../fixtures/set_input_mute.json"));
        assert_eq!(answer.request_type, "SetInputMute");
        assert!(answer.request_status.result);
        let answer = response(include_str!("../fixtures/toggle_input_mute.json"));
        assert_eq!(answer.request_type, "ToggleInputMute");
        assert_eq!(answer.response_data, Some(json!({ "inputMuted": true })), "OBS says which it is now");
        // Not kept: Mute has no For, and toggling is OBS's.
        assert_eq!(event(include_str!("../fixtures/input_mute_state_changed.json")), None);
    }

    #[test]
    fn filters_and_browser_sources_from_obs() {
        let answer = response(include_str!("../fixtures/get_source_filter_list.json"));
        assert_eq!(answer.request_id, "filters/Spout2 Capture");
        assert_eq!(
            filter_list(answer.response_data.as_ref().unwrap()).unwrap(),
            [Filter { name: "Image Mask/Blend".into(), enabled: true }]
        );
        let answer = response(include_str!("../fixtures/get_input_settings_browser.json"));
        let url = browser_url(answer.response_data.as_ref().unwrap());
        assert_eq!(url.as_deref(), Some("http://localhost:3000/"));
        assert!(is_app_url(&url.unwrap(), 3000));
        let answer = response(include_str!("../fixtures/press_input_properties_button.json"));
        assert_eq!(answer.request_type, "PressInputPropertiesButton");
        assert!(answer.request_status.result);
    }

    // Written from obs-websocket's protocol documentation until captured
    // from OBS.
    #[test]
    fn documented_events() {
        let data = json!({ "outputActive": true, "outputState": "OBS_WEBSOCKET_OUTPUT_STARTED" });
        assert_eq!(
            obs_event("StreamStateChanged", &data),
            Some(ObsEvent::Output { output: Output::Stream, started: true, file: None })
        );
    }

    #[test]
    fn a_source_dragged_in_obs() {
        // The chat moved (not resized) in OBS 32.2.2: the last of 296 such
        // events in a third of a second, hence waiting for them to stop.
        let Some(ObsEvent::Transformed { scene, id, transform }) =
            event(include_str!("../fixtures/scene_item_transform_changed.json"))
        else {
            panic!("a transform")
        };
        assert_eq!((scene.as_str(), id), ("WoW", 22));
        assert_eq!(transform["positionX"], 711.0);
        let moved = Transform::read(&transform).unwrap();
        assert!(!moved.is_scaled());
        assert_eq!(moved.page(), (457, 436));
        let mut before = transform.clone();
        before["positionX"] = json!(0.0);
        assert!(Transform::read(&before).unwrap().sized_as(&moved), "a move isn't a resize");
    }

    #[test]
    fn a_source_resized_in_obs_and_unscaled() {
        // The chat dragged smaller in OBS 32.2.2 with Scaled overlays on
        // Fix: the drag's last step, the fix's answer, then what OBS said
        // after the new scale and after the new page size.
        let transform = |fixture: &str| match event(fixture) {
            Some(ObsEvent::Transformed { transform, .. }) => transform,
            other => panic!("a transform, not {other:?}"),
        };
        let resized = transform(include_str!("../fixtures/scene_item_transform_changed_resized.json"));
        let scaled = Transform::read(&resized).unwrap();
        assert!(scaled.is_scaled());
        assert_eq!(scaled.page(), (457, 436));
        assert_eq!(scaled.unscaled_page(), Ok((366, 349)));
        let answer = response(include_str!("../fixtures/set_scene_item_transform.json"));
        assert_eq!(answer.request_type, "SetSceneItemTransform");
        assert!(answer.request_status.result);
        assert_eq!(answer.response_data, None);
        // As the plugin keeps it once fixed: what OBS then says isn't a
        // resize by hand, so it isn't fixed again.
        let mut kept = resized.clone();
        crate::scaling::merge(&mut kept, &scaled.unscaled());
        crate::scaling::merge(&mut kept, &json!({ "sourceWidth": 366, "sourceHeight": 349 }));
        let kept = Transform::read(&kept).unwrap();
        for fixture in [
            include_str!("../fixtures/scene_item_transform_changed_unscaled.json"),
            include_str!("../fixtures/scene_item_transform_changed_new_page.json"),
        ] {
            let echo = Transform::read(&transform(fixture)).unwrap();
            assert!(!echo.is_scaled());
            assert!(kept.sized_as(&echo));
        }
    }

    #[test]
    fn filters_in_order() {
        let data = json!({ "filters": [
            { "filterEnabled": false, "filterIndex": 1, "filterKind": "color_filter_v2", "filterName": "Grey", "filterSettings": {} },
            { "filterEnabled": true, "filterIndex": 0, "filterKind": "noise_suppress_filter_v2", "filterName": "Quiet", "filterSettings": {} }
        ] });
        assert_eq!(
            filter_list(&data).unwrap(),
            [Filter { name: "Quiet".into(), enabled: true }, Filter { name: "Grey".into(), enabled: false }]
        );
    }

    #[test]
    fn the_apps_pages() {
        assert!(is_app_url("http://localhost:3000/", 3000));
        assert!(is_app_url("http://127.0.0.1:3000/?overlay=chat", 3000));
        assert!(is_app_url("http://LOCALHOST:3000", 3000));
        assert!(is_app_url("http://[::1]:3000/", 3000));
        assert!(!is_app_url("http://localhost:3001/", 3000), "another port");
        assert!(!is_app_url("https://localhost:3000/", 3000));
        assert!(!is_app_url("http://example.com:3000/", 3000));
        assert!(!is_app_url("http://localhost/", 3000));
        assert!(!is_app_url("file:///C:/overlay.html", 3000));
    }

    #[test]
    fn identify_without_a_password() {
        let hello = Hello {
            obs_studio_version: "32.2.2".into(),
            obs_web_socket_version: "5.7.4".into(),
            rpc_version: 1,
            authentication: None,
        };
        assert_eq!(identify(&hello, ""), r#"{"d":{"eventSubscriptions":526335,"rpcVersion":1},"op":1}"#);
    }

    #[test]
    fn authentication_as_obs_websocket_works_it_out() {
        // Worked out separately, with Node's crypto.
        assert_eq!(
            authentication("supersecretpassword", "PZVbYpvAnZut2SS6JNJytDm9", "ztTBnnuqrqaKDzRM3xcVdbYm"),
            "zZgWipvwSGrw748kHN4gNpBC1IaeiiWX3Hjkrm849Sc="
        );
        // obs-websocket's protocol documentation's example.
        assert_eq!(
            authentication(
                "supersecretpassword",
                "lM1GncleQOaCu9lT1yeUZhFYnqhsLLP1G5lAGo3ixaI=",
                "+IxH4CnCiqpX1rM9scsNynZzbOe4KhDeYcTNS3PDaeY="
            ),
            "1Ct943GAT+6YQUUX47Ia/ncufilbe6+oD6lY+5kaCu4="
        );
    }

    #[test]
    fn the_canvas() {
        let data = json!({
            "baseWidth": 2560, "baseHeight": 1440, "outputWidth": 1920, "outputHeight": 1080,
            "fpsNumerator": 60, "fpsDenominator": 1
        });
        assert_eq!(canvas(&data), Ok((2560, 1440)));
        assert!(canvas(&json!({})).is_err());
    }

    #[test]
    fn requests() {
        let sent: Value = serde_json::from_str(&request("GetSceneList", "7", None)).unwrap();
        assert_eq!(sent, json!({ "op": 6, "d": { "requestType": "GetSceneList", "requestId": "7" } }));
        let sent: Value =
            serde_json::from_str(&request("GetInputList", "8", Some(json!({ "inputKind": "x" })))).unwrap();
        assert_eq!(sent["d"]["requestData"]["inputKind"], "x");
    }

    #[test]
    fn other_ops_are_left() {
        assert_eq!(parse(r#"{"op":9,"d":{}}"#).unwrap(), Incoming::Other(9));
        assert!(parse("not json").is_err());
    }
}
