//! Sounds in the panel: the app's sound settings on the Sounds plugin's
//! box (master volume, the sound files with ▶, their labels, delete, and
//! Upload), the sound and picture pickers used by alert styles, nodes and
//! overrides, and uploads (pictures too).

use protocol::panel::{MediaFolder, PanelRequest, UPLOAD_HEADER, picture_upload_path, sound_upload_path};
use protocol::plugin::{MediaKind, media_folder};
use protocol::sound::{SoundSettings, sound_url};
use wasm_bindgen::JsCast;
use web_sys::{HtmlAudioElement, HtmlInputElement};
use yew::prelude::*;

use crate::editor::fields::TextField;
use crate::views::icon;
use crate::{Panel, util};

/// What the file pickers accept.
const ACCEPT: &str = ".mp3,.wav,.ogg,.oga,.opus,.flac,.m4a,.webm";

/// Sends a file to an upload path; returns the name it was saved as.
pub async fn upload(path: &str, file: &web_sys::File) -> Result<String, String> {
    let request = gloo::net::http::Request::post(path)
        .header(UPLOAD_HEADER, "1")
        .body(wasm_bindgen::JsValue::from(file.clone()))
        .map_err(|error| error.to_string())?;
    let response = request.send().await.map_err(|error| error.to_string())?;
    let text = response.text().await.map_err(|error| error.to_string())?;
    if response.ok() { Ok(text) } else { Err(text) }
}

/// The file chosen in a file input's change event, and clears the input so
/// the same file can be picked again.
pub fn chosen_file(event: &Event) -> Option<web_sys::File> {
    let input = event.target()?.dyn_into::<HtmlInputElement>().ok()?;
    let file = input.files()?.get(0)?;
    input.set_value("");
    Some(file)
}

/// Plays a sound here, in the panel (an OBS dock plays to the desktop).
pub fn preview(sound: &str, volume: f32) {
    let Some(url) = sound_url(sound) else { return };
    if let Ok(audio) = HtmlAudioElement::new_with_src(&url) {
        audio.set_volume(f64::from(volume.clamp(0.0, 1.0)));
        let _ = audio.play();
    }
}

/// A picker's options: None, the files (sounds by their labels), the
/// folders (a file picked at random each time), and the value if it's none
/// of them (a file that's gone).
fn media_options(
    files: &[String],
    folders: &[MediaFolder],
    value: &str,
    kind: MediaKind,
    sounds: Option<&SoundSettings>,
) -> Html {
    let files: Vec<&String> = files.iter().filter(|file| kind.takes(file)).collect();
    let folders: Vec<(String, usize)> = folders
        .iter()
        .map(|folder| (folder.value(), folder.files.iter().filter(|file| kind.takes(file)).count()))
        .filter(|(_, count)| *count > 0)
        .collect();
    let listed = value.is_empty()
        || files.iter().any(|file| *file == value)
        || folders.iter().any(|(folder, _)| folder == value);
    let (one, many) = match kind {
        MediaKind::Picture => ("picture", "pictures"),
        MediaKind::Media => ("file", "files"),
        MediaKind::Sound => ("sound", "sounds"),
    };
    html! {
        <>
            <option value="" selected={value.is_empty()}>{ "None" }</option>
            { for files.iter().map(|file| html! {
                <option value={(*file).clone()} selected={*file == value}>
                    { sounds.map_or(file.as_str(), |sounds| sounds.label_of(file)).to_string() }
                </option>
            }) }
            if !folders.is_empty() {
                <optgroup label="One at random from a folder">
                    { for folders.iter().map(|(folder, count)| html! {
                        <option value={folder.clone()} selected={folder == value}>
                            { format!("{folder} ({count} {})", if *count == 1 { one } else { many }) }
                        </option>
                    }) }
                </optgroup>
            }
            if !listed {
                <option value={value.to_string()} selected=true>{ value.to_string() }</option>
            }
        </>
    }
}

/// A file for a value naming a folder (`"alerts/"`), as a setting gets
/// one (`"alerts/hype.gif"`): one at random; other values as they are.
/// `None` for a folder with none.
pub fn file_from(value: &str, folders: &[MediaFolder], kind: MediaKind) -> Option<String> {
    let Some(name) = media_folder(value) else { return Some(value.to_string()) };
    let files = preview_files(name, folders, kind, true);
    let index = (js_sys::Math::random() * files.len() as f64) as usize;
    files.get(index.min(files.len().saturating_sub(1))).cloned()
}

/// Files for a preview's setting naming a folder (as `PickFile` gives
/// them): the first, so the preview holds still, or with `every` all of
/// them.
pub fn preview_files(folder: &str, folders: &[MediaFolder], kind: MediaKind, every: bool) -> Vec<String> {
    let Some(found) = folders.iter().find(|found| found.name == folder) else { return Vec::new() };
    let files = found.files.iter().filter(|file| kind.takes(file)).map(|file| format!("{folder}/{file}"));
    if every { files.collect() } else { files.take(1).collect() }
}

#[derive(Properties, PartialEq)]
pub struct PicturePickProps {
    pub id: AttrValue,
    /// The chosen file or folder; empty = none.
    pub value: AttrValue,
    /// Its videos too.
    #[prop_or_default]
    pub videos: bool,
    pub onchange: Callback<String>,
}

/// A picture: None, a file from the pictures folder, or one of its folders
/// (a picture from it at random each time). Asks for the folder's files
/// when it gets the focus.
#[function_component]
pub fn PicturePick(props: &PicturePickProps) -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let kind = if props.videos { MediaKind::Media } else { MediaKind::Picture };
    let onchange = props.onchange.reform(|event: Event| util::value_of(&event));
    let onfocus = {
        let panel = panel.clone();
        Callback::from(move |_: FocusEvent| {
            panel.send(PanelRequest::Pictures);
        })
    };
    html! {
        <select id={props.id.clone()} {onchange} {onfocus}>
            { media_options(&panel.model.pictures, &panel.model.picture_folders, &props.value, kind, None) }
        </select>
    }
}

#[derive(Properties, PartialEq)]
pub struct SoundPickProps {
    pub id: AttrValue,
    /// The chosen file; empty = none.
    pub value: AttrValue,
    pub onchange: Callback<String>,
    /// For ▶.
    #[prop_or(0.8)]
    pub volume: f32,
}

/// A sound: None, a file from the sounds folder, or one of its folders (a
/// sound from it at random each time), and ▶ to listen (to one of a
/// folder's). Asks for the folder's files when it gets the focus.
#[function_component]
pub fn SoundPick(props: &SoundPickProps) -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let value = props.value.to_string();
    let onchange = props.onchange.reform(|event: Event| util::value_of(&event));
    let onfocus = {
        let panel = panel.clone();
        Callback::from(move |_: FocusEvent| {
            panel.send(PanelRequest::Sounds);
        })
    };
    let play = {
        let (value, volume, folders) = (value.clone(), props.volume, panel.model.sound_folders.clone());
        Callback::from(move |_| {
            if let Some(file) = file_from(&value, &folders, MediaKind::Sound) {
                preview(&file, volume);
            }
        })
    };
    html! {
        <div class="sound-pick">
            <select id={props.id.clone()} {onchange} {onfocus}>
                { media_options(&panel.model.sounds, &panel.model.sound_folders, &value, MediaKind::Sound,
                                panel.model.config.as_ref().map(|config| &config.effects.sound)) }
            </select>
            <button type="button" class="icon-button small-icon" title="Listen" aria-label="Listen"
                    disabled={value.is_empty()} onclick={play}>{ icon("play") }</button>
        </div>
    }
}

/// A small Upload button for the pictures folder (`videos`: videos too);
/// `then` gets the saved name.
#[derive(Properties, PartialEq)]
pub struct PictureUploadProps {
    pub id: AttrValue,
    #[prop_or_default]
    pub videos: bool,
    #[prop_or_default]
    pub then: Callback<String>,
}

#[function_component]
pub fn PictureUpload(props: &PictureUploadProps) -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let error = use_state(|| None::<String>);
    let onchange = {
        let (panel, error, then) = (panel.clone(), error.clone(), props.then.clone());
        Callback::from(move |event: Event| {
            let Some(file) = chosen_file(&event) else { return };
            let (panel, error, then) = (panel.clone(), error.clone(), then.clone());
            yew::platform::spawn_local(async move {
                match upload(&picture_upload_path(&file.name()), &file).await {
                    Ok(name) => {
                        error.set(None);
                        panel.send(PanelRequest::Pictures);
                        then.emit(name);
                    }
                    Err(message) => error.set(Some(message)),
                }
            });
        })
    };
    let (accept, title) = if props.videos {
        (
            ".png,.apng,.gif,.jpg,.jpeg,.webp,.avif,.webm,.mp4",
            "Add a picture or a video (PNG, GIF, JPEG, WebP, AVIF, WebM, MP4)",
        )
    } else {
        (".png,.apng,.gif,.jpg,.jpeg,.webp,.avif", "Add a picture (PNG, GIF, JPEG, WebP, AVIF)")
    };
    // A node has no room for a line of its own: a failed upload turns the
    // button red, with why as its tooltip.
    let title = match &*error {
        Some(message) => format!("Couldn't upload: {message}"),
        None => title.to_string(),
    };
    html! {
        <label class={classes!("icon-button", "small-icon", "upload", error.is_some().then_some("failed"))}
               title={title.clone()} aria-label={title}>
            { icon("plus") }
            <input id={props.id.clone()} type="file" class="visually-hidden" {accept} {onchange} />
        </label>
    }
}

/// What a sound upload takes.
const TITLE: &str = "Add a sound (MP3, WAV, OGG, Opus, FLAC, M4A, WebM)";

/// An Upload button for sounds; `then` gets the saved name.
#[derive(Properties, PartialEq)]
pub struct SoundUploadProps {
    pub id: AttrValue,
    #[prop_or_default]
    pub then: Callback<String>,
    /// A small "+" (on nodes and settings, like `PictureUpload`), instead
    /// of an Upload button.
    #[prop_or_default]
    pub compact: bool,
}

#[function_component]
pub fn SoundUpload(props: &SoundUploadProps) -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let error = use_state(|| None::<String>);
    let onchange = {
        let (panel, error, then) = (panel.clone(), error.clone(), props.then.clone());
        Callback::from(move |event: Event| {
            let Some(file) = chosen_file(&event) else { return };
            let (panel, error, then) = (panel.clone(), error.clone(), then.clone());
            yew::platform::spawn_local(async move {
                match upload(&sound_upload_path(&file.name()), &file).await {
                    Ok(name) => {
                        error.set(None);
                        panel.send(PanelRequest::Sounds);
                        then.emit(name);
                    }
                    Err(message) => error.set(Some(message)),
                }
            });
        })
    };
    if props.compact {
        // No room for a line of its own: a failed upload turns the button
        // red, with why as its tooltip.
        let title = match &*error {
            Some(message) => format!("Couldn't upload: {message}"),
            None => TITLE.to_string(),
        };
        return html! {
            <label class={classes!("icon-button", "small-icon", "upload", error.is_some().then_some("failed"))}
                   title={title.clone()} aria-label={title}>
                { icon("plus") }
                <input id={props.id.clone()} type="file" class="visually-hidden" accept={ACCEPT} {onchange} />
            </label>
        };
    }
    html! {
        <>
            <label class="button ghost small upload" title={TITLE}>
                { icon("plus") }{ "Upload" }
                <input id={props.id.clone()} type="file" class="visually-hidden" accept={ACCEPT} {onchange} />
            </label>
            if let Some(message) = &*error {
                <p class="error small" role="alert">{ format!("Couldn't upload: {message}") }</p>
            }
        </>
    }
}

#[derive(Properties, PartialEq)]
pub struct MasterVolumeProps {
    /// Prefixes the ids: `{id}-volume`, `{id}-mute`.
    pub id: AttrValue,
    /// A small pill with the mute button built in, no label (Activity's
    /// queue); otherwise a settings row, the slider with the mute button
    /// before it and the volume after (Plugins → Sounds).
    #[prop_or_default]
    pub compact: bool,
}

/// The master volume with its mute button: both apply live on the
/// overlays, to what's playing too. On Plugins → Sounds and Activity.
#[function_component]
pub fn MasterVolume(props: &MasterVolumeProps) -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let Some(config) = &panel.model.config else { return html! {} };
    let settings = &config.effects.sound;
    let (volume, muted) = (settings.volume, settings.muted);
    let on_volume = {
        let panel = panel.clone();
        Callback::from(move |event: InputEvent| {
            if let Some(number) = util::number_of(&event) {
                panel.edit(Some("master-volume"), move |config| {
                    config.effects.sound.volume = (number / 100.0).clamp(0.0, 1.0) as f32
                });
            }
        })
    };
    let on_mute = {
        let panel = panel.clone();
        Callback::from(move |_: MouseEvent| {
            panel.edit(None, move |config| config.effects.sound.muted = !muted);
        })
    };
    // While the compact pill's slider is dragged, its value floats above
    // the thumb: the pill has no label, and a title doesn't show mid-drag.
    let dragging = use_state(|| false);
    let drag = |on: bool| {
        let dragging = dragging.clone();
        Callback::from(move |_: PointerEvent| dragging.set(on))
    };
    let slider = format!("{}-volume", props.id);
    let mute_button = html! {
        <button type="button" id={format!("{}-mute", props.id)} onclick={on_mute}
                class={classes!("icon-button", (!props.compact).then_some("flat"), muted.then_some("on"))}
                aria-pressed={muted.to_string()}
                aria-label={if muted { "Unmute" } else { "Mute" }}
                title={if muted { "Unmute: what's playing is heard again" } else { "Mute: everything plays on, silently" }}>
            { icon(if muted { "mute" } else { "volume" }) }
        </button>
    };
    let range = html! {
        <input type="range" id={slider.clone()} min="0" max="100" step="1" aria-label="Master volume"
               value={(volume * 100.0).round().to_string()} oninput={on_volume} />
    };
    if props.compact {
        let percent = (volume * 100.0).round();
        return html! {
            <div class={classes!("volume-pill", muted.then_some("muted"))}
                 title={format!("Master volume: {percent:.0}%")}>
                { mute_button }
                <span class="volume-track" onpointerdown={drag(true)} onpointerup={drag(false)}
                      onpointercancel={drag(false)} onlostpointercapture={drag(false)}>
                    { range }
                    if *dragging {
                        <span class="volume-bubble" id={format!("{}-volume-value", props.id)}
                              style={format!("--at: {}", percent / 100.0)}>{ format!("{percent:.0}%") }</span>
                    }
                </span>
            </div>
        };
    }
    html! {
        <div class={classes!("setting-row", "master-volume", muted.then_some("muted"))}>
            <label class="setting-label" for={slider.clone()}>{ "Master volume" }</label>
            <div class="setting-control">
                <div class="slider-row">
                    <div class="volume-row">
                        { mute_button }
                        { range }
                    </div>
                    <span class="slider-value mono" id={format!("{}-volume-value", props.id)}>
                        { if muted { "Muted".to_string() } else { format!("{:.0}%", volume * 100.0) } }
                    </span>
                </div>
            </div>
        </div>
    }
}

/// The app's sound settings, on the Sounds plugin's box: the master volume
/// (every sound's, alerts' and spoken messages' too), and the files in the
/// sounds folder, each with ▶, its label and delete (`sound-file-{index}`).
#[function_component]
pub fn Sounds() -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    {
        let panel = panel.clone();
        use_effect_with((), move |()| {
            panel.send(PanelRequest::Sounds);
        });
    }
    let Some(config) = &panel.model.config else { return html! {} };
    let settings = &config.effects.sound;
    html! {
        <div class="plugin-settings" id="sound-app">
            <div class="setting-rows">
                <MasterVolume id="sound" />
                <div class="setting-row tall list">
                    <span class="setting-label" id="sound-files-label">{ "Sounds" }</span>
                    <div class="setting-control">
                        <div class="sound-list" role="group" aria-labelledby="sound-files-label">
                            { for panel.model.sounds.iter().enumerate().map(|(index, file)| html! {
                                <SoundRow key={file.clone()} {index} file={file.clone()}
                                          label={settings.label_of(file).to_string()} volume={settings.volume} />
                            }) }
                            if panel.model.sounds.is_empty() {
                                <p class="muted small">{ "No sounds yet." }</p>
                            }
                            <div class="sound-upload">
                                <SoundUpload id="sound-upload" />
                            </div>
                        </div>
                    </div>
                </div>
            </div>
        </div>
    }
}

#[derive(Properties, PartialEq)]
struct SoundRowProps {
    index: usize,
    file: AttrValue,
    /// Its label, or its file name.
    label: AttrValue,
    volume: f32,
}

/// A file in the sounds folder: ▶, its label in a field (the file name
/// until it has one, then the file name under it), and a bin that asks once
/// more before deleting the file.
#[function_component]
fn SoundRow(props: &SoundRowProps) -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let confirm = super::activity::use_confirm();
    let id = format!("sound-file-{}", props.index);
    let file = props.file.to_string();
    let play = {
        let (file, volume) = (file.clone(), props.volume);
        Callback::from(move |_| preview(&file, volume))
    };
    // Saved when the field is left (or on Enter), so emptying it to type
    // another label doesn't bring the file name straight back. Left empty,
    // it is the file name again.
    let onchange = {
        let (panel, file) = (panel.clone(), file.clone());
        Callback::from(move |label: String| {
            let file = file.clone();
            panel.edit(None, move |config| config.effects.sound.set_label(&file, &label));
        })
    };
    let delete = {
        let (panel, confirm, file) = (panel.clone(), confirm.clone(), file.clone());
        Callback::from(move |_: MouseEvent| {
            if !*confirm {
                confirm.set(true);
                return;
            }
            confirm.set(false);
            let gone = file.clone();
            panel.edit(None, move |config| {
                config.effects.sound.labels.remove(&gone);
            });
            panel.send(PanelRequest::DeleteSound { name: file.clone() });
        })
    };
    html! {
        <div class="sound-item">
            <div class="sound-line">
                <button type="button" class="icon-button flat" id={format!("{id}-play")} title="Listen"
                        aria-label={format!("Listen to {}", props.label)} onclick={play}>{ icon("play") }</button>
                <TextField id={id.clone()} value={props.label.clone()} aria_label={format!("Label for {file}")}
                           title="Its name in the pickers (empty: the file name)" {onchange} />
                if *confirm {
                    <button type="button" class="button small danger" id={format!("{id}-delete")}
                            onclick={delete}>{ "Click again to delete" }</button>
                } else {
                    <button type="button" class="icon-button flat" id={format!("{id}-delete")}
                            aria-label={format!("Delete {file}")} title="Delete the file" onclick={delete}>
                        { icon("trash") }
                    </button>
                }
            </div>
            if props.label != props.file {
                <div class="sound-file" title={file.clone()}>{ file.clone() }</div>
            }
        </div>
    }
}
